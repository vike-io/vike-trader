//! Proof 16 — `vike_secrets::create_store` (`vike-cli secrets init`): the EMPTY store on a fresh
//! box, and never a store that touches an existing one.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 16 — the explicit fresh start
// ---------------------------------------------------------------------------------------------
//
// An operator reaches the creator only by asking for it, and each test holds one of its invariants:
// the store it makes is empty and current; it is the shape every store has; an existing store does
// not move by a byte; and the rehearsal creates nothing.

fn init(fx: &Fixture) -> vike_secrets::StoreCreation {
    fx.create()
}

fn rows_in(fx: &Fixture, table: &str) -> i64 {
    fx.conn()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("count {table}: {e}"))
}

/// Every schema object of a store, as SQLite itself records it — the SHAPE two stores are compared
/// in, never a hand-written expectation of it.
fn shape(fx: &Fixture) -> Vec<(String, String, Option<String>)> {
    let conn = fx.conn();
    let mut stmt = conn
        .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
        .expect("prepare");
    let objects: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect();
    objects
}

/// **`secrets init` on a fresh box creates THE store: current schema, stamped, ZERO credential, node-key
/// and account rows — and from then on it answers, empty, without an error.**
///
/// Started from a box with no `settings/` directory at all, so "creating the directories" is
/// asserted rather than inherited from a fixture that made one.
#[test]
fn init_on_a_fresh_box_creates_the_empty_store_at_the_current_schema() {
    let fx = Fixture::empty();
    std::fs::remove_dir(fx.dir()).expect("start from a box with no settings directory at all");

    let done = init(&fx);
    assert!(done.created, "{done}");
    assert_eq!(done.db, fx.db(), "it must create the PROJECT's store, nowhere else");
    assert!(fx.db().is_file(), "the store must exist at {}", fx.db().display());

    let conn = fx.conn();
    assert_eq!(user_version(&conn), vike_secrets::SCHEMA_VERSION, "it must be STAMPED, finished");
    drop(conn);
    for table in ["credential", "node_key", "account"] {
        assert_eq!(rows_in(&fx, table), 0, "an initialised store holds no {table} row");
    }

    // It answers — and answers EMPTY, as a store and not as an error.
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Database(fx.db()));
    let resolved = vike_secrets::resolve_project(fx.arg()).expect("an initialised store reads");
    assert!(key_names(&resolved.secrets).is_empty(), "no credential: {:?}", resolved.source);
}

/// **An initialised store is the SAME SHAPE as one the credential writer has filled** — every
/// table, index and trigger SQLite records, the stamp and the venue roster — because a write
/// changes rows and never the schema it lands in.
#[test]
fn an_initialised_store_is_the_same_shape_as_a_written_one() {
    let empty = Fixture::initialised();
    let one_key = Fixture::seeded_with([("CLOUDFLARE_API_TOKEN", "fake")], is_node_key, &classify);

    assert_eq!(shape(&empty), shape(&one_key), "a write changed the store's schema");
    assert_eq!(user_version(&empty.conn()), user_version(&one_key.conn()));
    assert_eq!(rows_in(&empty, "venue"), rows_in(&one_key, "venue"), "the venue roster differs");
    assert!(rows_in(&empty, "venue") > 0, "the roster is seeded on a create, empty or not");
}

/// **`secrets init` on a box that ALREADY has a store is the ordinary no-op: not a re-create, not
/// an overwrite, not a byte.** Asserted on a store holding keys (whose loss would be the damage)
/// and on an initialised one (a second `init`).
#[test]
fn init_on_an_existing_store_is_a_no_op_and_moves_no_byte() {
    let seeded = Fixture::live_shaped();
    let before = std::fs::read(seeded.db()).expect("read the store");
    let keys_before = key_names(&vike_secrets::resolve_project(seeded.arg()).unwrap().secrets);

    let done = init(&seeded);
    assert!(!done.created, "{done}");
    assert_eq!(std::fs::read(seeded.db()).unwrap(), before, "init moved the store's bytes");
    assert_eq!(
        key_names(&vike_secrets::resolve_project(seeded.arg()).unwrap().secrets),
        keys_before,
        "init on an existing store LOST credentials"
    );

    let fresh = Fixture::empty();
    assert!(init(&fresh).created);
    let first = std::fs::read(fresh.db()).unwrap();
    assert!(!init(&fresh).created, "twice is once");
    assert_eq!(std::fs::read(fresh.db()).unwrap(), first, "a second init moved the bytes");
}

/// **The rehearsal of `secrets init` creates NOTHING and says what the run would do** — and on a box
/// that already has its store it predicts the no-op.
#[test]
fn init_dry_run_creates_nothing_and_predicts_the_empty_store() {
    let fx = Fixture::empty();
    let plan = preview(&fx);
    assert!(plan.would_create, "creating a store is irreversible for the box: {plan}");
    assert!(plan.to_string().contains("NOTHING WAS WRITTEN"), "{plan}");
    assert!(!fx.db().exists(), "A DRY RUN OF init CREATED THE STORE");
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Absent);

    // …and the apply then does what it said.
    assert!(init(&fx).created);

    // Beside an existing store the plan is the no-op.
    let seeded = Fixture::live_shaped();
    let plan = preview(&seeded);
    assert!(!plan.would_create, "{plan}");
    assert_eq!(plan.db, seeded.db());
}
