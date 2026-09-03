//! Integration coverage for how OANDA wires the REAL pure mapping functions
//! (`build_order_body`/`map_order_response`) through the shared `vike_bridge_core::exec_actor`
//! plumbing — the same seam `OandaExecutionClient::spawn` uses, minus the network hop.
//!
//! `OandaExecutionClient::spawn` hardcodes a real `OandaRest`/ureq transport inside its command
//! loop (`exec::run`), so it cannot be driven offline. Per the fake-client pattern in
//! `vike-bridge-core/tests/exec_actor_dead_thread.rs`, this test spawns an `ExecActor` directly
//! with a fake command loop that mirrors `exec::run`'s shape (emit `OrderSubmitted`, build the
//! real request body, then map a CANNED response through the real `map_order_response`) so the
//! wiring — event ordering, dual-publish, cancel outcomes, clean teardown — is proven against the
//! production mapping code without a live REST double.

use std::sync::mpsc::Receiver;
use std::time::Duration;

use vike_bridge_core::exec_actor::{
    cancel_batch_undeclared, cancel_event, CancelOutcome, ExecActor, ExecCommand,
};
use vike_exec::{event_channel, EventSender, ExecutionClient, Ingest};
use vike_model::events::{Event, OrderSubmitted};
use vike_model::{OrderRequest, TimeInForce};
use vike_oanda::{build_order_body, map_order_response};

/// A fake `run()` shaped exactly like `oanda::exec::run`: emit `OrderSubmitted`, build the real
/// order body (exercised for its own sake — a malformed request would panic/produce garbage
/// here exactly as it would in production), then map a canned "REST response" (keyed off the
/// request so different tests can script different outcomes) through the real
/// `map_order_response`. Cancel maps a canned outcome through the real `cancel_event`.
fn fake_run(events: EventSender, rx: Receiver<ExecCommand>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(req) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
                let body = build_order_body(&req);
                let resp = canned_post_response(&req, &body);
                for ev in map_order_response(&req.client_order_id, req.ts, &resp) {
                    let _ = events.blocking_send(ev);
                }
            }
            ExecCommand::Cancel { client_order_id: coid, .. } => {
                let outcome = canned_cancel_outcome(&coid);
                let _ = events.blocking_send(cancel_event(&coid, outcome));
            }
            // Shaped like `exec::run`'s own arm: OANDA declares no bulk lane, so `ExecActor` fans
            // a batch out into the per-id `Cancel`s above and this is unreachable.
            ExecCommand::CancelBatch { client_order_ids, .. } => {
                cancel_batch_undeclared(&events, &client_order_ids)
            }
            // no native amend on this venue: a modify leaves the resting order at its terms
            ExecCommand::Modify { .. } => {}
            ExecCommand::Shutdown => break,
        }
    }
}

/// Script: a MARKET order (units != 0 with FOK/IOC in the built body) fills inline; anything else
/// (our LIMIT test order) is accepted and rests.
fn canned_post_response(req: &OrderRequest, body: &serde_json::Value) -> serde_json::Value {
    if req.client_order_id == "reject-me" {
        return serde_json::json!({
            "orderRejectTransaction": {"id": "1", "rejectReason": "INSUFFICIENT_MARGIN"}
        });
    }
    if body["order"]["type"] == "MARKET" {
        serde_json::json!({
            "orderCreateTransaction": {"id": "9001", "type": "MARKET_ORDER"},
            "orderFillTransaction": {"id": "9002", "time": "1478012400.000000000",
                "instrument": body["order"]["instrument"], "units": body["order"]["units"],
                "price": "1.09000", "commission": "0.01"},
            "lastTransactionID": "9002"
        })
    } else {
        serde_json::json!({"orderCreateTransaction": {"id": "9003", "type": "LIMIT_ORDER"}})
    }
}

/// Script: cancel "resting-1" succeeds; any other coid fails (no `orderCancelTransaction`).
fn canned_cancel_outcome(coid: &str) -> CancelOutcome {
    if coid == "resting-1" {
        CancelOutcome::Canceled
    } else {
        CancelOutcome::Rejected(format!("no orderCancelTransaction for {coid}"))
    }
}

fn order(coid: &str, order_type: &str, side: i32) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "oanda".into(),
        symbol: "EURUSD".into(),
        side,
        qty: 1000.0,
        order_type: order_type.into(),
        price: Some(1.09),
        time_in_force: TimeInForce::Gtc,
        ts: 42,
        ..Default::default()
    }
}

/// Drain the ingest channel with a bounded wait, collecting exactly `n` events (or panicking on
/// timeout) — mirrors `exec_actor_dead_thread.rs`'s `recv_event` helper.
fn recv_n_events(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, n: usize) -> Vec<Event> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let ingest = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("timed out waiting for event")
                .expect("ingest channel closed");
            match ingest {
                Ingest::Event(ev) => out.push(ev),
                other => panic!("expected Ingest::Event, got {other:?}"),
            }
        }
        out
    })
}

#[test]
fn submit_market_order_wires_through_submitted_accepted_fill_and_wrap() {
    let (events, mut rx) = event_channel(16);
    let mut client =
        ExecActor::spawn("oanda-exec-fake", events.clone(), move |rx| fake_run(events, rx));

    client.submit(&order("c-market", "market", 1));

    let evs = recv_n_events(&mut rx, 4);
    assert!(matches!(&evs[0], Event::OrderSubmitted(s) if s.client_order_id == "c-market"));
    assert!(
        matches!(&evs[1], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("9001"))
    );
    match &evs[2] {
        Event::Fill(f) => {
            assert_eq!(f.trade_id, "9002");
            assert_eq!(f.last_qty, 1000.0);
            assert_eq!(f.side, 1);
        }
        other => panic!("expected bare Fill, got {other:?}"),
    }
    assert!(matches!(&evs[3], Event::OrderFilled(w) if w.fill.trade_id == "9002"));
}

#[test]
fn submit_limit_order_wires_through_submitted_then_accepted_only() {
    let (events, mut rx) = event_channel(16);
    let mut client =
        ExecActor::spawn("oanda-exec-fake", events.clone(), move |rx| fake_run(events, rx));

    client.submit(&order("c-limit", "limit", 1));

    let evs = recv_n_events(&mut rx, 2);
    assert!(matches!(&evs[0], Event::OrderSubmitted(s) if s.client_order_id == "c-limit"));
    assert!(
        matches!(&evs[1], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("9003"))
    );
}

#[test]
fn submit_rejected_order_wires_through_submitted_then_rejected() {
    let (events, mut rx) = event_channel(16);
    let mut client =
        ExecActor::spawn("oanda-exec-fake", events.clone(), move |rx| fake_run(events, rx));

    client.submit(&order("reject-me", "market", 1));

    let evs = recv_n_events(&mut rx, 2);
    assert!(matches!(&evs[0], Event::OrderSubmitted(_)));
    assert!(matches!(&evs[1], Event::OrderRejected(r) if r.reason == "INSUFFICIENT_MARGIN"));
}

#[test]
fn cancel_confirmed_and_cancel_failed_wire_through_the_real_cancel_event_mapping() {
    let (events, mut rx) = event_channel(16);
    let mut client =
        ExecActor::spawn("oanda-exec-fake", events.clone(), move |rx| fake_run(events, rx));

    client.cancel("resting-1");
    match &recv_n_events(&mut rx, 1)[0] {
        Event::OrderCanceled(c) => assert_eq!(c.client_order_id, "resting-1"),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }

    client.cancel("unknown-order");
    match &recv_n_events(&mut rx, 1)[0] {
        Event::OrderCancelRejected(c) => {
            assert_eq!(c.client_order_id, "unknown-order");
            assert!(c.reason.contains("unknown-order"));
        }
        other => panic!("expected OrderCancelRejected, got {other:?}"),
    }
}

#[test]
fn dropping_the_client_stops_the_command_thread_deterministically() {
    // ExecActor's Drop sends Shutdown and joins the thread; this must return promptly (not hang)
    // and the fake loop must observe Shutdown and break cleanly. If this test hangs, teardown
    // wiring for OANDA's exec thread is broken.
    let (events, _rx) = event_channel(16);
    let client =
        ExecActor::spawn("oanda-exec-fake", events.clone(), move |rx| fake_run(events, rx));
    drop(client); // must return; a `cargo test` timeout would fail this otherwise
}
