//! Restore-into-registry LIVE PAPER smoke (the proof `vike_venues::venue_restore`'s ibkr row waits for): an order a
//! PREVIOUS connection of the client left resting is found by the real `ReconcileClientIbkrSocket`, planned as ADOPT by
//! `vike_execution::recon::plan_restore`, put into a FRESH `Engine` with `reregister_orders`, raises nothing in
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
//! exists and the gateway answers; and a hard assert (`live_config`) refuses anything but a paper `DU...` account. A failed
//! assertion leaves the order on the paper account: cancel it in TWS, its `orderRef` starts with `vsrst`.
#![cfg(feature = "ibkr")]

use std::time::{Duration, Instant};

use crate::ibkr_untaught_replay_smoke::{live_config, reference_price, round2, todays_ids};
use tokio::sync::mpsc::Receiver;
use vike_bridge_core::exec_actor::ExecutionClientActor;
use vike_bridge_core::smoke::{adopts, plan_pass, restored_limit, wait_until_gone};
use vike_execution::recon::{ReconcileDivergence, diff};
use vike_execution::{
    Account, BalanceMode, Engine, EventBusOutbox, EventHandler, ExecutionClient, Ingest, RiskGate,
    event_channel,
};
use vike_ibkr::config::IbkrConfig;
use vike_ibkr::connect_execution_client;
use vike_ibkr::reconcile::socket::ReconcileClientIbkrSocket;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, RiskLimits, TimeInForce, now_ms};

/// The user name (decision 0290); the box's kept ibkr row (`AAPL.SMART.USD`) is what the bridge sends.
const SYMBOL: &str = "AAPL";

/// One connection of the client, its handover made with an empty ledger (the daemon that sent the order is gone).
fn connection(config: &IbkrConfig) -> (ExecutionClientActor, Receiver<Ingest>) {
    let (events, rx) = event_channel(1024);
    let mut last = String::new();
    for _ in 0..8 {
        match connect_execution_client(config, events.clone()) {
            Ok((mut client, _)) => {
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
                if let Ingest::Event(event) = &ingest {
                    fold(event);
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
    matches!(ingest, Ingest::TradeLink(u) if u.up)
}

#[test]
#[ignore = "live paper: places and cancels a non-filling order on the IB Gateway under a client id of its own (see module doc)"]
fn ibkr_restore_adopts_a_resting_order_and_the_engine_cancels_it() {
    vike_log::test_init();
    let Some((mut config, addr)) = live_config() else { return };
    let [exec_id, observer_id] = todays_ids();
    config.client_id = exec_id;
    config.reconcile_client_id = exec_id + 2; // `todays_ids` strides by four, so this one is free too
    let (ask, source) = reference_price(&addr, observer_id).expect("a reference price");
    let price = round2(ask * 0.6);
    eprintln!("reference {ask} ({source}): a far limit at {price}");

    // --- 1. the "previous session": a far resting LIMIT BUY, and then the connection is gone -------
    let order_client_id = format!("vsrst{}", now_ms() % 100_000_000);
    let (mut previous, mut rx) = connection(&config);
    pump(&mut rx, |_| {}, link_up);
    previous.submit(&OrderRequest {
        order_client_id: order_client_id.clone(),
        venue: "ibkr".into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        time_in_force: TimeInForce::Gtc,
        timestamp: now_ms(),
        ..Default::default()
    });
    pump(
        &mut rx,
        |event| {
            assert!(!matches!(event, Event::OrderRejected(_)), "paper refused the order: {event:?}")
        },
        |i| matches!(i, Ingest::Event(Event::OrderAccepted(a)) if a.order_client_id == order_client_id),
    );
    previous.detach();
    drop(previous);

    // --- 2. the restart: the real recon client, the planner ----------------------------------------
    let recon =
        ReconcileClientIbkrSocket::connect(&config, SYMBOL).expect("the recon client connects");
    let restored = [restored_limit(&order_client_id, "ibkr", SYMBOL)];
    let (plan, orders) = plan_pass(&recon, "ibkr", SYMBOL, &restored);
    assert!(
        adopts(&plan, &order_client_id),
        "the resting order client id must be ADOPTED: {plan:?}"
    );
    assert!(plan.filled_while_down.is_empty(), "nothing filled: {plan:?}");

    // --- 3. a FRESH engine over a new connection, taught as the restore teaches it -----------------
    let (client, mut rx) = connection(&config);
    let mut engine = Engine::new(
        Account::new(1.0, "ibkr", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "ibkr",
        SYMBOL,
    );
    engine
        .client
        .adopt_restored(&[vike_execution::RestoredOrder::new(order_client_id.clone(), SYMBOL)]);
    assert_eq!(engine.reregister_orders(&plan.adopt), 1, "one order re-registered");
    let adopted = &engine.registry[&order_client_id];
    assert!(adopted.status.is_live(), "restored live: {:?}", adopted.status);
    assert!(adopted.venue_order_id.is_some(), "the venue order id came across");
    assert_eq!((adopted.request.side, adopted.request.qty), (1, 1.0), "the venue's terms");
    let local = engine.local_view();
    let held: Vec<_> = diff(&orders, &[], &[], &local.as_view(), None)
        .into_iter()
        .filter(|d| {
            matches!(d, ReconcileDivergence::UnknownOrder(o) if o.order_client_id.as_deref() == Some(order_client_id.as_str()))
        })
        .collect();
    assert!(held.is_empty(), "an adopted order raises no UnknownOrder: {held:?}");
    let mut outbox = EventBusOutbox::default();
    pump(
        &mut rx,
        |event| {
            engine.on_event(event, &mut outbox);
        },
        link_up,
    );
    engine.client.detach();

    // --- 4. the daily rebuild: a new connection nobody teaches, and the engine's own cancel --------
    let (rebuilt, mut rx) = connection(&config);
    let old = std::mem::replace(&mut engine.client, rebuilt);
    drop(old);
    pump(
        &mut rx,
        |event| {
            engine.on_event(event, &mut outbox);
        },
        link_up,
    );
    assert!(
        engine.cancel_order(&order_client_id),
        "the registry holds it, so the engine sends the cancel"
    );
    pump(
        &mut rx,
        |event| {
            assert!(!matches!(event, Event::OrderCancelRejected(_)), "cancel refused: {event:?}");
            engine.on_event(event, &mut outbox);
        },
        |i| matches!(i, Ingest::Event(Event::OrderCanceled(c)) if c.order_client_id == order_client_id),
    );
    assert_eq!(engine.registry[&order_client_id].status, OrderStatus::Canceled, "the fold ends it");
    wait_until_gone(&recon, "ibkr", SYMBOL, &restored, &order_client_id);

    engine.client.detach();
    eprintln!(
        "restore smoke green: placed -> adopted -> re-registered -> rebuilt -> engine cancel -> gone at venue -> folded Canceled"
    );
}
