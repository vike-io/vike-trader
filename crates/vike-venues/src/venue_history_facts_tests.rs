//! The two history-fact tables, pinned verbatim over the roster.

use super::*;
use vike_model::VENUES;

/// **What a stored volume counts, verbatim**: an inverse candle counts the coin on binance, okx and deribit, and a
/// linear derivative's print counts an instrument size on okx alone; every other row is the order's unit.
#[test]
fn the_stored_volume_matrix_is_pinned_verbatim() {
    #[rustfmt::skip]
    const MATRIX: &[(&str, bool, bool)] = &[
        ("binance",     true,  false),
        ("bybit",       false, false),
        ("okx",         true,  true),
        ("deribit",     true,  false),
        ("oanda",       false, false),
        ("ig",          false, false),
        ("fxcm",        false, false),
        ("dukascopy",   false, false),
        ("polymarket",  false, false),
        ("ibkr",        false, false),
        ("ctrader",     false, false),
        ("alpaca",      false, false),
        ("aster",       false, false),
        ("hyperliquid", false, false),
        // vike:new-venue:row ("{venue}", false, false), // TODO(new-venue: {venue}): move with the row above
    ];
    assert_eq!(MATRIX.len(), VENUES.len(), "the pin names every roster venue once");
    let fallback = stored_volume_for("no-such-venue");
    for &(venue, coin, instrument_size) in MATRIX {
        assert!(VENUES.contains(&venue), "{venue} is not a roster venue");
        let row = stored_volume_for(venue);
        assert_eq!(row.inverse_candles_count_the_coin, coin, "{venue}");
        assert_eq!(row.linear_prints_count_an_instrument_size, instrument_size, "{venue}");
        assert_ne!(row, fallback, "{venue} rides the unknown-venue fallback");
        assert!(row.why.len() > 30, "{venue}: a row without a reason");
    }
    assert!(
        !fallback.inverse_candles_count_the_coin
            && !fallback.linear_prints_count_an_instrument_size
    );
}

/// **Who publishes a funding history, verbatim, and how a point is owed**: binance, hyperliquid, bybit, okx, deribit and
/// aster publish one, each point settled at its timestamp except deribit's, which accrues over the hour ending there.
#[test]
fn the_funding_history_matrix_is_pinned_verbatim() {
    #[rustfmt::skip]
    const MATRIX: &[(&str, bool, Option<i64>)] = &[
        ("binance",     true,  None),
        ("bybit",       true,  None),
        ("okx",         true,  None),
        ("deribit",     true,  Some(3_600_000)),
        ("oanda",       false, None),
        ("ig",          false, None),
        ("fxcm",        false, None),
        ("dukascopy",   false, None),
        ("polymarket",  false, None),
        ("ibkr",        false, None),
        ("ctrader",     false, None),
        ("alpaca",      false, None),
        ("aster",       true,  None),
        ("hyperliquid", true,  None),
        // vike:new-venue:row ("{venue}", false, None), // TODO(new-venue: {venue}): move with the row above
    ];
    assert_eq!(MATRIX.len(), VENUES.len(), "the pin names every roster venue once");
    let fallback = funding_history_for("no-such-venue");
    for &(venue, publishes, accrual) in MATRIX {
        assert!(VENUES.contains(&venue), "{venue} is not a roster venue");
        let row = funding_history_for(venue);
        assert_eq!(row.publishes_history, publishes, "{venue}");
        assert_eq!(row.accrual_period_ms, accrual, "{venue}");
        assert_ne!(row, fallback, "{venue} rides the unknown-venue fallback");
        assert!(row.why.len() > 30, "{venue}: a row without a reason");
        assert!(
            publishes || accrual.is_none(),
            "{venue}: an accrual period of a history it does not publish"
        );
    }
    assert_eq!((fallback.publishes_history, fallback.accrual_period_ms), (false, None));
}
