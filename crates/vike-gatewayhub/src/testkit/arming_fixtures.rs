//! **The credential FIELDS that arm each roster venue's arm** - the arming parity golden's fixtures
//! (`crates/vike-gatewayhub/tests/trade_roster/arming_parity.rs`, and the tradehub's
//! `crates/vike-tradehub/tests/hop/venue_backend_parity.rs`), compiled once here, in the testkit, for both
//! crates (a crate below the gatewayhub would have to spell the venue roster in its own `src/`, which
//! `vike-tradehub-core`'s decision-0096 gate refuses). The tradehub's `vike-backend venues` golden
//! (`crates/vike-tradehub/src/venues_cli_parity_tests.rs`) plants the SAME sets from here (it kept a twin
//! until 2026-10-31). One row per venue with a live arm; the sets are the ones
//! `crates/vike-gatewayhub/tests/trade_roster/mount_roster/preconnect.rs`'s `live_arming_cases` plants
//! (the tier column is dropped: the parity matrix drives EVERY tier). Both parity binaries hand these to
//! `vike_tradehub_core::test_support::arming_parity`, which renders the matrix and may spell no venue.

use vike_model::AccountTier;
use vike_secrets::VenueSettingRow;
use vike_secrets::venue_setting::VenueSettings;

/// The complete credential set that arms `venue`'s arm; empty for an id with no live arm.
pub fn armed_fields(venue: &str) -> &'static [(&'static str, &'static str)] {
    match venue {
        "binance" | "bybit" | "deribit" => &[("API_KEY", "k"), ("API_SECRET", "s")],
        "okx" => &[("API_KEY", "k"), ("API_SECRET", "s"), ("API_PASSPHRASE", "p")],
        "aster" => &[("USER", "0xUser"), ("PRIVATE_KEY", "0xkey")],
        "hyperliquid" => &[("PRIVATE_KEY", "0xkey")],
        "ctrader" => &[
            ("CLIENT_ID", "id"),
            ("CLIENT_SECRET", "sec"),
            ("ACCESS_TOKEN", "at"),
            ("REFRESH_TOKEN", "rt"),
        ],
        "alpaca" => &[("CLIENT_ID", "id"), ("CLIENT_SECRET", "sec"), ("ACCOUNT_ID", "acct")],
        "ig" => &[("API_KEY", "k"), ("IDENTIFIER", "u"), ("PASSWORD", "p")],
        "oanda" => &[("API_KEY", "k"), ("ACCOUNT_ID", "a")],
        "dukascopy" => &[("LOGIN", "u"), ("PASSWORD", "p")],
        "ibkr" => &[("ACCOUNT", "DU123456")],
        "fxcm" => &[("USER", "u"), ("PASSWORD", "p")],
        "polymarket" => &[("PRIVATE_KEY", "0xkey")],
        // vike:new-venue:row // TODO(new-venue: {venue}): the credential fields that arm its bridge's `resolve`, so every arming golden drives it (then re-record the arming golden in `crates/vike-gatewayhub/tests/trade_roster/arming_parity` and the `venues` golden in `crates/vike-tradehub/tests/arming_parity`)
        // vike:new-venue:row "{venue}" => &[],
        _ => &[],
    }
}

/// The venue settings a venue's live arm reads its backend from, one row per venue whose live arm
/// holds every ORDER (ibkr's cpapi backend: `HeldBelowLive::OrdersHeld`, decision 0140 stage A); `None`
/// for every other venue.
pub fn live_backend_settings(venue: &str) -> Option<VenueSettings> {
    let (field, value) = match venue {
        "ibkr" => ("BACKEND", "cpapi"),
        _ => return None,
    };
    let row = [VenueSettingRow {
        venue: venue.to_string(),
        tier: Some(AccountTier::Live),
        field: field.to_string(),
        value: value.to_string(),
    }];
    Some(VenueSettings::from_rows(venue, &row))
}
