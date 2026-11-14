use super::*;
use crate::MarkSource;
use crate::bus::EventHandler;
use crate::testing::{EngineBuilder, RecordingClient, denied_reason};
use crate::{Account, BalanceMode};
use vike_model::OrderRequest;
use vike_model::OrderStatus;
use vike_model::RiskLimits;
use vike_model::events::{FillEvent, OrderFilled};

fn engine() -> Engine<RecordingClient> {
    EngineBuilder::default().build()
}

/// `local_view()` must report the registered order client id, the folded trade_id, and the net position
/// after one submit+fill — the owned read `recon::diff` consumes across the reconcile driver's
/// thread boundary.
#[test]
fn local_view_reports_order_fill_and_position() {
    let mut engine_under_test = engine();
    let mut outbox = EventBusOutbox::default();

    let req = OrderRequest {
        order_client_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        timestamp: 1,
        ..Default::default()
    };
    engine_under_test.submit_order(&req, 1, &mut outbox);
    assert!(engine_under_test.registry.contains_key("c1"), "submit registers the order client id");

    let fill = FillEvent {
        trade_id: "t1".into(),
        order_client_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: 1,
        mark_price: Some(100.0),
        position_side: vike_model::events::PositionSide::Both,
    };
    // Real venue adapters emit both the bare fold lane and the FSM wrap for one fill (see
    // price_board_wiring.rs's comment on the same idiom).
    engine_under_test.on_event(&Event::Fill(fill.clone()), &mut outbox);
    engine_under_test.on_event(
        &Event::OrderFilled(OrderFilled { order_client_id: "c1".to_string(), fill, timestamp: 1 }),
        &mut outbox,
    );

    let owned = engine_under_test.local_view();
    assert_eq!(owned.venue, "sim");
    assert!(owned.orders.contains_key("c1"), "order client id present in orders");
    assert!(owned.seen_trade_ids.contains("t1"), "trade_id present in seen set");
    assert_eq!(
        owned.positions.get(&("BTCUSDT".to_string(), vike_model::events::PositionSide::Both)),
        Some(&1.0),
        "net position folded"
    );

    // as_view borrows the same content back out as a ReconcileLocalView.
    let view = owned.as_view();
    assert_eq!(view.venue, "sim");
    assert!(view.orders.contains_key("c1"));
    assert!(view.seen_trade_ids.contains("t1"));
    assert_eq!(
        view.positions.get(&("BTCUSDT".to_string(), vike_model::events::PositionSide::Both)),
        Some(&1.0)
    );
}

/// The liquidation law's partition at the ADMITTING gate: an Isolated position is backed by
/// its own walled-off wallet, not the shared equity the gate admits against, so it must not
/// inflate the gate's `margin_used` (double-charging a mixed account). The same book with the
/// position CROSS must still be charged: the filter keys on the MODE.
#[test]
fn gate_margin_used_excludes_isolated_positions() {
    use vike_model::MarginMode;
    let mk = |mode: MarginMode| {
        let mut engine = Engine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits {
                initial_margin_requirement: Some(0.1),
                ..RiskLimits::new()
            }),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        engine.equity_seed = 50.0; // equity 50 (no balance/realized/unreal: avg == mark)
        // an open ETH position that prices 10·100·1·0.1 = 100 margin IF counted
        engine.account.positions.insert(
            ("sim".into(), "ETHUSDT".into(), vike_model::events::PositionSide::Both),
            crate::account::PositionEntry {
                size: 10.0,
                average_price: 100.0,
                margin_mode: mode,
                isolated_margin: mode.is_isolated().then_some(100.0),
            },
        );
        engine.account.set_mark_from("sim", "ETHUSDT", 100.0, MarkSource::VenueMark, 0);
        // the gate's margin fold is resolver-priced: feed the board's mark slot the same price
        // the write-sites would
        engine.price_board.set_mark("sim", "ETHUSDT", 100.0, 0);
        engine
    };
    let order = OrderRequest {
        order_client_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        timestamp: 1,
        ..Default::default()
    };
    // order margin = 100·1·1·0.1 = 10 vs equity 50.
    // Isolated ETH excluded → margin_used 0 → free 50 ≥ 10 → ADMITTED.
    let mut iso = mk(MarginMode::Isolated);
    let mut outbox = EventBusOutbox::default();
    iso.submit_order(&order, 1, &mut outbox);
    assert_eq!(
        iso.client.submissions.len(),
        1,
        "an isolated position must not consume the shared gate margin"
    );
    // Cross ETH counted → margin_used 100 > equity 50 → free 0 → DENIED.
    let mut cross = mk(MarginMode::Cross);
    let mut outbox = EventBusOutbox::default();
    cross.submit_order(&order, 1, &mut outbox);
    assert!(
        cross.client.submissions.is_empty(),
        "the same book cross-margined must still be charged (filter keys on mode)"
    );
    assert!(
        outbox.0.iter().any(|e| matches!(e, Event::OrderDenied(_))),
        "cross case denies through the normal veto path"
    );
}

fn order_report(
    order_client_id: &str,
    status: OrderStatus,
    filled: f64,
) -> vike_model::OrderStatusReport {
    vike_model::OrderStatusReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v7".into(),
        order_client_id: Some(order_client_id.into()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 2.0,
        filled_qty: filled,
        average_price: if filled > 0.0 { 100.0 } else { 0.0 },
        status,
        timestamp: 5,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    }
}

/// Whether `outbox` holds an `OrderModifyRejected` that says the order's lifetime is the reason.
fn refused_over_its_lifetime(outbox: &EventBusOutbox) -> bool {
    outbox.0.iter().any(
        |event| matches!(event, Event::OrderModifyRejected(r) if r.reason.contains("lifetime")),
    )
}

/// **An order rebuilt from a venue report has no lifetime anybody knows** (a report carries none), and the
/// registry's default `gtc` is no knowledge. A venue whose amend sends the lifetime again would change it in silence,
/// so that modify is REFUSED by name with nothing sent; a venue whose amend leaves the lifetime alone takes it as
/// it always did. Unknown venue strings are refused: nobody has read their amend.
#[test]
fn a_modify_of_an_order_rebuilt_from_a_report_is_refused_where_the_amend_sends_its_lifetime() {
    for (venue, sent) in [
        ("oanda", false),
        ("ibkr", false),
        ("hyperliquid", false),
        ("binance", false),
        ("deribit", false),
        ("bybit", true),
        ("okx", true),
        ("sim", false),
    ] {
        let mut engine = crate::testing::engine_on(venue, "BTCUSDT", RecordingClient::default());
        assert_eq!(
            engine.reregister_orders(&[order_report("lost", OrderStatus::Accepted, 0.0)]),
            1
        );
        let mut outbox = EventBusOutbox::default();
        engine.modify_order("lost", None, Some(99.0), 5, &mut outbox);
        assert_eq!(!engine.client.modifies.is_empty(), sent, "{venue}: {:?}", outbox.0);
        assert_eq!(refused_over_its_lifetime(&outbox), !sent, "{venue}: {:?}", outbox.0);
    }
}

/// A cancel-then-new amend PLACES the order again on every venue, so the unknown lifetime is refused on all of them.
#[test]
fn a_replacement_of_an_order_rebuilt_from_a_report_is_refused_on_every_venue() {
    for venue in ["oanda", "bybit"] {
        let mut engine = crate::testing::engine_on(venue, "BTCUSDT", RecordingClient::default());
        engine.reregister_orders(&[order_report("lost", OrderStatus::Accepted, 0.0)]);
        let mut outbox = EventBusOutbox::default();
        let plan = engine.plan_replacement("lost", None, Some(99.0), None, 5, &mut outbox);
        assert!(plan.is_none(), "{venue}: {plan:?}");
        assert!(refused_over_its_lifetime(&outbox), "{venue}: {:?}", outbox.0);
    }
}

/// PRESERVE: once the lifetime is known (`vike-core`'s restore clears the flag), and for an order placed through the
/// engine, the amend goes out on the same venue.
#[test]
fn a_modify_of_an_order_whose_lifetime_is_known_is_sent() {
    let mut engine = crate::testing::engine_on("oanda", "BTCUSDT", RecordingClient::default());
    engine.reregister_orders(&[order_report("lost", OrderStatus::Accepted, 0.0)]);
    engine.registry.get_mut("lost").unwrap().lifetime_unknown = false;
    let mut outbox = EventBusOutbox::default();
    engine.modify_order("lost", None, Some(99.0), 5, &mut outbox);
    assert_eq!(engine.client.modifies.len(), 1, "{:?}", outbox.0);
    assert!(!refused_over_its_lifetime(&outbox), "{:?}", outbox.0);
}

/// A fill row `id` of `qty`, naming its order by `order_client_id`, or by the venue order id `v7` when `order_client_id` is `None`.
fn fill_row(id: &str, order_client_id: Option<&str>, qty: f64) -> vike_model::FillReport {
    vike_model::FillReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        trade_id: vike_model::events::TradeId::new(id).unwrap(),
        venue_order_id: "v7".into(),
        order_client_id: order_client_id.map(str::to_string),
        side: 1,
        last_qty: qty,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: vike_model::events::LiquiditySide::Maker,
        timestamp: 5,
    }
}

/// `mark_adopted_fills`' judgement, read off the FSM's dedup set: the rows an adopted report holds are marked, by order client id
/// or by venue order id; another order's are not; and a report that lags its own rows marks none of them, because a
/// fill marked but not in the report would never be counted.
#[test]
fn only_the_fills_inside_an_adopted_report_are_marked() {
    let seen = |report: vike_model::OrderStatusReport, fills: &[vike_model::FillReport]| {
        let mut engine_under_test = engine();
        engine_under_test.mark_adopted_fills(&[report], fills);
        engine_under_test.snapshot_state().seen_fsm_trade_ids
    };
    let held = || order_report("a", OrderStatus::PartiallyFilled, 0.5);
    assert_eq!(seen(held(), &[fill_row("e1", Some("a"), 0.5)]), ["e1"]);
    assert_eq!(seen(held(), &[fill_row("e1", None, 0.2)]), ["e1"], "joined by the venue order id");
    assert!(seen(held(), &[fill_row("e1", Some("b"), 0.2)]).is_empty(), "another order's row");
    assert!(
        seen(held(), &[fill_row("e1", Some("a"), 0.5), fill_row("e2", Some("a"), 0.2)]).is_empty(),
        "the rows exceed the report"
    );
    let no_venue_id = vike_model::OrderStatusReport { venue_order_id: "".into(), ..held() };
    let bare = vike_model::FillReport { venue_order_id: "".into(), ..fill_row("e1", None, 0.2) };
    assert!(seen(no_venue_id, &[bare]).is_empty(), "two empty ids join nothing");
}

/// Order-loss recovery (recon JournalDivergence confirm): `reregister_orders` INSERT-ONLY re-seeds
/// a lost order from its venue report (status/filled/venue_id restored, `created_ms: None` adopted),
/// never clobbers an order client id already present, and skips reports with no order_client_id.
#[test]
fn reregister_orders_reseeds_lost_order_insert_only() {
    let mut engine_under_test = engine();

    // an order local already knows must NOT be clobbered by a re-register.
    let mut outbox = EventBusOutbox::default();
    let req = OrderRequest {
        order_client_id: "keep".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    };
    engine_under_test.submit_order(&req, 1, &mut outbox);
    let keep_before = engine_under_test.registry.get("keep").cloned().unwrap();

    // one lost order to recover + one no-order-client-id report (skipped) + the already-known "keep".
    let no_order_client_id = vike_model::OrderStatusReport {
        order_client_id: None,
        ..order_report("x", OrderStatus::Accepted, 0.0)
    };
    let n = engine_under_test.reregister_orders(&[
        order_report("lost", OrderStatus::PartiallyFilled, 0.5),
        no_order_client_id,
        order_report("keep", OrderStatus::Canceled, 0.0), // already known → must be left untouched
    ]);
    assert_eq!(n, 1, "only the genuinely-lost order is re-registered");

    let lost = engine_under_test.registry.get("lost").expect("lost order re-registered");
    assert_eq!(lost.status, OrderStatus::PartiallyFilled, "status reconstructed from the report");
    assert_eq!(lost.filled_qty, 0.5);
    assert_eq!(lost.average_fill_price, 100.0);
    assert_eq!(lost.venue_order_id.as_deref(), Some("v7"));
    assert_eq!(lost.created_ms, None, "adopted order is never swept by the stuck watchdog");

    assert_eq!(
        engine_under_test.registry.get("keep"),
        Some(&keep_before),
        "existing order untouched"
    );
    assert!(
        !engine_under_test.registry.contains_key("x"),
        "a no-order-client-id report is skipped"
    );

    // idempotent: a second pass with the same lost report (now present) re-registers nothing.
    assert_eq!(
        engine_under_test.reregister_orders(&[order_report("lost", OrderStatus::Filled, 2.0)]),
        0
    );
    assert_eq!(
        engine_under_test.registry.get("lost").unwrap().status,
        OrderStatus::PartiallyFilled,
        "not clobbered"
    );
}

/// A lost STOP re-registered from a report that states its terms rests at its trigger again (and a trailing
/// order with its distance), not at no price: the terms ride `OrderRecord::from_status_report`.
#[test]
fn reregister_orders_rebuilds_a_stop_with_its_trigger() {
    let mut engine_under_test = engine();
    let stop = vike_model::OrderStatusReport {
        side: -1,
        order_type: vike_model::OrderType::Stop,
        trigger_price: Some(95.0),
        ..order_report("stop", OrderStatus::Accepted, 0.0)
    };
    let trailing = vike_model::OrderStatusReport {
        venue_order_id: "v8".into(),
        order_type: vike_model::OrderType::Trailing,
        trigger_price: Some(110.0),
        trail_percent: Some(1.5),
        ..order_report("trail", OrderStatus::Accepted, 0.0)
    };
    assert_eq!(engine_under_test.reregister_orders(&[stop, trailing]), 2);
    let stop = &engine_under_test.registry.get("stop").expect("re-registered").request;
    assert_eq!((stop.order_type.clone(), stop.side), (vike_model::OrderType::Stop, -1));
    assert_eq!((stop.price, stop.trigger_price), (None, Some(95.0)), "it rests at its trigger");
    let trail = &engine_under_test.registry.get("trail").expect("re-registered").request;
    assert_eq!(
        (trail.trigger_price, trail.trail, trail.trail_percent),
        (Some(110.0), None, Some(1.5))
    );
}

// --- The contract multiplier is lane-independent (the #458 multiplier-in-context class): the
// gate's notional (`qty × ref_price × ctx.multiplier`) feeds min_notional AND
// max_notional_per_order whether or not the margin lane is armed, as in the backtest ctx. ---

/// Engine with floors/caps armed: floor 5, cap 100; optional per-symbol multiplier grid
/// and optional buying-power lane (`initial_margin_requirement`).
fn multiplier_gate_engine(
    grid_multiplier: Option<f64>,
    initial_margin: Option<f64>,
) -> Engine<RecordingClient> {
    let grid: Option<IndexMap<String, vike_model::ContractValue>> = grid_multiplier.map(|m| {
        [("BTCUSDT".to_string(), vike_model::ContractValue::linear(m))].into_iter().collect()
    });
    let mut engine = Engine::new(
        Account::new(1.0, "sim", grid, BalanceMode::Delta),
        RiskGate::new(RiskLimits {
            min_notional: Some(5.0),
            max_notional_per_order: Some(100.0),
            initial_margin_requirement: initial_margin,
            ..RiskLimits::new()
        }),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    engine.equity_seed = 1_000.0; // ample equity for the armed lane; ignored (0.0 ctx) unarmed
    engine
}

fn buy_limit(order_client_id: &str, qty: f64, px: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(px),
        timestamp: 1,
        ..Default::default()
    }
}

/// Cap side: margin lane UNARMED + multiplier 100. qty·px = 5.0 sits on the floor and under
/// the 100 cap at multiplier 1.0, but the real notional is 5·100 = 500 > cap: DENIED.
#[test]
fn unarmed_lane_notional_cap_judged_with_contract_multiplier() {
    let mut engine = multiplier_gate_engine(Some(100.0), None);
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(&buy_limit("cap", 0.5, 10.0), 1, &mut outbox);
    assert_eq!(denied_reason(&outbox).as_deref(), Some("over-max-notional"));
    assert!(engine.client.submissions.is_empty(), "over-cap notional must not reach the venue");
    assert!(!engine.registry.contains_key("cap"), "a denied order never enters the registry");
}

/// Floor mirror: qty·px = 0.1 < floor 5 at multiplier 1.0, but the real notional 0.1·100 = 10
/// clears the floor and sits under the cap: ADMITTED.
#[test]
fn unarmed_lane_notional_floor_judged_with_contract_multiplier() {
    let mut engine = multiplier_gate_engine(Some(100.0), None);
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(&buy_limit("floor", 0.01, 10.0), 1, &mut outbox);
    assert_eq!(denied_reason(&outbox), None);
    assert_eq!(engine.client.submissions.len(), 1, "real notional clears the floor → submitted");
    assert!(engine.registry.contains_key("floor"));
}

/// THE COMPAT PIN: multiplier 1.0 (symbol absent from the grid). Verdicts are the raw qty·px
/// judgment in BOTH lanes.
#[test]
fn multiplier_one_verdicts_identical_across_lanes() {
    for initial_margin in [None, Some(0.001)] {
        // below the floor → denied, both lanes
        let mut engine = multiplier_gate_engine(None, initial_margin);
        let mut outbox = EventBusOutbox::default();
        engine.submit_order(&buy_limit("lo", 0.1, 10.0), 1, &mut outbox);
        assert_eq!(
            denied_reason(&outbox).as_deref(),
            Some("below-min-notional"),
            "im={initial_margin:?}"
        );
        // over the cap → denied, both lanes
        let mut engine = multiplier_gate_engine(None, initial_margin);
        let mut outbox = EventBusOutbox::default();
        engine.submit_order(&buy_limit("hi", 20.0, 10.0), 1, &mut outbox);
        assert_eq!(
            denied_reason(&outbox).as_deref(),
            Some("over-max-notional"),
            "im={initial_margin:?}"
        );
        // in-band → admitted, both lanes
        let mut engine = multiplier_gate_engine(None, initial_margin);
        let mut outbox = EventBusOutbox::default();
        engine.submit_order(&buy_limit("ok", 1.0, 10.0), 1, &mut outbox);
        assert_eq!(denied_reason(&outbox), None, "im={initial_margin:?}");
        assert_eq!(engine.client.submissions.len(), 1, "im={initial_margin:?}");
    }
}

/// THE ARMED-LANE PIN: same mult-100 over-cap order, margin lane ARMED, denies
/// over-max-notional; an in-band mult-100 order clears notional AND buying power
/// (notional 0.05·10·100 = 50 ∈ [5, 100]; initial margin = 50·0.01 = 0.5 ≤ equity 1000).
#[test]
fn armed_lane_multiplier_behavior_unchanged() {
    let mut engine = multiplier_gate_engine(Some(100.0), Some(0.01));
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(&buy_limit("cap", 0.5, 10.0), 1, &mut outbox);
    assert_eq!(denied_reason(&outbox).as_deref(), Some("over-max-notional"));

    let mut engine = multiplier_gate_engine(Some(100.0), Some(0.01));
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(&buy_limit("ok", 0.05, 10.0), 1, &mut outbox);
    assert_eq!(denied_reason(&outbox), None);
    assert_eq!(engine.client.submissions.len(), 1);
}

/// ⚠ **AN ORDER ALREADY IN FLIGHT MUST CONSUME BUYING POWER**, or a strategy that submits twice
/// before the first fill gets both admitted against the same equity (an un-filled order holds no
/// position). The backtest counts its pending orders (`OrderBrokerSimulator::margin_in_use_pending_aware`).
///
/// The numbers: equity 50, im 0.1, mark 100. One order of 3 commits 3·100·1·0.1 = 30, leaving
/// free 20. A second order of 3 needs 30 > 20 and must be DENIED.
///
/// NON-VACUOUS in both directions: the FIRST order is asserted to still be admitted (so the
/// term did not simply break the gate), and a size that fits inside the remaining 20 is
/// asserted to still pass (so the denial is the margin arithmetic, not a blanket refusal of any
/// second order).
#[test]
fn a_live_unfilled_order_consumes_buying_power() {
    let mk = || {
        let mut engine = Engine::new(
            Account::new(1.0, "sim", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits {
                initial_margin_requirement: Some(0.1),
                ..RiskLimits::new()
            }),
            RecordingClient::default(),
            "sim",
            "BTCUSDT",
        );
        engine.equity_seed = 50.0;
        engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0);
        engine.price_board.set_mark("sim", "BTCUSDT", 100.0, 0);
        engine
    };
    let order = |order_client_id: &str, qty: f64| OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty,
        timestamp: 1,
        ..Default::default()
    };

    let mut engine = mk();
    let mut outbox = EventBusOutbox::default();
    engine.submit_order(&order("c1", 3.0), 1, &mut outbox);
    assert_eq!(engine.client.submissions.len(), 1, "the first order is admitted: 30 <= 50");

    // The venue accepts it — live, un-filled, holding no position yet.
    engine.on_event(
        &Event::OrderAccepted(vike_model::events::OrderAccepted {
            order_client_id: "c1".into(),
            venue_order_id: None,
            timestamp: 1,
        }),
        &mut outbox,
    );

    // A second order of the same size needs 30 against a remaining 20.
    let mut ob2 = EventBusOutbox::default();
    engine.submit_order(&order("c2", 3.0), 2, &mut ob2);
    assert_eq!(
        engine.client.submissions.len(),
        1,
        "the second order must be DENIED — the first committed 30 of the 50 equity, and \
             counting positions alone made that commitment invisible"
    );
    assert!(
        ob2.0.iter().any(|e| matches!(e, Event::OrderDenied(_))),
        "and it denies through the normal veto path, not by vanishing"
    );

    // ...but one that FITS in the remaining 20 still passes, so this is the margin arithmetic
    // rather than a blanket refusal of any second order.
    let mut ob3 = EventBusOutbox::default();
    engine.submit_order(&order("c3", 1.0), 3, &mut ob3);
    assert_eq!(engine.client.submissions.len(), 2, "10 <= the remaining 20 is still admitted");
}

/// **What the exposure sum may count at an order's OWN price, and what stays unpriced** (A02): only the four kinds that
/// state a price (LIMIT, STOP, STOP-LIMIT, TAKE-PROFIT) and only a finite positive one. A MARKET order, a kind this
/// build does not know and a price that is zero, negative or not a number have none, so the sum stays unbounded for
/// them and the gate keeps denying `account-exposure-unpriced`. A limit wins over a trigger, the sizing the gate
/// gives the judged order.
#[test]
fn only_a_stated_finite_positive_price_counts_a_resting_order_when_its_market_is_closed() {
    use crate::execution_engine::valuation::resting_own_price;
    use vike_model::OrderType::{Limit, Market, Stop, StopLimit, TakeProfit, Unrecognized};
    let of = |order_type, price, trigger_price| {
        resting_own_price(&OrderRequest { order_type, price, trigger_price, ..Default::default() })
    };
    assert_eq!(of(Limit, Some(10.0), None), Some(10.0));
    assert_eq!(of(Stop, None, Some(11.0)), Some(11.0));
    assert_eq!(of(TakeProfit, None, Some(12.0)), Some(12.0));
    assert_eq!(of(StopLimit, Some(9.0), Some(11.0)), Some(9.0), "the limit before the trigger");
    assert_eq!(of(StopLimit, None, Some(11.0)), Some(11.0));
    assert_eq!(of(Limit, None, Some(13.0)), Some(13.0), "the gate's `price.or(trigger_price)`");

    assert_eq!(of(Market, Some(10.0), Some(10.0)), None, "a market order has no price of its own");
    assert_eq!(of(Unrecognized("iceberg".into()), Some(10.0), None), None);
    assert_eq!(of(Limit, None, None), None, "no price stated");
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(of(Limit, Some(bad), None), None, "{bad} is not a price");
        assert_eq!(of(Stop, None, Some(bad)), None, "{bad} is not a trigger");
    }
}

/// PIN for `counted_remaining`, the ONE rule of which orders `live_order_margin` and `live_order_notional`
/// (summed by `resolved_account_exposure_excluding`) charge: every order but the judged order client id whose status is live,
/// whose remaining quantity is finite and positive, and that is not a covered reduce. One counted order, then one
/// order per skip, each of which both walks would otherwise charge.
#[test]
fn both_live_order_walks_count_the_same_orders() {
    let mut engine_under_test = engine();
    engine_under_test.price_board.set_mark("sim", "BTCUSDT", 100.0, 0);
    // a long 10, so a sell of 2 is a covered reduce
    engine_under_test.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), vike_model::events::PositionSide::Both),
        crate::PositionEntry { size: 10.0, average_price: 100.0, ..Default::default() },
    );
    for (order_client_id, side, qty, filled, status) in [
        ("counted", 1, 3.0, 1.0, OrderStatus::PartiallyFilled), // 2 left: 2 x 100
        ("judged", 1, 5.0, 0.0, OrderStatus::Accepted),
        ("canceled", 1, 7.0, 0.0, OrderStatus::Canceled),
        ("liquidated", 1, 7.0, 0.0, OrderStatus::Liquidated),
        ("overfilled", 1, 4.0, 6.0, OrderStatus::Accepted),
        ("nan", 1, f64::NAN, 0.0, OrderStatus::Accepted),
        ("covered", -1, 2.0, 0.0, OrderStatus::Accepted),
    ] {
        let mut mo = OrderRecord::new(OrderRequest {
            order_client_id: order_client_id.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side,
            qty,
            order_type: vike_model::OrderType::Limit,
            price: Some(100.0),
            timestamp: 1,
            ..Default::default()
        });
        mo.status = status;
        mo.filled_qty = filled;
        engine_under_test.registry.insert(order_client_id.to_string(), mo);
    }
    let config = engine_under_test.price_config;
    assert_eq!(
        engine_under_test.live_order_margin(&config, 0.5, "judged").to_bits(),
        100.0_f64.to_bits()
    );
    // the BTCUSDT position is the judged symbol's (the gate re-adds it projected): only the orders count
    let exposure =
        engine_under_test.resolved_account_exposure_excluding("BTCUSDT", "judged", &config);
    assert_eq!(exposure.to_bits(), 200.0_f64.to_bits());
}

/// A live COMBO counts its LEGS in both walks (its own `symbol` is empty, `vike_model::build_combo`): each leg
/// `|ratio| x remaining` on the side `combo side x sign(ratio)` (`RiskGate::check_combo`'s law), at its own
/// symbol's price, contract value and initial margin. A covered-reduce leg is left out, as a plain one is; an unpriced leg is
/// skipped by the margin walk and makes the exposure sum infinite, the rule for a plain order with no price.
#[test]
fn a_live_combo_counts_each_leg_at_its_own_price() {
    let grid: IndexMap<String, vike_model::ContractValue> =
        [("LEGB".to_string(), vike_model::ContractValue::linear(10.0))].into_iter().collect();
    let mut engine = Engine::new(
        Account::new(1.0, "sim", Some(grid), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    engine.gate.limits.initial_margin_by_symbol.insert("LEGB".into(), 0.25);
    engine.price_board.set_mark("sim", "LEGA", 100.0, 0);
    // SELL 2 combos: LEGA (+1) sells 2, LEGB (-2) buys 4. A credit net, the case that read as unpriced.
    let spec = vike_model::OrderCombo {
        venue: "sim".into(),
        side: -1,
        qty: 2.0,
        legs: vec![
            vike_model::OrderComboLeg { symbol: "LEGA".into(), ratio: 1, limit: None },
            vike_model::OrderComboLeg { symbol: "LEGB".into(), ratio: -2, limit: None },
        ],
        net_limit: Some(-3.0),
        time_in_force: vike_model::TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    };
    let req = vike_model::build_combo(&spec, "combo").expect("a valid spec");
    engine.registry.insert("combo".into(), OrderRecord::new(req));
    let config = engine.price_config;

    // LEGB unpriced: the margin walk counts LEGA alone (2 x 100 x 0.5); the exposure sum is unbounded.
    assert_eq!(engine.live_order_margin(&config, 0.5, ""), 100.0);
    assert_eq!(engine.resolved_account_exposure_excluding("LEGA", "", &config), f64::INFINITY);

    // LEGB at 50: 4 x 50 x 10 = 2 000 of exposure, 500 of margin at its own 0.25.
    engine.price_board.set_mark("sim", "LEGB", 50.0, 0);
    assert_eq!(engine.live_order_margin(&config, 0.5, ""), 600.0);
    assert_eq!(engine.resolved_account_exposure_excluding("LEGA", "", &config), 2_200.0);

    // Long 5 LEGA: the LEGA leg (sell 2) is a covered reduce and commits nothing.
    engine.account.positions.insert(
        ("sim".into(), "LEGA".into(), vike_model::events::PositionSide::Both),
        crate::PositionEntry { size: 5.0, average_price: 100.0, ..Default::default() },
    );
    assert_eq!(engine.live_order_margin(&config, 0.5, ""), 500.0);
    assert_eq!(engine.resolved_account_exposure_excluding("LEGA", "", &config), 2_000.0);
}

/// **A partly filled combo's `filled_qty` must be COMBO units**, the unit of its `qty`, or both walks misjudge
/// what still rests. BUY 10 of a 2:-1 combo (LEGA +2, LEGB -1, both at 100); 3 combo units fill, delivered the
/// way the venue that rests a partial (deribit) and the paper book deliver them: a bare `Fill` plus an FSM wrap
/// per LEG row (LEGA 6, LEGB 3), then deribit's aggregate print under the combo instrument (3 at the net), which
/// books no position. 7 units rest: LEGA 14 x 100 + LEGB 7 x 100 = 2 100 of orders (1 050 of margin at 0.5),
/// plus the 900 the filled legs now hold as positions.
#[test]
fn a_partly_filled_combo_counts_its_remaining_combo_units() {
    let mut engine_under_test = engine();
    engine_under_test.extra_symbols = vec!["LEGA".into(), "LEGB".into()];
    engine_under_test.price_board.set_mark("sim", "LEGA", 100.0, 0);
    engine_under_test.price_board.set_mark("sim", "LEGB", 100.0, 0);
    let spec = vike_model::OrderCombo {
        venue: "sim".into(),
        side: 1,
        qty: 10.0,
        legs: vec![
            vike_model::OrderComboLeg { symbol: "LEGA".into(), ratio: 2, limit: None },
            vike_model::OrderComboLeg { symbol: "LEGB".into(), ratio: -1, limit: None },
        ],
        net_limit: Some(100.0),
        time_in_force: vike_model::TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    };
    engine_under_test.registry.insert(
        "combo".into(),
        OrderRecord::new(vike_model::build_combo(&spec, "combo").expect("a valid spec")),
    );
    let mut outbox = EventBusOutbox::default();
    engine_under_test.on_event(
        &Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            order_client_id: "combo".into(),
            timestamp: 1,
        }),
        &mut outbox,
    );
    engine_under_test.on_event(
        &Event::OrderAccepted(vike_model::events::OrderAccepted {
            order_client_id: "combo".into(),
            venue_order_id: Some("v-combo".into()),
            timestamp: 1,
        }),
        &mut outbox,
    );
    let row = |engine_under_test: &mut Engine<RecordingClient>,
               tid: &'static str,
               symbol: &str,
               side: i32,
               qty: f64| {
        let mut outbox = EventBusOutbox::default();
        let fill = FillEvent {
            trade_id: tid.into(),
            order_client_id: "combo".into(),
            venue: "sim".into(),
            symbol: symbol.into(),
            side,
            last_qty: qty,
            last_price: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            timestamp: 2,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        };
        engine_under_test.on_event(&Event::Fill(fill.clone()), &mut outbox);
        engine_under_test.on_event(
            &Event::OrderPartiallyFilled(vike_model::events::OrderPartiallyFilled {
                order_client_id: "combo".into(),
                fill,
                timestamp: 2,
            }),
            &mut outbox,
        );
    };
    row(&mut engine_under_test, "t-a", "LEGA", 1, 6.0);
    row(&mut engine_under_test, "t-b", "LEGB", -1, 3.0);
    let config = engine_under_test.price_config;
    let seen = |engine_under_test: &Engine<RecordingClient>| {
        (
            engine_under_test.registry["combo"].filled_qty,
            engine_under_test.live_order_margin(&config, 0.5, ""),
            engine_under_test.resolved_account_exposure_excluding("BTCUSDT", "", &config),
        )
    };
    assert_eq!(engine_under_test.registry["combo"].status, OrderStatus::PartiallyFilled);
    assert_eq!(
        seen(&engine_under_test),
        (3.0, 1_050.0, 3_000.0),
        "(filled, margin, exposure) after the legs"
    );
    row(&mut engine_under_test, "t-net", "SIM-COMBO-LEGA_LEGB", 1, 3.0);
    assert_eq!(
        seen(&engine_under_test),
        (3.0, 1_050.0, 3_000.0),
        "deribit's net print adds no unit"
    );
}

/// A trailing order is never modified at a venue: a price, an activation or a size change is refused in
/// `TRAILING_AMEND_REASON`'s words before anything is sent, and no cancel-then-new is planned (it would
/// restart the trail). A client that declares its own amend convention (the paper book) still moves the
/// size in place, and nothing else.
#[test]
fn a_trailing_order_is_never_modified_at_a_venue_and_only_resized_in_place() {
    use vike_model::OrderType::{Trailing, TrailingLimit};
    let rest = |client: RecordingClient, order_type: vike_model::OrderType| {
        let mut engine = crate::testing::engine_on("deribit", "BTCUSDT", client);
        let mut mo = OrderRecord::new(OrderRequest {
            order_client_id: "t1".into(),
            venue: "deribit".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 10.0,
            order_type,
            trail: Some(50.0),
            reduce_only: true,
            timestamp: 1,
            ..Default::default()
        });
        mo.status = OrderStatus::Accepted;
        engine.registry.insert("t1".to_string(), mo);
        engine
    };
    let refused = |outbox: &EventBusOutbox| {
        outbox.0.iter().any(|e| {
            matches!(e, Event::OrderModifyRejected(r)
                if r.reason == format!("deribit: {TRAILING_AMEND_REASON}"))
        })
    };
    for order_type in [Trailing, TrailingLimit] {
        let mut engine = rest(RecordingClient::default(), order_type.clone());
        for (qty, px, trig) in
            [(Some(5.0), None, None), (None, Some(99.0), None), (None, None, Some(1.0))]
        {
            let mut outbox = EventBusOutbox::default();
            engine.modify_order_with_trigger("t1", qty, px, trig, 5, &mut outbox);
            assert!(refused(&outbox), "{order_type}: {qty:?} {px:?} {trig:?}");
        }
        let mut outbox = EventBusOutbox::default();
        assert!(engine.plan_replacement("t1", Some(5.0), None, None, 5, &mut outbox).is_none());
        assert!(refused(&outbox), "{order_type}: no replacement is planned");
        assert!(engine.client.modifies.is_empty() && engine.client.trigger_modifies.is_empty());

        let paper = RecordingClient {
            declared_amend_semantics: Some(vike_venues::AmendSemantics::InPlaceRemaining),
            ..RecordingClient::default()
        };
        let mut engine = rest(paper, order_type.clone());
        let mut outbox = EventBusOutbox::default();
        engine.modify_order_with_trigger("t1", None, Some(99.0), None, 5, &mut outbox);
        assert!(refused(&outbox), "{order_type}: paper refuses a price too");
        let mut outbox = EventBusOutbox::default();
        engine.modify_order_with_trigger("t1", Some(5.0), None, None, 5, &mut outbox);
        assert!(!refused(&outbox), "{order_type}: paper moves the size in place");
        assert_eq!(engine.client.modifies.len(), 1, "{order_type}: the size reached the client");
    }
}

/// An ADJUSTABLE stop is refused as a trailing order is (decision 0267, `ADJUSTABLE_AMEND_REASON`): a
/// price, a trigger or (at a venue) a size change, natively or by cancel-then-new, since its adjustment may
/// already have made it another order; a client that moves it in place (the paper book) moves the size.
#[test]
fn an_adjustable_stop_is_never_repriced_and_only_resized_in_place() {
    let rest = |client: RecordingClient| {
        let mut engine = crate::testing::engine_on("deribit", "BTCUSDT", client);
        let mut mo = OrderRecord::new(OrderRequest {
            order_client_id: "a1".into(),
            venue: "deribit".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 10.0,
            order_type: vike_model::OrderType::Stop,
            trigger_price: Some(95.0),
            adjust: Some(vike_model::OrderStopAdjust {
                at: 105.0,
                to: vike_model::OrderStopAdjustTo::Stop { stop: 101.0 },
            }),
            timestamp: 1,
            ..Default::default()
        });
        mo.status = OrderStatus::Accepted;
        engine.registry.insert("a1".to_string(), mo);
        engine
    };
    let refused = |outbox: &EventBusOutbox| {
        outbox.0.iter().any(|e| {
            matches!(e, Event::OrderModifyRejected(r)
                if r.reason == format!("deribit: {ADJUSTABLE_AMEND_REASON}"))
        })
    };
    let mut engine = rest(RecordingClient::default());
    for (qty, px, trig) in
        [(Some(5.0), None, None), (None, Some(99.0), None), (None, None, Some(96.0))]
    {
        let mut outbox = EventBusOutbox::default();
        engine.modify_order_with_trigger("a1", qty, px, trig, 5, &mut outbox);
        assert!(refused(&outbox), "{qty:?} {px:?} {trig:?}");
    }
    let mut outbox = EventBusOutbox::default();
    assert!(engine.plan_replacement("a1", Some(5.0), None, None, 5, &mut outbox).is_none());
    assert!(refused(&outbox), "no replacement is planned");
    assert!(engine.client.modifies.is_empty() && engine.client.trigger_modifies.is_empty());

    let paper = RecordingClient {
        declared_amend_semantics: Some(vike_venues::AmendSemantics::InPlaceRemaining),
        ..RecordingClient::default()
    };
    let mut engine = rest(paper);
    let mut outbox = EventBusOutbox::default();
    engine.modify_order_with_trigger("a1", None, None, Some(96.0), 5, &mut outbox);
    assert!(refused(&outbox), "paper refuses a trigger too");
    let mut outbox = EventBusOutbox::default();
    engine.modify_order_with_trigger("a1", Some(5.0), None, None, 5, &mut outbox);
    assert!(!refused(&outbox), "paper moves the size in place");
    assert_eq!(engine.client.modifies.len(), 1, "the size reached the client");
}
