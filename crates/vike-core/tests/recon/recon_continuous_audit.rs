//! Task 16: continuous runtime audits — the interval-driven reconcile cadence. Mirrors the
//! Task 13 `CountingReconClient` harness (`recon_manager.rs`) and the Task 15 health-gate harness
//! (`recon_audit_gating.rs`): a real single-writer core + one engine, reconciled through the
//! `ReconClient` seam.
//!
//! THE KEY TEST (`interval_cadence_runs_second_pass_idempotently`): with `interval = Some(short)`,
//! the SAME external fill is fetched twice — once by the startup pass, once by the interval tick.
//! `vike_exec::recon::diff` dedups fills by `trade_id` against the local engine's `seen_trade_ids`
//! (`crates/vike-exec/src/recon/diff.rs`), so the second pass must diff to zero divergences: no
//! double-applied fill (position stays 1.0, not 2.0) and no growth of the recent-events ring
//! (`CoreSnapshot::recent_events` — `reconcile_reports` only pushes/logs when `n_events > 0` or
//! alerts are non-empty). This proves re-running a reconcile pass on a live cadence is safe.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_core::{spawn_core, spawn_recon, CoreConfig, ReconConfig};
use vike_exec::recon::{ReconClient, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionClient, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::events::LiquiditySide;
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

fn ext_fill() -> FillReport {
    FillReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        trade_id: "t1".into(),
        venue_order_id: "v9".into(),
        client_order_id: None, // external order — no local coid
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 5,
    }
}

/// Counts `fetch_order_status_reports` calls (one per pass, same convention as the Task 13/15
/// counting harnesses) and always returns the SAME single external fill — the fixed report set
/// that makes the second pass a genuine re-diff against already-folded local state, not a new
/// divergence.
struct CountingFillClient {
    calls: Arc<AtomicU64>,
    fill: FillReport,
}

impl ReconClient for CountingFillClient {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(vec![self.fill.clone()])
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(Vec::new())
    }
}

#[test]
fn interval_cadence_runs_second_pass_idempotently() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    let calls = Arc::new(AtomicU64::new(0));
    let client = CountingFillClient { calls: calls.clone(), fill: ext_fill() };
    let recon_cfg = ReconConfig {
        policy: ReconPolicy::default(),
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        interval: Some(Duration::from_millis(40)),
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    // Pass #1 (startup): wait for the external fill to land in the published position.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snap = cell.load_full();
        if snap
            .portfolio
            .venues
            .first()
            .and_then(|vb| vb.positions.iter().find(|p| p.symbol == "BTCUSDT"))
            .is_some()
        {
            break;
        }
        assert!(Instant::now() < deadline, "startup pass never folded the external fill");
        std::thread::sleep(Duration::from_millis(5));
    }
    let snap1 = cell.load_full();
    let pos1 =
        snap1.portfolio.venues[0].positions.iter().find(|p| p.symbol == "BTCUSDT").unwrap().size;
    assert!((pos1 - 1.0).abs() < 1e-12, "expected position 1.0 after pass #1, got {pos1}");
    let events_after_pass1 = snap1.recent_events.len();

    // Pass #2: prove the interval cadence actually fires a SECOND pass on its own, with no
    // trigger poke and no further help from the test.
    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Relaxed) < 2 {
        assert!(Instant::now() < deadline, "interval cadence never ran a second pass");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Give the fold thread a moment to drain pass #2's enqueued ReconcileReports command.
    std::thread::sleep(Duration::from_millis(100));

    // Idempotency: re-fetching the SAME already-folded fill must diff to zero divergences — no
    // double-applied fill, and no growth of the recent-events ring (which only grows when the
    // pass folds >=1 event or raises an alert).
    let snap2 = cell.load_full();
    let pos2 =
        snap2.portfolio.venues[0].positions.iter().find(|p| p.symbol == "BTCUSDT").unwrap().size;
    assert!(
        (pos2 - 1.0).abs() < 1e-12,
        "pass #2 must not double-apply the already-seen fill (dedup by trade_id), got {pos2}"
    );
    assert_eq!(
        snap2.recent_events.len(),
        events_after_pass1,
        "idempotent pass must not grow the recent-events ring"
    );

    driver.shutdown();
    handle.shutdown_and_join();
}

/// `interval: None` must be BYTE-IDENTICAL to pre-Task-16 behavior: no extra pass ever runs
/// beyond startup (no trigger poke sent here either). Regression guard alongside the unchanged
/// Task 13/15 tests in `recon_manager.rs`/`recon_audit_gating.rs`.
#[test]
fn absent_interval_never_runs_a_second_pass() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);

    let calls = Arc::new(AtomicU64::new(0));
    let client = CountingFillClient { calls: calls.clone(), fill: ext_fill() };
    let recon_cfg = ReconConfig {
        policy: ReconPolicy::default(),
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        interval: None, // back-compat
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Relaxed) < 1 {
        assert!(Instant::now() < deadline, "startup pass never ran");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Well past what would have been several interval ticks were `interval` wired — count must
    // stay pinned at 1.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(calls.load(Ordering::Relaxed), 1, "interval: None must never run a second pass");

    driver.shutdown();
    handle.shutdown_and_join();
}

/// `audit_interval` in-flight-timeout ticks are DELEGATED (they poke the core's existing
/// `Ingest::Watchdog` waker rather than reimplementing a sweep — see `ReconManager::run_audit_tick`
/// doc). This is a smoke test only: it proves the audit cadence coexists with the reconcile
/// interval cadence without disrupting it (no panic, no interference with pass counting) — the
/// PRIMARY deliverable is the interval reconcile cadence above; a poke into a config-less core is
/// intentionally inert (no `submit_ack_timeout` configured here), so there is nothing further to
/// assert on the fold side.
#[test]
fn audit_interval_coexists_with_reconcile_interval() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);

    let calls = Arc::new(AtomicU64::new(0));
    let client = CountingFillClient { calls: calls.clone(), fill: ext_fill() };
    let recon_cfg = ReconConfig {
        policy: ReconPolicy::default(),
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        interval: Some(Duration::from_millis(40)),
        audit_interval: Some(Duration::from_millis(15)),
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Relaxed) < 2 {
        assert!(
            Instant::now() < deadline,
            "reconcile interval pass never ran alongside audit_interval"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    driver.shutdown();
    handle.shutdown_and_join();
}
