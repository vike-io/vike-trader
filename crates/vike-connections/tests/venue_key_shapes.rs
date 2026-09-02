//! **Every venue's row reads the key names its own bridge reads** — the per-venue half of
//! `vike_connections::status`, and the byte-identity guard over the venues this file did not touch.
//!
//! # Why an integration test rather than a `#[cfg(test)]` block in `status.rs`
//!
//! Two reasons, both gates rather than taste, and `status.rs`' module doc carries both. The one
//! that decides THIS file: a bespoke key a bridge COMPOSES (`format!("IBKR_{tier}_ACCOUNT")`)
//! appears as no literal anywhere in the tree, so `vike_ops::settings::SETTINGS` declares
//! `IBKR_DEMO_ACCOUNT` and `IBKR_LIVE_ACCOUNT` and no `IBKR_SIM_ACCOUNT` — and the same for
//! `CTRADER_SIM_ACCESS_TOKEN`, `ALPACA_LIVE_CLIENT_ID` and `POLY_LIVE_PRIVATE_KEY`. Spelling one of
//! those in a `src/` fixture would make `crates/vike-ops/tests/settings_registry.rs`'s
//! `every_read_variable_is_declared` demand a registry row asserting a read `status.rs` does not
//! perform, since it composes the name too. A `tests/` file is a test region, where that literal
//! sweep deliberately does not look.
//!
//! # What is gated
//!
//! 1. **The four repaired venues** (alpaca / ctrader / ibkr / polymarket) read what their bridge
//!    loaders read, one test each, each citing the loader it was verified against.
//! 2. **The generic `{VENUE}_{TIER}_API_KEY`/`_API_SECRET` grid buys those four NOTHING** — the
//!    defect itself, stated as the property that fails without the fix.
//! 3. **Every other venue is byte-identical**, as an EQUALITY against `baseline` — a frozen copy of
//!    the pre-repair `venue_env_configured` — folded over a fixture that configures the whole
//!    roster in every shape. Not a pin of today's answer: both sides are computed.

use std::collections::HashMap;

use vike_bridge_core::credentials::{account_var, load_credentials_for_account, Environment};
use vike_connections::{credential_status, VenueCredStatus, VENUES};
use vike_model::account_keys::AccountLabel;

/// The venues this branch changed. Every other roster venue must answer exactly what `baseline`
/// answers, and that is the whole content of [`untouched_venues_are_unchanged`].
const REPAIRED: &[&str] = &["alpaca", "ctrader", "ibkr", "polymarket"];

/// **The FROZEN pre-repair `venue_env_configured`**, copied arm for arm from `origin/main` so the
/// byte-identity claim is an equality between two computations rather than a table of expected
/// bools somebody could have written to match whatever the code now does.
///
/// ⚠ It is a BASELINE, not a duplicate implementation: its job is to state what this grid answered
/// BEFORE, permanently. A future PR that deliberately changes an untouched venue's arm must edit
/// the matching arm here in the same commit — the failure is the point, because "a venue nobody
/// meant to touch moved" is exactly what these tests exist to notice.
mod baseline {
    use super::{account_var, load_credentials_for_account, AccountLabel, Environment, HashMap};

    fn keys_present(vars: &HashMap<String, String>, keys: &[String], label: &AccountLabel) -> bool {
        keys.iter().all(|k| account_var(vars, k, label).is_some())
    }

    fn fxcm(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
        let tier_keys =
            |tier: &str| vec![format!("FXCM_{tier}_USER"), format!("FXCM_{tier}_PASSWORD")];
        keys_present(vars, &tier_keys(env.as_str()), label)
            || env.legacy_str().is_some_and(|legacy| keys_present(vars, &tier_keys(legacy), label))
    }

    fn oanda(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
        let tier_keys =
            |tier: &str| vec![format!("OANDA_{tier}_API_KEY"), format!("OANDA_{tier}_ACCOUNT_ID")];
        keys_present(vars, &tier_keys(env.as_str()), label)
            || env.legacy_str().is_some_and(|legacy| keys_present(vars, &tier_keys(legacy), label))
    }

    fn ig(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
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

    fn dukascopy(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
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

    fn aster(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
        let tier = match env {
            Environment::Live => "LIVE",
            Environment::Demo => "TESTNET",
            Environment::Sim => return false,
        };
        keys_present(
            vars,
            &[format!("ASTER_{tier}_USER"), format!("ASTER_{tier}_PRIVATE_KEY")],
            label,
        )
    }

    fn hyperliquid(env: Environment, label: &AccountLabel, vars: &HashMap<String, String>) -> bool {
        let tier = match env {
            Environment::Live => "LIVE",
            Environment::Demo => "DEMO",
            Environment::Sim => return false,
        };
        keys_present(vars, &[format!("HYPERLIQUID_{tier}_PRIVATE_KEY")], label)
    }

    /// The pre-repair `match`, arm for arm — alpaca/ctrader/ibkr/polymarket still falling through
    /// to the generic `{VENUE}_{TIER}_API_KEY`/`_API_SECRET` grid, which IS the defect.
    pub fn venue_env_configured(
        venue: &str,
        env: Environment,
        label: &AccountLabel,
        vars: &HashMap<String, String>,
    ) -> bool {
        match venue {
            "fxcm" => fxcm(env, label, vars),
            "oanda" => oanda(env, label, vars),
            "ig" => ig(env, label, vars),
            "dukascopy" => dukascopy(env, label, vars),
            "aster" => aster(env, label, vars),
            "hyperliquid" => hyperliquid(env, label, vars),
            _ => load_credentials_for_account(venue, env, label, vars).is_some(),
        }
    }
}

/// The pre-repair grid, in `credential_status`' own shape and order, so the two are directly
/// comparable.
fn baseline_grid(vars: &HashMap<String, String>) -> Vec<VenueCredStatus> {
    let label = AccountLabel::Default;
    VENUES
        .iter()
        .map(|&venue| VenueCredStatus {
            venue: venue.to_string(),
            sim: baseline::venue_env_configured(venue, Environment::Sim, &label, vars),
            demo: baseline::venue_env_configured(venue, Environment::Demo, &label, vars),
            live: baseline::venue_env_configured(venue, Environment::Live, &label, vars),
        })
        .collect()
}

fn vars_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

fn row(vars: &HashMap<String, String>, venue: &str) -> VenueCredStatus {
    credential_status(vars)
        .into_iter()
        .find(|s| s.venue == venue)
        .unwrap_or_else(|| panic!("{venue} is not in the roster"))
}

/// One venue's three columns, in `Sim`/`Demo`/`Live` order — the shape every expectation below is
/// written in, so a wrong column cannot be read as the right one.
fn tiers(s: &VenueCredStatus) -> (bool, bool, bool) {
    (s.sim, s.demo, s.live)
}

/// **Every key in `required` really is required**: the full set configures `venue`'s demo tier, and
/// dropping ANY ONE of them un-configures it.
///
/// One key at a time, not one hand-picked omission — that distinction was measured rather than
/// assumed. The first version of the alpaca/ctrader tests dropped a single chosen key each, and a
/// mutation deleting `CTRADER_CLIENT_SECRET` from the required list survived both of them; only the
/// blank-value test happened to catch it, and for a reason that had nothing to do with the app pair
/// being required.
fn assert_each_key_is_required(venue: &str, required: &[(&str, &str)]) {
    let full = vars_of(required);
    assert!(row(&full, venue).demo, "{venue}: the full key set must configure the demo tier");
    for (dropped, _) in required {
        let mut partial = full.clone();
        partial.remove(*dropped);
        assert!(
            !row(&partial, venue).demo,
            "{venue} still reads as configured with {dropped} missing — that key is not actually \
             required by this grid, and the bridge's own loader gates on it"
        );
    }
}

/// The generic `{VENUE}_{TIER}_API_*` grid for EVERY roster venue at EVERY tier — the shape the
/// four repaired venues used to be judged by, and still the correct one for
/// binance/bybit/okx/deribit.
fn generic_grid_for_the_whole_roster() -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for venue in VENUES {
        for tier in ["SIM", "DEMO", "LIVE"] {
            let prefix = format!("{}_{tier}", venue.to_uppercase());
            for suffix in ["_API_KEY", "_API_SECRET", "_API_PASSPHRASE"] {
                vars.insert(format!("{prefix}{suffix}"), "v".to_string());
            }
        }
    }
    vars
}

/// Every BESPOKE shape the file knows, default account, laid over whatever it is given.
fn add_every_bespoke_shape(vars: &mut HashMap<String, String>) {
    for (k, v) in [
        // the six arms this branch did not touch
        ("FXCM_DEMO_USER", "u"),
        ("FXCM_DEMO_PASSWORD", "p"),
        ("FXCM_MAINNET_USER", "u"),
        ("FXCM_MAINNET_PASSWORD", "p"),
        ("OANDA_DEMO_API_KEY", "k"),
        ("OANDA_DEMO_ACCOUNT_ID", "101-004-1-001"),
        ("IG_DEMO_API_KEY", "k"),
        ("IG_DEMO_IDENTIFIER", "i"),
        ("IG_DEMO_PASSWORD", "p"),
        ("DUKASCOPY_DEMO1_LOGIN", "l"),
        ("DUKASCOPY_DEMO1_PASSWORD", "p"),
        ("ASTER_TESTNET_USER", "0xu"),
        ("ASTER_TESTNET_PRIVATE_KEY", "0xk"),
        ("ASTER_LIVE_USER", "0xu"),
        ("ASTER_LIVE_PRIVATE_KEY", "0xk"),
        ("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xk"),
        ("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xk"),
        // the four this branch repaired
        ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
        ("ALPACA_SANDBOX_CLIENT_SECRET", "csec"),
        ("ALPACA_SANDBOX_ACCOUNT_ID", "acct"),
        ("ALPACA_LIVE_CLIENT_ID", "cid"),
        ("ALPACA_LIVE_CLIENT_SECRET", "csec"),
        ("ALPACA_LIVE_ACCOUNT_ID", "acct"),
        ("CTRADER_CLIENT_ID", "app"),
        ("CTRADER_CLIENT_SECRET", "sec"),
        ("CTRADER_SIM_ACCESS_TOKEN", "at"),
        ("CTRADER_SIM_REFRESH_TOKEN", "rt"),
        ("CTRADER_DEMO_ACCESS_TOKEN", "at"),
        ("CTRADER_DEMO_REFRESH_TOKEN", "rt"),
        ("CTRADER_LIVE_ACCESS_TOKEN", "at"),
        ("CTRADER_LIVE_REFRESH_TOKEN", "rt"),
        ("IBKR_SIM_ACCOUNT", "DU1"),
        ("IBKR_DEMO_ACCOUNT", "DUQ186573"),
        ("IBKR_LIVE_ACCOUNT", "U13112916"),
        ("POLY_PRIVATE_KEY", "0xdeadbeef"),
    ] {
        vars.insert(k.to_string(), v.to_string());
    }
}

/// **THE byte-identity gate.** Every venue outside [`REPAIRED`] must answer exactly what the
/// pre-repair enumerator answered, over a fixture that configures the WHOLE roster in the generic
/// shape AND every bespoke shape at once — so each untouched arm is exercised with both the keys it
/// reads and the keys it must keep ignoring present at the same time.
///
/// An EQUALITY between two computations, deliberately: a table of expected bools would be a pin of
/// whatever this code does today and would survive a rewrite that changed every one of them
/// together.
#[test]
fn untouched_venues_are_unchanged() {
    let mut vars = generic_grid_for_the_whole_roster();
    add_every_bespoke_shape(&mut vars);

    let now = credential_status(&vars);
    let before = baseline_grid(&vars);
    assert_eq!(now.len(), VENUES.len(), "the grid must cover the roster");

    let untouched = |g: &[VenueCredStatus]| -> Vec<VenueCredStatus> {
        g.iter().filter(|s| !REPAIRED.contains(&s.venue.as_str())).cloned().collect()
    };
    assert_eq!(
        untouched(&now),
        untouched(&before),
        "a venue this branch did not touch changed its answer"
    );

    // ...and the fixture must actually exercise those arms, or the equality above is two grids of
    // `false`. Every untouched venue lights at least one tier under it.
    for s in untouched(&now) {
        assert!(
            s.sim || s.demo || s.live,
            "{} lights no tier under the full fixture — the equality above proves nothing for it",
            s.venue
        );
    }
}

/// The same equality over the fixture that ISOLATES the defect: only the generic grid, no bespoke
/// key anywhere. The untouched venues must still agree — which is what says the repair did not
/// reach them — and the repaired four are where it must not.
#[test]
fn untouched_venues_are_unchanged_under_the_generic_grid_alone() {
    let vars = generic_grid_for_the_whole_roster();
    let now = credential_status(&vars);
    let before = baseline_grid(&vars);
    for (a, b) in now.iter().zip(before.iter()) {
        if REPAIRED.contains(&a.venue.as_str()) {
            continue;
        }
        assert_eq!(a, b, "{} changed under the generic-grid-only fixture", a.venue);
    }
}

/// **The defect itself.** A store holding nothing but `{VENUE}_{TIER}_API_KEY`/`_API_SECRET`/
/// `_API_PASSPHRASE` for these four venues configures NONE of them — those names are read by no
/// loader under `crates/bridges/{alpaca,ctrader,vike-ibkr,polymarket}/`, so a dot beside one would
/// promise a mount `vike_mount::make_engine` will leave on paper.
///
/// Doubles as the anti-vacuity half of the two equality tests above: it shows the pre-repair
/// enumerator answering `true` for all twelve cells the repaired one answers `false` for, so
/// `baseline` is demonstrably a DIFFERENT function and not an accidental alias of the new one.
#[test]
fn the_generic_api_key_grid_configures_none_of_the_four() {
    let vars = generic_grid_for_the_whole_roster();
    let now = credential_status(&vars);
    let before = baseline_grid(&vars);
    for venue in REPAIRED {
        let a = now.iter().find(|s| s.venue == *venue).expect("roster venue");
        let b = before.iter().find(|s| s.venue == *venue).expect("roster venue");
        assert_eq!(
            tiers(a),
            (false, false, false),
            "{venue} must read nothing from the generic API_KEY/API_SECRET grid"
        );
        assert_eq!(
            tiers(b),
            (true, true, true),
            "the pre-repair enumerator must have lit {venue} from that grid — otherwise this test \
             is not measuring the defect it names"
        );
    }
}

/// Alpaca: the OAuth2 client-credentials trio plus the pinned account, under the bridge's own tier
/// token (`ALPACA_SANDBOX_*` for demo, `ALPACA_LIVE_*` for live) — `vike_alpaca::config`'s
/// `alpaca_tier` / `load_alpaca_config_from`. `Sim` is `false` because that same `alpaca_tier` maps
/// it onto the SANDBOX token demo already occupies: a second spelling, not a second account.
#[test]
fn alpaca_reads_the_oauth_trio_under_its_own_tier_token() {
    let vars = vars_of(&[
        ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
        ("ALPACA_SANDBOX_CLIENT_SECRET", "csec"),
        ("ALPACA_SANDBOX_ACCOUNT_ID", "acct"),
    ]);
    assert_eq!(tiers(&row(&vars, "alpaca")), (false, true, false));

    let live = vars_of(&[
        ("ALPACA_LIVE_CLIENT_ID", "cid"),
        ("ALPACA_LIVE_CLIENT_SECRET", "csec"),
        ("ALPACA_LIVE_ACCOUNT_ID", "acct"),
    ]);
    assert_eq!(tiers(&row(&live, "alpaca")), (false, false, true));

    // all three are REQUIRED, one at a time: the loader gates on each.
    assert_each_key_is_required(
        "alpaca",
        &[
            ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
            ("ALPACA_SANDBOX_CLIENT_SECRET", "csec"),
            ("ALPACA_SANDBOX_ACCOUNT_ID", "acct"),
        ],
    );

    // ...and the trio must not be reachable under the app's own `ALPACA_DEMO_*` tier spelling,
    // which is the token the generic grid would have used.
    let wrong_tier = vars_of(&[
        ("ALPACA_DEMO_CLIENT_ID", "cid"),
        ("ALPACA_DEMO_CLIENT_SECRET", "csec"),
        ("ALPACA_DEMO_ACCOUNT_ID", "acct"),
    ]);
    assert_eq!(tiers(&row(&wrong_tier, "alpaca")), (false, false, false));
}

/// cTrader: the tier-less Spotware APP pair plus this tier's OAuth grant —
/// `vike_ctrader::config::CtraderConfig::from_vars`. `CTRADER_{TIER}_ACCOUNT_ID` is optional there
/// (discovered at connect when unset) and must not be required here.
#[test]
fn ctrader_reads_the_app_pair_plus_the_per_tier_grant() {
    let app_pair = [("CTRADER_CLIENT_ID", "app"), ("CTRADER_CLIENT_SECRET", "sec")];
    for (tier, expect) in [
        ("SIM", (true, false, false)),
        ("DEMO", (false, true, false)),
        ("LIVE", (false, false, true)),
    ] {
        let mut vars = vars_of(&app_pair);
        vars.insert(format!("CTRADER_{tier}_ACCESS_TOKEN"), "at".to_string());
        vars.insert(format!("CTRADER_{tier}_REFRESH_TOKEN"), "rt".to_string());
        assert_eq!(tiers(&row(&vars, "ctrader")), expect, "tier {tier}");
    }

    // the APP pair alone configures nothing — the loader gates on the grant too.
    assert_eq!(tiers(&row(&vars_of(&app_pair), "ctrader")), (false, false, false));

    // ...and the GRANT alone configures nothing either.
    let grant_only =
        vars_of(&[("CTRADER_DEMO_ACCESS_TOKEN", "at"), ("CTRADER_DEMO_REFRESH_TOKEN", "rt")]);
    assert_eq!(tiers(&row(&grant_only, "ctrader")), (false, false, false));

    // all FOUR are required, one at a time — the app pair as much as the grant.
    assert_each_key_is_required(
        "ctrader",
        &[
            ("CTRADER_CLIENT_ID", "app"),
            ("CTRADER_CLIENT_SECRET", "sec"),
            ("CTRADER_DEMO_ACCESS_TOKEN", "at"),
            ("CTRADER_DEMO_REFRESH_TOKEN", "rt"),
        ],
    );

    // ...and `CTRADER_{TIER}_ACCOUNT_ID` is NOT required: the loader discovers it at connect when
    // unset, so demanding it here would report a working mount as unconfigured.
    let mut without_account_id = vars_of(&app_pair);
    without_account_id.insert("CTRADER_DEMO_ACCESS_TOKEN".to_string(), "at".to_string());
    without_account_id.insert("CTRADER_DEMO_REFRESH_TOKEN".to_string(), "rt".to_string());
    assert!(row(&without_account_id, "ctrader").demo);
}

/// IBKR: the account number and nothing else — `vike_ibkr::config::load_ibkr_config_from`, whose
/// only unconditional gate is `IBKR_{TIER}_ACCOUNT`. Every other name it reads has a default, so
/// demanding one here would report a working mount as unconfigured.
#[test]
fn ibkr_reads_only_the_account_number() {
    for (tier, expect) in [
        ("SIM", (true, false, false)),
        ("DEMO", (false, true, false)),
        ("LIVE", (false, false, true)),
    ] {
        let mut vars = HashMap::new();
        vars.insert(format!("IBKR_{tier}_ACCOUNT"), "DUQ186573".to_string());
        assert_eq!(tiers(&row(&vars, "ibkr")), expect, "tier {tier}");
    }

    // the OPTIONAL names alone configure nothing — no account, no mount.
    let no_account = vars_of(&[
        ("IBKR_DEMO_HOST", "127.0.0.1"),
        ("IBKR_DEMO_PORT", "7497"),
        ("IBKR_DEMO_CLIENT_ID", "7"),
        ("IBKR_DEMO_BACKEND", "socket"),
    ]);
    assert_eq!(tiers(&row(&no_account, "ibkr")), (false, false, false));

    // ...and their ABSENCE must not hide a configured account, which is what a
    // "require host + port + client id too" reading of that loader would do.
    let account_only = vars_of(&[("IBKR_DEMO_ACCOUNT", "DUQ186573")]);
    assert!(row(&account_only, "ibkr").demo);
}

/// Polymarket: `POLY_*`, not `POLYMARKET_*` — the prefix mismatch that put this venue in the
/// generic fallback. The gate is the L1 private key alone
/// (`vike_polymarket::config::load_polymarket_creds_from`), reachable on all three of that loader's
/// rungs, and LIVE-only because this venue has no testnet.
#[test]
fn polymarket_reads_the_poly_prefixed_private_key_live_only() {
    for key in ["POLY_LIVE_PRIVATE_KEY", "POLY_MAINNET_PRIVATE_KEY", "POLY_PRIVATE_KEY"] {
        let vars = vars_of(&[(key, "0xdeadbeef")]);
        assert_eq!(tiers(&row(&vars, "polymarket")), (false, false, true), "rung {key}");
    }

    // the L2 trio is DERIVED at connect and is not required — but it is not a substitute for the
    // L1 key either.
    let l2_only = vars_of(&[
        ("POLY_API_KEY", "k"),
        ("POLY_SECRET", "s"),
        ("POLY_PASSPHRASE", "p"),
        ("POLY_ADDRESS", "0xabc"),
    ]);
    assert_eq!(tiers(&row(&l2_only, "polymarket")), (false, false, false));

    // ...and the venue-SLUG prefix names nothing at all.
    let slug_prefixed =
        vars_of(&[("POLYMARKET_LIVE_PRIVATE_KEY", "0xk"), ("POLYMARKET_PRIVATE_KEY", "0xk")]);
    assert_eq!(tiers(&row(&slug_prefixed, "polymarket")), (false, false, false));
}

/// The blank-is-absent rule reaches the new arms too — it is `account_var`'s, applied by
/// `keys_present`, so an operator who wrote `IBKR_DEMO_ACCOUNT=` gets no dot.
#[test]
fn a_blank_value_is_absent_on_the_repaired_venues() {
    for (venue, keys) in [
        ("ibkr", vec![("IBKR_DEMO_ACCOUNT", "   ")]),
        ("polymarket", vec![("POLY_PRIVATE_KEY", "")]),
        (
            "alpaca",
            vec![
                ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
                ("ALPACA_SANDBOX_CLIENT_SECRET", "csec"),
                ("ALPACA_SANDBOX_ACCOUNT_ID", " "),
            ],
        ),
        (
            "ctrader",
            vec![
                ("CTRADER_CLIENT_ID", "app"),
                ("CTRADER_CLIENT_SECRET", ""),
                ("CTRADER_DEMO_ACCESS_TOKEN", "at"),
                ("CTRADER_DEMO_REFRESH_TOKEN", "rt"),
            ],
        ),
    ] {
        let vars = vars_of(&keys);
        assert_eq!(
            tiers(&row(&vars, venue)),
            (false, false, false),
            "{venue} must read a blank value as absent"
        );
    }
}

/// **Names only, never values.** The whole public surface of this grid is a venue id and three
/// bools, and nothing here may start carrying a credential. Guards the same contract
/// `crates/vike-connections/tests/connections_a11y.rs` guards at the widget tree, one layer down.
#[test]
fn the_grid_carries_no_credential_value() {
    let mut vars = generic_grid_for_the_whole_roster();
    add_every_bespoke_shape(&mut vars);
    // a value nothing else in the fixture uses, so finding it anywhere is unambiguous
    vars.insert("POLY_PRIVATE_KEY".to_string(), "0xC0FFEETHESECRET".to_string());

    let rendered = format!("{:?}", credential_status(&vars));
    assert!(!rendered.contains("C0FFEE"), "a credential VALUE reached the status grid");
    for forbidden in ["cid", "csec", "DUQ186573", "101-004-1-001"] {
        assert!(!rendered.contains(forbidden), "the value {forbidden:?} reached the status grid");
    }
}
