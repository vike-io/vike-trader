//! Tests of `open.rs` and error classification: journal arms, modes, version stamp, header probe.

use super::open::{check_schema_version, stamp_schema_version};
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
/// `crates/vike-secrets/tests/migration/database/refusals.rs`'s
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

#[test]
fn a_text_credential_file_is_not_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("secrets.env");
    std::fs::write(&p, "BINANCE_LIVE_API_KEY=abc\n").unwrap();
    assert!(!is_sqlite_file(&p));
    // ...while the coarse probe cannot tell them apart, which is the whole reason both exist.
    assert!(database_present(&p));
}

#[test]
fn an_absent_or_empty_path_is_not_a_database() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!is_sqlite_file(&dir.path().join("nothing-here")));
    let empty = dir.path().join("empty");
    std::fs::write(&empty, b"").unwrap();
    assert!(!is_sqlite_file(&empty));
    // Shorter than the header: a truncated write must not read as a database either.
    let stub = dir.path().join("stub");
    std::fs::write(&stub, b"SQLite").unwrap();
    assert!(!is_sqlite_file(&stub));
}

#[test]
fn a_real_database_is_recognised_by_its_own_header() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db").join(crate::DB_FILE);
    // Through the crate's own creator, so this asserts about the artifact `migrate` produces
    // rather than about a hand-planted 16 bytes.
    let conn = open_for_write(&db).unwrap().0;
    drop(conn);
    assert!(is_sqlite_file(&db));
}

/// ⚠ The failure this probe exists to prevent, driven end to end: a REAL database read as a
/// credential file. The assertion is not that the parse errors — it is that it SUCCEEDS and
/// yields nothing, which is the shape that reads as an empty store.
#[test]
fn a_database_read_as_text_is_silent_rather_than_loud() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db").join(crate::DB_FILE);
    drop(open_for_write(&db).unwrap().0);
    let bytes = std::fs::read(&db).unwrap();
    if let Ok(text) = String::from_utf8(bytes) {
        assert!(
            crate::parse_dotenv(&text).is_empty(),
            "a database parsed as KEY=VALUE yielded assignments; the refusal's premise moved"
        );
    }
    assert!(is_sqlite_file(&db), "and this is what stops it");
}
