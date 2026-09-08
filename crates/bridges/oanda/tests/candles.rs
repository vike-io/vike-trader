//! Integration coverage for the OANDA data half: `candles` JSON -> `Bar`, the granularity table,
//! and the canonical-symbol <-> OANDA-instrument mapping. All canned payloads below are hand
//! verified against the OANDA v20 `GET /v3/instruments/{instrument}/candles` response shape
//! (`Accept-Datetime-Format: UNIX`, `price=M` midpoints) — the retired Python app exported no
//! fixture for this venue (`fixtures/r6/` holds none), so unlike the frozen `r6_*_parity` bytes
//! these payloads are authored here rather than exported. Both kinds are compared exactly; the
//! difference is only where the expected values came from.

use vike_oanda::{OandaRest, fetch_candles, granularity, parse_candles, to_oanda_instrument};

#[test]
fn happy_path_keeps_only_complete_candles_and_leaves_symbol_unset() {
    let v = serde_json::json!({
        "instrument": "EUR_USD",
        "granularity": "H1",
        "candles": [
            {"complete": true, "volume": 100, "time": "1478012400.000000000",
             "mid": {"o": "1.09000", "h": "1.09500", "l": "1.08900", "c": "1.09300"}},
            {"complete": false, "volume": 5, "time": "1478016000.000000000",
             "mid": {"o": "1.09300", "h": "1.09400", "l": "1.09200", "c": "1.09350"}}
        ]
    });
    let bars = parse_candles(&v);
    assert_eq!(bars.len(), 1, "the still-forming candle must be dropped");
    let b = &bars[0];
    assert_eq!(b.ts, 1_478_012_400_000);
    assert_eq!(b.open, 1.09);
    assert_eq!(b.high, 1.095);
    assert_eq!(b.low, 1.089);
    assert_eq!(b.close, 1.093);
    assert_eq!(b.volume, 100.0);
    // parse_candles never reads the top-level "instrument" field — the caller (fetch_candles)
    // knows the instrument it asked for; Bar.symbol stays unset here.
    assert_eq!(b.symbol, None);
    assert_eq!(b.funding, None);
    assert_eq!(b.bid, None);
    assert_eq!(b.ask, None);
}

#[test]
fn missing_candles_key_yields_empty() {
    let v = serde_json::json!({"instrument": "EUR_USD", "granularity": "H1"});
    assert!(parse_candles(&v).is_empty());
}

#[test]
fn empty_candles_array_yields_empty() {
    let v = serde_json::json!({"candles": []});
    assert!(parse_candles(&v).is_empty());
}

#[test]
fn candle_missing_mid_object_is_skipped() {
    let v = serde_json::json!({"candles": [
        {"complete": true, "time": "1.0", "volume": 1}
    ]});
    assert!(parse_candles(&v).is_empty());
}

#[test]
fn candle_missing_one_ohlc_field_is_skipped() {
    // "c" (close) is absent -> the whole candle drops (open/high/low/close are all-or-nothing).
    let v = serde_json::json!({"candles": [
        {"complete": true, "time": "1.0", "volume": 1,
         "mid": {"o": "1.0", "h": "1.1", "l": "0.9"}}
    ]});
    assert!(parse_candles(&v).is_empty());
}

#[test]
fn mid_prices_as_json_numbers_instead_of_strings_are_silently_dropped() {
    // OANDA's wire convention is DECIMAL STRINGS for prices (so trailing-zero precision survives
    // JSON round-tripping). parse_candles reads them via `.as_str()`; a candle whose "mid" fields
    // arrive as JSON numbers (not strings) fails every `.as_str()` lookup and the whole candle is
    // dropped, not coerced. This locks in that gotcha.
    let v = serde_json::json!({"candles": [
        {"complete": true, "time": "1478012400.000000000", "volume": 100,
         "mid": {"o": 1.09, "h": 1.095, "l": 1.089, "c": 1.093}}
    ]});
    assert!(
        parse_candles(&v).is_empty(),
        "numeric (non-string) mid prices must be dropped, not coerced"
    );
}

#[test]
fn unparseable_or_missing_time_falls_back_to_zero_but_keeps_the_candle() {
    // Unlike a missing OHLC field, a bad/missing "time" does NOT drop the candle — ts defaults to 0.
    let v = serde_json::json!({"candles": [
        {"complete": true, "volume": 1, "mid": {"o": "1.0", "h": "1.1", "l": "0.9", "c": "1.05"}}
    ]});
    let bars = parse_candles(&v);
    assert_eq!(bars.len(), 1);
    assert_eq!(bars[0].ts, 0);

    let v2 = serde_json::json!({"candles": [
        {"complete": true, "time": "not-a-number", "volume": 1,
         "mid": {"o": "1.0", "h": "1.1", "l": "0.9", "c": "1.05"}}
    ]});
    assert_eq!(parse_candles(&v2)[0].ts, 0);
}

#[test]
fn missing_volume_defaults_to_zero() {
    let v = serde_json::json!({"candles": [
        {"complete": true, "time": "1.0", "mid": {"o": "1.0", "h": "1.1", "l": "0.9", "c": "1.05"}}
    ]});
    assert_eq!(parse_candles(&v)[0].volume, 0.0);
}

#[test]
fn multiple_complete_candles_all_kept_in_order() {
    let v = serde_json::json!({"candles": [
        {"complete": true, "time": "1.0", "volume": 10,
         "mid": {"o": "1.0", "h": "1.1", "l": "0.9", "c": "1.05"}},
        {"complete": true, "time": "2.0", "volume": 20,
         "mid": {"o": "1.05", "h": "1.2", "l": "1.0", "c": "1.1"}},
    ]});
    let bars = parse_candles(&v);
    assert_eq!(bars.len(), 2);
    assert_eq!(bars[0].close, 1.05);
    assert_eq!(bars[1].close, 1.1);
}

#[test]
fn granularity_full_supported_table() {
    let table = [
        ("1m", "M1"),
        ("2m", "M2"),
        ("4m", "M4"),
        ("5m", "M5"),
        ("10m", "M10"),
        ("15m", "M15"),
        ("30m", "M30"),
        ("1h", "H1"),
        ("2h", "H2"),
        ("3h", "H3"),
        ("4h", "H4"),
        ("6h", "H6"),
        ("8h", "H8"),
        ("12h", "H12"),
        ("1d", "D"),
        ("1w", "W"),
        ("1mo", "M"),
    ];
    for (interval, code) in table {
        assert_eq!(granularity(interval), Some(code), "interval {interval}");
    }
}

#[test]
fn granularity_unsupported_intervals_are_none() {
    for bad in ["1s", "7s", "3d", "", "M1", "1H"] {
        assert_eq!(granularity(bad), None, "interval {bad} must be unsupported");
    }
}

#[test]
fn to_oanda_instrument_covers_the_conversion_table() {
    assert_eq!(to_oanda_instrument("eurusd"), "EUR_USD");
    assert_eq!(to_oanda_instrument("EURUSD"), "EUR_USD");
    assert_eq!(to_oanda_instrument("EurUsd"), "EUR_USD");
    assert_eq!(to_oanda_instrument("EUR_USD"), "EUR_USD"); // already OANDA-shaped, just upcased
    assert_eq!(to_oanda_instrument("eur_usd"), "EUR_USD");
    assert_eq!(to_oanda_instrument("XAUUSD"), "XAU_USD");
    // not exactly 6 alpha chars -> left alone (just upcased), no split inserted
    assert_eq!(to_oanda_instrument("ABCDE"), "ABCDE");
    assert_eq!(to_oanda_instrument("ABCDEFG"), "ABCDEFG");
    // 6 chars but not all alphabetic -> left alone too (the split guard requires all-alpha)
    assert_eq!(to_oanda_instrument("123456"), "123456");
    assert_eq!(to_oanda_instrument("EUR12"), "EUR12");
}

#[test]
fn fetch_candles_rejects_an_unsupported_interval_before_touching_the_network() {
    // granularity() is checked BEFORE any REST call, so this exercises the real error path
    // without a network double: a bogus base URL would fail loudly if the code ever reached it.
    let rest = OandaRest::new("unused-token".to_string());
    let err = fetch_candles(&rest, "http://127.0.0.1:1", "EURUSD", "7s", 10)
        .expect_err("unsupported interval must error, not touch the network");
    assert_eq!(err.status, 0);
    assert_eq!(err.message, "unsupported interval 7s");
}
