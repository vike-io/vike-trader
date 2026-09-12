//! Task 15: probe/health fusion. A local network blip or feed gap must SUPPRESS a reconcile
//! pass (no fetch, no `Command::ReconcileReports`) rather than triggering a reconcile storm.
//! `ReconManager` takes an optional PER-VENUE health-probe closure (`Option<Arc<dyn Fn(&str) ->
//! ReconHealth ...>>`) so it stays layered under `vike-bridge-core` (which owns the REAL `StreamHealth`/
//! `ConnectivityProbe` types) — see `recon_manager.rs`'s module doc for the layering argument.
//! Mirrors the Task 13 `CountingReconClient` harness in `recon_manager.rs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, HealthProbe, ReconConfig, ReconHealth, spawn_core, spawn_recon};
use vike_exec::recon::ReconClient;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

type DynClient = Box<dyn ExecutionClient + Send>;

fn engine(venue: &str, symbol: &str, client: DynClient) -> ExecutionEngine<DynClient> {
    ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        venue,
        symbol,
    )
}

/// Counts fetch calls, same pattern as `recon_manager.rs`'s `CountingReconClient`.
struct CountingReconClient {
    calls: Arc<AtomicU64>,
}

impl ReconClient for CountingReconClient {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(Vec::new())
    }
}

/// 0 = Healthy, 1 = Degraded — flipped by the test to drive the probe closure. Venue-agnostic
/// (ignores the venue arg): these single-venue tests only flip one global flag.
fn health_flag_probe(flag: Arc<AtomicU8>) -> HealthProbe {
    Arc::new(move |_venue: &str| {
        if flag.load(Ordering::Relaxed) == 0 { ReconHealth::Healthy } else { ReconHealth::Degraded }
    })
}

#[test]
fn degraded_health_suppresses_trigger_pass_then_recovers() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);

    let calls = Arc::new(AtomicU64::new(0));
    let client = CountingReconClient { calls: calls.clone() };
    // Start Degraded so the STARTUP pass itself is suppressed too.
    let flag = Arc::new(AtomicU8::new(1));
    let recon_cfg = ReconConfig {
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        health: Some(health_flag_probe(flag.clone())),
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    // Give the startup pass time to (not) run.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(calls.load(Ordering::Relaxed), 0, "degraded startup pass must be suppressed");

    // A trigger poke while still Degraded must ALSO be suppressed — no fetch, no enqueue.
    driver.reconcile_trigger().send(()).expect("driver still listening");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(calls.load(Ordering::Relaxed), 0, "degraded trigger pass must be suppressed");

    // Recover: flip Healthy, then poke again — this pass must run.
    flag.store(0, Ordering::Relaxed);
    driver.reconcile_trigger().send(()).expect("driver still listening");
    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Relaxed) < 1 {
        assert!(Instant::now() < deadline, "trigger pass never ran after recovering to Healthy");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Exactly one pass — no extra passes sneak in.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::Relaxed), 1, "exactly one pass after recovery");

    driver.shutdown();
    handle.shutdown_and_join();
}

/// PER-VENUE gate (the fix): in ONE pass with two venues, a Degraded venue's leg is suppressed
/// while a Healthy venue still reconciles — the earlier binance-only gate would have either run
/// both or blocked both.
#[test]
fn degraded_one_venue_does_not_block_a_healthy_venue() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);

    let binance_calls = Arc::new(AtomicU64::new(0));
    let bybit_calls = Arc::new(AtomicU64::new(0));
    // Health probe: binance Healthy, bybit Degraded.
    let probe: HealthProbe =
        Arc::new(
            |venue: &str| {
                if venue == "bybit" { ReconHealth::Degraded } else { ReconHealth::Healthy }
            },
        );
    let recon_cfg = ReconConfig {
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        health: Some(probe),
        ..ReconConfig::default()
    };
    let driver = spawn_recon(
        &handle,
        vec![
            ("binance".to_string(), Box::new(CountingReconClient { calls: binance_calls.clone() })),
            ("bybit".to_string(), Box::new(CountingReconClient { calls: bybit_calls.clone() })),
        ],
        recon_cfg,
        None,
    );

    // binance's leg runs; bybit's is suppressed — in the SAME startup pass.
    let deadline = Instant::now() + Duration::from_secs(3);
    while binance_calls.load(Ordering::Relaxed) < 1 {
        assert!(Instant::now() < deadline, "healthy binance leg never reconciled");
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(binance_calls.load(Ordering::Relaxed), 1, "healthy venue reconciled once");
    assert_eq!(bybit_calls.load(Ordering::Relaxed), 0, "degraded venue's leg was suppressed");

    driver.shutdown();
    handle.shutdown_and_join();
}

#[test]
fn absent_health_probe_always_reconciles_backcompat() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);

    let calls = Arc::new(AtomicU64::new(0));
    let client = CountingReconClient { calls: calls.clone() };
    let recon_cfg = ReconConfig {
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        health: None, // back-compat: no probe wired = always Healthy
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Relaxed) < 1 {
        assert!(Instant::now() < deadline, "startup pass never ran with health: None");
        std::thread::sleep(Duration::from_millis(5));
    }

    driver.shutdown();
    handle.shutdown_and_join();
}
