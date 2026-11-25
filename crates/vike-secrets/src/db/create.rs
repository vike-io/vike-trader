//! Creating the settings database EMPTY: `create_store`, its dry run `preview_create_store`, and
//! the one probe both share.
//!
//! This is the only code in the workspace that may bring a settings database into existence, and
//! it only ever creates an EMPTY one (who may call it: [`create_store`]'s doc): the current schema,
//! stamped, with no row. It converts nothing: a database that already exists is left exactly as it
//! is, and one at any other schema is refused like every reader refuses it
//! (`docs/decisions/0117-there-are-no-migrations.md`).

use super::open::{FileIdentity, make_database_file, open_made_for_write, stamp_schema_version};
use super::*;

/// What [`create_store`] did. Its `Display` is the operator's report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreCreation {
    /// The database this run was about. It exists after every successful run.
    pub db: PathBuf,
    /// `true` when this run CREATED the database (EMPTY); `false` when one already existed at the
    /// current schema, in which case nothing was written and no connection was opened for writing.
    pub created: bool,
}

impl std::fmt::Display for StoreCreation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = if self.created {
            "created EMPTY, as asked"
        } else {
            "already exists, nothing written"
        };
        write!(f, "settings database {} ({what})", self.db.display())
    }
}

/// **What [`preview_create_store`] found: the same report [`StoreCreation`] carries, in the
/// conditional mood.**
///
/// ⚠ **It is deliberately NOT a [`StoreCreation`]**: a preview that returned one would hand its
/// caller a value whose `Display` says `created` when nothing was, and the caller most likely to be
/// misled is a CLI printing the report straight through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreCreationPlan {
    /// The database a run would be about. It may not exist.
    pub db: PathBuf,
    /// `true` when no database exists, so a run would CREATE it (EMPTY).
    pub would_create: bool,
}

impl std::fmt::Display for StoreCreationPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = if self.would_create {
            "would be CREATED EMPTY, as asked"
        } else {
            "already exists, nothing would be written"
        };
        write!(f, "settings database {} ({what})", self.db.display())?;
        f.write_str("\n  NOTHING WAS WRITTEN — this is a plan, not a run")
    }
}

/// **The probe — everything [`create_store`] decides except the write.** Where the database is,
/// and whether one is there.
///
/// It exists so that [`preview_create_store`] is the SAME code path rather than a second
/// implementation of the same probe: `db_tests::the_two_entry_points_share_one_probe` pins that
/// both call it.
///
/// **It opens NOTHING for writing.** `open_for_read` carries no `SQLITE_OPEN_CREATE`, so the whole
/// of this function is reachable on a box with no database and leaves it with no database. The
/// read-only open is also the version check: a database at any schema but [`SCHEMA_VERSION`] is
/// refused here, before either entry point decides anything.
fn probe(settings_dir: Option<&str>) -> Result<(PathBuf, bool), StoreError> {
    let db = crate::store_locator::workspace_db_path_from(settings_dir);
    let exists = database_present(&db)?;
    if exists {
        drop(open_for_read(&db)?);
    }
    Ok((db, exists))
}

/// The write half of [`create_store`] — open, fill, stamp, commit — in its own function so the
/// connection is DROPPED before its caller can unlink the file: on Windows an open handle refuses
/// the removal, so a cleanup written inline would silently leave exactly the database it was added
/// to remove.
///
/// `created` is whether THIS run made the file ([`make_database_file`]'s answer), not what the probe
/// predicted: a sibling process may have created it in between, and then this run lays no schema
/// down and stamps nothing — which is also exactly what a preview cannot promise, see
/// [`preview_create_store`].
///
/// # ⚠ The stamp rides the fill's transaction
///
/// [`open_made_for_write`] applies the DDL outside the transaction and does not stamp; the stamp is
/// written INSIDE it, and MEASURED in `db_tests::the_schema_stamp_and_the_ddl_are_both_transactional`:
/// `PRAGMA user_version` is rolled back with the transaction that set it. So a kill between the
/// schema batch and the commit leaves `user_version = 0` — the loud state [`check_schema_version`]
/// explains — and never a stamped store that holds nothing. The transaction is `IMMEDIATE`: it reads
/// (the roster top-up) before it writes, and only a lock taken at `BEGIN` waits out the busy timeout.
fn write_empty_store(db: &Path, created: bool) -> Result<(), StoreError> {
    let mut conn = open_made_for_write(db, created)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| StoreError::sql(db, e))?;

    fill_into(&tx).map_err(|e| StoreError::sql(db, e))?;

    // ⚠ INSIDE the transaction — see this function's doc. On a create the version is still withheld
    // by `open_made_for_write`, so the unfinished-store story is unchanged.
    if created {
        // ONE spelling of the stamp, reached through the transaction — `Transaction` derefs to
        // `Connection`, so this is the same call `db_tests` makes and there is no second way to
        // declare a database finished.
        stamp_schema_version(db, &tx)?;
    }
    tx.commit().map_err(|e| StoreError::sql(db, e))
}

/// **Everything a create WRITES into its open transaction** — the venue roster (and the columns
/// [`ensure_venue_rows`] adds to a store older than them) — and the one place it is spelled.
///
/// The STAMP and the COMMIT stay with [`write_empty_store`].
fn fill_into(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    // On a fresh create `venue` already exists (`open_made_for_write` ran the `DDL`), and
    // this fills its roster — see [`ensure_venue_rows`]'s own doc for why it is unconditional.
    ensure_venue_rows(tx)
}

/// **Create the settings database EMPTY** (`vike-cli secrets init`). Idempotent, non-destructive,
/// and refusing rather than half-finishing.
///
/// # Who may call it, and why
///
/// Two operator acts, and nothing unattended: `vike-cli secrets init`, and the desktop's FIRST
/// connect on a PC that has no settings database (`crates/vike-app-core/src/backend/server_link.rs`'s
/// `ensure_store`). The second is the owner's one-download rule of 2026-11-16 (*an end user downloads
/// ONE file*, `vike-desktop`, and it connects by itself): a desktop user has no `vike-cli` to run
/// `secrets init` with. Both reach THIS function, so both make the same store (no account, no key;
/// the connect then adds this PC's device key), and both keep its guarantees: a store that exists is
/// never written, and one that exists and cannot be read is refused, never made again.
///
/// `settings_dir` is [`crate::SETTINGS_DIR_ENV`]'s value — the same PARAMETER every other resolver
/// in this crate takes, so this performs no environment read and resolves the database through the
/// ONE walk (`crate::store_locator`'s `workspace_db_path_from`).
///
/// # What it does
///
/// * **No database** — creates it: the current schema, stamped, with ZERO credential rows, ZERO
///   hub-key rows and ZERO account rows (`created = true`). From then on
///   `crate::store::backend_at` answers `Database` for every process on the box.
/// * **A database at the current schema** — nothing (`created = false`), and no connection is
///   opened for writing at all, which is how *twice is the same as once* is structural here rather
///   than hoped for. It can never re-create, truncate or rewrite an existing store.
/// * **A database at any other schema** — refused, [`StoreErrorKind::SchemaVersion`], exactly as every
///   reader refuses it.
///
/// ⚠ **The probe is [`probe`], shared with [`preview_create_store`]** — this function is that probe
/// plus the write, so a dry run cannot describe a run different from the one that follows it.
///
/// # Errors
/// The engine, the create helpers' I/O, or a store at any schema but [`SCHEMA_VERSION`].
pub fn create_store(settings_dir: Option<&str>) -> Result<StoreCreation, StoreError> {
    let (db, exists) = probe(settings_dir)?;
    if exists {
        return Ok(StoreCreation { db, created: false });
    }

    // ⚠ **A RUN THAT FAILS LEAVES NO DATABASE BEHIND, and that is a DIFFERENT case from the
    // unfinished one this module already argues.** Draw the line before reading further:
    //
    // * **The process is KILLED** between the schema batch and the commit — nothing can prevent
    //   that, the file sits at `user_version = 0`, and every fallible reader refuses LOUDLY with the
    //   recipe ([`check_schema_version`]). Unchanged.
    // * **This function RETURNS `Err`** — a path we control. Leaving the file then converts a box
    //   into one whose store is unreadable until a human deletes a file by hand, in exchange for
    //   nothing: re-running is the whole repair.
    //
    // ⚠ The cleanup removes only THE FILE THIS RUN MADE, by identity ([`FileIdentity`]), never the
    // path: a store an operator moved into place meanwhile (a restore) is another file and stays.
    // The unlink is `let _ =` deliberately: a cleanup may not replace the operator's real error
    // with a worse one.
    let made = make_database_file(&db)?;
    match write_empty_store(&db, made.is_some()) {
        // `false`: a sibling process created the database between the probe and this run's create
        // (see `write_empty_store`'s note on `created`): this run added nothing to it.
        Ok(()) => Ok(StoreCreation { db, created: made.is_some() }),
        Err(e) => {
            if let Some(made) = made {
                remove_if_still(&db, made);
            }
            Err(e)
        }
    }
}

/// Remove `db` when the file there is still `made`, the one this run created — never a file put at
/// the path since.
pub(super) fn remove_if_still(db: &Path, made: FileIdentity) {
    if std::fs::metadata(db).is_ok_and(|meta| FileIdentity::of(&meta) == made) {
        let _ = std::fs::remove_file(db);
    }
}

/// **What [`create_store`] would do, without doing any of it.** Reads; creates nothing; writes
/// nothing.
///
/// # Why a real dry run, and not one of the three that look cheaper
///
/// Creating the store is irreversible in practice: once the database exists,
/// `crate::store::backend_at` answers `Database` for every process on the box. Three cheaper
/// previews were considered and all three are worse than the act they preview:
///
/// * **Open for write and roll back.** The creation open creates the directory, the file and the
///   schema and leaves `user_version` at 0 until [`stamp_schema_version`] runs — so an aborted
///   transaction leaves precisely the state [`StoreErrorKind::SchemaVersion`]'s zero arm exists to
///   explain. A preview that can strand a box is not a preview.
/// * **Create a COPY in a temp directory.** A second database created under the umask — MEASURED
///   as 0664 on the live box (0054's *What must land*) — in a directory this code does not own and
///   cannot promise to remove on a `SIGKILL`.
/// * **Just run it, it is idempotent.** The second run is safe precisely BECAUSE the first already
///   happened; idempotence says nothing about the act that creates the store.
///
/// This function is none of them: it is [`probe`], which opens the database READ-ONLY when one
/// exists and opens nothing at all when one does not.
///
/// # ⚠ What a dry run still cannot predict
///
/// It reports the DECISION, and it cannot promise the WRITE. Four things belong to the write alone:
///
/// * **[`StoreErrorKind::Io`] from the create helpers** — `create_dir_private` and
///   `create_file_private` run only on the write path, so a `settings/db` that cannot be created (a
///   read-only mount, a directory owned by another user, a full disk) surfaces on the apply and
///   never on the preview;
/// * **[`StoreErrorKind::JournalMode`]** — `PRAGMA journal_mode = DELETE` is set by the write open,
///   and a preview never asks the engine for it;
/// * **an INSERT or a COMMIT failing** — a full disk, a lock held by another writer;
/// * **staleness.** A plan is a photograph: the apply probes again rather than trusting this one.
///
/// # Errors
/// The engine, or a store at any schema but [`SCHEMA_VERSION`].
pub fn preview_create_store(settings_dir: Option<&str>) -> Result<StoreCreationPlan, StoreError> {
    let (db, exists) = probe(settings_dir)?;
    Ok(StoreCreationPlan { db, would_create: !exists })
}
