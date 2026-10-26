//! `StoreHealth` over the ONE read door, `crate::CredentialStore`: an absent store and a store that
//! will not open both leave a caller with an empty map, and the verdict tells them apart.

use super::*;
use crate::{CredentialScope, CredentialStore, Resolved, Source, StoreLocation};

/// A handle opened over NO location is the box with no store: an empty map, `Source::None`, a
/// store that ANSWERED — never an error.
#[test]
fn a_handle_with_no_location_answers_the_live_gate_and_never_errors() {
    for scope in [CredentialScope::Full, CredentialScope::NoLiveKeys, CredentialScope::NoVenueKeys]
    {
        let store = CredentialStore::open(None, scope);
        let read = store.read_all().expect("no location is an answer, not an error");
        assert!(read.secrets.is_empty(), "no store holds nothing");
        assert_eq!(read.source, Source::None);
        assert_eq!(read.health, StoreHealth::Readable, "absent is an ANSWER, not a fault");
        assert_eq!(read.withheld, 0);
        assert_eq!(read.scope, scope);
    }
}

/// ⚠ **THE TWO EMPTY MAPS ARE TELLABLE APART.** An ABSENT store and a store that EXISTS and will
/// not open both end in zero credentials for a caller that degrades; [`StoreHealth`] distinguishes
/// them, asserted over the REAL read. The unreadable store is bytes that are not a database at the
/// database's path — no permission bit a Windows box (or root) would ignore.
///
/// Reddens on the read folding its error into an empty `Ok`, or on `Resolved::unreadable` leaving
/// `StoreHealth::Readable` set — the shape that lets a UI render a measured `0 set` for a store it
/// never opened.
#[test]
fn an_unreadable_store_is_distinguishable_from_an_absent_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    let store = CredentialStore::open(Some(StoreLocation::new(&settings)), CredentialScope::Full);

    // ABSENT: no settings database at all. Empty map, and the store ANSWERED.
    let read = store.read_all().expect("an absent store is an answer");
    assert!(read.secrets.is_empty(), "an absent store holds nothing");
    assert_eq!(read.health, StoreHealth::Readable, "absent is an ANSWER, not a fault");
    assert!(read.health.is_readable());

    // PRESENT AND UNOPENABLE: the read is an ERROR, and the degraded answer carries the verdict.
    let db = crate::db_path_in(&settings);
    std::fs::create_dir_all(db.parent().expect("db dir")).expect("db dir");
    std::fs::write(&db, b"not a sqlite database, and not empty either").expect("plant");
    let e = store.read_all().expect_err("a store that EXISTS and will not open is LOUD");
    let degraded = Resolved::unreadable(store.scope(), &e);
    assert!(degraded.secrets.is_empty(), "the degradation is an empty map, the live gate");
    assert_eq!(degraded.source, Source::None);
    assert!(!degraded.health.is_readable(), "…and THAT is the whole point: {:?}", degraded.health);
    let StoreHealth::Unreadable(why) = &degraded.health else { panic!("{:?}", degraded.health) };
    assert!(why.contains(crate::DB_FILE), "the fault is quotable and names the store: {why}");
    assert_eq!(why, &e.to_string(), "the verdict is the error's own text, verbatim");
}
