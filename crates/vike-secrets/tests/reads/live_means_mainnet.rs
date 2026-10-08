//! Decision 0095's store migration: every `live` ceiling row of binance/bybit/okx/hyperliquid
//! becomes `demo`, once, journalled, and nothing else moves.

use std::path::Path;

use vike_model::change_journal::{Actor, Proc};
use vike_secrets::live_means_mainnet::{
    LiveMeansMainnet, SWITCHED_VENUES, apply_live_means_mainnet, live_means_mainnet_pending,
    unmark_live_means_mainnet,
};
use vike_secrets::{ArmingRow, StoredSettings};

fn arming(venue: &str, label: Option<&str>, mode: &str) -> ArmingRow {
    ArmingRow {
        venue: venue.to_string(),
        label: label.map(str::to_string),
        mode: mode.to_string(),
        max_exposure: None,
    }
}

/// A store as a PRE-0095 binary left it: rows written, no migration marker.
fn pre_0095(rows: Vec<ArmingRow>) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    vike_secrets::plant_settings_rows(
        d.path(),
        &StoredSettings { arming: rows, ..Default::default() },
    )
    .unwrap();
    unmark_live_means_mainnet(d.path());
    d
}

fn modes(d: &Path) -> Vec<(String, Option<String>, String)> {
    match vike_secrets::read_settings_in(d).unwrap() {
        vike_secrets::SettingsSource::Rows { rows, .. } => {
            rows.arming.into_iter().map(|r| (r.venue, r.label, r.mode)).collect()
        }
        other => panic!("{other}"),
    }
}

fn apply(d: &Path) -> LiveMeansMainnet {
    apply_live_means_mainnet(d, Actor::cli("test"), Proc::new("test", 1, "0.0.0"), 1).unwrap()
}

/// Review Focus 3.
#[test]
fn only_the_four_switched_venues_live_rows_are_rewritten() {
    let d = pre_0095(vec![
        arming("aster", None, "live"),
        arming("binance", None, "live"),
        arming("binance", Some("ALT"), "live"),
        arming("bybit", None, "demo"),
        arming("hyperliquid", None, "live"),
        arming("okx", None, "paper"),
        arming("polymarket", None, "live"),
    ]);
    assert!(live_means_mainnet_pending(d.path()).unwrap());
    let LiveMeansMainnet::Applied { rewrites, journal_error } = apply(d.path()) else {
        panic!("a pending store is migrated");
    };
    assert!(journal_error.is_none());
    let keys: Vec<String> = rewrites.iter().map(|r| r.key()).collect();
    assert_eq!(
        keys,
        ["policy.accounts.binance.ALT", "policy.venues.binance", "policy.venues.hyperliquid"],
        "exactly the `live` rows of the switched venues, venue lines and account lines alike"
    );
    let after = modes(d.path());
    let mode = |v: &str, l: Option<&str>| {
        after.iter().find(|(av, al, _)| av == v && al.as_deref() == l).unwrap().2.clone()
    };
    assert_eq!(mode("binance", None), "demo");
    assert_eq!(mode("binance", Some("ALT")), "demo");
    assert_eq!(mode("hyperliquid", None), "demo");
    assert_eq!(mode("aster", None), "live", "aster's `live` already meant real money");
    assert_eq!(mode("polymarket", None), "live", "polymarket's `live` already meant real money");
    assert_eq!(mode("bybit", None), "demo");
    assert_eq!(mode("okx", None), "paper", "a non-`live` row is untouched");
}

#[test]
fn the_migration_runs_once_and_is_journalled_with_its_reason() {
    let d = pre_0095(vec![arming("okx", None, "live")]);
    assert!(matches!(apply(d.path()), LiveMeansMainnet::Applied { .. }));
    assert!(!live_means_mainnet_pending(d.path()).unwrap());
    assert!(matches!(apply(d.path()), LiveMeansMainnet::NotPending), "idempotent");

    let mut journal = String::new();
    for e in std::fs::read_dir(d.path().join("state").join("changes")).unwrap().flatten() {
        journal.push_str(&std::fs::read_to_string(e.path()).unwrap());
    }
    assert!(journal.contains("policy.venues.okx"), "{journal}");
    assert!(journal.contains("0095"), "the record carries its reason: {journal}");
}

/// Review Focus 2.
#[test]
fn a_live_line_written_after_the_migration_survives_every_later_write() {
    let d = pre_0095(vec![arming("bybit", None, "live")]);
    apply(d.path());
    // The operator goes live ON PURPOSE, through the one row writer `config set` uses…
    vike_secrets::write_setting_row_in(
        d.path(),
        vike_secrets::RowChange::Arming {
            venue: "bybit".to_string(),
            label: None,
            mode: "live".to_string(),
        },
        std::time::Duration::from_secs(3),
        |_, _, _| Ok(()),
    )
    .unwrap();
    // …and a later, unrelated write runs the write funnel again.
    vike_secrets::set_venue_setting_in(d.path(), "ibkr", Some("demo"), "HOST", "<host>").unwrap();
    assert!(
        modes(d.path()).iter().any(|(v, l, m)| v == "bybit" && l.is_none() && m == "live"),
        "a migration that ran again would have undone the operator's `live`"
    );
}

/// The write funnel applies the migration BEFORE any write lands, so no write can precede it.
#[test]
fn any_write_to_a_pre_0095_store_migrates_it_first() {
    let d = pre_0095(vec![arming("binance", None, "live")]);
    vike_secrets::set_venue_setting_in(d.path(), "ibkr", Some("demo"), "HOST", "<host>").unwrap();
    assert!(!live_means_mainnet_pending(d.path()).unwrap());
    assert!(modes(d.path()).iter().any(|(v, _, m)| v == "binance" && m == "demo"));
}

#[test]
fn a_store_with_nothing_to_rewrite_is_not_pending_and_no_database_is_nothing() {
    let d = pre_0095(vec![arming("binance", None, "demo")]);
    assert!(!live_means_mainnet_pending(d.path()).unwrap(), "no `live` row of a switched venue");
    let none = tempfile::tempdir().unwrap();
    assert!(!live_means_mainnet_pending(none.path()).unwrap());
    assert!(matches!(apply(none.path()), LiveMeansMainnet::NoDatabase));
}

#[test]
fn the_switched_venue_list_is_on_the_roster() {
    for v in SWITCHED_VENUES {
        assert!(vike_model::VENUES.contains(&v), "{v}");
    }
}
