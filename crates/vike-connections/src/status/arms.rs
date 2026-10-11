//! The per-venue READ arms (which `.env` keys light a tier's dot) and the `venue_env_configured` dispatcher.

use std::collections::HashMap;

use vike_bridge_core::credentials::{Environment, account_var, load_credentials_for_account};
use vike_model::accounts::account_keys::AccountLabel;

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
    let tier = env.as_str();
    keys_present(vars, &[format!("FXCM_{tier}_USER"), format!("FXCM_{tier}_PASSWORD")], label)
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
    let tier = env.as_str();
    keys_present(
        vars,
        &[format!("OANDA_{tier}_API_KEY"), format!("OANDA_{tier}_ACCOUNT_ID")],
        label,
    )
}

/// IG's primary `.env` var names at one env tier: `IG_{TIER}_API_KEY`/`_IDENTIFIER`/`_PASSWORD` —
/// verified against `vike_ig::config::ig_env_var_names`/`load_ig_config_from`
/// (`crates/bridges/ig/src/config.rs`). Same `_API_SECRET`-vs-`_IDENTIFIER`/`_PASSWORD` mismatch
/// as OANDA.
fn ig_configured(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
    let tier = env.as_str();
    keys_present(
        vars,
        &[
            format!("IG_{tier}_API_KEY"),
            format!("IG_{tier}_IDENTIFIER"),
            format!("IG_{tier}_PASSWORD"),
        ],
        label,
    )
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
/// number ("ABSENT ACCOUNT → `None` → the app root keeps the venue paper"). Every other credential
/// name that loader reads — `_CLIENT_ID`/`_DATA_CLIENT_ID`/`_MKTDATA_TYPE` — has a default, so
/// requiring any of them here would report a working mount as unconfigured; the gateway
/// (`backend`/`cpapi_url`/`host`/`port`) is not a credential at all since decision 0095's Task 7 but the tier's
/// `venue.ibkr.<tier>.*` settings. The socket API has no in-crate auth at all (the Gateway holds the
/// login), so `_API_KEY`/`_API_SECRET` name nothing on this venue.
///
/// ⚠ **What this deliberately does NOT model**: that loader also answers `None` for a PRESENT but
/// unparseable gateway setting (`backend = grpc`, a non-numeric `port`). This grid answers
/// credential PRESENCE, like every other row here, so such a box reads configured while the mount
/// lands on paper. It is a strictly smaller error than the one being fixed — the operator wrote the
/// value, and the mount says so out loud — and modelling it would mean reading settings in a
/// function whose whole contract is that it touches credential names only.
fn ibkr_configured(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
    keys_present(vars, &[format!("IBKR_{}_ACCOUNT", env.as_str())], label)
}

/// Polymarket's primary `.env` var name: `POLY_PRIVATE_KEY` — the Ethereum L1 key that signs and
/// derives the L2 trio — reached through the loader's own two-rung chain, tier-suffixed
/// (`POLY_LIVE_PRIVATE_KEY`) then the tier-less spelling the real store uses. Verified against
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
/// real money on Polygon mainnet (`crates/bridges/polymarket/CLAUDE.md`), and its bridge's
/// `crates/bridges/polymarket/src/exec_plane/mount.rs`'s `PolymarketVenueMount` refuses anything
/// below a `live` tier outright (`PaperCause::LiveOnlyArm`, which the contract fold reports as
/// `vike_config::ArmingBlock::LiveOnlyArm`). The loader would answer `Some` for `Sim`/`Demo` —
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
    keys_present(vars, &[format!("POLY_{}_PRIVATE_KEY", env.as_str())], label)
        || keys_present(vars, &["POLY_PRIVATE_KEY".to_string()], label)
}

/// Per-venue "is this env tier configured" check. Every venue named in the `match` reads a bespoke
/// `.env` shape its own bridge loader defines — see each `*_configured` fn's doc comment for the
/// exact var names and the loader they were verified against. Only a venue that genuinely reads the
/// generic `{VENUE}_{ENV}_API_KEY`/`_API_SECRET` pair may fall through to
/// `load_credentials_for_account`, the shared naming authority: the fallback is a CLAIM about the
/// bridge, not a default (the parent `status` module's doc argues it).
pub(super) fn venue_env_configured(
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

#[path = "arms_tests.rs"]
#[cfg(test)]
mod arms_tests;
