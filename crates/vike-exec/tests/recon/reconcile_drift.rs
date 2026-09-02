//! Audit exec#2 — reconcile drift DETECTION. `ExecutionEngine::diff_snapshot` is the pure,
//! read-only diff run BEFORE `apply_snapshot` overwrites local state with venue truth: it returns
//! one warning per divergence (position size / authoritative balance / open-order set) and an empty
//! Vec when the snapshot matches. The vike-core runtime pushes these lines into the GUI-visible
//! recent-events ring (`runtime_smoke::reconcile_drift_surfaces_in_recent_events`); here we pin the
//! pure diff itself — venue truth still wins the seed, this only DETECTS.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, ExecutionEngine, ManagedOrder, PositionEntry, ReconcileSnapshot,
    RiskGate, RiskLimits,
};
use vike_model::OrderRequest;

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

fn seed_position(e: &mut ExecutionEngine<RecordingClient>, sym: &str, side: &str, size: f64) {
    e.account.positions.insert(
        ("sim".into(), sym.into(), side.into()),
        PositionEntry { size, avg_px: 100.0, ..Default::default() },
    );
}

/// A matching snapshot (same position, no authoritative balance, no open orders) yields NO warnings.
#[test]
fn matching_snapshot_no_drift() {
    let mut e = engine();
    seed_position(&mut e, "BTCUSDT", "BOTH", 1.0);
    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 1.0)],
        position_sides: vec![("BTCUSDT".to_string(), "BOTH".to_string())],
        position_avg_px: vec![("BTCUSDT".to_string(), 100.0)],
        ..Default::default()
    };
    let w = e.diff_snapshot(&snap);
    assert!(w.is_empty(), "{w:?}");
}

/// Sub-tolerance float noise (a last-ulp venue re-encode of a folded size) is NOT drift.
#[test]
fn sub_tolerance_position_is_not_drift() {
    let mut e = engine();
    seed_position(&mut e, "BTCUSDT", "BOTH", 1.0);
    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 1.0 + 1e-10)],
        position_sides: vec![("BTCUSDT".to_string(), "BOTH".to_string())],
        position_avg_px: vec![("BTCUSDT".to_string(), 100.0)],
        ..Default::default()
    };
    let w = e.diff_snapshot(&snap);
    assert!(w.is_empty(), "{w:?}");
}

/// A divergent position size surfaces one drift line naming venue, symbol, and both values.
#[test]
fn position_size_drift() {
    let mut e = engine();
    seed_position(&mut e, "BTCUSDT", "BOTH", 1.0);
    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 2.0)],
        position_sides: vec![("BTCUSDT".to_string(), "BOTH".to_string())],
        position_avg_px: vec![("BTCUSDT".to_string(), 100.0)],
        ..Default::default()
    };
    let w = e.diff_snapshot(&snap);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].starts_with("DRIFT position sim/BTCUSDT[BOTH]"), "{}", w[0]);
    assert!(w[0].contains("local 1") && w[0].contains("venue 2"), "{}", w[0]);
}

/// A local leg the venue snapshot never reports (venue implies flat) is drift too — the seed would
/// silently zero it out otherwise.
#[test]
fn local_ghost_position_drift() {
    let mut e = engine();
    seed_position(&mut e, "BTCUSDT", "BOTH", 0.5);
    let snap = ReconcileSnapshot::default(); // venue reports nothing
    let w = e.diff_snapshot(&snap);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("BTCUSDT") && w[0].contains("venue reports flat"), "{}", w[0]);
}

/// A divergent authoritative (non-zero) balance surfaces a balance drift line; a zero snapshot
/// balance is "not reported" and is never diffed (mirrors apply_snapshot's overwrite guard).
#[test]
fn balance_drift_only_when_snapshot_authoritative() {
    let mut e = engine();
    seed_position(&mut e, "BTCUSDT", "BOTH", 1.0);
    e.account.balance = 100.0;

    // zero snapshot balance → not diffed even though 0.0 != 100.0
    let snap_pos_only = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 1.0)],
        position_sides: vec![("BTCUSDT".to_string(), "BOTH".to_string())],
        position_avg_px: vec![("BTCUSDT".to_string(), 100.0)],
        balance: 0.0,
        ..Default::default()
    };
    assert!(e.diff_snapshot(&snap_pos_only).is_empty());

    // authoritative venue balance that diverges → exactly one balance drift line
    let snap_bal = ReconcileSnapshot { balance: 5000.0, ..snap_pos_only };
    let w = e.diff_snapshot(&snap_bal);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].starts_with("DRIFT balance sim"), "{}", w[0]);
    assert!(w[0].contains("local 100") && w[0].contains("venue 5000"), "{}", w[0]);
}

/// A live local order the venue does not report surfaces an open-order set drift.
#[test]
fn open_order_set_drift() {
    let mut e = engine();
    let req: OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "o1", "venue": "sim", "symbol": "BTCUSDT",
        "side": 1, "qty": 1.0, "order_type": "limit", "price": 100.0
    }))
    .unwrap();
    e.registry.insert("o1".to_string(), ManagedOrder::new(req)); // live (Initialized)

    let snap = ReconcileSnapshot::default(); // venue reports no open orders
    let w = e.diff_snapshot(&snap);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].starts_with("DRIFT open-orders sim"), "{}", w[0]);
    assert!(w[0].contains("1 local-only"), "{}", w[0]);
}

/// A snapshot reseed (`apply_snapshot`, the `Command::ApplySnapshot` path) is a balance SYNC just
/// like `apply_account_state`, so it must move the cash-reconcile realized-PnL baseline in lockstep
/// with the reseeded balance. Without this, `diff_balance` would compare venue cash against a stale
/// baseline and double-count Σ realized-PnL between that baseline and the fresh reseed → a spurious
/// `BalanceDrift`. INERT unless Feature 2 (`VIKE_RECONCILE_BALANCE`) is on — the baseline is read
/// ONLY by `diff_balance` and is not snapshotted, so this changes no journal/state-hash.
#[test]
fn apply_snapshot_reseed_moves_the_cash_reconcile_baseline() {
    use vike_exec::recon::{diff_balance, BalanceTol};

    let mut e = engine();
    // An account carrying realized PnL and a STALE baseline from an earlier sync (250 realized, but
    // the baseline was captured back when realized was 50).
    e.account.realized_pnl = 250.0;
    e.account.balance = 900.0;
    e.account.balance_mode = BalanceMode::Authoritative;
    e.account.realized_pnl_at_balance_sync = Some(50.0);

    // Venue snapshot reseeds cash to 1000.
    e.apply_snapshot(&ReconcileSnapshot { balance: 1000.0, ..Default::default() });

    assert_eq!(e.account.balance, 1000.0, "balance reseeded from the snapshot");
    assert_eq!(
        e.account.realized_pnl_at_balance_sync,
        Some(250.0),
        "the baseline must move to the CURRENT realized_pnl at the reseed, not stay stale at 50"
    );

    // A cash diff immediately after the reseed sees ZERO drift (expected == venue == 1000), not the
    // phantom Σ-realized-PnL (250 − 50 = 200) a stale baseline would have manufactured.
    let owned = e.local_view();
    let view = owned.as_view();
    assert!(
        diff_balance(&view, Some(1000.0), "USDT", BalanceTol::default(), 0).is_none(),
        "no drift right after a snapshot reseed — the baseline moved with the balance"
    );
}
