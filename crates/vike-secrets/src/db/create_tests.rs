//! Tests of the creator and the credential upsert: vanished database, ordinary write, not-held
//! refusal, one shared probe.

use super::open::stamp_schema_version;
use super::*;
use std::assert_matches;

/// **A write whose database VANISHED between the backend choice and the open creates nothing.**
///
/// The race [`upsert_rows`]' invariant section is about, reconstructed rather than performed —
/// the same technique, and for the same reason, as
/// `crates/vike-secrets/tests/store/database/refusals.rs`'s
/// `a_creation_interrupted_before_its_commit_is_a_loud_error_not_an_empty_map`: the window
/// between `crate::store::backend_in`'s `is_file` and this open is a few instructions wide and
/// cannot be widened portably from a test. What a test CAN do is hand [`upsert_rows`] the exact
/// STATE that race produces — a database path with nothing at it — which is precisely the
/// argument `crate::store::save_credentials_to_store_unjournalled` passes after its probe saw a file.
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
            ("ACME_DEMO_API_KEY".to_string(), "a-key".to_string()),
            ("ACME_DEMO_API_SECRET".to_string(), "a-secret".to_string()),
        ],
        UpsertIntent::Add,
    )
    .expect_err("a vanished database must be refused, not re-created");

    assert!(
        !db.exists(),
        "A DATABASE WAS CREATED BY A WRITE THAT HELD TWO KEYS. `backend_at` answers `Database` \
             from here on and holds only those two — every other key is gone, silently, and every \
             venue drops to paper."
    );
    assert_matches!(
        refused.kind,
        DbErrorKind::VanishedDatabase,
        "the refusal must name the vanished database: {refused}"
    );
    let said = refused.to_string();
    assert!(said.contains("NOTHING WAS WRITTEN"), "{said}");

    // …and the rollback left no half-open artifact either: a `-journal` sidecar beside a
    // database that does not exist is a second way for a later reader to find something.
    let mut sidecar = db.as_os_str().to_os_string();
    sidecar.push("-journal");
    assert!(!PathBuf::from(sidecar).exists(), "a rollback journal survived the rollback");

    // There is therefore still NO store, which is the property the caller retries against.
    assert!(!database_present(&db), "`database_present` must still say no");
}

/// **An ORDINARY write — the database is there — is untouched by the refusal above.**
///
/// The other half of the same edit, and the one that would catch a fix that refused too much: a
/// `created` check placed where it also fired for an existing database would turn every write on
/// every box into a hard failure, and the test above would still pass.
#[test]
fn an_ordinary_write_on_a_database_that_exists_still_lands() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");

    // Bring one into existence the ONLY way that is allowed to: a finished, stamped database.
    let (conn, created) = open_for_write(&db).expect("open");
    assert!(created);
    stamp_schema_version(&db, &conn).expect("stamp");
    drop(conn);

    // The FIRST write of a name this store has never held — the arm that inserts, filed as a
    // DEPLOYMENT row (a name-addressed write creates no account, decision 0156 item 6).
    upsert_rows(
        &db,
        Table::Credential,
        &[("ACME_DEMO_API_KEY".to_string(), "v1".to_string())],
        UpsertIntent::Add,
    )
    .expect("a write to a database that exists must land");
    // …and the UPDATE of a name it now holds, which a REPLACE may make: the row is already there.
    // That is what keeps a rotation writer working.
    upsert_rows(
        &db,
        Table::Credential,
        &[("ACME_DEMO_API_KEY".to_string(), "v2".to_string())],
        UpsertIntent::Replace,
    )
    .expect("…and so must the update of a name the table holds");

    // ⚠ Read back WITHOUT a `map.get("LITERAL")`, deliberately. `crates/vike-ops/tests/
    // settings_registry.rs`'s `find_map_lookups` treats a literal inside a lookup call as a
    // RESOLVED READ of that environment variable — a bare literal in a test region is dropped
    // from its evidence, a lookup CALL SITE is not — so the tidy spelling demands a `SETTINGS`
    // row declaring `vike-secrets` reads `ACME_DEMO_API_KEY`, which it does not. The whole map
    // is a stronger assertion here anyway: it also pins that the update REPLACED rather than
    // inserted a second row.
    let rows = read_table(&db, Table::Credential, crate::CredentialScope::Full)
        .expect("read back")
        .0
        .into_map();
    assert_eq!(rows.len(), 1, "the upsert must replace the row, not add one");
    let (name, value) = rows.into_iter().next().expect("one row");
    assert_eq!(name, "ACME_DEMO_API_KEY");
    assert_eq!(value, "v2", "the second write must win");
}

/// **[`create_store`] and [`preview_create_store`] reach their decision through ONE probe,
/// structurally.**
///
/// The property is not "the two agree on the fixtures we wrote" — that is the behavioural half,
/// and `crates/vike-secrets/tests/store/database/dry_run.rs` holds it. A behavioural test stays
/// green the day somebody adds a SECOND probe that happens to agree on the cases those fixtures
/// cover, and the whole hazard here is a dry run that describes a creation different from the
/// one that follows it. So this asserts the shape: one definition, exactly two callers, and each
/// entry point reaching it rather than deciding for itself.
///
/// It reads `create.rs` alone; the needles are composed anyway, so a copy of this test moved into
/// that file could not count itself.
#[test]
fn the_two_entry_points_share_one_probe() {
    const SELF: &str = include_str!("create.rs");
    let call = concat!("probe(", "settings_dir)?");
    let define = concat!("fn ", "probe(");

    assert_eq!(
        SELF.matches(define).count(),
        1,
        "there must be exactly ONE probe in this module — a second is the drift this module is \
             written against"
    );
    assert_eq!(
        SELF.matches(call).count(),
        2,
        "exactly two callers of it, `create_store` and `preview_create_store`, and nothing else"
    );
    for entry in ["pub fn create_store(", "pub fn preview_create_store("] {
        let at = SELF.find(entry).unwrap_or_else(|| panic!("{entry} must exist"));
        let rest = &SELF[at..];
        let end = rest.find("\n}\n").unwrap_or(rest.len());
        assert!(
            rest[..end].contains(call),
            "{entry} must reach its decision through the shared probe rather than carrying one \
                 of its own"
        );
    }
}

/// **A NEW credential name under a REPLACE (`UpsertIntent::Replace`) is REFUSED — a replace creates
/// no row.**
///
/// The writer that must never invent a row (a rotation of a key that is already there) says
/// `Replace`, and a name the table does not hold is refused BY NAME with the store untouched —
/// never quietly inserted as a deployment row the caller did not ask for.
#[test]
fn a_new_credential_name_under_a_replace_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (conn, _) = open_for_write(&db).expect("open");
    stamp_schema_version(&db, &conn).expect("stamp");
    drop(conn);

    let refused = upsert_rows(
        &db,
        Table::Credential,
        &[("ACME_DEMO_API_KEY".to_string(), "value-never-printed".to_string())],
        UpsertIntent::Replace,
    )
    .expect_err("a replace of a name this store has never held must be refused");
    assert_matches!(
        refused.kind,
        DbErrorKind::NotHeld { table: Table::Credential, .. },
        "{refused}"
    );
    assert!(refused.to_string().contains("ACME_DEMO_API_KEY"), "it must name the KEY");
    // A distinctive value: a bare `v1` once matched a random tempdir name (`.tmpPy3Sv1`).
    assert!(
        !refused.to_string().contains("value-never-printed"),
        "…and never its value: {refused}"
    );

    let rows = read_table(&db, Table::Credential, crate::CredentialScope::Full)
        .expect("read back")
        .0
        .into_map();
    assert!(rows.is_empty(), "the refusal must have written nothing");
}
