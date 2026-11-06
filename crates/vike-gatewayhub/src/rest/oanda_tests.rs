//! The OANDA practice-token READS, tested where they can be named: `oanda_history_credentials` and
//! `read_oanda_history_token` are `pub(crate)` (they return the plaintext practice token, so no
//! dependent crate may call them), which is why these tests live in the crate and not in
//! `tests/lanes/oanda_credential.rs` (that suite drives the lane through the real server and the client's
//! real `RemoteKlines`, and keeps the startup line, the memo and the scope). Moved from there; the
//! fixtures both sides plant are the testkit's (`crate::testkit::account_store::oanda`).
//!
//! Drift register row R7, the credential half: the read holds the one practice key and nothing else,
//! three states are three answers, the permission finding is logged by the announcing read only, an
//! unreadable store is not an absent key, and no value reaches a log line.
//!
//! Every fixture plants ACCOUNT ROWS and writes their FIELDS (decision 0156 item 6) - no credential
//! NAME is composed or spelled - and every VALUE is an obviously fake, distinctive string, so "no
//! value reached a log line" is an assertion that could actually fail.

use super::*;
use crate::testkit::account_store::oanda::{
    PRACTICE_VALUE, every_value, full_store_rows, neighbours, store_rows,
};
use crate::testkit::account_store::{plant_unreadable_store, write_store};
use crate::testkit::fixtures::under_capture;

/// The handle the boot would hand this root for a settings directory `dir` (the full scope).
fn at(dir: &std::path::Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::Full,
    )
}

/// The handle a boot that resolved NO settings directory hands this root: no location.
fn nowhere() -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(None, vike_secrets::CredentialScope::Full)
}

/// **The lane's credential read RETURNS the practice account's one field and NOTHING else — from the
/// one store.** The settings DATABASE answers through a bound query on one account's one field; this
/// plants the token among the practice account's own account id, its live, paper and labelled
/// siblings and another venue's key, and reads it back. A read that went back to the whole store, or
/// an address that drifted to another account or field, answers a neighbour's value.
#[test]
fn the_oanda_history_read_holds_the_one_practice_key_and_nothing_else() {
    let dir = tempfile::tempdir().expect("a temp dir");
    write_store(dir.path(), &full_store_rows());
    assert!(
        dir.path().join("db").join("vike.db").is_file(),
        "the fixture must really be a database store, or this half proves nothing"
    );
    let from_db = oanda_history_credentials(&at(dir.path()), false).expect("a readable database");
    assert_eq!(
        from_db.as_ref().map(|token| token.expose().as_str()),
        Some(PRACTICE_VALUE),
        "the DATABASE arm: exactly the practice account's token"
    );
    // …and with the practice account holding NO token, no neighbour answers in its place.
    write_store(dir.path(), &neighbours());
    let without = oanda_history_credentials(&at(dir.path()), false).expect("a readable database");
    assert!(without.is_none(), "a neighbour answered for the practice token");
}

/// **Three states, three answers, and no value in any line logged on the way.** No settings
/// directory, no store and a store without the key are ABSENT; a store that exists and will not read
/// is UNREADABLE (bytes that are not a database where the database belongs) and says so in an
/// `error!`; a present key is the value. The permission finding is logged by the announcing (startup)
/// read and by no other.
#[test]
fn the_oanda_history_token_answers_three_states_and_no_line_carries_a_value() {
    let absent = tempfile::tempdir().expect("a temp dir");
    let without = tempfile::tempdir().expect("a temp dir");
    write_store(without.path(), &neighbours());
    let with = tempfile::tempdir().expect("a temp dir");
    write_store(with.path(), &full_store_rows());
    let unreadable = tempfile::tempdir().expect("a temp dir");
    plant_unreadable_store(unreadable.path());

    let (answers, lines) = under_capture(|| {
        [
            read_oanda_history_token(&nowhere(), true),
            read_oanda_history_token(&at(absent.path()), true),
            read_oanda_history_token(&at(without.path()), true),
            read_oanda_history_token(&at(with.path()), false),
            read_oanda_history_token(&at(unreadable.path()), false),
        ]
    });
    assert_eq!(
        answers,
        [
            Err(HistoryTokenError::NotConfigured),
            Err(HistoryTokenError::NotConfigured),
            Err(HistoryTokenError::NotConfigured),
            Ok(PRACTICE_VALUE.to_string()),
            Err(HistoryTokenError::StoreUnreadable),
        ]
    );
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {lines:?}");
    assert!(errors[0].contains("NOT the same as the key being absent"), "{}", errors[0]);
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(&value)), "a value reached the log: {lines:?}");
    }
}

/// The finding a group- or world-readable store earns is logged by the ANNOUNCING read and by no
/// other — the connection memo's own read runs once per connection and must stay silent.
#[cfg(unix)]
#[test]
fn only_the_announcing_read_logs_the_stores_permission_finding() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("a temp dir");
    write_store(dir.path(), &full_store_rows());
    std::fs::set_permissions(
        vike_secrets::db_path_in(dir.path()),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("chmod");

    let (_, silent) = under_capture(|| read_oanda_history_token(&at(dir.path()), false));
    assert!(silent.is_empty(), "a per-connection read logged: {silent:?}");
    let (_, announced) = under_capture(|| read_oanda_history_token(&at(dir.path()), true));
    assert!(
        announced.iter().any(|l| l.starts_with("WARN")),
        "the startup read must report a store others can read: {announced:?}"
    );
    for value in every_value() {
        assert!(!announced.iter().any(|l| l.contains(&value)), "{announced:?}");
    }
}

/// **The presence word — every state its own answer, and no value in any line logged on the way**
/// (the design's §2.4; 0097's verdict 6). No settings directory, no store, a store without the key
/// and a store holding it BLANK are `Absent` — the answers the lane's own token read gives — a stored
/// key is `Present`, and a store that exists and will not read is `Unreadable`, logged once and never
/// folded into `Absent`.
#[test]
fn the_presence_probe_answers_every_state_and_no_line_carries_a_value() {
    let absent = tempfile::tempdir().expect("a temp dir");
    let without = tempfile::tempdir().expect("a temp dir");
    write_store(without.path(), &neighbours());
    let blank = tempfile::tempdir().expect("a temp dir");
    write_store(blank.path(), &store_rows(Some("   ")));
    let with = tempfile::tempdir().expect("a temp dir");
    write_store(with.path(), &full_store_rows());
    let unreadable = tempfile::tempdir().expect("a temp dir");
    plant_unreadable_store(unreadable.path());

    let (answers, lines) = under_capture(|| {
        [
            oanda_history_presence(&nowhere()),
            oanda_history_presence(&at(absent.path())),
            oanda_history_presence(&at(without.path())),
            oanda_history_presence(&at(blank.path())),
            oanda_history_presence(&at(with.path())),
            oanda_history_presence(&at(unreadable.path())),
        ]
    });
    assert_eq!(
        answers,
        [
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Present,
            CredentialPresence::Unreadable,
        ]
    );
    // The word agrees with the lane's own read on every one of those stores.
    for dir in [absent.path(), without.path(), blank.path(), with.path()] {
        let read = read_oanda_history_token(&at(dir), false);
        assert_eq!(
            read.is_ok(),
            oanda_history_presence(&at(dir)) == CredentialPresence::Present,
            "the presence word and the lane's read disagree about {}",
            dir.display()
        );
    }
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {lines:?}");
    assert!(errors[0].contains("NOT the same as the key being absent"), "{}", errors[0]);
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(&value)), "a value reached the log: {lines:?}");
    }
}
