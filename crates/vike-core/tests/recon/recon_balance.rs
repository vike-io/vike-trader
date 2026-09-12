//! Task 3: the reconcile pass seeds the venue's authoritative balance (`ReconClient::fetch_balance`)
//! into `Account` — balance-only, WITHOUT touching positions. Mirrors `recon_manager.rs`'s
//! startup-cadence harness (a real single-writer core + one engine, reconciled via the
//! `FakeReconClient` report seam), but the fake client carries no orders/fills/positions — only a
//! balance — so a passing test proves the balance path is wired independently of the fill-driven
//! position path (see that module's `startup_reconcile_folds_external_fill_into_position`).

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, ReconConfig, spawn_core, spawn_recon};
use vike_exec::recon::{FakeReconClient, ReconClient, ReconPolicy};
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

/// A reconcile pass whose ONLY divergence is the venue-reported balance (no fills/orders/
/// positions) must seed `Account` authoritatively — the reported equity reflects the venue
/// balance exactly (no positions => no unrealized, and Authoritative mode drops the seed_cash
/// term) — and must NOT create a position out of nothing.
#[test]
fn startup_reconcile_seeds_venue_balance_without_touching_positions() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    // The venue reports ONLY a balance — no orders, no fills, no positions.
    let client = FakeReconClient { balance: Some(9999.0), ..Default::default() };
    let recon_cfg = ReconConfig {
        policy: ReconPolicy::default(),
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    // Poll until the balance seed lands.
    let deadline = Instant::now() + Duration::from_secs(3);
    let (balance, equity) = loop {
        let snap = cell.load_full();
        if let Some(vb) = snap.portfolio.venues.first()
            && (vb.balance - 9999.0).abs() < 1e-9
        {
            break (vb.balance, vb.equity);
        }
        if Instant::now() > deadline {
            let snap = cell.load_full();
            break (
                snap.portfolio.venues.first().map(|v| v.balance).unwrap_or(f64::NAN),
                snap.portfolio.venues.first().map(|v| v.equity).unwrap_or(f64::NAN),
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    driver.shutdown();
    handle.shutdown_and_join();

    assert!((balance - 9999.0).abs() < 1e-9, "expected venue balance 9999.0, got {balance}");
    assert!(
        (equity - 9999.0).abs() < 1e-9,
        "expected equity 9999.0 (balance-only, no positions/unrealized), got {equity}"
    );

    let snap = cell.load_full();
    let vb = &snap.portfolio.venues[0];
    assert!(
        vb.positions.is_empty(),
        "balance-only reconcile must not create positions: {:?}",
        vb.positions
    );
}

/// Feature 2 (`VIKE_RECONCILE_BALANCE`) ON — first observation. The very first reconcile pass sees
/// an account still in `Delta` mode (balance == arbitrary `seed_cash`), so the diff engine ADOPTS
/// (seeds) venue cash without flagging — landing the IDENTICAL authoritative balance/equity the
/// legacy silent-seed path lands (the off-path proof above). This proves the on-path adopt is
/// wired and result-equivalent to the legacy seed on first observation.
#[test]
fn reconcile_balance_on_first_observation_adopts_like_the_legacy_seed() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    let client = FakeReconClient { balance: Some(9999.0), ..Default::default() };
    let recon_cfg = ReconConfig {
        policy: ReconPolicy::hybrid(),
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        reconcile_balance: true, // Feature 2 ON
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    let deadline = Instant::now() + Duration::from_secs(3);
    let (balance, equity) = loop {
        let snap = cell.load_full();
        if let Some(vb) = snap.portfolio.venues.first()
            && (vb.balance - 9999.0).abs() < 1e-9
        {
            break (vb.balance, vb.equity);
        }
        if Instant::now() > deadline {
            let snap = cell.load_full();
            break (
                snap.portfolio.venues.first().map(|v| v.balance).unwrap_or(f64::NAN),
                snap.portfolio.venues.first().map(|v| v.equity).unwrap_or(f64::NAN),
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    let snap = cell.load_full();
    let no_alert = snap.recon.alerts.iter().all(|a| !a.kind.contains("BalanceDrift"));
    driver.shutdown();
    handle.shutdown_and_join();

    assert!((balance - 9999.0).abs() < 1e-9, "first observation adopts venue cash, got {balance}");
    assert!((equity - 9999.0).abs() < 1e-9, "equity == adopted balance, got {equity}");
    assert!(no_alert, "first observation must NOT raise a BalanceDrift alert");
}

/// A `ReconClient` whose balance CHANGES after the first fetch — so the first pass adopts, and a
/// later pass sees a genuine (venue-side) cash move the account cannot explain.
struct ChangingBalanceClient {
    calls: AtomicUsize,
    first: f64,
    later: f64,
}

impl ReconClient for ChangingBalanceClient {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(Some(if n == 0 { self.first } else { self.later }))
    }
}

/// Feature 2 ON — a SURPRISE cash move QUARANTINES even under `hybrid`, the LOOSEST policy an
/// operator can name short of `synthesize` (and one they now reach by naming it — an unset
/// `VIKE_RECONCILE_POLICY` folds to `quarantine` at both live mounts since S2): it surfaces a
/// `BalanceDrift` alert and does NOT silently move `Account.balance`. This is the load-bearing
/// safety property — a surprise withdrawal/deposit/liquidation is held for operator confirm, never
/// auto-absorbed (contrast the legacy path, which would have overwritten balance to the new venue
/// number every pass with no trace).
#[test]
fn reconcile_balance_on_surprise_quarantines_under_hybrid_without_moving_balance() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    // Pass 1 adopts 9999; every later pass reports 10_250 — a +251 unexplained move, well past tol.
    let client =
        ChangingBalanceClient { calls: AtomicUsize::new(0), first: 9999.0, later: 10_250.0 };
    let recon_cfg = ReconConfig {
        policy: ReconPolicy::hybrid(),
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        interval: Some(Duration::from_millis(30)), // re-reconcile so a later pass sees the change
        reconcile_balance: true,
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    // Poll until a BalanceDrift alert surfaces (a later pass diffed the surprise).
    let deadline = Instant::now() + Duration::from_secs(5);
    let alerted = loop {
        let snap = cell.load_full();
        if snap.recon.alerts.iter().any(|a| a.kind.contains("BalanceDrift")) {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    let snap = cell.load_full();
    let balance = snap.portfolio.venues.first().map(|v| v.balance).unwrap_or(f64::NAN);
    driver.shutdown();
    handle.shutdown_and_join();

    assert!(alerted, "a surprise cash move must surface a BalanceDrift alert under hybrid");
    assert!(
        (balance - 9999.0).abs() < 1e-9,
        "quarantine must NOT move balance to the surprise value; got {balance}, expected 9999.0"
    );
}

/// A `ReconClient` whose balance CREEPS by a fixed, SUB-TOLERANCE amount on every fetch — the
/// third-party-funding shape this whole split exists for. No single step is large enough to raise
/// anything; the question is whether the steps accumulate.
struct CreepingBalanceClient {
    calls: AtomicUsize,
    first: f64,
    step: f64,
}

impl ReconClient for CreepingBalanceClient {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed) as f64;
        Ok(Some(self.first + n * self.step))
    }
}

/// **THE BOUND, driven through a real core: a sub-tolerance creep is NOT absorbed pass after
/// pass.**
///
/// This is the runtime half of `crates/vike-exec/src/recon/diff.rs`'s
/// `rolling_the_local_figure_forward_leaves_expected_invariant` — that one proves the ARITHMETIC
/// (applying an `Anchored` verdict leaves the next pass's `expected` bit-unchanged), this one
/// proves the WIRING: that `CoreThread::reconcile_reports` performs the roll-forward rather than
/// the adopt, so the anchor a creep is measured against genuinely stops moving.
///
/// The venue creeps +0.4 a pass on a ~10k account, where the default `BalanceTol` band is
/// `max(1.0, 1e-4·10_000) = 1.0`. Every single step is INSIDE the band, so no pass can flag on its
/// own evidence; the FOURTH pass is 1.2 away from a fixed anchor and flags. Under `hybrid` that
/// `BalanceDrift` is quarantined, so the surprise is surfaced and the local figure never moves.
///
/// ⚠ **It fails in BOTH directions on the pre-fix code, and the second is the important one.**
/// Restoring the every-pass re-adopt (`Anchored { .. } => balance_seed = Some(venue)`) re-anchors
/// the baseline onto the venue's own number every pass, so (a) no alert EVER surfaces — the
/// deadline expires — and (b) `balance` climbs by 0.4 a pass forever, which on the shipped 60 s
/// interval and a 250k account is ~25 a pass and ~36k a day of silently absorbed third-party cash.
#[test]
fn a_sub_tolerance_creep_is_not_absorbed_pass_after_pass() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    let client = CreepingBalanceClient { calls: AtomicUsize::new(0), first: 10_000.0, step: 0.4 };
    let recon_cfg = ReconConfig {
        policy: ReconPolicy::hybrid(),
        lookback_ms: 60_000,
        startup_delay: Duration::from_millis(0),
        interval: Some(Duration::from_millis(30)),
        reconcile_balance: true,
        ..ReconConfig::default()
    };
    let driver =
        spawn_recon(&handle, vec![("binance".to_string(), Box::new(client))], recon_cfg, None);

    // Poll until the ACCUMULATED creep crosses the band. Every intermediate reading of `balance`
    // is checked on the way: absorption must never happen at all, not merely be raised afterwards.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut worst_balance = 10_000.0_f64;
    let alerted = loop {
        let snap = cell.load_full();
        if let Some(vb) = snap.portfolio.venues.first()
            && vb.balance.is_finite()
            && vb.balance > 0.0
        {
            worst_balance = worst_balance.max(vb.balance);
        }
        if snap.recon.alerts.iter().any(|a| a.kind.contains("BalanceDrift")) {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    let snap = cell.load_full();
    let balance = snap.portfolio.venues.first().map(|v| v.balance).unwrap_or(f64::NAN);
    driver.shutdown();
    handle.shutdown_and_join();

    assert!(
        alerted,
        "a creep of 0.4 a pass inside a 1.0 band must ACCUMULATE against a fixed anchor and cross \
         it — it never does if each pass re-anchors onto the venue's own figure"
    );
    assert!(
        (balance - 10_000.0).abs() < 1e-9,
        "the local figure must never adopt a venue number this daemon cannot explain; got {balance}"
    );
    assert!(
        (worst_balance - 10_000.0).abs() < 1e-9,
        "no pass may absorb even one sub-tolerance step; the highest balance seen was \
         {worst_balance}"
    );
}
