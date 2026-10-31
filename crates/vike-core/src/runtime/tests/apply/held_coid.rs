//! An operator's explicit client order id that the engine already holds ANSWERED or FINISHED is refused
//! by `lower_submit`, and the held order is left exactly as it was (`CoreThread::answered_coid_status`,
//! `CoreThread::refuse_held_coid`; decision 0207).
//!
//! # The defect these gate
//!
//! The door refuses a reused coid from the PUBLISHED snapshot, which is coalesced: a second command
//! for one id inside one publish interval saw the same snapshot and passed. Past the door the core
//! inserted the new request into the registry UNDER THE SAME KEY, so an order the venue had accepted
//! was replaced by an `INITIALIZED` entry that the gatewayhub's dedup then never answered (it read the
//! repeat as a retry and dropped it), and the stuck-order watchdog rejected that entry locally while
//! the venue still held the original; a finished order's id gave the same result.
//!
//! # What is NOT refused, and why a test says so
//!
//! An order the venue has not answered (`INITIALIZED`, `SUBMITTED`) is the in-flight retry, passed as it
//! always was. `crates/vike-core/tests/runtime_latency.rs`'s `run_submit_hop` re-sends ONE coid to a
//! probe that never answers, so the same rule keeps the measured submit hop measuring the work it always
//! measured. A minted coid (empty on the wire) never asks the question at all.

use super::*;
use tracing_test::traced_test;
use vike_model::OrderStatus;

/// A resting limit order on the core's venue and symbol, with `price` as the only term a test varies.
fn limit(coid: &str, price: f64) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        price: Some(price),
        order_type: vike_model::OrderType::Limit,
        ..Default::default()
    })
}

/// `Initialized -> Submitted -> Accepted`, the pair every real adapter emits at submit.
fn accept(c: &mut CoreThread<RecordingClient>, coid: &str) {
    c.dispatch(Ingest::Event(Event::OrderSubmitted(vike_model::events::OrderSubmitted {
        client_order_id: coid.to_string(),
        ts: 0,
    })));
    c.dispatch(Ingest::Event(Event::OrderAccepted(vike_model::events::OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some(format!("v-{coid}").into()),
        ts: 0,
    })));
}

/// The ring lines that start with `prefix`.
fn lines(c: &CoreThread<RecordingClient>, prefix: &str) -> Vec<String> {
    c.recent.iter().filter(|l| l.starts_with(prefix)).map(|l| l.to_string()).collect()
}

/// THE MEASURED CASE: the venue accepted the order and the registry holds it LIVE. A second command
/// for the same id, with other terms, sends nothing, writes nothing, and says so.
#[test]
#[traced_test]
fn a_coid_whose_order_the_venue_accepted_is_refused_and_that_order_is_untouched() {
    let mut c = test_core();
    assert_eq!(c.apply_intent(OrderIntent::Submit(limit("dup1", 100.0)), 0), vec!["dup1"]);
    accept(&mut c, "dup1");
    let held = c.engine.registry.get("dup1").cloned().expect("premise: the order is held");
    assert_eq!(held.status, OrderStatus::Accepted, "premise: the venue answered it");

    let again = c.apply_intent(OrderIntent::Submit(limit("dup1", 101.0)), 0);

    assert!(again.is_empty(), "nothing was submitted, so no coid is returned: {again:?}");
    assert_eq!(c.engine.client.submissions.len(), 1, "the repeat never reached the client");
    assert_eq!(
        c.engine.registry.get("dup1"),
        Some(&held),
        "the held order is byte for byte as it was"
    );
    assert_eq!(c.engine.registry.len(), 1);
    let said = lines(&c, "order dup1 REFUSED, nothing sent");
    assert_eq!(said.len(), 1, "one ring line: {:?}", c.recent);
    assert!(
        said[0].contains("already used by an order that is ACCEPTED (still live)"),
        "{}",
        said[0]
    );
    assert!(said[0].contains("modify or cancel"), "a live order is changed by its id: {}", said[0]);
    assert!(logs_contain("is already used by an order that is ACCEPTED"), "a WARN at the command");
}

/// A FINISHED order's id is not free either: the dedup downstream would swallow the repeat.
#[test]
fn a_coid_whose_order_is_filled_is_refused_and_books_nothing_twice() {
    let mut c = test_core();
    c.apply_intent(OrderIntent::Submit(market_req("done1")), 0);
    for ev in fill_events("done1", "sim", "BTCUSDT", 1, 1.0, 100.0) {
        c.dispatch(Ingest::Event(ev));
    }
    let held = c.engine.registry.get("done1").cloned().expect("premise: the order is held");
    assert_eq!(held.status, OrderStatus::Filled, "premise: it filled");

    let again = c.apply_intent(OrderIntent::Submit(market_req("done1")), 0);

    assert!(again.is_empty(), "{again:?}");
    assert_eq!(c.engine.client.submissions.len(), 1, "the repeat never reached the client");
    assert_eq!(c.engine.registry.get("done1"), Some(&held), "the filled order is not replaced");
    let said = lines(&c, "order done1 REFUSED, nothing sent");
    assert_eq!(said.len(), 1, "{:?}", c.recent);
    assert!(
        said[0].ends_with("already used by an order that is FILLED: pick a new client order id"),
        "{}",
        said[0]
    );
}

/// PRESERVE: an order the venue has not answered is the in-flight retry and goes through as it always
/// did (the entry is rewritten, the client is sent the repeat and its dedup drops it). This is the
/// state `run_submit_hop`'s probe holds for the whole run.
#[test]
fn a_coid_whose_order_is_not_yet_answered_is_not_refused() {
    let mut c = test_core();
    assert_eq!(c.apply_intent(OrderIntent::Submit(limit("retry1", 100.0)), 0), vec!["retry1"]);
    assert_eq!(
        c.engine.registry.get("retry1").map(|o| o.status),
        Some(OrderStatus::Initialized),
        "premise: a recording client answers nothing"
    );

    assert_eq!(c.apply_intent(OrderIntent::Submit(limit("retry1", 100.0)), 0), vec!["retry1"]);
    c.dispatch(Ingest::Event(Event::OrderSubmitted(vike_model::events::OrderSubmitted {
        client_order_id: "retry1".to_string(),
        ts: 0,
    })));
    assert_eq!(c.engine.registry.get("retry1").map(|o| o.status), Some(OrderStatus::Submitted));
    assert_eq!(c.apply_intent(OrderIntent::Submit(limit("retry1", 100.0)), 0), vec!["retry1"]);

    assert_eq!(c.engine.client.submissions.len(), 3, "each retry reached the client, as before");
    assert_eq!(c.engine.registry.len(), 1, "one entry under the id");
    assert!(lines(&c, "order retry1 REFUSED").is_empty(), "{:?}", c.recent);
}

/// PRESERVE: a coid the engine does not hold, and a minted one, are never asked about.
#[test]
fn a_new_coid_and_a_minted_one_are_not_refused() {
    let mut c = test_core();
    c.apply_intent(OrderIntent::Submit(limit("a", 100.0)), 0);
    accept(&mut c, "a");

    assert_eq!(c.apply_intent(OrderIntent::Submit(limit("b", 100.0)), 0), vec!["b"]);
    let one = c.apply_intent(OrderIntent::Submit(limit("", 100.0)), 0);
    let two = c.apply_intent(OrderIntent::Submit(limit("", 100.0)), 0);

    assert_eq!((one.len(), two.len()), (1, 1), "a minted submit always lowers");
    assert_ne!(one, two, "two mints are two ids");
    assert_eq!(c.engine.registry.len(), 4);
    assert!(lines(&c, "order ").iter().all(|l| !l.contains("REFUSED")), "{:?}", c.recent);
}

/// Which statuses refuse. An exhaustive `match`, so a new `OrderStatus` does not compile until its
/// reuse is decided here (the unanswered pair is the only exemption, and the reason is in
/// `answered_coid_status`'s doc).
fn refused(status: OrderStatus) -> bool {
    match status {
        OrderStatus::Initialized | OrderStatus::Submitted => false,
        OrderStatus::Accepted
        | OrderStatus::Triggered
        | OrderStatus::PartiallyFilled
        | OrderStatus::Filled
        | OrderStatus::Canceled
        | OrderStatus::Rejected
        | OrderStatus::Denied
        | OrderStatus::Expired
        | OrderStatus::PendingCancel
        | OrderStatus::Liquidated
        | OrderStatus::Emulated
        | OrderStatus::Released => true,
    }
}

const EVERY_STATUS: [OrderStatus; 14] = [
    OrderStatus::Initialized,
    OrderStatus::Submitted,
    OrderStatus::Accepted,
    OrderStatus::Triggered,
    OrderStatus::PartiallyFilled,
    OrderStatus::Filled,
    OrderStatus::Canceled,
    OrderStatus::Rejected,
    OrderStatus::Denied,
    OrderStatus::Expired,
    OrderStatus::PendingCancel,
    OrderStatus::Liquidated,
    OrderStatus::Emulated,
    OrderStatus::Released,
];

/// Every status, planted directly on the registry entry: refused exactly when `refused` says so, and a
/// refusal leaves the entry (whatever its status) as it was.
#[test]
fn every_status_but_the_unanswered_pair_refuses_a_reused_coid() {
    for status in EVERY_STATUS {
        let mut c = test_core();
        c.apply_intent(OrderIntent::Submit(limit("p", 100.0)), 0);
        c.engine.registry.get_mut("p").expect("premise: held").status = status;
        let held = c.engine.registry.get("p").cloned();

        let again = c.apply_intent(OrderIntent::Submit(limit("p", 100.0)), 0);

        if refused(status) {
            assert!(again.is_empty(), "{status:?} must refuse, got {again:?}");
            assert_eq!(c.engine.registry.get("p"), held.as_ref(), "{status:?}: entry untouched");
            assert_eq!(c.engine.client.submissions.len(), 1, "{status:?}: nothing sent");
        } else {
            assert_eq!(again, vec!["p"], "{status:?} is the in-flight retry and must pass");
            assert_eq!(c.engine.client.submissions.len(), 2, "{status:?}: the retry was sent");
        }
    }
}

/// The probe is the ROUTED engine's registry: the same id on another account's engine is another
/// order (the adapters' dedup is per account), so it is not refused by a book it does not belong to.
#[test]
fn the_same_coid_on_another_engine_is_not_refused() {
    let mut c = core_of(
        sim_engine_with(RecordingClient::default()),
        vec![(1.0, engine_on("sim2", "BTCUSDT", RecordingClient::default()))],
        CoreConfig::default(),
    );
    c.apply_intent(OrderIntent::Submit(limit("shared", 100.0)), 0);
    accept(&mut c, "shared");
    assert_eq!(c.engine.registry.get("shared").map(|o| o.status), Some(OrderStatus::Accepted));

    let mut other = limit("shared", 100.0);
    other.venue = "sim2".into();
    let placed = c.apply_intent(OrderIntent::Submit(other), 0);

    assert_eq!(placed, vec!["shared"], "the other venue's engine holds no such id");
    assert!(c.extra_engines[0].1.registry.contains_key("shared"));
}
