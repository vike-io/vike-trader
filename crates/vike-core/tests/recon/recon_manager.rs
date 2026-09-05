//! `ReconManager` startup-cadence integration gate: a real single-writer core + one engine,
//! reconciled at startup through the `FakeReconClient` report seam. An external fill the local
//! engine never saw is fetched off-fold by the manager, enqueued as `Command::ReconcileReports`,
//! and folded on the core thread via `recon::diff`/`recon::resolve` — the synthesized fill lands
//! through the same `on_event` path real venue fills use, so the reconciled position appears in
//! the published `CoreSnapshot`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, ReconConfig, spawn_core, spawn_recon};
use vike_exec::recon::{FakeReconClient, ReconClient, ReconPolicy};
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

#[test]
fn startup_reconcile_folds_external_fill_into_position() {
    // A live core with one binance engine; the local engine has folded NOTHING.
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    // The venue reports an external fill (+1 BTCUSDT @ 100) the core never saw.
    let client = FakeReconClient { fills: vec![ext_fill()], ..Default::default() };
    let recon_cfg = ReconConfig {
        policy: ReconPolicy::default(), // Synthesize everything
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    // Poll the published snapshot until the reconciled position appears (avoids racing the
    // driver's own stop flag). The manager fetches off-fold, enqueues the reports, and the core
    // folds the synthesized fill.
    let deadline = Instant::now() + Duration::from_secs(3);
    let pos = loop {
        let snap = cell.load_full();
        if let Some(p) = snap
            .portfolio
            .venues
            .first()
            .and_then(|vb| vb.positions.iter().find(|p| p.symbol == "BTCUSDT"))
        {
            break p.size;
        }
        if Instant::now() > deadline {
            break f64::NAN;
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    assert!((pos - 1.0).abs() < 1e-12, "expected reconciled BTCUSDT position 1.0, got {pos}");

    driver.shutdown();
    handle.shutdown_and_join();

    // Final published snapshot still carries the reconciled position under the binance block.
    let snap = cell.load_full();
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.venue, "binance");
    assert!(
        vb.positions.iter().any(|p| p.symbol == "BTCUSDT" && (p.size - 1.0).abs() < 1e-12),
        "positions: {:?}",
        vb.positions
    );
}

/// Task 13: a bridge-side poke on [`ReconDriver::reconcile_trigger`]'s sender must cause a SECOND
/// fetch→enqueue pass, on demand, reusing the exact same code path the startup pass ran. Counts
/// fetch calls (rather than asserting on folded state) so the test is agnostic to what the pass
/// actually finds — it only needs to prove the on-demand pass ran, once, per poke.
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

#[test]
fn reconcile_trigger_runs_an_on_demand_pass_after_startup() {
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
        policy: ReconPolicy::default(),
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    // Wait for the startup pass (call #1) so it can't be mistaken for the trigger's pass.
    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Relaxed) < 1 {
        assert!(Instant::now() < deadline, "startup pass never ran");
        std::thread::sleep(Duration::from_millis(5));
    }

    // No further pass without a poke.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::Relaxed), 1, "no pass should run without a trigger poke");

    // Fire the trigger — must cause exactly one more pass, reusing the same fetch path.
    driver.reconcile_trigger().send(()).expect("driver still listening");
    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Relaxed) < 2 {
        assert!(Instant::now() < deadline, "reconcile trigger never caused an on-demand pass");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Exactly one pass per poke — no extra passes sneak in.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::Relaxed), 2, "exactly one on-demand pass per trigger poke");

    driver.shutdown();
    handle.shutdown_and_join();
}

/// Reconciliation-activation Task 7: `spawn_recon` can ADOPT an externally pre-created trigger
/// channel instead of minting its own. This matters because the app root builds every live venue's
/// exec client (and threads a `Sender` clone into its `run_resync_supervisor` call) well BEFORE
/// `spawn_core_multi`/`spawn_recon` run — see `recon_manager.rs`'s `spawn_recon` doc — so the
/// `Sender` a venue bridge holds is obtained from the caller's own pre-built channel, never from
/// `ReconDriver::reconcile_trigger()`. This test proves a poke on that pre-built `Sender` (cloned
/// out BEFORE `spawn_recon` is even called) still reaches the driver, and that
/// `reconcile_trigger()` keeps handing out clones of the exact same adopted channel afterward.
#[test]
fn spawn_recon_adopts_a_pre_created_trigger_channel() {
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
        policy: ReconPolicy::default(),
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        ..ReconConfig::default()
    };

    // Pre-create the channel BEFORE `spawn_recon` runs (mirrors main.rs's Task 7 wiring: the
    // Sender half is cloned into a venue's resync supervisor at `make_engine` time, long before a
    // `ReconDriver` exists) and clone the sender out — standing in for a venue bridge's
    // `on_reconcile` handle.
    let (pre_tx, pre_rx) = std::sync::mpsc::channel::<()>();
    let venue_trigger = pre_tx.clone();
    let driver = spawn_recon(
        &handle,
        vec![("binance".to_string(), Box::new(client))],
        recon_cfg,
        Some((pre_tx, pre_rx)),
    );

    // Wait for the startup pass (call #1) so it can't be mistaken for the trigger's pass.
    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Relaxed) < 1 {
        assert!(Instant::now() < deadline, "startup pass never ran");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Poke via the PRE-CREATED clone — never touched `driver.reconcile_trigger()` — must still
    // cause exactly one more pass, proving the driver reads from the adopted channel.
    venue_trigger.send(()).expect("driver still listening on the adopted channel");
    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Relaxed) < 2 {
        assert!(Instant::now() < deadline, "pre-created sender never reached the adopted driver");
        std::thread::sleep(Duration::from_millis(5));
    }

    // `reconcile_trigger()` must ALSO still work post-adoption — it clones off the SAME channel.
    driver.reconcile_trigger().send(()).expect("driver still listening");
    let deadline = Instant::now() + Duration::from_secs(3);
    while calls.load(Ordering::Relaxed) < 3 {
        assert!(Instant::now() < deadline, "reconcile_trigger() didn't reach the adopted channel");
        std::thread::sleep(Duration::from_millis(5));
    }

    driver.shutdown();
    handle.shutdown_and_join();
}
