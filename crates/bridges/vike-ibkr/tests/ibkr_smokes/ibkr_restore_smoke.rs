//! Restore-into-registry LIVE PAPER smoke (the proof `vike_venues::venue_restore`'s ibkr row waits for): an order a
//! PREVIOUS connection of the client left resting is found by the real `IbkrSocketReconClient`, planned as ADOPT by
//! `vike_exec::recon::plan_restore`, put into a FRESH `ExecutionEngine` with `reregister_orders`, raises nothing in
//! the diff, and is then cancelled THROUGH that engine, by a new connection that learned it from the venue. On the way
//! it proves the daily rebuild: a third connection, which only the bridge's own memory taught, holds the order too.
//!
//! ⚠ It PLACES A REAL PAPER ORDER (a limit BUY of 1 AAPL at 60 percent of the delayed ask, which cannot fill) under a
//! client id of its own; never run it casually: the lane, the credential store and the fence are
//! `docs/ops/live-smoke-lane.md`'s, and no workflow runs it. On a the CI box lane (the gateway is on the box):
//!
//!     just the CI box <branch> env VIKE_SETTINGS_DIR=/var/lib/vike/settings VIKE_NO_LIVE_KEYS=true \
//!       cargo test -p vike-ibkr --features ibkr,test-support --test ibkr_smokes -- --ignored --nocapture ibkr_restore_smoke::
//!
//! Triple-gated like the other live tests: `#[ignore]`; self-skips unless a demo ibkr account on the `socket` backend
//! exists and the gateway answers; and a hard assert (`live_cfg`) refuses anything but a paper `DU...` account. A failed
//! assertion leaves the order on the paper account: cancel it in TWS, its `orderRef` starts with `vsrst`.
#![cfg(feature = "ibkr")]

use std::time::{Duration, Instant};

use crate::ibkr_untaught_replay_smoke::{live_cfg, reference_price, round2, todays_ids};
use tokio::sync::mpsc::Receiver;
use vike_bridge_core::smoke::{adopts, plan_pass, restored_limit, wait_until_gone};
use vike_exec::recon::{Divergence, diff};
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionClient, ExecutionEngine, Ingest, Outbox, RiskGate,
    event_channel,
};
use vike_ibkr::IbkrExecutionClient;
use vike_ibkr::config::IbkrConfig;
use vike_ibkr::recon::socket::IbkrSocketReconClient;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, RiskLimits, TimeInForce, now_ms};

const SYMBOL: &str = "AAPL.SMART.USD";

/// One connection of the client, its handover made with an empty ledger (the daemon that sent the order is gone).
fn connection(cfg: &IbkrConfig) -> (IbkrExecutionClient, Receiver<Ingest>) {
    let (events, rx) = event_channel(1024);
    let mut last = String::new();
    for _ in 0..8 {
        match IbkrExecutionClient::connect(cfg, events.clone()) {
            Ok(mut client) => {
                client.adopt_restored(&[]);
                return (client, rx);
            }
            // An id the gateway was only just told to release answers an error for a moment.
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    panic!("the exec connection never opened: {last}");
}

/// Hand the lane's events to `fold` until `done` says so, for at most a minute.
fn pump(
    rx: &mut Receiver<Ingest>,
    mut fold: impl FnMut(&Event),
    mut done: impl FnMut(&Ingest) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(ingest) => {
                if let Ingest::Event(ev) = &ingest {
                    fold(ev);
                }
                if done(&ingest) {
                    return;
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(40)),
        }
    }
    panic!("the wait on the exec lane ran out");
}

fn link_up(ingest: &Ingest) -> bool {
    matches!(ingest, Ingest::ExecLink(u) if u.up)
}

#[test]
#[ignore = "live paper: places and cancels a non-filling order on the IB Gateway under a client id of its own (see module doc)"]
fn ibkr_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some((mut cfg, addr)) = live_cfg() else { return };
    let [exec_id, observer_id] = todays_ids();
    cfg.client_id = exec_id;
    cfg.recon_client_id = exec_id + 2; // `todays_ids` strides by four, so this one is free too
    let (ask, source) = reference_price(&addr, observer_id).expect("a reference price");
    let price = round2(ask * 0.6);
    eprintln!("reference {ask} ({source}): a far limit at {price}");

    // --- 1. the "previous session": a far resting LIMIT BUY, and then the connection is gone -------
    let coid = format!("vsrst{}", now_ms() % 100_000_000);
    let (mut previous, mut rx) = connection(&cfg);
    pump(&mut rx, |_| {}, link_up);
    previous.submit(&OrderRequest {
        client_order_id: coid.clone(),
        venue: "ibkr".into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        time_in_force: TimeInForce::Gtc,
        ts: now_ms(),
        ..Default::default()
    });
    pump(
        &mut rx,
        |ev| assert!(!matches!(ev, Event::OrderRejected(_)), "paper refused the order: {ev:?}"),
        |i| matches!(i, Ingest::Event(Event::OrderAccepted(a)) if a.client_order_id == coid),
    );
    previous.detach();
    drop(previous);

    // --- 2. the restart: the real recon client, the planner ----------------------------------------
    let recon = IbkrSocketReconClient::connect(&cfg, SYMBOL).expect("the recon client connects");
    let restored = [restored_limit(&coid, "ibkr", SYMBOL)];
    let (plan, orders) = plan_pass(&recon, "ibkr", SYMBOL, &restored);
    assert!(adopts(&plan, &coid), "the resting coid must be ADOPTED: {plan:?}");
    assert!(plan.filled_while_down.is_empty(), "nothing filled: {plan:?}");

    // --- 3. a FRESH engine over a new connection, taught as the restore teaches it -----------------
    let (client, mut rx) = connection(&cfg);
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "ibkr",
        SYMBOL,
    );
    engine.client.adopt_restored(&[(coid.clone(), SYMBOL.to_string())]);
    assert_eq!(engine.reregister_orders(&plan.adopt), 1, "one order re-registered");
    let adopted = &engine.registry[&coid];
    assert!(adopted.status.is_live(), "restored live: {:?}", adopted.status);
    assert!(adopted.venue_order_id.is_some(), "the venue order id came across");
    assert_eq!((adopted.request.side, adopted.request.qty), (1, 1.0), "the venue's terms");
    let local = engine.local_view();
    let held: Vec<_> = diff(&orders, &[], &[], &local.as_view(), None)
        .into_iter()
        .filter(|d| {
            matches!(d, Divergence::UnknownOrder(o) if o.client_order_id.as_deref() == Some(coid.as_str()))
        })
        .collect();
    assert!(held.is_empty(), "an adopted order raises no UnknownOrder: {held:?}");
    let mut outbox = Outbox::default();
    pump(
        &mut rx,
        |ev| {
            engine.on_event(ev, &mut outbox);
        },
        link_up,
    );
    engine.client.detach();

    // --- 4. the daily rebuild: a new connection nobody teaches, and the engine's own cancel --------
    let (rebuilt, mut rx) = connection(&cfg);
    let old = std::mem::replace(&mut engine.client, rebuilt);
    drop(old);
    pump(
        &mut rx,
        |ev| {
            engine.on_event(ev, &mut outbox);
        },
        link_up,
    );
    assert!(engine.cancel_order(&coid), "the registry holds it, so the engine sends the cancel");
    pump(
        &mut rx,
        |ev| {
            assert!(!matches!(ev, Event::OrderCancelRejected(_)), "cancel refused: {ev:?}");
            engine.on_event(ev, &mut outbox);
        },
        |i| matches!(i, Ingest::Event(Event::OrderCanceled(c)) if c.client_order_id == coid),
    );
    assert_eq!(engine.registry[&coid].status, OrderStatus::Canceled, "the fold ends it");
    wait_until_gone(&recon, "ibkr", SYMBOL, &restored, &coid);

    engine.client.detach();
    eprintln!(
        "restore smoke green: placed -> adopted -> re-registered -> rebuilt -> engine cancel -> gone at venue -> folded Canceled"
    );
}
