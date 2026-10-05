//! The settings database names each venue the way the venue spells itself — the owner's ruling of
//! 2026-09-30. The names are WRITTEN ONCE, by the migration that adds the column (or by the insert
//! that adds a roster venue the store does not hold yet), and nothing rewrites a title afterwards,
//! so an operator's own spelling stays.

use std::collections::{BTreeMap, BTreeSet};

mod support;
use support::Fixture;

fn titles(fx: &Fixture) -> BTreeMap<String, Option<String>> {
    vike_secrets::read_venues_in(fx.dir())
        .expect("the venue read")
        .into_iter()
        .map(|v| (v.name, v.title))
        .collect()
}

/// **The owner's spellings, every one, written out.** A venue's title is the way the venue spells
/// itself (the ruling of 2026-09-30): `cTrader`, not `CTRADER`; `Binance`, not `BINANCE`; and
/// `Interactive Brokers` for `ibkr`. The seed in `vike_secrets::db` is private, so this table is the
/// pin that sees it through the store: a spelling edited there, a venue dropped from it or one seeded
/// twice turns a test below red.
///
/// ⚠ A venue added to the roster reddens `the_owners_spellings_name_each_roster_venue_exactly_once`
/// until its row is here: that is the point, and `just new-venue` renders a placeholder row into
/// this table and the seed together so neither is forgotten.
#[rustfmt::skip]
const OWNER_SPELLINGS: &[(&str, &str)] = &[
    ("binance", "Binance"),
    ("bybit", "Bybit"),
    ("okx", "OKX"),
    ("deribit", "Deribit"),
    ("oanda", "OANDA"),
    ("ig", "IG"),
    ("fxcm", "FXCM"),
    ("dukascopy", "Dukascopy"),
    ("polymarket", "Polymarket"),
    ("ibkr", "Interactive Brokers"),
    ("ctrader", "cTrader"),
    ("alpaca", "Alpaca"),
    ("aster", "Aster"),
    ("hyperliquid", "Hyperliquid"),
    // vike:new-venue:row ("{venue}", "TODO(new-venue: {venue}): the venue's own spelling"),
];

/// [`OWNER_SPELLINGS`] as the map a store's venue read answers with.
fn owner_spellings() -> BTreeMap<String, Option<String>> {
    OWNER_SPELLINGS
        .iter()
        .map(|(venue, title)| (venue.to_string(), Some(title.to_string())))
        .collect()
}

/// The pin is well-formed: no venue is spelled twice, every key is on the roster, and every roster
/// venue has a spelling. Without the last two the pin could quietly cover fewer venues than the
/// store seeds, and every comparison below would pass over the gap.
#[test]
fn the_owners_spellings_name_each_roster_venue_exactly_once() {
    let mut seen = BTreeSet::new();
    for (venue, _) in OWNER_SPELLINGS {
        assert!(seen.insert(*venue), "`{venue}` is spelled twice");
        assert!(
            vike_model::VENUES.contains(venue),
            "`{venue}` is not on the roster `vike_model::venues::VENUES`"
        );
    }
    for venue in vike_model::VENUES {
        assert!(seen.contains(venue), "roster venue `{venue}` has no owner spelling written here");
    }
}

/// A migrated store holds EXACTLY the owner's spellings: every venue, verbatim, and nothing on the
/// roster left untitled.
#[test]
fn a_migrated_store_names_every_roster_venue_by_its_own_spelling() {
    let fx = Fixture::migrated();
    let held = titles(&fx);
    for venue in vike_model::VENUES {
        assert!(
            held.get(*venue).cloned().flatten().is_some(),
            "`{venue}` has no title — add its spelling to `VENUE_TITLES`"
        );
    }
    assert_eq!(held, owner_spellings(), "the seed is not the owner's spelling table");
}

#[test]
fn a_title_the_operator_changed_survives_every_later_write() {
    let fx = Fixture::migrated();
    {
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
        conn.execute("UPDATE venue SET title = 'Binance (main box)' WHERE name = 'binance'", [])
            .expect("the operator's own spelling");
    }
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("a later write through the funnel");
    assert_eq!(titles(&fx)["binance"].as_deref(), Some("Binance (main box)"));
}

#[test]
fn a_store_without_the_column_gains_it_and_its_titles_on_the_next_write() {
    let fx = Fixture::migrated();
    {
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
        conn.execute_batch("ALTER TABLE venue DROP COLUMN title;").expect("age the store");
    }
    assert_eq!(titles(&fx)["okx"], None, "an aged store answers no title, never an error");
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("the write that adds the column");
    // The column-add path fills the rows the store already holds, and a roster venue's insert takes
    // the same seed through a different statement: both must come out as the owner's table.
    assert_eq!(titles(&fx), owner_spellings(), "the column-add path seeded other spellings");
}

/// **NULL stays NULL.** A title somebody cleared is never refilled from the seed: the seed is read in
/// exactly two moments — when the column is added, and when a roster venue's row is inserted — and
/// a later write through the funnel does neither for a row that already exists. A reader shows the
/// venue's key for it. This is deliberate (a migration that rewrote cleared titles would be the
/// rewrite-on-every-write the owner ruled out), and it was unpinned.
#[test]
fn a_title_the_operator_cleared_is_never_refilled() {
    let fx = Fixture::migrated();
    {
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
        conn.execute("UPDATE venue SET title = NULL WHERE name = 'binance'", [])
            .expect("clear one title");
    }
    let before = titles(&fx);
    assert_eq!(before["binance"], None, "the fixture holds what this test says it holds");
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("a later write through the funnel");
    let held = titles(&fx);
    assert_eq!(held["binance"], None, "a cleared title was refilled from the seed");
    // Compared with what the store held, not with the owner's spellings: this test is about a write
    // leaving titles alone, and a spelling edit in the seed is another test's business.
    assert_eq!(held, before, "a later write changed a title it had no business touching");
}

// The reader's two EMPTY answers. A node with no store has no venues to name, and an error here
// would be a startup failure on a fresh install, so each is pinned: no database at all, and a
// schema-1 store (readable by design, and older than the `venue` table).

#[test]
fn a_node_with_no_database_names_no_venue_and_a_read_creates_none() {
    let fx = Fixture::file_store();
    assert!(!fx.db().exists(), "the fixture must start without a database");
    let rows = vike_secrets::read_venues_in(fx.dir()).expect("an absent database is not an error");
    assert!(rows.is_empty(), "a node with no store has no venues to name: {rows:?}");
    assert!(!fx.db().exists(), "a read must never create the database it was asked about");
}

#[test]
fn a_store_older_than_the_venue_table_names_no_venue() {
    let fx = Fixture::file_store();
    vike_secrets::plant_schema_1(&fx.db(), &[], &[]).expect("plant a schema-1 store");
    {
        let conn = rusqlite::Connection::open(fx.db()).expect("open");
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'venue'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(tables, 0, "the plant must predate the `venue` table, or this proves nothing");
    }
    let rows = vike_secrets::read_venues_in(fx.dir()).expect("an older store is readable");
    assert!(rows.is_empty(), "schema 1 has no `venue` table, so it names no venue: {rows:?}");
}
