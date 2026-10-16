//! The cTrader AMEND path end to end, against the in-process fake server: a resting limit order is placed,
//! `ExecutionClient::modify` writes a real `AMEND_ORDER_REQ`, and every way the venue can answer comes back as
//! exactly the event the engine needs.
//!
//! * `ORDER_REPLACED` -> `OrderModified` carrying the confirmed quantity and price (the proof the request
//!   reached the wire with the right volume in centi-units and the snapped price);
//! * an `ERROR_RES` echoing the request -> a NON-terminal `OrderModifyRejected`. It used to be a TERMINAL
//!   `OrderRejected` of an order that is still working: the verb is not in an `ERROR_RES`, and the request's
//!   `clientMsgId` was the bare coid, the same as a submit's;
//! * an `ORDER_ERROR_EVENT` naming the order -> `OrderModifyRejected`, where it used to read as a CANCEL
//!   rejection.
//!
//! The venue's own behaviour (does a real demo account replace a resting order the way the fake does) is a live
//! proof, not this file's.

mod common;

use std::sync::Arc;
use std::time::Duration;

use vike_ctrader::conn::{ConnConfig, connect_and_auth_exec};
use vike_exec::{ExecutionClient, event_channel};
use vike_model::OrderRequest;
use vike_model::events::Event;

use common::{AmendReply, FAKE_ORDER_ID, FakeCtrader, NoopSink, collect, drain};

fn resting_limit(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.10),
        ..Default::default()
    }
}

/// A started exec client with `coid` resting at the fake (submitted + accepted, the Submitted/Accepted pair read).
fn with_resting_order(
    reply: AmendReply,
    coid: &str,
) -> (
    FakeCtrader,
    vike_ctrader::exec::CtraderExec,
    tokio::sync::mpsc::Receiver<vike_exec::Ingest>,
    OrderRequest,
) {
    let server = FakeCtrader::start_resting_amends(reply);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());
    let order = resting_limit(coid);
    exec.submit(&order);
    let got = collect(&mut ingest, 2);
    assert!(matches!(got[0], Event::OrderSubmitted(_)), "{got:?}");
    assert!(matches!(got[1], Event::OrderAccepted(_)), "{got:?}");
    assert_eq!(exec.venue_order_id(coid), Some(FAKE_ORDER_ID), "the amend resolves off this map");
    (server, exec, ingest, order)
}

#[test]
fn a_modify_reaches_the_wire_and_the_replaced_event_becomes_order_modified() {
    let (server, mut exec, mut ingest, order) = with_resting_order(AmendReply::Replaced, "c-amend");

    // 2000 units -> volume 200_000 centi-units on the wire; the noisy price is snapped to the 5 digits.
    let noisy = f64::from_bits(1.1234_f64.to_bits() + 1);
    exec.modify(&order, Some(2000.0), Some(noisy));

    let got = collect(&mut ingest, 1);
    match got.as_slice() {
        [Event::OrderModified(m)] => {
            assert_eq!(m.client_order_id, "c-amend");
            assert_eq!(m.venue_order_id.as_deref(), Some(FAKE_ORDER_ID.to_string().as_str()));
            assert_eq!(
                m.new_qty,
                Some(2000.0),
                "volume 200_000 centi-units came back as 2000 units"
            );
            assert_eq!(m.new_price, Some(1.1234), "the wire price is the snapped decimal");
        }
        other => panic!("expected exactly one OrderModified, got {other:?}"),
    }
    server.assert_saw(&["NEW_ORDER_REQ", "AMEND_ORDER_REQ"]);
}

#[test]
fn a_venue_error_res_on_an_amend_refuses_the_amend_and_never_terminalizes_the_order() {
    let (server, mut exec, mut ingest, order) = with_resting_order(AmendReply::ErrorRes, "c-err");

    exec.modify(&order, None, Some(1.2));
    let got = collect(&mut ingest, 1);
    // give a (wrong) terminal reject the time it would need to arrive
    std::thread::sleep(Duration::from_millis(300));
    let late = drain!(ingest);
    match got.as_slice() {
        [Event::OrderModifyRejected(r)] => {
            assert_eq!(r.client_order_id, "c-err", "the coid, not the wire's amend-prefixed id");
            assert!(r.reason.contains("INVALID_REQUEST"), "{}", r.reason);
        }
        other => panic!("expected exactly one OrderModifyRejected, got {other:?}"),
    }
    assert!(
        !late.iter().any(|e| matches!(e, Event::OrderRejected(_) | Event::OrderCancelRejected(_))),
        "a refused amend must be neither a terminal reject nor a cancel reject: {late:?}"
    );
    server.assert_saw(&["NEW_ORDER_REQ", "AMEND_ORDER_REQ"]);
}

#[test]
fn an_order_error_event_on_an_amend_is_a_modify_rejection_not_a_cancel_rejection() {
    let (_server, mut exec, mut ingest, order) =
        with_resting_order(AmendReply::OrderError, "c-bad");

    exec.modify(&order, Some(2000.0), None);
    let got = collect(&mut ingest, 1);
    std::thread::sleep(Duration::from_millis(300));
    let late = drain!(ingest);
    match got.as_slice() {
        [Event::OrderModifyRejected(r)] => {
            assert_eq!(r.client_order_id, "c-bad");
            assert!(r.reason.contains("TRADING_BAD_VOLUME"), "{}", r.reason);
        }
        other => panic!("expected exactly one OrderModifyRejected, got {other:?}"),
    }
    assert!(
        !late.iter().any(|e| matches!(e, Event::OrderCancelRejected(_) | Event::OrderRejected(_))),
        "{late:?}"
    );
}
