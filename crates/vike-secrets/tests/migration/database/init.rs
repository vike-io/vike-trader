//! Proof 16 — `WhenNothingToCarry::CreateEmptyStore` (`vike-cli secrets migrate --init`): the EMPTY
//! store on a fresh box, and never an empty store that shadows a credential file or touches an
//! existing one.

use super::*;
use vike_secrets::{MigrationOutcome, PlannedOutcome, WhenNothingToCarry};

// ---------------------------------------------------------------------------------------------
// PROOF 16 — the explicit fresh start
// ---------------------------------------------------------------------------------------------
//
// `an_empty_database_is_never_created_so_it_cannot_shadow_the_real_file` (refusals.rs) is the
// default arm and stays exactly as it was: with nothing to carry, plain `migrate` creates nothing.
// These are the OTHER arm, which an operator reaches only by asking for it, and each test holds one
// of its invariants: the store it makes is empty and current; it is the same shape as one a
// migration makes; a file with a key is CARRIED, never shadowed; a refused file writes nothing; an
// existing store does not move by a byte; and the rehearsal creates nothing.

fn init(fx: &Fixture) -> vike_secrets::Migration {
    match vike_secrets::migrate(
        fx.arg(),
        is_node_key,
        &classify,
        WhenNothingToCarry::CreateEmptyStore,
    ) {
        Ok(m) => m,
        Err(e) => panic!("`--init` refused: {e}"),
    }
}

fn preview_init(fx: &Fixture) -> vike_secrets::MigrationPlan {
    match vike_secrets::preview(
        fx.arg(),
        is_node_key,
        &classify,
        WhenNothingToCarry::CreateEmptyStore,
    ) {
        Ok(p) => p,
        Err(e) => panic!("`--init --dry-run` refused: {e}"),
    }
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

/// **`--init` on a fresh box creates THE store: current schema, stamped, ZERO credential, node-key
/// and account rows — and from then on it answers, empty, without an error.**
///
/// Started from a box with no `settings/` directory at all, so "creating the directories" is
/// asserted rather than inherited from a fixture that made one.
#[test]
fn init_on_a_fresh_box_creates_the_empty_store_at_the_current_schema() {
    let fx = Fixture::empty();
    std::fs::remove_dir(fx.dir()).expect("start from a box with no settings directory at all");

    let done = init(&fx);
    assert_eq!(done.outcome, MigrationOutcome::Initialised, "{done}");
    assert!(done.created() && done.database_exists(), "{done}");
    assert!(done.inserted_keys.is_empty(), "an empty store inserts no key: {done}");
    assert_eq!(done.inserted(), 0);
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

/// **An initialised store is the SAME SHAPE as one a migration created** — every table, index and
/// trigger SQLite records, the stamp and the venue roster — because it is built by the same write
/// rather than by a second schema spelled for it.
#[test]
fn an_initialised_store_is_the_same_shape_as_a_migrated_one() {
    let empty = Fixture::empty();
    init(&empty);
    let one_key = Fixture::with_store_text("CLOUDFLARE_API_TOKEN=fake\n");
    assert_eq!(one_key.migrate().outcome, MigrationOutcome::Created);

    assert_eq!(shape(&empty), shape(&one_key), "an initialised store has a schema of its own");
    assert_eq!(user_version(&empty.conn()), user_version(&one_key.conn()));
    assert_eq!(rows_in(&empty, "venue"), rows_in(&one_key, "venue"), "the venue roster differs");
    assert!(rows_in(&empty, "venue") > 0, "the roster is seeded on a create, empty or not");
}

/// **THE INCIDENT GUARD: `--init` never creates an empty store that shadows a credential file.**
///
/// A file that carries a key makes the run an ordinary [`MigrationOutcome::Created`] that CARRIES
/// it — for either file — exactly as a plain run would; the outcome is asserted as well as the key,
/// so an `--init` that created the store "empty, as asked" with the key beside it fails too.
#[test]
fn init_beside_a_credential_file_carries_it_and_never_shadows_it() {
    let fx = Fixture::with_store_text("BINANCE_DEMO_API_KEY=fake-binance\n");
    let before = digest(&fx.store());

    let done = init(&fx);
    assert_eq!(done.outcome, MigrationOutcome::Created, "a file with a key is MIGRATED: {done}");
    assert_eq!(done.inserted_keys, vec!["BINANCE_DEMO_API_KEY".to_string()], "{done}");
    let resolved = vike_secrets::resolve_project(fx.arg()).expect("resolve");
    assert_eq!(
        key_names(&resolved.secrets),
        BTreeSet::from(["BINANCE_DEMO_API_KEY".to_string()]),
        "THE CREDENTIAL FILE WAS SHADOWED — the store answers without the key it carried"
    );
    assert_eq!(digest(&fx.store()), before, "the credential file is READ, never written");

    // The node file alone counts too: its key lands in its own table.
    let node_only = Fixture::empty();
    node_only.write_node_text("VIKE_TRADEHUB_OBSERVE_KEY=fake-observe\n");
    let done = init(&node_only);
    assert_eq!(done.outcome, MigrationOutcome::Created, "{done}");
    assert_eq!(rows_in(&node_only, "node_key"), 1, "the node key must be carried: {done}");
}

/// **A file the plan REFUSES is refused under `--init` too, and nothing is created.** The flag
/// decides the empty arm only; it is never a way past a refusal.
#[test]
fn init_on_an_ambiguous_box_writes_nothing() {
    let fx = Fixture::empty();
    fx.write_node_text("BINANCE_DEMO_API_KEY=filed-in-the-wrong-place\n");

    for refused in [
        vike_secrets::migrate(
            fx.arg(),
            is_node_key,
            &classify,
            WhenNothingToCarry::CreateEmptyStore,
        )
        .map(|m| m.to_string()),
        vike_secrets::preview(
            fx.arg(),
            is_node_key,
            &classify,
            WhenNothingToCarry::CreateEmptyStore,
        )
        .map(|p| p.to_string()),
    ] {
        match refused {
            Err(vike_secrets::MigrateError::Ambiguous(list)) => assert!(!list.is_empty()),
            other => panic!("a refused file must stay refused under --init: {other:?}"),
        }
        assert!(!fx.db().exists(), "a refusal under --init left a database behind");
        assert!(!fx.db().parent().unwrap().exists(), "…nor may it leave the directory");
    }
}

/// **`--init` on a box that ALREADY has a store is the ordinary no-op: not a re-create, not an
/// overwrite, not a byte.** Asserted on a migrated store holding keys (whose loss would be the
/// damage) and on an initialised one (a second `--init`).
#[test]
fn init_on_an_existing_store_is_a_no_op_and_moves_no_byte() {
    let migrated = Fixture::live_shaped();
    assert_eq!(migrated.migrate().outcome, MigrationOutcome::Created);
    let before = std::fs::read(migrated.db()).expect("read the store");
    let keys_before = key_names(&vike_secrets::resolve_project(migrated.arg()).unwrap().secrets);

    let done = init(&migrated);
    assert_eq!(done.outcome, MigrationOutcome::AlreadyComplete, "{done}");
    assert_eq!(std::fs::read(migrated.db()).unwrap(), before, "--init moved the store's bytes");
    assert_eq!(
        key_names(&vike_secrets::resolve_project(migrated.arg()).unwrap().secrets),
        keys_before,
        "--init on an existing store LOST credentials"
    );

    let fresh = Fixture::empty();
    assert_eq!(init(&fresh).outcome, MigrationOutcome::Initialised);
    let first = std::fs::read(fresh.db()).unwrap();
    assert_eq!(init(&fresh).outcome, MigrationOutcome::AlreadyComplete, "twice is once");
    assert_eq!(std::fs::read(fresh.db()).unwrap(), first, "a second --init moved the bytes");
}

/// **The rehearsal of `--init` creates NOTHING and says what the run would do** — and in every
/// state but the empty one it predicts exactly what it predicts without the flag.
#[test]
fn init_dry_run_creates_nothing_and_predicts_the_empty_store() {
    let fx = Fixture::empty();
    let plan = preview_init(&fx);
    assert_eq!(plan.outcome, PlannedOutcome::WouldInitialise, "{plan}");
    assert!(plan.would_create(), "an empty store is as irreversible as a filled one: {plan}");
    assert_eq!(plan.would_insert(), 0, "{plan}");
    assert!(plan.to_string().contains("NOTHING WAS WRITTEN"), "{plan}");
    assert!(!fx.db().exists(), "A DRY RUN OF --init CREATED THE STORE");
    assert!(!fx.db().parent().unwrap().exists(), "…nor even the `db/` directory");
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Absent);

    // …and the apply then does what it said.
    assert_eq!(init(&fx).outcome, MigrationOutcome::Initialised);

    // Beside a file with keys the flag changes nothing about the prediction.
    let live = Fixture::live_shaped();
    let with = preview_init(&live);
    let without = preview(&live);
    assert_eq!(with.outcome, PlannedOutcome::WouldCreate, "{with}");
    assert_eq!(with, without, "--init changed a plan it has no say in");
}

/// **A credential file with NO key — comments only — is nothing to carry**, so `--init` creates the
/// empty store beside it, and the file stays byte-identical. The boundary is "carries a key", not
/// "exists": a file with no key has nothing an empty store could shadow.
#[test]
fn a_comment_only_credential_file_is_nothing_to_carry() {
    let fx = Fixture::with_store_text("# nothing here yet\n\n");
    let before = digest(&fx.store());
    assert_eq!(fx.migrate().outcome, MigrationOutcome::NothingToMigrate, "plain: nothing");
    assert!(!fx.db().exists(), "plain `migrate` must still create nothing here");

    let done = init(&fx);
    assert_eq!(done.outcome, MigrationOutcome::Initialised, "{done}");
    assert_eq!(rows_in(&fx, "credential"), 0);
    assert_eq!(digest(&fx.store()), before, "the file is READ, never written");
}
