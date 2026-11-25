//! **`execution_stream` and `execution_stream_gap`: the datahub's cursor of each tradehub's execution stream.**
//!
//! A tradehub streams every execution its OMS journaled to its datahub, numbered by the journal's own sequence
//! (`vike_datahub_client::execution_stream`'s module doc is the grammar). The datahub is the AUTHORITY on how far it
//! stored each journal: one row of `execution_stream` per journal id, with two numbers.
//!
//! * `stored_through`: every record up to it is in the store. It is what the datahub answers a new open with, and
//!   what the tradehub's journal keeps everything after. NULL (`None`) while NOTHING is stored: a journal's first
//!   record is sequence 0, so `0` means "stored through record 0", never "nothing".
//! * `writing_through`: the window being written. Above `stored_through` only while a window is in flight, so a
//!   datahub that died mid-window finds the window it was writing, and rewrites the SAME window under the same commit
//!   keys: the store drops every series that already landed.
//!
//! The row is created at the journal's FIRST open ([`open_execution_stream_row`]), and keeps what that first contact
//! was: `first_contact_through`, the journal's head then, and `generation`, the journal's place in the order the
//! datahub first heard from each (one more than every row before it). A journal moved aside restarts its sequences,
//! and its successor opens after it (the tradehub drains a moved-aside predecessor before its successor), so the
//! generation is what orders one order's rows across the move. Every record up to it may already be in the store under
//! another journal id (a journal re-identified after a copy), so the datahub checks those against the stored rows
//! whatever reconnects happen in the middle of the catch-up; a record past it was journaled after the first contact.
//!
//! `execution_stream_gap` is one row per window of a journal the datahub knows it lacks: `pruned` (the journal pruned
//! records no datahub had stored) or `unverified` (a tradehub boot could not regenerate a crashed tail's reports).
//!
//! `execution_account_positions` is one row per account of every tradehub boot's POSITION SNAPSHOT (its open positions
//! as the core held them, or why it could not know them): the statement an account's window opens from, so a fill
//! recorded before the stream is never needed ([`read_account_positions`] answers the latest at or before a time).
//!
//! # It lives in the settings database, by the owner's ruling, and this is how that stays safe
//!
//! New operational tables go in the one `vike.db` (the owner's ruling of 2026-11-07); the file is also the owner's
//! only copy of his live keys, so what this module may do is narrow, `crate::link_sessions`'s rules:
//!
//! * **It inserts and updates rows of its three tables and nothing else.** It deletes nothing and rewrites, moves and
//!   truncates nothing. A store that does not exist is REFUSED, never created ([`StoreErrorKind::NoSettingsDatabase`]):
//!   a cursor that silently stopped being kept would make every reconnect first contact.
//! * **It is not settings**: the seal counts the `setting` table alone, and `config check` never sees these rows.
//! * **No version stamp, on purpose**: `CREATE TABLE IF NOT EXISTS` by every writer, the DDL outside [`crate::DDL`],
//!   and [`crate::SCHEMA_VERSION`] stays (`crate::link_sessions`'s module doc is the argument).
//! * **A write waits [`EXECUTION_STREAM_WRITE_BUSY`], then fails**, and the datahub retries the same window: a
//!   settings write never waits long behind it, and nothing is lost by the wait.
//! * **A read of a store that exists and cannot be read is an ERROR**, never "nothing stored": answering `None` would
//!   make the tradehub resend its whole journal as first contact.
//!
//! # What a row may hold
//!
//! A journal id (32 hex digits, minted by the tradehub), the tradehub's identity line, the non-secret fingerprint
//! of the key that streamed it, sequences and times. Never a key and never a credential.

use std::path::Path;
use std::time::Duration;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::db::{
    StoreError, StoreErrorKind, database_present, open_for_read, open_for_write_within,
};
use crate::store_locator::db_path_in;

/// A gap the journal PRUNED: records no datahub had stored, past the tradehub's hold.
pub const GAP_PRUNED: &str = "pruned";
/// A gap a tradehub boot could not regenerate: the executions of the window may be incomplete.
pub const GAP_UNVERIFIED: &str = "unverified";

/// How long a write waits for the store's lock before it fails and the datahub retries its window.
pub const EXECUTION_STREAM_WRITE_BUSY: Duration = Duration::from_millis(250);

const EXECUTION_STREAM_DDL: &str = "
CREATE TABLE IF NOT EXISTS execution_stream (
    journal_id                 TEXT PRIMARY KEY,
    tradehub                   TEXT NOT NULL,
    key_id                     TEXT,
    stored_through             INTEGER,
    stored_through_recorded_ms INTEGER,
    writing_through            INTEGER,
    first_ms                   INTEGER NOT NULL,
    last_stored_ms             INTEGER,
    first_contact_through      INTEGER NOT NULL,
    generation                 INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS execution_stream_gap (
    journal_id          TEXT NOT NULL,
    kind                TEXT NOT NULL,
    after_sequence      INTEGER NOT NULL,
    resumes_at_sequence INTEGER NOT NULL,
    lost_from_ms        INTEGER,
    lost_to_ms          INTEGER,
    noted_ms            INTEGER NOT NULL,
    reason              TEXT,
    order_client_ids    TEXT NOT NULL,
    UNIQUE (journal_id, kind, after_sequence)
) STRICT;
CREATE TABLE IF NOT EXISTS execution_account_positions (
    journal_id  TEXT NOT NULL,
    generation  INTEGER NOT NULL,
    sequence    INTEGER NOT NULL,
    as_of_ms    INTEGER NOT NULL,
    venue       TEXT NOT NULL,
    account     TEXT NOT NULL,
    unknown     TEXT,
    positions   TEXT NOT NULL,
    PRIMARY KEY (journal_id, sequence, venue, account)
) STRICT;
";

/// Which stream a write is about, and who streams it: the open's journal id and identity line, and the key the
/// connection authenticated with (`None` on a key-less server).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStreamIdentity {
    /// The journal directory's id.
    pub journal_id: String,
    /// The tradehub's identity line, as its open stated it.
    pub tradehub: String,
    /// The non-secret fingerprint of the key that streamed it.
    pub key_id: Option<String>,
}

/// One journal's cursor, as the table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStreamRow {
    /// The journal directory's id.
    pub journal_id: String,
    /// The tradehub's identity line, as its last write stated it.
    pub tradehub: String,
    /// The fingerprint of the key that last streamed it.
    pub key_id: Option<String>,
    /// Every record up to it is stored; `None` while nothing is.
    pub stored_through: Option<u64>,
    /// The `recorded_ms` of the record at `stored_through`, when the datahub stored one there.
    pub stored_through_recorded_ms: Option<i64>,
    /// The last window begun (`None` before the first): above `stored_through` only while a window is in flight, or
    /// after a crash inside one.
    pub writing_through: Option<u64>,
    /// When the row was created.
    pub first_ms: i64,
    /// When a window was last stored.
    pub last_stored_ms: Option<i64>,
    /// The journal's head at its first open: every record up to it is checked against the rows already stored, and
    /// dropped when another journal id's stream already stored it.
    pub first_contact_through: u64,
    /// The journal's place in the order the datahub first heard from each: what a row's `journal_sequence` is
    /// ordered within.
    pub generation: i64,
}

/// One window of a journal the datahub knows it lacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStreamGapRow {
    /// The journal the window belongs to.
    pub journal_id: String,
    /// [`GAP_PRUNED`] or [`GAP_UNVERIFIED`].
    pub kind: String,
    /// The last record known whole before the window.
    pub after_sequence: u64,
    /// The first record after the window.
    pub resumes_at_sequence: u64,
    /// When the window starts, when known.
    pub lost_from_ms: Option<i64>,
    /// When the window ends, when known.
    pub lost_to_ms: Option<i64>,
    /// When the datahub recorded it.
    pub noted_ms: i64,
    /// Why, in the words of whoever found it: an unverified window's boot. `None` for a pruned window (its kind says
    /// why).
    pub reason: Option<String>,
    /// The orders whose executions the window may lack, when the boot could name them.
    pub order_client_ids: Vec<String>,
}

/// One account's row of a tradehub boot's POSITION SNAPSHOT (`vike_journal::JournalRecord::PositionSnapshot`, as the
/// stream carried it): the statement an account's window opens from.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionAccountPositionsRow {
    /// The journal it was journaled in.
    pub journal_id: String,
    /// That journal's generation ([`ExecutionStreamRow::generation`]).
    pub generation: i64,
    /// Its journal sequence.
    pub sequence: u64,
    /// When the tradehub took it (the record's `recorded_ms`).
    pub as_of_ms: i64,
    /// The venue's roster slug.
    pub venue: String,
    /// The account, in the `account` column's spelling (`DEFAULT` for the unlabelled one).
    pub account: String,
    /// Why the tradehub could not know its positions; `None` when [`Self::positions`] is its book.
    pub unknown: Option<String>,
    /// Its open positions, stored as a JSON array in the row (`vike_model::AccountPosition`'s serde); empty with no
    /// [`Self::unknown`]: flat.
    pub positions: Vec<vike_model::AccountPosition>,
}

/// **Record one account's position snapshot**, once: a second write of the same `(journal_id, sequence, venue,
/// account)` (a resend of its window) changes nothing, and neither does a journal re-identified after a copy
/// resending a snapshot another id already stored (the same account, time and sequence under another journal id).
///
/// # Errors
/// As [`begin_execution_window`]. Nothing was written.
pub fn record_account_positions(
    settings_dir: &Path,
    row: &ExecutionAccountPositionsRow,
) -> Result<(), StoreError> {
    let positions = serde_json::to_string(&row.positions)
        .map_err(|e| StoreError::io(&db_path_in(settings_dir), std::io::Error::other(e)))?;
    in_write(settings_dir, |tx| {
        tx.execute(
            "INSERT OR IGNORE INTO execution_account_positions (journal_id, generation, sequence, as_of_ms, \
             venue, account, unknown, positions) SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8 WHERE NOT EXISTS \
             (SELECT 1 FROM execution_account_positions WHERE venue = ?5 AND account = ?6 AND as_of_ms = ?4 \
             AND sequence = ?3 AND journal_id <> ?1)",
            params![
                row.journal_id,
                row.generation,
                sequence(row.sequence),
                row.as_of_ms,
                row.venue,
                row.account,
                row.unknown,
                positions
            ],
        )
        .map(drop)
    })
}

/// **Every account's latest position snapshot at or before `at_or_before_ms`**: one row per `(venue, account)`, the
/// newest by `(as_of_ms, generation, sequence)`, ordered by venue and account. An account with none that early is
/// absent: its window starts before its first snapshot.
///
/// # Errors
/// As [`read_execution_stream`]: a store that exists and cannot be read is never "no snapshot", and neither is a
/// positions cell that is no such array.
pub fn read_account_positions(
    settings_dir: &Path,
    at_or_before_ms: i64,
) -> Result<Vec<ExecutionAccountPositionsRow>, StoreError> {
    in_read(settings_dir, |conn| {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' \
             AND name = 'execution_account_positions')",
            [],
            |r| r.get(0),
        )?;
        if !there {
            return Ok(Vec::new()); // a store a build before the table streamed into: no snapshot yet
        }
        let mut statement = conn.prepare(
            "SELECT journal_id, generation, sequence, as_of_ms, venue, account, unknown, positions \
             FROM execution_account_positions WHERE as_of_ms <= ?1 \
             ORDER BY venue, account, as_of_ms DESC, generation DESC, sequence DESC",
        )?;
        let rows = statement
            .query_map([at_or_before_ms], |r| {
                Ok(ExecutionAccountPositionsRow {
                    journal_id: r.get(0)?,
                    generation: r.get(1)?,
                    sequence: unsigned(r.get(2)?),
                    as_of_ms: r.get(3)?,
                    venue: r.get(4)?,
                    account: r.get(5)?,
                    unknown: r.get(6)?,
                    positions: positions_of(&r.get::<_, String>(7)?)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut latest: Vec<ExecutionAccountPositionsRow> = Vec::new();
        for row in rows {
            let seen =
                latest.last().is_some_and(|l| (&l.venue, &l.account) == (&row.venue, &row.account));
            if !seen {
                latest.push(row);
            }
        }
        Ok(latest)
    })
    .map(Option::unwrap_or_default)
}

/// A snapshot row's positions, stored as a JSON array: a cell that is no such array is an error, never "flat".
fn positions_of(cell: &str) -> rusqlite::Result<Vec<vike_model::AccountPosition>> {
    serde_json::from_str(cell).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(7, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// **Open a journal's stream**: its cursor row, CREATED on the journal's first open with nothing stored and
/// `first_contact_through = head_sequence`, the head the tradehub's open states. A later open keeps every number and
/// takes the open's identity line and key.
///
/// # Errors
/// No settings database (none is created), a store busy past [`EXECUTION_STREAM_WRITE_BUSY`], or one that will not
/// open. Nothing was written.
pub fn open_execution_stream_row(
    settings_dir: &Path,
    stream: &ExecutionStreamIdentity,
    head_sequence: u64,
    now_ms: i64,
) -> Result<ExecutionStreamRow, StoreError> {
    in_write(settings_dir, |tx| {
        tx.execute(
            "INSERT INTO execution_stream (journal_id, tradehub, key_id, stored_through, writing_through, \
             first_ms, first_contact_through, generation) VALUES (?1, ?2, ?3, NULL, NULL, ?4, ?5, \
             (SELECT coalesce(max(generation), 0) + 1 FROM execution_stream)) \
             ON CONFLICT (journal_id) DO UPDATE SET tradehub = excluded.tradehub, key_id = excluded.key_id",
            params![stream.journal_id, stream.tradehub, stream.key_id, now_ms, sequence(head_sequence)],
        )?;
        tx.query_row(
            &format!("SELECT {ROW_COLUMNS} FROM execution_stream WHERE journal_id = ?1"),
            [&stream.journal_id],
            stream_row,
        )
    })
}

/// **Begin a window**: `writing_through` set to `through`. Step 1 of a flush, BEFORE any row of the window is
/// appended, on a row [`open_execution_stream_row`] created.
///
/// # Errors
/// As [`open_execution_stream_row`], and a journal with no row (its open never created one): nothing was written.
pub fn begin_execution_window(
    settings_dir: &Path,
    stream: &ExecutionStreamIdentity,
    through: u64,
) -> Result<(), StoreError> {
    in_write(settings_dir, |tx| {
        one_row(tx.execute(
            "UPDATE execution_stream SET writing_through = ?2 WHERE journal_id = ?1",
            params![stream.journal_id, sequence(through)],
        )?)
    })
}

/// **End a window**: `stored_through` set to `through`, with the `recorded_ms` of the record there and the time it
/// was stored. Step 3 of a flush, AFTER every row of the window landed. A gap moves the cursor past itself the same
/// way, with no record there (`recorded_ms` `None`).
///
/// # Errors
/// As [`begin_execution_window`].
pub fn end_execution_window(
    settings_dir: &Path,
    stream: &ExecutionStreamIdentity,
    through: u64,
    recorded_ms: Option<i64>,
    now_ms: i64,
) -> Result<(), StoreError> {
    in_write(settings_dir, |tx| {
        one_row(tx.execute(
            "UPDATE execution_stream SET stored_through = ?2, stored_through_recorded_ms = ?3, \
             writing_through = coalesce(max(writing_through, ?2), ?2), last_stored_ms = ?4 WHERE journal_id = ?1",
            params![stream.journal_id, sequence(through), recorded_ms, now_ms],
        )?)
    })
}

/// A cursor write touched its journal's row, or the row is gone: an error, never a write that silently did nothing.
fn one_row(changed: usize) -> rusqlite::Result<()> {
    if changed == 1 { Ok(()) } else { Err(rusqlite::Error::QueryReturnedNoRows) }
}

/// **Record a gap**, once: a second write of the same `(journal_id, kind, after_sequence)` (a resend of its window)
/// changes nothing.
///
/// # Errors
/// As [`begin_execution_window`].
pub fn record_execution_gap(
    settings_dir: &Path,
    gap: &ExecutionStreamGapRow,
) -> Result<(), StoreError> {
    let order_ids = serde_json::to_string(&gap.order_client_ids)
        .map_err(|e| StoreError::io(&db_path_in(settings_dir), std::io::Error::other(e)))?;
    in_write(settings_dir, |tx| {
        tx.execute(
            "INSERT OR IGNORE INTO execution_stream_gap (journal_id, kind, after_sequence, \
             resumes_at_sequence, lost_from_ms, lost_to_ms, noted_ms, reason, order_client_ids) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                gap.journal_id,
                gap.kind,
                sequence(gap.after_sequence),
                sequence(gap.resumes_at_sequence),
                gap.lost_from_ms,
                gap.lost_to_ms,
                gap.noted_ms,
                gap.reason,
                order_ids
            ],
        )
        .map(drop)
    })
}

/// **One journal's cursor**; `None` when the datahub stored nothing of it (first contact).
///
/// # Errors
/// No settings database, or one that exists and cannot be read: never answered as "nothing stored".
pub fn read_execution_stream(
    settings_dir: &Path,
    journal_id: &str,
) -> Result<Option<ExecutionStreamRow>, StoreError> {
    in_read(settings_dir, |conn| {
        conn.query_row(
            &format!("SELECT {ROW_COLUMNS} FROM execution_stream WHERE journal_id = ?1"),
            [journal_id],
            stream_row,
        )
        .optional()
    })
    .map(Option::flatten)
}

/// **Every journal's cursor and its gaps**, journals by id and gaps by sequence.
///
/// # Errors
/// As [`read_execution_stream`].
pub fn read_execution_streams(
    settings_dir: &Path,
) -> Result<Vec<(ExecutionStreamRow, Vec<ExecutionStreamGapRow>)>, StoreError> {
    in_read(settings_dir, |conn| {
        let streams: Vec<ExecutionStreamRow> = conn
            .prepare(&format!("SELECT {ROW_COLUMNS} FROM execution_stream ORDER BY journal_id"))?
            .query_map([], stream_row)?
            .collect::<rusqlite::Result<_>>()?;
        let mut gaps = conn.prepare(
            "SELECT journal_id, kind, after_sequence, resumes_at_sequence, lost_from_ms, lost_to_ms, noted_ms, \
             reason, order_client_ids FROM execution_stream_gap WHERE journal_id = ?1 ORDER BY after_sequence, kind",
        )?;
        streams
            .into_iter()
            .map(|stream| {
                let held = gaps
                    .query_map([&stream.journal_id], |r| {
                        Ok(ExecutionStreamGapRow {
                            journal_id: r.get(0)?,
                            kind: r.get(1)?,
                            after_sequence: unsigned(r.get(2)?),
                            resumes_at_sequence: unsigned(r.get(3)?),
                            lost_from_ms: r.get(4)?,
                            lost_to_ms: r.get(5)?,
                            noted_ms: r.get(6)?,
                            reason: r.get(7)?,
                            order_client_ids: order_ids_of(&r.get::<_, String>(8)?)?,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                Ok((stream, held))
            })
            .collect()
    })
    .map(Option::unwrap_or_default)
}

const ROW_COLUMNS: &str = "journal_id, tradehub, key_id, stored_through, stored_through_recorded_ms, \
     writing_through, first_ms, last_stored_ms, first_contact_through, generation";

fn stream_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionStreamRow> {
    Ok(ExecutionStreamRow {
        journal_id: r.get(0)?,
        tradehub: r.get(1)?,
        key_id: r.get(2)?,
        stored_through: r.get::<_, Option<i64>>(3)?.map(unsigned),
        stored_through_recorded_ms: r.get(4)?,
        writing_through: r.get::<_, Option<i64>>(5)?.map(unsigned),
        first_ms: r.get(6)?,
        last_stored_ms: r.get(7)?,
        first_contact_through: unsigned(r.get(8)?),
        generation: r.get(9)?,
    })
}

/// A gap row's order ids, stored as a JSON array: a cell that is no such array is an error, never "no orders".
fn order_ids_of(cell: &str) -> rusqlite::Result<Vec<String>> {
    serde_json::from_str(cell).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// A journal sequence as SQLite's INTEGER: a sequence counts records, so it never nears `i64::MAX`.
fn sequence(s: u64) -> i64 {
    i64::try_from(s).unwrap_or(i64::MAX)
}

fn unsigned(s: i64) -> u64 {
    u64::try_from(s).unwrap_or(0)
}

/// A read of an EXISTING store: `None` when the tables were never created (no stream was ever stored).
fn in_read<T>(
    settings_dir: &Path,
    body: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
) -> Result<Option<T>, StoreError> {
    let db = db_path_in(settings_dir);
    if !database_present(&db)? {
        return Err(StoreError { path: db, kind: StoreErrorKind::NoSettingsDatabase });
    }
    let conn = open_for_read(&db)?;
    let query = || -> rusqlite::Result<Option<T>> {
        let there: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'execution_stream')",
            [],
            |r| r.get(0),
        )?;
        if there { body(&conn).map(Some) } else { Ok(None) }
    };
    query().map_err(|e| StoreError::sql(&db, e))
}

/// One `IMMEDIATE` transaction against an EXISTING store, its tables created first; the open is the store's own,
/// which creates nothing (`crate::link_sessions`'s `write_into` is the shape).
fn in_write<T>(
    settings_dir: &Path,
    body: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<T>,
) -> Result<T, StoreError> {
    let db = db_path_in(settings_dir);
    let Some(mut conn) = open_for_write_within(&db, EXECUTION_STREAM_WRITE_BUSY)? else {
        return Err(StoreError { path: db, kind: StoreErrorKind::NoSettingsDatabase });
    };
    let write = || -> rusqlite::Result<T> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(EXECUTION_STREAM_DDL)?;
        let out = body(&tx)?;
        tx.commit()?;
        Ok(out)
    };
    write().map_err(|e| StoreError::sql(&db, e))
}

#[path = "execution_streams_tests.rs"]
#[cfg(test)]
mod execution_streams_tests;
