//! Netting-truth end-to-end gate (law A7): the fake bridge's `netting` mode replays the Java
//! sidecar's position-per-order behavior — including the authoritative `position` lines — and
//! the exec client's re-anchor legs must land the core `Account` on EXACTLY the venue's
//! realized/remaining-basis attribution (see `src/netting.rs` module doc for the law).
//!
//! The brief's A/B scenario, in units of 1000 (venue minimum): short 1000@100 (A) +
//! short 1000@110 (B) → blended avg 105; buy 1000@108 nets against A in candidate order.
//! Venue truth: realized (100−108)·1000 = −8000, remainder short 1000@110. The blended fold
//! alone books −3000 and keeps 105 — the re-anchor legs must contribute the −5000 attribution
//! difference and re-base the remainder at 110. A final buy 1000@112 proves convergence: both
//! attributions agree on the last close (−2000), no further legs.

use std::time::Duration;

use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient};
use vike_exec::{event_channel, Account, BalanceMode, ExecutionClient, Ingest};
use vike_model::events::Event;
use vike_model::OrderRequest;

const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
    }
}

/// A market order the netting fake fills at `px` (it reads `price` as the fill price).
fn order(coid: &str, side: i32, qty: f64, px: f64, ts: i64) -> OrderRequest {
    OrderRequest {
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side,
        qty,
        order_type: "market".into(),
        price: Some(px),
        trigger_price: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .expect("timed out waiting for event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

/// Drain `n` events, folding every bare `Event::Fill` into `account` (exactly what the core's
/// engine does with the bare fill lane) and returning the drained events for shape asserts.
fn drain(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    account: &mut Account,
    n: usize,
) -> Vec<Event> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let ev = recv_event(rx);
        if let Event::Fill(fill) = &ev {
            account.apply_fill(fill);
        }
        out.push(ev);
    }
    out
}

fn position(account: &Account) -> (f64, f64) {
    let key: vike_exec::PositionKey = ("dukascopy".into(), "EURUSD".into(), "BOTH".into());
    account.positions.get(&key).map(|p| (p.size, p.avg_px)).unwrap_or((0.0, 0.0))
}

#[test]
fn netted_close_reanchors_account_to_venue_attribution() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["netting".into()],
        &config(),
        events,
    )
    .expect("ready");
    let mut account = Account::new(1.0, "dukascopy", None, BalanceMode::Delta);

    // 1) Short 1000 @ 100 (order A): Submitted, Accepted, bare Fill, OrderFilled — the venue
    //    position line (−1000 @ 100) matches the blend, so NO synthesized legs follow.
    client.submit(&order("c1", -1, 1000.0, 100.0, 1));
    let evs = drain(&mut rx, &mut account, 4);
    assert!(matches!(&evs[3], Event::OrderFilled(e) if e.client_order_id == "c1"), "{evs:?}");
    assert_eq!(position(&account), (-1000.0, 100.0));

    // 2) Short 1000 @ 110 (order B): blended avg (105) still equals the venue's signed-weighted
    //    avg of two WHOLE orders — in sync, no legs.
    client.submit(&order("c2", -1, 1000.0, 110.0, 2));
    let evs = drain(&mut rx, &mut account, 4);
    assert!(matches!(&evs[3], Event::OrderFilled(e) if e.client_order_id == "c2"), "{evs:?}");
    assert_eq!(position(&account), (-2000.0, 105.0));

    // 3) Buy 1000 @ 108 → netted close of A. The blend books (108−105)·(−1000) = −3000 and
    //    keeps 105; the venue realized (100−108)·1000 = −8000 and keeps the remainder at 110.
    //    The position line (−1000 @ 110) triggers the re-anchor: close 1000 @ 110 (realizing
    //    the −5000 attribution difference) + reopen short 1000 @ 110 — SIX events total.
    client.submit(&order("c3", 1, 1000.0, 108.0, 3));
    let evs = drain(&mut rx, &mut account, 6);
    assert!(matches!(&evs[3], Event::OrderFilled(e) if e.client_order_id == "c3"), "{evs:?}");
    let (Event::Fill(close), Event::Fill(reopen)) = (&evs[4], &evs[5]) else {
        panic!("expected two bare re-anchor fills after the wrap, got {evs:?}");
    };
    assert!(close.trade_id.starts_with("NETRA-EURUSD:"), "{}", close.trade_id);
    assert!(reopen.trade_id.starts_with("NETRA-EURUSD:"), "{}", reopen.trade_id);
    assert_ne!(close.trade_id, reopen.trade_id);
    assert_eq!((close.side, close.last_qty, close.last_px), (1, 1000.0, 110.0));
    assert_eq!((reopen.side, reopen.last_qty, reopen.last_px), (-1, 1000.0, 110.0));
    // EXACT venue attribution on the Account: realized −8000 as [−3000 blend, −5000 re-anchor],
    // remainder short 1000 re-based at B's own entry 110.
    assert_eq!(account.closed_pnls, vec![-3000.0, -5000.0]);
    assert_eq!(account.realized_pnl, -8000.0);
    assert_eq!(position(&account), (-1000.0, 110.0));

    // 4) Buy 1000 @ 112 closes the remainder. Blend and venue agree on a FULL close
    //    ((110−112)·1000 = −2000) — flat position line, NO further legs.
    client.submit(&order("c4", 1, 1000.0, 112.0, 4));
    let evs = drain(&mut rx, &mut account, 4);
    assert!(matches!(&evs[3], Event::OrderFilled(e) if e.client_order_id == "c4"), "{evs:?}");
    assert_eq!(account.closed_pnls, vec![-3000.0, -5000.0, -2000.0]);
    assert_eq!(account.realized_pnl, -10000.0);
    assert_eq!(position(&account), (0.0, 0.0));

    client.detach();
}

/// Multi-leg netting: one buy sweeps BOTH shorts (partial + terminal legs under one coid) and
/// opens a remainder. The re-anchor fires MID-PLAN — after leg 1 the venue's remaining basis is
/// B's own 110 while the blend still holds 105 — and from then on every leg realizes exactly the
/// venue's per-order attribution, so the running `closed_pnls` match the venue leg-for-leg.
#[test]
fn full_sweep_with_remainder_reanchors_mid_plan() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["netting".into()],
        &config(),
        events,
    )
    .expect("ready");
    let mut account = Account::new(1.0, "dukascopy", None, BalanceMode::Delta);

    client.submit(&order("c1", -1, 1000.0, 100.0, 1));
    drain(&mut rx, &mut account, 4);
    client.submit(&order("c2", -1, 1000.0, 110.0, 2));
    drain(&mut rx, &mut account, 4);

    // Buy 3000 @ 108: leg 1 closes A (partial wrap; venue line −1000@110 → re-anchor pair),
    // leg 2 closes B (partial wrap; flat line, in sync), remainder opens long 1000 (terminal
    // wrap; line +1000@108, in sync) — Submitted + Accepted + 3 × (bare fill + wrap) + 2
    // re-anchor legs = 10 events.
    client.submit(&order("c5", 1, 3000.0, 108.0, 3));
    let evs = drain(&mut rx, &mut account, 10);
    assert!(
        matches!(&evs[3], Event::OrderPartiallyFilled(e) if e.client_order_id == "c5"),
        "{evs:?}"
    );
    assert!(
        matches!((&evs[4], &evs[5]), (Event::Fill(c), Event::Fill(o))
            if c.trade_id.starts_with("NETRA-") && o.trade_id.starts_with("NETRA-")),
        "{evs:?}"
    );
    assert!(
        matches!(&evs[7], Event::OrderPartiallyFilled(e) if e.client_order_id == "c5"),
        "{evs:?}"
    );
    assert!(matches!(&evs[9], Event::OrderFilled(e) if e.client_order_id == "c5"), "{evs:?}");
    // Per-leg venue attribution, exactly: leg A −8000 (as −3000 blend + −5000 re-anchor),
    // leg B +2000 ((110−108) short), remainder long 1000 opens at 108 on both sides.
    assert_eq!(account.closed_pnls, vec![-3000.0, -5000.0, 2000.0]);
    assert_eq!(account.realized_pnl, -6000.0);
    assert_eq!(position(&account), (1000.0, 108.0));

    // Nothing further pending: the next submit's Submitted event must be the NEXT thing on the
    // lane (i.e. no stray re-anchor legs were queued behind the sweep).
    client.submit(&order("c6", -1, 1000.0, 109.0, 4));
    let evs = drain(&mut rx, &mut account, 4);
    assert!(matches!(&evs[0], Event::OrderSubmitted(e) if e.client_order_id == "c6"), "{evs:?}");
    assert!(matches!(&evs[3], Event::OrderFilled(e) if e.client_order_id == "c6"), "{evs:?}");
    assert_eq!(account.realized_pnl, -5000.0); // −6000 + (109−108)·1000
    assert_eq!(position(&account), (0.0, 0.0));

    client.detach();
}
