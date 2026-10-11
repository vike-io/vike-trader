//! Opening the database: version constant, check and stamp, read/write opens, modes, header probe.

use super::*;

/// **The one schema this code reads and writes.** Lives in `PRAGMA user_version`.
///
/// A database carrying any other number is [`DbErrorKind::SchemaVersion`] — loudly unreadable
/// rather than quietly empty, which is the difference between "this box is not configured" and
/// "this box is configured and cannot authenticate". ⚠ The loud half matters because of who reads
/// it: `vike_bridge_core::credentials::load_workspace_secrets_at` — the infallible wrapper EVERY
/// composition root reaches through — logs one line and returns an EMPTY map, and an empty
/// credential map is not an error downstream; it is the LIVE GATE.
/// `crates/vike-secrets/tests/store/database/read_path.rs`'s
/// `an_unreadable_database_is_empty_to_the_infallible_reader_and_loud_to_the_fallible_one` pins
/// that mechanism.
pub const SCHEMA_VERSION: i64 = 2;

/// Is there a database at `path`?
///
/// **The whole of the stage-2 decision, and deliberately one `is_file` on one path.** See
/// `crate::store::Backend`, which is where the argument for a per-RUN choice over a per-KEY
/// fallback lives.
#[must_use]
pub fn database_present(path: &Path) -> bool {
    path.is_file()
}

// ---------------------------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------------------------

/// **How long any statement here waits for a lock another connection is holding** before the engine
/// gives up and returns `SQLITE_BUSY`.
///
/// ⚠ **This is a PIN, not a new behaviour, and the difference is the reason it is written down.**
/// MEASURED in rusqlite 0.40's `InnerConnection::open_with_flags`: every connection it opens is
/// already given `sqlite3_busy_timeout(db, 5000)`, so this store has never been without one. What it
/// has been without is a timeout it CHOSE — rusqlite's own `Connection::busy_timeout` doc says the
/// default "may be subject to change", and a silent change to how long a live daemon's credential
/// read waits for the migrator's lock is not a thing this store should learn about from a dependency
/// bump. Setting it explicitly costs one call and makes the number this file's answer.
///
/// It is set on the READ path as well as the write path deliberately. The contended moment is a
/// process booting (reading credentials) while an operator runs `secrets init` or `set-book`;
/// waiting a few seconds for a sub-millisecond write is strictly better than failing, and the
/// failure it replaces is the LIVE GATE — a credential read that errors is a box with no keys.
///
/// ⚠ **A busy timeout does NOT fix a DEFERRED read-then-write, and reaching for one instead of
/// `TransactionBehavior::Immediate` is the mistake it invites.** SQLite returns `SQLITE_BUSY`
/// IMMEDIATELY — without consulting the busy handler at all — when a connection holding a SHARED
/// lock tries to promote to RESERVED while another connection already holds RESERVED, because
/// sleeping there could deadlock both sides. So the timeout below is what covers a writer waiting
/// for a writer; [`set_venue_account_id`]'s `Immediate` is what stops this module's one
/// read-then-write from being that unpromotable case.
pub(super) const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5_000);

/// Open READ-ONLY, verifying the schema version. Never creates anything.
///
/// Read-only is not a nicety: the deployed daemon mounts `settings/` read-only apart from one narrow
/// grant, an operator reading the store with the daemon down may have no write access to the
/// directory at all, and a reader that could CREATE the file would turn "this box has no database"
/// into "this box has an empty database" — which under `crate::store::Backend` is the difference
/// between the file answering and every venue silently going to paper.
pub(crate) fn open_for_read(path: &Path) -> Result<Connection, DbError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| DbError::sql(path, e))?;
    // The pin, before the first statement — see [`BUSY_TIMEOUT`].
    conn.busy_timeout(BUSY_TIMEOUT).map_err(|e| DbError::sql(path, e))?;
    check_schema_version(path, &conn)?;
    Ok(conn)
}

/// Open READ-WRITE, creating the directory, the file, the modes and the schema when they are absent.
///
/// The order is load-bearing and is the module doc's *the modes are set, never inherited*: directory
/// (0700), then an empty file (0600), then the engine — so no row has ever existed under the umask's
/// answer. Returns whether this call created the database.
///
/// # ⚠ It does NOT stamp [`SCHEMA_VERSION`], and that is the whole of the crash story
///
/// `PRAGMA user_version` is written by the CALLER, **after** its own transaction commits
/// ([`stamp_schema_version`]). This function used to stamp it here, between the schema batch and the
/// caller's inserts, and every failure after that point — a full disk, a read-only remount, `SIGKILL`
/// — left a file that `database_present` calls a database, that `check_schema_version` accepts, and
/// that holds NO ROWS. `crate::store::Backend` then answers `Database` for every process on the box
/// forever, which is not an error downstream but the LIVE GATE: every venue silently on paper.
///
/// With the stamp deferred, that same crash leaves `user_version = 0`, which
/// [`check_schema_version`] already treats as the loud [`DbErrorKind::SchemaVersion`]. The unfinished
/// database announces itself instead of impersonating a finished one.
/// Returns the connection and whether this call created the database. An existing database is
/// refused unless it carries [`SCHEMA_VERSION`].
///
/// ⚠ `PRAGMA foreign_keys` is SET AND VERIFIED here, and it is not decoration: it is **OFF by
/// default, per connection**, so schema 2's `credential.account_id REFERENCES account(id)` and
/// `account.parent_id REFERENCES account(id)` would enforce NOTHING without it — the spec's *a
/// schema is a gate* claim would be false for both columns. Same set-then-believe-the-engine shape
/// as the journal mode above, and for the same reason: a pragma that did not take is silent
/// everywhere else. It is set BEFORE any transaction because SQLite is a no-op for it inside one.
pub(crate) fn open_for_write(path: &Path) -> Result<(Connection, bool), DbError> {
    open_for_write_within(path, BUSY_TIMEOUT)
}

/// [`open_for_write`], with the busy wait as a PARAMETER rather than the shared [`BUSY_TIMEOUT`] —
/// for a caller whose own budget is a property of who is waiting rather than of this store, the same
/// argument `vike_config::write`'s `LockBudget` makes for the settings FILE's own lock.
/// `crate::settings::write_setting_row_in` is the one caller today: a GUI frame, a daemon connection
/// thread and an interactive CLI wait different amounts for the same `BEGIN IMMEDIATE`.
pub(crate) fn open_for_write_within(
    path: &Path,
    busy: std::time::Duration,
) -> Result<(Connection, bool), DbError> {
    let created = !path.exists();
    if let Some(dir) = path.parent() {
        create_dir_private(dir).map_err(|e| DbError::io(path, e))?;
    }
    if created {
        create_file_private(path).map_err(|e| DbError::io(path, e))?;
    }
    let conn = Connection::open(path).map_err(|e| DbError::sql(path, e))?;
    // The pin, before the first statement — see [`BUSY_TIMEOUT`]. ⚠ Ahead of the journal-mode
    // pragma below on purpose: that pragma takes a RESERVED lock, so a migrator holding one is
    // enough to make this open fail before any of this function's own checks have run.
    conn.busy_timeout(busy).map_err(|e| DbError::sql(path, e))?;

    // Set it, then BELIEVE THE ENGINE rather than the request. A journal mode that did not take is
    // the failure this pragma exists to prevent, and it is silent everywhere else.
    let answered: String = conn
        .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
        .map_err(|e| DbError::sql(path, e))?;
    if !answered.eq_ignore_ascii_case("delete") {
        return Err(DbError {
            path: path.to_path_buf(),
            kind: DbErrorKind::JournalMode { answered },
        });
    }

    conn.execute_batch("PRAGMA foreign_keys = ON;").map_err(|e| DbError::sql(path, e))?;
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .map_err(|e| DbError::sql(path, e))?;
    if fk != 1 {
        return Err(DbError { path: path.to_path_buf(), kind: DbErrorKind::ForeignKeys });
    }

    if created {
        conn.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(path, e))?;
    } else {
        check_schema_version(path, &conn)?;
    }
    Ok((conn, created))
}

pub(super) fn stamp_schema_version(path: &Path, conn: &Connection) -> Result<(), DbError> {
    conn.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(|e| DbError::sql(path, e))
}

/// Refuse a database unless it carries [`SCHEMA_VERSION`] — the one check, for every other
/// `user_version` alike.
pub(super) fn check_schema_version(path: &Path, conn: &Connection) -> Result<(), DbError> {
    let found: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| DbError::sql(path, e))?;
    if found == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(DbError {
            path: path.to_path_buf(),
            kind: DbErrorKind::SchemaVersion { found, expected: SCHEMA_VERSION },
        })
    }
}

/// `mkdir -p` at 0700 on unix — the components this call creates only. An existing directory's mode
/// is the operator's, and is REPORTED by `crate::store::permission_warning` rather than changed.
#[cfg(unix)]
pub(super) fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

/// Windows has no mode bits; the equivalent question is an ACL query, which needs a Win32 crate this
/// workspace does not carry. The same no-op posture `crate::store::permission_warning` already takes.
#[cfg(not(unix))]
pub(super) fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Create the database file EMPTY at 0600, before the engine sees the path.
///
/// A zero-length file is a valid empty SQLite database, so this costs nothing and closes the window
/// in which rows would exist at the umask's mode. `create_new` means a racing sibling that got there
/// first is joined, never clobbered.
#[cfg(unix)]
pub(super) fn create_file_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(not(unix))]
pub(super) fn create_file_private(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
