//! R6 slice-6/7 golden parity: the Deribit adapter against the FROZEN `fixtures/r6/deribit.json`
//! bytes — JSON-RPC auth/subscribe frames, option instruments, order params (the post_only:false
//! trap, numeric amount/price), the fills-only user.trades mapper (signed fee kept as-is), and the
//! frozen reconcile getter results (signed size incl. SHORT) read through the `ReconcileClient` parsers
//! production runs.
//!
//! Those bytes were exported from the PySide6 vterminal app before it was retired: that is
//! PROVENANCE, not a live comparison. This header read "vs the Python oracle"; no exporter
//! survives in this tree and nothing here consults Python at run time, so what the exact
//! assertions below now claim is that THIS port's arithmetic has not moved unnoticed
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`).

use vike_deribit::execution::event_mapper::{map_deribit_private, map_deribit_trade};
use vike_deribit::execution::rest::{VenueRestDeribit, parse_deribit_option_instruments};
use vike_deribit::transport::ws_auth::{
    build_client_credentials_auth, build_private_subscribe, build_refresh_token_auth,
};
use vike_model::f64_to_hex_bits;

fn fixture() -> serde_json::Value {
    vike_bridge_core::execution_drive::r6_fixture(env!("CARGO_MANIFEST_DIR"), "deribit.json")
}

#[test]
fn websocket_frames_parity() {
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
    for (name, instrument) in &instruments {
        let w = &want[name];
        for (field, val) in [
            ("tick_size", instrument.properties.tick_size),
            ("step_size", instrument.properties.step_size),
            ("min_qty", instrument.properties.min_qty),
            ("max_qty", instrument.properties.max_qty),
            ("min_notional", instrument.properties.min_notional),
            ("contract_size", instrument.contract_size),
        ] {
            assert_eq!(f64_to_hex_bits(val), w[field].as_str().unwrap(), "{name}.{field}");
        }
        assert_eq!(instrument.base_asset, w["base_asset"].as_str().unwrap());
    }
}

#[test]
fn order_params_parity() {
    let fx = fixture();
    let instruments = parse_deribit_option_instruments(&fx["instruments_payload"]);
    let instrument = &instruments["BTC-8JUL26-62000-C"];
    let transport =
        vike_deribit::transport::client::DeribitTransport::new("wss://unused", "x", "y", None);
    let client =
        VenueRestDeribit::new(transport, "BTC-8JUL26-62000-C", instrument.properties, "BTC");
    let limit_request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "vtr6dbL", "venue": "deribit", "symbol": "BTC-8JUL26-62000-C",
        "side": 1, "qty": 0.30000000000000004, "order_type": "limit", "price": 0.03330000001
    }))
    .unwrap();
    let market_request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "vtr6dbM", "venue": "deribit", "symbol": "BTC-8JUL26-62000-C",
        "side": -1, "qty": 0.1, "order_type": "market", "reduce_only": true
    }))
    .unwrap();
    // Value-compare (Python emitted plain dicts; both sides carry numeric amount/price)
    assert_eq!(client.build_order_params(&limit_request), fx["order_params"]["limit"]);
    assert_eq!(client.build_order_params(&market_request), fx["order_params"]["market"]);
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

// `reconcile_parity` retired (unit A2): it read the frozen `reconcile` results through the legacy
// snapshot builder, deleted with the dead `Command::ApplySnapshot` path; the same frozen results stay
// pinned through production's parsers below.

/// The frozen `reconcile` getter results through the parsers production runs, `ReconcileClientDeribit`'s
/// (`crates/bridges/deribit/src/reconcile/mod.rs`'s `parse_positions` and `parse_open_orders`). Their
/// `snapshot` bytes were exported from the legacy snapshot builder (deleted in unit A2); where
/// production's answer differs from them:
///
/// - **positions:** production keeps the instrument's row even when flat, and reports NOTHING for an
///   instrument the answer does not list (`other_instrument`); the frozen snapshot drops a flat row
///   and holds one zero row for the client's instrument in both cases. A live row agrees bit for
///   bit: signed size, average price. Production carries no mark price;
/// - **open orders:** production reports EVERY row it is given: an external order (empty `label`)
///   as one with no client id, and a row whose `order_state` is not `open`; the snapshot skips both.
///   The rows both keep agree on id, order client id, side, amount, filled amount, average fill price and
///   status; the kind differs in spelling only (`stop_market` is production's `OrderType::Stop`, the
///   snapshot keeps the venue word). Production carries no limit price;
/// - **balance:** the snapshot carries none (0.0); production reads `private/get_account_summary`,
///   which these frozen bytes do not hold.
#[test]
fn production_reads_the_frozen_results_through_the_reconcile_parsers() {
    use vike_deribit::reconcile::{parse_open_orders, parse_positions};
    use vike_model::OrderType;
    const INSTRUMENT: &str = "BTC-8JUL26-62000-C";
    let fx = fixture();
    for case in fx["reconcile"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let want = &case["snapshot"];

        let rows = case["positions_result"].as_array().unwrap();
        let reports =
            parse_positions(&case["positions_result"].to_string(), INSTRUMENT, 0).unwrap();
        let listed = rows.iter().filter(|r| r["instrument_name"] == INSTRUMENT).count();
        assert_eq!(reports.len(), listed, "{name}: the instrument's rows, a flat one included");
        let live: Vec<_> = reports.iter().filter(|r| r.qty != 0.0).collect();
        let legs: Vec<(&str, &str)> = want["positions"]
            .as_array()
            .unwrap()
            .iter()
            .zip(want["position_average_price"].as_array().unwrap())
            .map(|(q, a)| (q[1].as_str().unwrap(), a[1].as_str().unwrap()))
            .filter(|(q, _)| *q != "0000000000000000")
            .collect();
        assert_eq!(live.len(), legs.len(), "{name}: the live legs");
        for (r, (qty, avg)) in live.iter().zip(&legs) {
            assert_eq!(f64_to_hex_bits(r.qty), *qty, "{name}: qty");
            assert_eq!(f64_to_hex_bits(r.average_price), *avg, "{name}: avg_px");
        }

        let orders = parse_open_orders(&case["orders_result"].to_string()).unwrap();
        assert_eq!(
            orders.len(),
            case["orders_result"].as_array().unwrap().len(),
            "{name}: every row"
        );
        for w in want["open_orders"].as_array().unwrap() {
            let id = w["venue_order_id"].as_str().unwrap();
            let r = orders.iter().find(|r| r.venue_order_id.as_str() == id).expect("kept by both");
            assert_eq!(r.order_client_id.as_deref(), w["coid"].as_str(), "{name} {id}");
            assert_eq!(r.status.as_str(), w["status"].as_str().unwrap(), "{name} {id}");
            assert_eq!(i64::from(r.side), w["side"].as_i64().unwrap(), "{name} {id}");
            assert_eq!(f64_to_hex_bits(r.qty), w["qty"].as_str().unwrap(), "{name} {id}");
            assert_eq!(f64_to_hex_bits(r.filled_qty), w["filled_qty"].as_str().unwrap());
            assert_eq!(f64_to_hex_bits(r.average_price), w["average_fill_price"].as_str().unwrap());
            match w["order_type"].as_str().unwrap() {
                "stop_market" => assert_eq!(r.order_type, OrderType::Stop, "{name} {id}"),
                word => assert_eq!(r.order_type.as_str(), word, "{name} {id}"),
            }
        }
        let extra: Vec<&str> = orders
            .iter()
            .map(|r| r.venue_order_id.as_str())
            .filter(|id| {
                !want["open_orders"].as_array().unwrap().iter().any(|w| w["venue_order_id"] == *id)
            })
            .collect();
        if name == "long_with_orders" {
            assert_eq!(extra, ["D-2", "D-3"], "the external order and the filled row");
            let external = orders.iter().find(|r| r.venue_order_id.as_str() == "D-2").unwrap();
            assert_eq!(external.order_client_id, None, "an empty label is no client id");
        } else {
            assert!(extra.is_empty(), "{name}: {extra:?}");
        }
    }
}
