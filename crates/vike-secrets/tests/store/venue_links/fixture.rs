//! The planted stores and helpers more than one file in this module shares.

use super::*;
use crate::support;

/// A store with rows in every linked table: the fixture's accounts (binance demo and live,
/// dukascopy demo), one VENUE-scoped credential and one machine-scoped venue setting.
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
    fx
}

/// The store's settings rows, through the public read-only reader.
pub(super) fn settings_rows(fx: &Fixture) -> StoredSettings {
    match vike_secrets::read_settings(&fx.db()).expect("the settings read") {
        SettingsSource::Rows { rows, .. } => rows,
        other => panic!("the planted store has its settings tables: {other}"),
    }
}
