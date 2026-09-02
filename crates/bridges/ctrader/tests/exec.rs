//! Scripted integration test for the exec path (Task 5): `connect_and_auth_exec` against the
//! in-process fake cTrader server, a `CtraderExec` submits a market order, the fake replies with
//! `EXECUTION_EVENT(ORDER_ACCEPTED)` then `EXECUTION_EVENT(ORDER_FILLED)` (see
//! `tests/common/mod.rs::new_order_exec_events`), and the actor thread decodes+routes them onto the
//! ingest lane. Asserts the ingest channel saw `OrderSubmitted, OrderAccepted, OrderFilled` in that
//! order — the emitter split end to end (Rust emits Submitted synchronously; the venue emits the
//! rest) — proving the whole actor-owns-the-EventSender wiring, not just the pure mapper (see
//! `tests/offline/exec_mapper.rs`).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::conn::{connect_and_auth_exec, ConnConfig};
use vike_exec::lanes::Ingest;
use vike_exec::{event_channel, ExecutionClient};
use vike_model::events::Event;
use vike_model::OrderRequest;

use common::{FakeCtrader, NoopSink, FAKE_FILL_PRICE, FAKE_ORDER_ID};

/// Drain everything currently on the ingest lane into a flat `Vec<Event>`. The receiver is a tokio
/// mpsc; `try_recv` is non-blocking. Inlined via a macro so the concrete receiver type never has to
/// be named (this crate has no direct tokio dep) — mirrors `bybit/src/exec.rs`'s test drain.
macro_rules! drain {
    ($rx:expr) => {{
        let mut out: Vec<Event> = Vec::new();
        while let Ok(ing) = $rx.try_recv() {
            if let Ingest::Event(e) = ing {
                out.push(e);
            }
        }
        out
    }};
}

fn market_buy(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: "market".into(),
        ..Default::default()
    }
}

#[test]
fn submit_market_order_yields_submitted_accepted_filled_in_order() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market_buy("c-1"));

    // Poll the ingest lane until all FOUR events land (venue events arrive asynchronously after the
    // NewOrder round-trips through the fake server and back). The FILLED execution event
    // DUAL-PUBLISHES [Event::Fill, Event::OrderFilled], so the full sequence is 4 events, not 3.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got: Vec<Event> = Vec::new();
    while got.len() < 4 && Instant::now() < deadline {
        got.extend(drain!(ingest));
        if got.len() < 4 {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let kinds: Vec<&str> = got
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderAccepted(_) => "Accepted",
            Event::Fill(_) => "Fill",
            Event::OrderFilled(_) => "Filled",
            _ => "other",
        })
        .collect();
    // The bare `Event::Fill` (Account fold) is published FIRST, then the `Event::OrderFilled` wrap.
    assert_eq!(kinds, vec!["Submitted", "Accepted", "Fill", "Filled"], "got events: {got:?}");

    // Field-level assertions: coid carried throughout, venue order id + fill descaled.
    match &got[0] {
        Event::OrderSubmitted(s) => assert_eq!(s.client_order_id, "c-1"),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    match &got[1] {
        Event::OrderAccepted(a) => {
            assert_eq!(a.client_order_id, "c-1");
            assert_eq!(a.venue_order_id.as_deref(), Some(FAKE_ORDER_ID.to_string().as_str()));
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
    // [2] is the bare Fill the core Account folds into position/PnL.
    match &got[2] {
        Event::Fill(fill) => {
            assert_eq!(fill.client_order_id, "c-1");
            assert_eq!(fill.symbol.as_str(), "EURUSD");
            assert_eq!(fill.side, 1);
            assert_eq!(fill.last_qty, 1000.0); // 100_000 centi / 100
            assert_eq!(fill.last_px, FAKE_FILL_PRICE);
        }
        other => panic!("expected Event::Fill, got {other:?}"),
    }
    // [3] is the OrderFilled wrap carrying the same fill for the OMS FSM.
    match &got[3] {
        Event::OrderFilled(f) => {
            assert_eq!(f.client_order_id, "c-1");
            assert_eq!(f.fill.symbol.as_str(), "EURUSD");
            assert_eq!(f.fill.side, 1);
            assert_eq!(f.fill.last_qty, 1000.0); // 100_000 centi / 100
            assert_eq!(f.fill.last_px, FAKE_FILL_PRICE);
        }
        other => panic!("expected OrderFilled, got {other:?}"),
    }

    server.assert_saw(&["NEW_ORDER_REQ"]);

    // F3 fix 2: the ORDER_FILLED is a DEFINITELY-TERMINAL event, so the coid→orderId entry the
    // ORDER_ACCEPTED inserted is now PRUNED (it would otherwise linger forever and make every
    // reconnect's reconcile gap-diff warn for this historically-completed coid). Map resolution for
    // a still-LIVE order — the path cancel/modify need — is covered by
    // `conn_reconnect.rs::reconcile_rebuilds_coid_order_map_on_reconnect` (a real CANCEL_ORDER_REQ
    // resolves off the map) and `terminal_fill_prunes_coid_from_map` below.
    assert_eq!(exec.venue_order_id("c-1"), None, "a full fill must prune the coid from the map");
}

/// F3 fix 2 (dedicated): a filled order's coid must be pruned from the coid→orderId map on the
/// terminal `ORDER_FILLED` — so a later reconnect's reconcile gap-diff no longer warns for it (the
/// map used to accumulate every coid forever, spamming a "possible gap fill/cancel" warn for every
/// historically-completed order on every reconnect). Submits a market order, waits for the whole
/// Submitted→Accepted→Filled sequence, then asserts the map no longer resolves the coid.
#[test]
fn terminal_fill_prunes_coid_from_map() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market_buy("prune-1"));

    // Wait for the terminal fill to land (Submitted + Accepted + Filled) before asserting on the
    // map — the ACCEPTED inserts the coid, the terminal FILLED prunes it.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_fill = false;
    while !saw_fill && Instant::now() < deadline {
        for e in drain!(ingest) {
            if let Event::OrderFilled(f) = e {
                if f.client_order_id == "prune-1" {
                    saw_fill = true;
                }
            }
        }
        if !saw_fill {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(saw_fill, "expected the market order to fill");

    assert_eq!(
        exec.venue_order_id("prune-1"),
        None,
        "the terminal fill must prune the coid so reconnect gap-diff does not warn for it"
    );
}

/// This binary's verdict must not change when an operator HALT sentinel is engaged —
/// `common::assert_indifferent_to_an_engaged_halt_sentinel` carries the argument and the mechanism.
#[test]
fn the_exec_suite_is_indifferent_to_an_engaged_halt_sentinel() {
    common::assert_indifferent_to_an_engaged_halt_sentinel();
}
