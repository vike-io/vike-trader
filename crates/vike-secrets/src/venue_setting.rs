//! **The `venue_setting` NAMING GRAMMAR — one `(venue, tier, field)` row, the legacy credential
//! names it stands for, and the dotted key an operator types.**
//!
//! Ruling 10 took ten config-shaped names out of the `credential` table and into `venue_setting`.
//! Every reader kept looking the LEGACY name up, so something had to render a row back into the
//! names a loader asked for — that is [`venue_setting_names`], and the store's credential-map fold
//! applied it until decision 0095's Task 7 retired the fold. Every reader now takes the rows as
//! [`VenueSettings`]; what the renderer answers today is which credential NAMES a declared field
//! used to live under ([`declared_legacy_names`]), so a credential row still carrying one is
//! refused at boot rather than read by nothing ([`stranded_venue_setting_names`]).
//!
//! # ⚠ Why this lives in `vike-secrets` and not in `vike-bridge-core`
//!
//! It lived there until 2026-09-22, and the whole of this module's move is one measurement: the
//! fold could not reach two of the three readers. `vike_bridge_core::credentials` is layer 25 and
//! this crate is layer 15, dependencies run DOWN only, so a fold composed up there could only be
//! applied by a caller that had already reached up there. The two readers that had NOT —
//! [`crate::load_workspace_dotenv_from`] and `load_workspace_dotenv_scoped` (deleted since: its one
//! caller was a wrapper nothing called) — were blind, and not hypothetically:
//!
//! * `crates/bridges/vike-ibkr/tests/ibkr_mktdata_smoke.rs` read the right store through the plain
//!   reader, found no `IBKR_DEMO_PORT` because the row had moved, fell back to its built-in demo
//!   default, and SELF-SKIPPED while printing `test result: ok`.
//! * `crates/bridges/polymarket/src/egress.rs`'s now-deleted `dotenv_proxy_vars` read through the
//!   SCOPED one, so an operator's three `POLY_PROXY_*` rows were read by nothing at all and the
//!   built-in defaults stayed in force with no error anywhere. (Decision 0095 deleted that reader:
//!   the bridge takes its egress from a root's declaration now, never a store read.)
//!
//! CLAUDE.md states the cure exactly — *when two sides must not disagree, the cure is a shared
//! crate BELOW both, not a shared crate containing both* — so the grammar moved below its readers
//! and the fold happens inside the store. There is no `pub use` shim at the old home: a MOVE
//! updates every call site.
//!
//! The ONE outside name this needed was [`vike_model::credential_keys::CREDENTIAL_TIERS`], layer
//! 10, which this crate already depends on — so the move cost no new edge and no new package. ⚠
//! Since §4.4's `sim` -> `paper` rename it needs none at all: the tier vocabulary this grammar
//! classifies by is [`crate::schema::account_tier_named`], in this crate, because `account.tier`
//! and a credential key's TIER TOKEN stopped being the same word in a different case. The
//! dependency stays for the rest of the crate; this module no longer rests on it.

use std::collections::BTreeMap;
use std::path::Path;

use crate::db::DbError;

/// **§11 step 2 — the venue families the key grammar misses**, each with its reason.
///
/// `(store head, store tier token, venue, tier, discriminator, why)`. The PREFIX is the first two
/// composed by [`hand_mapped_prefix`]; no prefix here is a prefix of another (`DUKASCOPY`+`DEMO1`
/// and `DUKASCOPY`+`DEMO2` differ at their token), so order decides nothing except that the
/// tokenless polymarket row is reached after `vike_bridge_core::credentials`' `classify_poly` has
/// had its say.
///
/// ⚠ **The head and the token are SEPARATE columns rather than one spelled prefix**, and that is
/// not a style choice: `crates/vike-ops/tests/settings_secrets/settings_registry.rs`' loose sweep reads an
/// env-PREFIXED string literal in a `src/` file as evidence the file READS that variable, and then
/// demands a `SETTINGS` row for it. A prefix names no key in any store — it is exactly the part
/// §4.4 REMOVES — so such a row would declare a variable that does not exist, and `vike-cli config
/// show` reports a row as the ORIGIN of an effective value. Composing the prefix from two tokens
/// neither of which carries the trailing underscore keeps the literal out of that sweep while
/// saying the same thing more precisely.
///
/// ⚠ The DISCRIMINATOR is [`crate::AccountKey`]'s derivation-time-only field and reaches NO
/// column. It is how dukascopy's two accounts are told apart without the index token becoming an
/// identity again — the defect §1 of the spec is about, and §11.1's own rejected alternative. The
/// owner's signature rules that the `DEMO1`/`DEMO2` LABELS are not written at all, and they are not.
///
/// ⚠ **`pub` since the 2026-09-22 move**, because `vike_bridge_core::credentials`' classifier reads
/// the same table to go the OTHER way (name → row) and the two directions must not drift apart. It
/// was private while both lived in one file; nothing else about it changed.
pub const HAND_MAPPED_ACCOUNTS: &[HandMappedAccount] = &[
    (
        "ALPACA",
        "SANDBOX",
        "alpaca",
        "demo",
        None,
        "the tier token SANDBOX means demo; `CREDENTIAL_TIERS` does not carry that spelling. ONE \
         account.",
    ),
    (
        "DUKASCOPY",
        "DEMO1",
        "dukascopy",
        "demo",
        Some("DEMO1"),
        "ruling 1: DEMO1 and DEMO2 are TWO accounts of one venue at one tier — the index token \
         baked into the tier position becomes a ROW. This is the whole point of the schema, and \
         the ONLY (venue, tier) pair in the migration that yields two accounts.",
    ),
    (
        "DUKASCOPY",
        "DEMO2",
        "dukascopy",
        "demo",
        Some("DEMO2"),
        "the second of ruling 1's pair — see the row above.",
    ),
    (
        "POLY",
        "",
        "polymarket",
        "live",
        None,
        "head POLY, NO tier token, and no roster venue is spelled POLY: one account at tier live. \
         The family SPLITS key by key (§5.2) — `classify_poly` runs FIRST and this row is its \
         fallback.",
    ),
];

/// One row of [`HAND_MAPPED_ACCOUNTS`]: `(store head, store tier token, venue, tier,
/// discriminator, why)`. A named alias rather than the bare tuple because six positions is past
/// what a reader can hold, and because the `why` is the column that makes the table a table.
pub type HandMappedAccount =
    (&'static str, &'static str, &'static str, &'static str, Option<&'static str>, &'static str);

/// The store prefix a [`HAND_MAPPED_ACCOUNTS`] row names: `{HEAD}_{TOKEN}_`, or `{HEAD}_` for the
/// row whose family carries no tier token at all.
#[must_use]
pub fn hand_mapped_prefix(head: &str, token: &str) -> String {
    if token.is_empty() { format!("{head}_") } else { format!("{head}_{token}_") }
}

/// **THE MAP RENDERER — the inverse of `vike_bridge_core::credentials`' `classify_credential_name`,
/// and the thing §12 says must exist before ruling 10's rows may move.**
///
/// Given one `venue_setting` row — `(venue, tier, field)`, where `tier` is `None` for a
/// machine-scoped row — answer every legacy `credential.name` that row stands for. It was written
/// so a bridge loader could keep taking one `&HashMap<String, String>` and keep finding every name
/// it looked up, through the store's fold; since decision 0095's Task 7 retired that fold, no
/// reader looks a setting up by these names. What they are for now is RECOGNITION — the classifier,
/// the migration's collision label, and [`declared_legacy_names`], from which the boot refusal and
/// `vike-cli secrets move-venue-config` both answer.
///
/// ⚠ **`tier` is the RUST tier, never the stored word.** The column stores a machine-scoped row as
/// `'any'` since §5.2 step 7 (it said *"where `tier` is `NULL`"* here until then, which was the
/// stored spelling too), and a stored `'any'` handed to this function renders `{HEAD}_ANY_{FIELD}`
/// — a name no store holds, so the row goes UNREACHABLE with nothing erroring. This function does
/// NOT defend against that, deliberately: the translation lives in ONE place per direction
/// (`crate::schema::venue_setting_tier_of_stored` on the read side), and a second copy here would
/// be the second spelling that drifts. The one caller that handed it a STORED row was
/// `crate::store`'s fold (retired by decision 0095's Task 7), which read through
/// `crate::settings::read_settings`, which maps; every caller now hands it a catalog field or a
/// PARSED key (`parse_venue_setting_key`), neither of which carries the word.
///
/// # ⚠ Why this returns a LIST and not a name
///
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §11 measured it: the ten moving
/// config-shaped names land as **NINE rows**, because the two dukascopy demo SERVER keys hold the
/// same value in the live store and collapse onto one `(dukascopy, demo, SERVER)` row —
/// `venue_setting`'s own `UNIQUE (venue, tier, field)` admits no second (the partial index
/// `venue_setting_one_per_tier` said so until §5.2 step 7 folded it in). So a renderer that
/// answered ONE name would drop a key the operator wrote, and dukascopy's sidecar would fall back
/// to the default demo JNLP in silence.
///
/// The one-to-many falls out of [`HAND_MAPPED_ACCOUNTS`] rather than being spelled here: that table
/// carries two rows for `(dukascopy, demo)`, one per store tier token. A venue whose keys parse as
/// the ordinary `{VENUE}_{TIER}_{FIELD}` has no row there and renders exactly one name.
///
/// # ⚠ The collision this CANNOT see, and who owes the check
///
/// `credential.name` and the names this renders are ONE namespace, and SQLite can constrain a name
/// across two tables no more than it can across two databases — `credential_one_live_name` holds
/// inside `credential` alone. Nothing here stops a `venue_setting` row rendering a name a live
/// `credential` row already holds, and a caller folding both into one map would then hold one key
/// with two candidate values. The READ side no longer folds — a credential row under a rendered
/// name is a boot refusal (`vike_config::refuse_stranded_venue_settings`), not a second candidate —
/// and the MIGRATION's answer is stricter still: it refuses to CREATE one
/// (`vike_bridge_core::credentials::rendered_name_collisions`). This function is pure and is neither.
///
/// # ⚠ A machine-scoped row takes the store head with NO tier token
///
/// The polymarket proxy family carries no tier at all — it is machine-scoped by construction
/// (`vike_bridge_core::credentials`' `classify_poly` argues why: one process has one declared
/// egress, `vike_polymarket::declare_egress`, so it is not account-aware and cannot become so
/// without that declaration changing shape). So `tier: None` matches a hand-map row
/// on its VENUE alone and uses that row's head with an empty token, which is what makes `POLY` — a
/// head no roster venue is spelled as — render correctly.
#[must_use]
pub fn venue_setting_names(venue: &str, tier: Option<&str>, field: &str) -> Vec<String> {
    let mut out: Vec<String> = HAND_MAPPED_ACCOUNTS
        .iter()
        .filter(|(_, _, v, t, _, _)| {
            // A machine-scoped row has no tier to match on, so the VENUE is the whole key. A
            // tier-scoped one must match both, which is what keeps `(dukascopy, demo)`'s two rows
            // from rendering for `(dukascopy, live)`.
            *v == venue && tier.is_none_or(|want| *t == want)
        })
        .map(|(head, token, _, _, _, _)| {
            // ⚠ A machine-scoped row IGNORES the hand-map row's own token. `POLY`'s row spells an
            // empty one already, but stating it here is what stops a future head that DOES carry a
            // token from rendering `{HEAD}_{TOKEN}_{FIELD}` for a value that has no tier.
            let token = if tier.is_none() { "" } else { *token };
            format!("{}{field}", hand_mapped_prefix(head, token))
        })
        .collect();
    if out.is_empty() {
        // The ordinary grammar, for every conforming venue — ibkr and fxcm among the movers. The
        // tier token is the STORE's spelling of the normalized tier, which for a conforming venue
        // is the normalized tier uppercased; a venue whose store spells it differently is exactly
        // what earns a `HAND_MAPPED_ACCOUNTS` row, and it was matched above.
        let head = venue.to_uppercase();
        out.push(match tier {
            // ⚠ NOT `t.to_uppercase()`. Since §4.4's rename the stored tier is `paper` where the
            // credential key's token is `SIM`, so uppercasing the tier would render
            // `{HEAD}_PAPER_{FIELD}` — a name no store has ever held. The row would become
            // UNREACHABLE rather than wrong: nothing errors, the legacy key simply stops
            // answering. `crate::schema::key_token_of_account_tier` is that map.
            Some(t) => format!("{head}_{}_{field}", crate::schema::key_token_of_account_tier(t)),
            None => format!("{head}_{field}"),
        });
    }
    // Deterministic, and de-duplicated in case two hand-map rows ever share a prefix: the caller
    // folds these into a map, and a repeated name would make the fold's outcome depend on order.
    out.sort_unstable();
    out.dedup();
    out
}

/// **Where a venue's operational configuration LIVES — the dotted key an operator types.**
///
/// `venue.polymarket.proxy_host` for a machine-scoped value, `venue.ibkr.demo.backend` for a
/// tier-scoped one. Rendered under the `config` section, so the operator-facing spelling is
/// `config.venue.ibkr.demo.backend`.
///
/// # ⚠ Why this exists at all — the THIRD KEY SHAPE that was nearly added
///
/// `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §6 gives these ten values their own
/// table, `venue_setting`, keyed `(venue, tier, field)`. That would make **three** key shapes for
/// one question — `setting`'s `(section, key)`, `venue_arming`'s `(venue, label)`, and a third — and
/// the spec itself names the cost in its own open-items list:
///
/// > the table itself arrives with **no gate of its own**: a `venue_setting` row nothing reads is
/// > the defect `vike_config::CONSUMPTION` exists to refuse for settings keys, and **this table is
/// > outside that gate's vocabulary because its keys are not `section.key` paths**.
///
/// A `section.key` path costs nothing and buys all of it: `vike-cli config show` renders the value,
/// `vike-cli config set` writes it, and a TOML file still spells it as an ordinary nested table
/// (`[venue.ibkr.demo] backend = "cpapi"`). So the KEY is the operator's spelling — and the VALUES
/// nonetheless live in the `venue_setting` table, for the reason [`crate::StoredSettings::venue`]
/// carries: `vike_config::Config` has `#[serde(deny_unknown_fields)]` and no `venue` field, so a
/// `config.venue.*` row took the whole `config` section down with it. Owner's ruling 2026-09-20;
/// the column table MEASURED 2026-09-21.
///
/// # ⚠ The grammar is disambiguated by the TIER VOCABULARY, not by counting segments
///
/// `venue.ibkr.demo.backend` is `(ibkr, demo, BACKEND)` and `venue.polymarket.proxy_host` is
/// `(polymarket, None, PROXY_HOST)`, and both are four-or-fewer segments. What separates them is
/// that the second segment is a tier **iff [`crate::schema::account_tier_named`] answers for it**
/// — [`crate::ACCOUNT_TIERS`] plus the pre-rename `sim` spelling of `paper`, which that function's
/// own doc argues is a legacy INPUT spelling rather than an alias. That is safe because no field
/// this grammar carries is spelled as a tier, and
/// `a_field_is_never_mistaken_for_a_tier` is what keeps it that way.
#[must_use]
pub fn venue_setting_key(venue: &str, tier: Option<&str>, field: &str) -> String {
    let field = field.to_lowercase();
    match tier {
        Some(t) => format!("venue.{venue}.{}.{field}", t.to_lowercase()),
        None => format!("venue.{venue}.{field}"),
    }
}

/// The inverse of [`venue_setting_key`] — `None` for a key that is not one of these.
///
/// The FIELD comes back in the STORE's uppercase spelling, because that is what
/// [`venue_setting_names`] composes a legacy credential name from; the venue and tier come back
/// lowercased, which is how both the `account` table and the arming ceiling spell them.
#[must_use]
pub fn parse_venue_setting_key(key: &str) -> Option<(String, Option<String>, String)> {
    let rest = key.strip_prefix("venue.")?;
    let (venue, rest) = rest.split_once('.')?;
    if venue.is_empty() {
        return None;
    }
    // The tier is recognised by VOCABULARY. A `rest` that starts with a tier token AND carries
    // something after it is tier-scoped; anything else is machine-scoped and `rest` is the whole
    // field. See the grammar ⚠ on `venue_setting_key`.
    let tiered = rest.split_once('.').and_then(|(head, tail)| {
        (!tail.is_empty())
            .then(|| crate::schema::account_tier_named(head).map(|tier| (tier, tail)))
            .flatten()
    });
    Some(match tiered {
        // ⚠ The tier comes back CANONICAL, not merely lowercased, and that is the migration for a
        // key an operator typed before §4.4's rename: `venue.ibkr.sim.backend` classifies as
        // `(ibkr, paper, BACKEND)` and addresses the same row `venue.ibkr.paper.backend` does.
        Some((tier, field)) => (venue.to_lowercase(), Some(tier.to_string()), field.to_uppercase()),
        None if rest.is_empty() => return None,
        None => (venue.to_lowercase(), None, rest.to_uppercase()),
    })
}

/// Which tier a `venue_setting` row answers for: the whole machine (`Any` — stored as the tier word
/// `any`) or one account tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingTier {
    Any,
    Paper,
    Demo,
    Live,
}

impl SettingTier {
    /// The Rust tier a [`crate::VenueSettingRow`] carries (`None` = machine-scoped) as a
    /// [`SettingTier`]; `None` for a word outside the table's `CHECK`.
    #[must_use]
    pub fn of_row(tier: Option<&str>) -> Option<SettingTier> {
        match tier {
            None => Some(SettingTier::Any),
            Some(t) => match crate::schema::account_tier_named(t)? {
                "paper" => Some(SettingTier::Paper),
                "demo" => Some(SettingTier::Demo),
                "live" => Some(SettingTier::Live),
                _ => None,
            },
        }
    }

    /// The tier word [`venue_setting_key`] takes: `None` for [`SettingTier::Any`].
    #[must_use]
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            SettingTier::Any => None,
            SettingTier::Paper => Some("paper"),
            SettingTier::Demo => Some("demo"),
            SettingTier::Live => Some("live"),
        }
    }
}

/// **One venue's `venue_setting` rows, read once by a composition root and passed down as data** —
/// the mount contract's `settings` input (`docs/decisions/0096`). Fields are keyed in the store's
/// upper-case spelling; [`VenueSettings::get`] takes either spelling, so a caller writes the
/// catalog's (`vike_model::venues::venue_fields`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VenueSettings {
    venue: String,
    rows: BTreeMap<(SettingTier, String), String>,
}

impl VenueSettings {
    /// The rows of `venue` among `rows`; every other venue's row is ignored.
    #[must_use]
    pub fn from_rows(venue: &str, rows: &[crate::VenueSettingRow]) -> VenueSettings {
        let mut out = VenueSettings { venue: venue.to_string(), rows: BTreeMap::new() };
        for r in rows.iter().filter(|r| r.venue == venue) {
            if let Some(tier) = SettingTier::of_row(r.tier.as_deref()) {
                out.rows.insert((tier, r.field.to_ascii_uppercase()), r.value.clone());
            }
        }
        out
    }

    /// The venue id these rows belong to (`""` for [`VenueSettings::default`]).
    #[must_use]
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The value of `field` for `tier`: the row for exactly that tier, else the machine-scoped
    /// (`Any`) row. `field` may be spelled either way.
    #[must_use]
    pub fn get(&self, tier: SettingTier, field: &str) -> Option<&str> {
        let field = field.to_ascii_uppercase();
        self.rows
            .get(&(tier, field.clone()))
            .or_else(|| self.rows.get(&(SettingTier::Any, field)))
            .map(String::as_str)
    }

    /// The value of a TIER-scoped `field` for `tier`: the row for exactly that tier and never the
    /// machine-scoped (`Any`) one. `vike-cli config show` labels a machine-scoped row of a
    /// tier-scoped field "not read as that field's value", and a reader of such a field (the IBKR
    /// gateway, FXCM's host and connection, Dukascopy's server) uses this so that stays true.
    #[must_use]
    pub fn get_exact(&self, tier: SettingTier, field: &str) -> Option<&str> {
        self.rows.get(&(tier, field.to_ascii_uppercase())).map(String::as_str)
    }

    /// Every row, `(tier, stored field spelling, value)`, in tier-then-field order.
    pub fn rows(&self) -> impl Iterator<Item = (SettingTier, &str, &str)> + '_ {
        self.rows.iter().map(|((t, f), v)| (*t, f.as_str(), v.as_str()))
    }
}

/// **Every venue's `venue_setting` rows**, keyed by venue id — the one read a composition root makes.
/// A box with no settings database, or one whose store predates the table, answers an empty map.
///
/// # Errors
/// A store that exists and cannot be read.
pub fn load_venue_settings(
    settings_dir: &Path,
) -> Result<BTreeMap<String, VenueSettings>, DbError> {
    let rows = match crate::settings::read_settings_in(settings_dir)? {
        crate::SettingsSource::Rows { rows, .. } => rows.venue,
        crate::SettingsSource::NoDatabase { .. } | crate::SettingsSource::TablesAbsent { .. } => {
            return Ok(BTreeMap::new());
        }
    };
    let mut venues: Vec<&str> = rows.iter().map(|r| r.venue.as_str()).collect();
    venues.sort_unstable();
    venues.dedup();
    Ok(venues.into_iter().map(|v| (v.to_string(), VenueSettings::from_rows(v, &rows))).collect())
}

/// The declared field `stored_field` (the STORE's upper-case spelling, `RATE_GATE`) of `venue`, or
/// `None` — [`vike_model::venues::venue_fields::venue_field`] keyed the way a legacy credential name spells
/// the field. Exact: `rate_gate` is not the store's spelling and answers `None`, so a name the
/// legacy renderer could never have produced is never mistaken for one.
#[must_use]
pub fn declared_field(
    venue: &str,
    stored_field: &str,
) -> Option<&'static vike_model::venues::venue_fields::VenueField> {
    vike_model::venues::venue_fields::fields_of(venue)
        .find(|f| f.field.to_ascii_uppercase() == stored_field)
}

/// [`declared_legacy_names`], split by scope: `(machine-scoped, tier-scoped)`.
///
/// The split is what [`stranded_venue_setting_names`] needs: a tier-scoped name is matched through
/// a `__<LABEL>` suffix and a machine-scoped one only whole — see that function's doc.
fn legacy_names_by_scope() -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    use vike_model::credential_keys::{CREDENTIAL_TIERS, LEGACY_CREDENTIAL_TIERS};
    let mut machine = BTreeMap::new();
    let mut tiered = BTreeMap::new();
    for f in vike_model::venues::venue_fields::VENUE_FIELDS {
        let field = f.field.to_ascii_uppercase();
        if !f.tier_scoped {
            for name in venue_setting_names(f.venue, None, &field) {
                machine.insert(name, venue_setting_key(f.venue, None, &field));
            }
            continue;
        }
        // The renderer's own spelling for each tier — for dukascopy's demo tier its two hand-mapped
        // account tokens, `DEMO1` and `DEMO2`…
        for tier in crate::ACCOUNT_TIERS {
            for name in venue_setting_names(f.venue, Some(tier), &field) {
                tiered.insert(name, venue_setting_key(f.venue, Some(tier), &field));
            }
        }
        // …and the venue grammar's plain `{VENUE}_{TOKEN}_{FIELD}` for every tier token
        // `vike_model::accounts::account_keys::account_ref_from_key` reads, the legacy `MAINNET` included.
        // The credential classifier files every one of these as a pending venue setting, so
        // `vike-cli secrets move-venue-config` moves it — and a name the verb moves is a name this
        // set must refuse, or the refusal names a verb that cannot clear it (PF-31).
        let head = f.venue.to_ascii_uppercase();
        for token in CREDENTIAL_TIERS.iter().chain(LEGACY_CREDENTIAL_TIERS.iter()) {
            let name = format!("{head}_{token}_{field}");
            let Some(reference) = vike_model::accounts::account_keys::account_ref_from_key(&name)
            else {
                continue;
            };
            let tier = crate::schema::account_tier_of_key_token(reference.tier);
            tiered.insert(name, venue_setting_key(f.venue, Some(&tier), &field));
        }
    }
    (machine, tiered)
}

/// **Every legacy credential NAME a declared venue field was read under, mapped to the field's
/// dotted key** — the names the credential-map fold filled until decision 0095's Task 7 retired it.
///
/// A machine-scoped field renders once ([`venue_setting_names`] with no tier). A tier-scoped field
/// renders the renderer's own name for each tier, plus the venue grammar's plain
/// `{VENUE}_{TOKEN}_{FIELD}` for every credential tier token (`SIM`, `DEMO`, `LIVE` and the legacy
/// `MAINNET`), because the credential classifier files each of those as a pending venue setting.
/// Derived from [`vike_model::venues::venue_fields::VENUE_FIELDS`] and the renderer, never written down: a
/// field declared later joins it by being declared.
#[must_use]
pub fn declared_legacy_names() -> BTreeMap<String, String> {
    let (mut machine, tiered) = legacy_names_by_scope();
    machine.extend(tiered);
    machine
}

/// **The names in `names` that are a declared venue field's legacy credential name**, each with the
/// field's dotted key, sorted — the credential rows nothing reads since the fold retired.
///
/// A TIER-scoped name is found through a `__<LABEL>` suffix too: `IBKR_DEMO_HOST__HEDGE` was read
/// for the labelled account's gateway until the gateway became one setting per machine and tier
/// (ruling 10), so it is stranded like its unlabelled twin, and `vike-cli secrets
/// move-venue-config` moves it onto `venue.ibkr.demo.host`. A MACHINE-scoped name is matched only
/// whole: no reader ever looked a labelled spelling of one up, and the credential classifier files
/// it as an ordinary account row the move verb does not touch — refusing it would name a verb that
/// cannot clear it. `crates/vike-bridge-core/tests/credential_classification.rs`'s
/// `the_boot_refusal_and_the_move_verb_agree_on_every_name` holds this set equal to what the
/// classifier marks movable.
///
/// Names only: a caller reports these, and a value never travels with them.
#[must_use]
pub fn stranded_venue_setting_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> Vec<(String, String)> {
    let (machine, tiered) = legacy_names_by_scope();
    let mut out: Vec<(String, String)> = names
        .into_iter()
        .filter_map(|n| {
            let whole = machine.get(n).or_else(|| tiered.get(n));
            let labelled = || {
                let split = vike_model::accounts::account_keys::split_account_key(n).ok()?;
                split.label.text()?;
                tiered.get(split.base)
            };
            whole.or_else(labelled).map(|key| (n.to_string(), key.clone()))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// **The grammar's own properties** — the half of the old `venue_setting_renderer_tests` that needs
/// nothing but this module. The half that needs the CLASSIFIER stayed with the classifier, in
/// `crates/vike-bridge-core/src/credentials.rs`, because that is what it is about.
///
/// ⚠ **No test here spells a whole `{PREFIX}_{NAME}` literal**, and several of them used to. That
/// is not fastidiousness: `crates/vike-ops/tests/settings_secrets/settings_registry.rs`' loose sweep reads an
/// env-shaped literal in ANY `src/` file as evidence that file's CRATE reads that variable, rows
/// are keyed `(name, krate)`, and `vike-secrets` has no row for a venue key. The assertions
/// decompose the rendered name on `_` and compare the PARTS instead, which is also the stronger
/// claim — it names the head, the store's tier token and the field separately.
#[path = "venue_setting_tests.rs"]
#[cfg(test)]
mod venue_setting_tests;
