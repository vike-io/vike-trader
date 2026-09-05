//! **The credential-status grid, per ACCOUNT** — `vike_connections::credential_status_for_account`
//! and its default-account face `credential_status`.
//!
//! # Why these tests are an INTEGRATION test rather than a `#[cfg(test)]` module
//!
//! Every fixture here writes a LABELLED credential key name (`BINANCE_DEMO_API_KEY__ALT`), and
//! `crates/vike-ops/tests/settings_registry.rs`'s `every_read_variable_is_declared` harvests
//! env-var-shaped literals out of `src/` and demands a `vike_ops::settings::SETTINGS` row for each.
//! The labelled half of the credential grid deliberately HAS no rows — it is an unbounded,
//! operator-named key set, the declared blind spot `vike_model::account_keys`' module doc describes
//! — so a labelled literal cannot be declared and must not sit under `src/`. That is where every
//! other labelled fixture in this workspace already lives (`vike-mount/tests/account_fanout.rs`,
//! `vike-cli/tests/secrets_cli.rs`), and it is why this file exists at all rather than the block
//! staying beside the venue-tier tests in `status.rs`.
//!
//! # What is gated
//!
//! 1. **A single-account box is byte-identical** — the two faces answer the same grid, key for key,
//!    over a map exercising the generic grid AND every bespoke override.
//! 2. **No borrowing, in either direction** — a labelled account with no keys of its own reads
//!    all-absent rather than the default account's dots, and `__LABEL` keys never light the default
//!    account's row.
//! 3. **Every bespoke shape is reachable by label**, one venue per override arm in `status.rs`.

use std::collections::HashMap;

use vike_connections::{credential_status, credential_status_for_account};
use vike_model::account_keys::AccountLabel;

/// A label the grammar accepts, built through the ONE validator so this fixture cannot pin a
/// spelling `vike_model::account_keys` would refuse.
fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// **THE byte-identity gate for a single-account box.** The default-account face and the
/// venue-wide face must answer identically for EVERY venue at EVERY tier, over a map that
/// exercises the generic grid AND every bespoke shape `status.rs` overrides.
///
/// ⚠ It compares `credential_status` against `credential_status_for_account(.., Default)`
/// rather than against a hand-written expectation, because the property being defended is
/// "the default account's answer did not move", and only the two producers can state it.
#[test]
fn the_default_account_grid_is_unchanged() {
    let mut vars = HashMap::new();
    for (k, v) in [
        ("BINANCE_DEMO_API_KEY", "k"),
        ("BINANCE_DEMO_API_SECRET", "s"),
        ("OKX_LIVE_API_KEY", "k"),
        ("OKX_LIVE_API_SECRET", "s"),
        ("OKX_LIVE_API_PASSPHRASE", "p"),
        ("FXCM_DEMO_USER", "u"),
        ("FXCM_DEMO_PASSWORD", "p"),
        ("OANDA_DEMO_API_KEY", "k"),
        ("OANDA_DEMO_ACCOUNT_ID", "101-004-1-001"),
        ("IG_DEMO_API_KEY", "k"),
        ("IG_DEMO_IDENTIFIER", "i"),
        ("IG_DEMO_PASSWORD", "p"),
        ("DUKASCOPY_DEMO1_LOGIN", "l"),
        ("DUKASCOPY_DEMO1_PASSWORD", "p"),
        ("ASTER_TESTNET_USER", "0xu"),
        ("ASTER_TESTNET_PRIVATE_KEY", "0xk"),
        ("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xk"),
        ("BYBIT_MAINNET_API_KEY", "k"),
        ("BYBIT_MAINNET_API_SECRET", "s"),
        ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
        ("ALPACA_SANDBOX_CLIENT_SECRET", "csec"),
        ("ALPACA_SANDBOX_ACCOUNT_ID", "acct"),
        ("CTRADER_CLIENT_ID", "app"),
        ("CTRADER_CLIENT_SECRET", "sec"),
        ("CTRADER_DEMO_ACCESS_TOKEN", "at"),
        ("CTRADER_DEMO_REFRESH_TOKEN", "rt"),
        ("IBKR_DEMO_ACCOUNT", "DUQ186573"),
        ("POLY_PRIVATE_KEY", "0xk"),
    ] {
        vars.insert(k.to_string(), v.to_string());
    }
    assert_eq!(
        credential_status(&vars),
        credential_status_for_account(&vars, &AccountLabel::Default),
        "the default account's grid must be the venue-wide grid, key for key"
    );
    // …and the fixture must actually LIGHT rows up, or the equality above is two empty grids.
    let lit: usize = credential_status(&vars).iter().filter(|s| s.sim || s.demo || s.live).count();
    assert!(lit >= 8, "the fixture must configure most of the roster, not {lit} venues");
}

/// A store holding ONLY the default account's keys says the LABELLED account has nothing —
/// never the default account's dots, and never a fallback to its keys.
#[test]
fn a_labelled_account_does_not_borrow_the_default_accounts_credentials() {
    let mut vars = HashMap::new();
    vars.insert("BINANCE_DEMO_API_KEY".to_string(), "k".to_string());
    vars.insert("BINANCE_DEMO_API_SECRET".to_string(), "s".to_string());
    let default = credential_status(&vars);
    let binance = default.iter().find(|s| s.venue == "binance").expect("binance");
    assert!(binance.demo, "the default account is configured");

    let labelled = credential_status_for_account(&vars, &alt());
    for s in &labelled {
        assert!(!s.sim && !s.demo && !s.live, "{} must be all-absent for ALT", s.venue);
    }
}

/// The mirror image: `__ALT` keys configure the LABELLED account and leave the default one
/// absent. Covers the generic grid.
#[test]
fn labelled_keys_configure_only_that_account() {
    let mut vars = HashMap::new();
    vars.insert("BINANCE_DEMO_API_KEY__ALT".to_string(), "k".to_string());
    vars.insert("BINANCE_DEMO_API_SECRET__ALT".to_string(), "s".to_string());

    let labelled = credential_status_for_account(&vars, &alt());
    let binance = labelled.iter().find(|s| s.venue == "binance").expect("binance");
    assert!(binance.demo, "the ALT account's demo tier must be detected");
    assert!(!binance.live);

    let default = credential_status(&vars);
    let binance = default.iter().find(|s| s.venue == "binance").expect("binance");
    assert!(!binance.demo, "the DEFAULT account must not be lit by an ALT key set");
}

/// The BESPOKE shapes reach the labelled account too — the half a per-venue override table
/// would have had to be extended for, and does not, because the label is appended after the
/// WHOLE of each key. One venue per override arm in this file.
#[test]
fn every_bespoke_shape_is_reachable_by_label() {
    for (keys, venue, tier) in [
        (vec!["FXCM_DEMO_USER__ALT", "FXCM_DEMO_PASSWORD__ALT"], "fxcm", "demo"),
        (vec!["OANDA_DEMO_API_KEY__ALT", "OANDA_DEMO_ACCOUNT_ID__ALT"], "oanda", "demo"),
        (
            vec!["IG_DEMO_API_KEY__ALT", "IG_DEMO_IDENTIFIER__ALT", "IG_DEMO_PASSWORD__ALT"],
            "ig",
            "demo",
        ),
        (vec!["DUKASCOPY_DEMO1_LOGIN__ALT", "DUKASCOPY_DEMO1_PASSWORD__ALT"], "dukascopy", "demo"),
        (vec!["ASTER_TESTNET_USER__ALT", "ASTER_TESTNET_PRIVATE_KEY__ALT"], "aster", "demo"),
        (vec!["HYPERLIQUID_LIVE_PRIVATE_KEY__ALT"], "hyperliquid", "live"),
        // The four arms added when `status.rs` stopped judging these venues by the generic
        // `{VENUE}_{TIER}_API_KEY` grid none of their bridges reads
        // (`crates/vike-connections/tests/venue_key_shapes.rs` carries the shapes themselves).
        // Each is the account-aware half: the label is appended after the WHOLE of each key, so a
        // two-word suffix (`_CLIENT_SECRET`), a TIER-LESS name (`CTRADER_CLIENT_ID`,
        // `POLY_PRIVATE_KEY`) and a tier token that is not a `credential_keys` tier at all
        // (`SANDBOX`) all reach the labelled account without a table entry.
        (
            vec![
                "ALPACA_SANDBOX_CLIENT_ID__ALT",
                "ALPACA_SANDBOX_CLIENT_SECRET__ALT",
                "ALPACA_SANDBOX_ACCOUNT_ID__ALT",
            ],
            "alpaca",
            "demo",
        ),
        (
            vec![
                "CTRADER_CLIENT_ID__ALT",
                "CTRADER_CLIENT_SECRET__ALT",
                "CTRADER_DEMO_ACCESS_TOKEN__ALT",
                "CTRADER_DEMO_REFRESH_TOKEN__ALT",
            ],
            "ctrader",
            "demo",
        ),
        (vec!["IBKR_SIM_ACCOUNT__ALT"], "ibkr", "sim"),
        (vec!["IBKR_DEMO_ACCOUNT__ALT"], "ibkr", "demo"),
        (vec!["POLY_PRIVATE_KEY__ALT"], "polymarket", "live"),
    ] {
        let mut vars = HashMap::new();
        for k in &keys {
            vars.insert((*k).to_string(), "v".to_string());
        }
        let labelled = credential_status_for_account(&vars, &alt());
        let row = labelled.iter().find(|s| s.venue == venue).expect("venue present");
        let on = match tier {
            "sim" => row.sim,
            "demo" => row.demo,
            "live" => row.live,
            _ => unreachable!("the fixture names sim, demo or live"),
        };
        assert!(on, "{venue}'s {tier} tier must be detected for the ALT account from {keys:?}");
        // …and the DEFAULT account stays dark, so the label really is what selected it.
        let default = credential_status(&vars);
        let row = default.iter().find(|s| s.venue == venue).expect("venue present");
        assert!(
            !(row.sim || row.demo || row.live),
            "{venue} must be all-absent for the DEFAULT account given only __ALT keys"
        );
    }
}

/// A blank labelled value is an ABSENT one — the same rule the unlabelled half applies, so an
/// operator who wrote `BINANCE_DEMO_API_SECRET__ALT=` does not get a green dot.
#[test]
fn a_blank_labelled_value_is_absent() {
    let mut vars = HashMap::new();
    vars.insert("BINANCE_DEMO_API_KEY__ALT".to_string(), "k".to_string());
    vars.insert("BINANCE_DEMO_API_SECRET__ALT".to_string(), "   ".to_string());
    let labelled = credential_status_for_account(&vars, &alt());
    let binance = labelled.iter().find(|s| s.venue == "binance").expect("binance");
    assert!(!binance.demo, "a blank secret must not count as configured");
}
