//! Proofs 7-9 - a migration refuses rather than half-finishing, and a bad database is loud.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 7 — it refuses rather than half-finishing
// ---------------------------------------------------------------------------------------------

/// The same name in both files with different values — a half-migrated box. Merging would produce a
/// mismatched pair, whose symptom at the node is an opaque `bad mac`; picking a side is a guess.
#[test]
fn disagreeing_files_are_refused_and_nothing_is_written() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), "VIKE_TRADEHUB_OBSERVE_KEY=old\nBINANCE_DEMO_API_KEY=b\n").unwrap();
    std::fs::write(fx.node(), "VIKE_TRADEHUB_OBSERVE_KEY=new\n").unwrap();

    let found = refusal(&fx);
    assert_eq!(
        found,
        vec![vike_secrets::Ambiguity::DisagreeingFiles {
            key: "VIKE_TRADEHUB_OBSERVE_KEY".to_string()
        }]
    );
    assert!(!fx.db().exists(), "a refusal must not leave a half-filled database behind");
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the directory");
}

/// A venue credential sitting in the node file. The migration will not guess whether that is a key
/// filed in the wrong place or a node key the predicate has not heard of — either guess writes a
/// credential into a namespace nothing will look for it in.
#[test]
fn an_unclassifiable_name_in_the_node_file_is_refused() {
    let fx = Fixture::empty();
    std::fs::write(fx.node(), "BINANCE_DEMO_API_KEY=b\n").unwrap();

    let found = refusal(&fx);
    assert_eq!(
        found,
        vec![vike_secrets::Ambiguity::UnexpectedNameInNodeFile {
            key: "BINANCE_DEMO_API_KEY".to_string()
        }]
    );
    assert!(!fx.db().exists());
}

/// A file edited after the migration, disagreeing with the row already stored. Nothing here can tell
/// which is newer — so neither is overwritten, and the operator is told the key by name.
///
/// ⚠ **The refusal is PER-KEY, and the run succeeds.** It used to be whole-run: the stored value was
/// protected, and so was every UNRELATED key in the same edit — from being migrated at all. See
/// [`a_mixed_re_migration_lands_the_new_key_and_refuses_only_the_disagreeing_one`], which is the
/// half this test does not cover.
#[test]
fn a_file_that_disagrees_with_the_database_is_refused() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), "BINANCE_DEMO_API_KEY=first\n").unwrap();
    fx.migrate();
    let before = std::fs::read(fx.db()).unwrap();

    std::fs::write(fx.store(), "BINANCE_DEMO_API_KEY=second\n").unwrap();
    let report = fx.migrate();
    assert_eq!(
        report.refused,
        vec![vike_secrets::Ambiguity::DisagreesWithDatabase {
            key: "BINANCE_DEMO_API_KEY".to_string(),
            table: Table::Credential
        }]
    );
    assert_eq!(report.inserted(), 0, "nothing to insert: the only key was refused");
    assert_eq!(std::fs::read(fx.db()).unwrap(), before, "the refusal wrote nothing");
    // The stored value is the FIRST one — the file's edit did not win, and neither did it lose
    // silently: the key is named in the report.
    let stored = vike_secrets::read_table(&fx.db(), Table::Credential).unwrap().into_map();
    assert_eq!(stored.get("BINANCE_DEMO_API_KEY").map(String::as_str), Some("first"));
    assert!(report.to_string().contains("BINANCE_DEMO_API_KEY"), "{report}");
}

/// **The static predicate, enforced.** A name already in one table may not be written into the
/// other, whatever predicate a later run is handed — that is 0051's *one name, one home* expressed
/// as a check rather than as a convention.
#[test]
fn a_name_may_not_acquire_a_second_namespace() {
    let fx = Fixture::empty();
    std::fs::write(fx.store(), "VIKE_TRADEHUB_OBSERVE_KEY=o\n").unwrap();
    // First run: the union predicate files it as a node key.
    fx.migrate();
    assert_eq!(
        key_names(&vike_secrets::read_table(&fx.db(), Table::NodeKey).unwrap()),
        BTreeSet::from(["VIKE_TRADEHUB_OBSERVE_KEY".to_string()])
    );

    // Second run under a predicate that claims nothing — the shape a per-service predicate would
    // take for the OTHER service. It would file the same name as a venue credential.
    let found = match vike_secrets::migrate(
        fx.arg(),
        |_| false,
        &classify,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    ) {
        Err(vike_secrets::MigrateError::Ambiguous(list)) => list,
        Ok(m) => panic!("expected a refusal, got: {m}"),
        Err(e) => panic!("expected an ambiguity refusal, got: {e}"),
    };
    assert_eq!(
        found,
        vec![vike_secrets::Ambiguity::WrongTable {
            key: "VIKE_TRADEHUB_OBSERVE_KEY".to_string(),
            found_in: Table::NodeKey,
            wanted: Table::Credential,
        }]
    );
    assert!(
        vike_secrets::read_table(&fx.db(), Table::Credential).unwrap().is_empty(),
        "and the credential table stayed empty"
    );
}

/// Every refusal is collected, not just the first — one run tells the operator everything they have
/// to fix rather than sending them round a loop.
#[test]
fn every_ambiguity_is_reported_in_one_pass() {
    let fx = Fixture::empty();
    // Distinctive values, so the "names keys, never values" assertion below has something it could
    // actually fail on — a fixture of `a`/`b` would make it pass vacuously.
    std::fs::write(fx.store(), "VIKE_TRADEHUB_OBSERVE_KEY=UNIQUE_VALUE_ALPHA\n").unwrap();
    std::fs::write(
        fx.node(),
        "VIKE_TRADEHUB_OBSERVE_KEY=UNIQUE_VALUE_BRAVO\nBINANCE_DEMO_API_KEY=UNIQUE_VALUE_CHARLIE\n",
    )
    .unwrap();

    let found = refusal(&fx);
    assert_eq!(found.len(), 2, "both findings, not the first: {found:?}");
    let rendered = found.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(rendered.contains("half-migrated"), "{rendered}");
    assert!(rendered.contains("does not claim it"), "{rendered}");
    // A refusal names KEYS and never values — the same contract `SecretMap`'s `Debug` holds.
    for planted in ["UNIQUE_VALUE_ALPHA", "UNIQUE_VALUE_BRAVO", "UNIQUE_VALUE_CHARLIE"] {
        assert!(!rendered.contains(planted), "a refusal printed a credential VALUE: {rendered}");
    }
    assert!(
        rendered.contains("VIKE_TRADEHUB_OBSERVE_KEY"),
        "…but it must name the key: {rendered}"
    );
}

// ---------------------------------------------------------------------------------------------
// PROOF 8 — a database that is there and wrong is LOUD, never an empty map
// ---------------------------------------------------------------------------------------------

/// **A present-and-unreadable database errors; it never degrades to "no credentials".**
///
/// The distinction the root `CLAUDE.md` draws for the file store, carried onto the database: an
/// ABSENT store is the ordinary unconfigured state and is silent, while a store that EXISTS and
/// cannot be read must not look the same to an operator — otherwise a configured box drops every
/// venue to paper and looks exactly like a correct fresh install.
#[test]
fn a_database_that_is_not_this_schema_is_an_error_not_an_empty_map() {
    let fx = Fixture::live_shaped();
    fx.migrate();
    let conn = fx.conn();
    stamp_version(&conn, 99);
    drop(conn);

    let err = vike_secrets::resolve_project(fx.arg())
        .expect_err("a schema mismatch must not answer with an empty map");
    let said = err.to_string();
    assert!(said.contains("schema version 99"), "{said}");
    assert!(said.contains("vike.db"), "{said}");
}

/// A file that is not a database at all, at the database's path. Same rule.
#[test]
fn a_corrupt_database_is_an_error_not_an_empty_map() {
    let fx = Fixture::live_shaped();
    std::fs::create_dir_all(fx.db().parent().unwrap()).unwrap();
    std::fs::write(fx.db(), b"this is not a database").unwrap();

    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Database(fx.db()));
    let err = vike_secrets::resolve_project(fx.arg())
        .expect_err("a corrupt database must not answer with an empty map");
    assert!(err.to_string().contains("vike.db"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// PROOF 9 — AN EMPTY DATABASE CANNOT SHADOW A REAL CREDENTIAL FILE
// ---------------------------------------------------------------------------------------------
//
// The invariant these hold, and the one `Backend`'s existence-only probe silently assumes:
// **a database exists ⇒ a migration finished.**
//
// Without it the failure is silent, total and permanent. `backend_at` is one `is_file`, so a
// zero-row database answers `Database` for every process on the box forever; `resolve_project` then
// returns an EMPTY map, which downstream is not an error but the LIVE GATE — every venue drops to
// paper while `secrets.env` sits on disk looking exactly right.

/// **A migration with nothing to migrate creates NO DATABASE, so the file written afterwards can
/// still be carried in.**
///
/// ⚠ This is the regression test for the defect, written to prove the CURE. The old code's step 4
/// opened a write connection whenever `pending.is_empty()` was true AND the database did not exist —
/// and opening one CREATES the store. So a run on a fresh box, before the operator had written a
/// single key, minted a schema-stamped zero-row `vike.db` and shadowed the store they were about to
/// create.
///
/// The links in that chain are asserted separately, and each would fail on its own without the fix:
/// no file on disk, the backend still `Absent`, the file written afterwards REPORTED as unread
/// rather than hidden behind an empty store, and every key readable once a second run carries it.
#[test]
fn an_empty_database_is_never_created_so_it_cannot_shadow_the_real_file() {
    let fx = Fixture::empty();

    // 1. Migrate a project with NO credential file at all.
    let report = fx.migrate();
    assert_eq!(
        report.outcome,
        vike_secrets::MigrationOutcome::NothingToMigrate,
        "an empty project has nothing to migrate: {report}"
    );
    assert!(!report.database_exists(), "…and the run says so: {report}");
    assert!(
        !fx.db().exists(),
        "A DATABASE WAS CREATED WITH NOTHING IN IT. From here `backend_at` answers `Database` for \
         every process on this box with an EMPTY map — the LIVE GATE, not an error — and the \
         credential file written below is reported as merely shadowed."
    );
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert_eq!(
        vike_secrets::workspace_backend_from(fx.arg()),
        Backend::Absent,
        "still no store for this project"
    );

    // 2. NOW the operator writes their credential file, as the older runbooks told them to. It is
    //    NOT read — the file store is gone — but it is REPORTED, never hidden behind an empty store.
    write_store(&fx.store(), &LIVE_CREDENTIAL_KEYS);
    let unread = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert!(unread.secrets.is_empty(), "a credential FILE never answers");
    assert_eq!(
        unread.unread.expect("the file must be reported as unread").keyed,
        Ok(LIVE_CREDENTIAL_KEYS.len())
    );

    // 3. Every key comes back once the carry runs — the end-to-end path such a box takes.
    fx.migrate();
    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve_project");
    assert_eq!(resolved.source, Source::Database(fx.db()), "the DATABASE must answer");
    assert_eq!(
        key_names(&resolved.secrets),
        LIVE_CREDENTIAL_KEYS.iter().map(|k| (*k).to_string()).collect::<BTreeSet<_>>(),
        "the credential file was shadowed by an empty database"
    );
}

/// **A migration interrupted after the schema is stamped but before the commit is a LOUD error,
/// never an empty map.**
///
/// The same end state as the test above, reached from the OTHER direction and the worse of the two:
/// `open_for_write` runs `execute_batch(SCHEMA)` before the insert transaction, so a failure of any
/// INSERT or of the commit — a full disk, a read-only remount, `SIGKILL` — leaves a real file at the
/// database's path.
///
/// The cure is that `PRAGMA user_version` is stamped AFTER the commit, so that file carries
/// `user_version = 0`, which `check_schema_version` already refuses. **Restoring the stamp to its
/// old place makes this test fail with an empty map instead of an error**, which is the whole of its
/// value.
///
/// The interruption is reconstructed rather than performed, because a `SIGKILL` mid-transaction is
/// not something a test can arrange portably: the schema batch runs, the rows do not commit,
/// `user_version` is never stamped. That is the STATE a crash leaves, and the state the assertions
/// are about.
#[test]
fn a_migration_interrupted_before_its_commit_is_a_loud_error_not_an_empty_map() {
    let fx = Fixture::live_shaped();
    std::fs::create_dir_all(fx.db().parent().unwrap()).unwrap();

    let conn = fx.conn();
    conn.execute_batch(SCHEMA_AS_MIGRATE_WRITES_IT).expect("schema");
    drop(conn);

    // The file IS there, so the backend probe chooses it — that much is unavoidable and is not the
    // defect. What matters is what happens next.
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Database(fx.db()));

    let err = vike_secrets::resolve_project(fx.arg()).expect_err(
        "an unfinished database must NOT answer with an empty map — that is the live gate, and \
         every venue on this box would silently drop to paper with a full secrets.env on disk",
    );
    let said = err.to_string();
    assert!(said.contains("schema version 0"), "it must name what it found: {said}");
    assert!(said.contains("vike.db"), "…and the artifact: {said}");

    // The node-key half answers identically — a half-written database is not a half-usable one.
    let node_err = vike_secrets::resolve_node_keys(fx.arg(), is_node_key)
        .expect_err("the node-key read must be just as loud");
    assert!(node_err.to_string().contains("schema version 0"), "{node_err}");

    // …and a re-run of the migration is loud too, rather than quietly filling the orphan.
    match vike_secrets::migrate(
        fx.arg(),
        is_node_key,
        &classify,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    ) {
        Err(vike_secrets::MigrateError::Db(e)) => {
            assert!(e.to_string().contains("schema version 0"), "{e}");
        }
        Ok(m) => panic!("an unfinished database must not be silently adopted: {m}"),
        Err(e) => panic!("expected a schema-version error, got: {e}"),
    }
}

/// The schema exactly as `migrate` writes it, so the reconstruction above is the real state and not
/// an approximation of it. (`crate::db`'s `SCHEMA` is private; this is its text, and
/// [`a_finished_migration_stamps_the_schema_version`] is what keeps the two honest — if they ever
/// diverged, a real migration would stop being readable and that test would fail first.)
const SCHEMA_AS_MIGRATE_WRITES_IT: &str = "\
CREATE TABLE IF NOT EXISTS credential (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS node_key   (name TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;
";

/// **A finished migration stamps the version, so the two tests above cannot pass vacuously.**
///
/// The floor under them: if `stamp_schema_version` were simply never called, every read would be the
/// loud error above and the pair would go green while the feature was completely broken.
#[test]
fn a_finished_migration_stamps_the_schema_version() {
    let fx = Fixture::live_shaped();
    let report = fx.migrate();
    assert_eq!(report.outcome, vike_secrets::MigrationOutcome::Created);

    let conn = fx.conn();
    let found = user_version(&conn);
    assert_eq!(found, vike_secrets::SCHEMA_VERSION, "a finished migration must stamp the version");
    drop(conn);

    assert_eq!(vike_secrets::resolve_project(fx.arg()).expect("read back").secrets.len(), 67);
}
