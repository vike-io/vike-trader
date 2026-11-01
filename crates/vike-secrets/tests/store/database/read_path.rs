//! Proof 6 and the second reader - the read path is per-RUN, never per-KEY.

use super::*;

// ---------------------------------------------------------------------------------------------
// PROOF 6 — the read path: per-RUN, never per-KEY
// ---------------------------------------------------------------------------------------------

/// **RULE 1 — a box with no database has NO credentials.**
///
/// The probe says [`Backend::Absent`] and every reader answers EMPTY — the live gate: every venue
/// paper — and none of them brings a database into existence.
#[test]
fn a_box_with_no_database_has_no_credentials() {
    let fx = Fixture::empty();
    assert_eq!(
        vike_secrets::backend_in(&vike_secrets::workspace_settings_dir_from(fx.arg())),
        Backend::Absent,
        "no database, so: no store at all"
    );

    let resolved = crate::support::store_from(fx.arg()).read_all().expect("absent is an answer");
    assert_eq!(resolved.source, Source::None);
    assert!(resolved.secrets.is_empty(), "no credentials ⇒ every venue stays paper");

    let nodes = crate::support::store_from(fx.arg()).hub_keys(is_hub_key).expect("hub keys");
    assert_eq!(nodes.source, Source::None);
    assert!(nodes.secrets.is_empty());

    // ⚠ THE SECOND READER, on the same project: infallible, so it cannot report — it can only
    // answer, and the answer is the same empty map.
    assert!(
        crate::support::store_from(fx.arg())
            .read_all()
            .map(|r| r.secrets.into_map())
            .unwrap_or_default()
            .is_empty()
    );

    assert!(!fx.db().exists(), "a READ must never bring a database into existence");
}

/// **The hub keys come from the `hub_key` TABLE, and it answers for its own family.**
#[test]
fn the_hub_key_table_answers() {
    let fx = Fixture::live_shaped();

    let resolved = crate::support::store_from(fx.arg()).hub_keys(is_hub_key).expect("nodes");
    assert_eq!(resolved.source, Source::Database(fx.db()), "the TABLE answered");
    let map = resolved.secrets.into_map();
    assert_eq!(
        map.get("VIKE_TRADEHUB_OBSERVE_KEY"),
        Some(&fake_value("VIKE_TRADEHUB_OBSERVE_KEY")),
        "the stored value"
    );
    assert_eq!(map.len(), 4, "the whole hub_key table and nothing else");
}

/// **A store that still holds the RETIRED table `node_key` yields NO hub keys — there is no fallback
/// that reads the old name** (decision 0166 verdict 4, `docs/decisions/0117-there-are-no-migrations.md`).
///
/// The rows are the live four, under the retired table name, in an otherwise current store: the
/// state of a box whose operator has not yet run the hand rename `docs/ops/tradehub-the CI box.md`'s
/// "The hub-key table is `hub_key` from this release" gives. Three states are
/// measured (0166 "Consequences" names the first and the third):
///
/// 1. **Before any writer ran**: the read is an ERROR that says `no such table` — the
///    present-but-unreadable class, which every daemon turns into its refusal (the gatewayhub's
///    `KEYS_STORE_UNREADABLE`) or its keyless posture, never into the four keys of `node_key`.
/// 2. **A credential writer** is refused the same way (its two-namespace check reads `hub_key` first).
/// 3. **A hub-key writer** (`backend setup`, `datahub setup`, `copy-hub-keys`) creates `hub_key` itself
///    (`CREATE TABLE IF NOT EXISTS`) holding only what it wrote, and `node_key` still holds its four
///    rows, unread and untouched.
///
/// The retired table is planted with `CREATE TABLE` and `INSERT ... SELECT`, and the fixture's
/// `hub_key` is dropped: no statement in this repo renames the table, that is the operator's act.
///
/// **Making any reader fall back to `node_key` (a second `SELECT` on a miss, a probe for the old
/// name) fails the first and the last assertion.**
#[test]
fn a_store_holding_only_the_retired_node_key_table_yields_no_hub_keys() {
    let fx = Fixture::live_shaped();
    let conn = fx.conn();
    conn.execute_batch(
        "CREATE TABLE node_key (
             id    INTEGER PRIMARY KEY AUTOINCREMENT,
             name  TEXT NOT NULL UNIQUE,
             value TEXT NOT NULL
         ) STRICT;
         INSERT INTO node_key (name, value) SELECT name, value FROM hub_key;
         DROP TABLE hub_key;",
    )
    .expect("plant the pre-rename shape");
    let retired_rows: i64 =
        conn.query_row("SELECT count(*) FROM node_key", [], |r| r.get(0)).expect("count");
    assert_eq!(retired_rows, 4, "the premise: the retired table holds the four hub keys");
    drop(conn);

    // 1. Before any writer: an ERROR, never the four keys.
    let refused = crate::support::store_from(fx.arg())
        .hub_keys(is_hub_key)
        .expect_err("a store without a `hub_key` table is unreadable for hub keys, not empty");
    let said = refused.to_string();
    assert!(said.contains("no such table") && said.contains("hub_key"), "{said}");
    assert!(!said.contains("fake-value"), "an error printed a VALUE: {said}");

    // 2. A CREDENTIAL writer is refused the same way (its two-namespace check reads `hub_key` first),
    //    and creates nothing.
    let refused = vike_secrets::save_credentials_to_store_unjournalled(
        fx.dir(),
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect_err("a credential write on a store without `hub_key` is refused");
    assert!(refused.to_string().contains("no such table"), "{refused}");
    assert!(crate::support::store_from(fx.arg()).hub_keys(is_hub_key).is_err(), "still unreadable");

    // 3. A HUB-KEY writer creates `hub_key` itself (the `DDL` batch is `CREATE TABLE IF NOT EXISTS`) and
    //    the table holds ONLY what that write put there: the four retired rows are still unread.
    vike_secrets::save_credentials_to_store_unjournalled(
        fx.dir(),
        Table::HubKey,
        &[("VIKE_TRADEHUB_OBSERVE_KEY".to_string(), "freshly-minted".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("a hub-key write runs the DDL batch");
    let map = crate::support::store_from(fx.arg())
        .hub_keys(is_hub_key)
        .expect("the table exists again")
        .secrets
        .into_map();
    assert_eq!(map.len(), 1, "the retired table's rows were read: {:?}", map.keys());
    assert_eq!(map.get("VIKE_TRADEHUB_OBSERVE_KEY").map(String::as_str), Some("freshly-minted"));
    let conn = fx.conn();
    let still: i64 =
        conn.query_row("SELECT count(*) FROM node_key", [], |r| r.get(0)).expect("count");
    assert_eq!(still, 4, "nothing in this workspace touches the retired table");
}

// ---------------------------------------------------------------------------------------------
// PROOF — THE SECOND READER IS ON THE SAME LADDER
// ---------------------------------------------------------------------------------------------

/// **`load_project_secrets` answers from the DATABASE, exactly as `resolve_project` does.**
///
/// The defect this is written against was live in shipped code: that function was once a
/// `read_to_string` of a credential FILE and was never routed through `Backend`, so it was a SECOND
/// store choice sitting beside the one `resolve_project` makes — the ladder
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` forbids. Nothing errored — an absent
/// key IS the live gate — so an IBKR mount lost its account in silence.
///
/// **Making `load_project_secrets` answer from anywhere but `crate::store::resolve_store_in`
/// makes these assertions fail.**
#[test]
fn the_second_reader_answers_from_the_database() {
    let fx = Fixture::live_shaped();

    let map = crate::support::store_from(fx.arg())
        .read_all()
        .map(|r| r.secrets.into_map())
        .unwrap_or_default();
    assert_eq!(
        map.get("IBKR_DEMO_ACCOUNT"),
        Some(&fake_value("IBKR_DEMO_ACCOUNT")),
        "the infallible reader did not answer from the store"
    );
    assert_eq!(map.len(), 67, "exactly the table, nothing merged in");

    // …and it is the same answer the fallible reader gives, which is the property that makes the
    // two structurally incapable of disagreeing rather than merely agreeing today.
    let fallible = crate::support::store_from(fx.arg()).read_all().expect("read_all");
    assert_eq!(fallible.source, Source::Database(fx.db()));
    assert_eq!(map, fallible.secrets.into_map(), "one store choice, or it was never closed");
}

/// **A database that EXISTS and cannot be READ is an empty map to the infallible reader and a LOUD
/// error to the fallible one — and the asymmetry is pinned rather than assumed.**
///
/// `load_project_secrets` is infallible by signature and a read in this crate logs nothing, so
/// routing it through the backend could not give it a channel for the error. That is
/// deliberate — these are STARTUP paths that must not gain a new hard failure — but "empty map" and
/// "loud error" must not quietly become the same answer, so both halves are measured here on ONE
/// store in ONE state. The cure for a caller that must tell the two apart is named in the function's
/// own doc: use `resolve_project`.
#[test]
fn an_unreadable_database_is_empty_to_the_infallible_reader_and_loud_to_the_fallible_one() {
    let fx = Fixture::live_shaped();

    // The state a crash leaves and the state a future schema leaves, reached the same way the
    // interrupted-creation test reaches it: move `user_version` off `SCHEMA_VERSION`.
    let conn = fx.conn();
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION + 1);
    drop(conn);

    // The database is still THERE, so the backend still chooses it.
    assert_eq!(
        vike_secrets::backend_in(&vike_secrets::workspace_settings_dir_from(fx.arg())),
        Backend::Database(fx.db())
    );

    let loud = crate::support::store_from(fx.arg())
        .read_all()
        .expect_err("a store that exists and cannot be read is an ERROR");
    let said = loud.to_string();
    assert!(said.contains("schema version"), "the error must say what is wrong: {said}");

    let quiet = crate::support::store_from(fx.arg())
        .read_all()
        .map(|r| r.secrets.into_map())
        .unwrap_or_default();
    assert!(quiet.is_empty(), "the infallible reader answers nothing from a store it cannot read");
}

/// **The WRITE path on a project whose database was removed is REFUSED — it lands nowhere, and
/// it is not the race either.**
///
/// The companion to `crates/vike-secrets/src/db/create_tests.rs`'s
/// `a_write_whose_database_vanished_creates_nothing_and_fails_loudly`, which reconstructs the state
/// INSIDE the window between `backend_in`'s probe and `upsert_rows`' open. This one covers the
/// ordinary case that looks similar from outside: the database is gone BEFORE the probe, so the
/// probe says `Absent`, the write is REFUSED by name, and no database comes back.
#[test]
fn a_write_after_the_database_is_gone_is_refused_and_touches_nothing() {
    let fx = Fixture::live_shaped();

    // The operator removes the database — their own act, which nothing in this workspace performs.
    std::fs::remove_file(fx.db()).expect("remove the database");
    assert_eq!(
        vike_secrets::backend_in(&vike_secrets::workspace_settings_dir_from(fx.arg())),
        Backend::Absent,
        "the probe sees no database"
    );

    let refused = vike_secrets::save_credentials_to_store_unjournalled(
        fx.dir(),
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "rotated".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect_err("with no database there is no store to write");
    let said = refused.to_string();
    assert!(said.contains("vike-cli secrets init"), "the refusal must name the way in: {said}");

    assert!(!fx.db().exists(), "a WRITE must never bring a database back into existence");
    assert!(
        crate::support::store_from(fx.arg())
            .read_all()
            .map(|r| r.secrets.into_map())
            .unwrap_or_default()
            .is_empty(),
        "and with no database the box has no credentials"
    );
}
