//! One command through the REAL `exec_actor::run_loop`, and what the lane then said (`test-support`
//! only, the `scripted` shape).
//!
//! The per-symbol bridges' offline reproductions (`tests/coid_map_failure_repro.rs` of aster,
//! binance, bybit and okx) drive a cancel through the production loop over their own venue fake;
//! the drive and the read of its answer are venue-neutral, so they are written here once (the
//! scenarios themselves are `crate::coid_repro`). The venue's fake and router stay in the venue.
//!
//! [`drain`] is the other half every engine-fill suite of a bridge (alpaca, oanda, fxcm, vike-ibkr) wrote
//! for itself: the events an ingest lane delivered within a bound.
//!
//! The rest is what the bridges' offline suites copied around that drive: [`limit`], the order they
//! send, and the reads of what came back ([`bare_fills`], [`lifecycle_kinds`], [`event_tags`],
//! [`last_kind`], [`coid_of`], [`assert_events_json`], ...). Each reads only `vike_model` events.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use vike_exec::{CancelIntent, Ingest, event_channel};
use vike_model::events::{Event, FillEvent};
use vike_model::{OrderRequest, OrderType};

use crate::exec_actor::{ExecCommand, run_loop};
use crate::rest::VenueRest;

/// The emit seam of a settlement tick (hyperliquid's outcome settlement, polymarket's resolve) as a
/// test holds it: every event pushed into `out`, every one accepted.
pub fn collecting_sink(out: &mut Vec<Event>) -> impl FnMut(Event) -> bool + '_ {
    move |e| {
        out.push(e);
        true
    }
}

/// One `ExecCommand::Cancel` through the real `run_loop`; every event the lane handed the core.
pub fn drive_cancel<R: VenueRest>(rest: &R, coid: &str) -> Vec<Event> {
    let (events, mut ingest) = event_channel(64);
    let (commands, rx) = mpsc::channel();
    let cancel = ExecCommand::Cancel {
        client_order_id: coid.to_string(),
        intent: CancelIntent::Unspecified,
    };
    commands.send(cancel).expect("the loop's channel is open");
    commands.send(ExecCommand::Shutdown).expect("the loop's channel is open");
    run_loop(rest, &events, rx, Vec::new, Duration::from_secs(3600));
    let mut seen = Vec::new();
    while let Ok(Ingest::Event(event)) = ingest.try_recv() {
        seen.push(event);
    }
    seen
}

/// Whether the lane said `OrderCanceled` for `coid`.
pub fn said_canceled(events: &[Event], coid: &str) -> bool {
    events.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == coid))
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
            Some(Ingest::Event(ev)) => out.push(ev),
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
pub fn limit(venue: &str, coid: &str, symbol: &str, qty: f64, price: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(price),
        ..Default::default()
    }
}

/// The deny verdict a submit gate owes: exactly `[OrderSubmitted, OrderRejected]` for `coid`, the
/// reject's reason containing `reason_needle`.
pub fn assert_denied_pair(events: &[Event], coid: &str, reason_needle: &str) {
    assert_eq!(events.len(), 2, "{coid}: exactly the emitter-split pair: {events:?}");
    assert!(
        matches!(&events[0], Event::OrderSubmitted(s) if s.client_order_id == coid),
        "{coid}: the pair opens with its OrderSubmitted: {events:?}"
    );
    match &events[1] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, coid);
            assert!(
                r.reason.contains(reason_needle),
                "loud deny reason must contain {reason_needle:?}, got: {}",
                r.reason
            );
        }
        other => panic!("{coid}: expected terminal OrderRejected, got {other:?}"),
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

/// Each event as `Kind:coid` (a bare fill as `Fill:coid:trade_id`), `other:{debug}` for a kind a
/// history replay does not emit: what the replay tests compare.
pub fn event_tags(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::Fill(fill) => format!("Fill:{}:{}", fill.client_order_id, fill.trade_id),
            Event::OrderFilled(w) => format!("OrderFilled:{}", w.client_order_id),
            Event::OrderPartiallyFilled(w) => {
                format!("OrderPartiallyFilled:{}", w.client_order_id)
            }
            Event::OrderCanceled(w) => format!("OrderCanceled:{}", w.client_order_id),
            Event::OrderExpired(w) => format!("OrderExpired:{}", w.client_order_id),
            Event::OrderRejected(w) => format!("OrderRejected:{}", w.client_order_id),
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

/// The client order id of a submitted / accepted / fill / filled / canceled event, `""` for any
/// other kind.
pub fn coid_of(event: &Event) -> &str {
    match event {
        Event::OrderSubmitted(e) => &e.client_order_id,
        Event::OrderAccepted(e) => &e.client_order_id,
        Event::Fill(e) => &e.client_order_id,
        Event::OrderFilled(e) => &e.client_order_id,
        Event::OrderCanceled(e) => &e.client_order_id,
        _ => "",
    }
}

/// The frozen `fixtures/r6/<name>` parity export (`fixtures/README.md`), parsed through
/// `vike_exec::testing::frozen_fixture`: found above `manifest_dir`, the CALLING test crate's
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
    vike_exec::testing::frozen_fixture(&r6, name, "scripts/export_r6_fixtures.py")
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
pub fn assert_events_json(got: Vec<Event>, want: &serde_json::Value, what: &str) {
    let got: Vec<serde_json::Value> =
        got.iter().map(|e| serde_json::to_value(e).unwrap()).collect();
    let want = want.as_array().unwrap();
    assert_eq!(got.len(), want.len(), "{what}: count {got:?}");
    for (g, w) in got.iter().zip(want) {
        assert_eq!(g, w, "{what}");
    }
}
