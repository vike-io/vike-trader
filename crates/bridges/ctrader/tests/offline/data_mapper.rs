//! Unit tests for the pure spot/trendbar mapper (`mapper.rs`) — no socket, no actor, just the
//! descale + id->name resolution logic driven directly against a hand-built `SymbolMap`.

use vike_ctrader::event_mapper::spot_to_quote;
use vike_ctrader::proto::ProtoOaSpotEvent;
use vike_ctrader::test_support::{eurusd_symbol_map, usdjpy_symbol_map};

#[test]
fn spot_event_descales_to_price() {
    let syms = eurusd_symbol_map();
    let ev = ProtoOaSpotEvent {
        symbol_id: 1,
        bid: Some(113911),
        ask: Some(113912),
        ..Default::default()
    };
    let (sym, bid, ask) = spot_to_quote(&ev, &syms).unwrap();
    assert_eq!(sym, "EURUSD");
    assert!((bid - 1.13911).abs() < 1e-9);
    assert!((ask - 1.13912).abs() < 1e-9);
}

/// Live-verified regression: EURUSD (digits=5) proves nothing about a lower-digits symbol, since
/// `10^5 == 100000` happens to coincide with the true fixed scale. USDJPY (digits=3) is the case
/// that exposes the bug: the old `symbols.scale(id)` descale (`10^3`) gives `16233.6`/`16233.7` —
/// 100x too large — where live cTrader means `162.336`/`162.337`. This test FAILS under the old
/// descale and PASSES under the `RELATIVE_PRICE_SCALE` fix.
#[test]
fn spot_event_digits3_uses_fixed_1e5_scale_not_10_pow_digits() {
    let syms = usdjpy_symbol_map();
    let ev = ProtoOaSpotEvent {
        symbol_id: 4,
        bid: Some(16233600),
        ask: Some(16233700),
        ..Default::default()
    };
    let (sym, bid, ask) = spot_to_quote(&ev, &syms).unwrap();
    assert_eq!(sym, "USDJPY");
    assert!((bid - 162.336).abs() < 1e-6, "bid={bid}");
    assert!((ask - 162.337).abs() < 1e-6, "ask={ask}");
}

#[test]
fn spot_event_missing_bid_is_none() {
    let syms = eurusd_symbol_map();
    let ev = ProtoOaSpotEvent { symbol_id: 1, bid: None, ask: Some(113912), ..Default::default() };
    assert!(spot_to_quote(&ev, &syms).is_none());
}

#[test]
fn spot_event_missing_ask_is_none() {
    let syms = eurusd_symbol_map();
    let ev = ProtoOaSpotEvent { symbol_id: 1, bid: Some(113911), ask: None, ..Default::default() };
    assert!(spot_to_quote(&ev, &syms).is_none());
}

#[test]
fn spot_event_unknown_symbol_is_none() {
    let syms = eurusd_symbol_map();
    let ev = ProtoOaSpotEvent {
        symbol_id: 999,
        bid: Some(113911),
        ask: Some(113912),
        ..Default::default()
    };
    assert!(spot_to_quote(&ev, &syms).is_none());
}
