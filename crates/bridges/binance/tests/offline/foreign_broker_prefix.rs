//! **V8 (owner ruling 2026-11-12): an order id is OURS only when its prefix carries OUR broker link id.**
//!
//! Every reader of an id the venue echoes takes the account's link id (`LINK` here) and goes through
//! `crates/bridges/binance/src/family/execution/order_map.rs`'s `strip_broker_order_client_id_prefix`: the spot and perp
//! streams (`ALGO_UPDATE` too), the history replay, the reconcile open orders (regular and Algo
//! Service) and the stop-to-child map the fill naming joins on. Each one reads another broker's
//! `x-OTHER-abc123` as exactly that, a foreign id, and `x-LINK-abc123` as our `abc123`; with no link
//! id configured, `x-LINK-abc123` is foreign too. The last test is what a foreign order is to the
//! core: it matches no order, so its events are dropped as an unknown order client id and OUR order of the same
//! bare id is never touched.

use serde_json::{Value, json};
use vike_binance::execution::event_mapper::perp::map_binance_perp;
use vike_binance::execution::event_mapper::spot::map_binance_private;
use vike_binance::execution::rest::algo::{
    map_algo_history, parse_algo_children, parse_perp_open_algo_orders,
};
use vike_binance::execution::resync::map_binance_history;
use vike_binance::reconcile::parse_spot_open_orders;
use vike_bridge_core::execution_drive::order_client_id_of;
use vike_model::OrderStatus;
use vike_model::events::Event;

const LINK: Option<&str> = Some("LINK");

fn spot_new(c: &str) -> Value {
    json!({"e": "executionReport", "s": "BTCUSDT", "x": "NEW", "X": "NEW", "c": c, "T": 1, "i": 9})
}

fn spot_canceled(resting: &str) -> Value {
    json!({"e": "executionReport", "s": "BTCUSDT", "x": "CANCELED", "X": "CANCELED", "C": resting,
           "c": "web_cancel1", "r": "NONE", "T": 7, "i": 9})
}

fn perp_new(c: &str) -> Value {
    json!({"e": "ORDER_TRADE_UPDATE", "T": 1,
           "o": {"s": "BTCUSDT", "c": c, "x": "NEW", "X": "NEW", "i": 9}})
}

fn algo_new(caid: &str) -> Value {
    json!({"e": "ALGO_UPDATE", "T": 1, "o": {"caid": caid, "X": "NEW", "aid": 7}})
}

/// The order client id of the one event `events` holds.
fn one(events: &[Event]) -> &str {
    let [event] = events else { panic!("one event: {events:?}") };
    order_client_id_of(event)
}

#[test]
fn the_streams_read_only_our_own_prefix_as_ours() {
    for (link, wire, want) in [
        (LINK, "x-LINK-abc123", "abc123"),
        (LINK, "x-OTHER-abc123", "x-OTHER-abc123"),
        (None, "x-LINK-abc123", "x-LINK-abc123"),
        (LINK, "abc123", "abc123"),
        (None, "abc123", "abc123"),
    ] {
        let spot = map_binance_private(&spot_new(wire), "binance", "BTCUSDT", link);
        assert_eq!(one(&spot), want, "spot NEW `{wire}` under {link:?}");
        let spot = map_binance_private(&spot_canceled(wire), "binance", "BTCUSDT", link);
        assert_eq!(one(&spot), want, "spot CANCELED `{wire}` under {link:?}");
        let perp = map_binance_perp(&perp_new(wire), "binance", "BTCUSDT.P", link);
        assert_eq!(one(&perp), want, "perp NEW `{wire}` under {link:?}");
        let algo = map_binance_perp(&algo_new(wire), "binance", "BTCUSDT.P", link);
        assert_eq!(one(&algo), want, "ALGO_UPDATE `{wire}` under {link:?}");
    }
}

#[test]
fn the_history_replay_reads_only_our_own_prefix_as_ours() {
    let row = |order_client_id: &str| {
        json!([{"symbol": "BTCUSDT", "orderId": 9, "clientOrderId": order_client_id, "status": "CANCELED",
                "updateTime": 5, "origQty": "1", "executedQty": "0"}])
    };
    for (link, wire, want) in [
        (LINK, "x-LINK-abc123", "abc123"),
        (LINK, "x-OTHER-abc123", "x-OTHER-abc123"),
        (None, "x-LINK-abc123", "x-LINK-abc123"),
    ] {
        let events = map_binance_history(&row(wire), &json!([]), "binance", "BTCUSDT", link);
        assert_eq!(one(&events), want, "allOrders `{wire}` under {link:?}");
        let algo = json!([{"clientAlgoId": wire, "algoStatus": "CANCELED", "updateTime": 5}]);
        assert_eq!(
            one(&map_algo_history(&algo, link)),
            want,
            "allAlgoOrders `{wire}` under {link:?}"
        );
    }
}

#[test]
fn reconcile_reads_only_our_own_prefix_as_ours() {
    let open = |order_client_id: &str| {
        json!([{"symbol": "BTCUSDT", "orderId": 9, "clientOrderId": order_client_id, "price": "1",
                "origQty": "1", "executedQty": "0", "cummulativeQuoteQty": "0", "status": "NEW",
                "timeInForce": "GTC", "type": "LIMIT", "side": "BUY", "time": 1, "updateTime": 1}])
        .to_string()
    };
    let algo_open = |caid: &str| {
        json!([{"algoId": 7, "clientAlgoId": caid, "orderType": "STOP_MARKET", "symbol": "BTCUSDT",
                "side": "SELL", "quantity": "0.005", "triggerPrice": "60000", "algoStatus": "NEW",
                "updateTime": 1}])
        .to_string()
    };
    let children = |caid: &str| {
        json!([{"clientAlgoId": caid, "algoStatus": "FINISHED", "actualOrderId": "8001"}])
            .to_string()
    };
    for (link, wire, want) in [
        (LINK, "x-LINK-abc123", "abc123"),
        (LINK, "x-OTHER-abc123", "x-OTHER-abc123"),
        (None, "x-LINK-abc123", "x-LINK-abc123"),
    ] {
        let spot = parse_spot_open_orders(&open(wire), link).unwrap();
        assert_eq!(spot[0].order_client_id.as_deref(), Some(want), "openOrders `{wire}` {link:?}");
        let stops = parse_perp_open_algo_orders(&algo_open(wire), "binance", link).unwrap();
        assert_eq!(stops[0].order_client_id.as_deref(), Some(want), "openAlgoOrders `{wire}`");
        let (named, _) = parse_algo_children(&children(wire), link).unwrap();
        assert_eq!(
            named,
            [("8001".to_string(), want.to_string())],
            "allAlgoOrders children `{wire}`"
        );
    }
}

/// What a foreign order is to the core: our order `abc123` rests; another broker's order of the SAME
/// bare id is cancelled at the venue. Its report names `x-OTHER-abc123`, which matches no order: the
/// engine drops it as an unknown order client id (`dropped_unknown_order_client_id`, shown) and OUR order is never touched.
/// Read as `abc123` (the rule before V8), it would have closed our live order.
#[test]
fn a_foreign_report_never_touches_our_order_of_the_same_bare_id() {
    use vike_execution::testing::RecordingClient;
    use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, EventHandler, RiskGate};
    use vike_model::{OrderRecord, OrderRequest, OrderType, RiskLimits};

    for (link, foreign) in [(LINK, "x-OTHER-abc123"), (None, "x-LINK-abc123")] {
        let mut engine = Engine::new(
            Account::new(1.0, "binance", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "binance",
            "BTCUSDT",
        );
        let mut order = OrderRecord::new(OrderRequest {
            order_client_id: "abc123".to_string(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: 1,
            qty: 0.001,
            order_type: OrderType::Limit,
            price: Some(50_000.0),
            ..Default::default()
        });
        order.status = OrderStatus::Accepted;
        engine.registry.insert("abc123".to_string(), order);

        let report = map_binance_private(&spot_canceled(foreign), "binance", "BTCUSDT", link);
        let mut outbox = EventBusOutbox::default();
        for event in &report {
            engine.on_event(event, &mut outbox);
        }
        assert_eq!(engine.registry["abc123"].status, OrderStatus::Accepted, "{link:?}: {report:?}");
        assert_eq!(
            engine.dropped_unknown_order_client_id, 1,
            "{link:?}: the foreign report is dropped, counted"
        );
    }
}
