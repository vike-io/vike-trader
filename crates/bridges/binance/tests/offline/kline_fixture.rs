//! The kline parser against a committed venue response — offline, bit-exact on every field.
//! Moved from `crates/vike-backfill/tests/binance_backfill.rs` (docs/decisions/0094), where it
//! sat beside a program that no longer exists.

use vike_binance::data::parse_klines;
use vike_bridge_core::kline_fixture::btcusdt_1m_bars;
use vike_model::test_support::bars::assert_bars_bit_eq;

#[test]
fn parse_klines_fixture_maps_bars_exactly() {
    let body = include_str!("../fixtures/binance_klines_btcusdt_1m.json");
    let got = parse_klines(body).unwrap();
    assert_bars_bit_eq(&btcusdt_1m_bars(), &got);
}
