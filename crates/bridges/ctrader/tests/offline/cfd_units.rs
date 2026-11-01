//! What one unit of volume means for a CFD, pinned against the demo broker's own numbers.
//!
//! `tests/fixtures/ctrader_cfd_round_trips.json` is the output of `tests/ctrader_cfd_units_smoke.rs`
//! (`cfd_round_trip_smoke`, 2026-10-26): per symbol, the broker's symbol body (class, digits, lot size and the
//! volume grid) and ONE minimum-size market BUY closed by position id, with the entry and exit prices, the
//! gross profit the broker booked in the account's currency and the closing deal's
//! `quoteToDepositConversionRate`. These tests rebuild the handshake's lists from it and assert, for a metal,
//! an index, an energy, a commodity and a crypto CFD alike:
//!
//! - the any-symbol fence admits the class (`vike_ctrader::catalog`'s `MEASURED_CFD_CLASSES`), a quote currency
//!   other than the deposit's only through the converting fence;
//! - the engine's own linear formula (`vike_model::ContractValue::default()`, converted at the closing deal's
//!   rate) reproduces the broker's booked gross profit to the cent from `volume / 100` units, so the lot size
//!   (100 ounces for XAUUSD, 1000 for XAGUSD, 1 for an index) is not part of the unit;
//! - the bridge maps the engine's quantity to the wire volume the broker filled, and a fill of that volume
//!   back to the same quantity.
//!
//! They fail on the code that refused every CFD by name, and on a contract multiplier or a lot-size scaling that
//! the measurement does not show.

use serde_json::Value;
use vike_ctrader::catalog::{any_symbol_refusal, instruments_from_map, quoted_symbol_refusal};
use vike_ctrader::event_mapper::{deal_fill, order_to_new_order};
use vike_ctrader::proto::{
    ProtoOaAsset, ProtoOaAssetClass, ProtoOaDeal, ProtoOaExecutionEvent, ProtoOaLightSymbol,
    ProtoOaSymbol, ProtoOaSymbolCategory, ProtoOaTradeSide,
};
use vike_ctrader::symbols::{SymbolLists, SymbolMap};
use vike_model::{CfdUnderlying, Contract, ContractValue, OrderRequest, OrderType};

const ROUND_TRIPS: &str = include_str!("../fixtures/ctrader_cfd_round_trips.json");

fn fixture() -> Value {
    serde_json::from_str(ROUND_TRIPS).expect("the fixture is JSON")
}

fn trips(doc: &Value) -> &[Value] {
    doc["round_trips"].as_array().map(Vec::as_slice).expect("`round_trips` is an array")
}

fn int(v: &Value) -> i64 {
    v.as_i64().expect("an integer")
}

fn num(v: &Value) -> f64 {
    v.as_f64().expect("a number")
}

fn word(v: &Value) -> &str {
    v.as_str().expect("a string")
}

/// The handshake's lists for the fixture's symbols: one category per broker asset class (the real broker's
/// shape: every category is named `Default Category`), the assets the symbols and the deposit name.
fn lists(doc: &Value) -> SymbolLists {
    let mut classes: Vec<&str> = Vec::new();
    let mut assets: Vec<&str> = vec![word(&doc["deposit"])];
    for t in trips(doc) {
        let class = word(&t["asset_class"]);
        if !classes.contains(&class) {
            classes.push(class);
        }
        for a in [word(&t["base"]), word(&t["quote"])] {
            if !assets.contains(&a) {
                assets.push(a);
            }
        }
    }
    let class_id = |c: &str| classes.iter().position(|x| *x == c).expect("class") as i64 + 1;
    let asset_id = |a: &str| assets.iter().position(|x| *x == a).expect("asset") as i64 + 100;
    let mut light = Vec::new();
    let mut full = Vec::new();
    for (i, t) in trips(doc).iter().enumerate() {
        let id = 1000 + i as i64;
        light.push(ProtoOaLightSymbol {
            symbol_id: id,
            symbol_name: Some(word(&t["symbol"]).to_string()),
            symbol_category_id: Some(class_id(word(&t["asset_class"]))),
            base_asset_id: Some(asset_id(word(&t["base"]))),
            quote_asset_id: Some(asset_id(word(&t["quote"]))),
            ..Default::default()
        });
        full.push(ProtoOaSymbol {
            symbol_id: id,
            digits: int(&t["digits"]) as i32,
            pip_position: int(&t["pip_position"]) as i32,
            lot_size: Some(int(&t["lot_size"])),
            min_volume: Some(int(&t["min_volume"])),
            step_volume: Some(int(&t["step_volume"])),
            max_volume: Some(int(&t["max_volume"])),
            measurement_units: Some(word(&t["measurement_units"]).to_string()),
            ..Default::default()
        });
    }
    SymbolLists {
        light,
        full,
        categories: classes
            .iter()
            .map(|c| ProtoOaSymbolCategory {
                id: class_id(c),
                asset_class_id: class_id(c),
                name: "Default Category".to_string(),
                ..Default::default()
            })
            .collect(),
        asset_classes: classes
            .iter()
            .map(|c| ProtoOaAssetClass {
                id: Some(class_id(c)),
                name: Some((*c).to_string()),
                ..Default::default()
            })
            .collect(),
        assets: assets
            .iter()
            .map(|a| ProtoOaAsset {
                asset_id: asset_id(a),
                name: (*a).to_string(),
                ..Default::default()
            })
            .collect(),
        deposit_asset_id: Some(asset_id(word(&doc["deposit"]))),
        account_type: None,
    }
}

fn market_buy(symbol: &str, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "cfd-1".into(),
        venue: "ctrader".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: OrderType::Market,
        ..Default::default()
    }
}

/// **The whole measured table, class by class**: fence, grid, wire volume, fill quantity and the booked PnL.
#[test]
fn every_measured_cfd_round_trip_is_admitted_and_booked_by_the_engines_own_formula() {
    let doc = fixture();
    let deposit = word(&doc["deposit"]);
    let map = SymbolMap::from_lists(&lists(&doc));
    let rows = instruments_from_map(&map);
    assert_eq!(rows.len(), trips(&doc).len());

    for t in trips(&doc) {
        let name = word(&t["symbol"]);
        let row = rows.iter().find(|r| r.raw_symbol == name).unwrap_or_else(|| panic!("{name}"));

        // 1. the class is one of the four the fence admits (never read from the name: the broker's asset class
        //    said so), spelled out here so the pin does not move with the list in the bridge.
        assert!(
            matches!(
                &row.contract,
                Contract::Cfd(s) if matches!(
                    s.underlying_kind,
                    CfdUnderlying::Metal | CfdUnderlying::Index | CfdUnderlying::Commodity | CfdUnderlying::Crypto
                )
            ),
            "{name}: {:?}",
            row.contract
        );
        assert_eq!(
            quoted_symbol_refusal(row, deposit),
            None,
            "{name}: the converting fence admits it"
        );
        let plain = any_symbol_refusal(row, deposit);
        assert_eq!(
            plain.is_none(),
            word(&t["quote"]).eq_ignore_ascii_case(deposit),
            "{name}: the plain fence admits exactly the deposit-quoted ones: {plain:?}"
        );

        // 2. the engine's grid is volume / 100 and carries no contract multiplier (the measured fact: the lot
        //    size is not the unit; XAUUSD's lot is 100 ounces and US500's is one point, both booked on volume / 100).
        let (min, step) =
            (int(&t["min_volume"]) as f64 / 100.0, int(&t["step_volume"]) as f64 / 100.0);
        assert_eq!((row.properties.min_qty, row.properties.step_size), (min, step), "{name}");
        assert_eq!(row.properties.contract_size, 0.0, "{name}: no lot-size scaling");
        assert_eq!(row.properties.multiplier(), 1.0, "{name}");

        // 3. the engine's quantity is the wire volume the broker filled, and back.
        let volume = int(&t["volume"]);
        let qty = volume as f64 / 100.0;
        let order = order_to_new_order(&market_buy(name, qty), 1, &map).expect("a mappable order");
        assert_eq!(order.volume, volume, "{name}: qty {qty} is volume {volume}");
        let event = ProtoOaExecutionEvent {
            deal: Some(ProtoOaDeal {
                symbol_id: order.symbol_id,
                filled_volume: volume,
                execution_price: Some(num(&t["entry"])),
                trade_side: ProtoOaTradeSide::Buy as i32,
                ..Default::default()
            }),
            ..Default::default()
        };
        let fill = deal_fill(&event, "cfd-1", &map, 2).expect("a fill");
        assert_eq!(
            (fill.symbol.as_str(), fill.last_qty, fill.last_px),
            (name, qty, num(&t["entry"]))
        );
        // 4. the engine's linear PnL, converted at the closing deal's rate (account currency per ONE unit of
        //    the quote currency), is the gross profit the broker booked, to the cent.
        let rate = num(&t["quote_to_deposit_rate"]);
        let value = ContractValue::default().in_account(rate).expect("a usable rate");
        let engine = value.unrealized_pnl(num(&t["entry"]), num(&t["exit"]), qty);
        let booked = num(&t["gross_profit"]);
        assert!(
            ((engine * 100.0).round() / 100.0 - booked).abs() < 1e-9,
            "{name}: engine {engine} vs booked {booked} (qty {qty}, rate {rate})"
        );
    }
}

/// The classes nobody measured stay refused by name, over the REAL capture of the demo broker's symbol universe
/// (`tests/fixtures/ctrader_catalog_capture.json`): bonds, and the dated-futures wrappers (`Futures`,
/// `Futures Commodities`), while the measured classes pass.
#[test]
fn the_unmeasured_classes_of_the_real_capture_stay_refused_and_the_measured_ones_pass() {
    use super::catalog_capture::{catalog, find};
    let out = catalog();
    for name in ["XAUUSD", "BRENT", "XNGUSD", "BTCUSD"] {
        assert_eq!(any_symbol_refusal(find(&out, name), "USD"), None, "{name}");
    }
    // AUS200 is quoted in AUD: the plain fence says so, the converting one admits it.
    let aud = any_symbol_refusal(find(&out, "AUS200"), "USD").expect("AUD index on a USD account");
    assert!(aud.contains("quoted in AUD"), "{aud}");
    assert_eq!(quoted_symbol_refusal(find(&out, "AUS200"), "USD"), None);

    for name in ["DXY_H6", "EURBBL_H6", "Wheat_H6"] {
        let why = quoted_symbol_refusal(find(&out, name), "USD").expect(name);
        assert!(
            why.contains(&format!("{name} is a")) && why.contains("not been measured"),
            "{name}: {why}"
        );
    }
}
