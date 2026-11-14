//! Reconcile drift DETECTION. `Engine::diff_snapshot` is the pure,
//! read-only diff run BEFORE `apply_snapshot` overwrites local state with venue truth: it returns
//! one warning per divergence (position size / authoritative balance / open-order set) and an empty
//! Vec when the snapshot matches. No runtime path calls it since the `Command::ApplySnapshot` arm
//! was deleted (unit A2); here we pin the pure diff itself — it only DETECTS.

use vike_execution::testing::RecordingClient;
use vike_execution::{BalanceMode, Engine, PositionEntry, ReconcileSnapshot};
use vike_model::events::PositionSide;
use vike_model::{OrderRecord, OrderRequest};

use vike_execution::testing::EngineBuilder;

fn engine() -> Engine<RecordingClient> {
    EngineBuilder::default().build()
}

fn seed_position(e: &mut Engine<RecordingClient>, symbol: &str, side: PositionSide, size: f64) {
    e.account.positions.insert(
        ("sim".into(), symbol.into(), side),
        PositionEntry { size, average_price: 100.0, ..Default::default() },
    );
}

/// A matching snapshot (same position, no authoritative balance, no open orders) yields NO warnings.
#[test]
fn matching_snapshot_no_drift() {
    let mut e = engine();
    seed_position(&mut e, "BTCUSDT", PositionSide::Both, 1.0);
    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 1.0)],
        position_sides: vec![("BTCUSDT".to_string(), PositionSide::Both)],
        position_average_price: vec![("BTCUSDT".to_string(), 100.0)],
        ..Default::default()
    };
    let w = e.diff_snapshot(&snap);
    assert!(w.is_empty(), "{w:?}");
}

/// Sub-tolerance float noise (a last-ulp venue re-encode of a folded size) is NOT drift.
#[test]
fn sub_tolerance_position_is_not_drift() {
    let mut e = engine();
    seed_position(&mut e, "BTCUSDT", PositionSide::Both, 1.0);
    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 1.0 + 1e-10)],
        position_sides: vec![("BTCUSDT".to_string(), PositionSide::Both)],
        position_average_price: vec![("BTCUSDT".to_string(), 100.0)],
        ..Default::default()
    };
    let w = e.diff_snapshot(&snap);
    assert!(w.is_empty(), "{w:?}");
}

/// A divergent position size surfaces one drift line naming venue, symbol, and both values.
#[test]
fn position_size_drift() {
    let mut e = engine();
    seed_position(&mut e, "BTCUSDT", PositionSide::Both, 1.0);
    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 2.0)],
        position_sides: vec![("BTCUSDT".to_string(), PositionSide::Both)],
        position_average_price: vec![("BTCUSDT".to_string(), 100.0)],
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
    seed_position(&mut e, "BTCUSDT", PositionSide::Both, 0.5);
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
    seed_position(&mut e, "BTCUSDT", PositionSide::Both, 1.0);
    e.account.balance = 100.0;

    // zero snapshot balance → not diffed even though 0.0 != 100.0
    let snap_pos_only = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 1.0)],
        position_sides: vec![("BTCUSDT".to_string(), PositionSide::Both)],
        position_average_price: vec![("BTCUSDT".to_string(), 100.0)],
        balance: 0.0,
        ..Default::default()
    };
    assert!(e.diff_snapshot(&snap_pos_only).is_empty());

    // authoritative venue balance that diverges → exactly one balance drift line
    let snap_balance = ReconcileSnapshot { balance: 5000.0, ..snap_pos_only };
    let w = e.diff_snapshot(&snap_balance);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].starts_with("DRIFT balance sim"), "{}", w[0]);
    assert!(w[0].contains("local 100") && w[0].contains("venue 5000"), "{}", w[0]);
}

/// A live local order the venue does not report surfaces an open-order set drift.
#[test]
fn open_order_set_drift() {
    let mut e = engine();
    let req: OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "o1", "venue": "sim", "symbol": "BTCUSDT",
        "side": 1, "qty": 1.0, "order_type": "limit", "price": 100.0
    }))
    .unwrap();
    e.registry.insert("o1".to_string(), OrderRecord::new(req)); // live (Initialized)

    let snap = ReconcileSnapshot::default(); // venue reports no open orders
    let w = e.diff_snapshot(&snap);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].starts_with("DRIFT open-orders sim"), "{}", w[0]);
    assert!(w[0].contains("1 local-only"), "{}", w[0]);
}

/// A snapshot reseed (`apply_snapshot`) is a balance SYNC just
/// like `apply_account_state`, so it must move the cash-reconcile realized-PnL baseline in lockstep
/// with the reseeded balance, or `diff_balance` double-counts Σ realized-PnL between a stale
/// baseline and the reseed → a spurious `BalanceDrift`. Inert unless `VIKE_RECONCILE_BALANCE` is
/// on: the baseline is read ONLY by `diff_balance` and is not snapshotted (no journal/state-hash
/// change).
#[test]
fn apply_snapshot_reseed_moves_the_cash_reconcile_baseline() {
    use vike_execution::recon::{ReconcileBalanceCheck, ReconcileBalanceTolerance, diff_balance};

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
    assert_eq!(
        diff_balance(&view, Some(1000.0), "USDT", ReconcileBalanceTolerance::default(), 0),
        // `Anchored { drift: None }` and NOT `Adopt`: the reseed left a venue-anchored baseline, so
        // the pass DIFFS and agrees rather than taking the venue's word (`ReconcileBalanceCheck` tells the
        // two apart).
        ReconcileBalanceCheck::Anchored { expected: 1000.0, drift: None },
        "no drift right after a snapshot reseed — the baseline moved with the balance"
    );
}
