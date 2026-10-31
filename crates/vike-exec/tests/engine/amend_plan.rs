//! What the engine says about HOW a resting order is amended (`ExecutionEngine::amend_mode_of`) and the
//! order a cancel-then-new amend would place (`ExecutionEngine::plan_replacement`), judged by the same
//! RiskGate step as a native amend. The carrier that acts on them is `vike-core`'s `apply/amend.rs`
//! (its tests drive the whole machine); this suite pins the engine's half: the mode, the terms of the
//! replacement, and that planning SENDS nothing.

use vike_exec::testing::RecordingClient;
use vike_exec::{EventBus, ExecutionEngine, Outbox};
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::{OrderRequest, OrderStatus, OrderType, RiskLimits};
use vike_venues::AmendSemantics;
use vike_venues::venue_amend_mode::AmendMode;

use crate::support::EngineBuilder;

/// A resting order of `order_type`: a trigger kind is a reduce-only sell with its trigger (a stop-limit
/// with its limit price as well), anything else a buy at 100.
fn order(venue: &str, order_type: OrderType, coid: &str) -> OrderRequest {
    let trigger = order_type.is_trigger();
    let priced = matches!(order_type, OrderType::Limit | OrderType::StopLimit);
    OrderRequest {
        client_order_id: coid.into(),
        venue: venue.into(),
        symbol: "BTCUSDT".into(),
        side: if trigger { -1 } else { 1 },
        qty: 10.0,
        order_type,
        price: priced.then_some(100.0),
        trigger_price: trigger.then_some(90.0),
        reduce_only: trigger,
        ts: 1,
        ..Default::default()
    }
}

/// An engine on `venue` with `req` resting ACCEPTED.
fn resting(req: OrderRequest, limits: RiskLimits) -> ExecutionEngine<RecordingClient> {
    let venue = req.venue.clone();
    let mut e = EngineBuilder {
        venue: venue.clone(),
        symbol: "BTCUSDT".into(),
        limits,
        ..Default::default()
    }
    .build();
    let coid = req.client_order_id.clone();
    e.submit_order(&req, 0, &mut Outbox::default());
    assert_eq!(e.client.submissions.len(), 1, "precondition: the order was admitted");
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.clone(), ts: 0 }),
        &mut e,
    );
    bus.publish(
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: Some("v1".into()),
            ts: 1,
        }),
        &mut e,
    );
    e
}

fn rejections(outbox: &Outbox) -> Vec<String> {
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
    let mut outbox = Outbox::default();
    let plan = e.plan_replacement("s1", None, Some(91.0), 5, &mut outbox).expect("planned");
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
    let mut outbox = Outbox::default();
    let plan = e.plan_replacement("l1", None, Some(101.0), 5, &mut outbox).expect("planned");
    assert_eq!(plan.qty, 6.0, "10 ordered, 4 executed");
    assert_eq!(plan.price, Some(101.0));
    let plan = e.plan_replacement("l1", Some(8.0), None, 5, &mut outbox).expect("planned");
    assert_eq!(plan.qty, 8.0, "an explicit size is the fresh order's size");
    assert_eq!(plan.price, Some(100.0), "the resting price is kept");
}

/// Nothing left to rest, or an order that does not rest: a non-terminal refusal in words, no plan.
#[test]
fn an_order_with_nothing_to_rest_is_refused_in_words() {
    let mut e = resting(order("polymarket", OrderType::Limit, "l1"), RiskLimits::new());
    e.registry.get_mut("l1").unwrap().filled_qty = 10.0;
    let mut outbox = Outbox::default();
    assert!(e.plan_replacement("l1", None, Some(101.0), 5, &mut outbox).is_none());
    assert!(rejections(&outbox)[0].contains("nothing of the order is left unfilled"));

    let mut outbox = Outbox::default();
    assert!(e.plan_replacement("ghost", None, Some(101.0), 5, &mut outbox).is_none());
    assert!(rejections(&outbox)[0].contains("not resting"));
}

/// The RiskGate judges the replacement like any modify: a veto is a non-terminal refusal and no plan.
#[test]
fn the_risk_gate_judges_the_replacement() {
    let limits = RiskLimits { max_notional_per_order: Some(2_000.0), ..RiskLimits::new() };
    let mut e = resting(order("polymarket", OrderType::Limit, "l1"), limits);
    let mut outbox = Outbox::default();
    assert!(e.plan_replacement("l1", Some(1_000.0), None, 5, &mut outbox).is_none());
    let said = rejections(&outbox);
    assert_eq!(said.len(), 1);
    assert!(said[0].starts_with("risk:"), "{said:?}");
}

/// CONFORMANCE, every roster venue and every resting kind: the engine does what the venue's amend row says.
/// A native row forwards the modify to the client exactly once; a cancel-then-new row plans a replacement
/// (a reduce-only trigger order NEW-FIRST, anything else cancel-first) and sends nothing; a refused row is
/// skipped here (the engine's own refusal is `modify_unsupported_venue.rs`'s). The roster is DERIVED from
/// `vike_model::VENUES`, so a new venue is covered on arrival.
#[test]
fn every_roster_venue_does_what_its_amend_row_says() {
    use vike_exec::amend::Placement;
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
            };
            let req = order(venue, order_type, "c1");
            let mut e = resting(req.clone(), RiskLimits::new());
            let at = format!("{venue} / {}", kind.word());
            assert_eq!(e.amend_mode_of(&req), Some(mode), "{at}");
            let mut outbox = Outbox::default();
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
                    let plan = e.plan_replacement("c1", None, Some(95.0), 5, &mut outbox);
                    assert!(plan.is_some(), "{at}: {:?}", rejections(&outbox));
                    assert!(e.client.modifies.is_empty() && e.client.cancels.is_empty(), "{at}");
                    assert_eq!(e.client.submissions.len(), 1, "{at}: planning places nothing");
                    let want = if kind == AmendKind::Limit {
                        Placement::CancelFirst
                    } else {
                        Placement::NewFirst
                    };
                    assert_eq!(Placement::of(&req), want, "{at}");
                }
                AmendMode::Refused => unreachable!("skipped above"),
            }
            checked += 1;
        }
    }
    assert!(checked >= 20, "the roster x kinds matrix covered only {checked} cells");
}
