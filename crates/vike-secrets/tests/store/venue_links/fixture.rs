//! The planted stores and helpers more than one file in this module shares.

use super::*;

/// A store with rows in every linked table: the fixture's accounts (binance demo and live,
/// dukascopy demo), one VENUE-scoped credential and one machine-scoped venue setting.
///
/// ⚠ The venue-scoped credential is cTrader's application `CLIENT_ID`, written through the
/// owner-addressed writer (`CredentialOwner::Venue`). The shared fixture files its keys against
/// accounts and as a deployment row, and `credential`'s `CHECK (account_id IS NULL OR venue_id IS
/// NULL)` means an account-scoped row never carries a venue — so without this row the `credential`
/// table holds no venue link at all.
pub(super) fn planted() -> Fixture {
    let fx = Fixture::seeded();
    fx.attach_venue("ctrader", &["CLIENT_ID"]);
    vike_secrets::set_venue_setting_in_unjournalled(
        fx.dir(),
        "polymarket",
        None,
        "PROXY_ENABLED",
        "false",
    )
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
