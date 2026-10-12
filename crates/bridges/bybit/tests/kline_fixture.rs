//! The kline parser against a committed venue response — offline, bit-exact on every field.
//! Moved from `crates/vike-backfill/tests/bybit_backfill.rs` (docs/decisions/0094), where it
//! sat beside a program that no longer exists.

use vike_bybit::data::parse_bybit_klines;
use vike_model::Bar;
use vike_model::test_support::bars::assert_bars_bit_eq;

/// The three bars the fixture must map to, field-for-field, ASCENDING by ts (the fixture lists them
/// newest-first, so this also pins the reversal). Literals mirror the JSON's decimal strings.
fn expected_bars() -> Vec<Bar> {
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

#[test]
fn parse_klines_fixture_maps_bars_exactly_and_ascending() {
    let body = include_str!("fixtures/bybit_klines_btcusdt_1m.json");
    let got = parse_bybit_klines(body).unwrap();
    assert!(got.windows(2).all(|w| w[0].ts < w[1].ts), "reversed to strictly ascending");
    assert_bars_bit_eq(&expected_bars(), &got);
}
