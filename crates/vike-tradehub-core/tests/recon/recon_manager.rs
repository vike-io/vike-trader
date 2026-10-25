//! `ReconManager` startup-cadence integration gate: a real single-writer core + one engine,
//! reconciled at startup through the `FakeReconClient` report seam. An external fill the local
//! engine never saw is fetched off-fold by the manager, enqueued as `Command::ReconcileReports`,
//! and folded on the core thread via `recon::diff`/`recon::resolve` — the synthesized fill lands
//! through the same `on_event` path real venue fills use, so the reconciled position appears in
//! the published `CoreSnapshot`.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, spawn_core};
use vike_exec::recon::{FakeReconClient, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_tradehub_core::{ReconConfig, spawn_recon};

use crate::kit::doubles::CountingReconClient;
use crate::kit::engines::dyn_engine;
use crate::kit::events::external_fill_report;

#[test]
fn startup_reconcile_folds_external_fill_into_position() {
    // A live core with one binance engine; the local engine has folded NOTHING.
    let primary = dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    // The venue reports an external fill (+1 BTCUSDT @ 100) the core never saw.
    let client = FakeReconClient { fills: vec![external_fill_report()], ..Default::default() };
    let recon_cfg = ReconConfig {
        policy: ReconPolicy::default(), // Synthesize everything
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        ..ReconConfig::default()
    };
    let driver = spawn_recon(
        &handle,
        vec![vike_tradehub_core::ReconLeg::sole_account_of("binance", Box::new(client))],
        recon_cfg,
        None,
    );

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
#[test]
fn reconcile_trigger_runs_an_on_demand_pass_after_startup() {
    let primary = dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
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
    let driver = spawn_recon(
        &handle,
        vec![vike_tradehub_core::ReconLeg::sole_account_of("binance", Box::new(client))],
        recon_cfg,
        None,
    );

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
/// `spawn_core_multi`/`spawn_recon` run — see `crates/vike-tradehub-core/src/recon_manager/driver.rs`'s
/// `spawn_recon` doc — so the `Sender` a venue bridge holds is obtained from the caller's own
/// pre-built channel, never from `ReconDriver::reconcile_trigger()`. This test proves a poke on
/// that pre-built `Sender` (cloned out BEFORE `spawn_recon` is even called) still reaches the
/// driver, and that `reconcile_trigger()` keeps handing out clones of the exact same adopted
/// channel afterward.
#[test]
fn spawn_recon_adopts_a_pre_created_trigger_channel() {
    let primary = dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
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
        vec![vike_tradehub_core::ReconLeg::sole_account_of("binance", Box::new(client))],
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

/// **W13b finding 5: the driver's stop can be bounded.** A driver parked in a venue call cannot honour its
/// stop flag until the call returns, so `shutdown_within` gives up at its bound (`false`) instead of
/// joining for as long as the call takes, and an idle driver is joined at once (`true`).
#[test]
fn a_driver_inside_a_stalled_venue_call_is_abandoned_at_the_bound_and_an_idle_one_is_joined() {
    use std::sync::atomic::AtomicBool;
    use vike_exec::recon::ReconClient;
    use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

    struct Stalled {
        entered: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }
    impl ReconClient for Stalled {
        fn fetch_order_status_reports(
            &self,
            _since: i64,
        ) -> Result<Vec<OrderStatusReport>, String> {
            self.entered.store(true, Ordering::SeqCst);
            while !self.release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(Vec::new())
        }
        fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
            Ok(Vec::new())
        }
        fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
            Ok(Vec::new())
        }
    }

    let handle = spawn_core(
        dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default())),
        CoreConfig::default(),
    );
    let (entered, release) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let stalled = Stalled { entered: Arc::clone(&entered), release: Arc::clone(&release) };
    let cfg = ReconConfig { startup_delay: Duration::ZERO, ..ReconConfig::default() };
    let driver = spawn_recon(
        &handle,
        vec![vike_tradehub_core::ReconLeg::sole_account_of("binance", Box::new(stalled))],
        cfg,
        None,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !entered.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "the startup pass never reached the venue");
        std::thread::sleep(Duration::from_millis(5));
    }
    let started = Instant::now();
    driver.begin_shutdown();
    assert!(!driver.shutdown_within(Duration::from_millis(200)), "stalled: abandoned, not joined");
    assert!(started.elapsed() < Duration::from_secs(3), "and it gave up at the bound");
    release.store(true, Ordering::SeqCst);

    // An idle driver (nothing in flight) is joined well inside the bound.
    let idle = spawn_recon(
        &handle,
        vec![vike_tradehub_core::ReconLeg::sole_account_of(
            "binance",
            Box::new(FakeReconClient::default()),
        )],
        ReconConfig { startup_delay: Duration::from_secs(60), ..ReconConfig::default() },
        None,
    );
    assert!(idle.shutdown_within(Duration::from_secs(5)), "an idle driver ends at its stop flag");
    handle.shutdown_and_join();
}

/// **W13b finding 6: a pass that skipped a leg as NOT READY is tried again soon, not at the next
/// interval tick.** The first pass of a start has to land inside the 120 s window in which a restored
/// order mutes its strategy's tagged resubmit, and the default interval is 60 s: two skipped passes lost
/// the window. The venue below is NOT READY on its first orders fetch and healthy afterwards; with no
/// interval at all the driver must still come back (at `not_ready_retry`), and a zero period is the old
/// behaviour (nothing retries).
#[test]
fn a_pass_that_skipped_a_leg_as_not_ready_is_retried_at_the_retry_period() {
    use std::sync::atomic::AtomicU32;
    use vike_exec::recon::ReconClient;
    use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

    struct NotReadyOnce {
        orders: Arc<AtomicU32>,
        balance: Arc<AtomicU32>,
    }
    impl ReconClient for NotReadyOnce {
        fn fetch_order_status_reports(
            &self,
            _since: i64,
        ) -> Result<Vec<OrderStatusReport>, String> {
            if self.orders.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err("NOT READY: the link is down".to_string());
            }
            Ok(Vec::new())
        }
        fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
            Ok(Vec::new())
        }
        fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
            Ok(Vec::new())
        }
        fn fetch_balance(&self) -> Result<Option<f64>, String> {
            self.balance.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
    }

    for (retry, expect_retry) in [(Duration::from_millis(40), true), (Duration::ZERO, false)] {
        let handle = spawn_core(
            dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default())),
            CoreConfig::default(),
        );
        let (orders, balance) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
        let client = NotReadyOnce { orders: Arc::clone(&orders), balance: Arc::clone(&balance) };
        let cfg = ReconConfig {
            startup_delay: Duration::ZERO,
            interval: None,
            not_ready: Some(|e| e.starts_with("NOT READY")),
            not_ready_retry: retry,
            ..ReconConfig::default()
        };
        let driver = spawn_recon(
            &handle,
            vec![vike_tradehub_core::ReconLeg::sole_account_of("binance", Box::new(client))],
            cfg,
            None,
        );
        let deadline = Instant::now() + Duration::from_secs(if expect_retry { 5 } else { 1 });
        while balance.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let got_second_pass = balance.load(Ordering::SeqCst) > 0;
        assert_eq!(
            got_second_pass,
            expect_retry,
            "retry period {retry:?}: the second pass {} come",
            if expect_retry { "must" } else { "must not" }
        );
        assert_eq!(orders.load(Ordering::SeqCst) >= 2, expect_retry);
        driver.shutdown();
        handle.shutdown_and_join();
    }
}

/// **W13c finding 3: the NOT READY retry runs the legs that were NOT READY, not every leg of the pass.**
/// One stuck leg (the link down, a prefetch owed) used to make the healthy ones refetch orders, fills,
/// positions and balance every retry period for a minute at boot (a rate-limit exposure on ctrader and
/// fxcm). Two legs: `bybit` is NOT READY on every call, `binance` is healthy. The stuck one is asked
/// again and again; the healthy one is asked once, by the pass that found the stuck one.
#[test]
fn a_not_ready_retry_asks_only_the_legs_that_were_not_ready() {
    use std::sync::atomic::AtomicU32;
    use vike_exec::recon::ReconClient;
    use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

    /// Counts every fetch of every kind; `ready` false = the leg is NOT READY.
    struct Leg {
        ready: bool,
        orders: Arc<AtomicU32>,
        fills: Arc<AtomicU32>,
        positions: Arc<AtomicU32>,
        balance: Arc<AtomicU32>,
    }
    impl ReconClient for Leg {
        fn fetch_order_status_reports(
            &self,
            _since: i64,
        ) -> Result<Vec<OrderStatusReport>, String> {
            self.orders.fetch_add(1, Ordering::SeqCst);
            if self.ready { Ok(Vec::new()) } else { Err("NOT READY: the link is down".to_string()) }
        }
        fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
            self.fills.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
            self.positions.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn fetch_balance(&self) -> Result<Option<f64>, String> {
            self.balance.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
    }
    let counters = || {
        (
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicU32::new(0)),
        )
    };
    let (h_orders, h_fills, h_positions, h_balance) = counters();
    let (s_orders, s_fills, s_positions, s_balance) = counters();
    let healthy = Leg {
        ready: true,
        orders: Arc::clone(&h_orders),
        fills: Arc::clone(&h_fills),
        positions: Arc::clone(&h_positions),
        balance: Arc::clone(&h_balance),
    };
    let stuck = Leg {
        ready: false,
        orders: Arc::clone(&s_orders),
        fills: Arc::clone(&s_fills),
        positions: Arc::clone(&s_positions),
        balance: Arc::clone(&s_balance),
    };
    let handle = spawn_core(
        dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default())),
        CoreConfig::default(),
    );
    let cfg = ReconConfig {
        startup_delay: Duration::ZERO,
        interval: None,
        not_ready: Some(|e| e.starts_with("NOT READY")),
        not_ready_retry: Duration::from_millis(30),
        ..ReconConfig::default()
    };
    let driver = spawn_recon(
        &handle,
        vec![
            vike_tradehub_core::ReconLeg::sole_account_of("binance", Box::new(healthy)),
            vike_tradehub_core::ReconLeg::sole_account_of("bybit", Box::new(stuck)),
        ],
        cfg,
        None,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while s_orders.load(Ordering::SeqCst) < 6 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(s_orders.load(Ordering::SeqCst) >= 6, "the stuck leg is asked again and again");
    // The stuck leg never got past its orders fetch; the healthy one ran ONE whole leg, at the first pass.
    assert_eq!(
        (
            h_orders.load(Ordering::SeqCst),
            h_fills.load(Ordering::SeqCst),
            h_positions.load(Ordering::SeqCst),
            h_balance.load(Ordering::SeqCst)
        ),
        (1, 1, 1, 1),
        "a healthy leg is fetched once, not once per retry of its neighbour"
    );
    assert_eq!(s_balance.load(Ordering::SeqCst), 0, "a NOT READY leg skips its balance fetch");
    driver.shutdown();
    handle.shutdown_and_join();
}
