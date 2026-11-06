//! `StartedKeys`: a key changed under a recorded account is named, a restart (a new record) clears it, and
//! nothing but a route key ever leaves.

use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::{Account, Accounts};

use super::*;

fn row(id: i64, label: Option<&str>, tier: AccountTier, active: bool) -> Account {
    Account {
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
    }
}

/// A directory of `rows`, with `key` as the `API_KEY` of every row id in `keyed`.
fn directory(rows: Vec<Account>, keyed: &[(i64, &str)]) -> AccountDirectory {
    let mut book = CredentialBook::new();
    for (id, key) in keyed {
        book.insert_account(*id, "API_KEY", *key);
        book.insert_account(*id, "API_SECRET", "s");
    }
    AccountDirectory::from_rows(Accounts::Known(rows), None).with_credentials(book)
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
    let mut stripped = CredentialBook::new();
    stripped.insert_account(1, "API_KEY", "k2");
    stripped.insert_account(1, "API_SECRET", "s");
    stripped.insert_account(2, "API_KEY", "a1");
    let both =
        AccountDirectory::from_rows(Accounts::Known(rows.clone()), None).with_credentials(stripped);
    assert_eq!(started.changed(&both), vec!["binance".to_string(), "binance#ALT".to_string()]);

    // The default account restarts: the new start records the key it now holds.
    started.record(&rotated, "binance", &AccountLabel::Default, AccountTier::Demo);
    assert!(started.changed(&rotated).is_empty());
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
