//! The one-row writer, the whole-table replace and the test-support fixtures that plant rows.

use super::*;

// ---------------------------------------------------------------------------------------------
// The one-row writer (`docs/decisions/0086`)
// ---------------------------------------------------------------------------------------------

/// **One change [`write_setting_row_in`] can make — exactly one row, in exactly one table.**
///
/// The one shape a settings write takes: an ordinary `setting` row
/// (`policy.max_notional_per_order`, `config.tradehub_addr`, …). An enum of one variant so a
/// caller's `match` names the shape it handles. Turning a dotted key into this enum is
/// `vike-config`'s job (layer 20, which alone knows the key grammar); this crate only ever executes
/// the row change it is handed. An account's mode and its exposure ceiling are NOT settings: they
/// are columns of its `account` row, written by `crate::db::edit_account`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowChange {
    /// Upsert one `setting` row.
    Setting {
        /// One of [`SETTINGS_SECTIONS`].
        section: String,
        /// The key's path inside that section — no section prefix, as [`SettingRow::key`].
        key: String,
        /// One JSON scalar, as [`SettingRow::value`].
        value: String,
    },
}

/// What [`write_setting_row_in`] did, once its transaction committed.
#[derive(Debug, Clone, PartialEq)]
pub struct RowWritten {
    /// The row's own value before this write — `None` for a brand-new row.
    pub old_value: Option<String>,
    /// The full settings rows AFTER this write landed — the committed CANDIDATE, so a caller builds
    /// its `old -> new` report and its journal record from what actually happened rather than from
    /// the request.
    pub rows: StoredSettings,
}

/// Why [`write_setting_row_in`] refused. Every variant's `Display` is operator-facing.
#[derive(Debug)]
pub enum RowWriteError {
    /// No settings database on this box. Nothing was written and nothing was created.
    NoDatabase {
        /// Where a database would be.
        path: PathBuf,
    },
    /// **Another writer holds the store** and the busy budget ran out. NOTHING was written.
    Busy,
    /// The caller's validator refused: either the CURRENT store does not boot clean (a write must
    /// not re-bless an erased ceiling), or the CANDIDATE — the current rows plus this one change —
    /// does not boot clean, or would resolve a key other than the one named. The validator's own
    /// message says which.
    Rejected(String),
    /// The engine, mid-transaction. Every statement here runs inside one transaction, so a failure
    /// at any point rolls the whole write back — there is no partial state to repair.
    Sql(DbError),
}

impl std::fmt::Display for RowWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RowWriteError::NoDatabase { path } => {
                write!(f, "no settings database at {} — nothing was written", path.display())
            }
            RowWriteError::Busy => write!(
                f,
                "another process is writing the settings database — waited and gave up; NOTHING \
                 was written. Re-run the command."
            ),
            RowWriteError::Rejected(why) => write!(f, "{why}"),
            RowWriteError::Sql(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RowWriteError {}

impl From<DbError> for RowWriteError {
    fn from(e: DbError) -> Self {
        if matches!(e.kind, DbErrorKind::StoreBusy) {
            RowWriteError::Busy
        } else {
            RowWriteError::Sql(e)
        }
    }
}

/// **Change exactly ONE row, inside one `BEGIN IMMEDIATE` transaction** — the row-native writer
/// `docs/decisions/0086` designs: every settings write, from
/// `vike-cli config set` to the daemon's own control channel, lands through this one primitive.
///
/// `validate` is handed `(current, current's seal, candidate)` — the store's rows before this write,
/// the [`Adoption`] those rows were sealed under (`None` on a store that has never been sealed), and
/// the rows after this write — and answers whether the write may land. It lives here as a CLOSURE
/// rather than as code in this crate because only `vike-config` can resolve rows into a typed
/// `Settings` and fold them through `apply_rows`/`differing_keys`, and this crate must not
/// depend upward on that one (`vike-secrets` is layer 15, `vike-config` is layer 20). It is called
/// exactly once, with the transaction still open and before any row is touched, so a refusal — the
/// current store failing its own boot check, the candidate failing its, or the candidate resolving a
/// key other than the one named — leaves the database BYTE-IDENTICAL: nothing between the read and
/// the rollback can have moved it.
///
/// `busy` is the caller's own wait budget for the write lock — a GUI frame, a daemon connection
/// thread and an interactive CLI all wait different amounts for the same lock, exactly as
/// `vike_config::write`'s module doc argues for the settings FILE's own lock.
///
/// # What this does NOT do
///
/// It never DELETEs a row, and it touches exactly one row in `setting` — plus the adoption seal's
/// `setting_rows` count, RECOMPUTED from the same candidate rather than taken from the caller,
/// never a second independent write. A refusal, for any
/// reason including the engine's own [`RowWriteError::Sql`], rolls the whole transaction back.
///
/// # Errors
/// See [`RowWriteError`].
pub fn write_setting_row_in(
    settings_dir: &Path,
    change: RowChange,
    busy: std::time::Duration,
    validate: impl FnOnce(&StoredSettings, Option<&Adoption>, &StoredSettings) -> Result<(), String>,
) -> Result<RowWritten, RowWriteError> {
    let db = crate::store_locator::db_path_in(settings_dir);
    if !database_present(&db) {
        return Err(RowWriteError::NoDatabase { path: db });
    }
    let (mut conn, created) = crate::db::open_for_write_within(&db, busy)?;
    if created {
        // The file vanished between the probe above and the open — the same hazard [`write_settings`]
        // guards, and the same repair: never leave an EMPTY database behind for
        // `crate::store::Backend` to mistake for a real store.
        drop(conn);
        let _ = std::fs::remove_file(&db);
        return Err(RowWriteError::NoDatabase { path: db });
    }

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(&db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(&db, e))?;
    crate::db::ensure_venue_rows(&tx).map_err(|e| DbError::sql(&db, e))?;

    let current = read_rows(&tx, &db)?;
    // The seal AS IT STOOD before this write — over the SAME open, so the caller's "does the
    // current store boot clean" check cannot race a sibling write. `None` for a store that has
    // never been sealed, which is the ordinary pre-adoption state and not itself a refusal.
    let current_adoption =
        if table_exists(&db, &tx, "settings_adoption")? { read_adoption(&db, &tx)? } else { None };

    let RowChange::Setting { section, key, value } = &change;
    let mut candidate = current.clone();
    let existing = candidate.settings.iter_mut().find(|r| &r.section == section && &r.key == key);
    let old_value = match existing {
        Some(row) => {
            let old = row.value.clone();
            row.value = value.clone();
            Some(old)
        }
        None => {
            candidate.settings.push(SettingRow {
                section: section.clone(),
                key: key.clone(),
                value: value.clone(),
            });
            None
        }
    };
    let candidate = candidate.sorted();

    if let Err(why) = validate(&current, current_adoption.as_ref(), &candidate) {
        return Err(RowWriteError::Rejected(why));
    }

    if old_value.is_some() {
        tx.execute(
            "UPDATE setting SET value = ?1 WHERE section = ?2 AND key = ?3",
            rusqlite::params![value, section, key],
        )
    } else {
        tx.execute(
            "INSERT INTO setting (section, key, value) VALUES (?1, ?2, ?3)",
            rusqlite::params![section, key, value],
        )
    }
    .map_err(|e| DbError::sql(&db, e))?;

    // The seal moves by the SAME delta the write just made: created where none exists yet (every
    // store this primitive has ever written to before it existed), moved where one does.
    // ⚠ `venues_declared` and `arming_rows` are dead since decision 0119 (`crate::schema::DDL`'s
    // note): an existing store declares them `NOT NULL` without a default, so the INSERT fills them
    // with `0`, and the upsert never touches them, so a rollback to an older binary still finds the
    // counts it wrote itself.
    tx.execute(
        "INSERT INTO settings_adoption \
         (id, adopted_at, tool_version, files_present, venues_declared, setting_rows, arming_rows) \
         VALUES (1, datetime('now'), 'vike-secrets row-writer', '', 0, ?1, 0) \
         ON CONFLICT (id) DO UPDATE SET setting_rows = excluded.setting_rows",
        rusqlite::params![candidate.settings.len() as i64],
    )
    .map_err(|e| DbError::sql(&db, e))?;

    tx.commit().map_err(|e| DbError::sql(&db, e))?;
    Ok(RowWritten { old_value, rows: candidate })
}

/// **Plant a [`StoredSettings`] straight into the database beside `settings_dir`**, creating the
/// store first where none exists — the fixture every OTHER crate's row-writer test wants, behind
/// `test-support` so nothing outside a test can reach it.
///
/// ⚠ It goes through [`write_settings`] (the whole-table REPLACE), not through
/// [`write_setting_row_in`]: a fixture plants a whole table in one call, a state
/// [`write_setting_row_in`] reaches only one row at a time.
///
/// # Errors
/// The engine, or the filesystem.
#[cfg(feature = "test-support")]
pub fn plant_settings_rows(settings_dir: &Path, rows: &StoredSettings) -> Result<(), DbError> {
    let db = crate::store_locator::db_path_in(settings_dir);
    if !database_present(&db) {
        // ⚠ NOT `open_for_write` — that call's OWN doc says it deliberately does not stamp
        // `PRAGMA user_version` on the file it creates (the caller stamps it, after its own
        // transaction commits), and [`write_settings`] refuses a database it just found freshly
        // created as the "vanished between the probe and the open" hazard. So a fixture that opened
        // the file that way and then called `write_settings` would always hit
        // `DbErrorKind::SchemaVersion { found: 0, .. }` — MEASURED, the first version of this
        // function did exactly that. This is the same plant-a-finished-store sequence
        // `crate::settings::tests::planted` already uses successfully: create, set the pragma, run
        // the DDL, stamp the version — all before `write_settings` ever opens it.
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir).map_err(|e| DbError::io(&db, e))?;
        }
        let conn = rusqlite::Connection::open(&db).map_err(|e| DbError::sql(&db, e))?;
        conn.execute_batch("PRAGMA journal_mode = DELETE;").map_err(|e| DbError::sql(&db, e))?;
        conn.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(&db, e))?;
        conn.pragma_update(None, "user_version", crate::db::SCHEMA_VERSION)
            .map_err(|e| DbError::sql(&db, e))?;
    }
    write_settings(&db, rows).map(|_| ())
}

/// **Hold the settings database's write lock**, for a test proving [`write_setting_row_in`]'s
/// [`RowWriteError::Busy`] disposition. `BEGIN IMMEDIATE` on a second connection, released by
/// dropping the returned one — a rollback of a transaction that wrote nothing, so it leaves no trace.
///
/// # Panics
/// If the database cannot be opened or the lock cannot be taken — a test fixture, not a production
/// path, so it fails loudly rather than returning a `Result` nothing would check.
#[cfg(feature = "test-support")]
#[must_use]
pub fn hold_write_lock(settings_dir: &Path) -> rusqlite::Connection {
    let db = crate::store_locator::db_path_in(settings_dir);
    let conn = rusqlite::Connection::open(&db).expect("open the settings database");
    conn.execute_batch("BEGIN IMMEDIATE;").expect("take the write lock");
    conn
}

/// **Replace the settings rows in the database beside `settings_dir`.** The `_in` spelling, as
/// [`read_settings_in`].
pub fn write_settings_in(
    settings_dir: &Path,
    rows: &StoredSettings,
) -> Result<SettingsWritten, DbError> {
    write_settings(&crate::store_locator::db_path_in(settings_dir), rows)
}

/// **Replace the settings rows** — the whole `setting` table, in ONE transaction.
///
/// # Why a REPLACE rather than an upsert
///
/// The mirror's source is the four files, and a file that stops setting a key is a key that stops
/// being set. An upsert would leave the row behind, and a row nothing wrote is exactly the thing
/// 0057 forbids migrating: it reads as more authoritative than the file line it left. So a mirror
/// run makes the tables equal to the files or fails, and there is no third state — the transaction
/// is what buys that.
///
/// # ⚠ It refuses a project with no database
///
/// [`crate::DbErrorKind::NoSettingsDatabase`], and the reason is the live gate rather than tidiness:
/// see that variant. `vike-cli secrets init` is the one thing that may create the store.
///
/// The DDL batch runs inside the same transaction, `IF NOT EXISTS` throughout, so a store migrated
/// before these tables existed gains them here and a store born with them is untouched. No schema
/// version is stamped and none is read — again, see [`crate::schema::DDL`]'s note.
pub fn write_settings(db: &Path, rows: &StoredSettings) -> Result<SettingsWritten, DbError> {
    if !database_present(db) {
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }
    for row in &rows.settings {
        debug_assert!(
            section_is_known(&row.section),
            "a caller offered the unknown settings section {:?}; the DDL's CHECK refuses it too",
            row.section
        );
    }

    let (mut conn, created) = open_for_write(db)?;
    if created {
        // The file vanished between the probe above and the open, and this call has just re-created
        // it. Same hazard and same repair as `upsert_rows`' `VanishedDatabase`: an empty database
        // makes `Backend` answer `Database` for every process on the box forever.
        drop(conn);
        let _ = std::fs::remove_file(db);
        return Err(DbError { path: db.to_path_buf(), kind: DbErrorKind::NoSettingsDatabase });
    }

    let tables_created = !table_exists(db, &conn, "setting")?;

    let tx = conn.transaction().map_err(|e| DbError::sql(db, e))?;
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(db, e))?;
    // ⚠ This writer does not pass through `crate::db::fill_into`, so it runs the write funnel
    // itself — see `crate::db::ensure_venue_rows`'s own doc for why every writer does.
    crate::db::ensure_venue_rows(&tx).map_err(|e| DbError::sql(db, e))?;
    tx.execute("DELETE FROM setting", []).map_err(|e| DbError::sql(db, e))?;
    // ⚠⚠ **`venue_setting` IS DELIBERATELY NOT CLEARED HERE, AND ADDING IT WOULD DESTROY DATA.**
    // This function re-derives every row FROM THE CALLER, and venue settings have no file to be
    // derived from — that is the whole reason they are a table rather than a `config` key
    // ([`VenueSettingRow`] carries the argument). A `DELETE FROM venue_setting` beside the one
    // above would therefore not re-write those rows, it would simply remove them: one whole-table
    // write and a box loses its JForex server, its IBKR gateway and its polymarket egress,
    // silently, with the call reporting success.
    //
    // `the_mirror_does_not_touch_venue_settings` is the gate. It is a real risk rather than a
    // theoretical one: the other table this function knows about IS cleared, so the symmetry
    // argues for the deletion and only this note argues against it.
    {
        let mut stmt = tx
            .prepare("INSERT INTO setting (section, key, value) VALUES (?1, ?2, ?3)")
            .map_err(|e| DbError::sql(db, e))?;
        for row in &rows.settings {
            stmt.execute(rusqlite::params![row.section, row.key, row.value])
                .map_err(|e| DbError::sql(db, e))?;
        }
    }
    // ⚠ **The SEAL's count moves with the table, in the SAME transaction.** Without this line a
    // sanctioned whole-table write on a sealed box would trip its own erase detector at the next
    // boot, which would teach an operator that the detector is noise — and a detector people have
    // learned to work around is worse than none. `UPDATE` rather than upsert: a box with no seal
    // gets no row. The dead `arming_rows` column is left as it stands (`crate::schema::DDL`).
    tx.execute(
        "UPDATE settings_adoption SET setting_rows = ?1 WHERE id = 1",
        rusqlite::params![rows.settings.len() as i64],
    )
    .map_err(|e| DbError::sql(db, e))?;
    tx.commit().map_err(|e| DbError::sql(db, e))?;

    Ok(SettingsWritten { settings: rows.settings.len(), tables_created })
}
