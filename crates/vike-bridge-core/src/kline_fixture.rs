//! The bars the committed kline fixtures map to (`test-support` only).
//!
//! binance's, bybit's and okx's `tests/fixtures/*_klines_btcusdt_1m.json` carry the same three BTCUSDT
//! minutes, each in its own venue's wire shape (bybit and okx list them newest-first); each venue's
//! `kline_fixture` test parses its own file against this one expectation.

use vike_marketdata::Bar;

/// The three bars, field for field, ascending by `ts`. Literals mirror the JSON's decimal strings;
/// a parse must reproduce their exact f64 bit patterns.
pub fn btcusdt_1m_bars() -> Vec<Bar> {
    let mk = |ts, o: f64, h: f64, l: f64, c: f64, v: f64| Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: v,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    vec![
        mk(1_700_000_000_000, 27000.10, 27050.50, 26980.00, 27010.25, 12.345678),
        mk(1_700_000_060_000, 27010.25, 27100.00, 27000.00, 27080.10, 8.10),
        mk(1_700_000_120_000, 27080.10, 27090.00, 27010.00, 27033.33, 5.555555),
    ]
}
