//! Tests of `venues.rs`: `ensure_venue_rows`, `ensure_venue_id_columns` and the title seed.

use super::venues::{VENUE_TITLES, venue_title_seed};
use super::*;

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
    assert_eq!(count as usize, vike_model::VENUES.len());
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
    assert_eq!(count as usize, vike_model::VENUES.len());
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
/// `crates/vike-secrets/tests/migration/database/venue_id.rs`'s
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
/// re-creates a table from the shipped [`crate::schema::DDL`] — which carries `venue_id` (and
/// carried the text `venue` beside it until the venue-links plan's second release). So an UNARMED
/// plant would be normalized before the loop ever reached it, the
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
    // Rebuild `credential` WITHOUT `venue_id` (and without the text `venue` the shipped table
    // carried until the venue-links plan's second release), keeping every other schema-2 column —
    // `ALTER TABLE … DROP COLUMN venue_id` is refused outright (SQLite will not drop a column a
    // `CHECK` constraint references, and `credential`'s own `CHECK (account_id IS NULL OR venue_id
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
/// `venue_id` comes out of one write carrying it. The rebuild above the loop is therefore a wider
/// repair than the `ALTER TABLE` it precedes, and the loop's guard is left governing only the
/// tables the rebuild does not touch.
///
/// ⚠ **It normalizes onto the SHIPPED column set, and that set changed under this test.** It said
/// the store came out "carrying both" `venue` and `venue_id` until the venue-links plan's second
/// release took the text `venue` out of the shipped `credential`; the same rebuild now hands the
/// table `venue_id` and no text column, which is asserted rather than left implied.
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
        crate::settings::has_column(&tx, "credential", "venue_id").expect("check"),
        "the rebuild re-creates the table from the shipped DDL, so the column the plant lacked \
             comes back"
    );
    assert!(
        !crate::settings::has_column(&tx, "credential", "venue").expect("check"),
        "…and only the shipped set comes back: the text `venue` left the shipped `credential` \
             with the venue-links plan's second release"
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

/// **The title seed's own shape**, which `crates/vike-secrets/tests/accounts/venue_titles.rs`'s owner-spelling
/// pin cannot see through the store: every key is on the roster, no key is seeded twice, and every
/// roster venue has a seed. A key seeded twice would make a store's title depend on how it was
/// created — the insert path takes the FIRST row (`venue_title_seed`) while the column-add path runs
/// every row in order, so the LAST one wins — and a key off the roster would be a row nothing ever
/// inserts or fills.
#[test]
fn the_title_seed_names_each_roster_venue_exactly_once_and_nothing_else() {
    let mut seen = std::collections::BTreeSet::new();
    for (name, title) in VENUE_TITLES {
        assert!(seen.insert(*name), "`{name}` is seeded twice in `VENUE_TITLES`");
        assert!(
            vike_model::VENUES.contains(name),
            "`{name}` is not on the roster, so nothing would ever insert or fill it"
        );
        assert!(!title.trim().is_empty(), "`{name}` is seeded with an empty spelling");
    }
    for venue in vike_model::VENUES {
        assert!(venue_title_seed(venue).is_some(), "roster venue `{venue}` has no seed spelling");
    }
}
