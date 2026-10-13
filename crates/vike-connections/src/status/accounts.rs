//! `AccountGrids`: every account label the Connections panel can show, each with its own tier grid,
//! folded from the store's account ROWS and their credentials by FIELD.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use vike_model::accounts::account_keys::AccountLabel;
use vike_model::credential_schema::{FieldOwner, schema_for};
use vike_model::{AccountTier, VENUES};
use vike_secrets::account_credentials::CredentialFields;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::credentials::StoreHealth;
use vike_secrets::{Account, AccountKeys, CredentialStore};

use super::{AccountCell, TierCell, VenueCredStatus};
use crate::keys::{FieldTarget, FormPlace, Sensitivity, field_sensitivity, form_fields};

/// The fault an [`AccountDirectory`] nobody read reports — the panel renders no measurement from it.
const NOT_READ: &str = "the credential store was not read for this frame";

/// **What the store holds for ONE owner, by FIELD** — an account row's credentials, or a venue's
/// application rows. Presence, the PUBLIC values, and (an account's only) the stored NAME of each
/// live row.
///
/// ⚠ **The `readable` filter is the security boundary, and it is here rather than at the widget on
/// purpose.** The editor has to be able to show a login or an account number back — a masked field
/// that starts empty is right for a key and is exactly how a value an operator set six months ago
/// becomes unknowable from inside the app that wrote it. But a renderer that holds every value and
/// *chooses* not to draw the secrets is one mistaken line away from drawing one. Filtering at the
/// DERIVATION — through `crate::keys::field_sensitivity`, which fails closed — means `view` is never
/// handed a secret at all. `crates/vike-connections/tests/panel/a11y_secrets.rs`'s
/// `no_stored_credential_value_reaches_the_rail_the_detail_pane_or_an_opened_editor` is the
/// end-to-end proof.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldFields {
    /// Every field holding a non-blank value — names only.
    held: BTreeSet<String>,
    /// The PUBLIC fields' values (trimmed) — and nothing else.
    readable: BTreeMap<String, String>,
    /// field → the stored NAME of its live row, taken from `CredentialStore::account_keys`' listing
    /// (an account's only: no listing names a venue's application rows).
    names: BTreeMap<String, String>,
}

impl HeldFields {
    /// Fold one owner's fields. `keys` is the account's row of the store's key-NAME listing, `None`
    /// for a venue's application rows.
    fn of(venue: &str, fields: Option<&CredentialFields>, keys: Option<&AccountKeys>) -> Self {
        let mut out = HeldFields::default();
        let Some(fields) = fields else { return out };
        for field in fields.fields() {
            out.held.insert(field.to_string());
            // ⚠ The ONE place a stored value is carried past this module, and only the half that
            // is not a secret — see the type's doc.
            if field_sensitivity(venue, field) == Sensitivity::Public
                && let Some(value) = fields.get(field)
            {
                out.readable.insert(field.to_string(), value.to_string());
            }
            if let Some(name) = keys.and_then(|k| stored_name_of(k, field)) {
                out.names.insert(field.to_string(), name);
            }
        }
        out
    }

    /// Whether the owner holds `field` with a non-blank value.
    #[must_use]
    pub fn holds(&self, field: &str) -> bool {
        self.held.contains(field)
    }

    /// The fields held, sorted — names only.
    pub fn fields(&self) -> impl Iterator<Item = &str> {
        self.held.iter().map(String::as_str)
    }

    /// `field`'s value when it is PUBLIC and held, else `None`. Never a secret: the map this reads
    /// was filtered when it was built.
    #[must_use]
    pub fn readable(&self, field: &str) -> Option<&str> {
        self.readable.get(field).map(String::as_str)
    }

    /// The stored NAME of `field`'s live row — what the field REMOVE retires
    /// (`vike_secrets::retire_credential_in_store` is addressed by name). `None` when the listing
    /// did not name exactly one row for the field, and for a venue's application rows: the panel
    /// then offers no Remove rather than guessing a name.
    #[must_use]
    pub fn stored_name(&self, field: &str) -> Option<&str> {
        self.names.get(field).map(String::as_str)
    }
}

/// **The stored NAME of `field`'s live row, picked out of the account's own key listing — never
/// composed.**
///
/// `vike_secrets::AccountKeys` lists every live NAME the account owns and the PREFIXES the store
/// itself split off them (each name less its own `field`). The row of `field` is the listed name
/// that is one of those prefixes followed by exactly `field`. ⚠ It answers only when exactly ONE
/// listed name fits; two (or none) is `None`, and the panel then offers no Remove — a retire of a
/// guessed name is the one mistake this must not make.
fn stored_name_of(keys: &AccountKeys, field: &str) -> Option<String> {
    let mut fits = keys.names.iter().filter(|name| {
        name.ends_with(field)
            && keys
                .prefixes
                .iter()
                .any(|p| name.len() == p.len() + field.len() && name.starts_with(p.as_str()))
    });
    let first = fits.next()?;
    if fits.next().is_some() {
        return None;
    }
    Some(first.clone())
}

/// **Every account label the Connections panel can show, each with its own tier grid** — the
/// input a screen that renders ONE account at a time takes: the default (unlabelled) account's grid
/// plus one per label the store's account ROWS carry.
///
/// ⚠ **One snapshot, carrying its own health.** The rows, their key names and their credentials are
/// the three reads a composition root performs (`vike_secrets::CredentialStore::accounts`,
/// `account_keys`, `credential_book`, composed as a `vike_secrets::account_directory::AccountDirectory`),
/// and [`Self::health`] is THEIR verdict — so a count or a cell can never be folded from one read
/// while the banner describes another. A store that exists and would not open answers
/// `StoreHealth::Unreadable` and the panel claims no measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGrids {
    /// Whether the reads this was folded from answered.
    health: StoreHealth,
    /// The DEFAULT (unlabelled) account's grid.
    default_grid: Vec<VenueCredStatus>,
    /// One entry per LABEL an account row carries, sorted and deduplicated.
    labelled: Vec<(AccountLabel, Vec<VenueCredStatus>)>,
    /// Each account row's fields, by `account.id`.
    accounts: BTreeMap<i64, HeldFields>,
    /// Each venue's APPLICATION fields (`FieldOwner::Venue`).
    venue_apps: BTreeMap<String, HeldFields>,
    /// The venue-wide attribution DEPLOYMENT rows the store holds (by name) — public by
    /// construction (`crate::keys::FormField::sensitivity`), so their values may be read back.
    deployment: BTreeMap<String, String>,
}

impl AccountGrids {
    /// **The production read**: the three store reads through `store` (the caller's handle, under
    /// its scope), folded over the whole roster. `deployment` is the caller's NAME map (the same
    /// read every deployment name comes from); only the attribution tail's public names are kept
    /// from it.
    #[must_use]
    pub fn read(store: &CredentialStore, deployment: &HashMap<String, String>) -> Self {
        let dir =
            AccountDirectory::read(store.accounts(), store.account_keys(), store.credential_book());
        Self::from_directory(&dir, deployment)
    }

    /// The fold over the whole roster ([`vike_model::VENUES`]).
    #[must_use]
    pub fn from_directory(dir: &AccountDirectory, deployment: &HashMap<String, String>) -> Self {
        Self::for_venues(VENUES, dir, deployment)
    }

    /// **Nothing was read** — the value a caller holds on a frame that performs no store read.
    /// Every cell is UNKNOWN under it, never a measured absence.
    #[must_use]
    pub fn unread() -> Self {
        Self::from_directory(&AccountDirectory::unread(), &HashMap::new())
    }

    /// **The derivation**, over `venues` (the roster, or the subset a test harness wants).
    ///
    /// A label is in the strip when an account ROW carries it — the store's own answer to *which
    /// accounts exist*; there is no other enumeration. A label that fails `AccountLabel::parse` (the
    /// store's own floor refuses to write one) is skipped rather than rendered.
    #[must_use]
    pub fn for_venues(
        venues: &[&str],
        dir: &AccountDirectory,
        names: &HashMap<String, String>,
    ) -> Self {
        let mut faults: Vec<String> = Vec::new();
        let mut fault = |why: &str| {
            if !faults.iter().any(|f| f == why) {
                faults.push(why.to_string());
            }
        };
        let rows: Vec<Account> = match dir.rows() {
            None => {
                fault(NOT_READ);
                Vec::new()
            }
            Some(Err(why)) => {
                fault(why);
                Vec::new()
            }
            Some(Ok(accounts)) => accounts.known().map(<[Account]>::to_vec).unwrap_or_default(),
        };
        let keys: Option<&BTreeMap<i64, AccountKeys>> = match dir.keys() {
            None => {
                fault(NOT_READ);
                None
            }
            Some(Err(why)) => {
                fault(why);
                None
            }
            Some(Ok(keys)) => keys,
        };
        let book = match dir.credentials() {
            None => {
                fault(NOT_READ);
                None
            }
            Some(Err(why)) => {
                fault(why);
                None
            }
            Some(Ok(book)) => Some(book),
        };
        let health = if faults.is_empty() {
            StoreHealth::Readable
        } else {
            StoreHealth::Unreadable(faults.join("; "))
        };

        let accounts: BTreeMap<i64, HeldFields> = rows
            .iter()
            .map(|row| {
                let fields = book.and_then(|b| b.account(row.id));
                let listing = keys.and_then(|k| k.get(&row.id));
                (row.id, HeldFields::of(&row.venue, fields, listing))
            })
            .collect();
        let venue_apps: BTreeMap<String, HeldFields> = venues
            .iter()
            .filter_map(|venue| {
                let fields = book.and_then(|b| b.venue(venue))?;
                Some(((*venue).to_string(), HeldFields::of(venue, Some(fields), None)))
            })
            .collect();
        let deployment: BTreeMap<String, String> = venues
            .iter()
            .flat_map(|venue| form_fields(venue, FormPlace::Tier(AccountTier::Live)))
            .filter(|f| f.sensitivity() == Sensitivity::Public)
            .filter_map(|f| match f.target {
                FieldTarget::Deployment(name) => {
                    let value = names.get(&name)?;
                    (!value.trim().is_empty()).then(|| (name, value.clone()))
                }
                _ => None,
            })
            .collect();

        let mut labels: Vec<AccountLabel> = rows
            .iter()
            .filter_map(|r| r.label.as_deref())
            .filter_map(|l| AccountLabel::parse(l).ok())
            .collect();
        labels.sort();
        labels.dedup();
        let grid = |label: &AccountLabel| -> Vec<VenueCredStatus> {
            venues.iter().map(|venue| status_of(venue, label, &rows, &accounts)).collect()
        };
        let default_grid = grid(&AccountLabel::Default);
        let labelled = labels.into_iter().map(|l| (l.clone(), grid(&l))).collect();
        AccountGrids { health, default_grid, labelled, accounts, venue_apps, deployment }
    }

    /// Whether the store reads this was folded from answered — the input every cell and every count
    /// reads before claiming a measurement.
    #[must_use]
    pub fn health(&self) -> &StoreHealth {
        &self.health
    }

    /// The DEFAULT (unlabelled) account's grid.
    #[must_use]
    pub fn default_grid(&self) -> &[VenueCredStatus] {
        &self.default_grid
    }

    /// This label's grid, or `None` when no account row carries the label.
    ///
    /// ⚠ `None` is NOT "fall back to the default account": an account another label's rows hold is
    /// not this one's. It is also the ordinary state of a label an operator has just NAMED and whose
    /// rows do not exist yet.
    #[must_use]
    pub fn grid_for(&self, label: &AccountLabel) -> Option<&[VenueCredStatus]> {
        if label.is_default() {
            return Some(&self.default_grid);
        }
        self.labelled.iter().find(|(l, _)| l == label).map(|(_, g)| g.as_slice())
    }

    /// The LABELLED accounts, in order. Empty on a box whose rows carry no label.
    pub fn labels(&self) -> impl Iterator<Item = &AccountLabel> + '_ {
        self.labelled.iter().map(|(l, _)| l)
    }

    /// An all-[`TierCell::NoAccount`] grid over the same venues, in the same order — what a label no
    /// row carries renders as.
    #[must_use]
    pub fn absent_grid(&self) -> Vec<VenueCredStatus> {
        self.default_grid.iter().map(|s| VenueCredStatus::absent(&s.venue)).collect()
    }

    /// Account row `id`'s fields, or `None` when the store holds nothing for it.
    #[must_use]
    pub fn account_fields(&self, id: i64) -> Option<&HeldFields> {
        self.accounts.get(&id).filter(|held| held.fields().next().is_some())
    }

    /// `venue`'s APPLICATION fields, or `None` when it holds none.
    #[must_use]
    pub fn venue_fields(&self, venue: &str) -> Option<&HeldFields> {
        self.venue_apps.get(venue)
    }

    /// The value of a held venue-wide attribution DEPLOYMENT row (public by construction), or
    /// `None` when the store holds none under `name`.
    #[must_use]
    pub fn deployment_value(&self, name: &str) -> Option<&str> {
        self.deployment.get(name).map(String::as_str)
    }
}

/// One venue's three cells for one label.
fn status_of(
    venue: &str,
    label: &AccountLabel,
    rows: &[Account],
    accounts: &BTreeMap<i64, HeldFields>,
) -> VenueCredStatus {
    let cell = |tier| cell_of(venue, label, tier, rows, accounts);
    VenueCredStatus {
        venue: venue.to_string(),
        paper: cell(AccountTier::Paper),
        demo: cell(AccountTier::Demo),
        live: cell(AccountTier::Live),
    }
}

/// **The account row of `(venue, label, tier)`** — every row, active or not (an inactive row still
/// owns its credentials and is edited like any other). Exactly one row is that row; more than one is
/// resolved to the ONE active row among them when there is one (the row a mount would take), and
/// is [`TierCell::Ambiguous`] otherwise.
fn cell_of(
    venue: &str,
    label: &AccountLabel,
    tier: AccountTier,
    rows: &[Account],
    accounts: &BTreeMap<i64, HeldFields>,
) -> TierCell {
    let matching: Vec<&Account> = rows
        .iter()
        .filter(|r| r.venue == venue && r.tier == tier && r.label.as_deref() == label.text())
        .collect();
    let row = match matching.as_slice() {
        [] => return TierCell::NoAccount,
        [one] => *one,
        many => {
            let mut active = many.iter().filter(|r| r.active);
            match (active.next(), active.next()) {
                (Some(one), None) => *one,
                _ => return TierCell::Ambiguous(many.iter().map(|r| r.id).collect()),
            }
        }
    };
    let required = schema_for(venue).map(|s| s.required(FieldOwner::Account)).unwrap_or_default();
    let held = accounts.get(&row.id);
    let (held_required, missing_required): (Vec<&'static str>, Vec<&'static str>) =
        required.into_iter().partition(|field| held.is_some_and(|h| h.holds(field)));
    TierCell::Account(AccountCell {
        id: row.id,
        active: row.active,
        held_required,
        missing_required,
    })
}
