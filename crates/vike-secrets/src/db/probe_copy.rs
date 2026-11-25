//! **A deploy pre-flight's copy of the settings database, with every key replaced** — what `deploy/docker/vikectl`'s
//! `probe_copy` builds through `vike-cli backend probe-copy` before a NEW build's daemon boots on it (decision 0260).
//! It replaces the `sqlite3` scripts that stood there, so no box needs the `sqlite3` tool for a probe copy (the owner,
//! 2026-11-16).
//!
//! The order is the property, and it is the script's: the live database is attached to an IN-MEMORY one (it is only
//! ever read: its schema and rows are copied by `SELECT`, never written, inside ONE read transaction, so every table
//! comes from the same snapshot and a commit landing between two tables cannot hand the probe an orphan row), every
//! secret is replaced in memory (a credential's value and notes, a hub key's value, an account's notes, a SECRET venue
//! field's value and notes, [`ProbeCopy::secret_venue_fields`]), an invitation's table is dropped WHOLE
//! ([`DROPPED_TABLES`]), every `device_key` row is KEPT with a THROWAWAY pair in place of its private key
//! ([`rekey_devices`]), and only then is the copy written, compacted, by `VACUUM INTO` into a NEW file created at 0600
//! beside nothing (an existing one is refused, never replaced). No key value reaches the disk, and this function makes
//! none a Rust value: the copy and the overwrite are SQL inside the engine, and the throwaway pairs are the caller's
//! (this crate names no crypto). The written file is read back and refused unless every value left is a placeholder:
//! `probe-<row id>`, or the bare row id for a [`ProbeCopy::numeric_keys`] name, and no `device_key` row holds a
//! private key the live store holds.
//!
//! ⚠ **The device rows stay, their keys do not** (decision 0288: a private key never leaves its device). A daemon that
//! signs as its own device finds its row by `person/device` and boots: the Telegram channel REFUSES its start without
//! `owner/telegram`, which is why dropping the table failed its probe. A hub refuses the throwaway key, which costs a
//! probe nothing: it binds and starts, and dials no hub as a person.

use std::path::Path;

use rusqlite::Connection;

use super::open::{create_dir_private, create_file_private};
use super::{StoreError, open_for_read};

/// The tables a probe copy drops WHOLE: an invitation's secret (decision 0288). No probe needs their rows, and a table
/// a store has not created yet is skipped. `device_key` is not one: its rows stay, re-keyed ([`rekey_devices`]).
const DROPPED_TABLES: [&str; 1] = ["access_invite"];

/// What a probe copy changes beyond the cleaning every copy gets: the per-daemon rules of `deploy/docker/vikectl`'s
/// `shape`.
#[derive(Debug, Default)]
pub struct ProbeCopy<'a> {
    /// `credential` and `hub_key` rows removed by NAME (the tradehub's probe: the strategy builder's key, so its
    /// build thread waits instead of dialling the box's builder).
    pub drop_keys: &'a [String],
    /// Every `account` row set to `paper` (the tradehub's probe arms no venue account).
    pub paper_accounts: bool,
    /// `credential` names whose placeholder must be a NUMBER (the telegram channel's allowed ids): the row's id.
    pub numeric_keys: &'a [String],
    /// The `venue_setting` fields whose value is a SECRET, as `(venue, field)` (`vike_venues::venue_fields`'s
    /// `secret` rows, named by the caller, like the rest). Not a per-daemon rule: EVERY copy replaces their value
    /// with `probe-<row id>` and blanks their notes, and the read-back refuses one that survived.
    pub secret_venue_fields: &'a [(&'a str, vike_venues::venue_fields::VenueFieldName)],
}

/// **Write the probe copy of the live database `live` as `<into>/db/vike.db`.** `into` is the probe's settings
/// directory; its `db/` is created at 0700 and the file at 0600, and an existing file is refused. `throwaway` makes
/// one NEW device key per `device_key` row, its key id and its PKCS#8 document
/// (`vike_hub_protocol::access::DeviceKey::generate`, the caller's: this crate names no crypto). The read-back check
/// is the whole verdict.
///
/// # Errors
/// The live database does not open (or is no store this build reads), the destination exists or cannot be created,
/// a statement fails, `throwaway` fails, or the written copy still holds a value that is not a placeholder or a
/// private key the live store holds (the file is then removed).
pub fn write_probe_copy(
    live: &Path,
    into: &Path,
    copy: &ProbeCopy<'_>,
    throwaway: &mut dyn FnMut() -> std::io::Result<(String, Vec<u8>)>,
) -> Result<(), StoreError> {
    // The live store must be one this build reads: the version check every reader makes.
    drop(open_for_read(live)?);
    let dest = crate::store_locator::db_path_in(into);
    if dest.exists() {
        return Err(StoreError::io(
            &dest,
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a probe copy is written to a NEW file and never over one",
            ),
        ));
    }
    let memory = Connection::open_in_memory().map_err(|e| StoreError::sql(live, e))?;
    memory
        .execute_batch("PRAGMA temp_store = MEMORY; PRAGMA secure_delete = ON;")
        .map_err(|e| StoreError::sql(live, e))?;
    let live_path = live.to_string_lossy();
    memory
        .execute("ATTACH DATABASE ?1 AS live", [live_path.as_ref()])
        .map_err(|e| StoreError::sql(live, e))?;
    copy_live_into_memory(&memory).map_err(|e| StoreError::sql(live, e))?;
    memory.execute_batch("DETACH DATABASE live").map_err(|e| StoreError::sql(live, e))?;
    clean(&memory, copy).map_err(|e| StoreError::sql(live, e))?;
    rekey_devices(&memory, live, throwaway)?;

    if let Some(dir) = dest.parent() {
        create_dir_private(dir).map_err(|e| StoreError::io(&dest, e))?;
    }
    create_file_private(&dest).map_err(|e| StoreError::io(&dest, e))?;
    let dest_path = dest.to_string_lossy();
    memory
        .execute("VACUUM INTO ?1", [dest_path.as_ref()])
        .map_err(|e| StoreError::sql(&dest, e))?;
    drop(memory);
    let left = values_left(&dest, live, copy);
    match left {
        Ok(0) => Ok(()),
        other => {
            // A copy that still holds a value is no probe copy: it goes.
            let _ = std::fs::remove_file(&dest);
            Err(match other {
                Ok(n) => StoreError::io(
                    &dest,
                    std::io::Error::other(format!(
                        "the probe copy still held {n} value(s) that are not placeholders, so it was removed"
                    )),
                ),
                Err(e) => e,
            })
        }
    }
}

/// The live database's schema and rows, copied into `memory`'s main database: every table and its rows first, then
/// the indexes, triggers and views (a trigger created before the rows would fire on them), then the version stamp.
fn copy_live_into_memory(memory: &Connection) -> rusqlite::Result<()> {
    // ONE read transaction: every table is read from the same snapshot of the live store (an autocommit statement per
    // table would let a commit land between two of them). Committed before the caller's `DETACH`, which needs none open.
    let snapshot = memory.unchecked_transaction()?;
    let mut stmt = memory.prepare(
        "SELECT type, name, sql FROM live.sqlite_master \
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY rowid",
    )?;
    let objects: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (_, name, sql) in objects.iter().filter(|(kind, ..)| kind == "table") {
        memory.execute_batch(sql)?;
        let quoted = name.replace('"', "\"\"");
        memory.execute(
            &format!("INSERT INTO main.\"{quoted}\" SELECT * FROM live.\"{quoted}\""),
            [],
        )?;
    }
    let has_sequence: bool = memory.query_row(
        "SELECT count(*) > 0 FROM live.sqlite_master WHERE name = 'sqlite_sequence'",
        [],
        |r| r.get(0),
    )?;
    if has_sequence {
        memory.execute_batch(
            "DELETE FROM main.sqlite_sequence; \
             INSERT INTO main.sqlite_sequence SELECT * FROM live.sqlite_sequence;",
        )?;
    }
    for (_, _, sql) in objects.iter().filter(|(kind, ..)| kind != "table") {
        memory.execute_batch(sql)?;
    }
    let version: i64 = memory.query_row("PRAGMA live.user_version", [], |r| r.get(0))?;
    memory.pragma_update(None, "user_version", version)?;
    snapshot.commit()
}

/// Replace every secret in `memory`, drop [`DROPPED_TABLES`], then apply `copy`'s per-daemon rules.
fn clean(memory: &Connection, copy: &ProbeCopy<'_>) -> rusqlite::Result<()> {
    memory.execute_batch(
        "UPDATE credential SET value = 'probe-' || id, notes = NULL; \
         UPDATE hub_key SET value = 'probe-' || id; \
         UPDATE account SET notes = NULL;",
    )?;
    // The field is bound in its STORE spelling (`VenueFieldName::store_spelling`, `SOCKS_PROXY`), the one every
    // writer stores; the SQL still folds the case, so a row hand-typed SQL spelled otherwise (the store is never
    // migrated) is cleaned too. `field` has no `COLLATE NOCASE`. The venue is folded for the same reason.
    for (venue, field) in copy.secret_venue_fields {
        memory.execute(
            "UPDATE venue_setting SET value = 'probe-' || id, notes = NULL \
             WHERE upper(field) = upper(?2) \
             AND venue_id IN (SELECT id FROM venue WHERE lower(name) = lower(?1))",
            (venue, field.store_spelling()),
        )?;
    }
    for table in DROPPED_TABLES {
        memory.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))?;
    }
    for name in copy.drop_keys {
        memory.execute("DELETE FROM hub_key WHERE name = ?1", [name])?;
        memory.execute("DELETE FROM credential WHERE name = ?1", [name])?;
    }
    if copy.paper_accounts {
        memory.execute_batch("UPDATE account SET tier = 'paper';")?;
    }
    for name in copy.numeric_keys {
        memory.execute("UPDATE credential SET value = CAST(id AS TEXT) WHERE name = ?1", [name])?;
    }
    Ok(())
}

/// Give every `device_key` row a THROWAWAY pair from `throwaway`: its key id and its PKCS#8 document in place of the
/// live ones, the row's `person/device` kept, so a daemon finds its device and signs with a key no hub lists. Only
/// the row ids are read. A store that has not created the table yet has nothing to re-key.
fn rekey_devices(
    memory: &Connection,
    live: &Path,
    throwaway: &mut dyn FnMut() -> std::io::Result<(String, Vec<u8>)>,
) -> Result<(), StoreError> {
    let sql = |e| StoreError::sql(live, e);
    let held: bool = memory
        .query_row(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'device_key'",
            [],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if !held {
        return Ok(());
    }
    let mut stmt = memory.prepare("SELECT rowid FROM device_key").map_err(sql)?;
    let rows: Vec<i64> = stmt
        .query_map([], |r| r.get(0))
        .map_err(sql)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql)?;
    drop(stmt);
    for row in rows {
        let (key_id, document) = throwaway().map_err(|e| StoreError::io(live, e))?;
        memory
            .execute(
                "UPDATE device_key SET key_id = ?1, private_key = ?2 WHERE rowid = ?3",
                rusqlite::params![key_id, document, row],
            )
            .map_err(sql)?;
    }
    Ok(())
}

/// How many values the written copy at `dest` still holds that are not placeholders, counting a secret venue field
/// whose value or notes survived, a dropped table that is still there, a dropped key name that is still a row, and a
/// `device_key` row whose private key is one the LIVE store `live` holds (attached read-only and compared inside the
/// engine, so no key becomes a Rust value).
fn values_left(dest: &Path, live: &Path, copy: &ProbeCopy<'_>) -> Result<i64, StoreError> {
    let conn = open_for_read(dest)?;
    let count = |sql: &str, name: Option<&str>| -> Result<i64, StoreError> {
        match name {
            Some(n) => conn.query_row(sql, [n], |r| r.get(0)),
            None => conn.query_row(sql, [], |r| r.get(0)),
        }
        .map_err(|e| StoreError::sql(dest, e))
    };
    let mut left = count(
        "SELECT (SELECT count(*) FROM credential WHERE notes IS NOT NULL \
                 OR (value <> 'probe-' || id AND value <> CAST(id AS TEXT))) \
              + (SELECT count(*) FROM hub_key WHERE value <> 'probe-' || id) \
              + (SELECT count(*) FROM account WHERE notes IS NOT NULL)",
        None,
    )?;
    for (venue, field) in copy.secret_venue_fields {
        left += conn
            .query_row(
                "SELECT count(*) FROM venue_setting \
                 WHERE upper(field) = upper(?2) \
                 AND venue_id IN (SELECT id FROM venue WHERE lower(name) = lower(?1)) \
                 AND (value <> 'probe-' || id OR notes IS NOT NULL)",
                (venue, field.store_spelling()),
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| StoreError::sql(dest, e))?;
    }
    for table in DROPPED_TABLES {
        left += count(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            Some(table),
        )?;
    }
    for name in copy.drop_keys {
        left += count(
            "SELECT (SELECT count(*) FROM hub_key WHERE name = ?1) + (SELECT count(*) FROM credential WHERE name = ?1)",
            Some(name),
        )?;
    }
    let devices = count(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'device_key'",
        None,
    )?;
    if devices > 0 {
        let live_path = live.to_string_lossy();
        conn.execute("ATTACH DATABASE ?1 AS live", [live_path.as_ref()])
            .map_err(|e| StoreError::sql(live, e))?;
        left += count(
            "SELECT count(*) FROM main.device_key \
             WHERE private_key IN (SELECT private_key FROM live.device_key)",
            None,
        )?;
    }
    Ok(left)
}
