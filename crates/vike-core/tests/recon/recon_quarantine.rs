//! Task 17: quarantine/hybrid reconcile policies surface as a structured `CoreSnapshot.recon`
//! block, and an operator confirms a held alert via `Command::ConfirmRecon`. Reuses the Task 9
//! core-spawn harness + the pure `vike_exec::recon` types (no manager thread needed — this test
//! drives a `Command::ReconcileReports` directly, deterministically, like `recon_manager.rs`'s
//! startup test but without the off-fold fetch timing).

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, spawn_core};
use vike_exec::recon::{DivergenceKind, ReconMode, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionClient, ExecutionEngine, ReconcileReports, RiskGate,
    RiskLimits,
};
use vike_model::FillReport;
use vike_model::events::LiquiditySide;

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
fn quarantine_holds_events_as_snapshot_alert_then_confirm_folds_them() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    // Everything Quarantined: the external fill must NOT fold.
    let policy = ReconPolicy { default: ReconMode::Quarantine, ..Default::default() };
    let reports = ReconcileReports {
        venue: "binance".into(),
        since: 0,
        orders: Vec::new(),
        fills: vec![ext_fill()],
        positions: Vec::new(),
        policy,
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: None,
    };
    handle.send_command(Command::ReconcileReports(Box::new(reports)));

    // Poll until the pass has run and surfaced the held alert.
    let deadline = Instant::now() + Duration::from_secs(3);
    let alert = loop {
        let snap = cell.load_full();
        if let Some(a) = snap.recon.alerts.first().cloned() {
            break a;
        }
        assert!(Instant::now() < deadline, "reconcile pass never surfaced a held alert");
        std::thread::sleep(Duration::from_millis(5));
    };

    // The synthesized fill must NOT have folded: no BTCUSDT position yet.
    let snap = cell.load_full();
    assert!(
        snap.portfolio
            .venues
            .first()
            .is_none_or(|vb| !vb.positions.iter().any(|p| p.symbol == "BTCUSDT")),
        "quarantined divergence must not fold: positions = {:?}",
        snap.portfolio.venues.first().map(|vb| &vb.positions)
    );
    assert_eq!(alert.kind, format!("{:?}", DivergenceKind::MissingFill));
    assert!(!alert.detail.is_empty());
    assert_eq!(alert.proposed_event_count, 2, "OrderAccepted + Fill (external order, no coid)");
    assert_eq!(snap.recon.alerts.len(), 1);

    // Operator confirms the held alert by id.
    handle.send_command(Command::ConfirmRecon(alert.id));

    // Poll until the events fold (position appears) AND the alert is gone from the snapshot.
    let deadline = Instant::now() + Duration::from_secs(3);
    let pos = loop {
        let snap = cell.load_full();
        if snap.recon.alerts.is_empty()
            && let Some(p) = snap
                .portfolio
                .venues
                .first()
                .and_then(|vb| vb.positions.iter().find(|p| p.symbol == "BTCUSDT"))
        {
            break p.size;
        }
        assert!(Instant::now() < deadline, "ConfirmRecon never folded the held alert");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!((pos - 1.0).abs() < 1e-12, "expected confirmed BTCUSDT position 1.0, got {pos}");

    let snap = cell.load_full();
    assert!(snap.recon.alerts.is_empty(), "confirmed alert must be removed from the snapshot");

    handle.shutdown_and_join();
}

/// Fix-round-1 IMPORTANT-4: an UnknownOrder divergence recurs by design (the registry never
/// adopts its coid), so its held alert is dedup-keyed — a later pass re-raising the same
/// (venue, kind, venue_order_id) REFRESHES the existing row (same confirm id) instead of
/// appending a new alert per pass; and once the adoption is confirmed (its fill folded), the
/// divergence stops surfacing entirely (the recurring-pass no-op).
#[test]
fn recurring_unknown_order_alert_dedupes_and_clears_after_confirm() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    let policy = ReconPolicy { default: ReconMode::Quarantine, ..Default::default() };
    let unknown = vike_model::OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v-unk".into(),
        client_order_id: None, // externally placed
        side: 1,
        order_type: "LIMIT".into(),
        qty: 1.0,
        filled_qty: 1.0,
        avg_px: 100.0,
        status: "FILLED".into(), // terminal + no fill reports in-pass = the adoption case
        ts: 5,
    };
    let reports = |fills: Vec<FillReport>| {
        Box::new(ReconcileReports {
            venue: "binance".into(),
            since: 0,
            orders: vec![unknown.clone()],
            fills,
            positions: Vec::new(),
            policy: policy.clone(),
            balance: None,
            generate_missing_orders: true,
            reconcile_balance: false,
            balance_tol: vike_exec::recon::BalanceTol::default(),
            route_key: None,
        })
    };
    let wait_alerts = |n: usize, what: &str| {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let snap = cell.load_full();
            if snap.recon.alerts.len() >= n {
                break snap;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    // Pass 1: the adoptable UnknownOrder is held (quarantine policy) with accept + fill proposed.
    handle.send_command(Command::ReconcileReports(reports(Vec::new())));
    let snap = wait_alerts(1, "pass 1 unknown-order alert");
    assert_eq!(snap.recon.alerts.len(), 1);
    let unk = snap.recon.alerts[0].clone();
    assert_eq!(unk.kind, format!("{:?}", DivergenceKind::UnknownOrder));
    assert_eq!(unk.proposed_event_count, 2, "accept + cumulative fill");

    // Pass 2: the SAME unknown order recurs, plus a fresh quarantined MissingFill (its alert
    // appearing proves pass 2 fully processed). The unknown-order alert must be REFRESHED in
    // place — same id, still exactly one row — never appended.
    handle.send_command(Command::ReconcileReports(reports(vec![ext_fill()])));
    let snap = wait_alerts(2, "pass 2 missing-fill alert");
    assert_eq!(snap.recon.alerts.len(), 2, "dedup: no second UnknownOrder row ({:?})", {
        snap.recon.alerts.iter().map(|a| a.kind.clone()).collect::<Vec<_>>()
    });
    let unks: Vec<_> = snap
        .recon
        .alerts
        .iter()
        .filter(|a| a.kind == format!("{:?}", DivergenceKind::UnknownOrder))
        .collect();
    assert_eq!(unks.len(), 1);
    assert_eq!(unks[0].id, unk.id, "refresh keeps the confirm id");

    // Operator confirms the adoption: its fill folds (position appears), the row clears.
    handle.send_command(Command::ConfirmRecon(unk.id));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snap = cell.load_full();
        let adopted = snap
            .portfolio
            .venues
            .first()
            .is_some_and(|vb| vb.positions.iter().any(|p| p.symbol == "BTCUSDT"));
        if adopted && snap.recon.alerts.len() == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "confirm never folded the adoption");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Pass 3: the venue still reports the (now-adopted) order; a fresh MissingFill (new trade id)
    // proves the pass ran. NO UnknownOrder alert may re-raise — the recurring-pass no-op.
    let mut f2 = ext_fill();
    f2.trade_id = "t2".into();
    handle.send_command(Command::ReconcileReports(reports(vec![f2])));
    let snap = wait_alerts(2, "pass 3 second missing-fill alert");
    assert_eq!(snap.recon.alerts.len(), 2, "held t1 + new t2, nothing else");
    assert!(
        snap.recon.alerts.iter().all(|a| a.kind == format!("{:?}", DivergenceKind::MissingFill)),
        "an adopted UnknownOrder must not re-alert: {:?}",
        snap.recon.alerts.iter().map(|a| a.kind.clone()).collect::<Vec<_>>()
    );

    handle.shutdown_and_join();
}

#[test]
fn confirm_recon_unknown_id_is_a_harmless_no_op() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    // No alert was ever held — confirming id 999 must not panic or corrupt state.
    handle.send_command(Command::ConfirmRecon(999));

    // Drain: send a cheap follow-up command and wait for a fresh snapshot to prove the core is
    // still alive and folding normally.
    handle.send_command(Command::SetTradingState(vike_exec::TradingState::Active));
    std::thread::sleep(Duration::from_millis(50));
    let snap = cell.load_full();
    assert!(snap.recon.alerts.is_empty());
    assert!(handle.is_alive());

    handle.shutdown_and_join();
}
