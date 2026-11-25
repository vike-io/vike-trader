//! **`order_client_id_session`: every order client id session this install ever minted under, one row each.**
//!
//! An order client id proves it is THIS install's only by the session that minted it
//! (`vike_model::orders::client_id::order_client_id_minted_by`: the session's random base and its last sequence). The
//! core remembers only its CURRENT session; a past one (the v1 session before the v2 switch, a v2 session past its roll
//! point, the session of a journal moved aside) would be forgotten, and a late fill on an id it minted would read as
//! another install's. This table remembers them: the tradehub reads it at boot into
//! `vike_core::CoreConfig::order_client_id_past_sessions`, and raises a row whenever it learns a session's sequence.
//!
//! # It lives in the settings database, by the owner's ruling, and this is how that stays safe
//!
//! New operational tables go in the one `vike.db` (the owner's ruling of 2026-11-07); the file is also the owner's
//! only copy of his live keys, so what this module may do is narrow, `crate::link_sessions`'s rules:
//!
//! * **It inserts a row and raises `last_sequence`, and nothing else.** It never lowers a sequence, deletes no row and
//!   touches no other table. A store that does not exist is never created ([`raise_order_client_id_session`] is a
//!   no-op on it: a box with no settings database has no install identity to remember).
//! * **It is not settings**: the seal counts the `setting` table alone, and `config check` never sees these rows.
//! * **No version stamp, on purpose**: `CREATE TABLE IF NOT EXISTS` by the writer, the DDL outside [`crate::DDL`], and
//!   [`crate::SCHEMA_VERSION`] stays (`crate::link_sessions`'s module doc is the argument).
//! * **A write waits [`ORDER_CLIENT_ID_SESSION_WRITE_BUSY`], then fails**, and the caller says so: a settings write
//!   never waits long behind it. One row per session, a few in a box's life.
//!
//! # What a row may hold
//!
//! A session string (`[<tag>V]<8 hex>` for v1, the v2 session prefix otherwise: no secret, it is on every order the
//! session minted), when it was first recorded, and a BOUND at or above the highest sequence it reached: the tradehub
//! writes the row before the session mints and keeps it a lease ahead of its minting
//! (`vike_tradehub::execution_stream`'s module doc), so no id the session minted is ever above it.

use std::path::Path;
use std::time::Duration;

use rusqlite::{TransactionBehavior, params};

use crate::db::{StoreError, database_present, open_for_read, open_for_write_within};
use crate::store_locator::db_path_in;

/// How long a write waits for the store's lock before it fails.
pub const ORDER_CLIENT_ID_SESSION_WRITE_BUSY: Duration = Duration::from_millis(250);

const ORDER_CLIENT_ID_SESSION_DDL: &str = "
CREATE TABLE IF NOT EXISTS order_client_id_session (
    session       TEXT PRIMARY KEY,
    first_ms      INTEGER NOT NULL,
    last_sequence INTEGER NOT NULL
) STRICT;
";

/// **Record `session` as one of this install's, minted up to `last_sequence`**: a new row (first recorded at
/// `now_ms`), or the row's `last_sequence` raised to it. A lower `last_sequence` changes nothing. A box with no store
/// writes nothing and is no error.
///
/// # Errors
/// The store is busy past [`ORDER_CLIENT_ID_SESSION_WRITE_BUSY`], at another schema, or will not open. Nothing was
/// written.
pub fn raise_order_client_id_session(
    settings_dir: &Path,
    session: &str,
    last_sequence: u64,
    now_ms: i64,
) -> Result<(), StoreError> {
    let db = db_path_in(settings_dir);
    // No store: nothing to record, and the open creates none.
    let Some(mut conn) = open_for_write_within(&db, ORDER_CLIENT_ID_SESSION_WRITE_BUSY)? else {
        return Ok(());
    };
    let sequence = i64::try_from(last_sequence).unwrap_or(i64::MAX);
    let mut write = || -> rusqlite::Result<()> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(ORDER_CLIENT_ID_SESSION_DDL)?;
        tx.execute(
            "INSERT INTO order_client_id_session (session, first_ms, last_sequence) VALUES (?1, ?2, ?3) \
             ON CONFLICT (session) DO UPDATE SET last_sequence = max(last_sequence, excluded.last_sequence)",
            params![session, now_ms, sequence],
        )?;
        tx.commit()
    };
    write().map_err(|e| StoreError::sql(&db, e))
}

/// **Every session the table holds, `(session, last_sequence)`, oldest first.** Empty for a box with no store and for a
/// store with no such table yet.
///
/// # Errors
/// A store that exists and cannot be read: never "no sessions", which would read every late fill of a past session
/// as foreign.
pub fn read_order_client_id_sessions(
    settings_dir: &Path,
) -> Result<Vec<(String, u64)>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db)? {
        return Ok(Vec::new());
    }
    let conn = open_for_read(&db)?;
    let query = || -> rusqlite::Result<Vec<(String, u64)>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'order_client_id_session')",
            [],
            |r| r.get(0),
        )?;
        if !there {
            return Ok(Vec::new());
        }
        let mut rows = conn.prepare(
            "SELECT session, last_sequence FROM order_client_id_session ORDER BY first_ms, session",
        )?;
        rows.query_map([], |r| {
            let sequence: i64 = r.get(1)?;
            Ok((r.get(0)?, u64::try_from(sequence).unwrap_or(0)))
        })?
        .collect()
    };
    query().map_err(|e| StoreError::sql(&db, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp");
        crate::create_empty_store_for_test(&db_path_in(dir.path())).expect("a store");
        dir
    }

    /// A row is inserted once, its sequence only ever RAISED, and every session is read back oldest first.
    #[test]
    fn a_session_is_inserted_once_and_only_raised() {
        let dir = store();
        assert_eq!(
            read_order_client_id_sessions(dir.path()).expect("read"),
            Vec::new(),
            "no table yet"
        );
        raise_order_client_id_session(dir.path(), "W00000000abcd", 5, 100).expect("insert");
        raise_order_client_id_session(dir.path(), "W00000000abcd", 3, 200)
            .expect("a lower sequence");
        raise_order_client_id_session(dir.path(), "1f2e3d4c", 40, 150).expect("a second session");
        raise_order_client_id_session(dir.path(), "W00000000abcd", 9, 300).expect("raise");
        assert_eq!(
            read_order_client_id_sessions(dir.path()).expect("read"),
            vec![("W00000000abcd".to_string(), 9), ("1f2e3d4c".to_string(), 40)],
            "first recorded at 100 and 150; the lower sequence moved nothing"
        );
    }

    /// A box with no settings database writes nothing, creates nothing and reads no session.
    #[test]
    fn a_box_with_no_store_writes_nothing_and_creates_nothing() {
        let dir = tempfile::tempdir().expect("temp");
        raise_order_client_id_session(dir.path(), "W00000000abcd", 5, 100).expect("a no-op");
        assert!(!db_path_in(dir.path()).exists(), "no store was created");
        assert_eq!(read_order_client_id_sessions(dir.path()).expect("read"), Vec::new());
    }
}
