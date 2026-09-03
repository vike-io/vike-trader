//! Wave 5d: a reconcile pass carrying a venue-REPORTED per-position coin delta (Deribit
//! `get_positions.delta`) surfaces it on `CoreSnapshot`, keyed (venue, symbol), so the Greeks tool
//! can fold a perp/future hedge leg into net portfolio greeks (`coin_delta × spot`). `PositionView`
//! is fills-derived and carries no venue delta, so this side map is the wire. Drives
//! `Command::ReconcileReports` directly (like `recon_quarantine.rs`) and reads it back through the
//! `CoreSnapshot::coin_delta` accessor.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_core::{spawn_core, CoreConfig};
use vike_exec::recon::{ReconMode, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionClient, ExecutionEngine, ReconcileReports, RiskGate,
    RiskLimits,
};
use vike_model::events::PositionSide;
use vike_model::PositionStatusReport;

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

#[test]
fn reconcile_pass_surfaces_deribit_coin_delta_on_the_snapshot() {
    let primary = engine("deribit", "BTC-PERPETUAL", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    // A Deribit PERP position report: `qty` is USD NOTIONAL (inverse contract), `delta` the true
    // COIN delta — the value the greeks fold trusts.
    let perp = PositionStatusReport {
        venue: "deribit".into(),
        symbol: "BTC-PERPETUAL".into(),
        position_side: PositionSide::Both,
        qty: 52_000.0,
        avg_px: 104_000.0,
        ts: 1,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: Some(0.5),
    };
    // Quarantine so nothing folds — the coin-delta capture is independent of the resolve policy.
    let policy = ReconPolicy { default: ReconMode::Quarantine, ..Default::default() };
    let reports = ReconcileReports {
        venue: "deribit".into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: vec![perp],
        policy,
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: None,
    };
    handle.send_command(Command::ReconcileReports(Box::new(reports)));

    // Poll until the pass has run and the snapshot carries the coin delta.
    let deadline = Instant::now() + Duration::from_secs(3);
    let delta = loop {
        let snap = cell.load_full();
        if let Some(d) = snap.coin_delta("deribit", "BTC-PERPETUAL") {
            break d;
        }
        assert!(Instant::now() < deadline, "reconcile pass never surfaced the coin delta");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!((delta - 0.5).abs() < 1e-12, "expected coin delta 0.5, got {delta}");

    // A venue/symbol with no reported delta reads None (never fabricated).
    let snap = cell.load_full();
    assert_eq!(snap.coin_delta("deribit", "ETH-PERPETUAL"), None);
    assert_eq!(snap.coin_delta("binance", "BTC-PERPETUAL"), None);

    handle.shutdown_and_join();
}
