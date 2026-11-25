//! Unit pins for `crate::execution::rest::algo`: the New Algo Order golden, the open-list parse against the page's own
//! example body, the `ALGO_UPDATE` totality. The scripted-transport behaviour (routing, cancel,
//! lookup, reconcile) and the replay of the MEASURED trigger sequence through an engine are
//! `tests/offline/algo_orders.rs`.

use std::time::Duration;

use serde_json::json;
use vike_bridge_core::reconcile_naming::ALGO_NAMING_STRICT_FAILURES;
use vike_model::TriggerBy;
use vike_model::{OrderRequest, OrderStatus, OrderType, SymbolProperties};

use super::*;
use crate::execution::rest::perp::TIME_IN_FORCE_LANE;

fn properties() -> SymbolProperties {
    SymbolProperties { tick_size: 0.1, step_size: 0.001, ..Default::default() }
}

fn stop(trigger_by: Option<TriggerBy>) -> OrderRequest {
    OrderRequest {
        order_client_id: "vtr6stop".to_string(),
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
    let got = pairs(build_perp_algo_params(
        &stop(None),
        "BTCUSDT",
        &properties(),
        TIME_IN_FORCE_LANE,
        None,
    ));
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
        &properties(),
        TIME_IN_FORCE_LANE,
        Some("LINK"),
    ));
    assert_eq!(mark[5], ("clientAlgoId".to_string(), "x-LINK-vtr6stop".to_string()));
    assert_eq!(mark.last().unwrap(), &("workingType".to_string(), "MARK_PRICE".to_string()));
    assert_eq!(mark[mark.len() - 2].0, "triggerPrice", "workingType follows the trigger");
    assert_eq!(mark.len(), got.len() + 1);

    let last = pairs(build_perp_algo_params(
        &stop(Some(TriggerBy::Last)),
        "BTCUSDT",
        &properties(),
        TIME_IN_FORCE_LANE,
        None,
    ));
    assert_eq!(last.last().unwrap().1, "CONTRACT_PRICE");
}

/// A take-profit is the stop's golden with its own `type`: `TAKE_PROFIT_MARKET` with no limit (no
/// `price`, no `timeInForce`), `TAKE_PROFIT` with its limit in `price` and `timeInForce=GTC` after
/// `triggerPrice`, `workingType` still the tail. A stop and a stop-limit keep their bytes.
#[test]
fn the_take_profit_algo_params_are_pinned() {
    let take_profit = |price: Option<f64>| OrderRequest {
        order_type: OrderType::TakeProfit,
        trigger_price: Some(62_000.06),
        price,
        trigger_by: Some(TriggerBy::Mark),
        ..stop(None)
    };
    let head = [
        ("algoType", "CONDITIONAL"),
        ("symbol", "BTCUSDT"),
        ("side", "SELL"),
        ("type", "TAKE_PROFIT_MARKET"),
        ("quantity", "0.005"),
        ("clientAlgoId", "vtr6stop"),
        ("newOrderRespType", "ACK"),
        ("positionSide", "BOTH"),
        ("reduceOnly", "true"),
        ("triggerPrice", "62000.0"),
    ];
    let mut market = head.to_vec();
    market.push(("workingType", "MARK_PRICE"));
    let got = pairs(build_perp_algo_params(
        &take_profit(None),
        "BTCUSDT",
        &properties(),
        TIME_IN_FORCE_LANE,
        None,
    ));
    assert_eq!(got, want(&market));

    let mut limit = head.to_vec();
    limit[3] = ("type", "TAKE_PROFIT");
    limit.extend([("price", "61990.0"), ("timeInForce", "GTC"), ("workingType", "MARK_PRICE")]);
    let got = pairs(build_perp_algo_params(
        &take_profit(Some(61_990.04)),
        "BTCUSDT",
        &properties(),
        TIME_IN_FORCE_LANE,
        None,
    ));
    assert_eq!(got, want(&limit));
}

/// A PERCENT trailing stop: `TRAILING_STOP_MARKET`, the percent as `callbackRate` (the page's "1 for 1%")
/// and the activation, when stated, as `activatePrice` on the tick grid, both where a stop's `triggerPrice`
/// sits; no `triggerPrice`, no `price`, no `timeInForce`; `workingType` still the tail. A stop keeps its bytes.
#[test]
fn the_percent_trailing_algo_params_are_pinned() {
    let trailing = |activation: Option<f64>| OrderRequest {
        order_type: OrderType::Trailing,
        trigger_price: activation,
        trail_percent: Some(1.5),
        trigger_by: Some(TriggerBy::Mark),
        ..stop(None)
    };
    let mut rows = vec![
        ("algoType", "CONDITIONAL"),
        ("symbol", "BTCUSDT"),
        ("side", "SELL"),
        ("type", "TRAILING_STOP_MARKET"),
        ("quantity", "0.005"),
        ("clientAlgoId", "vtr6stop"),
        ("newOrderRespType", "ACK"),
        ("positionSide", "BOTH"),
        ("reduceOnly", "true"),
        ("callbackRate", "1.5"),
    ];
    let with_tail = |head: &[(&str, &str)]| {
        let mut all = head.to_vec();
        all.push(("workingType", "MARK_PRICE"));
        want(&all)
    };
    let got = pairs(build_perp_algo_params(
        &trailing(None),
        "BTCUSDT",
        &properties(),
        TIME_IN_FORCE_LANE,
        None,
    ));
    assert_eq!(got, with_tail(&rows));
    rows.push(("activatePrice", "64000.0"));
    let armed = trailing(Some(64_000.06));
    let got =
        pairs(build_perp_algo_params(&armed, "BTCUSDT", &properties(), TIME_IN_FORCE_LANE, None));
    assert_eq!(got, with_tail(&rows));
    for (percent, wire) in [(0.1, "0.1"), (1.0, "1"), (20.0, "20"), (2.25, "2.25")] {
        let req = OrderRequest { trail_percent: Some(percent), ..trailing(None) };
        let got =
            pairs(build_perp_algo_params(&req, "BTCUSDT", &properties(), TIME_IN_FORCE_LANE, None));
        assert_eq!(got[9], ("callbackRate".to_string(), wire.to_string()), "{percent}");
    }
    assert!(is_algo_kind(&OrderType::Trailing), "the venue refuses it on /fapi/v1/order (-4120)");
    assert!(!is_algo_kind(&OrderType::TrailingLimit) && !is_algo_kind(&OrderType::Limit));
}

/// `TRAILING_STOP_MARKET` reads back as a `trailing` on the reconcile report and on the by-order-client-id lookup's
/// kind ([`algo_order_type`]); every other word is the family's. ⚠ Not a capture: the page's row with
/// that word.
#[test]
fn a_trailing_row_reads_back_as_a_trailing_stop() {
    assert_eq!(algo_order_type("TRAILING_STOP_MARKET"), OrderType::Trailing);
    for raw in ["STOP_MARKET", "STOP", "TAKE_PROFIT_MARKET", "TAKE_PROFIT", "LIMIT", ""] {
        assert_eq!(algo_order_type(raw), conditional_order_type(raw), "{raw}");
    }
    let mut row = doc_open_row();
    row["orderType"] = json!("TRAILING_STOP_MARKET");
    row["price"] = json!("0.000");
    row["triggerPrice"] = json!("0.000");
    row["activatePrice"] = json!("760.000");
    row["callbackRate"] = json!("1.5");
    let reports = parse_perp_open_algo_orders(&json!([row]).to_string(), "binance", None).unwrap();
    assert_eq!(reports[0].order_type, OrderType::Trailing);
    assert_eq!(reports[0].order_client_id.as_deref(), Some("MRumok0dkhrP4kCm12AHaB"));
    // its terms: the activation from `activatePrice`, the percent from `callbackRate`, no price
    let r = &reports[0];
    assert_eq!(
        (r.price, r.trigger_price, r.trail, r.trail_percent),
        (None, Some(760.0), None, Some(1.5))
    );
}

/// A buy-side, non-reduce-only stop flips exactly the two fields it should.
#[test]
fn side_and_reduce_only_follow_the_request() {
    let mut req = stop(None);
    req.side = 1;
    req.reduce_only = false;
    let got =
        pairs(build_perp_algo_params(&req, "BTCUSDT", &properties(), TIME_IN_FORCE_LANE, None));
    let get = |k: &str| got.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.as_str());
    assert_eq!(get("side"), Some("BUY"));
    assert_eq!(get("reduceOnly"), Some("false"));
}

/// The broker-prefixed id must stay inside the venue's `clientAlgoId` pattern and length: prefixed
/// when the whole id fits 35, the order client id BARE and whole when it does not (never cut).
#[test]
fn the_client_algo_id_fits_the_venues_pattern() {
    for (order_client_id, want) in [
        ("S00A3k3x9q2m7d1p4CSSL", "x-LINK123-S00A3k3x9q2m7d1p4CSSL".to_string()),
        ("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "A".repeat(32)),
    ] {
        let mut req = stop(None);
        req.order_client_id = order_client_id.to_string();
        let got = pairs(build_perp_algo_params(
            &req,
            "BTCUSDT",
            &properties(),
            TIME_IN_FORCE_LANE,
            Some("LINK123"),
        ));
        let id = &got.iter().find(|(k, _)| k == "clientAlgoId").unwrap().1;
        assert_eq!(id, &want);
        assert!(id.len() <= 35 && !id.is_empty(), "{id}");
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || ".:/_-".contains(c)), "{id}");
    }
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

/// B-16 (owner ruling): every `algoStatus` word the Algo Service documents reads its status with no
/// line; a word it does not document is still read live (`Accepted`) but WARNED, as every other status
/// table reads one.
#[test]
fn an_unknown_algo_status_word_is_live_and_warned() {
    let read = |word: &str| {
        let mut row = doc_open_row();
        row["algoStatus"] = json!(word);
        let body = json!([row]).to_string();
        let (reports, logged) = vike_log::capture::captured(|| {
            parse_perp_open_algo_orders(&body, "binance", None).unwrap()
        });
        let warned = logged
            .iter()
            .filter(|e| e.level == tracing::Level::WARN && e.field("status") == Some(word))
            .count();
        (reports[0].status, warned)
    };
    for (word, want) in [
        ("NEW", OrderStatus::Accepted),
        ("TRIGGERING", OrderStatus::Accepted),
        ("TRIGGERED", OrderStatus::Accepted),
        ("FINISHED", OrderStatus::Accepted),
        ("CANCELED", OrderStatus::Canceled),
        ("EXPIRED", OrderStatus::Expired),
        ("REJECTED", OrderStatus::Rejected),
    ] {
        assert_eq!(read(word), (want, 0), "{word}");
    }
    assert_eq!(read("MYSTERY"), (OrderStatus::Accepted, 1));
}

#[test]
fn the_open_list_parses_to_reports_on_the_series_label() {
    let body = json!([doc_open_row()]).to_string();
    let reports = parse_perp_open_algo_orders(&body, "binance", None).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.symbol, "BNBUSDT.P");
    assert_eq!(&*r.venue_order_id, "2148627");
    assert_eq!(r.order_client_id.as_deref(), Some("MRumok0dkhrP4kCm12AHaB"));
    assert_eq!((r.side, r.qty, r.filled_qty), (-1, 0.01, 0.0));
    assert_eq!(r.order_type, OrderType::TakeProfit);
    assert_eq!(
        (r.price, r.trigger_price, r.trail, r.trail_percent),
        (Some(750.0), Some(750.0), None, None)
    );
    assert_eq!(r.status, OrderStatus::Accepted);
    assert_eq!(r.timestamp, 1750514941540);
    assert!(parse_perp_open_algo_orders("not json", "binance", None).is_err());
    assert!(parse_perp_open_algo_orders("{}", "binance", None).is_err(), "a non-array is no list");
}

/// A resting MARKET take-profit (`TAKE_PROFIT_MARKET`, the venue's zero `price`): the page's row with
/// that word reads as a take-profit with no limit on the reconcile report. ⚠ Not a capture.
#[test]
fn a_market_take_profit_row_reads_as_a_take_profit_without_a_limit() {
    let mut row = doc_open_row();
    row["orderType"] = json!("TAKE_PROFIT_MARKET");
    row["price"] = json!("0.000");
    let reports = parse_perp_open_algo_orders(&json!([row]).to_string(), "binance", None).unwrap();
    assert_eq!(reports[0].order_type, OrderType::TakeProfit);
    assert_eq!((reports[0].trigger_price, reports[0].price), (Some(750.0), None));
    assert_eq!(reports[0].order_client_id.as_deref(), Some("MRumok0dkhrP4kCm12AHaB"));
}

/// The row kinds the report FSM and the order kinds agree on; an unknown kind keeps its text. The algo
/// rows read the conditional words through `family::recon::conditional_order_type`, the report parser
/// reads its spot and plain words first and then the same fn: every word of both, pinned.
#[test]
fn the_algo_order_kinds_map_like_the_family_report_parser() {
    use crate::family::recon::normalize_order_type;
    for (raw, want) in [
        ("STOP_MARKET", OrderType::Stop),
        ("STOP", OrderType::StopLimit),
        ("TAKE_PROFIT_MARKET", OrderType::TakeProfit),
        ("TAKE_PROFIT", OrderType::TakeProfit),
        ("TRAILING_STOP_MARKET", OrderType::Unrecognized("trailing_stop_market".into())),
    ] {
        assert_eq!(conditional_order_type(raw), want, "{raw}");
        assert_eq!(normalize_order_type(raw, false), want, "{raw}");
    }
    for (raw, want) in [
        ("LIMIT", OrderType::Limit),
        ("LIMIT_MAKER", OrderType::Limit),
        ("MARKET", OrderType::Market),
        ("STOP_LOSS", OrderType::Stop),
        ("STOP_LOSS_LIMIT", OrderType::StopLimit),
        ("TAKE_PROFIT_LIMIT", OrderType::TakeProfit),
    ] {
        assert_eq!(normalize_order_type(raw, false), want, "{raw}");
        let not_algo = OrderType::Unrecognized(raw.to_ascii_lowercase());
        assert_eq!(conditional_order_type(raw), not_algo, "{raw} is no algo row's word");
    }
    // a spot row carrying `trailingDelta`: the two words a percent trail is sent as read `trailing`,
    // the two limit words (a FIXED limit) keep their kind
    for (raw, want) in [
        ("STOP_LOSS", OrderType::Trailing),
        ("TAKE_PROFIT", OrderType::Trailing),
        ("STOP_LOSS_LIMIT", OrderType::StopLimit),
        ("TAKE_PROFIT_LIMIT", OrderType::TakeProfit),
        ("LIMIT", OrderType::Limit),
    ] {
        assert_eq!(normalize_order_type(raw, true), want, "{raw} with a trailingDelta");
    }
}

/// A non-finite quantity must read `0.0`, not panic (the regular open-order map's regression).
#[test]
fn a_hostile_row_does_not_panic() {
    for qty in ["NaN", "inf", "-inf", "1e999", ""] {
        let body = json!([{"quantity": qty, "side": "BUY"}]).to_string();
        let reports = parse_perp_open_algo_orders(&body, "binance", None).unwrap();
        assert_eq!(reports[0].qty, 0.0, "{qty}");
    }
    assert!(parse_perp_open_algo_orders(r#"[{}, null, 3, "x"]"#, "binance", None).is_ok());
}

#[test]
fn the_algo_endpoints_say_unknown_in_the_order_endpoints_words() {
    let e = |code, msg: &str| VenueApiError { code, msg: msg.to_string() };
    assert!(is_unknown_order(&e(-2011, "Unknown order sent.")));
    assert!(is_unknown_order(&e(-2013, "Order does not exist.")));
    assert!(!is_unknown_order(&e(-2011, "This account may not place or cancel orders.")));
    for code in [0, -1003, -1021, -2010, -2015, -2021, -4117, -4120, 400, 404, 503] {
        assert!(!is_unknown_order(&e(code, "Order does not exist.")), "{code}");
    }
}

#[test]
fn the_order_client_id_set_remembers_and_forgets() {
    let set = OrderVenueIdRegistryAlgo::default();
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
        assert!(map_algo_update(&frame, None).is_empty(), "{frame}");
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
        match &map_algo_update(&frame, Some("LINK"))[..] {
            [Event::OrderAccepted(a)] => {
                assert_eq!(a.order_client_id, "sl");
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
    match &map_algo_history(&rows, Some("LINK"))[..] {
        [Event::OrderCanceled(a), Event::OrderExpired(b), Event::OrderCanceled(c)] => {
            assert_eq!((a.order_client_id.as_str(), a.timestamp), ("a", 5));
            assert!(a.reason.contains("closed during reconnect gap"), "{}", a.reason);
            assert_eq!((b.order_client_id.as_str(), b.timestamp), ("b", 6));
            assert_eq!((c.order_client_id.as_str(), c.timestamp), ("c", 7));
            assert!(c.reason.contains("rejected during reconnect gap"), "{}", c.reason);
            assert_eq!(
                (a.refusal_id.as_deref(), c.refusal_id.as_deref()),
                (None, Some("order.venue_refused")),
                "only the venue's refusal carries the id"
            );
        }
        other => panic!("{other:?}"),
    }
    for body in [json!(null), json!({}), json!("x"), json!([])] {
        assert!(map_algo_history(&body, None).is_empty(), "{body}");
    }
}

/// The fill-while-down name map: only a stop that TRIGGERED has an `actualOrderId`, whatever its
/// JSON type, the broker prefix is stripped, and a stop that never triggered (an absent, empty or `0`
/// id), an id-less row and an order-client-id-less row map to nothing.
#[test]
fn the_algo_history_maps_a_triggered_stops_child_order_to_its_order_client_id() {
    let body = json!([
        {"clientAlgoId": "x-LINK-stop1", "algoStatus": "FINISHED", "actualOrderId": "8001"},
        {"clientAlgoId": "stop2", "algoStatus": "FINISHED", "actualOrderId": 8002},
        {"clientAlgoId": "stop3", "algoStatus": "CANCELED", "actualOrderId": ""},
        {"clientAlgoId": "stop4", "algoStatus": "NEW", "actualOrderId": "0"},
        {"clientAlgoId": "stop5", "algoStatus": "NEW"},
        {"clientAlgoId": "", "algoStatus": "FINISHED", "actualOrderId": "8006"},
        {"algoStatus": "FINISHED", "actualOrderId": "8007"},
    ])
    .to_string();
    assert_eq!(
        parse_algo_children(&body, Some("LINK")).unwrap(),
        (
            vec![
                ("8001".to_string(), "stop1".to_string()),
                ("8002".to_string(), "stop2".to_string())
            ],
            7, // every row counts toward the page, named or not
        )
    );
    assert!(parse_algo_children("not json", None).is_err());
    assert!(parse_algo_children("{}", None).is_err(), "a non-array is no list");
}

/// The join names only an order-client-id-less fill of the SAME symbol whose order is a stop's child.
#[test]
fn only_an_order_client_id_less_fill_of_a_stops_child_order_is_named() {
    use vike_model::events::LiquiditySide;
    let fill = |symbol: &str, order: &str, order_client_id: Option<&str>| FillReport {
        venue: "binance".into(),
        symbol: symbol.into(),
        trade_id: "t1".into(),
        venue_order_id: order.into(),
        order_client_id: order_client_id.map(str::to_string),
        side: -1,
        last_qty: 1.0,
        last_price: 90.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 1,
    };
    let children = vec![("8001".to_string(), "stop-1".to_string())];
    let mut fills = [
        fill("BTCUSDT.P", "8001", None), // the child: named
        fill("BTCUSDT.P", "8002", None), // no stop's child: untouched
        fill("ETHUSDT.P", "8001", None), // another symbol's order 8001: untouched
        fill("BTCUSDT.P", "8001", Some("own-order-client-id")), // already names its order: untouched
    ];
    join_algo_children(&mut fills, "BTCUSDT.P", &children);
    let order_client_ids: Vec<_> = fills.iter().map(|f| f.order_client_id.as_deref()).collect();
    assert_eq!(order_client_ids, [Some("stop-1"), None, None, Some("own-order-client-id")]);
}

fn fill_on(symbol: &str, order_client_id: Option<&str>) -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: symbol.into(),
        trade_id: "t1".into(),
        venue_order_id: "8001".into(),
        order_client_id: order_client_id.map(str::to_string),
        side: -1,
        last_qty: 1.0,
        last_price: 90.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: vike_model::events::LiquiditySide::Taker,
        timestamp: 1,
    }
}

/// The history is read for each symbol that has an order-client-id-less fill, once, and for no more symbols than
/// the engine can serve (the cost bound): the rest stay unnamed and the caller says so.
#[test]
fn the_history_is_read_per_symbol_with_an_order_client_id_less_fill_and_capped_at_the_engines_ceiling()
 {
    let fills = [
        fill_on("ETHUSDT.P", None),
        fill_on("BTCUSDT.P", None),
        fill_on("ETHUSDT.P", None),
        fill_on("SOLUSDT.P", Some("named")),
    ];
    assert_eq!(
        history_labels(&fills),
        (vec!["BTCUSDT.P".to_string(), "ETHUSDT.P".to_string()], false)
    );

    let many: Vec<FillReport> =
        (0..ALGO_HISTORY_MAX_READS + 3).map(|n| fill_on(&format!("S{n:02}USDT.P"), None)).collect();
    let (labels, capped) = history_labels(&many);
    assert_eq!(labels.len(), ALGO_HISTORY_MAX_READS);
    assert!(capped, "symbols were left out");
    assert_eq!(
        ALGO_HISTORY_MAX_READS,
        vike_execution::MAX_EXTRA_SYMBOLS + 1,
        "the engine's own ceiling"
    );
}

/// An unreadable history is strict for the first few passes (the fill fetch fails, like the open-list
/// read beside it), then the pass goes ahead with the fills unnamed and the notice repeats once an
/// hour; one good pass restores strictness.
#[test]
fn an_unreadable_history_is_strict_for_a_few_passes_then_goes_ahead_with_one_notice_an_hour() {
    let naming = AlgoNaming::default();
    let t0 = Instant::now();
    for n in 1..=ALGO_NAMING_STRICT_FAILURES {
        assert_eq!(naming.failed(t0, format!("down {n}")), Err(format!("down {n}")), "pass {n}");
    }
    assert_eq!(naming.failed(t0, "down".into()), Ok(true), "the first degraded pass says so");
    let minute = Duration::from_secs(60);
    assert_eq!(naming.failed(t0 + minute, "down".into()), Ok(false), "inside the hour: silent");
    let almost = ALGO_NAMING_NOTICE_EVERY - Duration::from_secs(1);
    assert_eq!(naming.failed(t0 + almost, "down".into()), Ok(false));
    assert_eq!(naming.failed(t0 + ALGO_NAMING_NOTICE_EVERY, "down".into()), Ok(true), "an hour on");

    naming.succeeded();
    assert!(naming.failed(t0, "again".into()).is_err(), "strict again after a good pass");
}

/// The by-id memory keeps "no" as an answer (a plain order's fill is asked about once), tells "never
/// asked" apart from it, and drops HALF of its answers at its bound (never all of them, and never
/// the one just learned) instead of growing for the life of the client.
#[test]
fn the_by_id_memory_remembers_no_and_drops_half_at_its_bound() {
    let naming = AlgoNaming::default();
    assert_eq!(naming.child("BTCUSDT.P", "9"), None, "never asked");
    naming.remember_child("BTCUSDT.P", "9", None);
    assert_eq!(naming.child("BTCUSDT.P", "9"), Some(None), "asked, and it is no stop's child");
    naming.remember_child("BTCUSDT.P", "10", Some("stop1".to_string()));
    assert_eq!(naming.child("BTCUSDT.P", "10"), Some(Some("stop1".to_string())));
    assert_eq!(naming.child("ETHUSDT.P", "10"), None, "the order id is per symbol");

    for n in 0..ALGO_CHILD_MEMORY {
        naming.remember_child("ETHUSDT.P", &n.to_string(), None);
    }
    let kept = naming.state().children.len();
    assert!(kept < ALGO_CHILD_MEMORY, "bounded: half were dropped when it filled ({kept})");
    assert!(kept >= ALGO_CHILD_MEMORY / 2, "and only half ({kept})");
    let last = (ALGO_CHILD_MEMORY - 1).to_string();
    assert_eq!(naming.child("ETHUSDT.P", &last), Some(None), "the newest answer is still there");
}

/// The two hourly notices run on their own clocks: taking one does not hide the other, and each is
/// due again an hour later.
#[test]
fn the_unnamed_and_the_forgiven_notices_have_their_own_hourly_clocks() {
    let naming = AlgoNaming::default();
    let t0 = Instant::now();
    assert!(naming.notice_due(NOTICE_FORGIVEN, t0), "the first is due");
    assert!(!naming.notice_due(NOTICE_FORGIVEN, t0 + Duration::from_secs(60)), "then silent");
    assert!(
        naming.notice_due(NOTICE_UNNAMED, t0 + Duration::from_secs(60)),
        "the other is not hidden"
    );
    assert!(naming.notice_due(NOTICE_FORGIVEN, t0 + ALGO_NAMING_NOTICE_EVERY), "due an hour on");
}

/// Only an id of OURS names a fill: the bare order client id, or `x-<our link id>-<order client id>`; never a hand-placed
/// order's (`web_…`, another broker's `x-…`), and never an `x-` id when no link id is configured.
#[test]
fn only_our_own_client_ids_name_a_fill() {
    assert_eq!(
        own_order_client_id("abcd1234stop1", None).as_deref(),
        Some("abcd1234stop1"),
        "a bare order client id"
    );
    assert_eq!(
        own_order_client_id("abcd1234stop1", Some("LINK")).as_deref(),
        Some("abcd1234stop1")
    );
    assert_eq!(
        own_order_client_id("x-LINK-stop1", Some("LINK")).as_deref(),
        Some("stop1"),
        "ours, prefixed"
    );
    assert_eq!(
        own_order_client_id("x-LINK-stop1", None),
        None,
        "a prefixed id with no link id is not ours"
    );
    assert_eq!(own_order_client_id("x-OTHER-stop1", Some("LINK")), None, "another broker's");
    assert_eq!(
        own_order_client_id("x-LINKS-stop1", Some("LINK")),
        None,
        "a link id that merely starts like ours"
    );
    assert_eq!(
        own_order_client_id("web_ab12cd34", Some("LINK")),
        None,
        "a web order's id (not alphanumeric)"
    );
    assert_eq!(own_order_client_id("android_9Zx", None), None, "a mobile order's id");
    assert_eq!(own_order_client_id("", Some("LINK")), None, "no id");
    assert_eq!(
        own_order_client_id("x-LINK-", Some("LINK")),
        None,
        "a prefix with no order client id"
    );
    assert_eq!(
        own_order_client_id(&"a".repeat(33), None),
        None,
        "longer than an order client id of ours can be"
    );
}
