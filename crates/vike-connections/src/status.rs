//! Pure credential-status enumerator: for each known bridge venue, which of Sim/Demo/Live have
//! API keys configured in the workspace `.env`. Reads through `vike_bridge_core::credentials`'s
//! `load_credentials_from` (the SINGLE naming authority for the generic `{VENUE}_{ENV}_API_KEY`/
//! `_API_SECRET` convention) for every venue that follows it, and a per-venue override (below) for
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
//! them in the override list below.
//!
//! # ⚠ The grid is per ACCOUNT, and [`credential_status`] is its DEFAULT-account face
//!
//! A venue may hold more than one account since the mount began fanning out
//! (`vike_model::account_keys`), and a status grid keyed per VENUE answers for the default account
//! whatever account was asked about — so a labelled account's row on a screen would show somebody
//! else's tier dots. [`credential_status_for_account`] is the whole enumerator now and
//! [`credential_status`] calls it with [`AccountLabel::Default`].
//!
//! ⚠ **Every name below is composed through `vike_model::account_keys::account_key`**, reached via
//! `vike_bridge_core::credentials`' `account_var` / `load_credentials_for_account` — the same two
//! doors the MOUNT reads a labelled credential through. That grammar appends `__{LABEL}` after the
//! WHOLE of today's key and returns the key UNCHANGED for the default account, so:
//!
//! * a bespoke suffix (`OANDA_{TIER}_ACCOUNT_ID`, `IG_{TIER}_IDENTIFIER`, the FX `_USER`/
//!   `_PASSWORD` pair) needs no entry in any table here and none can be forgotten, and
//! * a single-account box reads the SAME `String`s it read before this existed — byte-identity is
//!   a property of the grammar, not of care at each site.
//!   `crates/vike-connections/tests/account_status.rs`'s `the_default_account_grid_is_unchanged`
//!   folds both faces over the whole roster and every tier and asserts it.
//!
//! ⚠ **The ACCOUNT tests are an integration test, not a `#[cfg(test)]` block here**, and the reason
//! is a gate rather than taste: every labelled fixture writes a `..__ALT` key name, and
//! `crates/vike-ops/tests/settings_registry.rs`'s `every_read_variable_is_declared` harvests
//! env-var-shaped literals out of `src/` and demands a `vike_ops::settings::SETTINGS` row for each.
//! The labelled half of the grid deliberately has no rows (an unbounded operator-named key set), so
//! such a literal cannot be declared and must not live under `src/`. That file's own module doc
//! carries the argument.
//!
//! ⚠ **The same rule caught a SECOND, unlabelled class**, which is why the per-venue key-shape
//! fixtures live in `crates/vike-connections/tests/venue_key_shapes.rs` rather than in the
//! `#[cfg(test)]` block below. A bespoke name a bridge COMPOSES (`format!("IBKR_{tier}_ACCOUNT")`)
//! appears as no literal anywhere, so `SETTINGS` carries `IBKR_DEMO_ACCOUNT` and `IBKR_LIVE_ACCOUNT`
//! and no `IBKR_SIM_ACCOUNT` — likewise `CTRADER_SIM_ACCESS_TOKEN`, `ALPACA_LIVE_CLIENT_ID` and
//! `POLY_LIVE_PRIVATE_KEY`. Spelling one of those in a `src/` fixture would demand a registry row
//! asserting a read this file does not perform (it composes the name too), so the fixtures sit in a
//! test region, where the literal sweep does not look.

use std::collections::HashMap;

use vike_bridge_core::credentials::{Environment, account_var, load_credentials_for_account};
use vike_model::account_keys::{AccountLabel, split_account_key};

/// The bridge venues (`crates/bridges/*`), one crate per venue. Sourced from `vike-catalog`'s
/// `BRIDGE_VENUES` — the single authoritative venue-slug list (`CatalogProvider` impls are wired
/// ad hoc per binary and aren't cleanly enumerable, so `vike-catalog` exposes this flat const
/// instead; see `vike_catalog::venues` for the full rationale). Adding a venue bridge means
/// updating that ONE list — this crate picks it up automatically.
pub const VENUES: &[&str] = vike_catalog::BRIDGE_VENUES;

/// Credential configuration status for one venue across the three environment tiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VenueCredStatus {
    pub venue: String,
    pub sim: bool,
    pub demo: bool,
    pub live: bool,
}

/// Whether every one of `keys` is present in `vars` for `label`'s account with a non-blank
/// (trimmed) value — the same "unset/blank means absent" rule `load_credentials_from`/the bridge
/// config loaders use.
///
/// ⚠ Reached through `vike_bridge_core::credentials::account_var` rather than a bare `vars.get`,
/// so the account grammar is applied by the ONE function the mount applies it with and the
/// blank-is-absent rule stays that function's. [`AccountLabel::Default`] leaves each `keys` entry
/// untouched, so a single-account box's lookups are the same map keys they always were.
fn keys_present(vars: &HashMap<String, String>, keys: &[String], label: &AccountLabel) -> bool {
    keys.iter().all(|k| account_var(vars, k, label).is_some())
}

/// FXCM's primary `.env` var names at one env tier: `FXCM_{TIER}_USER`/`_PASSWORD` — verified
/// against `vike_fxcm::config::fxcm_env_var_names`/`load_fxcm_config_from`
/// (`crates/bridges/fxcm/src/config.rs`), which reads exactly these two (not `_API_KEY`).
fn fxcm_configured(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
    let tier_keys = |tier: &str| vec![format!("FXCM_{tier}_USER"), format!("FXCM_{tier}_PASSWORD")];
    keys_present(vars, &tier_keys(env.as_str()), label)
        || env.legacy_str().is_some_and(|legacy| keys_present(vars, &tier_keys(legacy), label))
}

/// OANDA's primary `.env` var names at one env tier: `OANDA_{TIER}_API_KEY`/`_ACCOUNT_ID` —
/// verified against `vike_oanda::config::oanda_env_var_names`/`load_oanda_config_from`
/// (`crates/bridges/oanda/src/config.rs`). OANDA does set `_API_KEY` (so the generic check isn't
/// totally blind to it) but also requires `_ACCOUNT_ID`, not `_API_SECRET` — the generic
/// `load_credentials_from` check looks for `_API_SECRET` and always misses, so this venue still
/// needs the override.
fn oanda_configured(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> bool {
    let tier_keys =
        |tier: &str| vec![format!("OANDA_{tier}_API_KEY"), format!("OANDA_{tier}_ACCOUNT_ID")];
    keys_present(vars, &tier_keys(env.as_str()), label)
        || env.legacy_str().is_some_and(|legacy| keys_present(vars, &tier_keys(legacy), label))
}

/// IG's primary `.env` var names at one env tier: `IG_{TIER}_API_KEY`/`_IDENTIFIER`/`_PASSWORD` —
/// verified against `vike_ig::config::ig_env_var_names`/`load_ig_config_from`
/// (`crates/bridges/ig/src/config.rs`). Same `_API_SECRET`-vs-`_IDENTIFIER`/`_PASSWORD` mismatch
/// as OANDA.
fn ig_configured(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
    let tier_keys = |tier: &str| {
        vec![
            format!("IG_{tier}_API_KEY"),
            format!("IG_{tier}_IDENTIFIER"),
            format!("IG_{tier}_PASSWORD"),
        ]
    };
    keys_present(vars, &tier_keys(env.as_str()), label)
        || env.legacy_str().is_some_and(|legacy| keys_present(vars, &tier_keys(legacy), label))
}

/// Dukascopy's primary `.env` var names: bespoke per-account `DUKASCOPY_DEMO1_LOGIN`/`_PASSWORD`
/// (+ `DEMO2`) — verified against `vike_dukascopy::config::dukascopy_env_var_names`/
/// `load_dukascopy_config_from` (`crates/bridges/dukascopy/src/config.rs`). There is no `SIM` or
/// `LIVE` tier for this venue today (no such vars exist anywhere in the bridge), so those two
/// columns are always `false`; `Demo` is `true` when either DEMO1 or DEMO2 is configured.
fn dukascopy_configured(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> bool {
    if env != Environment::Demo {
        return false;
    }
    keys_present(
        vars,
        &["DUKASCOPY_DEMO1_LOGIN".to_string(), "DUKASCOPY_DEMO1_PASSWORD".to_string()],
        label,
    ) || keys_present(
        vars,
        &["DUKASCOPY_DEMO2_LOGIN".to_string(), "DUKASCOPY_DEMO2_PASSWORD".to_string()],
        label,
    )
}

/// Aster's primary `.env` var names at one env tier: `ASTER_{TIER}_USER`/`_PRIVATE_KEY` (`SIGNER`
/// is optional/derived, not required) — verified against
/// `vike_aster::signing::load_aster_credentials` (`crates/bridges/aster/src/signing.rs`), which
/// reads exactly these two required vars. The bridge's own loader treats any non-`Live` tier as
/// `TESTNET`, but this app-facing status grid only ever asks for `Demo`/`Live`/`Sim` — `Demo` maps
/// to `TESTNET`, `Live` maps to `LIVE`, and `Sim` has no tier for this venue at all (mirrors
/// dukascopy's no-`Sim`-tier shape below).
fn aster_configured(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> bool {
    let tier = match env {
        Environment::Live => "LIVE",
        Environment::Demo => "TESTNET",
        Environment::Sim => return false,
    };
    keys_present(vars, &[format!("ASTER_{tier}_USER"), format!("ASTER_{tier}_PRIVATE_KEY")], label)
}

/// Hyperliquid's primary `.env` var name at one env tier: `HYPERLIQUID_{DEMO|LIVE}_PRIVATE_KEY`
/// (`_ACCOUNT_ADDRESS` is optional — an agent wallet signing for a master account — so it is NOT
/// required here) — verified against `vike_hyperliquid::config::load`
/// (`crates/bridges/hyperliquid/src/config.rs`), whose live gate is exactly "private key present
/// and non-blank". There is no `SIM` tier for this venue (DEMO = testnet, LIVE = mainnet), so
/// `Sim` is always `false` (mirrors dukascopy/aster's no-`Sim`-tier shape above).
fn hyperliquid_configured(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> bool {
    let tier = match env {
        Environment::Live => "LIVE",
        Environment::Demo => "DEMO",
        Environment::Sim => return false,
    };
    keys_present(vars, &[format!("HYPERLIQUID_{tier}_PRIVATE_KEY")], label)
}

/// Alpaca's primary `.env` var names at one env tier: `ALPACA_{SANDBOX|LIVE}_CLIENT_ID`/
/// `_CLIENT_SECRET`/`_ACCOUNT_ID` — verified against `vike_alpaca::config::load_alpaca_config_from`
/// / `load_alpaca_config_for_account` (`crates/bridges/alpaca/src/config.rs`), whose live gate is
/// exactly "all three present and non-blank". This venue auths with an OAuth2 CLIENT-CREDENTIALS
/// pair exchanged for a short-lived Bearer plus one PINNED account, so `_API_KEY`/`_API_SECRET` are
/// names its bridge never asks for.
///
/// ⚠ **`Sim` is always `false`, and the reason is the bridge's own tier map.**
/// `vike_alpaca::config::alpaca_tier` sends BOTH `Sim` and `Demo` to the single `SANDBOX` token, so
/// `Sim` is not a tier this venue has — it is a second spelling of `Demo`, reading byte-identical
/// key names. Lighting two columns from one key set would say an operator had configured something
/// they had not; the same call is made for `aster` above, whose loader collapses non-`Live` to
/// `TESTNET` in exactly this shape.
fn alpaca_configured(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> bool {
    let tier = match env {
        Environment::Live => "LIVE",
        Environment::Demo => "SANDBOX",
        Environment::Sim => return false,
    };
    keys_present(
        vars,
        &[
            format!("ALPACA_{tier}_CLIENT_ID"),
            format!("ALPACA_{tier}_CLIENT_SECRET"),
            format!("ALPACA_{tier}_ACCOUNT_ID"),
        ],
        label,
    )
}

/// cTrader's primary `.env` var names at one env tier: the APP-level `CTRADER_CLIENT_ID`/
/// `_CLIENT_SECRET` pair (one Spotware app registration, no tier token) PLUS the per-tier OAuth
/// grant `CTRADER_{TIER}_ACCESS_TOKEN`/`_REFRESH_TOKEN` — verified against
/// `vike_ctrader::config::CtraderConfig::from_vars` /
/// `from_vars_with_store_for_account` (`crates/bridges/ctrader/src/config.rs`), which takes exactly
/// those four through `?` and treats `CTRADER_{TIER}_ACCOUNT_ID` as OPTIONAL (discovered at connect
/// when unset), so it is not required here either.
///
/// ⚠ **All three tiers are real for this venue**, unlike alpaca's above: the tier token is
/// `Environment::as_str` verbatim, so `CTRADER_SIM_ACCESS_TOKEN` is a DISTINCT grant an operator can
/// hold, not a second spelling of the demo one. (`ctrader_host` does point `Sim` and `Demo` at the
/// same demo host — but a host is not a credential, and refusing to see a `SIM` grant the loader
/// would accept is the false-negative half of the same defect this override exists to close.)
///
/// ⚠ **No legacy-`MAINNET` rung**: that loader spells `env.as_str()` and never calls
/// `Environment::legacy_str`, so neither does this.
fn ctrader_configured(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> bool {
    let tier = env.as_str();
    keys_present(
        vars,
        &[
            "CTRADER_CLIENT_ID".to_string(),
            "CTRADER_CLIENT_SECRET".to_string(),
            format!("CTRADER_{tier}_ACCESS_TOKEN"),
            format!("CTRADER_{tier}_REFRESH_TOKEN"),
        ],
        label,
    )
}

/// IBKR's primary `.env` var name at one env tier: `IBKR_{TIER}_ACCOUNT`, and that ALONE —
/// verified against `vike_ibkr::config::load_ibkr_config_from` / `load_ibkr_config_for_account`
/// (`crates/bridges/vike-ibkr/src/config.rs`), whose only `?` on an absent value is the account
/// number ("ABSENT ACCOUNT → `None` → the app root keeps the venue paper"). Every other name that
/// loader reads — `_HOST`/`_PORT`/`_CLIENT_ID`/`_DATA_CLIENT_ID`/`_BACKEND`/`_CPAPI_URL`/
/// `_MKTDATA_TYPE` — has a default, so requiring any of them here would report a working mount as
/// unconfigured. The socket API has no in-crate auth at all (the Gateway holds the login), so
/// `_API_KEY`/`_API_SECRET` name nothing on this venue.
///
/// ⚠ **What this deliberately does NOT model**: that loader also answers `None` for a PRESENT but
/// unparseable optional value (`_BACKEND=grpc`, a non-numeric `_PORT`). This grid answers presence,
/// like every other row here, so such a store reads configured while the mount lands on paper. It
/// is a strictly smaller error than the one being fixed — the operator wrote the key, and the mount
/// says so out loud — and modelling it would mean parsing values in a function whose whole contract
/// is that it touches names only.
fn ibkr_configured(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
    keys_present(vars, &[format!("IBKR_{}_ACCOUNT", env.as_str())], label)
}

/// Polymarket's primary `.env` var name: `POLY_PRIVATE_KEY` — the Ethereum L1 key that signs and
/// derives the L2 trio — reached through the loader's own three-rung chain, tier-suffixed
/// (`POLY_LIVE_PRIVATE_KEY`) then legacy-tier (`POLY_MAINNET_PRIVATE_KEY`) then the tier-less
/// spelling the real store uses. Verified against
/// `vike_polymarket::config::load_polymarket_creds_from` / `load_polymarket_creds_for_account`
/// (`crates/bridges/polymarket/src/config.rs`), whose gate is exactly "the private key is present
/// and non-blank"; the L2 `_API_KEY`/`_SECRET`/`_PASSPHRASE` are OPTIONAL (derived at connect via
/// EIP-712) and so are the `_ADDRESS`/`_FUNDER`/relayer names.
///
/// ⚠ **The venue prefix is `POLY_`, not `POLYMARKET_`** — the roster slug and the credential prefix
/// disagree, which is exactly why this venue fell through to a generic `POLYMARKET_{TIER}_API_KEY`
/// grid that nothing in the tree ever writes or reads.
///
/// ⚠ **`Sim` and `Demo` are always `false`: this venue has NO testnet.** Every key it reads signs
/// real money on Polygon mainnet (`crates/bridges/polymarket/CLAUDE.md`), and `vike_mount`'s
/// `("polymarket", _)` arm refuses anything below a `live` ceiling outright
/// (`vike_config::ArmingBlock::LiveOnlyArm`). The loader would answer `Some` for `Sim`/`Demo` —
/// its tier-less rung matches under any tier — so this is the one place the grid states a fact the
/// loader alone cannot: a paper-tier dot here would promise a sandbox that does not exist.
fn polymarket_configured(
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> bool {
    if env != Environment::Live {
        return false;
    }
    let tier_key = |tier: &str| keys_present(vars, &[format!("POLY_{tier}_PRIVATE_KEY")], label);
    tier_key(env.as_str())
        || env.legacy_str().is_some_and(tier_key)
        || keys_present(vars, &["POLY_PRIVATE_KEY".to_string()], label)
}

/// Per-venue "is this env tier configured" check. Every venue named in the `match` reads a bespoke
/// `.env` shape its own bridge loader defines — see each `*_configured` fn's doc comment for the
/// exact var names and the loader they were verified against. Only a venue that genuinely reads the
/// generic `{VENUE}_{ENV}_API_KEY`/`_API_SECRET` pair may fall through to
/// `load_credentials_for_account`, the shared naming authority: the fallback is a CLAIM about the
/// bridge, not a default (this module's doc argues it).
fn venue_env_configured(
    venue: &str,
    env: Environment,
    label: &AccountLabel,
    vars: &HashMap<String, String>,
) -> bool {
    match venue {
        "fxcm" => fxcm_configured(env, label, vars),
        "oanda" => oanda_configured(env, label, vars),
        "ig" => ig_configured(env, label, vars),
        "dukascopy" => dukascopy_configured(env, label, vars),
        "aster" => aster_configured(env, label, vars),
        "hyperliquid" => hyperliquid_configured(env, label, vars),
        "alpaca" => alpaca_configured(env, label, vars),
        "ctrader" => ctrader_configured(env, label, vars),
        "ibkr" => ibkr_configured(env, label, vars),
        "polymarket" => polymarket_configured(env, label, vars),
        _ => load_credentials_for_account(venue, env, label, vars).is_some(),
    }
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
/// (`vike_model::account_keys`), and asking this grid about the venue alone answers for the
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

/// **Every account the Connections grid can show, each with its own tier grid** — the input a
/// screen that renders ONE account at a time takes, and the shape
/// `crates/vike-app-core/src/tool_views/venues.rs`'s `VenueArmingInputs` already has for the same
/// reason (a `creds` for the default account plus a `labelled_creds` beside it).
///
/// ⚠ **`labelled` is EMPTY on a single-account box**, and that is what makes "unchanged" a
/// structural property rather than a careful one: with no labelled account the view has exactly
/// one account to offer, reads [`Self::default_grid`] — the very `Vec` [`credential_status`]
/// always produced — and composes every key name through [`AccountLabel::Default`], which
/// `vike_model::account_keys::account_key` returns unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountGrids {
    /// The DEFAULT account's grid — what [`credential_status`] returns, verbatim.
    default_grid: Vec<VenueCredStatus>,
    /// One entry per LABELLED account the store actually holds, sorted by label and deduplicated.
    labelled: Vec<(AccountLabel, Vec<VenueCredStatus>)>,
}

impl AccountGrids {
    /// **The derivation.** The default account's grid, plus one grid per labelled account the store
    /// holds — *"holds"* meaning **this grid lights at least one dot for it**, which is the only
    /// definition that cannot disagree with what the screen then shows.
    ///
    /// # Why not `vike_model::account_keys::accounts_in_store`
    ///
    /// That function is the store's own account enumerator and it is the right one for a screen
    /// built on `{VENUE}_{TIER}{SUFFIX}` keys — but it answers `None` for several of the credential
    /// families this grid genuinely edits, by its own documented classification, and it does so for
    /// TWO structurally different reasons:
    ///
    /// * **a tier token outside `vike_model::credential_keys::CREDENTIAL_TIERS`** — aster's
    ///   `TESTNET`, alpaca's `SANDBOX`, dukascopy's account-indexed `DEMO1`/`DEMO2` — so
    ///   `account_ref_from_key` finds no tier and skips the key; and
    /// * **a key prefix that is not a roster venue at all** — polymarket's store spelling is
    ///   `POLY_`, which `venue_prefix_of` cannot match against the `polymarket` slug (that venue's
    ///   own `load_polymarket_creds_for_account` says so in its doc).
    ///
    /// Every one of those is a shape `crates/vike-connections/tests/account_status.rs`'s
    /// `every_bespoke_shape_is_reachable_by_label` proves the READ side supports, so they would be
    /// accounts an operator could store credentials for and never see listed.
    /// `crates/vike-connections/tests/account_grids.rs`'s
    /// `the_store_enumerator_cannot_see_the_non_conforming_families` measures that gap rather
    /// than asserting it away.
    ///
    /// ⚠ Deliberately no COUNT of either the families or the bespoke shapes: this paragraph carried
    /// "two of the six" until the alpaca/ctrader/ibkr/polymarket arms landed and made it "four of
    /// the ten" in one commit. The test named above enumerates them; read it there.
    ///
    /// So the label set comes from `split_account_key` — the SAME parse, one rung lower, applied to
    /// the whole key rather than to a classified one — and the "does it exist" question is then
    /// answered by [`credential_status_for_account`] itself. A store key that is not a credential
    /// at all (an attribution code, a sidecar path) lights nothing and drops out; a malformed label
    /// fails the parse and drops out. Neither can produce an account nobody can fill in.
    ///
    /// Pure, like everything else here: the caller supplies the parsed store.
    #[must_use]
    pub fn from_vars(vars: &HashMap<String, String>) -> Self {
        let mut labels: Vec<AccountLabel> = vars
            .keys()
            .filter_map(|k| split_account_key(k).ok())
            .filter(|s| !s.label.is_default())
            .map(|s| s.label)
            .collect();
        labels.sort();
        labels.dedup();
        let labelled = labels
            .into_iter()
            .map(|l| {
                let grid = credential_status_for_account(vars, &l);
                (l, grid)
            })
            .filter(|(_, grid)| grid.iter().any(|s| s.sim || s.demo || s.live))
            .collect();
        Self { default_grid: credential_status(vars), labelled }
    }

    /// Already-computed grids, injected — the constructor a caller that is not deriving from a
    /// store uses, and the one a test harness that wants ONE venue's row rather than the whole
    /// roster needs. [`Self::from_vars`] is the DERIVATION and the only thing a production caller
    /// should reach for; this one asserts nothing about whether `labelled` agrees with any store.
    #[must_use]
    pub fn new(
        default_grid: Vec<VenueCredStatus>,
        labelled: Vec<(AccountLabel, Vec<VenueCredStatus>)>,
    ) -> Self {
        Self { default_grid, labelled }
    }

    /// A single-account view over an already-computed grid — the shape a test harness that injects
    /// one venue's row needs, and the shape a caller with no store to enumerate has.
    #[must_use]
    pub fn single(default_grid: Vec<VenueCredStatus>) -> Self {
        Self::new(default_grid, Vec::new())
    }

    /// The DEFAULT account's grid.
    #[must_use]
    pub fn default_grid(&self) -> &[VenueCredStatus] {
        &self.default_grid
    }

    /// This account's grid, or `None` when the store holds nothing for it.
    ///
    /// ⚠ `None` is NOT "fall back to the default account" — it is the same no-borrowing rule
    /// [`credential_status_for_account`] states: an account with no keys of its own reads absent,
    /// because green dots beside an account that cannot sign anything is the one answer worse than
    /// no answer. It is also the ordinary state of an account an operator has just NAMED and not
    /// yet filled in.
    #[must_use]
    pub fn grid_for(&self, label: &AccountLabel) -> Option<&[VenueCredStatus]> {
        if label.is_default() {
            return Some(&self.default_grid);
        }
        self.labelled.iter().find(|(l, _)| l == label).map(|(_, g)| g.as_slice())
    }

    /// The LABELLED accounts, in order. Empty on a single-account box.
    pub fn labels(&self) -> impl Iterator<Item = &AccountLabel> + '_ {
        self.labelled.iter().map(|(l, _)| l)
    }

    /// An all-absent grid over the same venues, in the same order — what a NAMED-but-unfilled
    /// account renders as. Built from [`Self::default_grid`]'s venue list rather than from
    /// [`VENUES`] so a caller that injected a subset of the roster gets its subset back.
    #[must_use]
    pub fn absent_grid(&self) -> Vec<VenueCredStatus> {
        self.default_grid
            .iter()
            .map(|s| VenueCredStatus {
                venue: s.venue.clone(),
                sim: false,
                demo: false,
                live: false,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_vars_all_absent() {
        let vars = HashMap::new();
        let statuses = credential_status(&vars);
        assert_eq!(statuses.len(), VENUES.len());
        for s in &statuses {
            assert!(!s.sim, "{} sim should be false", s.venue);
            assert!(!s.demo, "{} demo should be false", s.venue);
            assert!(!s.live, "{} live should be false", s.venue);
        }
    }

    #[test]
    fn binance_live_configured_detected() {
        let mut vars = HashMap::new();
        vars.insert("BINANCE_LIVE_API_KEY".to_string(), "key".to_string());
        vars.insert("BINANCE_LIVE_API_SECRET".to_string(), "secret".to_string());
        let statuses = credential_status(&vars);
        let binance = statuses.iter().find(|s| s.venue == "binance").expect("binance present");
        assert!(binance.live);
        assert!(!binance.sim);
        assert!(!binance.demo);
    }

    #[test]
    fn venue_list_order_is_stable() {
        let vars = HashMap::new();
        let statuses = credential_status(&vars);
        let names: Vec<&str> = statuses.iter().map(|s| s.venue.as_str()).collect();
        assert_eq!(names, VENUES.to_vec());
    }

    #[test]
    fn never_panics_on_arbitrary_vars() {
        let mut vars = HashMap::new();
        vars.insert("SOME_RANDOM_KEY".to_string(), "".to_string());
        vars.insert("".to_string(), "".to_string());
        let _ = credential_status(&vars);
    }

    /// FXCM's bespoke `_USER`/`_PASSWORD` shape: the generic `_API_KEY`/`_API_SECRET` check
    /// would always report "not set" for it, so this override must detect the real vars.
    #[test]
    fn fxcm_demo_user_password_detected() {
        let mut vars = HashMap::new();
        vars.insert("FXCM_DEMO_USER".to_string(), "D251112911".to_string());
        vars.insert("FXCM_DEMO_PASSWORD".to_string(), "s3cr3t".to_string());
        let statuses = credential_status(&vars);
        let fxcm = statuses.iter().find(|s| s.venue == "fxcm").expect("fxcm present");
        assert!(fxcm.demo, "fxcm demo should be detected via FXCM_DEMO_USER/_PASSWORD");
        assert!(!fxcm.sim);
        assert!(!fxcm.live);
    }

    /// FXCM's legacy `MAINNET` tier still counts as `Live`, mirroring the generic loader's
    /// legacy-tier fallback.
    #[test]
    fn fxcm_legacy_mainnet_counts_as_live() {
        let mut vars = HashMap::new();
        vars.insert("FXCM_MAINNET_USER".to_string(), "u".to_string());
        vars.insert("FXCM_MAINNET_PASSWORD".to_string(), "p".to_string());
        let statuses = credential_status(&vars);
        let fxcm = statuses.iter().find(|s| s.venue == "fxcm").expect("fxcm present");
        assert!(fxcm.live);
    }

    /// OANDA sets `_API_KEY` but not `_API_SECRET` — the generic check would always miss it
    /// even with real credentials present. The override must check `_ACCOUNT_ID` instead.
    #[test]
    fn oanda_api_key_and_account_id_detected() {
        let mut vars = HashMap::new();
        vars.insert("OANDA_DEMO_API_KEY".to_string(), "tok-abc-123".to_string());
        vars.insert("OANDA_DEMO_ACCOUNT_ID".to_string(), "101-004-1234567-001".to_string());
        let statuses = credential_status(&vars);
        let oanda = statuses.iter().find(|s| s.venue == "oanda").expect("oanda present");
        assert!(oanda.demo);
        // the API key alone (no account id) must NOT count as configured
        let mut key_only = HashMap::new();
        key_only.insert("OANDA_DEMO_API_KEY".to_string(), "tok-abc-123".to_string());
        let statuses = credential_status(&key_only);
        let oanda = statuses.iter().find(|s| s.venue == "oanda").expect("oanda present");
        assert!(!oanda.demo, "api key alone (no account id) must not count as configured");
    }

    /// IG needs all three of `_API_KEY`/`_IDENTIFIER`/`_PASSWORD`.
    #[test]
    fn ig_all_three_fields_required() {
        let mut vars = HashMap::new();
        vars.insert("IG_DEMO_API_KEY".to_string(), "key-xyz".to_string());
        vars.insert("IG_DEMO_IDENTIFIER".to_string(), "id".to_string());
        let statuses = credential_status(&vars);
        let ig = statuses.iter().find(|s| s.venue == "ig").expect("ig present");
        assert!(!ig.demo, "missing password must not count as configured");

        vars.insert("IG_DEMO_PASSWORD".to_string(), "pw".to_string());
        let statuses = credential_status(&vars);
        let ig = statuses.iter().find(|s| s.venue == "ig").expect("ig present");
        assert!(ig.demo);
    }

    /// Dukascopy has no Sim/Live tier at all; Demo is true when either DEMO1 or DEMO2 is set.
    #[test]
    fn dukascopy_demo1_and_demo2_detected_no_sim_or_live() {
        let mut vars = HashMap::new();
        vars.insert("DUKASCOPY_DEMO1_LOGIN".to_string(), "DEMO2cGyrc".to_string());
        vars.insert("DUKASCOPY_DEMO1_PASSWORD".to_string(), "s3cr3t".to_string());
        let statuses = credential_status(&vars);
        let duka = statuses.iter().find(|s| s.venue == "dukascopy").expect("dukascopy present");
        assert!(duka.demo);
        assert!(!duka.sim, "dukascopy has no SIM tier");
        assert!(!duka.live, "dukascopy has no LIVE tier");

        let mut demo2 = HashMap::new();
        demo2.insert("DUKASCOPY_DEMO2_LOGIN".to_string(), "u".to_string());
        demo2.insert("DUKASCOPY_DEMO2_PASSWORD".to_string(), "p".to_string());
        let statuses = credential_status(&demo2);
        let duka = statuses.iter().find(|s| s.venue == "dukascopy").expect("dukascopy present");
        assert!(duka.demo, "DEMO2 alone should also be detected");
    }

    /// Aster's `Demo` tier reads `ASTER_TESTNET_*`, not `ASTER_DEMO_*` — the bridge's own loader
    /// naming, not the generic app-tier naming.
    #[test]
    fn aster_testnet_user_and_private_key_detected() {
        let mut vars = HashMap::new();
        vars.insert("ASTER_TESTNET_USER".to_string(), "0xUser".to_string());
        vars.insert("ASTER_TESTNET_PRIVATE_KEY".to_string(), "0xkey".to_string());
        let statuses = credential_status(&vars);
        let aster = statuses.iter().find(|s| s.venue == "aster").expect("aster present");
        assert!(aster.demo);
        assert!(!aster.sim);
        assert!(!aster.live);
    }

    /// Aster's `Live` tier reads `ASTER_LIVE_*`.
    #[test]
    fn aster_live_user_and_private_key_detected() {
        let mut vars = HashMap::new();
        vars.insert("ASTER_LIVE_USER".to_string(), "0xUser".to_string());
        vars.insert("ASTER_LIVE_PRIVATE_KEY".to_string(), "0xkey".to_string());
        let statuses = credential_status(&vars);
        let aster = statuses.iter().find(|s| s.venue == "aster").expect("aster present");
        assert!(aster.live);
        assert!(!aster.demo);
    }

    /// `USER` alone (no `PRIVATE_KEY`) must not count as configured; `SIGNER` is optional but
    /// `PRIVATE_KEY` is not.
    #[test]
    fn aster_user_alone_is_not_configured() {
        let mut vars = HashMap::new();
        vars.insert("ASTER_TESTNET_USER".to_string(), "0xUser".to_string());
        let statuses = credential_status(&vars);
        let aster = statuses.iter().find(|s| s.venue == "aster").expect("aster present");
        assert!(!aster.demo, "private key missing must not count as configured");
    }

    /// Aster has no `Sim` tier — `sim` must stay `false` even if `ASTER_SIM_*` vars happen to be
    /// set (mirrors dukascopy's no-`Sim`/`Live`-tier shape).
    #[test]
    fn aster_sim_never_configured() {
        let mut vars = HashMap::new();
        vars.insert("ASTER_SIM_USER".to_string(), "0xUser".to_string());
        vars.insert("ASTER_SIM_PRIVATE_KEY".to_string(), "0xkey".to_string());
        let statuses = credential_status(&vars);
        let aster = statuses.iter().find(|s| s.venue == "aster").expect("aster present");
        assert!(!aster.sim, "aster has no SIM tier");
    }

    /// Hyperliquid appears in the grid at all (it was missing from `BRIDGE_VENUES` until the
    /// #498 roster audit), and its bespoke `HYPERLIQUID_{DEMO|LIVE}_PRIVATE_KEY` shape is
    /// detected — the generic `_API_KEY`/`_API_SECRET` check would always report "not set".
    /// `_ACCOUNT_ADDRESS` is optional (agent-wallet mode) and must NOT be required.
    #[test]
    fn hyperliquid_private_key_detected_per_tier() {
        let mut vars = HashMap::new();
        vars.insert("HYPERLIQUID_DEMO_PRIVATE_KEY".to_string(), "0xabc123".to_string());
        let statuses = credential_status(&vars);
        let hl = statuses.iter().find(|s| s.venue == "hyperliquid").expect("hyperliquid present");
        assert!(hl.demo, "demo private key alone (no account address) should be detected");
        assert!(!hl.live);
        assert!(!hl.sim);

        vars.insert("HYPERLIQUID_LIVE_PRIVATE_KEY".to_string(), "0xdeadbeef".to_string());
        let statuses = credential_status(&vars);
        let hl = statuses.iter().find(|s| s.venue == "hyperliquid").expect("hyperliquid present");
        assert!(hl.live);
    }

    /// Hyperliquid has no `Sim` tier — `sim` must stay `false` even if `HYPERLIQUID_SIM_*` vars
    /// happen to be set (mirrors dukascopy/aster).
    #[test]
    fn hyperliquid_sim_never_configured() {
        let mut vars = HashMap::new();
        vars.insert("HYPERLIQUID_SIM_PRIVATE_KEY".to_string(), "0xkey".to_string());
        let statuses = credential_status(&vars);
        let hl = statuses.iter().find(|s| s.venue == "hyperliquid").expect("hyperliquid present");
        assert!(!hl.sim, "hyperliquid has no SIM tier");
    }
}
