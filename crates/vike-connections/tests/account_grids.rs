//! **The Connections panel's account enumeration** — `vike_connections::AccountGrids::from_vars`.
//!
//! # Why this is an integration test and not a `#[cfg(test)]` block in `status.rs`
//!
//! Every fixture here writes a LABELLED credential key name, and
//! `crates/vike-ops/tests/settings_registry.rs`'s `every_read_variable_is_declared` harvests
//! env-var-shaped literals out of `src/` and demands a `vike_ops::settings::SETTINGS` row for each.
//! The labelled key space deliberately has none — it is operator-named and unbounded — so such a
//! literal must not live under `src/`. `crates/vike-connections/tests/account_status.rs`'s own
//! header carries the full argument; this file is its sibling one rung up, about the ENUMERATION
//! rather than the per-account read.
//!
//! # What is gated
//!
//! 1. **A single-account box enumerates NOTHING**, and its default grid is the `Vec`
//!    `credential_status` has always produced — the non-negotiable this whole change is held to.
//! 2. **An account exists when the grid can light a dot for it**, which is why the label set is not
//!    taken from `vike_model::account_keys::accounts_in_store`: that function classifies two real
//!    credential families as non-accounts, and both are families this editor writes.
//! 3. **Nothing borrows**: an account with no keys reads absent, never the default account's dots.

use std::collections::HashMap;

use vike_connections::{AccountGrids, credential_status};
use vike_model::account_keys::{AccountLabel, accounts_in_store};

/// A label built through the ONE validator, so no fixture can pin a spelling the grammar refuses.
fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// **THE unchanged-box gate.** A store with no labelled account enumerates no account, and the grid
/// the panel renders is the identical `Vec` the venue-wide enumerator returns.
///
/// Reddens on `from_vars` inventing an account, on it dropping the `filter` that requires a lit dot
/// (a store with no `__` key would still enumerate none, so the fixture below plants one that is
/// NOT a credential), and on `default_grid` ceasing to be `credential_status`'s own answer.
#[test]
fn a_store_with_no_labelled_account_enumerates_none() {
    let v = vars(&[
        ("BINANCE_LIVE_API_KEY", "k"),
        ("BINANCE_LIVE_API_SECRET", "s"),
        ("OKX_DEMO_API_KEY", "k"),
        ("OKX_DEMO_API_SECRET", "s"),
        ("OKX_DEMO_API_PASSPHRASE", "p"),
        // Not a credential key, and not even a venue — the store legitimately holds such names.
        ("JFOREX_BRIDGE_JAR", "/opt/vike/bin/jforex/bridge.jar"),
    ]);
    let grids = AccountGrids::from_vars(&v);

    assert_eq!(grids.labels().count(), 0, "no labelled account exists in this store");
    assert_eq!(
        grids.default_grid(),
        credential_status(&v).as_slice(),
        "the default account's grid must be the venue-wide grid, key for key"
    );
    assert_eq!(
        grids.grid_for(&AccountLabel::Default),
        Some(credential_status(&v).as_slice()),
        "the default account resolves to that same grid"
    );
    // …and the fixture is not vacuous: it really does light rows up.
    assert!(grids.default_grid().iter().any(|s| s.live || s.demo));
}

/// A labelled account is enumerated once, with ITS OWN grid — and the default account's grid is
/// untouched by it.
#[test]
fn a_labelled_account_is_enumerated_with_its_own_grid() {
    let v = vars(&[
        ("BINANCE_LIVE_API_KEY", "k"),
        ("BINANCE_LIVE_API_SECRET", "s"),
        ("BINANCE_DEMO_API_KEY__ALT", "k"),
        ("BINANCE_DEMO_API_SECRET__ALT", "s"),
    ]);
    let grids = AccountGrids::from_vars(&v);

    let labels: Vec<&AccountLabel> = grids.labels().collect();
    assert_eq!(labels, vec![&label("ALT")], "one labelled account, enumerated once");

    let alt = grids.grid_for(&label("ALT")).expect("the ALT account has a grid");
    let row = alt.iter().find(|s| s.venue == "binance").expect("binance");
    assert!(row.demo, "ALT's DEMO tier is configured");
    assert!(!row.live, "ALT has no LIVE keys — the default account's must not leak in");

    let def = grids.default_grid().iter().find(|s| s.venue == "binance").expect("binance");
    assert!(def.live && !def.demo, "the default account keeps exactly its own two dots");
}

/// **The measured gap that is the whole reason `from_vars` does not call `accounts_in_store`.**
///
/// `vike_model::account_keys::account_ref_from_key` classifies a key by a roster-venue prefix plus
/// a `vike_model::credential_keys::CREDENTIAL_TIERS` tier token, and the families below miss on one
/// of those two halves:
///
/// * **the tier token is not one of the three** — aster's `TESTNET`, alpaca's `SANDBOX`, and
///   dukascopy's `DEMO1`, which bakes an account INDEX into the tier;
/// * **the prefix is not a roster venue** — polymarket's store spelling is `POLY_`, which cannot
///   match the `polymarket` slug.
///
/// Every one is a shape `crates/vike-connections/tests/account_status.rs`'s
/// `every_bespoke_shape_is_reachable_by_label` proves the READ side supports, so an enumeration
/// built on that classifier would offer no chip for an account whose credentials this editor can
/// perfectly well write.
///
/// This test asserts the gap in BOTH directions, so it is a measurement rather than a claim: if
/// `account_ref_from_key` is ever widened to cover these families, the first half goes red and this
/// file's argument is re-examined rather than silently rotting.
///
/// ⚠ The name carries no COUNT on purpose. It was
/// `..._cannot_see_the_two_non_conforming_families` and the number was wrong the moment
/// `status.rs` grew arms for alpaca and polymarket — the same rot this repo has watched every
/// hand-copied count take.
#[test]
fn the_store_enumerator_cannot_see_the_non_conforming_families() {
    let v = vars(&[
        ("ASTER_TESTNET_USER__ALT", "0xu"),
        ("ASTER_TESTNET_PRIVATE_KEY__ALT", "0xk"),
        ("DUKASCOPY_DEMO1_LOGIN__ALT", "l"),
        ("DUKASCOPY_DEMO1_PASSWORD__ALT", "p"),
        ("ALPACA_SANDBOX_CLIENT_ID__ALT", "cid"),
        ("ALPACA_SANDBOX_CLIENT_SECRET__ALT", "csec"),
        ("ALPACA_SANDBOX_ACCOUNT_ID__ALT", "acct"),
        ("POLY_PRIVATE_KEY__ALT", "0xk"),
    ]);

    let store_accounts = accounts_in_store(v.keys().map(String::as_str));
    assert!(
        store_accounts.is_empty(),
        "the key-name classifier sees no account in any of these families: {store_accounts:?}"
    );

    let grids = AccountGrids::from_vars(&v);
    let labels: Vec<&AccountLabel> = grids.labels().collect();
    assert_eq!(labels, vec![&label("ALT")], "…but the panel must still offer the ALT chip");

    let alt = grids.grid_for(&label("ALT")).expect("the ALT account has a grid");
    for venue in ["aster", "dukascopy", "alpaca"] {
        let row = alt.iter().find(|s| s.venue == venue).expect("venue present");
        assert!(row.demo, "{venue}'s labelled DEMO tier must be lit for ALT");
    }
    // polymarket is LIVE-only — this venue has no testnet, so its dot is in the third column.
    let poly = alt.iter().find(|s| s.venue == "polymarket").expect("polymarket present");
    assert!(poly.live, "polymarket's labelled LIVE tier must be lit for ALT");
}

/// The CONVERSE, which keeps the test above honest: ctrader's grant keys DO conform
/// (`CTRADER_DEMO_ACCESS_TOKEN__ALT` is venue + tier + suffix), so that venue is discoverable by
/// the store enumerator even though its app pair (`CTRADER_CLIENT_ID`, tier-less) is not. Without
/// this, "the classifier misses bespoke shapes" would read as "the classifier misses every bespoke
/// shape", which is false and would justify widening a gap that is narrower than it looks.
#[test]
fn a_bespoke_shape_that_still_conforms_is_seen_by_the_store_enumerator() {
    let v = vars(&[
        ("CTRADER_CLIENT_ID__ALT", "app"),
        ("CTRADER_CLIENT_SECRET__ALT", "sec"),
        ("CTRADER_DEMO_ACCESS_TOKEN__ALT", "at"),
        ("CTRADER_DEMO_REFRESH_TOKEN__ALT", "rt"),
    ]);
    let store_accounts = accounts_in_store(v.keys().map(String::as_str));
    assert!(
        store_accounts.iter().any(|a| a.venue == "ctrader" && a.label == label("ALT")),
        "ctrader's per-tier grant keys conform and must be classified: {store_accounts:?}"
    );

    let grids = AccountGrids::from_vars(&v);
    let alt = grids.grid_for(&label("ALT")).expect("the ALT account has a grid");
    let row = alt.iter().find(|s| s.venue == "ctrader").expect("ctrader present");
    assert!(row.demo, "…and the grid lights it from the same keys");
}

/// A `__` name that is not a credential at all lights no dot and is therefore NOT an account — an
/// operator cannot be offered a chip for something no cell in the grid could ever fill in.
#[test]
fn a_labelled_name_that_is_not_a_credential_is_not_an_account() {
    let v = vars(&[("SOME_TOOL_PATH__ALT", "/tmp/x"), ("BINANCE_LIVE_API_KEY", "k")]);
    let grids = AccountGrids::from_vars(&v);
    assert_eq!(grids.labels().count(), 0, "a non-credential `__` name is not an account");
}

/// A malformed label is skipped rather than panicking or half-parsing — the store is the user's
/// file and may contain anything.
#[test]
fn a_malformed_label_is_skipped() {
    let v = vars(&[
        // lowercase — `AccountLabel::parse` refuses it, deliberately un-repaired.
        ("BINANCE_LIVE_API_KEY__alt", "k"),
        ("BINANCE_LIVE_API_SECRET__alt", "s"),
        // the separator with nothing after it.
        ("OKX_DEMO_API_KEY__", "k"),
    ]);
    let grids = AccountGrids::from_vars(&v);
    assert_eq!(grids.labels().count(), 0, "neither malformed name names an account");
}

/// Labels are sorted and deduplicated, so the chip strip does not reorder between frames and a
/// venue's three labelled keys yield one chip.
#[test]
fn labels_are_sorted_and_deduplicated() {
    let v = vars(&[
        ("BINANCE_LIVE_API_KEY__ZULU", "k"),
        ("BINANCE_LIVE_API_SECRET__ZULU", "s"),
        ("OKX_DEMO_API_KEY__ALT", "k"),
        ("OKX_DEMO_API_SECRET__ALT", "s"),
        ("OKX_DEMO_API_PASSPHRASE__ALT", "p"),
        ("BYBIT_DEMO_API_KEY__ALT", "k"),
        ("BYBIT_DEMO_API_SECRET__ALT", "s"),
    ]);
    let grids = AccountGrids::from_vars(&v);
    let labels: Vec<String> = grids.labels().map(ToString::to_string).collect();
    assert_eq!(labels, vec!["ALT".to_string(), "ZULU".to_string()]);
}

/// An account the store knows nothing about resolves to `None`, NOT to the default account's grid —
/// the no-borrowing rule, which is what a just-named account depends on to render honestly.
#[test]
fn an_unknown_label_resolves_to_none_rather_than_the_default_accounts_grid() {
    let v = vars(&[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")]);
    let grids = AccountGrids::from_vars(&v);
    assert!(grids.grid_for(&label("NEW")).is_none());

    let absent = grids.absent_grid();
    let venues: Vec<&str> = absent.iter().map(|s| s.venue.as_str()).collect();
    let expected: Vec<&str> = grids.default_grid().iter().map(|s| s.venue.as_str()).collect();
    assert_eq!(venues, expected, "the absent grid covers the same venues in the same order");
    assert!(
        absent.iter().all(|s| !s.sim && !s.demo && !s.live),
        "every dot of a not-yet-filled-in account is `not set`"
    );
}
