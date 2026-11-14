//! cpapi lifecycle over the shared `run_execution` loop. cpapi's transport correctness (REST/WS/reply
//! parsing) is unit-tested in `decode.rs`/`reply.rs`/`tests/ibkr_cpapi_decode.rs`; here we prove the
//! SAME `run_execution` + `EventMapper` handles a cpapi-flavored inbound sequence (order-client-id-carrying) end to
//! end: place → accept → partial → final. The `IbkrTransportFake` doesn't run `IbkrTransportCpapi`'s
//! internal order client id→order_id map (that's transport-internal machinery, exercised live only), so this
//! script supplies the ALREADY-RESOLVED numeric `order_id` the real transport's `number_inbound`
//! would have filled in — mirroring what the shared loop actually receives.
//!
//! Feature-gated on `test-support` only (not `ibkr-cpapi`): it exercises the shared `run_execution` loop
//! via `IbkrTransportFake`, not any cpapi-specific code.
#![cfg(feature = "test-support")]

use std::assert_matches;
use std::time::Duration;

use vike_execution::ExecutionClient;
use vike_execution::{Ingest, event_channel};
use vike_ibkr::run_execution_for_test;
use vike_ibkr::testing::{IbkrTransportFake, ScriptedInbound};
use vike_model::OrderRequest;
use vike_model::events::Event;

#[test]
fn cpapi_flavored_place_partial_final_lifecycle() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);

    // Script: NextValidId(101), AccountsReady, then — in response to the submit's place_order — a
    // cpapi-flavored sequence: Submitted status, a partial fill (exec e1, 4 of 10) + its commission,
    // then the final fill (exec e2, 6 of 10) + its commission.
    let mut fake = IbkrTransportFake::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake.on_place_order(|order_id| {
        vec![
            ScriptedInbound::OrderStatus {
                order_id,
                order_ref: "cp-1".into(),
                status: "Submitted".into(),
            },
            ScriptedInbound::ExecutionDetails {
                order_id,
                exec_id: "e1".into(),
                shares: 4.0,
                price: 190.0,
            },
            ScriptedInbound::Commission { exec_id: "e1".into(), commission: 0.0 },
            ScriptedInbound::ExecutionDetails {
                order_id,
                exec_id: "e2".into(),
                shares: 6.0,
                price: 190.0,
            },
            ScriptedInbound::Commission { exec_id: "e2".into(), commission: 0.0 },
        ]
    });

    let mut handle = run_execution_for_test(events, Box::new(fake));
    let order = OrderRequest {
        order_client_id: "cp-1".into(),
        venue: "ibkr".into(),
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 10.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    handle.submit(&order);

    // OrderSubmitted, OrderAccepted, then [Fill, OrderPartiallyFilled] and [Fill, OrderFilled] = 6 events.
    let got = drain(&mut rx_events, 6, Duration::from_secs(2));
    assert_eq!(got.len(), 6, "expected exactly 6 events in 2s, got {got:?}");
    assert_matches!(got[0], Event::OrderSubmitted(_), "got[0]={:?}", got[0]);
    assert_matches!(got[1], Event::OrderAccepted(_), "got[1]={:?}", got[1]);
    assert_matches!(got[2], Event::Fill(_), "got[2]={:?}", got[2]);
    assert_matches!(got[3], Event::OrderPartiallyFilled(_), "got[3]={:?}", got[3]);
    assert_matches!(got[4], Event::Fill(_), "got[4]={:?}", got[4]);
    match &got[5] {
        Event::OrderFilled(f) => assert_eq!(f.order_client_id, "cp-1"),
        other => panic!("got[5]={other:?}"),
    }

    handle.detach();
}

/// Block-receive up to `n` events within `timeout` off the ingest lane. Runs on a plain test
/// thread (no tokio runtime), so `try_recv` + a short sleep is the poll primitive; non-`Event`
/// `Ingest` variants are ignored. Mirrors `tests/ibkr_lifecycle.rs`'s helper.
fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, n: usize, timeout: Duration) -> Vec<Event> {
    let deadline = std::time::Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < n && std::time::Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(event)) => out.push(event),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    out
}
