//! Pure credential-status enumerator: for each known bridge venue, which of Sim/Demo/Live have
//! API keys configured in the workspace `.env`. Reads through `vike_bridge_core::credentials`'s
//! `load_credentials_from` (the SINGLE naming authority for the generic `{VENUE}_{ENV}_API_KEY`/
//! `_API_SECRET` convention) for every venue that follows it, and a per-venue override (in `status::arms`) for
//! every venue whose bridge crate reads a bespoke `.env` shape instead — those overrides read the
//! exact var names each bridge's own credential loader reads, never invented independently.
//! [`venue_env_configured`]'s `match` is the roster of which is which; this paragraph deliberately
//! neither counts them nor names them, because the count was wrong twice.
//!
//! # ⚠ The generic grid is the FALLBACK, and falling through to it is a CLAIM
//!
//! A venue that reaches `load_credentials_for_account` is asserting that its bridge really reads
//! `{VENUE}_{TIER}_API_KEY`/`_API_SECRET`. When that is false the grid disagrees with the MOUNT in
//! both directions at once — a green dot beside a venue `vike_mount::make_engine` will leave on
//! paper, and no dot beside one it will arm. alpaca/ctrader/ibkr/polymarket each sat in that
//! fallback while reading none of those names (an OAuth client-credentials trio, an OAuth app pair
//! plus per-tier grant, one account number, one Ethereum private key), which is what put each of
//! them in the override list in `status::arms`.
//!
//! # ⚠ The grid is per ACCOUNT, and [`credential_status`] is its DEFAULT-account face
//!
//! A venue may hold more than one account since the mount began fanning out
//! (`vike_model::accounts::account_keys`), and a status grid keyed per VENUE answers for the default account
//! whatever account was asked about — so a labelled account's row on a screen would show somebody
//! else's tier dots. [`credential_status_for_account`] is the whole enumerator now and
//! [`credential_status`] calls it with [`AccountLabel::Default`].
//!
//! ⚠ **Every name below is composed through `vike_model::accounts::account_keys::account_key`**, reached via
//! `vike_bridge_core::credentials`' `account_var` / `load_credentials_for_account` — the same two
//! doors the MOUNT reads a labelled credential through. That grammar appends `__{LABEL}` after the
//! WHOLE of today's key and returns the key UNCHANGED for the default account, so:
//!
//! * a bespoke suffix (`OANDA_{TIER}_ACCOUNT_ID`, `IG_{TIER}_IDENTIFIER`, the FX `_USER`/
//!   `_PASSWORD` pair) needs no entry in any table here and none can be forgotten, and
//! * a single-account box reads the SAME `String`s it read before this existed — byte-identity is
//!   a property of the grammar, not of care at each site.
//!   `crates/vike-connections/tests/key_shapes/account_status.rs`'s `the_default_account_grid_is_unchanged`
//!   folds both faces over the whole roster and every tier and asserts it.
//!
//! ⚠ **The ACCOUNT tests are an integration test, not a `#[cfg(test)]` block here**, and the reason
//! is a gate rather than taste: every labelled fixture writes a `..__ALT` key name, and
//! `crates/vike-ops/tests/settings/settings_registry/directions.rs`'s `every_read_variable_is_declared` harvests
//! env-var-shaped literals out of `src/` and demands a `vike_ops::settings::SETTINGS` row for each.
//! The labelled half of the grid deliberately has no rows (an unbounded operator-named key set), so
//! such a literal cannot be declared and must not live under `src/`. That file's own module doc
//! carries the argument.
//!
//! ⚠ **The same rule caught a SECOND, unlabelled class**, which is why the per-venue key-shape
//! fixtures live in `crates/vike-connections/tests/key_shapes/read_shapes.rs` rather than in the
//! `#[cfg(test)]` block below. A bespoke name a bridge COMPOSES (`format!("IBKR_{tier}_ACCOUNT")`)
//! appears as no literal anywhere, so `SETTINGS` carries `IBKR_DEMO_ACCOUNT` and `IBKR_LIVE_ACCOUNT`
//! and no `IBKR_SIM_ACCOUNT` — likewise `CTRADER_SIM_ACCESS_TOKEN`, `ALPACA_LIVE_CLIENT_ID` and
//! `POLY_LIVE_PRIVATE_KEY`. Spelling one of those in a `src/` fixture would demand a registry row
//! asserting a read this file does not perform (it composes the name too), so the fixtures sit in a
//! test region, where the literal sweep does not look.

use std::collections::HashMap;

use vike_bridge_core::credentials::Environment;
use vike_model::VENUES;
use vike_model::accounts::account_keys::AccountLabel;

mod accounts;
mod arms;

pub use accounts::AccountGrids;
use arms::venue_env_configured;

/// Credential configuration status for one venue across the three environment tiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VenueCredStatus {
    pub venue: String,
    pub sim: bool,
    pub demo: bool,
    pub live: bool,
}

/// Compute credential status for every venue in [`VENUES`] against a parsed `.env`-shaped var
/// map — for **the DEFAULT account**, the account a single-account box has. Pure — never reads the
/// filesystem or process env itself (callers pass
/// `vike_bridge_core::credentials::load_workspace_dotenv()`'s output, or a test fixture).
///
/// ⚠ This is [`credential_status_for_account`] with [`AccountLabel::Default`], and that is the
/// whole of the difference: the grammar returns every key name unchanged for that account, so this
/// function reads the same map keys it has always read. Every existing caller means the default
/// account — the Connections grid edits it, and a screen that renders one row per VENUE has no
/// second account to describe.
pub fn credential_status(vars: &HashMap<String, String>) -> Vec<VenueCredStatus> {
    credential_status_for_account(vars, &AccountLabel::Default)
}

/// Compute credential status for every venue in [`VENUES`] for **ONE account** — the enumerator a
/// screen with one row per ACCOUNT needs.
///
/// A venue's labelled account is configured by `{VENUE}_{TIER}{SUFFIX}__{LABEL}` keys
/// (`vike_model::accounts::account_keys`), and asking this grid about the venue alone answers for the
/// DEFAULT account whatever account the row is about — a labelled row showing somebody else's tier
/// dots. `label` is what removes that.
///
/// ⚠ **Names and presence only, exactly as before.** Every lookup goes through
/// `vike_bridge_core::credentials`' `account_var`/`load_credentials_for_account`, both of which
/// answer `Option`, and nothing here holds, formats or returns a credential VALUE — the returned
/// [`VenueCredStatus`] is three bools and a venue id.
///
/// ⚠ **No fallback to the unlabelled key**, matching the loader the mount uses: a labelled account
/// whose credentials are absent reads ABSENT rather than borrowing the default account's, because
/// the opposite would show green dots beside an account that cannot sign anything.
pub fn credential_status_for_account(
    vars: &HashMap<String, String>,
    label: &AccountLabel,
) -> Vec<VenueCredStatus> {
    VENUES
        .iter()
        .map(|&venue| VenueCredStatus {
            venue: venue.to_string(),
            sim: venue_env_configured(venue, Environment::Sim, label, vars),
            demo: venue_env_configured(venue, Environment::Demo, label, vars),
            live: venue_env_configured(venue, Environment::Live, label, vars),
        })
        .collect()
}

/// **Which venues this box holds credentials for AT ALL** — one slug per venue with at least one
/// configured tier on the DEFAULT account.
///
/// The question `docs/decisions/0066`'s decision 9 made a surface need: a credentialed venue's
/// Instruments row must say whether the locally-credentialed refresh it names is ARMED, and "are
/// there keys here" is exactly [`credential_status`]'s answer collapsed across the three tiers.
///
/// ⚠ **DERIVED from [`credential_status`] rather than asking a second way**, which is the whole
/// value of the function: a row claiming "your alpaca keys are saved" beside a Connections grid
/// showing three empty dots would be two answers to one question. Every per-venue rule
/// ([`venue_env_configured`]'s bespoke arms) is inherited unchanged.
///
/// ⚠ **The DEFAULT account only**, matching [`credential_status`]: an Instruments row is a VENUE
/// row and has no account to be about. A box whose only alpaca keys sit under a labelled account
/// therefore reads as unconfigured here — declared rather than hidden, and the same simplification
/// that function's own doc argues for a screen with one row per venue.
///
/// ⚠ It is a PRESENCE claim and not a VERIFICATION one. Nothing here dials a venue; a key that is
/// present and wrong reads the same as a key that works, and the act the row names is what finds
/// out. `vike-backend catalog refresh` reports the venue's own error, which is where a bad key surfaces.
#[must_use]
pub fn credentialed_venues(vars: &HashMap<String, String>) -> Vec<String> {
    credential_status(vars)
        .into_iter()
        .filter(|v| v.sim || v.demo || v.live)
        .map(|v| v.venue)
        .collect()
}

#[path = "status_tests.rs"]
#[cfg(test)]
mod status_tests;
