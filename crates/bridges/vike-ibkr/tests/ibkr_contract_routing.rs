//! **What contract does a submitted order actually go out against?** — driven through the REAL
//! `run_execution` loop over the `IbkrTransportFake`, and asserted on the `IbkrContract` the transport was
//! HANDED rather than on any string.
//!
//! ⚠ This file exists because of a measured order-path defect.
//! `crates/bridges/vike-ibkr/src/instrument/contract.rs`'s `parse_simplified` used to return
//! `SecType::Stk` for every canonical it did not recognise, so `ESZ5.GLOBEX.USD` — the E-mini S&P
//! 500 future — was submitted as a STOCK. Nothing failed: no test looked at the contract (the
//! double discarded it), the event lane showed an ordinary `OrderSubmitted`, and IBKR either
//! rejected the order or matched it to some equity ticker and filled it. The three assertions
//! below are the three things that had to be true for that to be impossible: a futures canonical
//! arrives as a FUTURE, an unreadable one is REFUSED before anything reaches the transport, and
//! the equity canonical this tree starts is unchanged.
//!
//! Feature-gated on `test-support` like its sibling `ibkr_lifecycle.rs`: the `testing` surface
//! only exists behind that feature.
#![cfg(feature = "test-support")]

use std::assert_matches;
use std::time::{Duration, Instant};

use vike_bridge_core::execution_drive::drain;

use vike_execution::ExecutionClient;
use vike_execution::event_channel;
use vike_ibkr::instrument::contract::{IbkrContract, SecType};
use vike_ibkr::run_execution_for_test;
use vike_ibkr::testing::{IbkrTransportFake, ScriptedInbound};
use vike_model::OrderRequest;
use vike_model::events::Event;

fn order(order_client_id: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "ibkr".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    }
}

/// A connected fake: the two handshake seeds `submit_refusal`'s readiness gate waits for.
fn ready_fake() -> IbkrTransportFake {
    let mut fake = IbkrTransportFake::new();
    fake.script(ScriptedInbound::NextValidId(101));
    fake.script(ScriptedInbound::AccountsReady);
    fake
}

/// Wait until `place_order` has been called `n` times, or the deadline passes. Returns whatever the
/// transport recorded — so an assertion of ZERO calls is still a real wait rather than a race the
/// test wins by being fast.
fn placed(fake: &IbkrTransportFake, n: usize, timeout: Duration) -> Vec<IbkrContract> {
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

/// THE FIX, end to end: a future's user name (`ES_991218`, decision 0290) reaches the transport as the FUTURES
/// contract its kept row's wire (`ES.GLOBEX.USD.FUT.20991218`) names, carrying that expiry. A contract far from its
/// last trading day: the delivery guard refuses every order on one whose last trading day has begun.
#[test]
fn a_futures_canonical_is_submitted_as_a_futures_contract() {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let fake = ready_fake();
    let probe = fake.clone();

    let mut handle = run_execution_for_test(events, Box::new(fake));
    handle.submit(&order("order-client-id-fut", "ES_991218"));

    let got = placed(&probe, 1, Duration::from_secs(2));
    assert_eq!(got.len(), 1, "the order did not reach the transport: {got:?}");
    assert_eq!(got[0].sec_type, SecType::Fut, "submitted as {:?}", got[0].sec_type);
    assert_eq!(got[0].symbol, "ES");
    assert_eq!(got[0].exchange, "GLOBEX");
    assert_eq!(got[0].currency, "USD");
    assert_eq!(got[0].expiry.as_deref(), Some("20991218"));

    // The intent is live, not refused: exactly the synchronous OrderSubmitted and nothing terminal.
    let evs = drain(|| rx_events.try_recv().ok(), 2, Duration::from_millis(300));
    assert_matches!(evs.first(), Some(Event::OrderSubmitted(_)), "{evs:?}");
    assert!(
        !evs.iter().any(|e| matches!(e, Event::OrderRejected(_))),
        "a placeable future was rejected: {evs:?}"
    );
    handle.detach();
}

/// The refusal of `symbol`, submitted: it REACHES NO TRANSPORT and comes back as a terminal rejection.
///
/// Both halves matter and neither alone is the test: an order that is silently dropped also
/// records zero place calls, and an order that is rejected AFTER being placed also emits a
/// rejection.
fn refusal_of(symbol: &str) -> String {
    vike_log::test_init();
    let (events, mut rx_events) = event_channel(64);
    let fake = ready_fake();
    let probe = fake.clone();

    let mut handle = run_execution_for_test(events, Box::new(fake));
    handle.submit(&order("order-client-id-bad", symbol));

    let evs = drain(|| rx_events.try_recv().ok(), 2, Duration::from_secs(2));
    assert_eq!(evs.len(), 2, "expected Submitted + Rejected, got {evs:?}");
    assert_matches!(evs[0], Event::OrderSubmitted(_), "{evs:?}");
    let Event::OrderRejected(ref r) = evs[1] else { panic!("expected a rejection: {evs:?}") };
    assert_eq!(r.order_client_id, "order-client-id-bad");
    assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
    assert!(
        placed(&probe, 1, Duration::from_millis(300)).is_empty(),
        "the refused order still reached the transport"
    );
    handle.detach();
    r.reason.to_string()
}

/// ⚠ THE DEFECT, driven through the order path: `ESZ5.GLOBEX.USD` (and any name no kept row reads) is REFUSED before
/// the transport, naming the verb that adds an instrument (decision 0290's O-10: IBKR lists nothing). Before the fix
/// this canonical produced ONE place call carrying `SecType::Stk`; the wire grammar's own refusal of it is
/// `instrument/contract_tests.rs`'s.
#[test]
fn an_unreadable_canonical_is_refused_before_it_reaches_the_transport() {
    for unread in ["ESZ5.GLOBEX.USD", "IBM"] {
        let reason = refusal_of(unread);
        assert!(reason.contains(unread), "names the symbol: {reason}");
        assert!(
            reason.contains("vike-cli data instruments add --venue ibkr"),
            "names the fix: {reason}"
        );
    }
}

/// **An old spelling is refused naming its user name** (decision 0290: a wire name is the bridge's alone).
#[test]
fn a_wire_name_is_refused_naming_its_user_name() {
    let reason = refusal_of("AAPL.SMART.USD");
    assert!(reason.contains("`AAPL`"), "names the user name: {reason}");
}

/// **A forex pair's user name** (`USDJPY`) goes out as the IDEALPRO CASH pair its kept row's wire
/// (`USD.JPY.IDEALPRO`) names: one unit is one dollar, priced in yen.
#[test]
fn a_forex_user_name_is_submitted_as_the_idealpro_cash_pair() {
    vike_log::test_init();
    let (events, _rx) = event_channel(64);
    let fake = ready_fake();
    let probe = fake.clone();

    let mut handle = run_execution_for_test(events, Box::new(fake));
    handle.submit(&order("order-client-id-fx", "USDJPY"));

    let got = placed(&probe, 1, Duration::from_secs(2));
    assert_eq!(got.len(), 1, "the order did not reach the transport");
    assert_eq!(
        got[0],
        IbkrContract {
            sec_type: SecType::Cash,
            symbol: "USD".into(),
            exchange: "IDEALPRO".into(),
            currency: "JPY".into(),
            expiry: None,
            strike: None,
            right: None,
            multiplier: None,
            con_id: None,
        }
    );
    handle.detach();
}

/// The equity this tree starts, by its user name (`AAPL`, decision 0290), goes out as the same SMART USD stock as
/// before — asserted on the WHOLE contract so any drift in the default path reddens here.
#[test]
fn the_engine_equity_canonical_is_submitted_exactly_as_before() {
    vike_log::test_init();
    let (events, _rx) = event_channel(64);
    let fake = ready_fake();
    let probe = fake.clone();

    let mut handle = run_execution_for_test(events, Box::new(fake));
    handle.submit(&order("order-client-id-eq", "AAPL"));

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
