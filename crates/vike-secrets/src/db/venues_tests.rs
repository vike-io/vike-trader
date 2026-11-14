//! Tests of `venues.rs`: `ensure_venue_rows` and the title seed.

use super::venues::{VENUE_TITLES, venue_title_seed};
use super::*;

/// ⚠ **THE CRITICAL CASE: a store already at [`SCHEMA_VERSION`] that PREDATES the `venue`
/// table.** Not hypothetical — the root `CLAUDE.md` records both the CI box and the dev box migrating
/// to schema 2 on 2026-09-14, so both are in exactly this shape until a writer tops them up.
/// [`open_for_write`] only runs `DDL` when it CREATES the file, so it never re-runs `DDL` for a
/// store that is already current, and without [`ensure_venue_rows`] re-running the whole batch
/// itself, its `INSERT OR IGNORE` hits a bare `no such table: venue` on the very next credential
/// write.
///
/// Simulated by DROPPING the table a normal create already made, rather than by hand-writing an
/// older DDL: the failure mode under test is "this table is simply missing from an otherwise
/// current store," which is exactly what dropping it produces, with no risk of the fixture
/// silently drifting from the real historical schema-2 shape.
#[test]
fn ensure_venue_rows_creates_the_table_on_a_store_that_predates_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (mut conn, created) = open_for_write(&db).expect("open");
    assert!(created);
    conn.execute_batch("DROP TABLE venue;").expect("simulate a pre-venue-table schema-2 store");

    let tx = conn.transaction().expect("tx");
    ensure_venue_rows(&tx).expect("must self-heal rather than fail with `no such table: venue`");
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM venue", [], |r| r.get(0)).expect("count");
    assert_eq!(count as usize, vike_model::VENUES.len());
}

/// **The top-up is idempotent WITHIN one transaction, independently of anything [`create_store`]
/// does around it.** [`create_store`]'s own doc says a run over an existing store opens no write
/// connection at all, so an integration test that re-runs `create_store()` unchanged never reaches
/// [`ensure_venue_rows`] a second time and cannot tell `INSERT OR IGNORE` from a plain `INSERT`.
/// This calls the crate-private function directly, twice, over one transaction — which no
/// integration test in `tests/` can do.
#[test]
fn ensure_venue_rows_is_idempotent_within_one_transaction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("db").join("vike.db");
    let (mut conn, created) = open_for_write(&db).expect("open");
    assert!(created);

    let tx = conn.transaction().expect("tx");
    ensure_venue_rows(&tx).expect("first top-up");
    ensure_venue_rows(&tx).expect("a second top-up must not duplicate a single row");
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM venue", [], |r| r.get(0)).expect("count");
    assert_eq!(count as usize, vike_model::VENUES.len());
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
