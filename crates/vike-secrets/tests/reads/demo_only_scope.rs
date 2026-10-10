//! **The DEMO-ONLY scope withholds the same names on both arms, and the two spellings of the rule
//! agree.**
//!
//! `vike_secrets::withheld_by_demo_scope` is the Rust spelling of which names a demo-only read may
//! never return; `crates/vike-secrets/src/db/read.rs`'s `DEMO_SCOPE_WITHHELD_SQL` is the SQL one, applied
//! inside the database's own `WHERE` so a withheld row's value is never selected. A rule spelled
//! twice drifts, so this plants one name set — chosen to sit on every edge of the rule — and holds:
//!
//! 1. the Rust predicate against the hand-written expectation for every planted name;
//! 2. the DATABASE arm of `resolve_store_demo_only_in` against the Rust predicate (the SQL twin);
//! 3. the unscoped read of the same store as the control: every name is there, so the scope is what
//!    removed the withheld ones.

use std::assert_matches;
use std::collections::BTreeSet;

use vike_secrets::{Classification, Source, Table};

use crate::support::{self, Fixture};

/// Names the scope must KEEP. Each sits next to a withheld spelling on purpose.
const KEPT: &[&str] = &[
    "BINANCE_DEMO_API_KEY",
    "BYBIT_DEMO_API_SECRET",
    "OKX_DEMO_API_PASSPHRASE",
    "DUKASCOPY_DEMO1_LOGIN",
    "ALPACA_SANDBOX_CLIENT_ID",
    "HYPERLIQUID_DEMO_PRIVATE_KEY",
    // App-level and attribution names carry no tier, and the scope does not touch them.
    "CTRADER_CLIENT_ID",
    "OKX_BROKER_CODE",
    // Near misses: `ASTER_`/`POLY_` are PREFIXES, and `_LIVE` must be a whole segment.
    "MASTER_SWITCH_TOKEN",
    "ASTERISK_NOTE",
    "POLYGON_RPC_URL",
    "BINANCE_DEMO_API_KEY__LIVELY",
];

/// Names the scope must WITHHOLD — every tier spelling and family the rule names.
const WITHHELD: &[&str] = &[
    "BINANCE_LIVE_API_KEY",
    "BINANCE_LIVE_API_KEY__ALT",
    "HYPERLIQUID_MAINNET_PRIVATE_KEY",
    "CTRADER_LIVE_ACCESS_TOKEN",
    "BINANCE_MAINNET",
    "OKX_DEMO_API_KEY__LIVE",
    "ASTER_DEMO_API_KEY",
    "ASTER_LIVE_PRIVATE_KEY",
    "POLY_PRIVATE_KEY",
];

/// A store holding every planted name, each with its fake value — filed as infrastructure, since
/// the scope judges the NAME and nothing here is about which account owns it.
fn planted() -> Fixture {
    Fixture::seeded_with(
        support::fake_rows(KEPT.iter().chain(WITHHELD)),
        support::is_node_key,
        &Classification::unrecognised,
    )
}

fn expected_kept() -> BTreeSet<String> {
    KEPT.iter().map(|n| (*n).to_string()).collect()
}

#[test]
fn the_rust_predicate_matches_the_planted_expectation() {
    for n in KEPT {
        assert!(!vike_secrets::withheld_by_demo_scope(n), "`{n}` must be KEPT by the demo scope");
    }
    for n in WITHHELD {
        assert!(
            vike_secrets::withheld_by_demo_scope(n),
            "`{n}` must be WITHHELD by the demo scope"
        );
        assert!(
            vike_secrets::withheld_by_demo_scope(&n.to_ascii_lowercase()),
            "`{n}` in lower case must be withheld too: case can only widen the rule"
        );
    }
}

#[test]
fn the_database_arm_withholds_inside_the_query_exactly_what_the_rust_predicate_names() {
    let fx = planted();

    // The CONTROL: the unscoped read of the same database returns every planted name.
    let all = vike_secrets::resolve_store_in(fx.dir(), Table::Credential).expect("unscoped read");
    assert_matches!(all.source, Source::Database(_), "the database must answer: {:?}", all.source);
    assert_eq!(all.secrets.len(), KEPT.len() + WITHHELD.len(), "the control lost names");

    let (scoped, withheld) =
        vike_secrets::resolve_store_demo_only_in(fx.dir()).expect("demo-only read");
    assert_matches!(scoped.source, Source::Database(_), "same store: {:?}", scoped.source);
    assert_eq!(
        support::key_names(&scoped.secrets),
        expected_kept(),
        "the DATABASE arm's SQL predicate disagrees with `withheld_by_demo_scope`"
    );
    assert_eq!(withheld, WITHHELD.len(), "the withheld count is wrong");
    let map = scoped.secrets.into_map();
    for n in KEPT {
        assert_eq!(
            map.get(*n),
            Some(&support::fake_value(n)),
            "`{n}` came back with the wrong value"
        );
    }
}

#[test]
fn an_absent_store_is_the_live_gate_under_the_scope_too() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (scoped, withheld) =
        vike_secrets::resolve_store_demo_only_in(&tmp.path().join("settings")).expect("absent");
    assert_eq!(scoped.source, Source::None);
    assert!(scoped.secrets.is_empty());
    assert_eq!(withheld, 0);
}
