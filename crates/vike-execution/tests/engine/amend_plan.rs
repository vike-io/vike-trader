//! What the engine says about HOW a resting order is amended (`Engine::amend_mode_of`) and the
//! order a cancel-then-new amend would place (`Engine::plan_replacement`), judged by the same
//! RiskGate step as a native amend. The carrier that acts on them is `vike-core`'s `apply/amend.rs`
//! (its tests drive the whole machine); this suite pins the engine's half: the mode, the terms of the
//! replacement, and that planning SENDS nothing.

use vike_execution::testing::RecordingClient;
use vike_execution::{Engine, EventBus, EventBusOutbox};
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::{OrderRequest, OrderStatus, OrderType, RiskLimits};
use vike_venues::AmendSemantics;
use vike_venues::venue_amend_mode::AmendMode;

use vike_execution::testing::EngineBuilder;

/// A resting order of `order_type`: a trigger kind is a reduce-only sell with its trigger (a stop-limit
/// with its limit price as well), anything else a buy at 100.
fn order(venue: &str, order_type: OrderType, order_client_id: &str) -> OrderRequest {
    let trigger = order_type.is_trigger();
    let priced = matches!(order_type, OrderType::Limit | OrderType::StopLimit);
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        side: if trigger { -1 } else { 1 },
        qty: 10.0,
        order_type,
        price: priced.then_some(100.0),
        trigger_price: trigger.then_some(90.0),
        reduce_only: trigger,
        timestamp: 1,
        ..Default::default()
    }
}

/// An engine on `venue` with `req` resting ACCEPTED.
fn resting(req: OrderRequest, limits: RiskLimits) -> Engine<RecordingClient> {
    let venue = req.venue.clone();
    let mut e = EngineBuilder {
        venue: venue.clone(),
        symbol: "BTCUSDT".into(),
        limits,
        ..Default::default()
    }
    .build();
    let order_client_id = req.order_client_id.clone();
    e.submit_order(&req, 0, &mut EventBusOutbox::default());
    assert_eq!(e.client.submissions.len(), 1, "precondition: the order was admitted");
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.clone(),
            timestamp: 0,
        }),
        &mut e,
    );
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            order_client_id,
            venue_order_id: Some("v1".into()),
            timestamp: 1,
        }),
        &mut e,
    );
    e
}

fn rejections(outbox: &EventBusOutbox) -> Vec<String> {
    outbox
        .0
        .iter()
        .filter_map(|e| match e {
            Event::OrderModifyRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_mode_is_the_venue_tables_for_a_venue_adapter() {
    let e = resting(order("binance", OrderType::Limit, "l1"), RiskLimits::new());
    let req = |t| order("binance", t, "x");
    assert_eq!(e.amend_mode_of(&req(OrderType::Limit)), Some(AmendMode::Native));
    assert_eq!(e.amend_mode_of(&req(OrderType::Stop)), Some(AmendMode::CancelThenNew));
    assert_eq!(e.amend_mode_of(&req(OrderType::Market)), None, "a market order does not rest");
    let p = resting(order("polymarket", OrderType::Limit, "l1"), RiskLimits::new());
    assert_eq!(
        p.amend_mode_of(&order("polymarket", OrderType::Limit, "x")),
        Some(AmendMode::CancelThenNew)
    );
}

/// A client that declares its own amend convention (the paper exchange) is judged by it, whatever
/// venue string the engine carries: its book re-prices every kind in place.
#[test]
fn a_client_that_declares_a_convention_is_native_for_every_kind() {
    let mut e = resting(order("polymarket", OrderType::Limit, "l1"), RiskLimits::new());
    e.client.declared_amend_semantics = Some(AmendSemantics::InPlaceRemaining);
    for t in [OrderType::Limit, OrderType::Stop] {
        assert_eq!(e.amend_mode_of(&order("polymarket", t, "x")), Some(AmendMode::Native));
    }
}

/// Planning a stop's replacement returns the NEW terms (the price an amend names is the stop's
/// trigger), keeps everything else, and sends nothing.
#[test]
fn planning_a_stop_replacement_moves_the_trigger_and_sends_nothing() {
    let mut e = resting(order("binance", OrderType::Stop, "s1"), RiskLimits::new());
    let mut outbox = EventBusOutbox::default();
    let plan = e.plan_replacement("s1", None, Some(91.0), None, 5, &mut outbox).expect("planned");
    assert_eq!(plan.trigger_price, Some(91.0));
    assert_eq!(plan.price, None, "a stop has no limit price and the plan does not invent one");
    assert_eq!(plan.qty, 10.0);
    assert!(plan.reduce_only && plan.order_type == OrderType::Stop);
    assert!(rejections(&outbox).is_empty());
    assert_eq!(e.client.submissions.len(), 1, "planning places nothing");
    assert!(e.client.cancels.is_empty() && e.client.modifies.is_empty());
}

/// A replacement never re-places lots the old order already executed: unnamed, the quantity is the
/// UNFILLED part; named, it is the fresh order's own size (a cancel-replace, nothing netted).
#[test]
fn a_partly_filled_order_is_replaced_by_what_is_unfilled() {
    let mut e = resting(order("polymarket", OrderType::Limit, "l1"), RiskLimits::new());
    let mo = e.registry.get_mut("l1").unwrap();
    mo.status = OrderStatus::PartiallyFilled;
    mo.filled_qty = 4.0;
    let mut outbox = EventBusOutbox::default();
    let plan = e.plan_replacement("l1", None, Some(101.0), None, 5, &mut outbox).expect("planned");
    assert_eq!(plan.qty, 6.0, "10 ordered, 4 executed");
    assert_eq!(plan.price, Some(101.0));
    let plan = e.plan_replacement("l1", Some(8.0), None, None, 5, &mut outbox).expect("planned");
    assert_eq!(plan.qty, 8.0, "an explicit size is the fresh order's size");
    assert_eq!(plan.price, Some(100.0), "the resting price is kept");
}

/// Nothing left to rest, or an order that does not rest: a non-terminal refusal in words, no plan.
#[test]
fn an_order_with_nothing_to_rest_is_refused_in_words() {
    let mut e = resting(order("polymarket", OrderType::Limit, "l1"), RiskLimits::new());
    e.registry.get_mut("l1").unwrap().filled_qty = 10.0;
    let mut outbox = EventBusOutbox::default();
    assert!(e.plan_replacement("l1", None, Some(101.0), None, 5, &mut outbox).is_none());
    assert!(rejections(&outbox)[0].contains("nothing of the order is left unfilled"));

    let mut outbox = EventBusOutbox::default();
    assert!(e.plan_replacement("ghost", None, Some(101.0), None, 5, &mut outbox).is_none());
    assert!(rejections(&outbox)[0].contains("not resting"));
}

/// The RiskGate judges the replacement like any modify: a veto is a non-terminal refusal and no plan.
#[test]
fn the_risk_gate_judges_the_replacement() {
    let limits = RiskLimits { max_notional_per_order: Some(2_000.0), ..RiskLimits::new() };
    let mut e = resting(order("polymarket", OrderType::Limit, "l1"), limits);
    let mut outbox = EventBusOutbox::default();
    assert!(e.plan_replacement("l1", Some(1_000.0), None, None, 5, &mut outbox).is_none());
    let said = rejections(&outbox);
    assert_eq!(said.len(), 1);
    assert!(said[0].starts_with("risk:"), "{said:?}");
}

/// A stop-limit or limit-if-touched the venue has FIRED rests as the limit it released, so its
/// replacement is that limit with no trigger (it would otherwise re-arm one already spent): after an
/// `OrderTriggered`, and after a part fill, which only a fired one can have. An unfired stop-limit and a
/// fired stop keep their kinds, as before. A market-if-touched takes no price at all
/// (`a_price_on_a_market_if_touched_is_refused_before_anything_is_sent`).
#[test]
fn a_fired_stop_limit_is_replaced_by_the_limit_it_released() {
    use OrderType::{Limit, Stop, StopLimit, TakeProfit};
    use vike_model::events::OrderTriggered;
    let lit = OrderRequest { price: Some(100.0), ..order("deribit", TakeProfit, "c1") };
    #[rustfmt::skip]
    let rows = [
        ("stop-limit, fired", order("deribit", StopLimit, "c1"), true, (Limit, Some(95.0), None)),
        ("limit-if-touched, fired", lit, true, (Limit, Some(95.0), None)),
        ("stop-limit, not fired", order("deribit", StopLimit, "c1"), false, (StopLimit, Some(95.0), Some(90.0))),
        ("stop, fired", order("deribit", Stop, "c1"), true, (Stop, None, Some(95.0))),
    ];
    let fire = |e: &mut Engine<RecordingClient>| {
        let triggered = OrderTriggered { order_client_id: "c1".into(), timestamp: 2 };
        EventBus::new().publish(Event::OrderTriggered(triggered), e);
        assert_eq!(e.registry.get("c1").unwrap().status, OrderStatus::Triggered);
    };
    for (case, req, fired, want) in rows {
        let mut e = resting(req, RiskLimits::new());
        if fired {
            fire(&mut e);
        }
        let mut outbox = EventBusOutbox::default();
        let plan = e.plan_replacement("c1", None, Some(95.0), None, 5, &mut outbox);
        let plan = plan.unwrap_or_else(|| panic!("{case}: {:?}", rejections(&outbox)));
        assert_eq!((plan.order_type, plan.price, plan.trigger_price), want, "{case}");
        assert!(plan.reduce_only && plan.qty == 10.0, "{case}: the rest of the terms are kept");
    }
    // unnamed, the price is the released limit itself; a part fill is proof of the fire as well
    let mut e = resting(order("deribit", StopLimit, "c1"), RiskLimits::new());
    fire(&mut e);
    let plan = e
        .plan_replacement("c1", None, None, None, 5, &mut EventBusOutbox::default())
        .expect("planned");
    assert_eq!((plan.order_type, plan.price, plan.trigger_price), (Limit, Some(100.0), None));
    let mut e = resting(order("deribit", StopLimit, "c1"), RiskLimits::new());
    let mo = e.registry.get_mut("c1").unwrap();
    (mo.status, mo.filled_qty) = (OrderStatus::PartiallyFilled, 4.0);
    let plan = e
        .plan_replacement("c1", None, None, None, 5, &mut EventBusOutbox::default())
        .expect("planned");
    assert_eq!((plan.order_type, plan.qty, plan.trigger_price), (Limit, 6.0, None));
}

/// CONFORMANCE, every roster venue and every resting kind: the engine does what the venue's amend row says.
/// A native row forwards the modify to the client exactly once; a cancel-then-new row plans a replacement
/// (a reduce-only trigger order NEW-FIRST, anything else cancel-first) and sends nothing; a refused row is
/// skipped here (the engine's own refusal is `modify_unsupported_venue.rs`'s). The roster is DERIVED from
/// `vike_model::VENUES`, so a new venue is covered on arrival.
#[test]
fn every_roster_venue_does_what_its_amend_row_says() {
    use vike_execution::amend::AmendPlacement;
    use vike_venues::venue_amend_mode::{AmendKind, amend_mode};
    let mut checked = 0;
    for &venue in vike_model::VENUES {
        for kind in AmendKind::ALL {
            let mode = amend_mode(venue, kind);
            if mode == AmendMode::Refused {
                continue;
            }
            let order_type = match kind {
                AmendKind::Limit => OrderType::Limit,
                AmendKind::Stop => OrderType::Stop,
                AmendKind::StopLimit => OrderType::StopLimit,
                AmendKind::TakeProfit => OrderType::TakeProfit,
                AmendKind::Trailing => OrderType::Trailing,
                AmendKind::TrailingLimit => OrderType::TrailingLimit,
                AmendKind::TrailingTakeProfit => OrderType::TrailingTakeProfit,
            };
            // A take-profit WITH a limit: a market-if-touched has no limit to move (`MIT_PRICE_AMEND_REASON`).
            let req = match kind {
                AmendKind::TakeProfit => {
                    OrderRequest { price: Some(100.0), ..order(venue, order_type, "c1") }
                }
                _ => order(venue, order_type, "c1"),
            };
            let mut e = resting(req.clone(), RiskLimits::new());
            let at = format!("{venue} / {}", kind.word());
            assert_eq!(e.amend_mode_of(&req), Some(mode), "{at}");
            let mut outbox = EventBusOutbox::default();
            match mode {
                AmendMode::Native => {
                    e.modify_order("c1", None, Some(95.0), 5, &mut outbox);
                    assert_eq!(
                        e.client.modifies.len(),
                        1,
                        "{at}: a native amend reaches the client once"
                    );
                    assert!(rejections(&outbox).is_empty(), "{at}: {:?}", rejections(&outbox));
                }
                AmendMode::CancelThenNew => {
                    let plan = e.plan_replacement("c1", None, Some(95.0), None, 5, &mut outbox);
                    assert!(plan.is_some(), "{at}: {:?}", rejections(&outbox));
                    assert!(e.client.modifies.is_empty() && e.client.cancels.is_empty(), "{at}");
                    assert_eq!(e.client.submissions.len(), 1, "{at}: planning places nothing");
                    let want = if kind == AmendKind::Limit {
                        AmendPlacement::CancelFirst
                    } else {
                        AmendPlacement::NewFirst
                    };
                    assert_eq!(AmendPlacement::of(&req), want, "{at}");
                }
                AmendMode::Refused => unreachable!("skipped above"),
            }
            checked += 1;
        }
    }
    assert!(checked >= 20, "the roster x kinds matrix covered only {checked} cells");
}

/// The TRIGGER amend, native half: a stop-limit's trigger goes to `modify_trigger` with its limit beside it
/// and `modify` is not called; a stop's explicit trigger rides `modify`'s one price, as a stop's trigger
/// always has; with no trigger, `modify_order_with_trigger` is `modify_order` exactly.
#[test]
fn a_trigger_amend_reaches_modify_trigger_and_a_stops_rides_its_one_price() {
    use OrderType::{Stop, StopLimit};
    let mut e = resting(order("deribit", StopLimit, "c1"), RiskLimits::new());
    let mut outbox = EventBusOutbox::default();
    e.modify_order_with_trigger("c1", None, Some(99.0), Some(91.0), 5, &mut outbox);
    assert!(rejections(&outbox).is_empty(), "{:?}", rejections(&outbox));
    assert_eq!(e.client.trigger_modifies, vec![("c1".to_string(), None, Some(99.0), 91.0)]);
    assert!(e.client.modifies.is_empty(), "the limit is not moved alone");

    let mut e = resting(order("deribit", Stop, "s1"), RiskLimits::new());
    e.modify_order_with_trigger(
        "s1",
        None,
        Some(95.0),
        Some(94.0),
        5,
        &mut EventBusOutbox::default(),
    );
    assert_eq!(e.client.modifies, vec![("s1".to_string(), None, Some(94.0))], "the explicit wins");
    assert!(e.client.trigger_modifies.is_empty());

    let mut e = resting(order("deribit", StopLimit, "c1"), RiskLimits::new());
    e.modify_order_with_trigger(
        "c1",
        Some(5.0),
        Some(99.0),
        None,
        5,
        &mut EventBusOutbox::default(),
    );
    let mut f = resting(order("deribit", StopLimit, "c1"), RiskLimits::new());
    f.modify_order("c1", Some(5.0), Some(99.0), 5, &mut EventBusOutbox::default());
    assert_eq!(e.client.modifies, f.client.modifies, "no trigger: the one-price modify, unchanged");
    assert!(e.client.trigger_modifies.is_empty());
}

/// A trigger is never dropped: a client that cannot move one refuses in words (the venue named) and
/// nothing is sent; a trigger on an order with no trigger to move (a limit, a FIRED stop-limit) is refused
/// in words by the engine, natively and in a cancel-then-new plan alike.
#[test]
fn a_trigger_that_cannot_move_is_refused_in_words_and_nothing_is_sent() {
    use OrderType::{Limit, StopLimit};
    use vike_execution::execution_engine::{
        TRIGGER_AMEND_NO_TRIGGER_REASON, TRIGGER_AMEND_UNSUPPORTED_REASON,
    };
    let mut e = resting(order("deribit", StopLimit, "c1"), RiskLimits::new());
    e.client.refuses_triggers = true;
    let mut outbox = EventBusOutbox::default();
    e.modify_order_with_trigger("c1", None, None, Some(91.0), 5, &mut outbox);
    assert_eq!(rejections(&outbox), vec![format!("deribit: {TRIGGER_AMEND_UNSUPPORTED_REASON}")]);
    assert!(e.client.modifies.is_empty() && e.client.trigger_modifies.is_empty());

    let mut e = resting(order("deribit", Limit, "l1"), RiskLimits::new());
    let mut outbox = EventBusOutbox::default();
    e.modify_order_with_trigger("l1", None, Some(101.0), Some(91.0), 5, &mut outbox);
    assert_eq!(rejections(&outbox), vec![format!("deribit: {TRIGGER_AMEND_NO_TRIGGER_REASON}")]);
    assert!(e.client.modifies.is_empty(), "the price is not moved alone either");
    let mut outbox = EventBusOutbox::default();
    assert!(e.plan_replacement("l1", None, None, Some(91.0), 5, &mut outbox).is_none());
    assert_eq!(rejections(&outbox), vec![format!("deribit: {TRIGGER_AMEND_NO_TRIGGER_REASON}")]);

    let mut e = resting(order("deribit", StopLimit, "c1"), RiskLimits::new());
    e.registry.get_mut("c1").unwrap().status = OrderStatus::Triggered;
    let mut outbox = EventBusOutbox::default();
    e.modify_order_with_trigger("c1", None, None, Some(91.0), 5, &mut outbox);
    assert_eq!(rejections(&outbox), vec![format!("deribit: {TRIGGER_AMEND_NO_TRIGGER_REASON}")]);
    let mut outbox = EventBusOutbox::default();
    assert!(e.plan_replacement("c1", None, None, Some(91.0), 5, &mut outbox).is_none());
    assert_eq!(rejections(&outbox).len(), 1, "a spent trigger is not re-armed by a replacement");
    assert!(e.client.trigger_modifies.is_empty() && e.client.submissions.len() == 1);
}

/// The cancel-then-new half: the replacement of an unfired stop-limit carries the new trigger and the
/// new limit, judged by the gate with the trigger folded in.
#[test]
fn a_planned_replacement_carries_the_new_trigger() {
    let mut e = resting(order("binance", OrderType::StopLimit, "c1"), RiskLimits::new());
    let mut outbox = EventBusOutbox::default();
    let plan = e.plan_replacement("c1", None, Some(89.0), Some(91.0), 5, &mut outbox);
    let plan = plan.unwrap_or_else(|| panic!("{:?}", rejections(&outbox)));
    assert_eq!(
        (plan.order_type, plan.price, plan.trigger_price),
        (OrderType::StopLimit, Some(89.0), Some(91.0))
    );
    let plan = e.plan_replacement("c1", None, None, Some(92.0), 5, &mut outbox).expect("planned");
    assert_eq!((plan.price, plan.trigger_price), (Some(100.0), Some(92.0)), "the limit is kept");
    assert!(e.client.trigger_modifies.is_empty() && e.client.submissions.len() == 1);
}

/// A `new_price` on a market-if-touched (a take-profit resting with no limit) is refused in words before
/// anything is sent or planned: landed on its limit it would turn it into a limit-if-touched, and on a
/// cancel-then-new venue the replacement could be refused AFTER the original was cancelled. Its trigger
/// still moves.
#[test]
fn a_price_on_a_market_if_touched_is_refused_before_anything_is_sent() {
    use vike_execution::execution_engine::MIT_PRICE_AMEND_REASON;
    let mit = order("deribit", OrderType::TakeProfit, "m1");
    assert!(mit.price.is_none() && mit.trigger_price.is_some(), "precondition: an MIT");
    let mut e = resting(mit, RiskLimits::new());
    let mut outbox = EventBusOutbox::default();
    e.modify_order("m1", None, Some(95.0), 5, &mut outbox);
    assert_eq!(rejections(&outbox), vec![format!("deribit: {MIT_PRICE_AMEND_REASON}")]);
    let mut outbox = EventBusOutbox::default();
    assert!(e.plan_replacement("m1", None, Some(95.0), None, 5, &mut outbox).is_none());
    assert_eq!(rejections(&outbox), vec![format!("deribit: {MIT_PRICE_AMEND_REASON}")]);
    assert!(e.client.modifies.is_empty() && e.client.trigger_modifies.is_empty());
    assert_eq!(e.client.submissions.len(), 1, "nothing was placed");

    let mut outbox = EventBusOutbox::default();
    let plan = e.plan_replacement("m1", None, None, Some(95.0), 5, &mut outbox);
    let plan = plan.unwrap_or_else(|| panic!("{:?}", rejections(&outbox)));
    assert_eq!(
        (plan.order_type, plan.price, plan.trigger_price),
        (OrderType::TakeProfit, None, Some(95.0)),
        "its trigger moves and it stays a market-if-touched"
    );

    let lit = OrderRequest { price: Some(100.0), ..order("deribit", OrderType::TakeProfit, "t1") };
    let mut e = resting(lit, RiskLimits::new());
    let mut outbox = EventBusOutbox::default();
    e.modify_order("t1", None, Some(99.0), 5, &mut outbox);
    assert!(rejections(&outbox).is_empty(), "a limit-if-touched moves its limit");
}

/// The mode of a TRIGGER move is the venue table's trigger answer: an adapter that amends the kind in
/// place but whose trigger move is unproven refuses it; a cancel-then-new kind stays one; a limit has no
/// trigger; a client that declares its own convention moves every trigger in place.
#[test]
fn the_trigger_mode_is_the_venue_tables_trigger_answer() {
    let req = |venue, t| order(venue, t, "x");
    // deribit amends a stop-limit in place but overrides no trigger move (bybit's is proven since 2026-11-08)
    let e = resting(order("deribit", OrderType::Limit, "l1"), RiskLimits::new());
    assert_eq!(e.amend_mode_of(&req("deribit", OrderType::StopLimit)), Some(AmendMode::Native));
    assert_eq!(
        e.trigger_amend_mode_of(&req("deribit", OrderType::StopLimit)),
        Some(AmendMode::Refused)
    );
    assert_eq!(
        e.trigger_amend_mode_of(&req("deribit", OrderType::Limit)),
        Some(AmendMode::Refused)
    );
    assert_eq!(e.trigger_amend_mode_of(&req("deribit", OrderType::Market)), None);
    let b = resting(order("binance", OrderType::Limit, "l1"), RiskLimits::new());
    assert_eq!(
        b.trigger_amend_mode_of(&req("binance", OrderType::StopLimit)),
        Some(AmendMode::CancelThenNew)
    );
    let mut p = resting(order("deribit", OrderType::Limit, "l1"), RiskLimits::new());
    p.client.declared_amend_semantics = Some(AmendSemantics::InPlaceRemaining);
    assert_eq!(
        p.trigger_amend_mode_of(&req("deribit", OrderType::StopLimit)),
        Some(AmendMode::Native)
    );
}
