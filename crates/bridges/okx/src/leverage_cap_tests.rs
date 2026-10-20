use super::*;
use serde_json::json;

/// The REAL `/api/v5/public/instruments?instType=SWAP&instId=BTC-USDT-SWAP` row, captured live
/// 2026-08-05 (trimmed to the keys this parser reads plus `lever`). Every numeric is a STRING,
/// including `lever` — the venue-wide convention `json_num` decodes.
fn live_row() -> Value {
    json!({"data": [{
        "instType": "SWAP", "instId": "BTC-USDT-SWAP", "instFamily": "BTC-USDT",
        "ctType": "linear", "ctVal": "0.01", "ctValCcy": "BTC", "ctMult": "1",
        "lever": "100",
        "lotSz": "0.01", "minSz": "0.01", "tickSz": "0.1", "maxMktSz": "35000",
        "settleCcy": "USDT", "state": "live"
    }]})
}

/// The cap rides the response the exec thread ALREADY fetches for `ctVal` and the tick/lot grid
/// — the whole reason okx clamps for free — and its arrival changes nothing else.
#[test]
fn parses_lever_from_the_live_instruments_shape() {
    let instruments = parse_okx_perp_instruments(&live_row());
    let inst = &instruments["BTC-USDT-SWAP"];
    assert_eq!(inst.max_leverage, Some(100.0), "top-level `lever` \"100\"");
    // …and the pre-existing fields are exactly what they were before the field existed.
    assert_eq!(inst.properties.tick_size, 0.1);
    assert_eq!(inst.properties.step_size, 0.01);
    assert_eq!(inst.properties.min_qty, 0.01);
    assert_eq!(inst.properties.max_qty, 35000.0);
    assert_eq!(inst.ct_val, 0.01);
    assert_eq!(inst.ct_mult, 1.0);
    assert_eq!(inst.base_asset, "BTC");
}

/// Absent / malformed ⇒ `None` (UNKNOWN), never `0.0`. OKX omits `lever` on SPOT and OPTION,
/// and an empty string is its idiom for "not applicable" — both must read as unknown, because
/// `None` is what makes the clamp a no-op instead of de-leveraging an account to 0.
#[test]
fn an_absent_or_not_applicable_lever_is_unknown() {
    let payload = json!({"data": [
        {"instId": "NOLEVER-USDT-SWAP", "ctValCcy": "N", "lotSz": "1"},
        {"instId": "EMPTY-USDT-SWAP", "ctValCcy": "E", "lever": ""},
        {"instId": "JUNK-USDT-SWAP", "ctValCcy": "J", "lever": "n/a"},
        {"instId": "NULL-USDT-SWAP", "ctValCcy": "X", "lever": null},
    ]});
    let got = parse_okx_perp_instruments(&payload);
    for id in ["NOLEVER-USDT-SWAP", "EMPTY-USDT-SWAP", "JUNK-USDT-SWAP", "NULL-USDT-SWAP"] {
        assert_eq!(got[id].max_leverage, None, "{id} must be UNKNOWN, not 0.0");
    }
}

/// End to end at the seam that matters: the parsed cap, fed to the shared clamp, is exactly
/// `min(requested, cap)` — and an instrument without one changes nothing.
#[test]
fn the_parsed_cap_drives_the_shared_clamp() {
    use vike_bridge_core::leverage::clamp_to_venue_cap;
    let capped = parse_okx_perp_instruments(&live_row())["BTC-USDT-SWAP"].max_leverage;
    assert_eq!(clamp_to_venue_cap(2.0, capped, VENUE, "BTC-USDT-SWAP"), 2.0, "under the cap");
    assert_eq!(clamp_to_venue_cap(125.0, capped, VENUE, "BTC-USDT-SWAP"), 100.0, "clamped");
    let payload = json!({"data": [{"instId": "NOCAP-USDT-SWAP", "ctValCcy": "N"}]});
    let unknown = parse_okx_perp_instruments(&payload)["NOCAP-USDT-SWAP"].max_leverage;
    assert_eq!(clamp_to_venue_cap(125.0, unknown, VENUE, "NOCAP-USDT-SWAP"), 125.0, "unknown");
}
