//! Journal cross-check (#3 P2): the live wiring at `reconcile_reports`. When a
//! `CoreConfig::journal_view_provider` is supplied, a reconcile pass builds the venue's
//! `JournalView` from it and runs the three-way (local vs venue vs journal) `diff`, so a venue
//! fill the JOURNAL has recorded but the live `Account` lost surfaces as a `JournalDivergence`
//! alert in `CoreSnapshot.recon` — regardless of policy (the P1 always-surface rule). With no
//! provider (the default) the pass is the two-way check, byte-identical.
//!
//! Drives a `Command::ReconcileReports` directly (like `recon_quarantine.rs`) with a FAKE provider,
//! so the SEAM is tested without the vike-data store — the real store-backed provider lives in the
//! binary (vike-app).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, spawn_core};
use vike_exec::recon::{DivergenceKind, JournalView, ReconMode, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, ExecutionClient, ExecutionEngine, ReconcileReports, RiskGate,
    RiskLimits,
};
use vike_model::events::LiquiditySide;
use vike_model::{FillReport, OrderStatusReport};

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
        client_order_id: None,
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 5,
    }
}

fn reports(policy: ReconPolicy) -> ReconcileReports {
    ReconcileReports {
        venue: "binance".into(),
        since: 0,
        orders: Vec::new(),
        fills: vec![ext_fill()], // trade_id t1
        positions: Vec::new(),
        policy,
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: None,
    }
}

/// A provider that says the journal HAS recorded trade_id t1 (the fill the venue reports), which the
/// fresh live Account has not seen → the reconcile must classify it as a JournalDivergence.
fn journal_has_t1() -> vike_core::JournalViewHook {
    Box::new(|_venue: &str| JournalView {
        seen_trade_ids: HashSet::from(["t1".to_string()]),
        orders: Default::default(),
    })
}

#[test]
fn journal_divergence_surfaces_as_a_snapshot_alert() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        journal_view_provider: Some(journal_has_t1()),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    // Even under the fold-everything Synthesize default, a JournalDivergence always surfaces.
    handle.send_command(Command::ReconcileReports(Box::new(reports(ReconPolicy::default()))));

    let deadline = Instant::now() + Duration::from_secs(3);
    let alert = loop {
        let snap = cell.load_full();
        if let Some(a) = snap.recon.alerts.first().cloned() {
            break a;
        }
        assert!(Instant::now() < deadline, "journal divergence never surfaced an alert");
        std::thread::sleep(Duration::from_millis(5));
    };

    assert_eq!(alert.kind, format!("{:?}", DivergenceKind::JournalDivergence));
    assert_eq!(alert.proposed_event_count, 0, "investigative alert carries no proposed events");
    assert!(alert.detail.contains("t1"));
    // and the fill did NOT fold as a MissingFill (no synthesized BTCUSDT position)
    let snap = cell.load_full();
    assert!(
        snap.portfolio
            .venues
            .first()
            .is_none_or(|vb| !vb.positions.iter().any(|p| p.symbol == "BTCUSDT")),
        "a journal divergence is not auto-applied"
    );
}

#[test]
fn no_provider_is_the_two_way_check_no_journal_alert() {
    // Without a provider the pass is local-vs-venue only: the unseen fill is a plain MissingFill,
    // which the Synthesize default FOLDS — no JournalDivergence alert, byte-identical to pre-#3.
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        // journal_view_provider defaulted to None
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    handle.send_command(Command::ReconcileReports(Box::new(reports(ReconPolicy {
        default: ReconMode::Synthesize,
        ..Default::default()
    }))));

    // give the pass time to run, then assert NO JournalDivergence alert is present
    std::thread::sleep(Duration::from_millis(80));
    let snap = cell.load_full();
    assert!(
        !snap
            .recon
            .alerts
            .iter()
            .any(|a| a.kind == format!("{:?}", DivergenceKind::JournalDivergence)),
        "no provider ⇒ no journal divergence"
    );
}

/// The venue reports a live order local does not have; the journal (provider) records that coid as
/// LIVE → an order-loss `JournalDivergence` held alert. `Command::ConfirmRecon` on it RE-REGISTERS
/// the lost order into local state (it appears in `snapshot.orders` only after the confirm).
#[test]
fn order_loss_divergence_reregisters_on_confirm() {
    let primary = engine("binance", "BTCUSDT", Box::new(RecordingClient::default()));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        journal_view_provider: Some(Box::new(|_venue: &str| JournalView {
            seen_trade_ids: HashSet::new(),
            // journal says c-lost is still LIVE (Accepted) — local has never heard of it.
            orders: std::iter::once(("c-lost".to_string(), vike_exec::OrderStatus::Accepted))
                .collect(),
        })),
        ..CoreConfig::default()
    };
    let handle = spawn_core(primary, config);
    let cell = handle.snapshot_cell();

    let venue_order = OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v-lost".into(),
        client_order_id: Some("c-lost".into()),
        side: 1,
        order_type: "limit".into(),
        qty: 2.0,
        filled_qty: 0.5,
        avg_px: 100.0,
        status: "PARTIALLY_FILLED".into(),
        ts: 9,
    };
    handle.send_command(Command::ReconcileReports(Box::new(ReconcileReports {
        venue: "binance".into(),
        since: 0,
        orders: vec![venue_order],
        fills: Vec::new(),
        positions: Vec::new(),
        policy: ReconPolicy::default(),
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: None,
    })));

    // the order-loss divergence surfaces as a held alert; local does NOT yet know the order.
    let deadline = Instant::now() + Duration::from_secs(3);
    let alert_id = loop {
        let snap = cell.load_full();
        if let Some(a) = snap
            .recon
            .alerts
            .iter()
            .find(|a| a.kind == format!("{:?}", DivergenceKind::JournalDivergence))
        {
            assert!(
                !snap.orders.iter().any(|o| o.client_order_id == "c-lost"),
                "the lost order is NOT re-registered before the operator confirms"
            );
            break a.id;
        }
        assert!(Instant::now() < deadline, "order-loss divergence never surfaced");
        std::thread::sleep(Duration::from_millis(5));
    };

    // operator confirms → the lost order is re-registered into local state.
    handle.send_command(Command::ConfirmRecon(alert_id));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snap = cell.load_full();
        if let Some(o) = snap.orders.iter().find(|o| o.client_order_id == "c-lost") {
            assert_eq!(o.venue_order_id.as_deref(), Some("v-lost"));
            assert_eq!(o.filled_qty, 0.5, "reconstructed filled qty from the venue report");
            break;
        }
        assert!(Instant::now() < deadline, "confirm did not re-register the lost order");
        std::thread::sleep(Duration::from_millis(5));
    }
}
