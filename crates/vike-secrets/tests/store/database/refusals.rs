//! Proofs 8-9 - a bad database is loud, and no store is ever created unasked.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 8 — a database that is there and wrong is LOUD, never an empty map
// ---------------------------------------------------------------------------------------------

/// **A present-and-unreadable database errors; it never degrades to "no credentials".**
///
/// An ABSENT store is the ordinary unconfigured state and is silent, while a store that EXISTS and
/// cannot be read must not look the same to an operator — otherwise a configured box drops every
/// venue to paper and looks exactly like a correct fresh install.
///
/// `1` is the oldest shape's stamp, and it is refused like any other number — by the reader and by
/// `create_store`, which converts nothing.
#[test]
fn a_database_that_is_not_this_schema_is_an_error_not_an_empty_map() {
    for found in [1, 99] {
        let fx = Fixture::live_shaped();
        let conn = fx.conn();
        stamp_version(&conn, found);
        drop(conn);

        let err = vike_secrets::resolve_project(fx.arg())
            .expect_err("a schema mismatch must not answer with an empty map");
        let said = err.to_string();
        assert!(said.contains(&format!("schema version {found}")), "{said}");
        assert!(said.contains("vike.db"), "{said}");

        match vike_secrets::create_store(fx.arg()) {
            Err(e) => {
                assert!(e.to_string().contains(&format!("schema version {found}")), "{e}");
            }
            Ok(m) => panic!("a store at schema {found} must be refused, not adopted: {m}"),
        }
    }
}

/// A file that is not a database at all, at the database's path. Same rule.
#[test]
fn a_corrupt_database_is_an_error_not_an_empty_map() {
    let fx = Fixture::empty();
    std::fs::create_dir_all(fx.db().parent().unwrap()).unwrap();
    std::fs::write(fx.db(), b"this is not a database").unwrap();

    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Database(fx.db()));
    let err = vike_secrets::resolve_project(fx.arg())
        .expect_err("a corrupt database must not answer with an empty map");
    assert!(err.to_string().contains("vike.db"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// PROOF 9 — NO STORE IS EVER CREATED UNASKED
// ---------------------------------------------------------------------------------------------
//
// The invariant these hold, and the one `Backend`'s existence-only probe silently assumes:
// **a database exists ⇒ a creation finished, because somebody asked for it.**
//
// Without it the failure is silent, total and permanent. `backend_at` is one `is_file`, so a
// half-written database answers `Database` for every process on the box forever; `resolve_project`
// then returns an EMPTY map or an error, never the store the operator meant to build.

/// **A creation interrupted after the schema batch but before the commit is a LOUD error, never an
/// empty map.**
///
/// `open_for_write` runs `execute_batch(SCHEMA)` before the transaction, so a failure of the
/// transaction or of the commit — a full disk, a read-only remount, `SIGKILL` — leaves a real file
/// at the database's path.
///
/// The cure is that `PRAGMA user_version` is stamped INSIDE that transaction, so the file carries
/// `user_version = 0`, which `check_schema_version` already refuses. **Moving the stamp out of the
/// transaction makes this test fail with an empty map instead of an error**, which is the whole of
/// its value.
///
/// The interruption is reconstructed rather than performed, because a `SIGKILL` mid-transaction is
/// not something a test can arrange portably: the schema batch (`vike_secrets::DDL`, the one a
/// create lays down) runs, the transaction does not commit, `user_version` is never stamped. That
/// is the STATE a crash leaves, and the state the assertions are about.
/// [`a_finished_creation_stamps_the_schema_version`] is what keeps the reconstruction honest.
#[test]
fn a_creation_interrupted_before_its_commit_is_a_loud_error_not_an_empty_map() {
    let fx = Fixture::empty();
    std::fs::create_dir_all(fx.db().parent().unwrap()).unwrap();

    let conn = fx.conn();
    conn.execute_batch(vike_secrets::DDL).expect("schema");
    drop(conn);

    // The file IS there, so the backend probe chooses it — that much is unavoidable and is not the
    // defect. What matters is what happens next.
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Database(fx.db()));

    let err = vike_secrets::resolve_project(fx.arg()).expect_err(
        "an unfinished database must NOT answer with an empty map — that is the live gate, and \
         every venue on this box would silently drop to paper",
    );
    let said = err.to_string();
    assert!(said.contains("schema version 0"), "it must name what it found: {said}");
    assert!(said.contains("vike.db"), "…and the artifact: {said}");

    // The node-key half answers identically — a half-written database is not a half-usable one.
    let node_err = vike_secrets::resolve_node_keys(fx.arg(), is_node_key)
        .expect_err("the node-key read must be just as loud");
    assert!(node_err.to_string().contains("schema version 0"), "{node_err}");

    // …and a re-run of the creator is loud too, rather than quietly adopting the orphan.
    match vike_secrets::create_store(fx.arg()) {
        Err(e) => {
            assert!(e.to_string().contains("schema version 0"), "{e}");
        }
        Ok(m) => panic!("an unfinished database must not be silently adopted: {m}"),
    }
}

/// **A finished creation stamps the version, so the test above cannot pass vacuously.**
///
/// The floor under it: if `stamp_schema_version` were simply never called, every read would be the
/// loud error above and that test would go green while the feature was completely broken.
#[test]
fn a_finished_creation_stamps_the_schema_version() {
    let fx = Fixture::empty();
    let report = fx.create();
    assert!(report.created, "{report}");

    let conn = fx.conn();
    let found = user_version(&conn);
    assert_eq!(found, vike_secrets::SCHEMA_VERSION, "a finished creation must stamp the version");
    drop(conn);

    assert!(vike_secrets::resolve_project(fx.arg()).expect("read back").secrets.is_empty());
}
