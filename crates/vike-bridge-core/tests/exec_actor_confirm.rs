//! `Command::ConfirmOrder` → `ExecActor::confirm` (audit ex1 residual): the core's stuck-order
//! watchdog can ACTIVELY prod a wedged adapter to re-confirm an un-acked order instead of only
//! waiting for the grace to lapse. This proves the SHARED `ExecActor` seam:
//!
//!   * a venue WITHOUT a re-query wired → `confirm` is a clean no-op (nothing on the lane);
//!   * a venue WITH a re-query → the actor runs it on its OWN worker thread (off the core fold) and
//!     forwards the authoritative event onto the same ingest lane every venue event rides;
//!   * a venue-HELD (live/filled) order re-queries to `OrderAccepted`, NEVER a phantom reject (the
//!     dangerous case: a synthesized reject on a real position); only a venue-confirmed-absent order
//!     becomes the true terminal `OrderRejected` — the exact `resolve_ambiguous_submit` mapping the
//!     real bybit/okx confirm closures reuse.
//!
//! Uses the fake-`ExecActor` pattern from `exec_actor_dead_thread.rs`: a real `ExecActor` with a
//! trivial command loop, driven with canned re-query closures — no network, no real orders.

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::exec_actor::{ConfirmFn, ExecActor, ExecCommand};
use vike_bridge_core::resolve_ambiguous_submit;
use vike_exec::{event_channel, EventSender, ExecutionClient, Ingest};
use vike_model::events::{Event, FillEvent, OrderFilled, TradeId};

/// A command loop shaped like a real venue's: drain until `Shutdown` so teardown joins cleanly.
/// `confirm` never routes through here (the actor runs the re-query on its own worker thread), so
/// this loop only exists to keep the actor's command thread alive + tear down deterministically.
fn drain_until_shutdown(rx: std::sync::mpsc::Receiver<ExecCommand>) {
    while let Ok(cmd) = rx.recv() {
        if matches!(cmd, ExecCommand::Shutdown) {
            break;
        }
    }
}

/// Spawn a fake ExecActor with an optional confirm re-query closure.
fn actor(events: EventSender, confirm: Option<ConfirmFn>) -> ExecActor {
    let a = ExecActor::spawn("confirm-fake", events, drain_until_shutdown);
    match confirm {
        Some(c) => a.with_confirm(c),
        None => a,
    }
}

/// Block up to 10s for one `Ingest::Event` (mirrors `exec_actor_dead_thread.rs::recv_event`).
fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .expect("timed out waiting for a confirm event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

/// Assert NOTHING lands on the lane within a short window (used for the no-op case).
fn assert_silent(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let got =
        rt.block_on(async { tokio::time::timeout(Duration::from_millis(300), rx.recv()).await });
    assert!(got.is_err(), "expected NO event, but the lane produced one: {got:?}");
}

#[test]
fn confirm_with_no_requery_wired_is_a_clean_noop() {
    let (events, mut rx) = event_channel(16);
    let mut client = actor(events, None); // no `with_confirm` → default no-op
    client.confirm("c1");
    assert_silent(&mut rx); // the watchdog's stage-2 reject is what backstops such a venue
}

#[test]
fn confirm_of_a_venue_held_or_filled_order_emits_accepted_never_a_phantom_reject() {
    // The venue HAS the order (live, or already filled) → the re-query returns its venue id. This is
    // the DANGEROUS case: a phantom reject here would strand a real position. `resolve_ambiguous_submit`
    // maps it to a managed OrderAccepted (the fill, if any, follows on user-data) — the exact mapping
    // the real bybit/okx confirm closures use.
    let (events, mut rx) = event_channel(16);
    let confirm: ConfirmFn = Arc::new(|coid: &str| {
        vec![resolve_ambiguous_submit(coid, 7, Ok(Some("venue-42".to_string())))]
    });
    let mut client = actor(events, Some(confirm));
    client.confirm("c1");
    match recv_event(&mut rx) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.client_order_id, "c1");
            assert_eq!(a.venue_order_id.as_deref(), Some("venue-42"));
        }
        other => panic!("a venue-held order must confirm to OrderAccepted, got {other:?}"),
    }
}

#[test]
fn confirm_of_an_absent_order_emits_the_true_terminal_reject() {
    // The venue confirms the order never landed → the ONE case that legitimately becomes a terminal
    // OrderRejected (the wedged-adapter order really is gone).
    let (events, mut rx) = event_channel(16);
    let confirm: ConfirmFn =
        Arc::new(|coid: &str| vec![resolve_ambiguous_submit(coid, 9, Ok(None))]);
    let mut client = actor(events, Some(confirm));
    client.confirm("gone-1");
    match recv_event(&mut rx) {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "gone-1");
            assert!(r.reason.contains("venue confirms order absent"), "reason: {}", r.reason);
        }
        other => panic!("an absent order must confirm to OrderRejected, got {other:?}"),
    }
}

#[test]
fn confirm_forwards_a_filled_terminal_from_the_requery() {
    // A venue whose status re-query surfaces the terminal fill directly: the actor forwards WHATEVER
    // the re-query concludes onto the ingest lane verbatim — so an order the venue actually filled
    // yields the real OrderFilled terminal, never a synthesized reject.
    let (events, mut rx) = event_channel(16);
    let confirm: ConfirmFn = Arc::new(|coid: &str| {
        let fill = FillEvent {
            trade_id: TradeId::prefixed("fill-", coid),
            client_order_id: coid.to_string(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            ts: 3,
            mark_price: Some(100.0),
            position_side: "BOTH".into(),
        };
        vec![Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts: 3 })]
    });
    let mut client = actor(events, Some(confirm));
    client.confirm("filled-1");
    match recv_event(&mut rx) {
        Event::OrderFilled(w) => {
            assert_eq!(w.client_order_id, "filled-1");
            assert_eq!(w.fill.trade_id, "fill-filled-1");
        }
        other => panic!("a filled order's re-query must forward OrderFilled, got {other:?}"),
    }
}

#[test]
fn dropping_a_confirm_actor_tears_down_deterministically() {
    // The confirm worker + command thread must both join on Drop — a hang here means teardown is
    // broken (a `cargo test` timeout would fail this).
    let (events, _rx) = event_channel(16);
    let confirm: ConfirmFn =
        Arc::new(|coid: &str| vec![resolve_ambiguous_submit(coid, 1, Ok(None))]);
    let mut client = actor(events, Some(confirm));
    client.confirm("c1");
    drop(client); // must return promptly
}
