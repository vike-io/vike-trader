//! Task 18 (P4): the reconcile idempotency gate. Running the SAME reconcile pass twice against
//! the SAME venue reports must have no additional effect once pass 1's synthesized events have
//! been folded — fills dedup on `trade_id` (`ExecutionEngine::on_event`'s `seen_trade_ids` guard)
//! and a matching position no longer diverges (`diff`'s `qty_tol` check). This is the structural
//! half of "reconcile is safe to re-run": a crashed/retried reconcile driver, or a periodic
//! cadence poll that re-observes the same venue state, must not double-book PnL or re-synthesize
//! events. `run_pass` (`src/recon/client.rs`) is the exact function under test — the composition
//! `fetch -> diff -> resolve` both the offline `FakeReconClient` and the live `ReconManager` share.

use std::collections::HashSet;

use vike_exec::recon::{FakeReconClient, LocalView, Recon, ReconPolicy, run_pass};
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate, RiskLimits,
};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";

fn engine() -> ExecutionEngine<vike_exec::testing::RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        vike_exec::testing::RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    // The external position leg in this file's fixture is on a DIFFERENT symbol than the
    // engine's own mounted symbol (BTCUSDT) — a realistic "the account also holds ETHUSDT"
    // scenario. `accepts_symbol` gates which fills fold into `account.positions`, so extend it
    // (Phase D multi-mount) or the synthesized ETHUSDT fill would be silently dropped and the
    // position would never converge, breaking the idempotency proof for the wrong reason.
    e.extra_symbols.push("ETHUSDT".to_string());
    e
}

/// The venue reports: one external fill (never seen locally) plus a position that only exists on
/// the venue side (`PositionOnlyExternal`) — a realistic "we reconnected and missed some activity"
/// snapshot. Both divergence kinds are local-origin/no-local-origin mixes covered by the default
/// (all-Synthesize) policy, so `resolve` folds both automatically.
fn client() -> FakeReconClient {
    FakeReconClient {
        orders: Vec::new(),
        fills: vec![FillReport {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            trade_id: "ext-t1".into(),
            venue_order_id: "v9".into(),
            client_order_id: None,
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 5,
        }],
        positions: vec![PositionStatusReport {
            venue: VENUE.into(),
            symbol: "ETHUSDT".into(),
            position_side: PositionSide::Both,
            qty: 2.0,
            avg_px: 50.0,
            ts: 5,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        }],
        balance: None,
    }
}

/// Running the same `run_pass` twice against the same venue reports: pass 1 finds divergences and
/// synthesizes events; folding those into the engine brings local state in line with the venue.
/// Pass 2, run against a `LocalView` built AFTER folding pass 1's output, must find NOTHING left
/// to reconcile — `Recon { events: [], alerts: [] }` exactly.
#[test]
fn second_pass_against_updated_state_is_empty() {
    let recon_client = client();
    let policy = ReconPolicy::default();

    // --- Pass 1: engine starts empty, diverges from the venue on both fill and position. ---
    let mut eng = engine();
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local_before = LocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        qty_tol: 1e-9,
        cash: Default::default(),
    };
    let pass1 = run_pass(&recon_client, 0, &local_before, None, &policy, None, false).unwrap();
    // MissingFill (no client_order_id) synthesizes OrderAccepted + Fill (2 events);
    // PositionOnlyExternal (from-flat, no crossing) synthesizes one Fill. Total 3.
    assert_eq!(pass1.events.len(), 3, "{:?}", pass1.events);
    assert!(pass1.alerts.is_empty(), "default policy synthesizes everything");

    // Fold pass 1's events — this is what brings local state in line with the venue.
    for e in &pass1.events {
        eng.on_event(e, &mut Outbox::default());
    }

    // --- Pass 2: same venue reports, but now against the engine's POST-pass-1 local view. ---
    let owned = eng.local_view();
    let local_after = owned.as_view();
    let pass2 = run_pass(&recon_client, 0, &local_after, None, &policy, None, false).unwrap();

    assert_eq!(
        pass2,
        Recon::default(),
        "a second pass against the identical venue reports, after folding the first pass's \
         output, must find nothing left to reconcile"
    );
    assert!(pass2.events.is_empty());
    assert!(pass2.alerts.is_empty());
}

/// Folding pass 2's (empty) output is a true no-op: engine state after pass 1 + pass 2 is
/// identical to engine state after pass 1 alone.
#[test]
fn folding_the_empty_second_pass_does_not_change_engine_state() {
    let recon_client = client();
    let policy = ReconPolicy::default();

    let mut eng = engine();
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local_before = LocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        qty_tol: 1e-9,
        cash: Default::default(),
    };
    let pass1 = run_pass(&recon_client, 0, &local_before, None, &policy, None, false).unwrap();
    for e in &pass1.events {
        eng.on_event(e, &mut Outbox::default());
    }

    let closed_pnls_after_pass1 = eng.account.closed_pnls.clone();
    let positions_after_pass1 = eng.account.positions.clone();

    let owned = eng.local_view();
    let pass2 = run_pass(&recon_client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    for e in &pass2.events {
        eng.on_event(e, &mut Outbox::default());
    }

    assert_eq!(eng.account.closed_pnls, closed_pnls_after_pass1);
    assert_eq!(eng.account.positions, positions_after_pass1);
    // Nothing new landed in seen_trade_ids either — pass 2 synthesized zero fills.
    assert_eq!(eng.local_view().seen_trade_ids.len(), owned.seen_trade_ids.len());
}

// --- generate_missing_orders: UnknownOrder adoption idempotency ---

/// A TERMINAL, already-filled external order whose fills fell outside the lookback (no fill
/// reports at all) — the one case the adoption arm synthesizes for.
fn client_with_unknown_order() -> FakeReconClient {
    FakeReconClient {
        orders: vec![OrderStatusReport {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            venue_order_id: "v-unk".into(),
            client_order_id: None, // externally-placed — no local coid to match on
            side: 1,
            order_type: "LIMIT".into(),
            qty: 1.0,
            filled_qty: 1.0,
            avg_px: 100.0,
            status: "FILLED".into(),
            ts: 5,
        }],
        fills: Vec::new(),
        positions: Vec::new(),
        balance: None,
    }
}

/// The recurring-pass no-op invariant, end-to-end through `run_pass`: the venue keeps reporting
/// the adopted order (the registry never learns its coid, so `diff` re-detects the same
/// `UnknownOrder` every pass), but once pass 1's adoption fill has folded, its deterministic
/// trade_id sits in the engine's seen set and `resolve` skips the divergence entirely
/// (`AdoptionCase::AlreadyAdopted`) — pass 2 is `Recon::default()`: nothing re-folds (so the
/// `dropped_unknown_coid` anomaly counter cannot inflate pass-over-pass) and no alert re-raises.
#[test]
fn a_second_pass_after_adoption_synthesizes_nothing() {
    let recon_client = client_with_unknown_order();
    let policy = ReconPolicy::default();

    let mut eng = engine();
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local_before = LocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        qty_tol: 1e-9,
        cash: Default::default(),
    };
    let pass1 = run_pass(&recon_client, 0, &local_before, None, &policy, None, true).unwrap();
    // OrderAccepted (coid-less adoption) + Fill (filled_qty 1.0) — mirrors MissingFill's shape.
    assert_eq!(pass1.events.len(), 2, "{:?}", pass1.events);
    for e in &pass1.events {
        eng.on_event(e, &mut Outbox::default());
    }
    let dropped_unknown_after_pass1 = eng.dropped_unknown_coid;
    let closed_pnls_after_pass1 = eng.account.closed_pnls.clone();
    let positions_after_pass1 = eng.account.positions.clone();

    // Pass 2: the SAME venue report is still an UnknownOrder divergence, but the adoption fill is
    // in seen_trade_ids now — resolve emits NOTHING (events OR alerts).
    let owned = eng.local_view();
    let pass2 = run_pass(&recon_client, 0, &owned.as_view(), None, &policy, None, true).unwrap();
    assert_eq!(pass2, Recon::default(), "recurring pass must be a true no-op");

    // And engine state is untouched by the (empty) second pass — including the anomaly counter.
    assert_eq!(eng.dropped_unknown_coid, dropped_unknown_after_pass1);
    assert_eq!(eng.account.closed_pnls, closed_pnls_after_pass1);
    assert_eq!(eng.account.positions, positions_after_pass1);
}

/// Fix-round-1 CRITICAL-2, end-to-end through `run_pass` + a real engine fold: an external order
/// filling PROGRESSIVELY across passes (its fills inside the lookback, arriving as MissingFill
/// divergences) must converge local state to venue truth with NO phantom close leg and NO
/// double-booked qty — the live UnknownOrder synthesizes nothing while the fill lane books each
/// execution once under its real trade-id.
#[test]
fn progressively_filling_unknown_order_converges_without_phantom_legs() {
    let policy = ReconPolicy::default();
    let mut eng = engine();

    let order_report = |filled: f64, status: &str| OrderStatusReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        venue_order_id: "v-unk".into(),
        client_order_id: None,
        side: 1,
        order_type: "LIMIT".into(),
        qty: 2.0,
        filled_qty: filled,
        avg_px: 100.0,
        status: status.into(),
        ts: 5,
    };
    let fill_report = |trade_id: &'static str, ts: i64| FillReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        trade_id: trade_id.into(),
        venue_order_id: "v-unk".into(),
        client_order_id: None,
        side: 1,
        last_qty: 1.0,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts,
    };
    let position_report = |qty: f64| PositionStatusReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        position_side: PositionSide::Both,
        qty,
        avg_px: 100.0,
        ts: 7,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    };

    // Pass 1: order live, filled 1 of 2; the execution is a MissingFill; the venue position (+1)
    // is fully explained by it.
    let pass1_client = FakeReconClient {
        orders: vec![order_report(1.0, "PARTIALLY_FILLED")],
        fills: vec![fill_report("t-real-1", 5)],
        positions: vec![position_report(1.0)],
        balance: None,
    };
    let owned = eng.local_view();
    let pass1 = run_pass(&pass1_client, 0, &owned.as_view(), None, &policy, None, true).unwrap();
    for e in &pass1.events {
        eng.on_event(e, &mut Outbox::default());
    }
    let pos_after_1 = eng.local_view().positions.get(&(SYMBOL.into(), "BOTH".into())).copied();
    assert_eq!(pos_after_1, Some(1.0), "pass 1 books exactly the real fill");

    // Pass 2: filled 2 of 2 (now FILLED); the second execution arrives as its own MissingFill.
    // Local must move 1 → 2 (venue truth), with no close leg / realized PnL and no EXT-ORD print.
    let pass2_client = FakeReconClient {
        orders: vec![order_report(2.0, "FILLED")],
        fills: vec![fill_report("t-real-1", 5), fill_report("t-real-2", 6)],
        positions: vec![position_report(2.0)],
        balance: None,
    };
    let owned = eng.local_view();
    let pass2 = run_pass(&pass2_client, 0, &owned.as_view(), None, &policy, None, true).unwrap();
    for e in &pass2.events {
        if let vike_model::events::Event::Fill(f) = e {
            assert!(!f.trade_id.starts_with("EXT-"), "no synthetic legs in pass 2: {e:?}");
        }
        eng.on_event(e, &mut Outbox::default());
    }
    let pos_after_2 = eng.local_view().positions.get(&(SYMBOL.into(), "BOTH".into())).copied();
    assert_eq!(pos_after_2, Some(2.0), "local converges to the venue's 2.0");
    assert!(eng.account.closed_pnls.is_empty(), "no phantom close leg / bogus realized PnL");

    // Pass 3: identical venue snapshot — nothing left to reconcile at all.
    let owned = eng.local_view();
    let pass3 = run_pass(&pass2_client, 0, &owned.as_view(), None, &policy, None, true).unwrap();
    assert_eq!(pass3, Recon::default(), "converged: recurring pass is a true no-op");
}

#[test]
fn unknown_order_adoption_is_byte_identical_when_the_flag_is_off() {
    // The inert-default doctrine, proven end-to-end through run_pass: with
    // generate_missing_orders=false (the default), an UnknownOrder report synthesizes nothing at
    // all — pre-generate_missing_orders behavior exactly.
    let recon_client = client_with_unknown_order();
    let policy = ReconPolicy::default();
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local = LocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        qty_tol: 1e-9,
        cash: Default::default(),
    };
    let recon = run_pass(&recon_client, 0, &local, None, &policy, None, false).unwrap();
    assert_eq!(recon, Recon::default(), "flag off: UnknownOrder proposes nothing");
}
