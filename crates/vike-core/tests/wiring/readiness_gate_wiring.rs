//! Portfolio-observer PR-4 T5 wiring gate: with `CoreConfig::readiness_gate` set, a REAL
//! `spawn_core` + `run()` loop — driven purely through the public `CoreHandle` message lanes,
//! never a direct `CoreThread::maintain_mount_readiness`/`drive_strategy_tick` call — starts a
//! mounted strategy `Pending` and flips it `Ready` only once the drain-loop boundary probe
//! (`CoreThread::run`'s `if self.config.readiness_gate { self.maintain_mount_readiness(now) }`
//! block in `crates/vike-core/src/runtime/mod.rs`) has re-checked the price board and found a
//! price.
//!
//! This closes the one untested gap the runtime-internal unit tests
//! (`runtime::tests::pending_mount_discards_submits_until_symbol_prices` /
//! `readiness_gate_off_is_byte_identical`) leave open: those call `maintain_mount_readiness` and
//! `drive_strategy_tick` directly on a bare `CoreThread`, so they would keep passing even if the
//! boundary block that wires the probe into the actual `run()` loop were deleted. This test drives
//! everything through `spawn_core`/`CoreHandle` instead — deleting that boundary block would leave
//! the mount `Pending` forever and this test would time out (fail).
//!
//! Mechanism under test, end to end: the strategy submits a market order on every quote tick it
//! sees (mirroring the unit tests' `AlwaysSubmitStrategy`, rewritten here against only the public
//! `vike_model`/`vike_core` surface). The FIRST quote both (a) sets the price board AND (b) fires
//! the strategy hook — but `drain_broker` gates on the mount's state as of the START of that same
//! dispatch, which is still `Pending` (the boundary probe has not re-run yet), so that first
//! submit is silently discarded even though the price is now on the board. Only at THIS message's
//! drain-loop boundary — after the dispatch, before the next `blocking_recv` — does
//! `maintain_mount_readiness` see the fresh price and flip the mount to `Ready`. A SECOND quote
//! (a "poke") then drains its submit to the engine's order registry, observable in
//! `CoreHandle::snapshot().orders`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_core::{spawn_core, CoreConfig, CoreHandle, LiveBroker, StrategyMount};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, QuoteUpdate, RiskGate, RiskLimits};
use vike_model::{Broker, QuoteTick, Strategy};

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";

/// Submits a 1-lot market buy on every quote tick — the mount whose readiness this test probes.
/// Rewritten against only the public surface from the runtime-internal `AlwaysSubmitStrategy`
/// unit-test helper (`runtime::tests`, not exported).
struct AlwaysSubmitStrategy {
    calls: Arc<AtomicUsize>,
}

impl Strategy<LiveBroker> for AlwaysSubmitStrategy {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        broker.submit_market(SYMBOL, 1, 1.0);
    }
}

fn quote_tick(ts: i64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: 99.0,
        ask: 101.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: SYMBOL.into(),
    }
}

fn send_quote(handle: &CoreHandle, ts: i64) {
    handle
        .tick_sender()
        .quote(QuoteUpdate { venue: VENUE.into(), symbol: SYMBOL.into(), quote: quote_tick(ts) })
        .expect("core alive");
}

/// Send one quote per poll (real cross-thread messaging — the core folds on its own thread) until
/// `cond` holds or `secs` elapses. Mirrors `state_save_timer_wiring.rs`'s `poke_until_saved` /
/// `vike-run/tests/mount_scripted.rs`'s `poke_until`: the core only re-checks its drain-loop
/// boundary (and therefore the readiness probe) when a new message arrives, so a silent wait would
/// never observe a boundary-driven flip.
fn poke_until(handle: &CoreHandle, secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut ts = 2;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        send_quote(handle, ts);
        ts += 1;
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

#[test]
fn readiness_gate_flips_via_real_run_loop_boundary_probe() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        readiness_gate: true,
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            interval: "1m".into(),
            strategy: Box::new(AlwaysSubmitStrategy { calls: Arc::clone(&calls) }),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);

    // The FIRST quote primes the price board AND fires the strategy hook, but the mount is still
    // `Pending` as of THIS dispatch (the boundary probe re-runs only after) -- its submit must be
    // discarded. Give the core a moment to fold + publish, then confirm nothing landed.
    send_quote(&handle, 1);
    std::thread::sleep(Duration::from_millis(200));
    assert!(calls.load(Ordering::Relaxed) >= 1, "sanity: the strategy hook must have run");
    assert_eq!(
        handle.snapshot().orders.len(),
        0,
        "the FIRST quote's submit must be discarded while the mount is still Pending"
    );

    // Pump further quotes -- real run() loop, real drain-loop boundary passes each time -- until
    // the boundary probe has flipped the mount Ready and a subsequent submit lands. If the
    // `if self.config.readiness_gate { self.maintain_mount_readiness(now) }` boundary block were
    // ever deleted, the mount would stay Pending forever and this call times out (fails).
    let landed = poke_until(&handle, 15, || !handle.snapshot().orders.is_empty());
    assert!(landed, "mount never flipped Ready via the real run() loop boundary probe (timed out)");

    let snap = handle.snapshot();
    assert!(
        snap.mounts.iter().any(|m| m.venue == VENUE && m.symbol == SYMBOL && m.ready),
        "snapshot must show the mount as ready once its submit has landed: {:?}",
        snap.mounts
    );

    handle.shutdown_and_join();
}

/// Negative control (byte-identical sanity, mirrors `runtime::tests::readiness_gate_off_is_byte_identical`
/// but through the real `run()` loop): with the gate OFF (the default), the very FIRST quote's
/// submit lands immediately -- no Pending suppression, no boundary probe needed.
#[test]
fn readiness_gate_off_submits_immediately_via_real_run_loop() {
    assert!(!CoreConfig::default().readiness_gate, "sanity: the gate defaults to off");
    let engine = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            interval: "1m".into(),
            strategy: Box::new(AlwaysSubmitStrategy { calls: Arc::clone(&calls) }),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);

    let landed = poke_until(&handle, 10, || !handle.snapshot().orders.is_empty());
    assert!(landed, "gate off: the very first submit must land without any pricing/pumping");
    assert!(calls.load(Ordering::Relaxed) >= 1, "sanity: the strategy hook must have run");

    handle.shutdown_and_join();
}
