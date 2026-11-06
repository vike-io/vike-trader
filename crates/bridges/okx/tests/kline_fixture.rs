//! The kline parser against a committed venue response — offline, bit-exact on every field.
//! Moved from `crates/vike-backfill/tests/okx_backfill.rs` (docs/decisions/0094), where it
//! sat beside a program that no longer exists.

use vike_bridge_core::kline_fixture::btcusdt_1m_bars;
use vike_model::test_support::bars::assert_bars_bit_eq;
use vike_okx::data::parse_okx_klines;

/// The fixture lists the bars newest-first, so this also pins the reversal.
#[test]
fn parse_klines_fixture_maps_bars_exactly_and_ascending() {
    let body = include_str!("fixtures/okx_klines_btcusdt_1m.json");
    let got = parse_okx_klines(body).unwrap();
    assert!(got.windows(2).all(|w| w[0].ts < w[1].ts), "reversed to strictly ascending");
    assert_bars_bit_eq(&btcusdt_1m_bars(), &got);
}
