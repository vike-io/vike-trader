//! Bybit `execution.fast` early fast-fill hint (steal/bybit-execution-fast).
//!
//! Two levels of proof:
//!   * MAPPER — an `execution.fast` frame maps to ONE early bare `Event::Fill` (no dual-publish
//!     wrap), with the `execId` as the `trade_id` dedup key and fee UNKNOWN (0.0); and a normal
//!     `execution` Trade frame is UNCHANGED by the additive fast arm (still dual-publishes) — the
//!     OFF-path-at-the-mapper proof (the subscribe-list OFF proof lives in `execution/user_data.rs`'s units).
//!   * ENGINE — the no-double-count guard end-to-end: an early `execution.fast` fill plus its later
//!     full `execution` twin on the SAME execId net to ONE position booking and ONE `on_fill`
//!     delivery (the fill-rate breaker), while the RETAINED `execution` twin's terminal wrap
//!     terminalizes the FSM (bare-fast-only left `seen_fsm_trade_ids` free, so the slow wrap is not
//!     dedup-dropped). This is the same execId-keyed dedup the binance/reconnect lane relies on.
//!
//! Author-blind note: these tests are written but NOT run here (no cargo in this worktree).

use serde_json::json;
use std::assert_matches;
use vike_bybit::execution::event_mapper::{map_bybit_perp, map_execution_fast};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, EventHandler, RiskGate};
use vike_model::OrderRequest;
use vike_model::OrderStatus;
use vike_model::RiskLimits;
use vike_model::events::{Event, LiquiditySide, OrderSubmitted, PositionSide};

/// A slim Bybit `execution.fast` frame: category/symbol/orderId/isMaker/orderLinkId/side/execId/
/// execPrice/execQty/execTime — deliberately OMITS execType, execFee/feeCurrency, cum/order/leaves
/// qty, markPrice and positionIdx (the real fast-stream shape).
fn fast_frame(
    order_client_id: &str,
    exec_id: &str,
    side: &str,
    qty: f64,
    px: f64,
    is_maker: bool,
) -> serde_json::Value {
    json!({
        "topic": "execution.fast",
        "creationTime": 1,
        "data": [{
            "category": "linear",
            "symbol": "BTCUSDT",
            "orderId": "v-oid",
            "orderLinkId": order_client_id,
            "execId": exec_id,
            "side": side,
            "execPrice": px.to_string(),
            "execQty": qty.to_string(),
            "isMaker": is_maker,
            "execTime": "2"
        }]
    })
}

/// The slow full `execution` Trade twin — carries execFee/feeCurrency + the cum/order/leaves qty that
/// make it terminal, and the SAME execId as its fast twin.
fn slow_full_fill_frame(order_client_id: &str, exec_id: &str) -> serde_json::Value {
    json!({
        "topic": "execution",
        "data": [{
            "execType": "Trade",
            "category": "linear",
            "symbol": "BTCUSDT",
            "orderLinkId": order_client_id,
            "execId": exec_id,
            "side": "Buy",
            "execPrice": "100.0",
            "execQty": "1.0",
            "execFee": "0.06",
            "feeCurrency": "USDT",
            "isMaker": false,
            "cumExecQty": "1.0",
            "orderQty": "1.0",
            "leavesQty": "0"
        }]
    })
}

// ---- MAPPER --------------------------------------------------------------------------------------

#[test]
fn map_execution_fast_builds_one_bare_fill_with_execid_as_trade_id() {
    vike_bybit::history::kept_list_for_tests();
    let frame = fast_frame("c1", "E1", "Buy", 1.5, 100.0, true);
    let event = map_execution_fast(&frame["data"][0], "bybit", "BTCUSDT.P");
    assert_eq!(event.len(), 1, "fast row → exactly ONE bare Fill (no wrap): {event:?}");
    let Event::Fill(f) = &event[0] else { panic!("expected a bare Fill, got {:?}", event[0]) };
    assert_eq!(f.trade_id.as_str(), "E1", "trade_id = execId (the dedup key across both topics)");
    assert_eq!(f.order_client_id, "c1");
    assert_eq!(f.symbol.as_str(), "BTCUSDT.P", "the linear row's user name (decision 0290)");
    assert_eq!(f.side, 1, "Buy → +1");
    assert_eq!(f.last_qty, 1.5);
    assert_eq!(f.last_price, 100.0);
    assert_eq!(f.commission, 0.0, "fee UNKNOWN on the fast wire → 0.0");
    assert!(f.commission_asset.as_str().is_empty(), "no feeCurrency on the fast wire");
    assert_eq!(f.liquidity_side, LiquiditySide::Maker, "isMaker=true → Maker");
    assert_eq!(f.position_side, PositionSide::Both, "no positionIdx on the fast wire → BOTH");
    assert!(f.mark_price.is_none(), "no markPrice on the fast wire → None");
}

#[test]
fn perp_dispatch_routes_execution_fast_to_a_single_bare_fill_no_wrap() {
    let event =
        map_bybit_perp(&fast_frame("c1", "E1", "Sell", 2.0, 50.0, false), "bybit", "BTCUSDT.P");
    assert_eq!(
        event.len(),
        1,
        "fast frame emits ONLY the bare Fill (no terminal/partial wrap): {event:?}"
    );
    let Event::Fill(f) = &event[0] else { panic!("expected a bare Fill, got {:?}", event[0]) };
    assert_eq!(f.trade_id.as_str(), "E1");
    assert_eq!(f.side, -1, "Sell → -1");
    assert_eq!(f.liquidity_side, LiquiditySide::Taker, "isMaker=false → Taker");
}

#[test]
fn categorised_execution_fast_topic_is_also_routed() {
    // A caller that subscribed the categorised `execution.fast.linear` gets that topic on the wire.
    let mut frame = fast_frame("c1", "E9", "Buy", 1.0, 100.0, false);
    frame["topic"] = json!("execution.fast.linear");
    let event = map_bybit_perp(&frame, "bybit", "BTCUSDT.P");
    assert_eq!(event.len(), 1, "categorised fast topic still routes to the bare fill: {event:?}");
    assert_matches!(event[0], Event::Fill(_));
}

#[test]
fn normal_execution_frame_is_unchanged_by_the_additive_fast_arm() {
    // OFF-path at the mapper: a real `execution` Trade frame still DUAL-publishes [Fill, OrderFilled]
    // — the fast arm (exact-string-distinct) never intercepts it, so today's behavior is intact.
    let event = map_bybit_perp(&slow_full_fill_frame("c1", "E1"), "bybit", "BTCUSDT.P");
    assert_eq!(event.len(), 2, "unchanged dual-publish: {event:?}");
    assert_matches!(event[0], Event::Fill(_), "even slot is the bare Fill");
    assert_matches!(event[1], Event::OrderFilled(_), "odd slot is the terminal wrap");
}

// ---- ENGINE (end-to-end no-double-count) ---------------------------------------------------------

/// The engine of the mount `BTCUSDT.P` (decision 0290's user name, whose fills the mapper names off the held kept
/// list).
fn engine() -> Engine<RecordingClient> {
    vike_bybit::history::kept_list_for_tests();
    let mut engine_instance = Engine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT.P",
    );
    // Arm the strategy-delivery buffer so we can prove the fill-rate breaker (on_fill) fires ONCE.
    engine_instance.collect_applied_fills = true;
    engine_instance
}

/// Drive a fresh order to ACCEPTED (submit → OrderSubmitted → OrderAccepted) so the terminal wrap
/// later has a legal source state. `submit_order` registers at INITIALIZED; OrderSubmitted is the
/// Rust-emitted edge, OrderAccepted comes from the venue `order` topic (`orderStatus=New`).
fn accepted_order(engine: &mut Engine<RecordingClient>, outbox: &mut EventBusOutbox) {
    let req = OrderRequest {
        order_client_id: "c1".into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        timestamp: 1,
        ..Default::default()
    };
    engine.submit_order(&req, 1, outbox);
    assert!(engine.registry.contains_key("c1"), "submit registers the order client id");
    engine.on_event(
        &Event::OrderSubmitted(OrderSubmitted { order_client_id: "c1".to_string(), timestamp: 1 }),
        outbox,
    );
    let accept = json!({"topic": "order", "data": [
        {"orderLinkId": "c1", "orderStatus": "New", "orderId": "v-oid", "updatedTime": 1}]});
    for event in &map_bybit_perp(&accept, "bybit", "BTCUSDT.P") {
        engine.on_event(event, outbox);
    }
    assert_eq!(
        engine.registry.get("c1").unwrap().status,
        OrderStatus::Accepted,
        "order is ACCEPTED"
    );
}

#[test]
fn fast_then_slow_same_execid_nets_to_one_booking() {
    let mut engine_under_test = engine();
    let mut outbox = EventBusOutbox::default();
    accepted_order(&mut engine_under_test, &mut outbox);

    // 1) The EARLY execution.fast fill (fee unknown) — books position + fires on_fill immediately.
    let fast =
        map_bybit_perp(&fast_frame("c1", "E1", "Buy", 1.0, 100.0, false), "bybit", "BTCUSDT.P");
    assert_eq!(fast.len(), 1, "fast emits ONLY the bare Fill");
    for event in &fast {
        engine_under_test.on_event(event, &mut outbox);
    }
    assert_eq!(
        engine_under_test.position_size(vike_model::events::PositionSide::Both),
        1.0,
        "fast fill books position EARLY"
    );
    assert_eq!(
        engine_under_test.applied_fills.len(),
        1,
        "fill-rate breaker sees the fill ONCE, off the fast row"
    );
    assert_eq!(
        engine_under_test.registry.get("c1").unwrap().status,
        OrderStatus::Accepted,
        "the bare fast fill does NOT advance the FSM (no wrap emitted)"
    );

    // 2) The later FULL execution twin on the SAME execId — its bare Fill is deduped, its terminal
    //    wrap terminalizes the FSM (bare-fast left seen_fsm_trade_ids free).
    let slow = map_bybit_perp(&slow_full_fill_frame("c1", "E1"), "bybit", "BTCUSDT.P");
    assert_eq!(slow.len(), 2, "slow twin dual-publishes [Fill, OrderFilled]");
    for event in &slow {
        engine_under_test.on_event(event, &mut outbox);
    }
    assert_eq!(
        engine_under_test.position_size(vike_model::events::PositionSide::Both),
        1.0,
        "slow twin deduped by execId — position booked ONCE"
    );
    assert_eq!(
        engine_under_test.applied_fills.len(),
        1,
        "no second on_fill delivery for the deduped twin"
    );
    assert_eq!(
        engine_under_test.registry.get("c1").unwrap().status,
        OrderStatus::Filled,
        "the slow twin's terminal wrap terminalizes the FSM (not dedup-dropped)"
    );
}

#[test]
fn slow_first_then_fast_also_nets_to_one_booking() {
    // Ordering independence: if the FULL execution twin arrives first (fast lagged), the fast twin's
    // bare Fill is the one deduped — still ONE booking, still one on_fill, FSM already terminal.
    let mut engine_under_test = engine();
    let mut outbox = EventBusOutbox::default();
    accepted_order(&mut engine_under_test, &mut outbox);

    for event in &map_bybit_perp(&slow_full_fill_frame("c1", "E1"), "bybit", "BTCUSDT.P") {
        engine_under_test.on_event(event, &mut outbox);
    }
    assert_eq!(engine_under_test.position_size(vike_model::events::PositionSide::Both), 1.0);
    assert_eq!(engine_under_test.applied_fills.len(), 1);
    assert_eq!(engine_under_test.registry.get("c1").unwrap().status, OrderStatus::Filled);

    // The lagging fast twin (same execId) must be a pure no-op — no double count, no extra on_fill.
    for event in
        &map_bybit_perp(&fast_frame("c1", "E1", "Buy", 1.0, 100.0, false), "bybit", "BTCUSDT.P")
    {
        engine_under_test.on_event(event, &mut outbox);
    }
    assert_eq!(
        engine_under_test.position_size(vike_model::events::PositionSide::Both),
        1.0,
        "lagging fast twin deduped — position still booked ONCE"
    );
    assert_eq!(
        engine_under_test.applied_fills.len(),
        1,
        "no second on_fill delivery for the lagging fast twin"
    );
}
