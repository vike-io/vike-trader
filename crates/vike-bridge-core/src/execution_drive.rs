//! One command through the REAL `exec_actor::run_loop`, and what the lane then said (`test-support`
//! only, the `scripted` shape).
//!
//! The per-symbol bridges' offline reproductions (`tests/order_client_id_map_failure_repro.rs` of aster,
//! binance, bybit and okx) drive a cancel through the production loop over their own venue fake;
//! the drive and the read of its answer are venue-neutral, so they are written here once (the
//! scenarios themselves are `crate::order_client_id_repro`). The venue's fake and router stay in the venue.
//!
//! [`drain`] is the other half every engine-fill suite of a bridge (alpaca, oanda, fxcm, vike-ibkr) wrote
//! for itself: the events an ingest lane delivered within a bound.
//!
//! The rest is what the bridges' offline suites copied around that drive: [`limit`], the order they
//! send, and the reads of what came back ([`bare_fills`], [`lifecycle_kinds`], [`event_tags`],
//! [`last_kind`], [`assert_events_json`], ...). Each reads only `vike_model` events; the order client id an
//! event names is `vike_model::events::Event::order_client_id`'s.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use vike_execution::{Ingest, event_channel};
use vike_model::OrderCancelIntent;
use vike_model::events::{Event, FillEvent};
use vike_model::{OrderRequest, OrderType};

use crate::exec_actor::{ExecutionClientActorCommand, run_loop};
use crate::rest::VenueRest;

/// The emit seam of a settlement tick (hyperliquid's outcome settlement, polymarket's resolve) as a
/// test holds it: every event pushed into `out`, every one accepted.
pub fn collecting_sink(out: &mut Vec<Event>) -> impl FnMut(Event) -> bool + '_ {
    move |e| {
        out.push(e);
        true
    }
}

/// One `ExecutionClientActorCommand::Cancel` through the real `run_loop`; every event the lane handed the core.
pub fn drive_cancel<R: VenueRest>(rest: &R, order_client_id: &str) -> Vec<Event> {
    let (events, mut ingest) = event_channel(64);
    let (commands, rx) = mpsc::channel();
    let cancel = ExecutionClientActorCommand::Cancel {
        order_client_id: order_client_id.to_string(),
        intent: OrderCancelIntent::Unspecified,
    };
    commands.send(cancel).expect("the loop's channel is open");
    commands.send(ExecutionClientActorCommand::Shutdown).expect("the loop's channel is open");
    run_loop(rest, &events, &rx, Vec::new, Duration::from_secs(3600));
    let mut seen = Vec::new();
    while let Ok(Ingest::Event(event)) = ingest.try_recv() {
        seen.push(event);
    }
    seen
}

/// Whether the lane said `OrderCanceled` for `order_client_id`.
pub fn said_canceled(events: &[Event], order_client_id: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == order_client_id))
}

/// Up to `n` `Ingest::Event`s, in arrival order, from `try_next` (an ingest lane's
/// `|| rx.try_recv().ok()`), collected until `n` arrived or `timeout` passed (polled every 5 ms); a
/// non-event message is skipped, and an empty or disconnected lane is waited out to the deadline.
pub fn drain(
    mut try_next: impl FnMut() -> Option<Ingest>,
    n: usize,
    timeout: Duration,
) -> Vec<Event> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < n && Instant::now() < deadline {
        match try_next() {
            Some(Ingest::Event(event)) => out.push(event),
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    out
}

/// Every `Ingest::Event` already waiting in `try_next` (an ingest lane's `|| rx.try_recv().ok()`), in
/// arrival order, without waiting; a non-event message is skipped.
pub fn drain_ready(mut try_next: impl FnMut() -> Option<Ingest>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Some(ingest) = try_next() {
        if let Ingest::Event(e) = ingest {
            out.push(e);
        }
    }
    out
}

/// A BUY limit of `qty` `symbol` at `price` on `venue`, every other field defaulted (GTC, no
/// trigger, not reduce-only). A test that needs another fact sets it by struct update over this.
pub fn limit(
    venue: &str,
    order_client_id: &str,
    symbol: &str,
    qty: f64,
    price: f64,
) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(price),
        ..Default::default()
    }
}

/// The deny verdict a submit gate owes: exactly `[OrderSubmitted, OrderRejected]` for `order_client_id`, the
/// reject's reason containing `reason_needle`.
pub fn assert_denied_pair(events: &[Event], order_client_id: &str, reason_needle: &str) {
    assert_eq!(events.len(), 2, "{order_client_id}: exactly the emitter-split pair: {events:?}");
    assert!(
        matches!(&events[0], Event::OrderSubmitted(s) if s.order_client_id == order_client_id),
        "{order_client_id}: the pair opens with its OrderSubmitted: {events:?}"
    );
    match &events[1] {
        Event::OrderRejected(r) => {
            assert_eq!(r.order_client_id, order_client_id);
            assert!(
                r.reason.contains(reason_needle),
                "loud deny reason must contain {reason_needle:?}, got: {}",
                r.reason
            );
        }
        other => panic!("{order_client_id}: expected terminal OrderRejected, got {other:?}"),
    }
}

/// The bare `Event::Fill`s of `events`, in order.
pub fn bare_fills(events: &[Event]) -> Vec<&FillEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect()
}

/// The trade id of each bare fill, in order.
pub fn fill_trade_ids(events: &[Event]) -> Vec<String> {
    bare_fills(events).iter().map(|f| f.trade_id.to_string()).collect()
}

/// The symbol each bare fill was stamped with, in order.
pub fn fill_symbols(events: &[Event]) -> Vec<String> {
    bare_fills(events).iter().map(|f| f.symbol.to_string()).collect()
}

/// The commission the first bare fill booked; panics when there is no bare fill.
pub fn first_commission(events: &[Event]) -> f64 {
    bare_fills(events).first().expect("a bare Event::Fill").commission
}

/// The order lifecycle the engine sees, by variant name: `OrderSubmitted`, `OrderAccepted`, `Fill`,
/// `OrderFilled`, and `other` for anything else.
pub fn lifecycle_kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "OrderSubmitted",
            Event::OrderAccepted(_) => "OrderAccepted",
            Event::Fill(_) => "Fill",
            Event::OrderFilled(_) => "OrderFilled",
            _ => "other",
        })
        .collect()
}

/// Each event as `Kind:order_client_id` (a bare fill as `Fill:order_client_id:trade_id`), `other:{debug}` for a kind a
/// history replay does not emit: what the replay tests compare.
pub fn event_tags(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::Fill(fill) => format!("Fill:{}:{}", fill.order_client_id, fill.trade_id),
            Event::OrderFilled(w) => format!("OrderFilled:{}", w.order_client_id),
            Event::OrderPartiallyFilled(w) => {
                format!("OrderPartiallyFilled:{}", w.order_client_id)
            }
            Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.order_client_id),
            Event::OrderExpired(w) => format!("OrderExpired:{}", w.order_client_id),
            Event::OrderRejected(w) => format!("OrderRejected:{}", w.order_client_id),
            other => format!("other:{other:?}"),
        })
        .collect()
}

/// The last event's verdict on a submit: `Accepted`, `Rejected` or `other`; panics on no event.
pub fn last_kind(events: &[Event]) -> &'static str {
    match events.last().unwrap() {
        Event::OrderAccepted(_) => "Accepted",
        Event::OrderRejected(_) => "Rejected",
        _ => "other",
    }
}

/// The frozen `fixtures/r6/<name>` parity export (`fixtures/README.md`), parsed through
/// `vike_execution::testing::frozen_fixture`: found above `manifest_dir`, the CALLING test crate's
/// `env!("CARGO_MANIFEST_DIR")` (expanded there, so no compile-time path is baked into this crate).
pub fn r6_fixture(manifest_dir: &str, name: &str) -> serde_json::Value {
    let r6 = std::path::Path::new(manifest_dir)
        .ancestors()
        .map(|dir| dir.join("fixtures/r6"))
        .find(|dir| dir.is_dir())
        .unwrap_or_else(|| panic!("no fixtures/r6 above {manifest_dir}"));
    // ⚠ This said "run scripts/export_r6_fixtures.py", which is a command no checkout can run: the
    // exporter was deleted with the rest of the Python app and `fixtures/r6/` is now the oracle
    // itself (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). The dead path
    // is still NAMED — it is the evidence for where the bytes came from, and it is carried in
    // `crates/vike-ops/tests/docs/citation_gate/dead_paths.rs`'s `DEAD_PATH_EXCEPTIONS`.
    vike_execution::testing::frozen_fixture(&r6, name, "scripts/export_r6_fixtures.py")
}

/// A fixture's `[[name, value], ...]` parameter list as `(name, value)` pairs, values kept as JSON.
pub fn typed_pairs(v: &serde_json::Value) -> Vec<(String, serde_json::Value)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_str().unwrap().to_string(), p[1].clone()))
        .collect()
}

/// `got`, each event serialized, equal to the JSON array `want` element for element; `what` names
/// the case in the failure.
///
/// One key is compared apart: a refusal's `refusal_id` (2026-11-23), which the frozen oracle predates, so
/// no fixture can hold it. It is taken off `got` before the exact comparison of everything else, and it
/// is bounded instead: it must be a word of the order catalog (`vike_model::OrderRefusalId`), never free
/// text. Which id a mapper sets is pinned by the mapper's own tests, not here.
pub fn assert_events_json(got: Vec<Event>, want: &serde_json::Value, what: &str) {
    let got: Vec<serde_json::Value> = got
        .iter()
        .map(|e| {
            let mut value = serde_json::to_value(e).unwrap();
            if let Some(id) = value.as_object_mut().and_then(|o| o.remove("refusal_id")) {
                let word = id.as_str().unwrap_or_default();
                assert!(
                    vike_model::OrderRefusalId::from_word(word).is_some(),
                    "{what}: refusal_id {id} is no order catalog id"
                );
            }
            value
        })
        .collect();
    let want = want.as_array().unwrap();
    assert_eq!(got.len(), want.len(), "{what}: count {got:?}");
    for (g, w) in got.iter().zip(want) {
        assert_eq!(g, w, "{what}");
    }
}
