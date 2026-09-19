//! **What contract does a submitted order actually go out against?** — driven through the REAL
//! `run_exec` loop over the `FakeTransport`, and asserted on the `IbkrContract` the transport was
//! HANDED rather than on any string.
//!
//! ⚠ This file exists because of a measured order-path defect.
//! `crates/bridges/vike-ibkr/src/contract.rs`'s `parse_simplified` used to return
//! `SecType::Stk` for every canonical it did not recognise, so `ESZ5.GLOBEX.USD` — the E-mini S&P
//! 500 future — was submitted as a STOCK. Nothing failed: no test looked at the contract (the
//! double discarded it), the event lane showed an ordinary `OrderSubmitted`, and IBKR either
//! rejected the order or matched it to some equity ticker and filled it. The three assertions
//! below are the three things that had to be true for that to be impossible: a futures canonical
//! arrives as a FUTURE, an unreadable one is REFUSED before anything reaches the transport, and
//! the equity canonical this tree mounts is unchanged.
//!
//! Feature-gated on `test-support` like its sibling `ibkr_lifecycle.rs`: the `testing` surface
//! only exists behind that feature.
#![cfg(feature = "test-support")]

use std::time::{Duration, Instant};

use vike_exec::ExecutionClient;
use vike_exec::lanes::{Ingest, event_channel};
use vike_ibkr::contract::{IbkrContract, SecType};
use vike_ibkr::run_exec_for_test;
use vike_ibkr::testing::{FakeTransport, ScriptedInbound};
use vike_model::OrderRequest;
use vike_model::events::Event;

fn order(coid: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "ibkr".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: "market".into(),
        ..Default::default()
    }
}

/// A connected fake: the two handshake seeds `submit_refusal`'s readiness gate waits for.
fn ready_fake() -> FakeTransport {
    let mut fake = FakeTransport::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake
}

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

/// Wait until `place_order` has been called `n` times, or the deadline passes. Returns whatever the
/// transport recorded — so an assertion of ZERO calls is still a real wait rather than a race the
/// test wins by being fast.
fn placed(fake: &FakeTransport, n: usize, timeout: Duration) -> Vec<IbkrContract> {
    let handle = fake.place_contracts();
    let deadline = Instant::now() + timeout;
    loop {
        let got = handle.lock().unwrap().clone();
        if got.len() >= n || Instant::now() >= deadline {
            return got;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// THE FIX, end to end: a futures canonical reaches the transport as a FUTURES contract carrying
/// the expiry the operator named.
#[test]
fn a_futures_canonical_is_submitted_as_a_futures_contract() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let fake = ready_fake();
    let probe = fake.clone();

    let mut handle = run_exec_for_test(events, Box::new(fake));
    handle.submit(&order("coid-fut", "ES.GLOBEX.USD.FUT.20251219"));

    let got = placed(&probe, 1, Duration::from_secs(2));
    assert_eq!(got.len(), 1, "the order did not reach the transport: {got:?}");
    assert_eq!(got[0].sec_type, SecType::Fut, "submitted as {:?}", got[0].sec_type);
    assert_eq!(got[0].symbol, "ES");
    assert_eq!(got[0].exchange, "GLOBEX");
    assert_eq!(got[0].currency, "USD");
    assert_eq!(got[0].expiry.as_deref(), Some("20251219"));

    // The intent is live, not refused: exactly the synchronous OrderSubmitted and nothing terminal.
    let evs = drain(&mut rx_events, 2, Duration::from_millis(300));
    assert!(matches!(evs.first(), Some(Event::OrderSubmitted(_))), "{evs:?}");
    assert!(
        !evs.iter().any(|e| matches!(e, Event::OrderRejected(_))),
        "a placeable future was rejected: {evs:?}"
    );
    handle.detach();
}

/// ⚠ THE DEFECT, driven through the order path. `ESZ5.GLOBEX.USD` must REACH NO TRANSPORT and must
/// come back as a terminal rejection whose reason names what could not be read and what would work.
///
/// Both halves matter and neither alone is the test: an order that is silently dropped also
/// records zero place calls, and an order that is rejected AFTER being placed also emits a
/// rejection. Before the fix this canonical produced ONE place call carrying `SecType::Stk`.
#[test]
fn an_unreadable_canonical_is_refused_before_it_reaches_the_transport() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let fake = ready_fake();
    let probe = fake.clone();

    let mut handle = run_exec_for_test(events, Box::new(fake));
    handle.submit(&order("coid-bad", "ESZ5.GLOBEX.USD"));

    let evs = drain(&mut rx_events, 2, Duration::from_secs(2));
    assert_eq!(evs.len(), 2, "expected Submitted + Rejected, got {evs:?}");
    assert!(matches!(evs[0], Event::OrderSubmitted(_)), "{evs:?}");
    let Event::OrderRejected(ref r) = evs[1] else { panic!("expected a rejection: {evs:?}") };
    assert_eq!(r.client_order_id, "coid-bad");
    let reason = r.reason.to_string();
    assert!(reason.contains("ESZ5.GLOBEX.USD"), "names the canonical: {reason}");
    assert!(reason.contains("GLOBEX"), "names the field it could not read: {reason}");
    assert!(reason.contains("FUT.YYYYMMDD"), "names an accepted spelling: {reason}");

    assert!(
        placed(&probe, 1, Duration::from_millis(300)).is_empty(),
        "the refused order still reached the transport"
    );
    handle.detach();
}

/// The equity canonical this tree actually mounts (`crates/vike-run/src/node.rs`'s `IBKR_MARKET`),
/// unchanged — asserted on the WHOLE contract so any drift in the default path reddens here.
#[test]
fn the_mounted_equity_canonical_is_submitted_exactly_as_before() {
    vike_log::test_init();
    let (events, _rx) = event_channel(64);
    let fake = ready_fake();
    let probe = fake.clone();

    let mut handle = run_exec_for_test(events, Box::new(fake));
    handle.submit(&order("coid-eq", "AAPL.SMART.USD"));

    let got = placed(&probe, 1, Duration::from_secs(2));
    assert_eq!(got.len(), 1, "the order did not reach the transport");
    assert_eq!(
        got[0],
        IbkrContract {
            sec_type: SecType::Stk,
            symbol: "AAPL".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            expiry: None,
            strike: None,
            right: None,
            multiplier: None,
            con_id: None,
        }
    );
    handle.detach();
}
