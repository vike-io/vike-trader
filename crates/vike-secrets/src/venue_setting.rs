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

use crate::db::DbError;

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
/// that the second segment is a tier **iff `vike_model::AccountTier`'s one parser takes it** —
/// exactly `paper`, `demo` or `live`. The pre-rename `sim` spelling of `paper` is NOT a tier any
/// more (decision 0129): `venue.ibkr.sim.backend` is the MACHINE-scoped field `SIM.BACKEND`, and
/// so is an uppercase `LIVE` segment. That is safe because no field
/// this grammar carries is spelled as a tier, and
/// `a_field_is_never_mistaken_for_a_tier` is what keeps it that way.
#[must_use]
pub fn venue_setting_key(venue: &str, tier: Option<AccountTier>, field: &str) -> String {
    let field = field.to_lowercase();
    match tier {
        Some(t) => format!("venue.{venue}.{t}.{field}"),
        None => format!("venue.{venue}.{field}"),
    }
}

/// The inverse of [`venue_setting_key`] — `None` for a key that is not one of these.
///
/// The FIELD comes back in the STORE's uppercase spelling (`venue_setting.field`); the venue comes
/// back lowercased, which is how the `account` table spells it, and the tier as the
/// `vike_model::AccountTier` its strict parser read.
#[must_use]
pub fn parse_venue_setting_key(key: &str) -> Option<(String, Option<AccountTier>, String)> {
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
        Some((tier, field)) => (venue.to_lowercase(), Some(tier), field.to_uppercase()),
        None if rest.is_empty() => return None,
        None => (venue.to_lowercase(), None, rest.to_uppercase()),
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

/// **One venue's `venue_setting` rows, read once by a composition root and passed down as data** —
/// the mount contract's `settings` input (`docs/decisions/0096`). Fields are keyed in the store's
/// upper-case spelling; [`VenueSettings::get`] takes either spelling, so a caller writes the
/// catalog's (`vike_venues::venue_fields`).
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
            let tier = SettingTier::of_row(r.tier);
            out.rows.insert((tier, r.field.to_ascii_uppercase()), r.value.clone());
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

/// `venue`'s stored `mark_streams` row (decision 0095), or `None` — the feed then keeps its charter
/// default (`vike_bridge_core::mark_streams_from`). `None` is every one of: `venue` has no entry in
/// `venue_settings`, it has no machine-scoped (`Any`) row, or the row is tier-scoped only; a row
/// that exists is returned as stored (an empty value included) and judged by the feed, not here.
/// The ONE reader of the field: the trading daemon's feed wiring (`crates/vike-tradehub/src/feeds.rs`)
/// and the data server's `build_client` (`crates/vike-datahub/src/feeds/venues.rs`) both call it.
#[must_use]
pub fn mark_streams_setting<'a>(
    venue_settings: &'a BTreeMap<String, VenueSettings>,
    venue: &str,
) -> Option<&'a str> {
    venue_settings.get(venue).and_then(|s| s.get(SettingTier::Any, "mark_streams"))
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
/// `None` — [`vike_venues::venue_fields::venue_field`] keyed the way the store spells the
/// field. Exact: `rate_gate` is not the store's spelling and answers `None`.
#[must_use]
pub fn declared_field(
    venue: &str,
    stored_field: &str,
) -> Option<&'static vike_venues::venue_fields::VenueField> {
    vike_venues::venue_fields::fields_of(venue)
        .find(|f| f.field.to_ascii_uppercase() == stored_field)
}

/// **The grammar's own properties.**
///
/// ⚠ **No test here spells a whole `{PREFIX}_{NAME}` literal**: `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'
/// loose sweep reads an env-shaped literal in ANY `src/` file as evidence that file's CRATE reads
/// that variable, rows are keyed `(name, krate)`, and `vike-secrets` has no row for a venue key.
#[path = "venue_setting_tests.rs"]
#[cfg(test)]
mod venue_setting_tests;
