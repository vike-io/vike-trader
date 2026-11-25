//! `StartedKeys`: a key changed under a recorded account is named, a restart (a new record) clears it, and
//! nothing but a route key ever leaves.

use vike_gatewayhub_client::proto::ArmingRowWire;
use vike_secrets::account_credentials::CredentialRegistry;
use vike_secrets::{AccountRow, AccountRows};

use super::*;

fn row(id: i64, label: Option<&str>, tier: AccountTier, active: bool) -> AccountRow {
    AccountRow {
        id,
        venue: "binance".to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: vike_secrets::fixture_account_symbol("binance").map(str::to_string),
        facts: Default::default(),
    }
}

/// A directory of `rows`, with `key` as the `API_KEY` of every row id in `keyed`.
fn directory(rows: Vec<AccountRow>, keyed: &[(i64, &str)]) -> AccountDirectory {
    let mut book = CredentialRegistry::new();
    for (id, key) in keyed {
        book.insert_account(*id, "API_KEY", *key);
        book.insert_account(*id, "API_SECRET", "s");
    }
    AccountDirectory::from_rows(AccountRows::Known(rows), None).with_credentials(book)
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a label")
}

/// **A changed row raises the warning, a restart clears it.** The recorded account is quiet while the store
/// matches, named (by route key, nothing else) once its key is rotated, still named while the store keeps
/// the new key and the session the old one, and quiet again once a start records the new key.
#[test]
fn a_changed_key_is_named_until_a_restart_records_the_new_one() {
    let rows =
        vec![row(1, None, AccountTier::Demo, true), row(2, Some("ALT"), AccountTier::Demo, true)];
    let started = StartedKeys::default();
    let at_start = directory(rows.clone(), &[(1, "k1"), (2, "a1")]);
    started.record(&at_start, "binance", &AccountLabel::Default, AccountTier::Demo);
    started.record(&at_start, "binance", &alt(), AccountTier::Demo);
    assert!(
        started.changed(&at_start).is_empty(),
        "the store matches what the sessions started with"
    );

    // The default account's key is rotated in the database; ALT's is untouched.
    let rotated = directory(rows.clone(), &[(1, "k2"), (2, "a1")]);
    assert_eq!(started.changed(&rotated), vec!["binance".to_string()]);
    assert_eq!(started.changed(&rotated), vec!["binance".to_string()], "asking changes nothing");

    // ALT's secret is removed: a key set that lost a field is a changed key set too.
    let mut stripped = CredentialRegistry::new();
    stripped.insert_account(1, "API_KEY", "k2");
    stripped.insert_account(1, "API_SECRET", "s");
    stripped.insert_account(2, "API_KEY", "a1");
    let both = AccountDirectory::from_rows(AccountRows::Known(rows.clone()), None)
        .with_credentials(stripped);
    assert_eq!(started.changed(&both), vec!["binance".to_string(), "binance#ALT".to_string()]);

    // The default account restarts: the new start records the key it now holds.
    started.record(&rotated, "binance", &AccountLabel::Default, AccountTier::Demo);
    assert!(started.changed(&rotated).is_empty());
}

/// **What a start reads is counted, per account, and what crosses is a counter, not a digest.** Asking again moves
/// nothing, a key replaced in place moves that account alone, the same key written back moves it again instead of
/// bringing the old number back (so the number says nothing about the value), a venue setting moves that venue's
/// accounts and a deployment row or a flag a start reads every account (`flags.preflight_skip`, which no start
/// reads, none); a restart of the daemon starts elsewhere.
#[test]
fn what_a_start_reads_is_counted_per_account_and_the_count_is_no_digest_of_a_value() {
    let table_row = |label: Option<&str>| ArmingRowWire {
        venue: "binance".to_string(),
        label: label.map(str::to_string),
        tier: Some(AccountTier::Demo),
        effective: Some(AccountTier::Demo),
        block_word: "none".to_string(),
        account_ids: Vec::new(),
        book: None,
    };
    let (table, rows) = (
        vec![
            table_row(None),
            table_row(Some("ALT")),
            ArmingRowWire { tier: None, ..table_row(Some("OFF")) },
        ],
        vec![row(1, None, AccountTier::Demo, true), row(2, Some("ALT"), AccountTier::Demo, true)],
    );
    let snapshot = |keyed: &[(i64, &str)]| TradeLaneSnapshot {
        accounts: directory(rows.clone(), keyed),
        ..TradeLaneSnapshot::default()
    };
    let no_vars = HashMap::new();
    let started = StartedKeys::default();
    let count = |at: &TradeLaneSnapshot, vars: &HashMap<String, String>| {
        let counted = started.start_inputs(&table, at, vars);
        assert!(!counted.contains_key("binance#OFF"), "a row that states no tier is not counted");
        (counted["binance"], counted["binance#ALT"])
    };

    let first = snapshot(&[(1, "k1"), (2, "a1")]);
    let (default, alt_first) = count(&first, &no_vars);
    for _ in 0..50 {
        assert_eq!(count(&first, &no_vars), (default, alt_first), "asking moves nothing");
    }

    let rotated = snapshot(&[(1, "k1"), (2, "a2")]);
    assert_eq!(count(&rotated, &no_vars), (default, alt_first.wrapping_add(1)));
    assert_eq!(
        count(&first, &no_vars),
        (default, alt_first.wrapping_add(2)),
        "the old key written back is a third state, not the old number"
    );

    let mut with_setting = snapshot(&[(1, "k1"), (2, "a1")]);
    let setting = vike_secrets::VenueSettingRow {
        venue: "binance".to_string(),
        tier: None,
        field: "HOST".to_string(),
        value: "a".to_string(),
    };
    with_setting.venue_settings.insert(
        "binance".to_string(),
        vike_secrets::venue_setting::VenueSettings::from_rows("binance", &[setting]),
    );
    assert_eq!(
        count(&with_setting, &no_vars),
        (default.wrapping_add(1), alt_first.wrapping_add(3)),
        "a venue setting is read by every account of the venue"
    );

    let vars = HashMap::from([("OKX_BROKER_CODE".to_string(), "c".to_string())]);
    assert_eq!(
        count(&with_setting, &vars),
        (default.wrapping_add(2), alt_first.wrapping_add(4)),
        "a deployment row is read by every start"
    );

    let mut flagged = with_setting.clone();
    flagged.flags.poly_exec = true;
    assert_eq!(
        count(&flagged, &vars),
        (default.wrapping_add(3), alt_first.wrapping_add(5)),
        "a flag a start reads is read by every start"
    );
    // `flags.preflight_skip` is the tradehub's preflight's, and no bridge start reads it.
    flagged.flags.preflight_skip = true;
    assert_eq!(
        count(&flagged, &vars),
        (default.wrapping_add(3), alt_first.wrapping_add(5)),
        "a flag no start reads moves nothing"
    );

    let restarted = StartedKeys::default().start_inputs(&table, &first, &no_vars);
    assert_ne!(restarted["binance"], default, "a restart reads as moved: nobody counted meanwhile");
}

/// A row that no longer states the recorded tier is the tradehub's stop or restart, not a key warning, and
/// an account nothing was recorded for is never named.
#[test]
fn only_an_account_that_still_runs_at_its_recorded_tier_is_named() {
    let started = StartedKeys::default();
    let at_start = directory(vec![row(1, None, AccountTier::Demo, true)], &[(1, "k1")]);
    started.record(&at_start, "binance", &AccountLabel::Default, AccountTier::Demo);

    let deactivated = directory(vec![row(1, None, AccountTier::Demo, false)], &[(1, "k2")]);
    assert!(started.changed(&deactivated).is_empty(), "inactive: the watcher stops it");
    let retiered = directory(vec![row(1, None, AccountTier::Live, true)], &[(1, "k2")]);
    assert!(started.changed(&retiered).is_empty(), "another tier: the watcher restarts it");
    let other_venue = directory(vec![], &[]);
    assert!(started.changed(&other_venue).is_empty(), "no row: not a key change");
}
