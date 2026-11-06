//! Tests of the DELETE's database half that only this module can reach: the marker the readers
//! hide is the marker the delete writes, and a delete whose database vanished creates nothing.

use super::*;
use std::assert_matches;

/// **The read predicate hides the marker the delete writes** — `live_predicate` spells the
/// delete's `RETIRED_MARK` as a literal (it is a `&'static str`), so a rename of the constant
/// that left the predicate behind would hand every deleted value straight back to every reader.
/// This is the pin that makes the two one spelling.
#[test]
fn the_read_predicate_hides_the_mark_the_delete_writes() {
    let predicate = live_predicate(Table::Credential);
    assert!(
        predicate.contains(&format!("'{}'", super::retire::RETIRED_MARK)),
        "the reader's predicate no longer names the delete's marker: {predicate}"
    );
}

/// **A delete whose database VANISHED creates nothing** — the race `upsert_rows`' invariant section
/// argues, handed to [`retire_row`] as the STATE that race leaves: a path with nothing at it.
#[test]
fn a_delete_whose_database_vanished_creates_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let refused = retire_row(&db, Table::Credential, "ACME_DEMO_API_KEY").expect_err("no database");
    assert_matches!(refused.kind, DbErrorKind::VanishedDatabase, "{refused}");
    assert!(!db.exists(), "the file the open created was unlinked");
}

/// **A hub key is refused BEFORE anything is opened** when the caller names the hub-key table —
/// so not even the vanished-database path is reached.
#[test]
fn a_hub_key_table_delete_is_refused_before_the_store_is_opened() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let refused =
        retire_row(&db, Table::HubKey, "VIKE_TRADEHUB_OBSERVE_KEY").expect_err("a hub key");
    assert_matches!(refused.kind, DbErrorKind::HubKeyNotRetired { .. }, "{refused}");
    assert!(!db.exists(), "nothing was opened, so nothing was created");
}
