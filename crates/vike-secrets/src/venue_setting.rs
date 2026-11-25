//! **The `venue_setting` grammar — one `(venue, tier, field)` row and the dotted key an operator
//! types for it**, and [`VenueSettings`], the rows of one venue as a composition root hands them
//! down.
//!
//! Ruling 10 took ten config-shaped names out of the `credential` table and into `venue_setting`.
//! Every reader takes the rows as [`VenueSettings`] (decision 0095's Task 7 retired the fold that
//! rendered them back into legacy credential names). Since decision 0156 item 6 no credential name
//! carries a tier, so the renderer that composed `{HEAD}_{TOKEN}_{FIELD}` out of a row — and the
//! hand-mapped token table it rested on (`SANDBOX`, `DEMO1`/`DEMO2`, the tokenless `POLY`) — is gone
//! with every other name-to-tier map: a credential row under a setting's old name is read by
//! nothing, and a credential write by NAME files only a deployment row, so it cannot land one
//! anywhere a reader looks.
//!
//! # ⚠ Why this lives in `vike-secrets` and not in `vike-bridge-core`
//!
//! It lived there until 2026-09-22, and the move is one measurement: a fold composed up there could
//! not reach the readers below it. CLAUDE.md states the cure — *when two sides must not disagree,
//! the cure is a shared crate BELOW both* — so the grammar moved below its readers. There is no
//! `pub use` shim at the old home: a MOVE updates every call site.

use std::collections::BTreeMap;
use std::path::Path;

use vike_model::AccountTier;
use vike_venues::venue_fields::{MARK_STREAMS, VenueFieldName};
use vike_venues::venue_urls::VenueUrls;

use crate::db::StoreError;

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
/// A `section.key` path costs nothing and buys all of it: `vike-cli config show` renders the value
/// and `vike-cli config set` writes it. So the KEY is the operator's spelling — and the VALUES
/// nonetheless live in the `venue_setting` table, for the reason [`crate::StoredSettings::venue`]
/// carries: `vike_config::Config` has `#[serde(deny_unknown_fields)]` and no `venue` field, so a
/// `config.venue.*` row took the whole `config` section down with it. Owner's ruling 2026-09-20;
/// the column table MEASURED 2026-09-21.
///
/// # ⚠ The grammar is disambiguated by the TIER VOCABULARY, not by counting segments
///
/// `venue.ibkr.demo.backend` is `(ibkr, demo, BACKEND)` and `venue.polymarket.proxy_host` is
/// `(polymarket, None, PROXY_HOST)`, and both are four-or-fewer segments. What separates them is
/// that the second segment is a tier **iff `vike_model::AccountTier`'s one parser takes it** —
/// exactly `paper`, `demo` or `live`. The pre-rename `sim` spelling of `paper` is NOT a tier any
/// more (decision 0129): `venue.ibkr.sim.backend` is the MACHINE-scoped field `SIM.BACKEND`, and
/// so is an uppercase `LIVE` segment. That is safe because no field
/// this grammar carries is spelled as a tier, and
/// `a_field_is_never_mistaken_for_a_tier` is what keeps it that way.
///
/// The field is spelled the operator's way ([`VenueFieldName::operator_spelling`]).
#[must_use]
pub fn venue_setting_key(venue: &str, tier: Option<AccountTier>, field: &VenueFieldName) -> String {
    let field = field.operator_spelling();
    match tier {
        Some(t) => format!("venue.{venue}.{t}.{field}"),
        None => format!("venue.{venue}.{field}"),
    }
}

/// The inverse of [`venue_setting_key`] — `None` for a key that is not one of these.
///
/// The FIELD comes back as a [`VenueFieldName`], whatever case it was typed in; the venue comes
/// back lowercased, which is how the `account` table spells it, and the tier as the
/// `vike_model::AccountTier` its strict parser read.
#[must_use]
pub fn parse_venue_setting_key(key: &str) -> Option<(String, Option<AccountTier>, VenueFieldName)> {
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
            .then(|| head.parse::<AccountTier>().ok().map(|tier| (tier, tail)))
            .flatten()
    });
    Some(match tiered {
        // The tier is exactly what the operator typed: the parser repairs no spelling.
        Some((tier, field)) => {
            (venue.to_lowercase(), Some(tier), VenueFieldName::normalised(field))
        }
        None if rest.is_empty() => return None,
        None => (venue.to_lowercase(), None, VenueFieldName::normalised(rest)),
    })
}

/// Which tier a `venue_setting` row answers for: the whole machine (`Any` — stored as the tier word
/// `any`) or one account tier, a `vike_model::AccountTier` (decision 0129).
///
/// A named enum rather than a bare `Option<AccountTier>` because a reader's call site says which it
/// means — `SettingTier::Any` reads as the machine-scoped row, where a bare `None` would read as
/// "no value".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingTier {
    /// The machine-scoped row, which answers for every tier.
    Any,
    /// One account tier.
    Tier(AccountTier),
}

impl SettingTier {
    /// The tier a [`crate::VenueSettingRow`] carries (`None` = machine-scoped) as a [`SettingTier`].
    #[must_use]
    pub fn of_row(tier: Option<AccountTier>) -> SettingTier {
        tier.map_or(SettingTier::Any, SettingTier::Tier)
    }

    /// The tier [`venue_setting_key`] takes: `None` for [`SettingTier::Any`].
    #[must_use]
    pub fn tier(self) -> Option<AccountTier> {
        match self {
            SettingTier::Any => None,
            SettingTier::Tier(tier) => Some(tier),
        }
    }
}

impl From<AccountTier> for SettingTier {
    fn from(tier: AccountTier) -> SettingTier {
        SettingTier::Tier(tier)
    }
}

/// **One venue's `venue_setting` rows and its `venue_url` addresses, read once by a composition root
/// and passed down as data** — the mount contract's `settings` input (`docs/decisions/0096`). Fields
/// are keyed by [`VenueFieldName`], so a row stored in ANY case answers (every writer stores upper
/// case; only hand-typed SQL or a test plant spells a row otherwise, and nothing migrates it), and a caller asks
/// with the catalog's name (`vike_venues::venue_fields`). The addresses are
/// [`VenueSettings::urls`], asked through the ONE lookup, `vike_venues::venue_urls::VenueUrls::url`.
///
/// ⚠ `Debug` is written by hand: a SECRET field's value (`vike_venues::venue_fields::VenueField::secret`,
/// the proxy URL with its password) prints as `<secret>`, so a type that derives `Debug` over these
/// rows (`TradeLaneSnapshot`, `AdapterPolicy`) prints no credential either.
#[derive(Clone, PartialEq, Eq, Default, Hash)]
pub struct VenueSettings {
    venue: String,
    rows: BTreeMap<(SettingTier, VenueFieldName), String>,
    urls: VenueUrls,
}

impl std::fmt::Debug for VenueSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rows: BTreeMap<(SettingTier, &str), &str> = self
            .rows
            .iter()
            .map(|((tier, field), value)| {
                let secret = vike_venues::venue_fields::venue_field(&self.venue, field)
                    .is_some_and(|declared| declared.secret);
                (
                    (*tier, field.operator_spelling()),
                    if secret { "<secret>" } else { value.as_str() },
                )
            })
            .collect();
        f.debug_struct("VenueSettings")
            .field("venue", &self.venue)
            .field("rows", &rows)
            .field("urls", &self.urls)
            .finish()
    }
}

impl VenueSettings {
    /// The rows of `venue` among `rows`; every other venue's row is ignored. It carries NO address:
    /// [`VenueSettings::with_urls`] adds them ([`load_venue_settings`] does). A field stored in any
    /// case is the one [`VenueFieldName`]; where a store holds the same field twice in two cases, the
    /// later row in `rows` answers (the store reads them sorted by field, so the lower-case one).
    #[must_use]
    pub fn from_rows(venue: &str, rows: &[crate::VenueSettingRow]) -> VenueSettings {
        let mut out = VenueSettings {
            venue: venue.to_string(),
            rows: BTreeMap::new(),
            urls: VenueUrls::default(),
        };
        for r in rows.iter().filter(|r| r.venue == venue) {
            let tier = SettingTier::of_row(r.tier);
            out.rows.insert((tier, VenueFieldName::normalised(&r.field)), r.value.clone());
        }
        out
    }

    /// The same settings carrying `urls` as the venue's addresses. A test hands a bridge the
    /// release's defaults with `VenueSettings::default().with_urls(VenueUrls::seed("binance"))`.
    #[must_use]
    pub fn with_urls(mut self, urls: VenueUrls) -> VenueSettings {
        self.urls = urls;
        self
    }

    /// **The venue's addresses**: ask them with `urls().url(type, channel, protocol, product)`
    /// (`vike_venues::venue_urls::VenueUrls::url`, the four-step rule and the demo guard). Empty for
    /// [`VenueSettings::default`], so every lookup is refused, naming the row.
    #[must_use]
    pub fn urls(&self) -> &VenueUrls {
        &self.urls
    }

    /// The venue id these rows belong to (`""` for [`VenueSettings::default`]).
    #[must_use]
    pub fn venue(&self) -> &str {
        &self.venue
    }

    /// The value of `field` for `tier`: the row for exactly that tier, else the machine-scoped
    /// (`Any`) row.
    #[must_use]
    pub fn get(&self, tier: SettingTier, field: &VenueFieldName) -> Option<&str> {
        self.get_exact(tier, field).or_else(|| self.get_exact(SettingTier::Any, field))
    }

    /// The value of a TIER-scoped `field` for `tier`: the row for exactly that tier and never the
    /// machine-scoped (`Any`) one. `vike-cli config show` labels a machine-scoped row of a
    /// tier-scoped field "not read as that field's value", and a reader of such a field (the IBKR
    /// gateway, FXCM's host and connection, Dukascopy's server) uses this so that stays true.
    #[must_use]
    pub fn get_exact(&self, tier: SettingTier, field: &VenueFieldName) -> Option<&str> {
        self.rows.get(&(tier, field.clone())).map(String::as_str)
    }

    /// **A SWITCH field's value for `tier`, as the bool its declared grammar states** — the one typed read of
    /// a `venue_setting` switch, so no reader re-parses a value the write already checked. The row is found
    /// the way the field is scoped ([`VenueSettings::get_exact`] for a tier-scoped field, else
    /// [`VenueSettings::get`]) and read by `vike_venues::venue_fields::VenueFieldGrammar::switch_value`.
    ///
    /// `None` is the caller's default: no row answers, `field` is not a declared switch of this venue, or the
    /// stored value is no legal spelling of its grammar (the write refuses one, so only hand-typed SQL plants
    /// it). An annotated `1 x` is therefore never on.
    #[must_use]
    pub fn get_bool(&self, tier: SettingTier, field: &VenueFieldName) -> Option<bool> {
        let declared = vike_venues::venue_fields::venue_field(&self.venue, field)?;
        let value =
            if declared.tier_scoped { self.get_exact(tier, field) } else { self.get(tier, field) }?;
        declared.grammar.switch_value(value)
    }

    /// Every row, `(tier, field, value)`, in tier-then-field order.
    pub fn rows(&self) -> impl Iterator<Item = (SettingTier, &VenueFieldName, &str)> + '_ {
        self.rows.iter().map(|((t, f), v)| (*t, f, v.as_str()))
    }
}

/// `venue`'s `mark_streams` switch (decision 0095), read by its declared grammar through
/// [`VenueSettings::get_bool`], or `None` — the feed then keeps its charter default
/// (`vike_bridge_core::data_client::ws::DataClientWebsocket::with_mark_streams`). `None` is every one of: `venue`
/// has no entry in `venue_settings`, it has no machine-scoped (`Any`) row, the row is tier-scoped only, or the
/// stored value is no legal spelling (`1 x`, an empty value).
/// The ONE reader of the field: the gatewayhub's stream clients (`crates/vike-gatewayhub/src/data/venues.rs`'s
/// `build_client`), which own every venue's feed since decision 0164.
#[must_use]
pub fn mark_streams_setting(
    venue_settings: &BTreeMap<String, VenueSettings>,
    venue: &str,
) -> Option<bool> {
    venue_settings.get(venue).and_then(|s| s.get_bool(SettingTier::Any, &MARK_STREAMS))
}

/// **Every venue's `venue_setting` rows and `venue_url` addresses**, keyed by venue id — the one read
/// a composition root makes. A venue with either kind of row has an entry. A box with no settings
/// database, or one whose store predates the `venue_setting` table, has no setting rows; a box with
/// no database or no `venue_url` table yet reads the release's addresses
/// (`crate::venue_url::read_venue_urls`), so its entries carry the seed.
///
/// # Errors
/// A store that exists and cannot be read.
pub fn load_venue_settings(
    settings_dir: &Path,
) -> Result<BTreeMap<String, VenueSettings>, StoreError> {
    let rows = match crate::settings::read_settings_in(settings_dir)? {
        crate::SettingsSource::Rows { rows, .. } => rows.venue,
        crate::SettingsSource::NoDatabase { .. } | crate::SettingsSource::TablesAbsent { .. } => {
            Vec::new()
        }
    };
    let urls = crate::venue_url::read_venue_urls(settings_dir)?;
    Ok(venue_settings_from(&rows, &urls))
}

/// The release's addresses alone, as [`load_venue_settings`] shapes them: what a root that could
/// not read its store, or has none, hands its venue clients (each venue's built-in defaults).
#[must_use]
pub fn seeded_venue_settings() -> BTreeMap<String, VenueSettings> {
    venue_settings_from(&[], &crate::venue_url::seed_rows())
}

fn venue_settings_from(
    rows: &[crate::VenueSettingRow],
    urls: &[crate::venue_url::VenueUrlRow],
) -> BTreeMap<String, VenueSettings> {
    let mut venues: Vec<&str> = rows
        .iter()
        .map(|r| r.venue.as_str())
        .chain(urls.iter().map(|u| u.key.venue.as_str()))
        .collect();
    venues.sort_unstable();
    venues.dedup();
    venues
        .into_iter()
        .map(|v| {
            let settings = VenueSettings::from_rows(v, rows)
                .with_urls(crate::venue_url::venue_urls_of(urls, v));
            (v.to_string(), settings)
        })
        .collect()
}

/// **The grammar's own properties.**
///
/// ⚠ **No test here spells a whole `{PREFIX}_{NAME}` literal**: `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'
/// loose sweep reads an env-shaped literal in ANY `src/` file as evidence that file's CRATE reads
/// that variable, rows are keyed `(name, krate)`, and `vike-secrets` has no row for a venue key.
#[path = "venue_setting_tests.rs"]
#[cfg(test)]
mod venue_setting_tests;
