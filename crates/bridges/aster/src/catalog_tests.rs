use super::*;
use vike_model::{Contract, Margining, PerpSpec};

#[test]
fn spot_exchange_info_maps_to_cryptospot_instruments() {
    let payload = serde_json::json!({
        "symbols": [
            { "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT", "status": "TRADING" },
            { "symbol": "OLDUSDT", "baseAsset": "OLD", "quoteAsset": "USDT", "status": "BREAK" }
        ]
    });
    let out = parse_spot(&payload);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].raw_symbol, "BTCUSDT");
    assert_eq!(out[0].contract, Contract::CryptoSpot);
    assert_eq!(out[0].asset_class(), AssetClass::CryptoSpot);
}

#[test]
fn perp_exchange_info_maps_to_cryptoperp() {
    let payload = serde_json::json!({
        "symbols": [
            { "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT", "marginAsset": "USDT",
              "status": "TRADING", "contractType": "PERPETUAL" }
        ]
    });
    let out = parse_perp(&payload);
    assert_eq!(out[0].asset_class(), AssetClass::CryptoPerp);
    assert_eq!(out[0].raw_symbol, "BTCUSDT.P", "perp gets the .P distinct-symbol suffix");
    assert_eq!(out[0].base, "BTC");
    assert_eq!(out[0].quote, "USDT");
}

/// The picker over four verbatim entries of Aster's mainnet `/fapi/v3/exchangeInfo`
/// (`crates/bridges/aster/tests/fixtures/exchange_info/aster_fapi.json`, read 2026-10-11): the
/// two `TRADING` perps list as linear `Contract::Perp`s settling in their own `marginAsset`,
/// carrying their grid and the venue's `contractType` verbatim; the `SETTLING` and the
/// `PENDING_TRADING` entries do not list.
#[test]
fn real_aster_fapi_entries_list_as_typed_perps() {
    let payload: Value =
        serde_json::from_str(include_str!("../tests/fixtures/exchange_info/aster_fapi.json"))
            .expect("fixture is valid JSON");
    let out = parse_perp(&payload);
    let symbols: Vec<&str> = out.iter().map(|i| i.raw_symbol.as_str()).collect();
    assert_eq!(symbols, ["BTCUSDT.P", "BTCUSD1.P"]);
    let perp = |settle: &str| {
        Contract::Perp(PerpSpec {
            margining: Margining::Linear,
            settle: settle.to_string(),
            value_ccy: None,
            funding_interval_ms: None,
            settling_ms: None,
        })
    };
    assert_eq!(out[0].contract, perp("USDT"));
    assert_eq!(out[1].contract, perp("USD1"));
    assert_eq!(out[0].contract_type.as_deref(), Some("PERPETUAL"));
    assert_eq!(out[0].properties.tick_size, 0.1);
    assert_eq!(out[0].properties.max_qty, 120.0);
    assert_eq!(out[0].properties.min_notional, 5.0);
    assert_eq!(out[0].properties.asset_class, Some(AssetClass::CryptoPerp));
}

#[test]
fn perp_requires_perpetual_contract_type() {
    // TRADING but a delivery future (not PERPETUAL) must be excluded — the extra gate beyond
    // spot's plain status check.
    let payload = serde_json::json!({
        "symbols": [
            { "symbol": "BTCUSDT_240329", "baseAsset": "BTC", "quoteAsset": "USDT",
              "status": "TRADING", "contractType": "CURRENT_QUARTER" }
        ]
    });
    assert!(
        parse_perp(&payload).is_empty(),
        "delivery futures must not appear in the perp catalog"
    );
}

#[test]
fn spot_and_perp_urls_are_aster_mainnet() {
    assert_eq!(spot_url(), "https://sapi.asterdex.com/api/v3/exchangeInfo");
    assert_eq!(perp_url(), "https://fapi.asterdex.com/fapi/v3/exchangeInfo");
}
