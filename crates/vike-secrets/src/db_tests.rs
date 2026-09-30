use super::*;

/// A `rusqlite::Error` carrying one extended code, built the way [`preview_rows`] builds its
/// own — the classifier's INPUT, so nothing here has to plant an on-disk state.
fn failure(extended: i32) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(extended), None)
}

/// **The whole finding, as a classification.** `SQLITE_READONLY_ROLLBACK` used to reach
/// [`DbErrorKind::Sqlite`], where the operator got the engine's six words — *attempt to write
/// a readonly database* — about a READ, on a store whose permissions are perfect, naming no
/// repair.
#[test]
fn a_surviving_rollback_journal_is_its_own_named_kind() {
    let e = DbError::sql(Path::new("/x/settings/db/vike.db"), failure(776));
    assert!(
        matches!(e.kind, DbErrorKind::ReadOnlyRollback),
        "SQLITE_READONLY_ROLLBACK must not fall through to the engine's own words: {:?}",
        e.kind
    );
    assert_eq!(
        rusqlite::ffi::SQLITE_READONLY_ROLLBACK,
        776,
        "the extended code this arm keys on moved under it"
    );
}

/// ⚠ **The other `SQLITE_READONLY_*` codes are NOT this arm**, and that is the half a match on
/// the primary `rusqlite::ErrorCode::ReadOnly` would have got wrong. A `chmod 400` store, a
/// read-only directory and a database moved out from under an open handle all raise
/// `ErrorCode::ReadOnly`, and telling any of those operators *a writer died mid-write* would be
/// this arm committing the defect it was added to fix.
#[test]
fn the_other_read_only_codes_are_not_a_dead_writer() {
    // SQLITE_READONLY (8), _RECOVERY (264), _CANTLOCK (520), _DBMOVED (1032).
    for code in [8, 264, 520, 1032] {
        let e = DbError::sql(Path::new("/x/settings/db/vike.db"), failure(code));
        assert!(
            matches!(e.kind, DbErrorKind::Sqlite(_)),
            "extended code {code} must stay unclassified, not borrow the rollback story: {:?}",
            e.kind
        );
    }
}

/// **The message must name the REPAIR, and say the daemon cannot perform it.**
///
/// Asserted as PROPERTIES rather than as a verbatim string: the wording is meant to be improved
/// again, and a `assert_eq!` on the whole sentence turns every improvement into a test edit
/// that says nothing about whether the sentence got better. What may not regress is what an
/// operator can DO with it.
#[test]
fn the_rollback_message_names_the_repair_and_who_must_perform_it() {
    let msg = DbError::sql(Path::new("/x/settings/db/vike.db"), failure(776)).to_string();
    for needle in [
        // the command, spelled so it can be pasted
        "vike-cli secrets",
        // ...from where, because the daemon's own namespace is the one place it does not work
        "OPERATOR SHELL",
        // ...and the thing an operator fears first, answered before anything else
        "NOTHING IS CORRUPT",
        // ...and that a restart is not the answer
        "CANNOT REPAIR THIS ITSELF",
        // ...and WHICH file is sitting there
        "-journal",
    ] {
        assert!(msg.contains(needle), "the repair is not reachable from the message: {msg}");
    }
    assert!(
        !msg.contains("could not be read"),
        "this arm replaces the opaque sentence rather than prefixing it — the outer \
             `SecretsError` already says the store could not be read once: {msg}"
    );
}

/// The busy arm is untouched by the widening — it is matched FIRST and on the primary code, so
/// a future extended code in the busy family cannot be stolen by the rollback branch.
#[test]
fn the_busy_arm_still_wins_its_own_codes() {
    // SQLITE_BUSY (5), SQLITE_BUSY_SNAPSHOT (517), SQLITE_LOCKED (6).
    for code in [5, 517, 6] {
        let e = DbError::sql(Path::new("/x/settings/db/vike.db"), failure(code));
        assert!(
            matches!(e.kind, DbErrorKind::StoreBusy),
            "extended code {code} must stay the named busy arm: {:?}",
            e.kind
        );
    }
}
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// The journal path SQLite uses in DELETE mode: the database's own path plus `-journal`.
#[cfg(unix)]
fn journal_of(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push("-journal");
    PathBuf::from(name)
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn the_rollback_journal_is_0600_while_a_transaction_is_in_flight() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");

    let (mut conn, created, _) = open_for_write(&db).expect("open");
    assert!(created);
    assert_eq!(mode_of(&db), 0o600, "the database itself must be 0600 — the precondition");

    let journal = journal_of(&db);
    assert!(!journal.exists(), "no journal before a transaction starts");

    // In flight: a transaction with a real row in it, NOT committed. This is the only window in
    // which the journal exists, so the `stat` has to happen here.
    let tx = conn.transaction().expect("begin");
    tx.execute(
        "INSERT INTO credential (name, value, field) VALUES (?1, ?2, 'API_KEY')",
        ("BINANCE_DEMO_API_KEY", "a-value-that-must-not-become-world-readable"),
    )
    .expect("insert");

    assert!(
        journal.exists(),
        "no rollback journal appeared — if SQLite stopped writing one this test measures \
             nothing and must be re-derived, not deleted"
    );
    // ⚠ THE MEASUREMENT. If this ever fails at 0644/0664, the fix is to PRE-CREATE the journal
    // path at 0600 before the transaction the way `create_file_private` pre-creates the database
    // — not to relax the assertion.
    let measured = mode_of(&journal);
    assert_eq!(
        measured, 0o600,
        "the rollback journal holds PAGES OF THE DATABASE — i.e. plaintext venue credentials — \
             and was created at {measured:o} rather than 0600"
    );

    tx.commit().expect("commit");
    stamp_schema_version(&db, &conn).expect("stamp");
    drop(conn);
    assert!(!journal.exists(), "and nothing is left at rest — DELETE mode's whole point");
}

/// **`open_for_write` leaves the version UNSTAMPED — only a committed caller stamps it.**
///
/// ⚠ This is the mutation-provable half of the crash story, and it exists because its
/// integration twin is not.
/// `crates/vike-secrets/tests/database_migration.rs`'s
/// `a_migration_interrupted_before_its_commit_is_a_loud_error_not_an_empty_map` PLANTS the state
/// a crash leaves and proves a reader is loud about it — a real and necessary property, and one
/// that stays GREEN if the stamp moves back into [`open_for_write`], because the planted state
/// is the same either way. An assertion that cannot fail for its stated reason is worse than no
/// assertion, so the ORDER gets its own test, here, where the two functions are visible.
///
/// Move `pragma_update(user_version)` back into [`open_for_write`] and this goes RED on its
/// first assertion. That is the whole of its job.
#[test]
fn open_for_write_leaves_the_version_unstamped_until_the_caller_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");

    let (conn, created, _) = open_for_write(&db).expect("open");
    assert!(created, "a fresh path must report that this call created it");
    let version = |c: &Connection| -> i64 {
        c.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("read user_version")
    };
    assert_eq!(
        version(&conn),
        0,
        "opening for write must NOT declare the database finished — a failure between here and \
             the caller's commit would leave a schema-stamped, ZERO-ROW store that \
             `check_schema_version` accepts and `Backend` then prefers forever, which is an empty \
             credential map and therefore the LIVE GATE"
    );
    // …and the schema IS there, so what the stamp withholds is the VERSION and nothing else.
    // (`field` joined this statement with schema 2 — it is `NOT NULL`, and a row without one
    // is exactly the misfiling `a_new_credential_name_without_a_classifier_is_refused…`
    // refuses.)
    conn.execute(
        "INSERT INTO credential (name, value, field) VALUES (?1, ?2, ?3)",
        ("BINANCE_DEMO_API_KEY", "a-value", "API_KEY"),
    )
    .expect("the tables must exist even though the version is unstamped");

    // The reader's verdict on that state, asked of the real function.
    let refused = check_schema_version(&db, &conn).expect_err("an unstamped store is refused");
    assert!(matches!(refused.kind, DbErrorKind::SchemaVersion { found: 0, .. }), "{refused}");

    stamp_schema_version(&db, &conn).expect("stamp");
    assert_eq!(version(&conn), SCHEMA_VERSION, "…and the stamp is what finishes it");
    check_schema_version(&db, &conn).expect("a stamped store reads");
}

/// **The version-0 refusal tells the operator HOW TO GET OUT, because nothing else will.**
///
/// ⚠ The state is unrecoverable and that was written down only in a doc comment. A process
/// killed between the migration's commit and [`stamp_schema_version`] leaves rows on disk at
/// `user_version = 0`; every fallible reader then refuses LOUDLY, which is the designed
/// behaviour AND a box whose credentials are unreadable until a human deletes a file by hand.
/// There is no resume, no repair verb and no `--force`, so the one message anybody actually
/// sees has to carry the recipe — `schema version 0, not 1` is a number and a dead end.
///
/// The second half is the part a widened message would break: a database at some OTHER version
/// is not this story (a future schema, or a file this project did not write), and telling its
/// operator to delete it would be advice to destroy a store a newer binary can read.
#[test]
fn the_unfinished_database_says_it_cannot_be_resumed_and_names_the_way_out() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (conn, _, _) = open_for_write(&db).expect("open");

    let refused = check_schema_version(&db, &conn).expect_err("an unstamped store is refused");
    let said = refused.to_string();
    assert!(
        said.contains(&db.display().to_string()),
        "the refusal must name the file to delete, not just describe one: {said}"
    );
    for needle in ["never finished", "delete", "again"] {
        assert!(
            said.contains(needle),
            "the version-0 refusal no longer says `{needle}`. This is the ONE place an operator \
                 is told that the half-written database cannot be resumed and what to do instead; \
                 a doc comment beside code they are not reading is not that place: {said}"
        );
    }
    assert!(
        said.contains("untouched"),
        "…and that their credential files were not touched, which is what makes deleting the \
             database a safe instruction rather than a frightening one: {said}"
    );

    // A DIFFERENT version is a different story and must not carry the same recipe.
    let newer = DbError {
        path: db.clone(),
        kind: DbErrorKind::SchemaVersion { found: SCHEMA_VERSION + 1, expected: SCHEMA_VERSION },
    };
    assert!(
        !newer.to_string().contains("delete"),
        "a database at a LATER schema version is not an interrupted migration — telling that \
             operator to delete it is telling them to destroy a store a newer binary reads: {newer}"
    );
}

/// The directory holding both is 0700, so even a journal that did NOT inherit 0600 would not be
/// reachable by another user. Stated as a SECOND, independent floor rather than as a reason to
/// skip the measurement above: a directory mode protects the path, not the file, and a backup
/// job or an operator `cp -r` carries the file's own mode onward.
#[cfg(unix)]
#[test]
fn the_directory_holding_the_journal_is_0700() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (conn, _, _) = open_for_write(&db).expect("open");
    drop(conn);
    assert_eq!(mode_of(db.parent().expect("parent")), 0o700);
}

/// **A write whose database VANISHED between the backend choice and the open creates nothing.**
///
/// The race [`upsert_rows`]' invariant section is about, reconstructed rather than performed —
/// the same technique, and for the same reason, as
/// `crates/vike-secrets/tests/database_migration.rs`'s
/// `a_migration_interrupted_before_its_commit_is_a_loud_error_not_an_empty_map`: the window
/// between `crate::store::backend_in`'s `is_file` and this open is a few instructions wide and
/// cannot be widened portably from a test. What a test CAN do is hand [`upsert_rows`] the exact
/// STATE that race produces — a database path with nothing at it — which is precisely the
/// argument `crate::store::save_credentials_to_store` passes after its probe saw a file.
///
/// It lives here rather than in `tests/` because [`upsert_rows`] is crate-private (nothing
/// outside this crate may choose a store), so no integration test can reach the state at all.
///
/// ⚠ **Restoring the old tail — `if created { stamp_schema_version(path, &conn)?; }` after the
/// commit, with this early return removed — makes this test fail on its FIRST assertion**, and
/// that is the whole of its value. What that code left behind was a schema-complete,
/// version-stamped database holding only this write's two keys, from which
/// `crate::store::backend_at` answers `Database` forever and every other credential on the box
/// is retired in silence.
#[test]
fn a_write_whose_database_vanished_creates_nothing_and_fails_loudly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    assert!(!db.exists(), "the precondition: the probe saw a file and it is gone now");

    let refused = upsert_rows(
        &db,
        Table::Credential,
        &[
            ("BINANCE_DEMO_API_KEY".to_string(), "a-key".to_string()),
            ("BINANCE_DEMO_API_SECRET".to_string(), "a-secret".to_string()),
        ],
        Some(&test_classify),
    )
    .expect_err("a vanished database must be refused, not re-created");

    assert!(
        !db.exists(),
        "A DATABASE WAS CREATED BY A WRITE THAT HELD TWO KEYS. `backend_at` answers `Database` \
             from here on and the credential file beside it is never read again — the other \
             sixty-odd keys are gone, silently, and every venue drops to paper."
    );
    assert!(
        matches!(refused.kind, DbErrorKind::VanishedDatabase),
        "the refusal must name the vanished database: {refused}"
    );
    let said = refused.to_string();
    assert!(said.contains("NOTHING WAS WRITTEN"), "{said}");

    // …and the rollback left no half-open artifact either: a `-journal` sidecar beside a
    // database that does not exist is a second way for a later reader to find something.
    let mut sidecar = db.as_os_str().to_os_string();
    sidecar.push("-journal");
    assert!(!PathBuf::from(sidecar).exists(), "a rollback journal survived the rollback");

    // The store this write was routed to is therefore still the FILES one, which is the
    // property the caller retries against.
    assert!(!database_present(&db), "`database_present` must still say no");
}

/// **An ORDINARY write — the database is there — is untouched by the refusal above.**
///
/// The other half of the same edit, and the one that would catch a fix that refused too much: a
/// `created` check placed where it also fired for an existing database would turn every write on
/// every migrated box into a hard failure, and the test above would still pass.
#[test]
fn an_ordinary_write_on_a_database_that_exists_still_lands() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");

    // Bring one into existence the ONLY way that is allowed to: a finished, stamped database.
    let (conn, created, _) = open_for_write(&db).expect("open");
    assert!(created);
    stamp_schema_version(&db, &conn).expect("stamp");
    drop(conn);

    // The FIRST write of a name this store has never held — the arm that needs a
    // classification, because schema 2's `field` is `NOT NULL`.
    upsert_rows(
        &db,
        Table::Credential,
        &[("OKX_DEMO_API_KEY".to_string(), "v1".to_string())],
        Some(&test_classify),
    )
    .expect("a write to a database that exists must land");
    // …and the UPDATE of a name it now holds, which needs NO classification at all: the row
    // already carries one. That is what keeps the venue's own rotation writer working.
    upsert_rows(
        &db,
        Table::Credential,
        &[("OKX_DEMO_API_KEY".to_string(), "v2".to_string())],
        None,
    )
    .expect("…and so must the update, with no classifier in sight");

    // ⚠ Read back WITHOUT a `map.get("LITERAL")`, deliberately. `crates/vike-ops/tests/
    // settings_registry.rs`'s `find_map_lookups` treats a literal inside a lookup call as a
    // RESOLVED READ of that environment variable — a bare literal in a test region is dropped
    // from its evidence, a lookup CALL SITE is not — so the tidy spelling demands a `SETTINGS`
    // row declaring `vike-secrets` reads `OKX_DEMO_API_KEY`, which it does not. The whole map
    // is a stronger assertion here anyway: it also pins that the update REPLACED rather than
    // inserted a second row.
    let rows = read_table(&db, Table::Credential).expect("read back").into_map();
    assert_eq!(rows.len(), 1, "the upsert must replace the row, not add one");
    let (name, value) = rows.into_iter().next().expect("one row");
    assert_eq!(name, "OKX_DEMO_API_KEY");
    assert_eq!(value, "v2", "the second write must win");
}

/// ⚠ **THE CRITICAL CASE: a store already at [`SCHEMA_VERSION`] that PREDATES the `venue`
/// table.** Not hypothetical — the root `CLAUDE.md` records both the CI box and the dev box migrating
/// to schema 2 on 2026-09-14, so both are in exactly this shape until a writer tops them up.
/// [`open_for_write`] only runs `DDL` when it CREATES the file, and [`fill_into`]'s reshape
/// branch only fires below `SCHEMA_VERSION` — neither path re-runs `DDL` for a store that is
/// already current, so without [`ensure_venue_rows`] re-running the whole batch itself, its
/// `INSERT OR IGNORE` hits a bare `no such table: venue` on the very next credential write.
///
/// Simulated by DROPPING the table a normal create already made, rather than by hand-writing an
/// older DDL: the failure mode under test is "this table is simply missing from an otherwise
/// current store," which is exactly what dropping it produces, with no risk of the fixture
/// silently drifting from the real historical schema-2 shape.
#[test]
fn ensure_venue_rows_creates_the_table_on_a_store_that_predates_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (mut conn, created, _) = open_for_write(&db).expect("open");
    assert!(created);
    conn.execute_batch("DROP TABLE venue;").expect("simulate a pre-venue-table schema-2 store");

    let tx = conn.transaction().expect("tx");
    ensure_venue_rows(&tx).expect("must self-heal rather than fail with `no such table: venue`");
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM venue", [], |r| r.get(0)).expect("count");
    assert_eq!(count as usize, vike_model::venues::VENUES.len());
}

/// **The top-up is idempotent WITHIN one transaction, independently of anything [`migrate`] does
/// around it.** [`migrate`]'s own doc says a run with nothing pending opens no write connection
/// at all, so an integration test that re-runs `migrate()` unchanged never reaches
/// [`ensure_venue_rows`] a second time and cannot tell `INSERT OR IGNORE` from a plain `INSERT`.
/// This calls the crate-private function directly, twice, over one transaction — which no
/// integration test in `tests/` can do.
#[test]
fn ensure_venue_rows_is_idempotent_within_one_transaction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (mut conn, created, _) = open_for_write(&db).expect("open");
    assert!(created);

    let tx = conn.transaction().expect("tx");
    ensure_venue_rows(&tx).expect("first top-up");
    ensure_venue_rows(&tx).expect("a second top-up must not duplicate a single row");
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM venue", [], |r| r.get(0)).expect("count");
    assert_eq!(count as usize, vike_model::venues::VENUES.len());
}

/// **Regression for the final-review fix wave's Finding 2 — `ensure_venue_id_columns` must SKIP
/// a `credential` table with no text `venue` column, rather than fail preparing its backfill's
/// sub-select with a bare `no such column: venue`.**
///
/// ⚠ **Built as a REBUILD rather than by planting a genuine schema-1 store, and the reason is a
/// SEPARATE, pre-existing defect this fix wave's own investigation found while trying to plant
/// one.** `crate::schema::DDL`'s `CREATE UNIQUE INDEX IF NOT EXISTS credential_one_live_value ON
/// credential (account_id, field) …` — part of the ORIGINAL 2026-09-14 schema-2 rollout, not
/// this branch — fails to prepare against a genuine schema-1 `credential`
/// (`name TEXT PRIMARY KEY, value TEXT`), because that table has neither `account_id` nor
/// `field`. Every non-`fill_into` writer runs the WHOLE `DDL` batch (their own call, which
/// predates this task) before this function is ever reached, so that crash fires first and this
/// function's guard is never exercised via any real write path against genuine schema 1 today —
/// `crates/vike-secrets/tests/database_migration.rs`'s
/// `write_settings_still_refuses_a_genuine_schema_1_store_for_an_unrelated_pre_existing_reason`
/// pins that CURRENT failure so it is not confused with this one. So this test isolates the ONE
/// precondition this function's guard actually checks — a `credential` table carrying every
/// OTHER schema-2 column (so the index above succeeds) but not `venue`/`venue_id` — a shape with
/// no history on this codebase's own timeline (they landed together on 2026-09-14) but the exact
/// boundary the guard is written against, reachable directly here because a unit test can call
/// this crate-private function without going through a writer's own preceding `DDL` call.
///
/// ⚠ **The planted `id` carries `AUTOINCREMENT`, and that word is load-bearing rather than
/// cosmetic.** [`ensure_venue_id_columns`] now calls
/// [`crate::schema::migrate_tables_onto_autoincrement`] BEFORE this loop, and that rebuild
/// re-creates a table from the shipped [`crate::schema::DDL`] — which carries `venue` and
/// `venue_id`. So an UNARMED plant would be normalized before the loop ever reached it, the
/// guard would have nothing to skip, and this test would be asserting something about a table
/// that no longer exists in the shape it planted. Arming the plant makes the rebuild skip it,
/// which is the only state in which the guard is still reachable. The rebuild's own composed
/// behaviour is the test below.
#[test]
fn ensure_venue_id_columns_skips_a_credential_table_with_no_venue_column() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (mut conn, created, _) = open_for_write(&db).expect("open");
    assert!(created);
    // Rebuild `credential` WITHOUT `venue`/`venue_id`, keeping every other schema-2 column —
    // `ALTER TABLE … DROP COLUMN venue` is refused outright (SQLite will not drop a column a
    // `CHECK` constraint references, and `credential`'s own `CHECK (account_id IS NULL OR venue
    // IS NULL)` does), so this rebuilds via rename-copy-drop, the same shape
    // `crate::schema::reshape_into` already uses for the real schema-1-to-2 case.
    conn.execute_batch(
        "ALTER TABLE credential RENAME TO credential_old;
             CREATE TABLE credential (
                 id            INTEGER PRIMARY KEY AUTOINCREMENT,
                 account_id    INTEGER REFERENCES account(id),
                 field         TEXT    NOT NULL,
                 value         TEXT    NOT NULL,
                 name          TEXT    NOT NULL,
                 secret        INTEGER NOT NULL DEFAULT 1,
                 superseded_at TEXT,
                 notes         TEXT,
                 CHECK (secret IN (0, 1))
             ) STRICT;
             INSERT INTO credential
                 (id, account_id, field, value, name, secret, superseded_at, notes)
                 SELECT id, account_id, field, value, name, secret, superseded_at, notes
                 FROM credential_old;
             DROP TABLE credential_old;",
    )
    .expect("simulate a credential table with no `venue`/`venue_id` at all");

    let tx = conn.transaction().expect("tx");
    ensure_venue_id_columns(&tx)
        .expect("must skip `credential` rather than fail preparing `no such column: venue`");
    assert!(
        crate::settings::has_column(&tx, "credential", "id").expect("check"),
        "the premise this test rests on: the plant is still there. If the rebuild replaced it, \
             every assertion below is about the shipped shape rather than the planted one"
    );
    assert!(
        !crate::settings::has_column(&tx, "credential", "venue").expect("check"),
        "…and it still has no text `venue`, which is what the guard under test keys on — an \
             ARMED plant is skipped by `migrate_tables_onto_autoincrement`, and this is that skip \
             asserted rather than assumed"
    );
    assert!(
        !crate::settings::has_column(&tx, "credential", "venue_id").expect("check"),
        "a table with no text `venue` must not gain `venue_id` either — nothing to shadow"
    );
}

/// **§4.1's rebuild NORMALIZES the column set, and that is what closes the shape the test
/// above plants** — stated positively rather than left as a side effect somebody meets.
///
/// [`crate::schema::migrate_tables_onto_autoincrement`] re-creates each table from the shipped
/// [`crate::schema::DDL`] rather than patching it, so a store whose `credential` is missing
/// `venue`/`venue_id` comes out of one write carrying both. The rebuild above the loop is
/// therefore a wider repair than the `ALTER TABLE` it precedes, and the loop's guard is left
/// governing only the tables the rebuild does not touch.
///
/// ⚠ This is the SAME normalization `crate::schema::migrate_sim_tier_to_paper` has performed on
/// `account` since §4.4 landed — `venue_id` and `armed` reach an old `account` through it
/// already. What stage 4 changed is only which tables it reaches.
#[test]
fn the_autoincrement_rebuild_normalizes_a_credential_table_that_lost_its_venue_columns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (mut conn, created, _) = open_for_write(&db).expect("open");
    assert!(created);
    // The same plant as the test above, UNARMED — the shape a store written before §4.1 is in.
    conn.execute_batch(
        "ALTER TABLE credential RENAME TO credential_old;
             CREATE TABLE credential (
                 id            INTEGER PRIMARY KEY,
                 account_id    INTEGER REFERENCES account(id),
                 field         TEXT    NOT NULL,
                 value         TEXT    NOT NULL,
                 name          TEXT    NOT NULL,
                 secret        INTEGER NOT NULL DEFAULT 1,
                 superseded_at TEXT,
                 notes         TEXT,
                 CHECK (secret IN (0, 1))
             ) STRICT;
             DROP TABLE credential_old;",
    )
    .expect("plant an unarmed credential table with no `venue`/`venue_id`");

    let tx = conn.transaction().expect("tx");
    ensure_venue_id_columns(&tx).expect("the rebuild must run rather than refuse");
    assert!(
        crate::settings::has_column(&tx, "credential", "venue").expect("check"),
        "the rebuild re-creates the table from the shipped DDL, so the columns the plant \
             lacked come back"
    );
    assert!(
        crate::settings::has_column(&tx, "credential", "venue_id").expect("check"),
        "…both of them"
    );
    let sql: String = tx
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'credential'",
            [],
            |r| r.get(0),
        )
        .expect("the rebuilt statement");
    assert!(
        sql.to_uppercase().contains("AUTOINCREMENT"),
        "…and the point of the rebuild is on it: {sql}"
    );
}

/// **[`migrate`] and [`preview`] reach their decision through ONE classifier, structurally.**
///
/// The property is not "the two agree on the fixtures we wrote" — that is the behavioural half,
/// and `crates/vike-secrets/tests/database_migration.rs`'s
/// `the_dry_run_predicts_exactly_what_the_apply_does` holds it. A behavioural test stays green
/// the day somebody adds a SECOND classifier that happens to agree on the cases those fixtures
/// cover, and the whole hazard here is a dry run that describes a migration different from the
/// one that follows it. So this asserts the shape: one definition, exactly two callers, and each
/// entry point reaching it rather than deciding for itself.
///
/// ⚠ **The needles are COMPOSED rather than spelled**, for the reason
/// `crates/vike-ops/tests/credential_writer_gate.rs`'s `writer_names` gives about its own
/// self-scan: this test reads THIS FILE, so a whole spelling here would be one more occurrence
/// of the very thing being counted, and the count is the assertion.
#[test]
fn the_two_entry_points_share_one_classifier() {
    const SELF: &str = include_str!("db.rs");
    let call = concat!("plan(settings_dir, ", "is_node_key)?");
    let define = concat!("fn ", "plan(");

    assert_eq!(
        SELF.matches(define).count(),
        1,
        "there must be exactly ONE classifier in this module — a second is the drift this \
             module is written against"
    );
    assert_eq!(
        SELF.matches(call).count(),
        2,
        "exactly two callers of it, `migrate` and `preview`, and nothing else"
    );
    for entry in ["pub fn migrate(", "pub fn preview("] {
        // The FIRST occurrence is the definition; the array above puts a second copy of each
        // string in this file, below it.
        let at = SELF.find(entry).unwrap_or_else(|| panic!("{entry} must exist"));
        let rest = &SELF[at..];
        let end = rest.find("\n}\n").unwrap_or(rest.len());
        assert!(
            rest[..end].contains(call),
            "{entry} must reach its decision through the shared classifier rather than \
                 carrying one of its own"
        );
    }
}

/// A classification for the handful of names these unit tests write. It is not the production
/// one and does not pretend to be — `vike_bridge_core::credentials::classify_credential_name`
/// is, and this crate cannot see it. What every test below needs is only that a name resolves
/// to SOME account deterministically.
fn test_classify(name: &str) -> crate::schema::Classification {
    use crate::schema::{AccountKey, Classification, Placement};
    // ⚠ The prefix is COMPOSED from two tokens rather than spelled whole, for the reason
    // `vike_bridge_core::credentials`' `HAND_MAPPED_ACCOUNTS` gives: an env-prefixed literal in
    // a `src/` file is read by `crates/vike-ops/tests/settings_registry.rs`' sweep as evidence
    // this file READS that variable, and a prefix is not a variable.
    for (head, tier, venue) in [("OKX", "DEMO", "okx"), ("BINANCE", "DEMO", "binance")] {
        if let Some(field) = name.strip_prefix(&format!("{head}_{tier}_")) {
            return Classification {
                placement: Placement::Account(AccountKey {
                    venue: venue.to_string(),
                    tier: "demo".to_string(),
                    label: None,
                    discriminator: None,
                }),
                field: field.to_string(),
                secret: true,
                recognised: true,
                pending_move: None,
            };
        }
    }
    Classification::unrecognised(name)
}

/// **MEASURED: `PRAGMA user_version` and DDL are BOTH rolled back with the transaction that
/// set them** — which is what lets the schema-1 → schema-2 reshape be ONE atomic step.
///
/// ⚠ This is a measurement of the ENGINE, pinned rather than assumed, and the design above it
/// is unshippable if it ever stops holding. `crate::schema::reshape_into`'s doc carries the
/// consequence: schema 2's `credential` still has `name` and `value` columns, so
/// `SELECT name, value FROM credential` — the exact statement a schema-1 binary runs — is still
/// valid SQL against the schema-2 shape. A reshape that could commit its tables and NOT its
/// version stamp would leave a store an older binary accepts and answers from, with the two
/// superseded rows folding onto their live twins' names. Atomicity is what removes that state.
///
/// If this ever fails, the fix is a different reshape (a sentinel version committed first, and
/// a message that does NOT tell the operator to delete a store holding keys the files do not) —
/// **not** a relaxed assertion.
#[test]
fn the_schema_stamp_and_the_ddl_are_both_transactional() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (mut conn, _, _) = open_for_write(&db).expect("open");
    stamp_schema_version(&db, &conn).expect("stamp");

    let tx = conn.transaction().expect("begin");
    tx.pragma_update(None, "user_version", 99i64).expect("set the version inside a transaction");
    tx.execute_batch("CREATE TABLE a_probe (a TEXT) STRICT;").expect("ddl inside it too");
    let inside: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("read");
    assert_eq!(inside, 99, "the precondition: the write took effect inside the transaction");
    tx.rollback().expect("rollback");

    let after: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).expect("read");
    assert_eq!(
        after, SCHEMA_VERSION,
        "`PRAGMA user_version` SURVIVED a rollback. The reshape's atomicity rests on it not \
             doing that: a committed set of schema-2 tables under a schema-1 stamp is a store an \
             older binary reads and answers from."
    );
    let probe: i64 = conn
        .query_row("SELECT count(*) FROM sqlite_master WHERE name = 'a_probe'", [], |r| r.get(0))
        .expect("count");
    assert_eq!(probe, 0, "DDL survived a rollback, so the table rebuild is not atomic either");
}

/// **`PRAGMA foreign_keys` is ON, so schema 2's two `REFERENCES` clauses are a gate.**
///
/// It is OFF by default and PER CONNECTION, so this is the difference between a schema that
/// refuses a credential naming an account row that does not exist and one that merely
/// describes refusing it. Asserted by attempting the dangling write, because asking the pragma
/// what it is set to would pass against a connection that had set it and an engine that had
/// ignored it.
#[test]
fn a_credential_naming_an_account_that_does_not_exist_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (conn, created, version) = open_for_write(&db).expect("open");
    assert!(created);
    assert_eq!(version, SCHEMA_VERSION, "a fresh store is born at the current schema");

    let refused = conn.execute(
        "INSERT INTO credential (account_id, field, value, name) VALUES (?1, ?2, ?3, ?4)",
        (9999i64, "API_KEY", "a-value", "OKX_DEMO_API_KEY"),
    );
    assert!(
        refused.is_err(),
        "a credential row naming account 9999 was ACCEPTED — `PRAGMA foreign_keys` did not \
             take, and schema 2's two REFERENCES clauses enforce nothing"
    );
}

/// **A NEW credential name with no classifier is REFUSED — never filed as the deployment's.**
///
/// The tempting fix for schema 2's `field NOT NULL` is `field = name`, `account_id` NULL,
/// `venue` NULL. That is not a fallback, it is a MISFILING: `(NULL, NULL)` is §5.1's
/// infrastructure classification, so a venue credential written that way is silently detached
/// from its account and from its venue, and nothing downstream can tell it apart from a
/// `CLOUDFLARE_API_TOKEN`.
#[test]
fn a_new_credential_name_without_a_classifier_is_refused_rather_than_misfiled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (conn, _, _) = open_for_write(&db).expect("open");
    stamp_schema_version(&db, &conn).expect("stamp");
    drop(conn);

    let refused = upsert_rows(
        &db,
        Table::Credential,
        &[("OKX_DEMO_API_KEY".to_string(), "v1".to_string())],
        None,
    )
    .expect_err("a name this store has never held needs a classification");
    assert!(matches!(refused.kind, DbErrorKind::Unclassified { .. }), "{refused}");
    assert!(refused.to_string().contains("OKX_DEMO_API_KEY"), "it must name the KEY");
    assert!(!refused.to_string().contains("v1"), "…and never its value: {refused}");

    let rows = read_table(&db, Table::Credential).expect("read back").into_map();
    assert!(rows.is_empty(), "the refusal must have written nothing");
}

/// **A write to a store at an OLDER schema is refused by name, and says how to fix it.**
///
/// The asymmetry this pins: [`READABLE_SCHEMA_VERSIONS`] keeps an unmigrated box READING, which
/// is what makes the version bump deployable on its own. Writing is a different act — there is
/// nowhere in schema 1's table to put the account classification a schema-2 row carries — so it
/// refuses, rather than half-supporting a shape and leaving the operator to find out later.
#[cfg(feature = "test-support")]
#[test]
fn a_new_key_written_to_a_schema_1_store_is_refused_and_names_the_way_out() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    plant_schema_1(&db, &[("BINANCE_DEMO_API_KEY".to_string(), "v1".to_string())], &[])
        .expect("plant");

    // The READ still works — that is the whole point of the dual-read.
    let rows = read_table(&db, Table::Credential).expect("a schema-1 store still reads");
    assert_eq!(rows.len(), 1);

    // …and a write of a name it already holds still works too, because no classification is
    // needed to replace a value.
    upsert_rows(
        &db,
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "v2".to_string())],
        Some(&test_classify),
    )
    .expect("replacing a known key needs no schema-2 column");

    let refused = upsert_rows(
        &db,
        Table::Credential,
        &[("OKX_DEMO_API_KEY".to_string(), "new".to_string())],
        Some(&test_classify),
    )
    .expect_err("a NEW name has nowhere to record its account in schema 1");
    assert!(matches!(refused.kind, DbErrorKind::WriteToOlderSchema { found: 1, .. }), "{refused}");
    let said = refused.to_string();
    assert!(said.contains("OKX_DEMO_API_KEY"), "it must name the KEY: {said}");
    assert!(!said.contains("new"), "…and never its value: {said}");
    assert!(said.contains("migrate"), "…and name the verb that fixes it: {said}");
    assert!(said.contains("NOTHING WAS WRITTEN"), "…and say that the store is unchanged: {said}");
}
