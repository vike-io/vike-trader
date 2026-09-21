//! Runtime wiring tests for the dead-man's switch (auto cancel-on-disconnect) — the on-`CoreThread`
//! half of the feature (the pure trip logic is unit-tested in [`super::deadman`]). Drives a
//! synchronous `CoreThread` (no OS thread) with a `RecordingClient` so we can observe exactly which
//! cancels the trip issued, feeding the freshness clock and the boundary sweep by hand (injected
//! timestamps — zero wall-clock sleeps). Covers: (a) OFF-by-default never trips, (b/c) a trip after
//! the timeout cancels every open order, (d) recovery re-arms for a second outage, and (e)
//! `CancelAllAndHalt` engages the HALT sentinel + the in-process halt.

use super::*;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, RiskGate, RiskLimits, TradingState};
use vike_model::OrderRequest;

/// Build a synchronous `CoreThread<RecordingClient>` with the given `CoreConfig` (mirrors
/// `apply.rs`'s `test_core`, but lets the caller pre-set `config.deadman`).
fn core_with(config: CoreConfig) -> CoreThread<RecordingClient> {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&engine.venue, &engine.symbol)));
    assemble_core(engine, Vec::new(), config, market, snapshot, Arc::new(AtomicU64::new(0)))
}

fn deadman_cfg(
    ms: u64,
    action: DeadManAction,
    halt_file: Option<std::path::PathBuf>,
) -> CoreConfig {
    CoreConfig {
        deadman: Some(DeadManConfig {
            timeout: std::time::Duration::from_millis(ms),
            action,
            halt_file,
        }),
        ..Default::default()
    }
}

/// Submit a resting market order under `coid` through the ONE order-write path; it registers live
/// (RecordingClient never acks, so it stays in a non-terminal, cancelable state).
fn submit(c: &mut CoreThread<RecordingClient>, coid: &str) {
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "market".into(),
            ..Default::default()
        })),
        0,
    );
}

// (a) OFF by default: no config ⇒ no state machine, and a sweep at any time is a complete no-op.
#[test]
fn disabled_by_default_never_trips() {
    let mut c = core_with(CoreConfig::default());
    assert!(c.deadman.is_none(), "default config builds no dead-man state machine");
    submit(&mut c, "c1");
    // even called directly, the sweep does nothing when the feature is off
    c.sweep_deadman(1_000_000_000);
    assert!(c.engine.client.cancels.is_empty(), "disabled switch must never cancel anything");
    assert_eq!(c.engine.trading_state, TradingState::Active, "disabled switch must never halt");
}

// (b)+(c) trips after the timeout of no data and cancels ALL open orders. CancelAll leaves trading
// state untouched.
#[test]
fn trips_after_timeout_and_cancels_all_open_orders() {
    let mut c = core_with(deadman_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, "c1");
    submit(&mut c, "c2");
    c.deadman.as_mut().unwrap().observe(0); // one live data frame at t=0

    // still fresh — no trip, no cancels
    c.sweep_deadman(500);
    assert!(c.engine.client.cancels.is_empty(), "fresh feed must not trip");

    // silent past the timeout — TRIP: every resting order is cancelled
    c.sweep_deadman(2000);
    let mut cancelled = c.engine.client.cancels.clone();
    cancelled.sort();
    assert_eq!(cancelled, vec!["c1".to_string(), "c2".to_string()], "trip cancels all open orders");
    assert_eq!(c.engine.trading_state, TradingState::Active, "CancelAll does not engage HALT");

    // trips exactly once per outage — a later still-silent tick issues no further cancels
    c.sweep_deadman(3000);
    assert_eq!(c.engine.client.cancels.len(), 2, "no re-fire within the same outage");
}

// (d) recovery re-arms: a fresh frame clears the latch, and a SECOND outage trips again.
#[test]
fn recovery_re_arms_for_a_second_outage() {
    let mut c = core_with(deadman_cfg(1000, DeadManAction::CancelAll, None));
    submit(&mut c, "c1");
    c.deadman.as_mut().unwrap().observe(0);

    c.sweep_deadman(2000); // first outage trips
    assert_eq!(c.engine.client.cancels.len(), 1, "first outage cancels the resting order");

    // fresh data resumes → recovery re-arms (no trip on the fresh tick)
    c.deadman.as_mut().unwrap().observe(3000);
    c.sweep_deadman(3000);
    assert_eq!(c.engine.client.cancels.len(), 1, "recovery tick issues no cancel");

    // a NEW outage after recovery trips again (the order is still live — RecordingClient never
    // terminalized it — so it is cancelled a second time)
    c.sweep_deadman(4001);
    assert_eq!(c.engine.client.cancels.len(), 2, "second outage trips again after re-arm");
}

// (e) CancelAllAndHalt cancels AND engages HALT: the in-process trading_state flips to Halted on the
// engine AND the cross-process HALT sentinel file is written.
#[test]
fn cancel_all_and_halt_engages_the_sentinel() {
    let path = std::env::temp_dir().join(format!(
        "vike_deadman_halt_{}_{}.sentinel",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let _ = std::fs::remove_file(&path); // ensure a clean slate
    assert!(!path.exists());

    let mut c = core_with(deadman_cfg(1000, DeadManAction::CancelAllAndHalt, Some(path.clone())));
    submit(&mut c, "c1");
    c.deadman.as_mut().unwrap().observe(0);

    c.sweep_deadman(2000); // TRIP
    assert_eq!(c.engine.client.cancels, vec!["c1".to_string()], "halt trip still cancels first");
    assert_eq!(c.engine.trading_state, TradingState::Halted, "in-process RiskGate halt engaged");
    assert!(path.exists(), "the cross-process HALT sentinel file was written");

    let _ = std::fs::remove_file(&path); // cleanup
}
