//! **The credential FIELDS that arm each roster venue's arm** — the `vike-backend venues` golden's fixtures
//! (`src/venues_cli_parity_tests.rs`). The arming parity golden moved with the venue half
//! (`crates/vike-gatewayhub/tests/trade_roster/arming_parity.rs`) and keeps its own twin of the field sets
//! (`crates/vike-gatewayhub/tests/trade_roster/support/arming_fixtures.rs`), so each side of the wire holds
//! the SAME fixtures. One row per venue with a live arm; the sets are the ones
//! `crates/vike-gatewayhub/tests/trade_roster/mount_roster/preconnect.rs`'s `live_arming_cases` plants (the
//! tier column is dropped: the parity matrix drives EVERY tier).
#![allow(dead_code)]

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::AdapterPolicy;
use vike_venues::credential_schema::{FieldOwner, schema_for};

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
        // vike:new-venue:row // TODO(new-venue: {venue}): the credential fields that arm its bridge's `resolve`, so both parity goldens drive it (then re-record the `venues` golden in `crates/vike-tradehub/tests/arming_parity` and the arming golden in `crates/vike-gatewayhub/tests/trade_roster/arming_parity`)
        // vike:new-venue:row "{venue}" => &[],
        _ => &[],
    }
}

/// Plant one ACTIVE row `(venue, label)` at `tier` holding `fields` — each FIELD filed against the
/// owner the venue's schema row names (cTrader's OAuth application pair is the VENUE's, every other
/// field the ACCOUNT's), the filing `tests/support/account_book.rs`'s `with_keyed_account` does.
pub(crate) fn keyed_account(
    policy: AdapterPolicy,
    venue: &str,
    label: &AccountLabel,
    tier: AccountTier,
    fields: &[(&str, &str)],
) -> AdapterPolicy {
    let is_venue_field = |field: &str| {
        schema_for(venue).and_then(|s| s.field(field)).is_some_and(|f| f.owner == FieldOwner::Venue)
    };
    let account_fields: Vec<(&str, &str)> =
        fields.iter().copied().filter(|(f, _)| !is_venue_field(f)).collect();
    let venue_fields: Vec<(&str, &str)> =
        fields.iter().copied().filter(|(f, _)| is_venue_field(f)).collect();
    policy
        .with_credentials(venue, label, tier, &account_fields)
        .with_venue_credentials(venue, &venue_fields)
}
