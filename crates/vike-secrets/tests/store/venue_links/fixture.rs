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
/// ⚠ **A table with no text `venue` is skipped, and on the shipped shape that is three of the
/// four.** `account`, `credential` and `venue_setting` carry `venue_id` alone, so there is nothing
/// in them to lie with: no statement can read a venue there but by the number. The tests built on
/// this prove the property through `venue_arming`, the one table that keeps a text column. The probe
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
/// classifier has never seen, written through the one credential writer with a classifier layered
/// over `support::classify` that answers `Placement::Venue` for it alone. The shared classifier
/// answers only `Account` or `Infrastructure`, and `credential`'s `CHECK (account_id IS NULL OR
/// venue_id IS NULL)` means an account-scoped row never carries a venue — so without this row the
/// `credential` table holds no venue link at all.
pub(super) fn planted() -> Fixture {
    let fx = Fixture::seeded();
    let venue_key = "CTRADER_CLIENT_ID";
    let classify = support::classifier_over(
        [support::Rule::exact(venue_key).venue("ctrader").field("CLIENT_ID")],
        support::classify,
    );
    fx.add_key(venue_key, &classify);
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
