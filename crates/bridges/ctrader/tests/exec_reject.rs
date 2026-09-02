//! Scripted integration test for the venue-REJECT path (final-review Fix A): a submitted order
//! whose venue reply is an `ERROR_RES` (not an execution event) must NOT sit in `Submitted`
//! forever. `connect_and_auth_exec` against the in-process fake server, a `CtraderExec` submits a
//! market order, the fake replies with an `ERROR_RES` carrying that order's envelope `clientMsgId`
//! (the coid), and the actor thread correlates it and synthesizes a terminal `OrderRejected`.
//! Asserts the ingest lane saw `OrderSubmitted` then `OrderRejected` for that coid — the
//! "no order silently vanishes" contract on the reject path (companion to `tests/exec.rs`).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::conn::{connect_and_auth_exec, ConnConfig};
use vike_exec::lanes::Ingest;
use vike_exec::{event_channel, ExecutionClient};
use vike_model::events::Event;
use vike_model::OrderRequest;

use common::{FakeCtrader, NoopSink};

/// Drain everything currently on the ingest lane into a flat `Vec<Event>` (see `tests/exec.rs`).
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
fn submit_then_venue_error_res_yields_submitted_then_terminal_rejected() {
    let server = FakeCtrader::start_reject_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let (events, mut ingest) = event_channel(64);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let mut exec = common::exec_with_no_halt(handle, events, module_path!());

    exec.submit(&market_buy("c-err"));

    // Poll the ingest lane until both events land (the venue ERROR_RES round-trips through the fake
    // server asynchronously after the NewOrder is written).
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got: Vec<Event> = Vec::new();
    while got.len() < 2 && Instant::now() < deadline {
        got.extend(drain!(ingest));
        if got.len() < 2 {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let kinds: Vec<&str> = got
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderRejected(_) => "Rejected",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["Submitted", "Rejected"], "got events: {got:?}");

    match &got[0] {
        Event::OrderSubmitted(s) => assert_eq!(s.client_order_id, "c-err"),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    match &got[1] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "c-err", "reject must carry the submitted coid");
            // Reason is the venue error code/description — never a credential.
            assert!(
                r.reason.contains("NOT_ENOUGH_MONEY"),
                "reject reason should carry the venue error code, got {:?}",
                r.reason
            );
        }
        other => panic!("expected terminal OrderRejected, got {other:?}"),
    }

    server.assert_saw(&["NEW_ORDER_REQ"]);
}

/// This binary's verdict must not change when an operator HALT sentinel is engaged —
/// `common::assert_indifferent_to_an_engaged_halt_sentinel` carries the argument and the mechanism.
#[test]
fn the_exec_reject_suite_is_indifferent_to_an_engaged_halt_sentinel() {
    common::assert_indifferent_to_an_engaged_halt_sentinel();
}
