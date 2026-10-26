//! The catalog over a REAL broker capture: `tests/fixtures/ctrader_catalog_capture.json` is the
//! output of the read-only `tests/ctrader_catalog_smoke.rs` against the cTrader demo gateway
//! (2026-10-11), copied verbatim — every asset class, every symbol category, 40 symbols spread
//! across them with their full-symbol fields, and the assets they reference. It carries no account
//! id and no credential (the smoke prints none).
//!
//! These tests rebuild the handshake's `SymbolLists` from it and assert the exact `Contract` the
//! catalog (`vike_ctrader::catalog::instruments_from_map`) builds for each kind the broker lists,
//! so the asset-class keyword table is pinned against the names the broker really sends.

use serde_json::Value;
use vike_ctrader::catalog::instruments_from_map;
use vike_ctrader::proto::{
    ProtoOaAsset, ProtoOaAssetClass, ProtoOaLightSymbol, ProtoOaSymbol, ProtoOaSymbolCategory,
};
use vike_ctrader::symbols::{SymbolLists, SymbolMap};
use vike_model::{
    AssetClass, CfdExpiry, CfdProduct, CfdSpec, CfdUnderlying, Contract, FxSpec, Instrument,
};

const CAPTURE: &str = include_str!("../fixtures/ctrader_catalog_capture.json");

fn capture() -> Value {
    serde_json::from_str(CAPTURE).expect("the capture is JSON")
}

fn rows<'a>(doc: &'a Value, key: &str) -> &'a [Value] {
    doc[key].as_array().map(Vec::as_slice).unwrap_or_else(|| panic!("`{key}` is an array"))
}

fn int(v: &Value) -> Option<i64> {
    v.as_i64()
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// The handshake's lists, rebuilt from the capture.
fn lists(doc: &Value) -> SymbolLists {
    let light = rows(doc, "symbols")
        .iter()
        .map(|s| ProtoOaLightSymbol {
            symbol_id: int(&s["symbol_id"]).expect("symbol_id"),
            symbol_name: text(&s["name"]),
            description: text(&s["description"]),
            symbol_category_id: int(&s["category_id"]),
            base_asset_id: int(&s["base_asset_id"]),
            quote_asset_id: int(&s["quote_asset_id"]),
            ..Default::default()
        })
        .collect();
    let full = rows(doc, "symbols")
        .iter()
        .filter(|s| !s["full"].is_null())
        .map(|s| {
            let f = &s["full"];
            ProtoOaSymbol {
                symbol_id: int(&s["symbol_id"]).expect("symbol_id"),
                digits: int(&f["digits"]).expect("digits") as i32,
                pip_position: int(&f["pip_position"]).expect("pip_position") as i32,
                lot_size: int(&f["lot_size"]),
                min_volume: int(&f["min_volume"]),
                step_volume: int(&f["step_volume"]),
                max_volume: int(&f["max_volume"]),
                swap_long: f["swap_long"].as_f64(),
                swap_short: f["swap_short"].as_f64(),
                swap_calculation_type: int(&f["swap_calculation_type"]).map(|t| t as i32),
                ..Default::default()
            }
        })
        .collect();
    let categories = rows(doc, "categories")
        .iter()
        .map(|c| ProtoOaSymbolCategory {
            id: int(&c["id"]).expect("category id"),
            asset_class_id: int(&c["asset_class_id"]).expect("asset_class_id"),
            name: text(&c["name"]).expect("category name"),
            ..Default::default()
        })
        .collect();
    let asset_classes = rows(doc, "asset_classes")
        .iter()
        .map(|a| ProtoOaAssetClass {
            id: int(&a["id"]),
            name: text(&a["name"]),
            ..Default::default()
        })
        .collect();
    let assets = rows(doc, "assets")
        .iter()
        .map(|a| ProtoOaAsset {
            asset_id: int(&a["asset_id"]).expect("asset_id"),
            name: text(&a["name"]).expect("asset name"),
            display_name: text(&a["display_name"]),
            ..Default::default()
        })
        .collect();
    // The capture records the symbol universe only, not the trader response.
    SymbolLists {
        light,
        full,
        categories,
        asset_classes,
        assets,
        deposit_asset_id: None,
        account_type: None,
    }
}

pub(crate) fn catalog() -> Vec<Instrument> {
    instruments_from_map(&SymbolMap::from_lists(&lists(&capture())))
}

pub(crate) fn find<'a>(out: &'a [Instrument], name: &str) -> &'a Instrument {
    out.iter().find(|i| i.raw_symbol == name).unwrap_or_else(|| panic!("{name} in the capture"))
}

fn cfd(underlying_kind: CfdUnderlying, swap_long: f64, swap_short: f64) -> Contract {
    Contract::Cfd(CfdSpec {
        underlying_kind,
        underlying: None,
        product: CfdProduct::Cfd,
        expiry: CfdExpiry::Unknown,
        margin_rate: None,
        swap_long: Some(swap_long),
        swap_short: Some(swap_short),
    })
}

/// The evidence for reading the ASSET CLASS rather than the category: every category the demo
/// broker publishes is named `Default Category`, one per asset class.
#[test]
fn every_captured_category_is_named_default_category() {
    let doc = capture();
    let categories = rows(&doc, "categories");
    assert_eq!(categories.len(), 11);
    for c in categories {
        assert_eq!(c["name"], "Default Category", "{c}");
        assert_eq!(c["id"], c["asset_class_id"], "one category per asset class: {c}");
    }
}

/// The exact contract, base and quote for one symbol of every kind the capture holds.
#[test]
fn every_captured_kind_builds_its_exact_contract() {
    let out = catalog();
    let cases: [(&str, (&str, &str), Contract); 9] = [
        (
            "EURUSD",
            ("EUR", "USD"),
            Contract::Fx(FxSpec {
                pip_size: Some(0.0001),
                lot_size: Some(100_000.0),
                swap_long: Some(-0.8),
                swap_short: Some(0.13),
            }),
        ),
        ("XAUUSD", ("XAU", "USD"), cfd(CfdUnderlying::Metal, -57.03, 39.77)),
        ("AUS200", ("AUS200", "AUD"), cfd(CfdUnderlying::Index, -22.7, 1.19)),
        ("BRENT", ("BRENT", "USD"), cfd(CfdUnderlying::Commodity, -0.205, 0.085)),
        ("XNGUSD", ("XNG", "USD"), cfd(CfdUnderlying::Commodity, -5.08, 3.7)),
        // The old name heuristic's known victim: six uppercase letters, and a crypto CFD.
        ("BTCUSD", ("BTC", "USD"), cfd(CfdUnderlying::Crypto, -20.0, 0.0)),
        // Class `Futures`: the wrapper, not the underlying — `Other` on purpose.
        ("DXY_H6", ("DXY_H6", "USD"), cfd(CfdUnderlying::Other, 0.0, 0.0)),
        ("EURBBL_H6", ("EURBBL_H6", "EUR"), cfd(CfdUnderlying::Bond, 0.0, 0.0)),
        // Class `Futures Commodities`: a dated wrapper like `Futures`, not the spot-like `Commodities`.
        ("Wheat_H6", ("Wheat_H6", "USD"), cfd(CfdUnderlying::Other, 0.0, 0.0)),
    ];
    for (name, (base, quote), contract) in cases {
        let row = find(&out, name);
        assert_eq!(row.contract, contract, "{name}");
        assert_eq!((row.base.as_str(), row.quote.as_str()), (base, quote), "{name}");
        assert_eq!(
            row.properties.asset_class,
            Some(row.asset_class()),
            "{name}: read, not guessed"
        );
    }
}

/// Exactly the `Forex` class is FX: every sampled symbol in it, and nothing outside it — whatever
/// its name looks like. Every sampled row names its base and quote from the asset list.
#[test]
fn fx_is_exactly_the_forex_class_and_every_row_names_its_assets() {
    let doc = capture();
    let out = catalog();
    assert_eq!(out.len(), rows(&doc, "symbols").len());
    for s in rows(&doc, "symbols") {
        let name = s["name"].as_str().expect("name");
        let row = find(&out, name);
        let forex = s["category_id"] == 1;
        assert_eq!(row.asset_class() == AssetClass::Fx, forex, "{name}");
        assert!(!row.base.is_empty() && !row.quote.is_empty(), "{name}: base/quote from assets");
    }
}
