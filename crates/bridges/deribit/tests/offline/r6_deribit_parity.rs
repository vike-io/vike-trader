//! R6 slice-6/7 golden parity: the Deribit adapter against the FROZEN `fixtures/r6/deribit.json`
//! bytes — JSON-RPC auth/subscribe frames, option instruments, order params (the post_only:false
//! trap, numeric amount/price), the fills-only user.trades mapper (signed fee kept as-is), and the
//! reconcile builder (signed size incl. SHORT, empty-label skip, "market_price" → None, partial
//! seeding).
//!
//! Those bytes were exported from the PySide6 vterminal app before it was retired: that is
//! PROVENANCE, not a live comparison. This header read "vs the Python oracle"; no exporter
//! survives in this tree and nothing here consults Python at run time, so what the exact
//! assertions below now claim is that THIS port's arithmetic has not moved unnoticed
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`).

use std::path::PathBuf;
use vike_deribit::client::{DeribitRest, parse_deribit_option_instruments};
use vike_deribit::event_mapper::{map_deribit_private, map_deribit_trade};
use vike_deribit::reconcile::build_reconcile_snapshot;
use vike_deribit::ws_auth::{
    build_client_credentials_auth, build_private_subscribe, build_refresh_token_auth,
};
use vike_model::f64_to_hex_bits;

fn fixture() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/r6/deribit.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[test]
fn ws_frames_parity() {
    let fx = fixture();
    let frames = &fx["ws_frames"];
    assert_eq!(build_client_credentials_auth("cid-XXXX", "csec-YYYY", None, 1), frames["auth"]);
    assert_eq!(
        build_client_credentials_auth(
            "cid-XXXX",
            "csec-YYYY",
            Some("session:vike trade:read_write"),
            2
        ),
        frames["auth_scoped"]
    );
    assert_eq!(build_refresh_token_auth("rt-ZZZZ", 3), frames["refresh"]);
    assert_eq!(
        build_private_subscribe(&["user.trades.option.BTC.raw".to_string()], 4),
        frames["subscribe"]
    );
}

#[test]
fn instruments_parity() {
    let fx = fixture();
    let instruments = parse_deribit_option_instruments(&fx["instruments_payload"]);
    let want = fx["instruments"].as_object().unwrap();
    assert_eq!(instruments.len(), want.len(), "future + non-option rows skipped");
    for (name, inst) in &instruments {
        let w = &want[name];
        for (field, val) in [
            ("tick_size", inst.properties.tick_size),
            ("step_size", inst.properties.step_size),
            ("min_qty", inst.properties.min_qty),
            ("max_qty", inst.properties.max_qty),
            ("min_notional", inst.properties.min_notional),
            ("contract_size", inst.contract_size),
        ] {
            assert_eq!(f64_to_hex_bits(val), w[field].as_str().unwrap(), "{name}.{field}");
        }
        assert_eq!(inst.base_asset, w["base_asset"].as_str().unwrap());
    }
}

#[test]
fn order_params_parity() {
    let fx = fixture();
    let instruments = parse_deribit_option_instruments(&fx["instruments_payload"]);
    let inst = &instruments["BTC-8JUL26-62000-C"];
    let transport =
        vike_deribit::transport::DeribitOrderTransport::new("wss://unused", "x", "y", None);
    let client = DeribitRest::new(transport, "BTC-8JUL26-62000-C", inst.properties, "BTC");
    let limit_req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "vtr6dbL", "venue": "deribit", "symbol": "BTC-8JUL26-62000-C",
        "side": 1, "qty": 0.30000000000000004, "order_type": "limit", "price": 0.03330000001
    }))
    .unwrap();
    let market_req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "vtr6dbM", "venue": "deribit", "symbol": "BTC-8JUL26-62000-C",
        "side": -1, "qty": 0.1, "order_type": "market", "reduce_only": true
    }))
    .unwrap();
    // Value-compare (Python emitted plain dicts; both sides carry numeric amount/price)
    assert_eq!(client.build_order_params(&limit_req), fx["order_params"]["limit"]);
    assert_eq!(client.build_order_params(&market_req), fx["order_params"]["market"]);
}

#[test]
fn mapper_parity() {
    let fx = fixture();
    for (i, case) in fx["mapper"].as_array().unwrap().iter().enumerate() {
        let got: Vec<serde_json::Value> =
            map_deribit_private(&case["frame"], "deribit", "BTC-8JUL26-62000-C")
                .iter()
                .map(|e| serde_json::to_value(e).unwrap())
                .collect();
        let want = case["events"].as_array().unwrap();
        assert_eq!(got.len(), want.len(), "mapper[{i}] count: {got:?}");
        for (g, w) in got.iter().zip(want) {
            assert_eq!(g, w, "mapper[{i}]");
        }
    }
    assert!(map_deribit_private(&serde_json::json!("ping"), "deribit", "X").is_empty());
}

#[test]
fn trade_surfaces_the_fee_currency() {
    // Deribit trade objects carry `fee_currency` alongside the signed `fee`.
    let item = serde_json::json!({
        "trade_id": "t1", "label": "c1", "instrument_name": "BTC-8JUL26-62000-C",
        "direction": "buy", "amount": "1", "price": "0.05", "fee": "0.0001",
        "fee_currency": "BTC", "state": "filled", "timestamp": 5, "liquidity": "T"
    });
    let fill = map_deribit_trade(&item, "deribit", "BTC-8JUL26-62000-C")
        .into_iter()
        .find_map(|e| match e {
            vike_model::events::Event::OrderFilled(f) => Some(f.fill),
            vike_model::events::Event::OrderPartiallyFilled(f) => Some(f.fill),
            _ => None,
        })
        .expect("a fill event");
    assert_eq!(fill.commission, 0.0001);
    assert_eq!(fill.commission_asset, "BTC");
}

#[test]
fn reconcile_parity() {
    let fx = fixture();
    for case in fx["reconcile"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let snap = build_reconcile_snapshot(
            &case["positions_result"],
            &case["orders_result"],
            "BTC-8JUL26-62000-C",
        );
        let want = &case["snapshot"];
        let check = |got: &[(String, f64)], want: &serde_json::Value, what: &str| {
            for ((sym, v), w) in got.iter().zip(want.as_array().unwrap()) {
                assert_eq!(sym, w[0].as_str().unwrap());
                assert_eq!(f64_to_hex_bits(*v), w[1].as_str().unwrap(), "{name}: {what}");
            }
        };
        check(&snap.positions, &want["positions"], "positions");
        check(&snap.position_avg_px, &want["position_avg_px"], "avg");
        check(&snap.position_mark_px, &want["position_mark_px"], "mark");
        let want_orders = want["open_orders"].as_array().unwrap();
        assert_eq!(snap.open_orders.len(), want_orders.len(), "{name}: order count");
        for (mo, w) in snap.open_orders.iter().zip(want_orders) {
            assert_eq!(mo.client_order_id(), w["coid"].as_str().unwrap(), "{name}");
            assert_eq!(mo.status.as_str(), w["status"].as_str().unwrap(), "{name}");
            assert_eq!(mo.venue_order_id.as_deref(), w["venue_order_id"].as_str());
            assert_eq!(i64::from(mo.request.side), w["side"].as_i64().unwrap());
            assert_eq!(f64_to_hex_bits(mo.request.qty), w["qty"].as_str().unwrap());
            assert_eq!(mo.request.order_type, w["order_type"].as_str().unwrap());
            match (mo.request.price, w["price"].as_str()) {
                (Some(p), Some(hex)) => assert_eq!(f64_to_hex_bits(p), hex, "{name} price"),
                (None, None) => {}
                (a, b) => panic!("{name}: price presence {a:?} vs {b:?}"),
            }
            assert_eq!(f64_to_hex_bits(mo.filled_qty), w["filled_qty"].as_str().unwrap());
            assert_eq!(f64_to_hex_bits(mo.avg_fill_px), w["avg_fill_px"].as_str().unwrap());
        }
        if name == "short" {
            assert!(snap.positions[0].1 < 0.0, "SHORT stays signed (never re-signed)");
        }
    }
}
