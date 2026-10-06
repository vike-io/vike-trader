//! The dry run: `PlannedOutcome`, `MigrationPlan`, `preview` and the in-memory `preview_rows`.

use super::*;

// ---------------------------------------------------------------------------------------------
// The dry run
// ---------------------------------------------------------------------------------------------

/// What a migration WOULD do. The conditional mood of [`MigrationOutcome`], and a separate type
/// because the indicative one would lie here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedOutcome {
    /// No database exists and there are rows to write, so a run would create one.
    WouldCreate,
    /// A database exists and a run would add rows to it.
    WouldAdd,
    /// **A database exists at an OLDER schema and a run would UPGRADE it in place**, with or
    /// without new rows. The conditional twin of [`MigrationOutcome::SchemaUpgraded`], and the one
    /// an operator most wants to see before running the verb: the upgrade is a ONE-WAY DOOR for
    /// every binary older than this one.
    WouldUpgradeSchema,
    /// A database exists and already holds every key the files carry. A run would open no write
    /// connection at all.
    ///
    /// ⚠ Asserts COMPLETENESS, so it may only be chosen when [`MigrationPlan::refused`] is empty —
    /// see [`Self::NothingNewButRefused`].
    AlreadyComplete,
    /// A database exists, nothing new is pending, and a key WOULD BE refused. The twin of
    /// [`MigrationOutcome::NothingNewButRefused`], and it matters more on this side: a preview is
    /// read to decide whether to run the thing at all.
    NothingNewButRefused,
    /// **Nothing to migrate, and NO DATABASE WOULD BE CREATED.** Neither file carries a key and none
    /// exists. See [`MigrationOutcome::NothingToMigrate`] for why creating one is the harmful act.
    NothingToMigrate,
}

impl std::fmt::Display for PlannedOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PlannedOutcome::WouldCreate => "would be CREATED",
            PlannedOutcome::WouldAdd => "already exists; rows would be ADDED",
            PlannedOutcome::WouldUpgradeSchema => {
                "already exists at an OLDER schema; it would be UPGRADED in place"
            }
            PlannedOutcome::AlreadyComplete => "already exists and already holds every key",
            PlannedOutcome::NothingNewButRefused => {
                "already exists; nothing new to carry, and a key would be REFUSED"
            }
            PlannedOutcome::NothingToMigrate => {
                "would NOT be created — there is nothing to migrate"
            }
        })
    }
}

/// **What [`preview`] found: the same report [`Migration`] carries, in the conditional mood.**
///
/// ⚠ **It is deliberately NOT a [`Migration`]**, and the distinction is the whole reason this type
/// exists rather than a `dry_run: bool` on the other one. [`Migration::database_exists`] is
/// documented as *"as a result of this run having SUCCEEDED"* and [`MigrationOutcome`]'s `Display`
/// says `created`; a preview that returned one would hand its caller a value asserting a database
/// exists when none does, and the caller most likely to be misled is a CLI printing the report
/// straight through. So the report shapes match field for field — which is what makes them
/// comparable — and the two vocabularies do not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlan {
    /// The database this run would be about. It may not exist, and on
    /// [`PlannedOutcome::NothingToMigrate`] it deliberately would not be made to.
    pub db: PathBuf,
    /// What a run would do. See [`PlannedOutcome`].
    pub outcome: PlannedOutcome,
    /// The schema the database carries today, or `None` when there is none.
    pub schema_before: Option<i64>,
    /// The schema a run would leave it at.
    pub schema_now: i64,
    /// One row per (file, table) pair that could contribute. ⚠ `SourceReport::inserted` reads as
    /// *would be inserted* here — the same type, the same numbers, the conditional mood.
    pub sources: Vec<SourceReport>,
    /// See [`Migration::doubly_claimed`].
    pub doubly_claimed: Vec<String>,
    /// See [`Migration::refused`] — the keys a run would refuse WITHOUT refusing the run.
    ///
    /// ⚠ Non-empty here proves a database already exists (the per-key refusal compares against a
    /// STORED row), so this list is always empty on the one run that is irreversible.
    pub refused: Vec<Ambiguity>,
    /// See [`Migration::inserted_keys`] — the names a run WOULD insert, in the conditional mood.
    ///
    /// Carried here rather than left out because it is the one field an operator can check by eye
    /// against their own file before an irreversible act, and because a preview that reported fewer
    /// fields than the apply is a preview nobody can compare to the apply.
    pub inserted_keys: Vec<String>,
    /// **What the schema-2 fill WOULD do** — the conditional twin of [`Migration::rows`], produced
    /// by running the real classifier over an in-memory REPLICA of this store. See [`preview_rows`]
    /// for how the replica is built and for the measurement that made this field necessary: without
    /// it the preview reported *would be upgraded, every key name would still answer* immediately
    /// before an apply that failed.
    ///
    /// `None` only when there is nothing to do at all ([`PlannedOutcome::NothingToMigrate`]).
    pub rows: Option<crate::schema::RowReport>,
}

impl MigrationPlan {
    /// Rows a run would insert. `0` means a run would write nothing.
    #[must_use]
    pub fn would_insert(&self) -> usize {
        self.sources.iter().map(|s| s.inserted).sum()
    }

    /// Would a run CREATE the database? The question the first run's irreversibility hangs on.
    #[must_use]
    pub fn would_create(&self) -> bool {
        matches!(self.outcome, PlannedOutcome::WouldCreate)
    }

    /// Total names read out of the files. Named like [`Migration::keys_read`] and for the same
    /// reason — see that method.
    #[must_use]
    pub fn keys_read(&self) -> usize {
        self.sources.iter().map(|s| s.read).sum()
    }
}

impl std::fmt::Display for MigrationPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "settings database {} ({})", self.db.display(), self.outcome)?;
        if self.outcome == PlannedOutcome::WouldUpgradeSchema {
            write!(
                f,
                "\n  schema {} -> {}: the account becomes a ROW. Every key name would still \
                 answer exactly as it does today, and the credential file would be READ ONLY.\n  \
                 ⚠ ONE-WAY: after it, a binary older than this one refuses this store — which \
                 downstream is an EMPTY credential map and therefore every venue on paper. Roll \
                 the BINARY back only together with this store.",
                self.schema_before.unwrap_or(0),
                self.schema_now,
            )?;
        }
        if let Some(rows) = &self.rows
            && !rows.is_quiet()
        {
            // ⚠ Printed only when it has something to SAY. The counts on a `RowReport` are
            // indicative ("3 account row(s) written"), and a preview that stated them as fact would
            // be the second mood problem this type exists to avoid — see this type's own doc. Its
            // FINDINGS are mood-free: a refused key is refused, a name the classifier cannot place
            // cannot be placed, and an alias is an alias.
            write!(f, "\n  what the classifier finds in this store:\n  {rows}")?;
        }
        for s in &self.sources {
            write!(
                f,
                "\n  {} -> {}: {} key(s) read, {} would be inserted, {} already present",
                s.file.display(),
                s.table,
                s.read,
                s.inserted,
                s.already_present
            )?;
        }
        if !self.refused.is_empty() {
            write!(
                f,
                "\n  ⚠ {} key(s) would be REFUSED and nothing would be overwritten — every other \
                 key above would land:",
                self.refused.len()
            )?;
            for a in &self.refused {
                write!(f, "\n    - {a}")?;
            }
        }
        if !self.doubly_claimed.is_empty() {
            write!(
                f,
                "\n  {} name(s) are in BOTH source files with an identical value and are counted \
                 under the node file's row above, so the credential file holds that many more \
                 `KEY=` lines than its row reports: {}",
                self.doubly_claimed.len(),
                self.doubly_claimed.join(", ")
            )?;
        }
        f.write_str("\n  NOTHING WAS WRITTEN — this is a plan, not a run")
    }
}

/// **What [`migrate`] would do, without doing any of it.** Reads; creates nothing; writes nothing.
///
/// Same two parameters as [`migrate`] and the same meaning — pass the SAME `is_node_key` predicate,
/// because the classification is the thing being previewed.
///
/// # Why a real dry run, and not one of the three that look cheaper
///
/// A migration's FIRST successful run is irreversible in practice: once the database exists,
/// `crate::store::backend_at` answers `Database` for every process on the box, `secrets.env` stops
/// being read, and the `is_node_key` classification is baked into two tables that
/// [`Ambiguity::WrongTable`] then refuses to re-decide. There is no repair verb. Three cheaper
/// previews were considered and all three are worse than the act they preview:
///
/// * **Open for write and roll back.** [`open_for_write`] creates the directory, the file and the
///   schema and leaves `user_version` at 0 until [`stamp_schema_version`] runs — so an aborted
///   transaction leaves precisely the one state this module has no resume path for, the state
///   [`DbErrorKind::SchemaVersion`]'s zero arm exists to explain. A preview that can strand a box is
///   not a preview.
/// * **Migrate a COPY in a temp directory.** It answers the right question and produces a SECOND
///   PLAINTEXT COPY of every live venue key, created under the umask — MEASURED as 0664 on the live
///   box (0054's *What must land*), i.e. group- and world-readable — in a directory this code does
///   not own and cannot promise to remove on a `SIGKILL`. The whole modes section of this module
///   exists to stop that happening once; doing it deliberately for a preview is worse.
/// * **Just run it, it is idempotent.** The second run is safe precisely BECAUSE the first already
///   happened. Idempotence says nothing about the act that creates the store, which is the act the
///   operator wants to see the shape of first.
///
/// This function is none of them: it is [`plan`], which opens the database READ-ONLY when one
/// exists and opens nothing at all when one does not, plus [`preview_rows`], which runs the REAL
/// classifier over an in-memory replica and opens nothing at all either.
///
/// # ⚠ The classifier RUNS here, and it did not
///
/// [`plan`] alone predicts the decisions made BEFORE a row is written. It cannot see a single one
/// of `crate::schema::write_rows`' own refusals, and the upgrade's failures live there — measured
/// end to end by a reviewer on a store holding both `ASTER_LIVE_API_KEY` and
/// `ASTER_MAINNET_API_KEY`, where this function printed *would be UPGRADED* and *every key name
/// would still answer exactly as it does today* and the apply that followed it failed on a unique
/// index. [`MigrationPlan::rows`] is that half, and [`preview_rows`] argues the replica.
///
/// # ⚠ What a dry run still cannot predict
///
/// It reports the DECISIONS, which are the whole of what is interesting, and it cannot promise the
/// WRITE. Four things belong to step 4 alone and are invisible here:
///
/// * **[`DbErrorKind::Io`] from the create helpers** — `create_dir_private` and
///   `create_file_private` run only on the write path, so a `settings/db` that cannot be created (a
///   read-only mount, a directory owned by another user, a full disk) surfaces on the apply and
///   never on the preview;
/// * **[`DbErrorKind::JournalMode`]** — `PRAGMA journal_mode = DELETE` is set by [`open_for_write`],
///   and a preview never asks the engine for it;
/// * **an INSERT or a COMMIT failing** — a full disk, a lock held by another writer, a transaction
///   that cannot land;
/// * **staleness.** A plan is a photograph. Both files and the database can change between the
///   preview and the apply, and the apply re-plans from scratch rather than trusting this one — see
///   [`migrate`], whose `created` comes from the OPEN and not from the plan's probe.
///
/// A refusal, by contrast, IS predicted: [`MigrateError::Ambiguous`] is decided entirely in [`plan`]
/// and returns from both entry points identically.
pub fn preview(
    settings_dir: Option<&str>,
    is_node_key: impl Fn(&str) -> bool,
    classify: &dyn Fn(&str) -> crate::schema::Classification,
) -> Result<MigrationPlan, MigrateError> {
    let planned = plan(settings_dir, is_node_key)?;
    let must_upgrade = planned.version.is_some_and(|v| v != SCHEMA_VERSION);
    let outcome = if must_upgrade {
        // ⚠ The upgrade OUTRANKS every other conditional outcome, including *already holds every
        // key*: a store at an older schema is not complete in the sense the operator reads that
        // word, because every credential write to it is refused.
        PlannedOutcome::WouldUpgradeSchema
    } else if planned.pending.is_empty() {
        if planned.exists {
            if planned.refused.is_empty() {
                PlannedOutcome::AlreadyComplete
            } else {
                PlannedOutcome::NothingNewButRefused
            }
        } else {
            PlannedOutcome::NothingToMigrate
        }
    } else if planned.exists {
        PlannedOutcome::WouldAdd
    } else {
        PlannedOutcome::WouldCreate
    };
    let inserted_keys: Vec<String> = planned.pending.keys().map(|(_, n)| n.clone()).collect();
    // ⚠ Skipped on the ONE outcome where a run does nothing at all: with no database, no pending
    // row and no upgrade due, there is no fill to describe and an empty report would read as
    // "nothing found" rather than "nothing asked".
    let rows = if outcome == PlannedOutcome::NothingToMigrate {
        None
    } else {
        preview_rows(&planned, classify)
            .map_err(|e| DbError::sql(&planned.db, e))
            .map_err(MigrateError::from)?
    };
    Ok(MigrationPlan {
        db: planned.db,
        outcome,
        schema_before: planned.version,
        schema_now: SCHEMA_VERSION,
        sources: planned.sources,
        doubly_claimed: planned.doubly_claimed,
        refused: planned.refused,
        inserted_keys,
        rows,
    })
}

/// **Run the real classifier over a REPLICA of this store, in memory, and report what it did.**
///
/// The dry run's missing half. [`plan`] predicts the DECISIONS that are made before a row is
/// written — which table a name belongs to, which keys are ambiguous, what is already stored — and
/// those were the whole of what `--dry-run` reported. They are not the whole of what an upgrade
/// does: [`crate::schema::write_rows`] makes a second set of decisions per row (which account, and
/// what to do when two names resolve to one), and every one of its refusals was invisible to the
/// preview. A reviewer measured the consequence end to end on a store holding both
/// `ASTER_LIVE_API_KEY` and `ASTER_MAINNET_API_KEY`: the dry run said the store *would be upgraded*
/// and that *every key name would still answer exactly as it does today*, and the apply that
/// followed it failed. A preview that cannot see the one failure the reshape introduces is not a
/// preview.
///
/// # Why a REPLICA, and why in memory
///
/// [`preview`]'s own list of three cheaper previews rules out the two obvious shapes, and both
/// arguments still hold: opening the real database for writing creates the file and the schema
/// before any transaction can be rolled back, and copying it to a temp directory writes a second
/// PLAINTEXT copy of every live venue key under the umask. An in-memory database is neither. It has
/// no path, no mode and no umask; nothing is created, nothing is opened for writing, and the bytes
/// it holds are bytes this process already has — [`plan`] read the whole store into `stored` before
/// this function exists. `temp_store = MEMORY` is set so the engine cannot spill them to a file
/// under memory pressure, which is the one way an in-memory database can reach a disk.
///
/// # ⚠ What the replica is NOT
///
/// It is rebuilt from `(name, value)`, so the surrogate `account.id` and `credential.id` values it
/// derives are its own. Nothing in the report depends on them — [`crate::schema::RowReport`] names
/// accounts by `(venue, tier)` and keys by name — but a caller must not read a predicted id as the
/// id the apply will assign.
///
/// ⚠ **A refusal that fails the whole run comes back in the INDICATIVE mood**, and that is a
/// declared residual rather than an oversight: the message is
/// [`crate::schema::reshape_into`]'s own, so it says *NOTHING WAS WRITTEN and this store is still
/// at its old schema* where a preview would rather say *would not be*. Both sentences are TRUE of
/// a dry run — nothing was written, and the store is indeed still at its old schema — and the half
/// that matters, the KEY it could not carry, is named either way. Re-spelling it in the conditional
/// would mean a second message to keep in step with the one the apply prints, which is the defect
/// this whole function exists to remove.
fn preview_rows(
    planned: &Plan,
    classify: &dyn Fn(&str) -> crate::schema::Classification,
) -> rusqlite::Result<Option<crate::schema::RowReport>> {
    let mut mem = Connection::open_in_memory()?;
    // Never a spill file: an in-memory database's temporary B-trees would otherwise land in
    // `SQLITE_TMPDIR` under the umask, holding plaintext credentials. Same hazard `reshape_into`
    // refuses a `VACUUM` for.
    //
    // ⚠ Both pragmas are set as SQL and then VERIFIED, exactly as `open_for_write` sets and
    // verifies its own: a pragma that did not take is silent, and both of these are load-bearing —
    // one keeps plaintext off the disk and the other makes schema 2's `REFERENCES` clauses a gate
    // rather than a comment, which is what lets a preview predict a dangling reference.
    mem.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA foreign_keys = ON;")?;
    let temp_store: i64 = mem.query_row("PRAGMA temp_store", [], |r| r.get(0))?;
    let foreign_keys: i64 = mem.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if temp_store != 2 || foreign_keys != 1 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
            Some(format!(
                "the dry run's in-memory replica did not take its pragmas (temp_store={temp_store}, \
                 foreign_keys={foreign_keys}); refusing to classify credentials in it rather than \
                 risk spilling them to a temp file"
            )),
        ));
    }
    let stored_of = |table: Table| -> BTreeMap<String, String> {
        planned
            .stored
            .iter()
            .filter(|((t, _), _)| *t == table)
            .map(|((_, n), v)| (n.clone(), v.clone()))
            .collect()
    };

    let tx = mem.transaction()?;
    let version = planned.version.unwrap_or(SCHEMA_VERSION);
    if !planned.exists {
        // Nothing to replicate — the apply would create the tables and fill them from the files.
        tx.execute_batch(crate::schema::DDL)?;
    } else if version < SCHEMA_VERSION {
        // The shape the reshape will find. `SCHEMA_1` is the real DDL rather than an approximation
        // of it, for the reason `plant_schema_1` gives.
        tx.execute_batch(SCHEMA_1)?;
        for (table, rows) in [
            (Table::Credential, stored_of(Table::Credential)),
            (Table::NodeKey, stored_of(Table::NodeKey)),
        ] {
            let sql = format!("INSERT INTO {} (name, value) VALUES (?1, ?2)", table.sql_name());
            for (name, value) in &rows {
                tx.execute(&sql, (name, value))?;
            }
        }
    } else {
        // A store already at this schema. Its account rows are a pure function of its key NAMES and
        // this same classifier, so they are re-derived rather than copied — which is what lets this
        // work from `(name, value)` at all. The report of THAT fill is discarded: it describes work
        // the store has already had done to it.
        //
        // ⚠ The `DDL` batch AND the `venue` roster (`ensure_venue_rows` is both), because the
        // fill below MINTS account rows and `account.venue_id` is `NOT NULL` since the venue-links
        // flip: each takes its number from `venue` in the same statement, so on the empty roster
        // the batch alone leaves, the dry run of every already-current store refused with
        // `NOT NULL constraint failed: account.venue_id` (MEASURED).
        ensure_venue_rows(&tx)?;
        for (name, value) in &stored_of(Table::NodeKey) {
            tx.execute("INSERT INTO node_key (name, value) VALUES (?1, ?2)", (name, value))?;
        }
        crate::schema::write_rows(&tx, &stored_of(Table::Credential), &planned.comments, classify)?;
    }

    let report = fill_into(&tx, planned, classify, version, !planned.exists)?;
    // Nothing is committed and nothing could be: the database has no file. The rollback is stated
    // rather than left to the drop so that the intent is on the page.
    tx.rollback()?;
    Ok(report)
}

/// **Schema 1's two flat tables — the shape this code no longer WRITES and still READS.**
///
/// A test that planted its own approximation of the old DDL would be proving the reshape against a
/// table nobody ever shipped, so the real one is stated here once. `crate::schema::DDL` is the
/// shape every real create uses.
///
/// ⚠ It sat behind `test-support` until [`preview_rows`] needed it: a dry run over an unmigrated
/// store builds the schema-1 shape IN MEMORY so the real reshape can be run against it, and that is
/// production code. The feature still gates [`plant_schema_1`], which is the part that puts a
/// superseded schema on DISK and that nothing in a shipped binary may reach.
pub const SCHEMA_1: &str = "\
CREATE TABLE IF NOT EXISTS credential (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS node_key   (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
";
