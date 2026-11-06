//! **The daemon's stop, in its order and without a tax** (`TradeHub::shutdown`).
//!
//! The order is the contract: the reaper is stopped, every attached lease is told `Bye(ServerStopping)`, each
//! one's connection is waited for (up to its bound) and only then is its session retired; a resting order is
//! never cancelled (that is the client's own sweep). What a stop does NOT wait for: the reaper's tick (the stop
//! ends its wait) and a lease whose connection already ended (`server::trade`'s `EndLease` finished it).

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{AdapterReply, AdapterStart, TradeBye, TradeDown, TradeUp};
use vike_model::{AccountTier, HaltAdmit, OrderRequest};

use super::{OpenRequest, TradeHub};
use crate::budget::ledger::Budget;
use crate::testkit::trade::{ExecCall, ScriptedAdapter, ScriptedTrade};
use crate::testkit::trade_rig::hub_config;
use crate::trade::lease::{Flow, Lease, Next};

const WAIT: Duration = Duration::from_secs(5);

/// A stop with no live client owes only its own work: far below the reaper's tick (500 ms) and the lease
/// bound (3 s) it used to sit out.
const AT_ONCE: Duration = Duration::from_millis(250);

fn hub(lane: &ScriptedTrade) -> Arc<TradeHub> {
    TradeHub::new(Arc::new(lane.clone()), hub_config(), Arc::new(Budget::new()))
}

/// A lease with a running adapter and one order resting at it (the scripted venue answers nothing, so nothing
/// ends the order).
fn lease_with_a_resting_order(
    lane: &ScriptedTrade,
    hub: &TradeHub,
) -> (Arc<Lease>, ScriptedAdapter) {
    let lease = hub
        .open(OpenRequest {
            client_id: "tradehub-1".to_string(),
            venue: "binance".to_string(),
            account: None,
            tier: AccountTier::Demo,
            resume: None,
            takeover: false,
            key_id: Some("nk-test".to_string()),
            closer: Arc::new(|| {}),
        })
        .unwrap_or_else(|e| panic!("the open is accepted: {e:?}"))
        .lease;
    let start = AdapterStart {
        rid: 7,
        symbol: "BTCUSDT".to_string(),
        declared_legs: Vec::new(),
        recon_enabled: true,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: HaltAdmit::Admit,
        withhold: None,
        account_leverage: None,
        read_account_ccy: false,
        route_key: "binance".to_string(),
    };
    assert_eq!(lease.handle(TradeUp::AdapterStart(start)), Flow::Continue);
    match lease.next(WAIT) {
        Next::Frame(TradeDown::AdapterReply { rid: 7, reply }) => {
            assert!(matches!(*reply, AdapterReply::Live { .. }), "{reply:?}");
        }
        other => panic!("expected the adapter reply, got {other:?}"),
    }
    let order = OrderRequest {
        client_order_id: "resting-1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        price: Some(100.0),
        ts: 1_700_000_000_000,
        ..Default::default()
    };
    assert_eq!(lease.handle(TradeUp::Submit { cseq: 1, order }), Flow::Continue);
    let adapter = lane.adapter("binance", None).expect("the adapter was started");
    let deadline = Instant::now() + WAIT;
    while adapter.submitted().is_empty() {
        assert!(Instant::now() < deadline, "the order never reached the adapter");
        std::thread::sleep(Duration::from_millis(5));
    }
    (lease, adapter)
}

fn cancels(adapter: &ScriptedAdapter) -> Vec<ExecCall> {
    let calls = adapter.calls().into_iter();
    calls.filter(|c| matches!(c, ExecCall::Cancel { .. } | ExecCall::CancelBatch { .. })).collect()
}

/// **A client that is gone is not waited for, nor is the reaper's tick**: the connection finished the lease and
/// the reaper wakes on the stop instead of sleeping out its `REAPER_TICK`, so the stop tears the adapter down
/// at once, and the order resting at the venue stays there.
#[test]
fn a_hub_whose_client_is_gone_stops_at_once_and_cancels_no_resting_order() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (lease, adapter) = lease_with_a_resting_order(&lane, &hub);
    // What the connection's guard does when its reader leaves (`server::trade`'s `EndLease`).
    lease.close();
    lease.finish();
    let began = Instant::now();
    hub.shutdown();
    let took = began.elapsed();
    assert!(took < AT_ONCE, "the stop took {took:?} with no live client");
    assert!(adapter.is_detached(), "the stop tore the adapter down");
    assert_eq!(cancels(&adapter), Vec::<ExecCall>::new(), "the stop cancelled a resting order");
    assert_eq!(hub.sessions(), 0);
}

/// **A suite with no socket that ends its leases stops at once** (`TradeHub::end_attached_leases`, a rig's
/// cleanup): the lease it left attached is finished as a connection's guard would finish it, so the stop waits
/// for no goodbye, and the resting order stays.
#[test]
fn a_socketless_suite_that_ends_its_leases_stops_at_once() {
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (_lease, adapter) = lease_with_a_resting_order(&lane, &hub);
    hub.end_attached_leases();
    let began = Instant::now();
    hub.shutdown();
    let took = began.elapsed();
    assert!(took < AT_ONCE, "the stop took {took:?} after the suite ended its leases");
    assert!(adapter.is_detached(), "the stop tore the adapter down");
    assert_eq!(cancels(&adapter), Vec::<ExecCall>::new(), "the stop cancelled a resting order");
}

/// **The order of the stop.** A live client hears `Bye(ServerStopping)` while its adapter still runs, the stop
/// WAITS for that client's goodbye (it does not retire a session under a live client), and only then is the
/// adapter torn down; the resting order is not cancelled.
#[test]
fn the_stop_says_bye_first_waits_for_the_goodbye_then_retires_and_cancels_nothing() {
    const GOODBYE_AFTER: Duration = Duration::from_millis(300);
    let lane = ScriptedTrade::new(&["binance"]);
    let hub = hub(&lane);
    let (lease, adapter) = lease_with_a_resting_order(&lane, &hub);
    let connection = {
        let (lease, adapter) = (Arc::clone(&lease), adapter.clone());
        std::thread::Builder::new()
            .name("test-connection".into())
            .spawn(move || {
                // The writer's side: what is queued (the order's ack), then the closing Bye.
                let deadline = Instant::now() + WAIT;
                loop {
                    match lease.next(Duration::from_millis(50)) {
                        Next::Bye(TradeBye::ServerStopping) => break,
                        Next::Frame(_) | Next::Idle => {
                            assert!(Instant::now() < deadline, "the stop never said Bye");
                        }
                        other => panic!("expected Bye(ServerStopping), got {other:?}"),
                    }
                }
                let detached_at_bye = adapter.is_detached();
                std::thread::sleep(GOODBYE_AFTER);
                let detached_before_goodbye = adapter.is_detached();
                // The connection's guard: the reader left, the writer was joined.
                lease.close();
                lease.finish();
                (detached_at_bye, detached_before_goodbye)
            })
            .expect("spawn the connection")
    };
    hub.shutdown();
    let (detached_at_bye, detached_before_goodbye) = connection.join().expect("the connection");
    assert!(!detached_at_bye, "the session was retired before its client was told Bye");
    assert!(!detached_before_goodbye, "the stop retired the session under a live client");
    assert!(adapter.is_detached(), "the stop tore the adapter down after the goodbye");
    assert_eq!(cancels(&adapter), Vec::<ExecCall>::new(), "the stop cancelled a resting order");
    assert_eq!(hub.sessions(), 0);
}
