//! The planted stores and helpers more than one file in this module shares.

use super::*;
use crate::support;

/// Rewrite every text `venue` cell of the four linked tables to `text-lies-<table>-<id>`.
/// `venue_id` is untouched, so a reader that still answers `binance` read the number.
///
/// ⚠ Each table must have at least one cell to rewrite, asserted rather than assumed: a table the
/// fixture left empty would make every "the reader ignored the lie" claim about it vacuous, which is
/// how this helper rewrote zero `credential` cells until [`planted`] filed a venue-scoped one.
///
/// ⚠ **The lie names its TABLE as well as its row, and it named only the row until review.** Two
/// tables' rows share an id often — the binance `demo` account and the binance `demo` arming row
/// are both id 1 in [`planted`] — so `text-lies-<id>` told the same lie twice, and a statement that
/// JOINED or MATCHED two tables by their text agreed with itself: the arming fold reverted to the
/// text on BOTH of its reads still armed account 1 under arming row 1's `text-lies-1`, and
/// `the_arming_fold_reads_both_tables_by_venue_id` stayed green over a fold that read no number at
/// all. A table-specific lie cannot agree across tables.
///
/// ⚠ **A table with no text `venue` is skipped, and since the plan's second release that is three
/// of the four.** `account`, `credential` and `venue_setting` lost the column, so there is nothing
/// in them to lie with: no statement can read a venue there but by the number. The tests built on
/// this keep running unchanged and prove the property through `venue_arming`, the one table that
/// keeps a text column until Plan B deletes it, and through the absence of any text column
/// elsewhere (`the_contracting_write_removes_the_text_column_and_readers_still_answer`). The probe
/// asks the engine, so a store whose text column came back would be lied to again.
pub(super) fn falsify_the_text_column(fx: &Fixture) {
    let conn = fx.conn();
    for table in ["account", "credential", "venue_setting", "venue_arming"] {
        if !has_text_venue(&conn, table) {
            continue;
        }
        let rewritten = conn
            .execute(
                &format!(
                    "UPDATE {table} SET venue = 'text-lies-{table}-' || id WHERE venue IS NOT NULL"
                ),
                [],
            )
            .unwrap_or_else(|e| panic!("falsify `{table}`: {e}"));
        assert!(
            rewritten > 0,
            "`{table}` had no text venue to falsify; the lie would prove nothing"
        );
    }
}

/// A store with rows in every linked table: the fixture's accounts (binance demo and live,
/// dukascopy demo), one VENUE-scoped credential, one machine-scoped venue setting and one venue
/// arming row.
///
/// ⚠ The venue-scoped credential is filed the way `tests/venue_table.rs` files one: a key the shared
/// classifier has never seen, appended to the file store and migrated with a classifier layered
/// over `support::classify` that answers `Placement::Venue` for it alone. The shared classifier
/// answers only `Account` or `Infrastructure`, and `credential`'s `CHECK (account_id IS NULL OR
/// venue_id IS NULL)` (over the text `venue` until the plan's second release) means an
/// account-scoped row never carries a venue — so without this row the `credential` table holds no
/// venue link at all.
pub(super) fn planted() -> Fixture {
    let fx = Fixture::migrated();
    let venue_key = "CTRADER_CLIENT_ID";
    fx.append_key(venue_key);
    let classify = support::classifier_over(
        [support::Rule::exact(venue_key).venue("ctrader").field("CLIENT_ID")],
        support::classify,
    );
    fx.migrate_with(support::is_node_key, &classify);
    vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "false")
        .expect("a venue setting");
    vike_secrets::write_settings_in(
        fx.dir(),
        &StoredSettings {
            arming: vec![ArmingRow {
                venue: "binance".to_string(),
                label: None,
                mode: "demo".to_string(),
                max_exposure: None,
            }],
            ..Default::default()
        },
    )
    .expect("an arming row");
    fx
}

/// The store's settings rows, through the public read-only reader.
pub(super) fn settings_rows(fx: &Fixture) -> StoredSettings {
    match vike_secrets::read_settings(&fx.db()).expect("the settings read") {
        SettingsSource::Rows { rows, .. } => rows,
        other => panic!("the planted store has its settings tables: {other}"),
    }
}

/// [`vike_secrets::live_means_mainnet::apply_live_means_mainnet`], the boot path's entry, as a test
/// root calls it.
pub(super) fn apply_0095(fx: &Fixture) -> vike_secrets::live_means_mainnet::LiveMeansMainnet {
    try_apply_0095(fx).expect("the boot-path migration")
}

pub(super) fn try_apply_0095(
    fx: &Fixture,
) -> Result<vike_secrets::live_means_mainnet::LiveMeansMainnet, vike_secrets::DbError> {
    vike_secrets::live_means_mainnet::apply_live_means_mainnet(
        fx.dir(),
        vike_model::change_journal::Actor::cli("test"),
        vike_model::change_journal::Proc::new("test", 1, "0.0.0"),
        1,
    )
}

/// Plant a store on `ddl` the way `support::sql::plant_ddl` does (the one preamble every aged-store
/// test follows), with the whole roster in `venue`.
pub(super) fn plant(ddl: &str) -> (Fixture, rusqlite::Connection) {
    let fx = Fixture::file_store();
    let conn = plant_ddl(&fx.db(), ddl);
    for (i, venue) in vike_model::VENUES.iter().enumerate() {
        conn.execute("INSERT INTO venue (id, name) VALUES (?1, ?2)", (i as i64 + 1, venue))
            .expect("roster");
    }
    (fx, conn)
}

/// Plant the store BOTH live boxes hold today, the shipped `DDL` with this plan's first release
/// reverted, and give it rows in every linked table.
pub(super) fn planted_on_the_old_shape() -> Fixture {
    let (fx, conn) = plant(&vike_secrets::venue_links::pre_venue_link_ddl());
    conn.execute_batch(
        "INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (1, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'demo');
         INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (2, 'binance', (SELECT id FROM venue WHERE name = 'binance'), 'live');
         INSERT INTO venue_setting (venue, venue_id, tier, field, value) \
             VALUES ('polymarket', (SELECT id FROM venue WHERE name = 'polymarket'), 'any', \
                     'PROXY_ENABLED', 'false');
         INSERT INTO venue_arming (venue, venue_id, label, mode) \
             VALUES ('binance', (SELECT id FROM venue WHERE name = 'binance'), NULL, 'demo');",
    )
    .expect("rows");
    stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);
    fx
}

/// The one `venue_arming` row of `fx`, raw: `(venue text, venue_id, mode)`.
pub(super) fn the_arming_row(fx: &Fixture) -> (String, Option<i64>, String) {
    let conn = fx.conn();
    conn.query_row("SELECT venue, venue_id, mode FROM venue_arming", [], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })
    .expect("exactly one arming row")
}

/// Whether decision 0095's marker row is on the store.
pub(super) fn marked_0095(fx: &Fixture) -> bool {
    let conn = fx.conn();
    conn.query_row(
        "SELECT COUNT(*) FROM store_migration WHERE name = ?1",
        [vike_secrets::live_means_mainnet::LIVE_MEANS_MAINNET],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// The release-before shape with one `account` row naming a venue the roster lacks — the store
/// trap 7 refuses every write on.
pub(super) fn with_an_off_roster_account() -> Fixture {
    let fx = planted_on_the_old_shape();
    let conn = fx.conn();
    conn.execute(
        "INSERT INTO account (id, venue, venue_id, tier) VALUES (7, 'no-such-venue', NULL, 'demo')",
        [],
    )
    .expect("plant the unresolvable row");
    fx
}
