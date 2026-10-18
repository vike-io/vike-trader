//! **The credential FIELDS that arm each roster venue's arm** - the arming parity golden's fixtures
//! (`tests/trade_roster/arming_parity.rs`, which `#[path]`-includes this file). The tradehub's `vike-backend
//! venues` golden (`crates/vike-tradehub/src/arming_fixtures_tests.rs`) records the SAME sets; the two files
//! name the same fields because each side of the wire holds its own golden. One row per venue with a live
//! arm; the sets are the ones `tests/trade_roster/mount_roster/preconnect.rs`'s `live_arming_cases` plants
//! (the tier column is dropped: the parity matrix drives EVERY tier).
#![allow(dead_code)]

/// The complete credential set that arms `venue`'s arm; empty for an id with no live arm.
pub(crate) fn armed_fields(venue: &str) -> &'static [(&'static str, &'static str)] {
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
        // vike:new-venue:row // TODO(new-venue: {venue}): the credential fields that arm its bridge's `resolve`, so both parity goldens drive it (then re-record the goldens in `crates/vike-gatewayhub/tests/trade_roster/arming_parity`)
        // vike:new-venue:row "{venue}" => &[],
        _ => &[],
    }
}
