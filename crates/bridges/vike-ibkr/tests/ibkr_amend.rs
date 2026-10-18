//! Order modify on the socket backend, over the `FakeTransport` — no Gateway, no ibapi.
//!
//! TWS has no amend verb: the bridge re-places the order under its own id and believes the amend only
//! when the venue's `openOrder` echo carries the new terms (`exec::amend`). The fake's `on_modify_order`
//! hook is that echo. Before this, `modify` through the control door was accepted and did nothing: the
//! socket transport inherited a no-op and no event ever came back.
//!
//! Feature-gated on `test-support` like `ibkr_lifecycle.rs`.
#![cfg(feature = "test-support")]

use std::assert_matches;
use std::time::{Duration, Instant};

use vike_exec::ExecutionClient;
use vike_exec::{Ingest, event_channel};
use vike_ibkr::run_exec_for_test;
use vike_ibkr::testing::{AmendOutcome, FakeTransport, ScriptedInbound};
use vike_model::OrderRequest;
use vike_model::events::Event;

/// Block-receive up to `n` events within `timeout` off the ingest lane (non-`Event` ingest is ignored).
fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, n: usize, timeout: Duration) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < n && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(ev)) => out.push(ev),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    out
}

/// A fake that accepts the order `coid-1` as venue id 101, with the identity that makes the venue's rows "ours".
fn fake_with_a_resting_order() -> FakeTransport {
    let mut fake = FakeTransport::new().with_socket_identity();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(|order_id| {
        vec![ScriptedInbound::OrderStatus {
            order_id,
            order_ref: "coid-1".into(),
            status: "Submitted".into(),
        }]
    });
    fake
}

fn resting_limit() -> OrderRequest {
    OrderRequest {
        client_order_id: "coid-1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 5.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(190.0),
        ..Default::default()
    }
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
            order_ref: "coid-1".into(),
            total_qty: 7.0,
            limit_price: Some(191.0),
            aux_price: None,
        }]
    });
    let specs = fake.modify_specs();
    let calls = fake.modify_calls();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    let order = resting_limit();
    handle.submit(&order);
    handle.modify(&order, Some(7.0), Some(191.0));

    let got = drain(&mut rx_events, 3, Duration::from_secs(3));
    assert_eq!(got.len(), 3, "{got:?}");
    assert_matches!(got[0], Event::OrderSubmitted(_), "{got:?}");
    assert_matches!(got[1], Event::OrderAccepted(_), "{got:?}");
    match &got[2] {
        Event::OrderModified(m) => {
            assert_eq!(m.client_order_id, "coid-1");
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
    assert_eq!(spec.order_ref, "coid-1", "the amend keeps the coid tag");
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
            order_ref: "coid-1".into(),
            total_qty: 5.0,
            limit_price: None,
            aux_price: Some(182.0),
        }]
    });
    let specs = fake.modify_specs();
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    let stop = OrderRequest {
        order_type: vike_model::OrderType::Stop,
        price: None,
        trigger_price: Some(180.0),
        ..resting_limit()
    };
    handle.submit(&stop);
    handle.modify(&stop, None, Some(182.0));

    let got = drain(&mut rx_events, 3, Duration::from_secs(3));
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

/// A backend that refuses (the write failed, or it cannot amend) says so with a non-terminal
/// `OrderModifyRejected` carrying its words; nothing is left waiting for an echo.
#[test]
fn a_refused_amend_is_an_explicit_modify_rejection() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.modify_outcome(AmendOutcome::Refused("socket write failed".into()));
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    let order = resting_limit();
    handle.submit(&order);
    handle.modify(&order, None, Some(191.0));

    let got = drain(&mut rx_events, 3, Duration::from_secs(3));
    match got.get(2) {
        Some(Event::OrderModifyRejected(r)) => {
            assert_eq!(r.client_order_id, "coid-1");
            assert!(r.reason.contains("socket write failed"), "{}", r.reason);
        }
        other => panic!("expected OrderModifyRejected, got {other:?} in {got:?}"),
    }
    handle.detach();
}

/// A backend that vouches for the amend synchronously (the cpapi REST reply) is `OrderModified` at once,
/// with the requested terms.
#[test]
fn an_acknowledged_amend_is_order_modified_at_once() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let mut fake = fake_with_a_resting_order();
    fake.modify_outcome(AmendOutcome::Acknowledged);
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    let order = resting_limit();
    handle.submit(&order);
    handle.modify(&order, Some(6.0), Some(189.5));

    let got = drain(&mut rx_events, 3, Duration::from_secs(3));
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
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    let order = resting_limit();
    handle.submit(&order);
    handle.modify(&order, None, Some(191.003));

    let got = drain(&mut rx_events, 4, Duration::from_secs(2));
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
    let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
    let order = resting_limit();
    handle.submit(&order);
    handle.modify(&order, None, Some(191.0));
    handle.modify(&order, None, Some(192.0));

    let got = drain(&mut rx_events, 3, Duration::from_secs(3));
    match got.get(2) {
        Some(Event::OrderModifyRejected(r)) => {
            assert_eq!(r.client_order_id, "coid-1");
            assert!(r.reason.contains("NOT sent"), "{}", r.reason);
        }
        other => panic!("expected OrderModifyRejected, got {other:?} in {got:?}"),
    }
    assert_eq!(calls.lock().unwrap().len(), 1, "only the first amend reached the wire");
    handle.detach();
}

/// An order the platform does not hold the terms of (a stop-limit, or a limit without its price as an order
/// restored from a venue report is) is refused in words and nothing is re-placed.
#[test]
fn an_order_whose_terms_are_not_known_is_refused_and_nothing_is_written() {
    vike_log::test_init();
    for order in [
        OrderRequest { order_type: vike_model::OrderType::StopLimit, ..resting_limit() },
        OrderRequest { price: None, ..resting_limit() },
        OrderRequest {
            order_type: vike_model::OrderType::Stop,
            trigger_price: None,
            ..resting_limit()
        },
    ] {
        let (events, mut rx_events) = event_channel(64);
        let fake = fake_with_a_resting_order();
        let calls = fake.modify_calls();
        let mut handle = run_exec_for_test(events, Box::new(fake.clone()));
        handle.submit(&order);
        handle.modify(&order, Some(6.0), Some(191.0));

        let got = drain(&mut rx_events, 3, Duration::from_secs(3));
        match got.get(2) {
            Some(Event::OrderModifyRejected(r)) => {
                assert!(r.reason.contains("cannot be amended in place"), "{}", r.reason)
            }
            other => panic!("expected OrderModifyRejected, got {other:?} in {got:?}"),
        }
        assert!(
            calls.lock().unwrap().is_empty(),
            "nothing was re-placed for {:?}",
            order.order_type
        );
        handle.detach();
    }
}
