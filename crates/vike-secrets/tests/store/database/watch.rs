//! The commit counter the running tradehub follows the account rows with (decision 0169 item 3).

use super::*;

/// **A commit by ANOTHER connection is reported exactly once; a quiet store reports nothing.** The
/// watcher is the only thing between an operator's `account activate` and the running node noticing
/// it, so both halves matter: a missed commit is an account that never starts, and a phantom one
/// re-reads the rows a second for nothing.
#[test]
fn another_connections_commit_is_seen_once_and_a_quiet_store_is_not() {
    let fx = Fixture::live_shaped();
    let mut watch = crate::support::store_from(fx.arg())
        .watch()
        .expect("the store opens")
        .expect("a store exists to watch");
    assert!(!watch.changed().expect("asks"), "nothing committed since the baseline");
    assert!(!watch.changed().expect("asks"), "and asking again moves nothing");

    vike_secrets::save_credentials_to_store_unjournalled(
        fx.dir(),
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated-by-another-process".to_string())],
        vike_secrets::UpsertIntent::Replace,
    )
    .expect("the write succeeds");

    assert!(watch.changed().expect("asks"), "the commit made through another connection is seen");
    assert!(!watch.changed().expect("asks"), "and reported once, not on every poll after it");
}

/// A box with no database has nothing to watch: `None`, never an error, and no file is created.
#[test]
fn a_box_with_no_database_has_nothing_to_watch() {
    let fx = Fixture::empty();
    let watch = crate::support::store_from(fx.arg()).watch().expect("absent is an answer");
    assert!(watch.is_none());
    assert!(!fx.db().exists(), "watching creates nothing");
}
