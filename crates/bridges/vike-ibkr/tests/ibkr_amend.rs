//! Order modify on the socket backend, over the `IbkrTransportFake` — no Gateway, no ibapi.
//!
//! TWS has no amend verb: the bridge re-places the order under its own id and believes the amend only
//! when the venue's `openOrder` echo carries the new terms (`exec::amend`). The fake's `on_modify_order`
//! hook is that echo. Before this, `modify` through the control door was accepted and did nothing: the
//! socket transport inherited a no-op and no event ever came back.
//!
//! Feature-gated on `test-support` like `ibkr_lifecycle.rs`.
#![cfg(feature = "test-support")]

use std::assert_matches;
use std::time::Duration;

use vike_bridge_core::execution_drive::drain;

use vike_execution::ExecutionClient;
use vike_execution::event_channel;
use vike_ibkr::run_execution_for_test;
use vike_ibkr::testing::{AmendOutcome, IbkrTransportFake, ScriptedInbound};
use vike_model::OrderRequest;
use vike_model::events::Event;

/// A fake that accepts the order `order-client-id-1` as venue id 101, with the identity that makes the venue's rows "ours".
fn fake_with_a_resting_order() -> IbkrTransportFake {
    let mut fake = IbkrTransportFake::new().with_socket_identity();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(|order_id| {
        vec![ScriptedInbound::OrderStatus {
            order_id,
            order_ref: "order-client-id-1".into(),
            status: "Submitted".into(),
        }]
    });
    fake
}

fn resting_limit() -> OrderRequest {
    vike_bridge_core::execution_drive::limit("ibkr", "order-client-id-1", "AAPL", 5.0, 190.0)
}

/// THE FIX: a modify reaches the wire as the order re-placed under the SAME id with the new price and size
/// (and its orderRef), and the venue's echo of the new terms becomes `OrderModified`.
#[test]
fn modify_replaces_the_order_under_its_own_id_and_the_echo_confirms_it() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.on_modify_order(|order_id| {
        vec![ScriptedInbound::VenueOrderPriced {
            order_id,
            order_ref: "order-client-id-1".into(),
            total_qty: 7.0,
            limit_price: Some(191.0),
            aux_price: None,
        }]
    });
    let specs = fake.modify_specs();
    let calls = fake.modify_calls();
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let order = resting_limit();
    handle.submit(&order);
    handle.modify(&order, Some(7.0), Some(191.0));

    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    assert_eq!(got.len(), 3, "{got:?}");
    assert_matches!(got[0], Event::OrderSubmitted(_), "{got:?}");
    assert_matches!(got[1], Event::OrderAccepted(_), "{got:?}");
    match &got[2] {
        Event::OrderModified(m) => {
            assert_eq!(m.order_client_id, "order-client-id-1");
            assert_eq!(m.venue_order_id.as_deref(), Some("101"));
            assert_eq!(m.new_qty, Some(7.0));
            assert_eq!(m.new_price, Some(191.0));
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
    assert_eq!(*calls.lock().unwrap(), [101], "re-placed under the order's own id");
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(spec.order_type, "LMT");
    assert_eq!(spec.lmt_price, Some(191.0));
    assert_eq!(spec.total_qty, 7.0);
    assert_eq!(spec.order_ref, "order-client-id-1", "the amend keeps the order client id tag");
    handle.detach();
}

/// A STOP's price is its trigger: the amend must move `aux_price`, not set a limit price the order never had
/// (the old code set `price` for every kind, so a stop amend re-sent the OLD trigger).
#[test]
fn a_stop_amend_moves_the_trigger_price() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.on_modify_order(|order_id| {
        vec![ScriptedInbound::VenueOrderPriced {
            order_id,
            order_ref: "order-client-id-1".into(),
            total_qty: 5.0,
            limit_price: None,
            aux_price: Some(182.0),
        }]
    });
    let specs = fake.modify_specs();
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let stop = OrderRequest {
        order_type: vike_model::OrderType::Stop,
        price: None,
        trigger_price: Some(180.0),
        ..resting_limit()
    };
    handle.submit(&stop);
    handle.modify(&stop, None, Some(182.0));

    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    match got.get(2) {
        Some(Event::OrderModified(m)) => assert_eq!(m.new_price, Some(182.0)),
        other => panic!("expected OrderModified, got {other:?} in {got:?}"),
    }
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(spec.order_type, "STP");
    assert_eq!(spec.aux_price, Some(182.0), "the TRIGGER moved");
    assert_eq!(spec.lmt_price, None);
    handle.detach();
}

/// A STOP-LIMIT's amend price is its LIMIT (`vike_model::modified_price_is_trigger`): the re-placed `STP LMT` carries
/// the new limit and the trigger it had, and the echo is matched on the limit.
#[test]
fn a_stop_limit_amend_moves_the_limit_and_keeps_the_trigger() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.on_modify_order(|order_id| {
        vec![ScriptedInbound::VenueOrderPriced {
            order_id,
            order_ref: "order-client-id-1".into(),
            total_qty: 5.0,
            limit_price: Some(178.5),
            aux_price: Some(180.0),
        }]
    });
    let specs = fake.modify_specs();
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let stop_limit = OrderRequest {
        order_type: vike_model::OrderType::StopLimit,
        side: -1,
        price: Some(179.0),
        trigger_price: Some(180.0),
        ..resting_limit()
    };
    handle.submit(&stop_limit);
    handle.modify(&stop_limit, None, Some(178.5));

    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    match got.get(2) {
        Some(Event::OrderModified(m)) => assert_eq!(m.new_price, Some(178.5)),
        other => panic!("expected OrderModified, got {other:?} in {got:?}"),
    }
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(spec.order_type, "STP LMT");
    assert_eq!(spec.lmt_price, Some(178.5), "the LIMIT moved");
    assert_eq!(spec.aux_price, Some(180.0), "the trigger is re-sent as it was");
    handle.detach();
}

/// A TAKE-PROFIT is IB's if-touched order. A limit-if-touched (`LIT`) amend moves its LIMIT, like a stop-limit's, and
/// keeps its trigger; a market-if-touched (`MIT`) has no limit, so its size alone is amended (the echo, matched on no
/// limit, carries no price) and a new price is refused with nothing written.
#[test]
fn a_take_profit_amend_moves_the_limit_of_a_lit_and_only_the_size_of_a_mit() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.on_modify_order(|order_id| {
        vec![ScriptedInbound::VenueOrderPriced {
            order_id,
            order_ref: "order-client-id-1".into(),
            total_qty: 5.0,
            limit_price: Some(201.0),
            aux_price: Some(200.0),
        }]
    });
    let specs = fake.modify_specs();
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let lit = OrderRequest {
        order_type: vike_model::OrderType::TakeProfit,
        side: -1,
        price: Some(200.5),
        trigger_price: Some(200.0),
        ..resting_limit()
    };
    handle.submit(&lit);
    handle.modify(&lit, None, Some(201.0));
    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    match got.get(2) {
        Some(Event::OrderModified(m)) => assert_eq!(m.new_price, Some(201.0)),
        other => panic!("expected OrderModified, got {other:?} in {got:?}"),
    }
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(
        (spec.order_type, spec.lmt_price, spec.aux_price),
        ("LIT", Some(201.0), Some(200.0))
    );
    handle.detach();

    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.on_modify_order(|order_id| {
        vec![ScriptedInbound::VenueOrderPriced {
            order_id,
            order_ref: "order-client-id-1".into(),
            total_qty: 7.0,
            limit_price: None,
            aux_price: Some(200.0),
        }]
    });
    let (specs, calls) = (fake.modify_specs(), fake.modify_calls());
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let mit = OrderRequest { price: None, ..lit };
    handle.submit(&mit);
    handle.modify(&mit, None, Some(199.0));
    handle.modify(&mit, Some(7.0), None);
    let got = drain(|| rx_events.try_recv().ok(), 4, Duration::from_secs(3));
    match got.get(2) {
        Some(Event::OrderModifyRejected(r)) => {
            assert!(
                r.reason.contains("market-if-touched") && r.reason.contains("Nothing was sent"),
                "{}",
                r.reason
            )
        }
        other => panic!("expected the price amend's refusal, got {other:?} in {got:?}"),
    }
    match got.get(3) {
        Some(Event::OrderModified(m)) => assert_eq!((m.new_qty, m.new_price), (Some(7.0), None)),
        other => panic!("expected the size amend's OrderModified, got {other:?} in {got:?}"),
    }
    assert_eq!(calls.lock().unwrap().len(), 1, "only the size amend reached the wire");
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!(
        (spec.order_type, spec.lmt_price, spec.aux_price, spec.total_qty),
        ("MIT", None, Some(200.0), 7.0)
    );
    handle.detach();
}

/// Place `order` (order-client-id-1, venue id 101), amend its TRIGGER through `modify_trigger` with `(new_price, new_trigger)`,
/// and have the venue echo `(limit, aux)` at the same size. Returns the event after the acceptance and the
/// re-placed spec, if one was written.
fn trigger_amend(
    order: &OrderRequest,
    new_price: Option<f64>,
    new_trigger: f64,
    echo: (Option<f64>, Option<f64>),
) -> (Option<Event>, Option<vike_ibkr::execution::order::IbkrOrderSpec>) {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    let qty = order.qty;
    fake.on_modify_order(move |order_id| {
        vec![ScriptedInbound::VenueOrderPriced {
            order_id,
            order_ref: "order-client-id-1".into(),
            total_qty: qty,
            limit_price: echo.0,
            aux_price: echo.1,
        }]
    });
    let specs = fake.modify_specs();
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    handle.submit(order);
    assert_eq!(handle.modify_trigger(order, None, new_price, new_trigger), Ok(()), "queued");
    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    let spec = specs.lock().unwrap().first().cloned();
    handle.detach();
    (got.get(2).cloned(), spec)
}

/// **The trigger amend is native**: the re-placed order carries the new trigger as `auxPrice` beside the limit it keeps
/// or moves, and the venue's echo of the new trigger becomes `OrderModified` with `new_trigger_price`. A stop-limit's
/// trigger moves with its limit kept; a market-if-touched's trigger moves (it has no limit); a limit-if-touched moves
/// both. A trigger on a kind whose only price IS its trigger, or that has none, is refused with nothing written (the
/// engine sends a stop's trigger as `modify`'s price).
#[test]
fn a_trigger_amend_re_places_the_order_with_its_new_aux_price() {
    let stop_limit = OrderRequest {
        order_type: vike_model::OrderType::StopLimit,
        side: -1,
        price: Some(179.0),
        trigger_price: Some(180.0),
        ..resting_limit()
    };
    let (event, spec) = trigger_amend(&stop_limit, None, 181.0, (Some(179.0), Some(181.0)));
    match event {
        Some(Event::OrderModified(m)) => {
            assert_eq!((m.new_price, m.new_trigger_price), (Some(179.0), Some(181.0)))
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
    let spec = spec.expect("re-placed");
    assert_eq!(
        (spec.order_type, spec.lmt_price, spec.aux_price),
        ("STP LMT", Some(179.0), Some(181.0))
    );

    let mit = OrderRequest {
        order_type: vike_model::OrderType::TakeProfit,
        price: None,
        trigger_price: Some(185.0),
        ..resting_limit()
    };
    let (event, spec) = trigger_amend(&mit, None, 184.0, (None, Some(184.0)));
    assert!(
        matches!(&event, Some(Event::OrderModified(m)) if m.new_trigger_price == Some(184.0) && m.new_price.is_none()),
        "{event:?}"
    );
    let spec = spec.expect("re-placed");
    assert_eq!((spec.order_type, spec.lmt_price, spec.aux_price), ("MIT", None, Some(184.0)));

    let lit = OrderRequest { price: Some(185.5), ..mit.clone() };
    let (event, spec) = trigger_amend(&lit, Some(184.5), 184.0, (Some(184.5), Some(184.0)));
    assert!(
        matches!(&event, Some(Event::OrderModified(m)) if (m.new_price, m.new_trigger_price) == (Some(184.5), Some(184.0))),
        "{event:?}"
    );
    let spec = spec.expect("re-placed");
    assert_eq!(
        (spec.order_type, spec.lmt_price, spec.aux_price),
        ("LIT", Some(184.5), Some(184.0))
    );

    for (order, kind) in [
        (resting_limit(), "limit"),
        (
            OrderRequest {
                order_type: vike_model::OrderType::Stop,
                price: None,
                trigger_price: Some(180.0),
                ..resting_limit()
            },
            "stop",
        ),
    ] {
        let (event, spec) = trigger_amend(&order, None, 181.0, (None, None));
        match event {
            Some(Event::OrderModifyRejected(r)) => {
                assert_eq!(
                    r.reason.to_string(),
                    format!(
                        "ibkr: a {kind} order has no trigger to amend apart from its price; nothing was sent"
                    )
                );
                assert_eq!(
                    r.refusal_id.as_deref(),
                    Some(vike_model::OrderRefusalId::VenueCapability.word())
                );
            }
            other => panic!("expected the refusal for {kind}, got {other:?}"),
        }
        assert!(spec.is_none(), "nothing was re-placed for {kind}");
    }
}

/// A backend that refuses (the write failed, or it cannot amend) says so with a non-terminal
/// `OrderModifyRejected` carrying its words; nothing is left waiting for an echo.
#[test]
fn a_refused_amend_is_an_explicit_modify_rejection() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.modify_outcome(AmendOutcome::Refused(
        "socket write failed".into(),
        vike_model::OrderRefusalId::NotSent,
    ));
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let order = resting_limit();
    handle.submit(&order);
    handle.modify(&order, None, Some(191.0));

    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    match got.get(2) {
        Some(Event::OrderModifyRejected(r)) => {
            assert_eq!(r.order_client_id, "order-client-id-1");
            assert!(r.reason.contains("socket write failed"), "{}", r.reason);
            assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
        }
        other => panic!("expected OrderModifyRejected, got {other:?} in {got:?}"),
    }
    handle.detach();
}

/// **An amend whose write failed is the shared answer's** (`vike_bridge_core::rest::modify_rejected`, decision 0303):
/// one of OUTCOME UNKNOWN (the cpapi amend whose reply was lost and whose read back did not show the asked terms, IB-4)
/// is a non-terminal `OrderModifyRejected` carrying `order.outcome_unknown`, never the venue's refusal; one never sent
/// carries `order.not_sent`.
#[test]
fn a_failed_amend_is_answered_by_its_outcome_unknown_or_not_sent() {
    use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
    use vike_model::OrderRefusalId;
    vike_log::test_init();
    for (code, id) in
        [(E_TIMEOUT_AMBIGUOUS, OrderRefusalId::OutcomeUnknown), (0, OrderRefusalId::NotSent)]
    {
        let (events, mut rx_events) = event_channel(64);
        let mut fake = fake_with_a_resting_order();
        let msg = "cpapi amend: no reply".to_string();
        fake.modify_outcome(AmendOutcome::Failed(VenueApiError { code, msg }));
        let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
        let order = resting_limit();
        handle.submit(&order);
        handle.modify(&order, None, Some(191.0));

        let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
        match got.get(2) {
            Some(Event::OrderModifyRejected(r)) => {
                assert_eq!(r.order_client_id, "order-client-id-1");
                assert_eq!(r.refusal_id.as_deref(), Some(id.word()), "{}", r.reason);
            }
            other => panic!("expected OrderModifyRejected, got {other:?} in {got:?}"),
        }
        handle.detach();
    }
}

/// A backend that vouches for the amend synchronously (the cpapi REST reply) is `OrderModified` at once,
/// with the requested terms.
#[test]
fn an_acknowledged_amend_is_order_modified_at_once() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.modify_outcome(AmendOutcome::Acknowledged);
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let order = resting_limit();
    handle.submit(&order);
    handle.modify(&order, Some(6.0), Some(189.5));

    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    match got.get(2) {
        Some(Event::OrderModified(m)) => {
            assert_eq!(m.new_qty, Some(6.0));
            assert_eq!(m.new_price, Some(189.5));
        }
        other => panic!("expected OrderModified, got {other:?} in {got:?}"),
    }
    handle.detach();
}

/// An order-rejection-class error naming the order while its amend is in flight refuses the AMEND. It must
/// not reach the mapper's terminal `OrderRejected`: the order still works at its old terms.
#[test]
fn a_rejection_during_an_amend_refuses_the_amend_and_leaves_the_order_alone() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.on_modify_order(|order_id| {
        vec![ScriptedInbound::Error {
            code: 201,
            order_id,
            msg: "Order rejected - price does not conform to the minimum price variation".into(),
        }]
    });
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let order = resting_limit();
    handle.submit(&order);
    handle.modify(&order, None, Some(191.003));

    let got = drain(|| rx_events.try_recv().ok(), 4, Duration::from_secs(2));
    assert_eq!(got.len(), 3, "submitted, accepted, and ONE advisory - no terminal: {got:?}");
    match &got[2] {
        Event::OrderModifyRejected(r) => assert!(r.reason.contains("minimum price variation")),
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
    handle.detach();
}

/// One amend in flight per order: a second, sent before TWS answered the first, is refused (non-terminal) and
/// NOT written - two open requests cannot be told apart by their echoes.
#[test]
fn a_second_amend_before_the_first_is_answered_is_refused_and_not_written() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let fake = fake_with_a_resting_order(); // no echo scripted: the first amend stays open
    let calls = fake.modify_calls();
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let order = resting_limit();
    handle.submit(&order);
    handle.modify(&order, None, Some(191.0));
    handle.modify(&order, None, Some(192.0));

    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    match got.get(2) {
        Some(Event::OrderModifyRejected(r)) => {
            assert_eq!(r.order_client_id, "order-client-id-1");
            assert!(r.reason.contains("NOT sent"), "{}", r.reason);
        }
        other => panic!("expected OrderModifyRejected, got {other:?} in {got:?}"),
    }
    assert_eq!(calls.lock().unwrap().len(), 1, "only the first amend reached the wire");
    handle.detach();
}

/// An order the platform does not hold the terms of (the engine's view of an order restored from a venue report: a
/// limit without its price, a stop or a stop-limit without its trigger) is refused in words and nothing is re-placed.
/// The order rests at the venue with its full terms; only the view the amend is built from lacks them. The words are
/// pinned whole: a limit's and a stop's are the ones operators have read since before a stop-limit could be amended,
/// byte for byte, and a stop-limit has its own.
#[test]
fn an_order_whose_terms_are_not_known_is_refused_and_nothing_is_written() {
    vike_log::test_init();
    const OLD_WORDS: &str = "ibkr: this order cannot be amended in place: only a limit or stop order whose price \
                             and time in force the platform holds can be (this one is {KIND}, restored from the \
                             venue without its price); amend it in TWS. Nothing was sent";
    let stop_limit = OrderRequest {
        order_type: vike_model::OrderType::StopLimit,
        trigger_price: Some(189.0),
        ..resting_limit()
    };
    let stop = OrderRequest {
        order_type: vike_model::OrderType::Stop,
        price: None,
        trigger_price: Some(189.0),
        ..resting_limit()
    };
    for (placed, view, words) in [
        (
            stop_limit.clone(),
            OrderRequest { trigger_price: None, ..stop_limit },
            "ibkr: this stop-limit order cannot be amended in place: an amend re-sends it whole, and the platform \
             does not hold both its limit and its trigger price (restored from the venue without them); amend it \
             in TWS. Nothing was sent"
                .to_string(),
        ),
        (
            resting_limit(),
            OrderRequest { price: None, ..resting_limit() },
            OLD_WORDS.replace("{KIND}", "Limit"),
        ),
        (stop.clone(), OrderRequest { trigger_price: None, ..stop }, OLD_WORDS.replace("{KIND}", "Stop")),
        (
            OrderRequest {
                order_type: vike_model::OrderType::TakeProfit,
                trigger_price: Some(189.0),
                ..resting_limit()
            },
            OrderRequest { order_type: vike_model::OrderType::TakeProfit, ..resting_limit() },
            "ibkr: this take-profit (if-touched) order cannot be amended in place: an amend re-sends it whole, and \
             the platform does not hold its trigger price, or its limit (restored from the venue without them); \
             amend it in TWS. Nothing was sent"
                .to_string(),
        ),
        (
            OrderRequest { order_type: vike_model::OrderType::MarketToLimit, price: None, ..resting_limit() },
            OrderRequest { order_type: vike_model::OrderType::MarketToLimit, price: None, ..resting_limit() },
            "ibkr: a market-to-limit order cannot be amended here: the limit its remainder rests at is the price \
             of its first fill, which the venue set and the platform does not hold; amend it in TWS. Nothing was \
             sent"
                .to_string(),
        ),
    ] {
        let (events, mut rx_events) = event_channel(64);
        let fake = fake_with_a_resting_order();
        let calls = fake.modify_calls();
        let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
        handle.submit(&placed);
        handle.modify(&view, Some(6.0), Some(191.0));

        let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
        match got.get(2) {
            Some(Event::OrderModifyRejected(r)) => {
                assert_eq!(r.reason.to_string(), words);
                // A kind amended here lacks only its terms; a market-to-limit is not amended here at all.
                let refusal = if view.order_type == vike_model::OrderType::MarketToLimit {
                    vike_model::OrderRefusalId::VenueCapability
                } else {
                    vike_model::OrderRefusalId::AmendRefused
                };
                assert_eq!(r.refusal_id.as_deref(), Some(refusal.word()));
            }
            other => panic!("expected OrderModifyRejected, got {other:?} in {got:?}"),
        }
        let order = view;
        assert!(
            calls.lock().unwrap().is_empty(),
            "nothing was re-placed for {:?}",
            order.order_type
        );
        handle.detach();
    }
}

/// **The re-place of an amend keeps the attributes** (an amend re-sends the order whole): an all-or-none, sweep-to-fill,
/// discretionary, HIDDEN limit amended in price goes out with every attribute it was placed with, and a time release IB
/// still holds (in the future) keeps its `goodAfterTime`.
#[test]
fn an_amend_re_places_the_order_with_its_attributes() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.on_modify_order(|order_id| {
        vec![ScriptedInbound::VenueOrderPriced {
            order_id,
            order_ref: "order-client-id-1".into(),
            total_qty: 5.0,
            limit_price: Some(191.0),
            aux_price: None,
        }]
    });
    let specs = fake.modify_specs();
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let attributed = OrderRequest {
        display_quantity: Some(0.0),
        attributes: Some(vike_model::OrderAttributes {
            all_or_none: true,
            sweep_to_fill: true,
            discretionary_amount: Some(0.5),
        }),
        // 2100-01-01T00:00:00Z: in the future whenever this runs
        release: Some(vike_model::OrderRelease { at: 4_102_444_800_000, price: None }),
        ..resting_limit()
    };
    handle.submit(&attributed);
    handle.modify(&attributed, None, Some(191.0));

    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    assert_matches!(got.get(2), Some(Event::OrderModified(_)), "{got:?}");
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!((spec.order_type, spec.lmt_price), ("LMT", Some(191.0)));
    assert_eq!(
        (spec.hidden, spec.all_or_none, spec.sweep_to_fill, spec.discretionary_amt),
        (true, true, true, Some(0.5))
    );
    assert_eq!(spec.good_after_time.as_deref(), Some("21000101-00:00:00"));
    handle.detach();
}

/// **A release whose time has passed is live at IB**: the amend's re-place goes WITHOUT its `goodAfterTime` (the
/// placed order carried it), and keeps every other term.
#[test]
fn an_amend_of_an_order_past_its_release_re_places_it_without_good_after_time() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.on_modify_order(|order_id| {
        vec![ScriptedInbound::VenueOrderPriced {
            order_id,
            order_ref: "order-client-id-1".into(),
            total_qty: 5.0,
            limit_price: Some(191.0),
            aux_price: None,
        }]
    });
    let placed = fake.place_specs();
    let specs = fake.modify_specs();
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let released = OrderRequest {
        // 2026-01-01T00:00:00Z: already past
        release: Some(vike_model::OrderRelease { at: 1_767_225_600_000, price: None }),
        ..resting_limit()
    };
    handle.submit(&released);
    handle.modify(&released, None, Some(191.0));

    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    assert_matches!(got.get(2), Some(Event::OrderModified(_)), "{got:?}");
    assert_eq!(placed.lock().unwrap()[0].good_after_time.as_deref(), Some("20260101-00:00:00"));
    let spec = specs.lock().unwrap()[0].clone();
    assert_eq!((spec.order_type, spec.lmt_price, spec.good_after_time), ("LMT", Some(191.0), None));
    handle.detach();
}

/// **An adjustable stop's amend is refused by name, nothing written**: the re-place would restate an adjustment IB may
/// already have made.
#[test]
fn an_adjustable_stop_is_never_amended() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let fake = fake_with_a_resting_order();
    let calls = fake.modify_calls();
    let mut handle = run_execution_for_test(events, Box::new(fake.clone()));
    let adjustable = OrderRequest {
        order_type: vike_model::OrderType::Stop,
        side: -1,
        price: None,
        trigger_price: Some(180.0),
        adjust: Some(vike_model::OrderStopAdjust {
            at: 200.0,
            to: vike_model::OrderStopAdjustTo::Stop { stop: 195.0 },
        }),
        ..resting_limit()
    };
    handle.submit(&adjustable);
    handle.modify(&adjustable, None, Some(182.0));

    let got = drain(|| rx_events.try_recv().ok(), 3, Duration::from_secs(3));
    match got.get(2) {
        Some(Event::OrderModifyRejected(r)) => {
            let why = r.reason.to_string();
            assert!(why.contains("adjustable stop") && why.contains("Nothing was sent"), "{why}");
            assert_eq!(
                r.refusal_id.as_deref(),
                Some(vike_model::OrderRefusalId::VenueCapability.word())
            );
        }
        other => panic!("expected OrderModifyRejected, got {other:?} in {got:?}"),
    }
    assert!(calls.lock().unwrap().is_empty(), "nothing was re-placed");
    handle.detach();
}
