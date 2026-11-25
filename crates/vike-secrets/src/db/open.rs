//! Opening the database: version constant, check and stamp, read/write opens, modes, header probe.

use super::*;

/// **The one schema this code reads and writes.** Lives in `PRAGMA user_version`.
///
/// A database carrying any other number is [`StoreErrorKind::SchemaVersion`] — loudly unreadable
/// rather than quietly empty, which is the difference between "this box is not configured" and
/// "this box is configured and cannot authenticate". ⚠ The loud half matters because of who reads
/// it: a root that DEGRADES an unreadable store (`vike_secrets::CredentialRead::unreadable`, then
/// `Resolved::report`) logs one line and continues with an EMPTY map, and an empty
/// credential map is not an error downstream; it is the LIVE GATE.
/// `crates/vike-secrets/tests/store/database/read_path.rs`'s
/// `an_unreadable_database_is_empty_to_the_infallible_reader_and_loud_to_the_fallible_one` pins
/// that mechanism.
pub const SCHEMA_VERSION: i64 = 2;

/// Is there a database at `path`? Three answers: `Ok(true)` a file is there, `Ok(false)` nothing is
/// (the live gate), and `Err` the filesystem would not say (a directory this process may not search,
/// an I/O error).
///
/// **The whole of the stage-2 decision, and deliberately one `stat` on one path.** See
/// `crate::store::StoreBackend`, which is where the argument for a per-RUN choice over a per-KEY
/// fallback lives. ⚠ The `Err` is the root rule's *a store that EXISTS and cannot be read is an
/// error*: this was `is_file()`, which answers `false` on every stat failure, so a store behind a
/// directory this process may not search read as "no store, not configured".
///
/// # Errors
/// [`StoreErrorKind::Io`] with the filesystem's error, for anything but "not found".
pub fn database_present(path: &Path) -> Result<bool, StoreError> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.is_file()),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(StoreError::io(path, e)),
    }
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
/// for a writer, and EVERY write transaction in this crate is `Immediate` (it takes the write lock
/// at `BEGIN`, where the busy handler can wait for it), which is what keeps a read-then-write from
/// being that unpromotable case: `crates/vike-secrets/tests/store/write_open.rs`'s
/// `a_write_waits_for_a_held_lock_and_lands` holds the lock against the writers.
pub(super) const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5_000);

/// Open READ-ONLY, verifying the schema version. Never creates anything.
///
/// Read-only is not a nicety: the deployed daemon mounts `settings/` read-only apart from one narrow
/// grant, an operator reading the store with the daemon down may have no write access to the
/// directory at all, and a reader that could CREATE the file would turn "this box has no database"
/// into "this box has an empty database" — which under `crate::store::StoreBackend` is the difference
/// between the file answering and every venue silently going to paper.
///
/// ⚠ **The one case it may touch the file: a writer killed inside a commit.** A read-only connection may not replay the journal
/// that left, and a refused read is the live gate, so [`replay_hot_journal`] replays it once and this opens again. Every other
/// refusal stands, and so does this one for a process that may not write.
pub(crate) fn open_for_read(path: &Path) -> Result<Connection, StoreError> {
    let open_read_only = || -> Result<Connection, StoreError> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| StoreError::sql(path, e))?;
        // The pin, before the first statement — see [`BUSY_TIMEOUT`].
        conn.busy_timeout(BUSY_TIMEOUT).map_err(|e| StoreError::sql(path, e))?;
        check_schema_version(path, &conn)?;
        Ok(conn)
    };
    match open_read_only() {
        Err(StoreError { kind: StoreErrorKind::ReadOnlyRollback, .. }) => {
            replay_hot_journal(path);
            open_read_only()
        }
        opened => opened,
    }
}

/// What the last replay in this process said, for a root with no log subscriber ([`take_store_recovery`]).
static RECOVERY: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// **The replay this process performed, if any, once.** `vike-cli`'s `main` prints it to stderr: it installs no subscriber, and
/// as a daemon's `ExecStartPre=` it is the reader most likely to meet the journal, so without this a killed writer's recovery
/// leaves no trace in the unit's journal. A read returns findings and never logs; this is that rule for a read deep in a boot.
pub fn take_store_recovery() -> Option<String> {
    RECOVERY.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
}

/// **SQLite's normal recovery, asked for by the one reader that met it:** one brief read-write open, whose first read replays
/// the journal a killed writer left (the database back at its last committed state, the journal gone). No row is written, no
/// pragma set and nothing created: `SQLITE_OPEN_CREATE` is not among the flags. The engine decides the journal is hot only
/// while no writer holds the store, so a live writer's journal is never touched.
///
/// A process that may not write gets a read-only handle from the engine and the same refusal, so nothing here reports a
/// failure: the caller's second try is the answer, in [`StoreErrorKind::ReadOnlyRollback`]'s words.
///
/// ⚠ **No retry here, measured.** The pager takes the `EXCLUSIVE` lock a replay needs with no busy handler of its own, but the
/// btree above it retries that whole step through the connection's, so another reader's `SHARED` costs up to
/// [`BUSY_TIMEOUT`], not a refusal (`tests/store/hot_rollback_journal.rs`). A loop here would only multiply that wait.
pub(crate) fn replay_hot_journal(path: &Path) {
    let replayed = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))
    });
    if replayed.is_ok() {
        let said = format!(
            "the settings database {} held the journal of a writer killed mid-write, and it has been replayed, \
             SQLite's normal recovery: what that writer never committed is gone and every committed row stands",
            path.display()
        );
        crate::warn_once(&said);
        *RECOVERY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(said);
    }
}

/// **Open an EXISTING store READ-WRITE** — what every writer but the store's creation uses. `None`:
/// there is no database at `path`, and NOTHING was created (no `SQLITE_OPEN_CREATE`, no file made), so
/// there is nothing to remove either: the caller refuses with its own "no store" word.
///
/// ⚠ It used to open with SQLite's default CREATE flag and hand back `created`, and seventeen callers
/// then removed, BY PATH, a `vike.db` they found they had just made, so anything put at that path in
/// between (an operator restoring the store with `mv`) was deleted. Without the flag there is no such
/// moment: an absent store stays absent.
///
/// An existing database is refused unless it carries [`SCHEMA_VERSION`], and loses the retired
/// `account_one_account_per_book` index if it still has one (decision 0274).
pub(crate) fn open_for_write(path: &Path) -> Result<Option<Connection>, StoreError> {
    open_for_write_within(path, BUSY_TIMEOUT)
}

/// [`open_for_write`], with the busy wait as a PARAMETER rather than the shared [`BUSY_TIMEOUT`] —
/// for a caller whose own budget is a property of who is waiting rather than of this store, the same
/// argument `vike_config::write`'s `LockBudget` makes for the settings FILE's own lock: a GUI frame, a
/// daemon connection thread and an interactive CLI wait different amounts for the same `BEGIN IMMEDIATE`.
pub(crate) fn open_for_write_within(
    path: &Path,
    busy: std::time::Duration,
) -> Result<Option<Connection>, StoreError> {
    let conn = match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(conn) => conn,
        Err(_) if !database_present(path)? => return Ok(None),
        Err(e) => return Err(StoreError::sql(path, e)),
    };
    set_write_pragmas(path, &conn, busy)?;
    check_schema_version(path, &conn)?;
    // Decision 0274: the venue account number is a field, never a key, so the one index that
    // keyed on it leaves an older store at its first write-open. It moves no row and changes no
    // version (`crate::schema::DDL`'s note); on a store without it, it is a no-op.
    conn.execute_batch("DROP INDEX IF EXISTS account_one_account_per_book;")
        .map_err(|e| StoreError::sql(path, e))?;
    Ok(Some(conn))
}

/// **The store's CREATION open, for the `test-support` fixtures and this crate's tests**: the
/// directory (0700) and the EMPTY file (0600) when absent ([`make_database_file`]), then the engine
/// over it ([`open_made_for_write`]). Returns the connection and whether this call made the file.
/// [`create_store`] composes the same two halves itself, so it keeps WHICH file it made.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn open_or_create_for_write(path: &Path) -> Result<(Connection, bool), StoreError> {
    let made = make_database_file(path)?.is_some();
    Ok((open_made_for_write(path, made)?, made))
}

/// The creation's first half: the directory, then the EMPTY file, before the engine sees the path.
///
/// The order is load-bearing and is the module doc's *the modes are set, never inherited*: directory
/// (0700), then an empty file (0600), then the engine — so no row has ever existed under the umask's
/// answer. Returns which file this call made, `None` when one was already there (a sibling's, joined
/// and never clobbered), so a cleanup can remove the one it made and nothing else ([`create_store`]).
pub(super) fn make_database_file(path: &Path) -> Result<Option<FileIdentity>, StoreError> {
    if let Some(dir) = path.parent() {
        create_dir_private(dir).map_err(|e| StoreError::io(path, e))?;
    }
    create_file_private(path).map_err(|e| StoreError::io(path, e))
}

/// The creation's engine open, over the file [`make_database_file`] made (`made`: the schema is laid
/// down here) or found (an existing store, opened and checked as every writer's is). The engine is
/// never asked to create a file: one that vanished between the two halves is
/// [`StoreErrorKind::VanishedDatabase`].
///
/// # ⚠ It does NOT stamp [`SCHEMA_VERSION`], and that is the whole of the crash story
///
/// `PRAGMA user_version` is written by the CALLER, **after** its own transaction commits
/// ([`stamp_schema_version`]). The stamp used to be written here, between the schema batch and the
/// caller's inserts, and every failure after that point — a full disk, a read-only remount, `SIGKILL`
/// — left a file that `database_present` calls a database, that `check_schema_version` accepts, and
/// that holds NO ROWS. `crate::store::StoreBackend` then answers `Database` for every process on the box
/// forever, which is not an error downstream but the LIVE GATE: every venue silently on paper.
///
/// With the stamp deferred, that same crash leaves `user_version = 0`, which
/// [`check_schema_version`] already treats as the loud [`StoreErrorKind::SchemaVersion`]. The unfinished
/// database announces itself instead of impersonating a finished one.
pub(super) fn open_made_for_write(path: &Path, made: bool) -> Result<Connection, StoreError> {
    let vanished =
        || StoreError { path: path.to_path_buf(), kind: StoreErrorKind::VanishedDatabase };
    if !made {
        return open_for_write(path)?.ok_or_else(vanished);
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| StoreError::sql(path, e))?;
    set_write_pragmas(path, &conn, BUSY_TIMEOUT)?;
    conn.execute_batch(crate::schema::DDL).map_err(|e| StoreError::sql(path, e))?;
    Ok(conn)
}

/// The pragmas every write connection sets before its first statement.
///
/// ⚠ `PRAGMA foreign_keys` is SET AND VERIFIED here, and it is not decoration: it is **OFF by
/// default, per connection**, so schema 2's `credential.account_id REFERENCES account(id)` and
/// `account.parent_id REFERENCES account(id)` would enforce NOTHING without it — the spec's *a
/// schema is a gate* claim would be false for both columns. Same set-then-believe-the-engine shape
/// as the journal mode, and for the same reason: a pragma that did not take is silent everywhere
/// else. It is set BEFORE any transaction because SQLite is a no-op for it inside one.
fn set_write_pragmas(
    path: &Path,
    conn: &Connection,
    busy: std::time::Duration,
) -> Result<(), StoreError> {
    // The pin, before the first statement — see [`BUSY_TIMEOUT`]. ⚠ Ahead of the journal-mode
    // pragma below on purpose: that pragma takes a RESERVED lock, so a migrator holding one is
    // enough to make this open fail before any of this function's own checks have run.
    conn.busy_timeout(busy).map_err(|e| StoreError::sql(path, e))?;

    // Set it, then BELIEVE THE ENGINE rather than the request. A journal mode that did not take is
    // the failure this pragma exists to prevent, and it is silent everywhere else.
    let answered: String = conn
        .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
        .map_err(|e| StoreError::sql(path, e))?;
    if !answered.eq_ignore_ascii_case("delete") {
        return Err(StoreError {
            path: path.to_path_buf(),
            kind: StoreErrorKind::JournalMode { answered },
        });
    }

    conn.execute_batch("PRAGMA foreign_keys = ON;").map_err(|e| StoreError::sql(path, e))?;
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .map_err(|e| StoreError::sql(path, e))?;
    if fk != 1 {
        return Err(StoreError { path: path.to_path_buf(), kind: StoreErrorKind::ForeignKeys });
    }
    Ok(())
}

pub(super) fn stamp_schema_version(path: &Path, conn: &Connection) -> Result<(), StoreError> {
    conn.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(|e| StoreError::sql(path, e))
}

/// Refuse a database unless it carries [`SCHEMA_VERSION`] — the one check, for every other
/// `user_version` alike.
pub(super) fn check_schema_version(path: &Path, conn: &Connection) -> Result<(), StoreError> {
    let found: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| StoreError::sql(path, e))?;
    if found == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(StoreError {
            path: path.to_path_buf(),
            kind: StoreErrorKind::SchemaVersion { found, expected: SCHEMA_VERSION },
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

/// **Which file a path names**, so the creation's cleanup removes the file it made and never one put
/// at the path since ([`create_store`]): the device and the inode on unix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileIdentity(u64, u64);

impl FileIdentity {
    #[cfg(unix)]
    pub(super) fn of(meta: &std::fs::Metadata) -> FileIdentity {
        use std::os::unix::fs::MetadataExt;
        FileIdentity(meta.dev(), meta.ino())
    }

    // shortcut: stable std has no file index on Windows, so the creation time stands in (a file moved
    // into place keeps its own); upgrade to `MetadataExt::file_index` once it is stable.
    #[cfg(not(unix))]
    pub(super) fn of(meta: &std::fs::Metadata) -> FileIdentity {
        use std::os::windows::fs::MetadataExt;
        FileIdentity(meta.creation_time(), 0)
    }
}

/// Create the database file EMPTY (0600 on unix), before the engine sees the path, and answer WHICH
/// file it made (`None`: one was already there).
///
/// A zero-length file is a valid empty SQLite database, so this costs nothing and closes the window
/// in which rows would exist at the umask's mode. `create_new` means a racing sibling that got there
/// first is joined, never clobbered. Windows has no mode bits (the equivalent question is an ACL
/// query this workspace has no crate for, the posture `crate::store::permission_warning` takes), so
/// the file is made there with the platform's own.
pub(super) fn create_file_private(path: &Path) -> std::io::Result<Option<FileIdentity>> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options.open(path) {
        Ok(file) => file.metadata().map(|meta| Some(FileIdentity::of(&meta))),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
        Err(e) => Err(e),
    }
}
