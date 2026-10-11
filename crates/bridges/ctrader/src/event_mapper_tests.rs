use super::*;
use crate::proto::{ProtoOaLightSymbol, ProtoOaSymbol};

fn test_symbol_map() -> SymbolMap {
    let light = vec![ProtoOaLightSymbol {
        symbol_id: 1,
        symbol_name: Some("EURUSD".to_string()),
        ..Default::default()
    }];
    let full = vec![ProtoOaSymbol { symbol_id: 1, digits: 5, ..Default::default() }];
    SymbolMap::from_symbols(&light, &full)
}

/// USDJPY, `digits=3` — the live-verified regression fixture: cTrader's relative price scale
/// is fixed at 1e5 for every symbol, NOT `10^digits` (digits=3 would wrongly give `10^3`).
fn usdjpy_symbol_map() -> SymbolMap {
    let light = vec![ProtoOaLightSymbol {
        symbol_id: 4,
        symbol_name: Some("USDJPY".to_string()),
        ..Default::default()
    }];
    let full = vec![ProtoOaSymbol { symbol_id: 4, digits: 3, ..Default::default() }];
    SymbolMap::from_symbols(&light, &full)
}

#[test]
fn descales_spot_event() {
    let syms = test_symbol_map();
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

/// Live-verified regression: a `digits=3` symbol (USDJPY) MUST still descale by the fixed
/// 1e5 relative-price scale, not `10^digits` (which would wrongly yield 16233.6/16233.7). This
/// test fails under the old `symbols.scale(id)` descale and passes under the fix.
#[test]
fn descales_spot_event_digits3_uses_fixed_scale_not_10_pow_digits() {
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
fn interval_period_roundtrip() {
    for interval in ["1m", "5m", "15m", "30m", "1h", "4h", "12h", "1d", "1w", "1M"] {
        let period = trendbar_period_for_interval(interval).expect("known interval");
        assert_eq!(interval_for_trendbar_period(period), interval);
    }
    assert!(trendbar_period_for_interval("1s").is_none());
}

#[test]
fn trendbar_decodes_delta_encoded_ohlc() {
    let syms = test_symbol_map();
    let tb = ProtoOaTrendbar {
        volume: 42,
        period: Some(ProtoOaTrendbarPeriod::M1 as i32),
        low: Some(113900),
        delta_open: Some(5),
        delta_close: Some(12),
        delta_high: Some(20),
        utc_timestamp_in_minutes: Some(1000),
    };
    let ev = ProtoOaSpotEvent { symbol_id: 1, trendbar: vec![tb], ..Default::default() };
    let bars = spot_to_bars(&ev, &syms);
    assert_eq!(bars.len(), 1);
    let (sym, interval, bar) = &bars[0];
    assert_eq!(sym, "EURUSD");
    assert_eq!(*interval, "1m");
    assert_eq!(bar.ts, 1000 * 60_000);
    assert!((bar.low - 1.139).abs() < 1e-9);
    assert!((bar.open - 1.13905).abs() < 1e-9);
    assert!((bar.close - 1.13912).abs() < 1e-9);
    assert!((bar.high - 1.1392).abs() < 1e-9);
    assert_eq!(bar.volume, 42.0);
}

/// Live-verified regression: trendbar OHLC (low + deltas) on a `digits=3` symbol (USDJPY)
/// also descales by the fixed 1e5 scale, not `10^digits`. Raw `low=16231200` -> `162.312`.
#[test]
fn trendbar_decodes_delta_encoded_ohlc_digits3_uses_fixed_scale() {
    let syms = usdjpy_symbol_map();
    let tb = ProtoOaTrendbar {
        volume: 7,
        period: Some(ProtoOaTrendbarPeriod::M1 as i32),
        low: Some(16231200),
        delta_open: Some(500),
        delta_close: Some(1200),
        delta_high: Some(2000),
        utc_timestamp_in_minutes: Some(2000),
    };
    let ev = ProtoOaSpotEvent { symbol_id: 4, trendbar: vec![tb], ..Default::default() };
    let bars = spot_to_bars(&ev, &syms);
    assert_eq!(bars.len(), 1);
    let (sym, interval, bar) = &bars[0];
    assert_eq!(sym, "USDJPY");
    assert_eq!(*interval, "1m");
    assert_eq!(bar.ts, 2000 * 60_000);
    assert!((bar.low - 162.312).abs() < 1e-6, "low={}", bar.low);
    assert!((bar.open - 162.317).abs() < 1e-6, "open={}", bar.open);
    assert!((bar.close - 162.324).abs() < 1e-6, "close={}", bar.close);
    assert!((bar.high - 162.332).abs() < 1e-6, "high={}", bar.high);
    assert_eq!(bar.volume, 7.0);
}

#[test]
fn trendbar_missing_field_is_skipped() {
    let syms = test_symbol_map();
    let tb = ProtoOaTrendbar {
        volume: 1,
        period: Some(ProtoOaTrendbarPeriod::M1 as i32),
        low: None, // missing — incomplete entry
        delta_open: Some(5),
        delta_close: Some(12),
        delta_high: Some(20),
        utc_timestamp_in_minutes: Some(1000),
    };
    let ev = ProtoOaSpotEvent { symbol_id: 1, trendbar: vec![tb], ..Default::default() };
    assert!(spot_to_bars(&ev, &syms).is_empty());
}

#[test]
fn spot_to_bars_unknown_symbol_is_empty() {
    let syms = test_symbol_map();
    let ev = ProtoOaSpotEvent { symbol_id: 999, ..Default::default() };
    assert!(spot_to_bars(&ev, &syms).is_empty());
}

#[test]
fn trendbar_period_ms_matches_known_durations() {
    assert_eq!(trendbar_period_ms(ProtoOaTrendbarPeriod::M1), 60_000);
    assert_eq!(trendbar_period_ms(ProtoOaTrendbarPeriod::M5), 5 * 60_000);
    assert_eq!(trendbar_period_ms(ProtoOaTrendbarPeriod::H1), 3_600_000);
    assert_eq!(trendbar_period_ms(ProtoOaTrendbarPeriod::D1), 86_400_000);
}
