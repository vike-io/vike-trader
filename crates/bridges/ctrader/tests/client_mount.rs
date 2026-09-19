//! Scripted integration test for the F4 single-socket combined mount (`client::CtraderClient`).
//! ONE `CtraderClient::connect` opens ONE authenticated socket; its `data()` and `exec()` views
//! then drive that SAME actor/socket — a `subscribe_quotes` delivers a descaled quote to the sink
//! AND a `submit` round-trips Submitted/Accepted/Filled on the ingest lane, all over one connection.
//! Proves (a) both seams share one socket (exactly ONE `APPLICATION_AUTH_REQ` seen) and (b) the
//! views do NOT own the actor — dropping them leaves the socket up until the client is shut down.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::client::CtraderClient;
use vike_ctrader::conn::ConnConfig;
use vike_data::DataClient;
use vike_exec::lanes::Ingest;
use vike_exec::{ExecutionClient, event_channel};
use vike_model::OrderRequest;
use vike_model::events::Event;

use common::{FAKE_FILL_PRICE, FakeCtrader, RecordingSink};

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
fn one_client_drives_data_and_exec_over_a_single_socket() {
    let server = FakeCtrader::start_scripted();
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let sink = Arc::new(RecordingSink::default());
    let (events, mut ingest) = event_channel(64);

    // ONE connection, ONE handshake.
    let client = CtraderClient::connect(cfg, sink.clone(), events).expect("auth ok");
    assert_eq!(client.ctid(), 99);

    // Two views over the SAME socket. The exec view is PINNED to a sentinel this process owns and
    // never creates — `CtraderExec`'s default watches the PROCESS-WIDE operator HALT file, which
    // would refuse the opening submit below on any box that has one (see
    // `common::unengaged_halt_sentinel`).
    let mut data = client.data();
    let mut exec = client.exec().with_halt_path(common::unengaged_halt_sentinel(module_path!()));

    // Data seam: subscribe → the fake pushes a SPOT_EVENT → the sink records a descaled quote.
    data.subscribe_quotes("EURUSD").expect("subscribe ok");
    // Exec seam: submit a market order → Submitted/Accepted/Filled on the ingest lane.
    exec.submit(&market_buy("c-1"));

    // Wait for the quote AND the three exec events, both of which arrive asynchronously.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got: Vec<Event> = Vec::new();
    loop {
        got.extend(drain!(ingest));
        let quote_in = !sink.quotes().is_empty();
        // 4 exec events now: Submitted, Accepted, then the DUAL-PUBLISHED [Fill, Filled] pair.
        if (quote_in && got.len() >= 4) || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // Data delivery over the shared socket.
    let quote = sink
        .quotes()
        .first()
        .cloned()
        .expect("a quote should have reached the sink over the shared socket");
    assert_eq!(quote.0, "ctrader");
    assert_eq!(quote.1, "EURUSD");
    assert!((quote.2.bid - 1.13911).abs() < 1e-9);

    // Exec round-trip over the SAME shared socket.
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
    // The FILLED execution event DUAL-PUBLISHES: the bare `Event::Fill` (Account fold) FIRST, then
    // the `Event::OrderFilled` wrap (OMS FSM) — so the exec round-trip is Submitted/Accepted/Fill/Filled.
    assert_eq!(kinds, vec!["Submitted", "Accepted", "Fill", "Filled"], "got: {got:?}");
    match got.last() {
        Some(Event::OrderFilled(f)) => {
            assert_eq!(f.client_order_id, "c-1");
            assert_eq!(f.fill.last_px, FAKE_FILL_PRICE);
        }
        other => panic!("expected OrderFilled last, got {other:?}"),
    }

    // The whole point of F4: ONE socket. Only a single handshake ever happened, even though both a
    // data view and an exec view are live.
    assert_eq!(
        server.count_seen("APPLICATION_AUTH_REQ"),
        1,
        "combined mount must use exactly one socket/handshake"
    );

    // Dropping the VIEWS must not tear the socket down (they are not owners).
    drop(data);
    drop(exec);
    // The client is the sole owner — this is the one place the socket closes.
    client.shutdown();
}

/// This binary's verdict must not change when an operator HALT sentinel is engaged —
/// `common::assert_indifferent_to_an_engaged_halt_sentinel` carries the argument and the mechanism.
#[test]
fn the_client_mount_suite_is_indifferent_to_an_engaged_halt_sentinel() {
    common::assert_indifferent_to_an_engaged_halt_sentinel();
}
