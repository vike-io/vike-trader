//! Unit pins for `crate::algo`: the New Algo Order golden, the open-list parse against the page's own
//! example body, the `ALGO_UPDATE` totality. The scripted-transport behaviour (routing, cancel,
//! lookup, reconcile) and the replay of the MEASURED trigger sequence through an engine are
//! `tests/offline/algo_orders.rs`.

use serde_json::json;
use vike_model::TriggerBy;
use vike_model::{OrderRequest, OrderStatus, OrderType, SymbolProperties};

use super::*;
use crate::perp::TIF_LANE;

fn props() -> SymbolProperties {
    SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() }
}

fn stop(trigger_by: Option<TriggerBy>) -> OrderRequest {
    OrderRequest {
        client_order_id: "vtr6stop".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: -1,
        qty: 0.0054,
        order_type: OrderType::Stop,
        trigger_price: Some(57999.94),
        reduce_only: true,
        trigger_by,
        ..Default::default()
    }
}

fn pairs(params: Vec<(&'static str, String)>) -> Vec<(String, String)> {
    params.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn want(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// THE golden: parameter NAMES and ORDER of a New Algo Order for a bracket stop-loss leg. `triggerPrice`
/// (not `stopPrice`), `clientAlgoId` (not `newClientOrderId`), the new `algoType`, no `timeInForce`
/// (a `STOP_MARKET` carries none), no `closePosition`/`priceProtect`; qty and trigger floored to the
/// grid by the pinned `format_to_step_f` site. `workingType` only when a source was asked for.
#[test]
fn the_new_algo_order_params_are_pinned() {
    let got = pairs(build_perp_algo_params(&stop(None), "BTCUSDT", &props(), TIF_LANE, None));
    assert_eq!(
        got,
        want(&[
            ("algoType", "CONDITIONAL"),
            ("symbol", "BTCUSDT"),
            ("side", "SELL"),
            ("type", "STOP_MARKET"),
            ("quantity", "0.005"),
            ("clientAlgoId", "vtr6stop"),
            ("newOrderRespType", "ACK"),
            ("positionSide", "BOTH"),
            ("reduceOnly", "true"),
            ("triggerPrice", "57999.9"),
        ])
    );

    let mark = pairs(build_perp_algo_params(
        &stop(Some(TriggerBy::Mark)),
        "BTCUSDT",
        &props(),
        TIF_LANE,
        Some("LINK"),
    ));
    assert_eq!(mark[5], ("clientAlgoId".to_string(), "x-LINK-vtr6stop".to_string()));
    assert_eq!(mark.last().unwrap(), &("workingType".to_string(), "MARK_PRICE".to_string()));
    assert_eq!(mark[mark.len() - 2].0, "triggerPrice", "workingType follows the trigger");
    assert_eq!(mark.len(), got.len() + 1);

    let last = pairs(build_perp_algo_params(
        &stop(Some(TriggerBy::Last)),
        "BTCUSDT",
        &props(),
        TIF_LANE,
        None,
    ));
    assert_eq!(last.last().unwrap().1, "CONTRACT_PRICE");
}

/// A buy-side, non-reduce-only stop flips exactly the two fields it should.
#[test]
fn side_and_reduce_only_follow_the_request() {
    let mut req = stop(None);
    req.side = 1;
    req.reduce_only = false;
    let got = pairs(build_perp_algo_params(&req, "BTCUSDT", &props(), TIF_LANE, None));
    let get = |k: &str| got.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.as_str());
    assert_eq!(get("side"), Some("BUY"));
    assert_eq!(get("reduceOnly"), Some("false"));
}

/// The broker-prefixed id must stay inside the venue's `clientAlgoId` pattern and length.
#[test]
fn the_client_algo_id_fits_the_venues_pattern() {
    let mut req = stop(None);
    req.client_order_id = "A".repeat(32);
    let got = pairs(build_perp_algo_params(&req, "BTCUSDT", &props(), TIF_LANE, Some("LINK123")));
    let id = &got.iter().find(|(k, _)| k == "clientAlgoId").unwrap().1;
    assert!(id.len() <= 36 && !id.is_empty(), "{id}");
    assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || ".:/_-".contains(c)), "{id}");
}

/// The page's own `Current All Algo Open Orders` example row, verbatim.
fn doc_open_row() -> serde_json::Value {
    json!({
        "algoId": 2148627, "clientAlgoId": "MRumok0dkhrP4kCm12AHaB", "algoType": "CONDITIONAL",
        "orderType": "TAKE_PROFIT", "symbol": "BNBUSDT", "side": "SELL", "positionSide": "BOTH",
        "timeInForce": "GTC", "quantity": "0.01", "algoStatus": "NEW", "actualOrderId": "",
        "actualPrice": "0.00000", "triggerPrice": "750.000", "price": "750.000",
        "icebergQuantity": null, "tpTriggerPrice": "0.000", "tpPrice": "0.000",
        "slTriggerPrice": "0.000", "slPrice": "0.000", "tpOrderType": "",
        "selfTradePreventionMode": "EXPIRE_MAKER", "workingType": "CONTRACT_PRICE",
        "priceMatch": "NONE", "closePosition": false, "priceProtect": false, "reduceOnly": false,
        "createTime": 1750514941540_i64, "updateTime": 1750514941540_i64, "triggerTime": 0,
        "goodTillDate": 0
    })
}

#[test]
fn an_open_algo_row_maps_to_a_resting_order() {
    let mo = map_perp_open_algo_order(&doc_open_row(), "binance");
    assert_eq!(mo.client_order_id(), "MRumok0dkhrP4kCm12AHaB");
    assert_eq!(mo.status, OrderStatus::Accepted);
    assert_eq!(mo.venue_order_id.as_deref(), Some("2148627"));
    assert_eq!(mo.request.symbol, "BNBUSDT");
    assert_eq!(mo.request.side, -1);
    assert_eq!(mo.request.qty, 0.01);
    assert_eq!(mo.request.order_type, OrderType::TakeProfit);
    assert_eq!(mo.request.trigger_price, Some(750.0));
    assert_eq!(mo.request.price, Some(750.0));
    assert!(!mo.request.reduce_only);
}

#[test]
fn the_open_list_parses_to_reports_on_the_series_label() {
    let body = json!([doc_open_row()]).to_string();
    let reports = parse_perp_open_algo_orders(&body, "binance").unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.symbol, "BNBUSDT.P");
    assert_eq!(&*r.venue_order_id, "2148627");
    assert_eq!(r.client_order_id.as_deref(), Some("MRumok0dkhrP4kCm12AHaB"));
    assert_eq!((r.side, r.qty, r.filled_qty), (-1, 0.01, 0.0));
    assert_eq!(r.order_type, OrderType::TakeProfit);
    assert_eq!(r.status, OrderStatus::Accepted);
    assert_eq!(r.ts, 1750514941540);
    assert!(parse_perp_open_algo_orders("not json", "binance").is_err());
    assert!(parse_perp_open_algo_orders("{}", "binance").is_err(), "a non-array is no list");
}

/// The row kinds the report FSM and the order kinds agree on; an unknown kind keeps its text.
#[test]
fn the_algo_order_kinds_map_like_the_family_report_parser() {
    for (raw, want) in [
        ("STOP_MARKET", OrderType::Stop),
        ("STOP", OrderType::StopLimit),
        ("TAKE_PROFIT_MARKET", OrderType::TakeProfit),
        ("TAKE_PROFIT", OrderType::TakeProfit),
        ("TRAILING_STOP_MARKET", OrderType::Unrecognized("trailing_stop_market".into())),
    ] {
        assert_eq!(algo_order_type(raw), want, "{raw}");
    }
}

/// A non-finite quantity must read `0.0`, not panic (the regular open-order map's regression).
#[test]
fn a_hostile_row_does_not_panic() {
    for qty in ["NaN", "inf", "-inf", "1e999", ""] {
        let mo = map_perp_open_algo_order(&json!({"quantity": qty, "side": "BUY"}), "binance");
        assert_eq!(mo.request.qty, 0.0, "{qty}");
    }
    assert!(parse_perp_open_algo_orders(r#"[{}, null, 3, "x"]"#, "binance").is_ok());
}

#[test]
fn only_the_two_order_endpoint_words_are_unknown_order() {
    assert!(is_unknown_order(-2011) && is_unknown_order(-2013));
    for code in [0, -1003, -1021, -2010, -2015, -2021, -4117, -4120, 400, 404, 503] {
        assert!(!is_unknown_order(code), "{code}");
    }
}

#[test]
fn the_coid_set_remembers_and_forgets() {
    let set = AlgoCoids::default();
    assert!(!set.contains("a"));
    set.insert("a");
    assert!(set.contains("a") && !set.contains("b"));
    set.remove("a");
    assert!(!set.contains("a"));
}

/// Totality: any junk becomes no events, never a panic.
#[test]
fn junk_frames_map_to_nothing() {
    for frame in [
        json!(null),
        json!([]),
        json!({"e": "ALGO_UPDATE"}),
        json!({"e": "ALGO_UPDATE", "o": 3}),
        json!({"e": "ALGO_UPDATE", "o": {}}),
        json!({"e": "ALGO_UPDATE", "o": {"X": "CANCELED"}}),
        json!({"e": "ALGO_UPDATE", "o": {"caid": "", "X": "TRIGGERED", "ai": "9"}}),
        json!({"e": "ALGO_UPDATE", "o": {"caid": "sl", "X": "FINISHED"}}),
        json!({"e": "ALGO_UPDATE", "o": {"caid": "sl", "X": "nonsense"}}),
    ] {
        assert!(map_algo_update(&frame).is_empty(), "{frame}");
    }
}

/// `NEW` is the stream's second ack: `OrderAccepted` with the frame's algo id, whatever JSON type the
/// venue spells it in (the capture shows an integer), and with no id the order is still accepted.
#[test]
fn new_accepts_with_the_algo_id_whatever_its_json_type() {
    for (aid, want) in [
        (json!(2148719), Some("2148719")),
        (json!("2148719"), Some("2148719")),
        (json!(null), None),
    ] {
        let frame =
            json!({"e": "ALGO_UPDATE", "T": 1, "o": {"caid": "x-LINK-sl", "X": "NEW", "aid": aid}});
        match &map_algo_update(&frame)[..] {
            [Event::OrderAccepted(a)] => {
                assert_eq!(a.client_order_id, "sl");
                assert_eq!(a.venue_order_id.as_deref(), want);
            }
            other => panic!("{other:?}"),
        }
    }
}

/// The resync's rows: only the terminal states replay (with the reconnect-gap reason), a stripped
/// broker prefix, the row's `updateTime`; live, triggered and junk rows replay nothing and never panic.
#[test]
fn history_rows_replay_only_their_terminal_states() {
    let rows = json!([
        {"clientAlgoId": "x-LINK-a", "algoStatus": "CANCELED", "updateTime": 5},
        {"clientAlgoId": "b", "algoStatus": "EXPIRED", "updateTime": 6},
        {"clientAlgoId": "c", "algoStatus": "REJECTED", "updateTime": 7},
        {"clientAlgoId": "d", "algoStatus": "NEW"},
        {"clientAlgoId": "e", "algoStatus": "TRIGGERED"},
        {"clientAlgoId": "f", "algoStatus": "FINISHED"},
        {"algoStatus": "CANCELED"},
        {"clientAlgoId": "", "algoStatus": "CANCELED"},
        null, 3, "x"
    ]);
    match &map_algo_history(&rows)[..] {
        [Event::OrderCanceled(a), Event::OrderExpired(b), Event::OrderCanceled(c)] => {
            assert_eq!((a.client_order_id.as_str(), a.ts), ("a", 5));
            assert!(a.reason.contains("closed during reconnect gap"), "{}", a.reason);
            assert_eq!((b.client_order_id.as_str(), b.ts), ("b", 6));
            assert_eq!((c.client_order_id.as_str(), c.ts), ("c", 7));
            assert!(c.reason.contains("rejected during reconnect gap"), "{}", c.reason);
        }
        other => panic!("{other:?}"),
    }
    for body in [json!(null), json!({}), json!("x"), json!([])] {
        assert!(map_algo_history(&body).is_empty(), "{body}");
    }
}
