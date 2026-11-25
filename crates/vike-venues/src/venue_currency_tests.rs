//! The currency table, pinned verbatim over the roster and in today's order.

use super::*;
use vike_model::VENUES;

/// **Every row, verbatim**, as the hub code it replaced decided it (B3.01): the rate source was the tradehub's
/// `QUOTE_CONVERTING_VENUES` (`OtherSymbols`) and its `MOUNT_CONVERTING_VENUES` subset (`OtherSymbolsAndMount`); a
/// budgeted read its `reads_currency_on_ask` and the gatewayhub's twin (ibkr, ig); a required stamp its
/// `rate_must_state_its_ledger` (ibkr); an unnamed balance its `balance_carries_no_currency` (oanda, ig, fxcm); the
/// engine's money is the door's ibkr arm and its dukascopy|fxcm pair arm; the minor units are ibkr's gate over the
/// model's table; the settle coins were bybit's two-coin list and hyperliquid's perp quote.
#[test]
fn the_venue_currency_matrix_is_pinned_verbatim() {
    use VenueBalanceCurrency::{Named, Unnamed};
    use VenueCurrencyRead::{Budgeted, Free};
    use VenueEngineMoney::{Ledger, MountedContract, MountedPairQuote};
    use VenueQuoteConversion::{OtherSymbols, OtherSymbolsAndMount};
    use VenueRateLedgerStamp::{Optional, Required};
    const NO: VenueQuoteConversion = VenueQuoteConversion::None;
    type Row = (
        &'static str,
        VenueQuoteConversion,
        VenueCurrencyRead,
        VenueRateLedgerStamp,
        VenueBalanceCurrency,
        VenueEngineMoney,
        bool,
        &'static [&'static str],
    );
    // (venue, rate source, currency read, rate stamp, balance, engine money, the model's minor units, settle coins)
    #[rustfmt::skip]
    const MATRIX: &[Row] = &[
        ("oanda",       OtherSymbolsAndMount, Free,     Optional, Unnamed, Ledger,           false, &[]),
        ("ctrader",     OtherSymbolsAndMount, Free,     Optional, Named,   Ledger,           false, &[]),
        ("ibkr",        OtherSymbols,         Budgeted, Required, Named,   MountedContract,  true,  &[]),
        ("dukascopy",   OtherSymbolsAndMount, Free,     Optional, Named,   MountedPairQuote, false, &[]),
        ("ig",          OtherSymbolsAndMount, Budgeted, Optional, Unnamed, Ledger,           false, &[]),
        ("fxcm",        OtherSymbolsAndMount, Free,     Optional, Unnamed, MountedPairQuote, false, &[]),
        ("binance",     NO,                   Free,     Optional, Named,   Ledger,           false, &[]),
        ("bybit",       NO,                   Free,     Optional, Named,   Ledger,           false, &["USDT", "USDC"]),
        ("okx",         NO,                   Free,     Optional, Named,   Ledger,           false, &[]),
        ("deribit",     NO,                   Free,     Optional, Named,   Ledger,           false, &[]),
        ("polymarket",  NO,                   Free,     Optional, Named,   Ledger,           false, &[]),
        ("alpaca",      NO,                   Free,     Optional, Named,   Ledger,           false, &[]),
        ("aster",       NO,                   Free,     Optional, Named,   Ledger,           false, &[]),
        ("hyperliquid", NO,                   Free,     Optional, Named,   Ledger,           false, &["USDC"]),
        // vike:new-venue:row ("{venue}", NO, Free, Optional, Named, Ledger, false, &[]), // TODO(new-venue: {venue}): move with its row in venue_currency.rs
    ];
    assert_eq!(MATRIX.len(), VENUES.len(), "the pin names every roster venue once");
    for venue in VENUES {
        assert!(MATRIX.iter().any(|row| row.0 == *venue), "{venue} has no pinned row");
    }
    assert_eq!(
        VENUE_CURRENCY.iter().map(|row| row.venue).collect::<Vec<_>>(),
        MATRIX.iter().map(|row| row.0).collect::<Vec<_>>(),
        "the table's rows, in the pin's order"
    );
    for &(venue, quote, read, stamp, balance, money, minor, settle) in MATRIX {
        let row = venue_currency_for(venue);
        let minor_units: &[MinorUnit] = if minor { &MINOR_UNITS } else { &[] };
        assert_eq!(
            (row.venue, row.quote_conversion, row.currency_read, row.rate_ledger_stamp),
            (venue, quote, read, stamp),
            "{venue}"
        );
        assert_eq!((row.balance_currency, row.engine_money), (balance, money), "{venue}");
        assert_eq!((row.minor_units, row.settle_coins), (minor_units, settle), "{venue}");
        assert!(!row.why.is_empty(), "{venue}: a roster row says why");
        assert_ne!(
            *row,
            VenueCurrencyRow::NONE,
            "{venue}: a roster row is NAMED, not the fallback"
        );
    }
}

/// **The rate-source venues keep their order**: the door's refusal sentence
/// (`crates/vike-tradehub/src/server/symbol_admission.rs`'s `quote_conversion_refusal`) lists them from this table,
/// in the bytes it printed before B3.
#[test]
fn the_rate_source_venues_keep_todays_order() {
    let rate_sources: Vec<&str> = VENUE_CURRENCY
        .iter()
        .filter(|row| row.quote_conversion != VenueQuoteConversion::None)
        .map(|row| row.venue)
        .collect();
    assert_eq!(rate_sources, ["oanda", "ctrader", "ibkr", "dukascopy", "ig", "fxcm"]);
    assert_eq!(rate_sources.join(", "), "oanda, ctrader, ibkr, dukascopy, ig, fxcm");
}

/// **An unknown venue string answers the fallback**: no rate, a free read, an optional stamp, a named balance, the
/// ledger's money, nothing else.
#[test]
fn an_unknown_venue_answers_the_none_row() {
    let row = venue_currency_for("no-such-venue");
    assert_eq!(*row, VenueCurrencyRow::NONE);
    assert_eq!(row.quote_conversion, VenueQuoteConversion::None);
    assert_eq!(row.currency_read, VenueCurrencyRead::Free);
    assert_eq!(row.rate_ledger_stamp, VenueRateLedgerStamp::Optional);
    assert_eq!(row.balance_currency, VenueBalanceCurrency::Named);
    assert_eq!(row.engine_money, VenueEngineMoney::Ledger);
    assert!(row.minor_units.is_empty() && row.settle_coins.is_empty());
    assert_eq!(
        venue_currency_for(""),
        &VenueCurrencyRow::NONE,
        "the empty string is no venue either"
    );
}
