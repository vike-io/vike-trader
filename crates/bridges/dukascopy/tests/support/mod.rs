//! The fake-sidecar kit the dukascopy integration suites share: the fake's path, the configuration
//! a client is started with, the EURUSD orders they send (built on `vike_dukascopy::test_support`'s
//! `order`), the engine that holds one, and the quoting spawn. The `sidecar` binary (`tests/sidecar.rs`) declares it ONCE, `mod support;`, and each
//! suite reads it as `crate::support`.

// A part no suite of the group calls stays compiled for the next one.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use vike_dukascopy::quotes::QuoteBus;
use vike_dukascopy::test_support::order;
use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, Ingest, RiskGate, event_channel};
use vike_model::events::Event;
use vike_model::{ManagedOrder, OrderRequest, RiskLimits};

pub const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

pub fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
        ledger_mount: None,
    }
}

/// A market order the netting fake fills at `px` (it reads `price` as the fill price).
pub fn priced_order(coid: &str, side: i32, qty: f64, px: f64, ts: i64) -> OrderRequest {
    OrderRequest { side, qty, price: Some(px), ts, ..order(coid) }
}

/// A native stop-loss on a long EURUSD position: a reduce-only SELL stop.
pub fn resting_stop(coid: &str) -> OrderRequest {
    OrderRequest {
        order_type: vike_model::OrderType::Stop,
        side: -1,
        trigger_price: Some(1.09),
        reduce_only: true,
        ..order(coid)
    }
}

/// The node's real order FSM holding `req` as a registered order, as the engine of a dukascopy account would.
pub fn engine_holding(req: &OrderRequest) -> ExecutionEngine<RecordingClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "dukascopy", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "dukascopy",
        "EURUSD",
    );
    engine.registry.insert(req.client_order_id.clone(), ManagedOrder::new(req.clone()));
    engine
}

/// A client on the fake started in `mode` (none when empty), publishing onto `bus`. Nothing reads its
/// ingest lane: a tick is not an `Event`, and the fake sends no other.
pub fn spawn(mode: &str, bus: &Arc<QuoteBus>) -> DukascopyExecutionClient {
    let (events, _rx) = event_channel(64);
    let args: Vec<String> = if mode.is_empty() { vec![] } else { vec![mode.to_string()] };
    DukascopyExecutionClient::spawn_with_program_quoting(
        BRIDGE,
        &args,
        &config(),
        events,
        Arc::clone(bus),
    )
    .expect("the fake sidecar says ready")
}

/// The pids the fake wrote into its launch log, one per launch.
pub fn launches(log: &std::path::Path) -> Vec<u32> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.parse().expect("pid line"))
        .collect()
}

/// Every event that is on the lane right now (an exec client's lane carries nothing but events).
pub fn drain(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Vec<Event> {
    vike_bridge_core::exec_drive::drain_ready(|| rx.try_recv().ok())
}

/// Wait up to 20 s for `ready` (the restart and adopt suites' bound); the failure names `what`.
pub fn wait_until(what: &str, ready: impl FnMut() -> bool) {
    vike_log::poll::wait_for(Duration::from_secs(20), what, ready);
}
