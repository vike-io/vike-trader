//! **The settings database's nightly copy, and its integrity check** — the owner's "agree" of 2026-11-23 to ruling Q5 of
//! the error-handling plan; the design is `docs/decisions/0054-settings-move-into-one-database.md`'s addendum of
//! 2026-11-23. One long-running process calls both (`crates/vike-gatewayhub/src/store_backup.rs`): the check at its start,
//! the copy once a UTC day.
//!
//! * [`check_store_in`] asks the engine's `PRAGMA quick_check` of the store, read-only. A store it finds damaged (or one
//!   the engine calls corrupt or not a database at all) is [`StoreIntegrity::Damaged`], which names the newest copy to
//!   restore from. Any other failure (a busy store, an absent one, another schema) is the [`StoreError`]: it is no damage
//!   and a restore would be the wrong advice.
//! * [`back_up_store_in`] writes ONE copy per UTC day, `<settings>/db/backups/vike.db.YYYY-MM-DD`, and does nothing when
//!   that day's copy exists, so a restart makes no second one. It checks the store first and copies only a sound one: a
//!   damaged copy would push a sound one out. The copy is `VACUUM INTO` from ONE read transaction (a consistent
//!   snapshot; the store is only read), into a NEW file created at 0600 in a 0700 folder and then given the store's own
//!   permissions, checked the same way, and only then renamed to its day's name, so a name always holds a whole copy.
//!   Then the copies past the newest [`STORE_BACKUPS_KEPT`] are removed, oldest first.
//!
//! ⚠ **A copy IS a credential store** (every venue key is in it), and the rules of one hold: it never leaves the box (it
//! sits beside the store, and nothing here sends a byte anywhere), no row value becomes a Rust value here (the copy is
//! SQL inside the engine; a finding is the engine's own words about pages and indexes), and it has the store's mode.
//! **What this file removes is a COPY of its own name, never the store**: the store is not in `backups/` and no name
//! `copy_day` accepts is the store's. A file of any other name there (an operator's hand copy, an older script's) is
//! never touched. The removal is the owner's ruling, and `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`
//! keys [`back_up_store_in`] so each caller is argued.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::ErrorCode;
use vike_model::MS_PER_DAY;

use super::open::{create_dir_private, create_file_private};
use super::{StoreError, StoreErrorKind, open_for_read};
use crate::store_locator::{DB_DIR, DB_FILE, db_path_in};

/// How many nightly copies stay: the newest seven, so a week of days to restore from (the owner's ruling).
pub const STORE_BACKUPS_KEPT: usize = 7;

/// The folder beside the store that holds the copies.
const BACKUP_DIR: &str = "backups";

/// The end of the name a copy is written under before it is whole, `vike.db.partial`: one name for every day, so a
/// copy a killed process left half-written is removed by the next attempt rather than kept.
const IN_PROGRESS: &str = "partial";

/// What `PRAGMA quick_check` found in a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreIntegrity {
    /// The engine answered `ok`.
    Sound,
    /// The engine found damage.
    Damaged(StoreDamage),
}

/// A damaged store, and the copy to restore it from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreDamage {
    /// What the engine said: its own lines about pages and indexes, or its refusal to read the file at all. Never a row
    /// value.
    pub findings: Vec<String>,
    /// The newest nightly copy, the one to restore from; `None` when there is none yet.
    pub restore_from: Option<PathBuf>,
}

/// What one call of [`back_up_store_in`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreBackup {
    /// The day's copy was written; `removed` are the older copies past the newest [`STORE_BACKUPS_KEPT`].
    Made {
        /// The new copy.
        copy: PathBuf,
        /// The copies removed, oldest first.
        removed: Vec<PathBuf>,
    },
    /// The day's copy already exists: nothing was read, written or removed.
    AlreadyMade(PathBuf),
    /// The store is damaged, so nothing was copied and nothing removed.
    Damaged(StoreDamage),
}

/// **`PRAGMA quick_check` of the store in `settings_dir`**, read-only. A damaged store names the newest copy.
///
/// # Errors
/// The store could not be checked for a reason that is not damage: absent, busy, another schema, a read failure.
pub fn check_store_in(settings_dir: &Path) -> Result<StoreIntegrity, StoreError> {
    Ok(match quick_check(&db_path_in(settings_dir))? {
        None => StoreIntegrity::Sound,
        Some(findings) => StoreIntegrity::Damaged(StoreDamage {
            findings,
            restore_from: newest_copy(&backup_dir(settings_dir)),
        }),
    })
}

/// **The day's copy of the store in `settings_dir`**, the day `now_ms`'s UTC date; see the module doc for the order.
///
/// # Errors
/// The store could not be checked (as [`check_store_in`]), the copy could not be written, checked or named, or
/// an old copy could not be removed (the new copy then stands, and the next call removes it).
pub fn back_up_store_in(settings_dir: &Path, now_ms: i64) -> Result<StoreBackup, StoreError> {
    let db = db_path_in(settings_dir);
    let dir = backup_dir(settings_dir);
    let copy = dir.join(copy_name(now_ms));
    if copy.is_file() {
        return Ok(StoreBackup::AlreadyMade(copy));
    }
    if let Some(findings) = quick_check(&db)? {
        return Ok(StoreBackup::Damaged(StoreDamage { findings, restore_from: newest_copy(&dir) }));
    }
    let partial = dir.join(format!("{DB_FILE}.{IN_PROGRESS}"));
    write_copy(&db, &partial)?;
    fs::rename(&partial, &copy).map_err(|e| StoreError::io(&copy, e))?;
    let copies = copies(&dir).map_err(|e| StoreError::io(&dir, e))?;
    let mut removed = Vec::new();
    for old in &copies[..copies.len().saturating_sub(STORE_BACKUPS_KEPT)] {
        fs::remove_file(old).map_err(|e| StoreError::io(old, e))?;
        removed.push(old.clone());
    }
    Ok(StoreBackup::Made { copy, removed })
}

/// `<settings>/db/backups`.
fn backup_dir(settings_dir: &Path) -> PathBuf {
    settings_dir.join(DB_DIR).join(BACKUP_DIR)
}

/// `vike.db.YYYY-MM-DD`, the UTC date of `now_ms`.
fn copy_name(now_ms: i64) -> String {
    let (year, month, day) = vike_utility::calendar::civil_from_days(now_ms.div_euclid(MS_PER_DAY));
    format!("{DB_FILE}.{year:04}-{month:02}-{day:02}")
}

/// The day a copy's name carries (`YYYY-MM-DD`), or `None` for every other name: the store's, the copy in progress, an
/// operator's hand copy, an older script's.
fn copy_day(name: &str) -> Option<&str> {
    let day = name.strip_prefix(DB_FILE)?.strip_prefix('.')?;
    let shaped = day.len() == 10
        && day
            .bytes()
            .enumerate()
            .all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
    shaped.then_some(day)
}

/// Every copy in `dir`, oldest first (a name sorts by its day). No folder is no copy.
fn copies(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.file_name().and_then(|n| n.to_str()).and_then(copy_day).is_some() && path.is_file()
        {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// The newest copy in `dir`. A folder that cannot be listed names none, and the damage line says none was found.
fn newest_copy(dir: &Path) -> Option<PathBuf> {
    copies(dir).ok().and_then(|mut found| found.pop())
}

/// `PRAGMA quick_check` of the database `db`: `None` when the engine answered `ok`, else what it found. The engine calling
/// the file corrupt or not a database, at the open or mid-check, is a finding too.
fn quick_check(db: &Path) -> Result<Option<Vec<String>>, StoreError> {
    let checked = open_for_read(db).and_then(|conn| {
        let mut statement =
            conn.prepare("PRAGMA quick_check").map_err(|e| StoreError::sql(db, e))?;
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))
            .and_then(|found| found.collect::<rusqlite::Result<Vec<String>>>())
            .map_err(|e| StoreError::sql(db, e))?;
        Ok(rows)
    });
    match checked {
        Ok(rows) if rows == ["ok"] => Ok(None),
        Ok(rows) => Ok(Some(rows)),
        Err(e) if is_damage(&e) => Ok(Some(vec![e.to_string()])),
        Err(e) => Err(e),
    }
}

/// The engine's two words for a file that is damaged rather than busy, absent or of another schema.
fn is_damage(e: &StoreError) -> bool {
    matches!(
        &e.kind,
        StoreErrorKind::Sqlite(rusqlite::Error::SqliteFailure(inner, _))
            if matches!(inner.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    )
}

/// `VACUUM INTO` the NEW file `partial` from `db`, with `db`'s permissions, then checked. The engine opens the output
/// read-write even from this read-only connection, and syncs it at the store's own `synchronous` level.
fn write_copy(db: &Path, partial: &Path) -> Result<(), StoreError> {
    let io_error = |e: io::Error| StoreError::io(partial, e);
    if let Some(dir) = partial.parent() {
        create_dir_private(dir).map_err(io_error)?;
    }
    match fs::remove_file(partial) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(e)),
    }
    // Empty at 0600 before the engine writes a page into it (`create_file_private`'s doc); the store's mode after.
    create_file_private(partial).map_err(io_error)?;
    let conn = open_for_read(db)?;
    conn.execute("VACUUM INTO ?1", [partial.to_string_lossy().as_ref()])
        .map_err(|e| StoreError::sql(db, e))?;
    drop(conn);
    let permissions = fs::metadata(db).map_err(|e| StoreError::io(db, e))?.permissions();
    fs::set_permissions(partial, permissions).map_err(io_error)?;
    match quick_check(partial)? {
        None => Ok(()),
        Some(findings) => Err(io_error(io::Error::other(format!(
            "the copy failed its own integrity check ({}); it stays under its in-progress name until the next attempt",
            findings.first().map_or("", String::as_str)
        )))),
    }
}
