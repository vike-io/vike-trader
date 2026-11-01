use super::*;
use serde_json::json;

/// The REAL `/v5/market/instruments-info?category=linear&symbol=BTCUSDT` row shape, captured
/// live 2026-08-05 (trimmed to the keys this parser reads plus the leverage filter). Note
/// `maxLeverage` is a decimal STRING with trailing zeros — `json_num` handles it.
fn live_row() -> serde_json::Value {
    json!({"result": {"list": [{
        "symbol": "BTCUSDT",
        "contractType": "LinearPerpetual",
        "status": "Trading",
        "baseCoin": "BTC",
        "quoteCoin": "USDT",
        "leverageFilter": {"minLeverage": "1", "maxLeverage": "100.00", "leverageStep": "0.01"},
        "priceFilter": {"minPrice": "0.10", "maxPrice": "1999999.80", "tickSize": "0.10"},
        "lotSizeFilter": {
            "maxOrderQty": "1500.000", "minOrderQty": "0.001", "qtyStep": "0.001",
            "postOnlyMaxOrderQty": "1500.000", "maxMktOrderQty": "150.000",
            "minNotionalValue": "5"
        }
    }]}})
}

/// The cap is parsed out of the response the exec thread ALREADY fetches — the whole reason
/// bybit clamps for free — and the rest of the grid is untouched by its arrival.
#[test]
fn parses_max_leverage_from_the_live_instruments_info_shape() {
    let instruments = parse_bybit_perp_instruments(&live_row());
    let inst = &instruments["BTCUSDT"];
    assert_eq!(inst.max_leverage, Some(100.0), "leverageFilter.maxLeverage \"100.00\"");
    // …and the pre-existing fields are exactly what they were before the field existed.
    assert_eq!(inst.properties.tick_size, 0.10);
    assert_eq!(inst.properties.step_size, 0.001);
    assert_eq!(inst.properties.min_qty, 0.001);
    assert_eq!(inst.properties.max_qty, 1500.0);
    assert_eq!(inst.properties.min_notional, 5.0);
    assert_eq!(inst.base_asset, "BTC");
}

/// A non-integer ceiling round-trips verbatim — bybit's `leverageStep` is `0.01`, so a
/// fractional cap is a real venue value, and truncating it would clamp BELOW what the venue
/// allows.
#[test]
fn a_fractional_cap_is_not_truncated() {
    let payload = json!({"result": {"list": [
        {"symbol": "ALTUSDT", "baseCoin": "ALT", "leverageFilter": {"maxLeverage": "12.50"}}
    ]}});
    assert_eq!(parse_bybit_perp_instruments(&payload)["ALTUSDT"].max_leverage, Some(12.5));
}

/// Absent / malformed ⇒ `None` (UNKNOWN), never `0.0`. `None` is what makes the clamp a no-op,
/// so this is the arm that keeps a venue-shape change from silently de-leveraging an account.
#[test]
fn an_absent_or_malformed_leverage_filter_is_unknown() {
    let payload = json!({"result": {"list": [
        // no leverageFilter at all (the shape the r6 golden fixture carries)
        {"symbol": "NOFILTER", "baseCoin": "N", "lotSizeFilter": {"qtyStep": "0.01"}},
        // present but empty
        {"symbol": "EMPTYFILTER", "baseCoin": "E", "leverageFilter": {}},
        // present, unparseable
        {"symbol": "JUNK", "baseCoin": "J", "leverageFilter": {"maxLeverage": "n/a"}},
        // present, null
        {"symbol": "NULLCAP", "baseCoin": "X", "leverageFilter": {"maxLeverage": null}},
    ]}});
    let got = parse_bybit_perp_instruments(&payload);
    for sym in ["NOFILTER", "EMPTYFILTER", "JUNK", "NULLCAP"] {
        assert_eq!(got[sym].max_leverage, None, "{sym} must be UNKNOWN, not 0.0");
    }
}

/// End to end at the seam that matters: the parsed cap, fed to the shared clamp, is exactly
/// `min(requested, cap)` — and a row without one changes nothing.
#[test]
fn the_parsed_cap_drives_the_shared_clamp() {
    use vike_bridge_core::leverage::clamp_to_venue_cap;
    let capped = parse_bybit_perp_instruments(&live_row())["BTCUSDT"].max_leverage;
    assert_eq!(clamp_to_venue_cap(2.0, capped, VENUE, "BTCUSDT"), 2.0, "under the cap");
    assert_eq!(clamp_to_venue_cap(150.0, capped, VENUE, "BTCUSDT"), 100.0, "clamped to 100x");
    let payload = json!({"result": {"list": [{"symbol": "NOCAP", "baseCoin": "N"}]}});
    let unknown = parse_bybit_perp_instruments(&payload)["NOCAP"].max_leverage;
    assert_eq!(clamp_to_venue_cap(150.0, unknown, VENUE, "NOCAP"), 150.0, "unknown ⇒ unchanged");
}
