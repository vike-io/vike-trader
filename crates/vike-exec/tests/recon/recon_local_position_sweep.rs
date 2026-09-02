//! The local-side position sweep (`diff` step 5 → `Divergence::OrphanLocalPosition`), end-to-end
//! through `run_pass` — the `fetch -> diff -> resolve` composition the offline `FakeReconClient`
//! and the live `ReconManager` share.
//!
//! THE BLIND SPOT THIS CLOSES: `diff`'s position leg iterated VENUE rows only, so a locally-open
//! position whose venue row was omitted ENTIRELY raised no divergence at all — the pass reported
//! "reconciled" while local still believed it held risk the venue never confirmed. (#830 corrected
//! a venue doc that claimed the diff engine already reconciled an absent row as closed; this file
//! proves the engine side now does something about it.)
//!
//! What it deliberately does NOT do is flatten anything: `LocalView` carries the signed net qty
//! and no basis price, so there is no defensible price for a synthetic close, and an absent row is
//! evidence-by-absence that an incomplete/symbol-scoped venue fetch produces just as readily as a
//! genuinely closed position. So the divergence folds ZERO events under every policy and only
//! decides no-op-vs-surface — see `vike_exec::recon::resolve`'s module doc, the authority.

use vike_exec::recon::{
    run_pass, DivergenceKind, FakeReconClient, LocalView, Recon, ReconMode, ReconPolicy,
};
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate, RiskLimits,
};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FillReport, PositionStatusReport};

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";

fn engine() -> ExecutionEngine<vike_exec::testing::RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        vike_exec::testing::RecordingClient::default(),
        VENUE,
        SYMBOL,
    )
}

fn position_report(symbol: &str, qty: f64) -> PositionStatusReport {
    PositionStatusReport {
        venue: VENUE.into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        qty,
        avg_px: 100.0,
        ts: 9,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    }
}

/// An engine holding a genuine +1 `BTCUSDT` position, seeded the way the live core seeds one — a
/// `MissingFill` reconcile pass whose synthesized events are folded through `on_event` (the same
/// idiom `recon_idempotency.rs` uses, so the seed itself is already gated behavior).
fn engine_holding_one_btc() -> ExecutionEngine<vike_exec::testing::RecordingClient> {
    let mut eng = engine();
    let seed = FakeReconClient {
        fills: vec![FillReport {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            trade_id: "seed-t1".into(),
            venue_order_id: "v-seed".into(),
            client_order_id: None,
            side: 1,
            last_qty: 1.0,
            last_px: 100.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            ts: 5,
        }],
        ..Default::default()
    };
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = std::collections::HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local = LocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        qty_tol: 1e-9,
        cash: Default::default(),
    };
    let seeded = run_pass(&seed, 0, &local, None, &ReconPolicy::default(), None, false).unwrap();
    for e in &seeded.events {
        eng.on_event(e, &mut Outbox::default());
    }
    assert_eq!(
        eng.local_view().positions.get(&(SYMBOL.to_string(), "BOTH".to_string())).copied(),
        Some(1.0),
        "fixture precondition: the engine really holds +1 BTCUSDT"
    );
    eng
}

/// THE FIX. The venue's position report carries rows (so the fetch demonstrably works) but none for
/// the symbol local is long. Under `hybrid` that now surfaces exactly one held, dedup-keyed
/// `OrphanLocalPosition` alert — and folds nothing, so the position is never silently flattened.
#[test]
fn venue_report_omitting_a_local_position_raises_a_held_alert_under_hybrid() {
    let mut eng = engine_holding_one_btc();
    let client =
        FakeReconClient { positions: vec![position_report("ETHUSDT", 0.0)], ..Default::default() };

    let owned = eng.local_view();
    let pass =
        run_pass(&client, 0, &owned.as_view(), None, &ReconPolicy::hybrid(), None, false).unwrap();

    assert!(pass.events.is_empty(), "never auto-flattens: {:?}", pass.events);
    assert_eq!(pass.alerts.len(), 1, "{:?}", pass.alerts);
    let alert = &pass.alerts[0];
    assert_eq!(alert.kind, DivergenceKind::OrphanLocalPosition);
    assert_eq!(alert.dedup_key.as_deref(), Some("position:BTCUSDT:BOTH"));
    assert!(alert.proposed_events.is_empty(), "investigate-only: nothing for a confirm to fold");
    assert!(alert.detail.contains("BTCUSDT"), "{}", alert.detail);

    // Folding the pass's (empty) event list leaves the position exactly where it was.
    for e in &pass.events {
        eng.on_event(e, &mut Outbox::default());
    }
    assert_eq!(
        eng.local_view().positions.get(&(SYMBOL.to_string(), "BOTH".to_string())).copied(),
        Some(1.0),
        "an unreported position must not be flattened by the pass"
    );
    assert!(eng.account.closed_pnls.is_empty(), "no fabricated realized PnL");
}

/// THE GATE. An EMPTY position report is indistinguishable from "this venue has no position concept
/// / the fetch is not implemented" — binance SPOT's `fetch_position_status_reports` returns
/// `Ok(Vec::new())` unconditionally — so it must sweep NOTHING. Without this, every spot inventory
/// row would raise a permanent, un-healable alert on every pass.
#[test]
fn an_empty_venue_position_report_leaves_a_local_position_untouched() {
    let eng = engine_holding_one_btc();
    let client = FakeReconClient::default(); // no orders, no fills, NO position rows
    let owned = eng.local_view();
    let pass =
        run_pass(&client, 0, &owned.as_view(), None, &ReconPolicy::hybrid(), None, false).unwrap();
    assert_eq!(pass, Recon::default(), "empty position report sweeps nothing");
}

/// NO DOUBLE-REPORTING. A venue row that IS present for the local key — including an explicitly
/// FLAT one, the shape several `ReconClient`s deliberately emit rather than filtering zero rows —
/// stays entirely on the pre-existing `PositionDrift` path: under `hybrid` (where `PositionDrift`
/// auto-applies) its healing leg folds and NO `OrphanLocalPosition` alert is raised.
#[test]
fn a_venue_flat_row_for_the_local_symbol_stays_on_the_position_drift_path() {
    let eng = engine_holding_one_btc();
    let client =
        FakeReconClient { positions: vec![position_report(SYMBOL, 0.0)], ..Default::default() };
    let owned = eng.local_view();
    let pass =
        run_pass(&client, 0, &owned.as_view(), None, &ReconPolicy::hybrid(), None, false).unwrap();

    assert!(pass.alerts.is_empty(), "the flat row is a drift, not an orphan: {:?}", pass.alerts);
    assert_eq!(pass.events.len(), 1, "{:?}", pass.events);
    match &pass.events[0] {
        vike_model::events::Event::Fill(f) => {
            assert!(f.trade_id.starts_with("EXT-POS-"), "the drift's own healing leg");
            assert_eq!(f.side, -1);
            assert_eq!(f.last_qty, 1.0);
        }
        other => panic!("expected the PositionDrift healing Fill, got {other:?}"),
    }
}

/// The sweep is inert on an empty local book — the venue-anchored legs behave exactly as before.
#[test]
fn an_empty_local_book_with_venue_rows_is_unchanged() {
    let client =
        FakeReconClient { positions: vec![position_report("ETHUSDT", 2.0)], ..Default::default() };
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = std::collections::HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local = LocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        qty_tol: 1e-9,
        cash: Default::default(),
    };
    let pass = run_pass(&client, 0, &local, None, &ReconPolicy::hybrid(), None, false).unwrap();
    // PositionOnlyExternal is quarantined under hybrid (no local origin) — the pre-existing shape.
    assert!(pass.events.is_empty());
    assert_eq!(pass.alerts.len(), 1);
    assert_eq!(pass.alerts[0].kind, DivergenceKind::PositionOnlyExternal);
}

/// RECURRING PASSES. Nothing about this divergence self-heals (no events fold, so the venue keeps
/// not reporting the row), so every pass re-raises the IDENTICAL alert. That is exactly what the
/// `dedup_key` is for: the runtime refreshes the one held row per (venue, symbol, side) instead of
/// appending. Pinning the two passes as equal is what makes that refresh a true no-op.
#[test]
fn a_recurring_pass_reproduces_the_identical_dedup_keyed_alert() {
    let eng = engine_holding_one_btc();
    let client =
        FakeReconClient { positions: vec![position_report("ETHUSDT", 0.0)], ..Default::default() };
    let policy = ReconPolicy::hybrid();

    let owned = eng.local_view();
    let pass1 = run_pass(&client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    let pass2 = run_pass(&client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    assert_eq!(pass1, pass2, "a recurring pass is identical — the held row refreshes in place");
}

/// The `synthesize` policy's NAMED residual, pinned so it cannot drift silently: with everything on
/// auto-apply the kind folds its (empty) event list, so the pass is a true no-op — nothing folds
/// AND nothing alerts. An operator who wants this blind spot surfaced runs `hybrid`/`quarantine`.
#[test]
fn synthesize_policy_is_silent_about_an_unreported_local_position() {
    let eng = engine_holding_one_btc();
    let client =
        FakeReconClient { positions: vec![position_report("ETHUSDT", 0.0)], ..Default::default() };
    let owned = eng.local_view();
    let pass =
        run_pass(&client, 0, &owned.as_view(), None, &ReconPolicy::default(), None, false).unwrap();
    assert_eq!(pass, Recon::default(), "synthesize: a documented no-op, not an alert");
}

/// `quarantine` holds it exactly like `hybrid` — the same single, dedup-keyed, event-free alert.
#[test]
fn quarantine_policy_holds_the_same_orphan_alert() {
    let eng = engine_holding_one_btc();
    let client =
        FakeReconClient { positions: vec![position_report("ETHUSDT", 0.0)], ..Default::default() };
    let policy = ReconPolicy { default: ReconMode::Quarantine, ..Default::default() };
    let owned = eng.local_view();
    let pass = run_pass(&client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    assert!(pass.events.is_empty());
    assert_eq!(pass.alerts.len(), 1);
    assert_eq!(pass.alerts[0].kind, DivergenceKind::OrphanLocalPosition);
    assert_eq!(pass.alerts[0].dedup_key.as_deref(), Some("position:BTCUSDT:BOTH"));
}
