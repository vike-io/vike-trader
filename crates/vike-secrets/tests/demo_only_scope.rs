//! **The DEMO-ONLY scope withholds the same names on both arms, and the two spellings of the rule
//! agree.**
//!
//! `vike_secrets::withheld_by_demo_scope` is the Rust spelling of which names a demo-only read may
//! never return; `crates/vike-secrets/src/db.rs`'s `DEMO_SCOPE_WITHHELD_SQL` is the SQL one, applied
//! inside the database's own `WHERE` so a withheld row's value is never selected. A rule spelled
//! twice drifts, so this plants one name set — chosen to sit on every edge of the rule — and holds:
//!
//! 1. the Rust predicate against the hand-written expectation for every planted name;
//! 2. the DATABASE arm of `resolve_store_demo_only_in` against the Rust predicate (the SQL twin);
//! 3. the FILE arm against the same;
//! 4. the unscoped read of the same store as the control: every name is there, so the scope is what
//!    removed the withheld ones.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vike_secrets::{Classification, Source, Table};

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

fn value_for(name: &str) -> String {
    format!("value-for-{name}")
}

fn plant_file(settings: &Path) {
    std::fs::create_dir_all(settings).expect("settings dir");
    let text: String =
        KEPT.iter().chain(WITHHELD).map(|n| format!("{n}={}\n", value_for(n))).collect();
    std::fs::write(settings.join("secrets.env"), text).expect("write the credential file");
}

fn names(map: &vike_secrets::SecretMap) -> BTreeSet<String> {
    map.keys().map(str::to_string).collect()
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
    let tmp = tempfile::tempdir().expect("tempdir");
    let settings: PathBuf = tmp.path().join("settings");
    plant_file(&settings);
    let classify = |name: &str| Classification::unrecognised(name);
    vike_secrets::migrate(settings.to_str(), |_| false, &classify).expect("migrate");

    // The CONTROL: the unscoped read of the same database returns every planted name.
    let all = vike_secrets::resolve_store_in(&settings, Table::Credential).expect("unscoped read");
    assert!(
        matches!(all.source, Source::Database(_)),
        "the database must answer: {:?}",
        all.source
    );
    assert_eq!(all.secrets.len(), KEPT.len() + WITHHELD.len(), "the control lost names");

    let (scoped, withheld) =
        vike_secrets::resolve_store_demo_only_in(&settings).expect("demo-only read");
    assert!(matches!(scoped.source, Source::Database(_)), "same store: {:?}", scoped.source);
    assert_eq!(
        names(&scoped.secrets),
        expected_kept(),
        "the DATABASE arm's SQL predicate disagrees with `withheld_by_demo_scope`"
    );
    assert_eq!(withheld, WITHHELD.len(), "the withheld count is wrong");
    let map = scoped.secrets.into_map();
    for n in KEPT {
        assert_eq!(map.get(*n), Some(&value_for(n)), "`{n}` came back with the wrong value");
    }
}

#[test]
fn the_file_arm_withholds_exactly_what_the_rust_predicate_names() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let settings: PathBuf = tmp.path().join("settings");
    plant_file(&settings);

    let (scoped, withheld) =
        vike_secrets::resolve_store_demo_only_in(&settings).expect("demo-only read");
    assert!(matches!(scoped.source, Source::File(_)), "the file must answer: {:?}", scoped.source);
    assert_eq!(names(&scoped.secrets), expected_kept(), "the FILE arm disagrees with the rule");
    assert_eq!(withheld, WITHHELD.len(), "the withheld count is wrong");
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
