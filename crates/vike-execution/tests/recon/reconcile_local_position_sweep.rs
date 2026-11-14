//! The local-side position sweep (`diff` step 5 → `Divergence::OrphanLocalPosition`), end-to-end
//! through `run_pass` — the `fetch -> diff -> resolve` composition the offline `ReconcileClientFake`
//! and the live `ReconcileManager` share. The position twin of `reconcile_local_order_sweep.rs`.
//!
//! A locally-open position whose venue row is omitted ENTIRELY raises a divergence; a diff over
//! VENUE rows only would report "reconciled" while local still believed it held risk the venue
//! never confirmed.
//!
//! It deliberately flattens nothing: `ReconcileLocalView` carries the signed net qty and no basis price, so
//! there is no defensible price for a synthetic close, and an absent row is evidence-by-absence
//! that an incomplete or symbol-scoped fetch produces as readily as a closed position. So the
//! divergence folds ZERO events under every policy and only decides no-op-vs-surface
//! (`vike_execution::recon::resolve`'s module doc is the authority).

use vike_execution::recon::{
    ReconcileClientFake, ReconcileDivergenceKind, ReconcileLocalView, ReconcileMode,
    ReconcilePassOutcome, ReconcilePolicy, run_pass,
};
use vike_execution::{Engine, EventBusOutbox, EventHandler};
use vike_model::events::LiquiditySide;
use vike_model::{FillReport, PositionStatusReport};

use vike_execution::testing::EngineBuilder;

use crate::reconcile_local_order_sweep::sweep_pass;

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";

fn engine() -> Engine<vike_execution::testing::RecordingClient> {
    EngineBuilder { venue: VENUE.into(), symbol: SYMBOL.into(), ..Default::default() }.build()
}

fn position_report(symbol: &str, qty: f64) -> PositionStatusReport {
    PositionStatusReport {
        timestamp: 9,
        ..vike_execution::testing::position_report(VENUE, symbol, qty)
    }
}

/// An engine holding +1 `BTCUSDT`, seeded as the live core seeds one: a `MissingFill` pass whose
/// synthesized events fold through `on_event` (`reconcile_idempotency.rs`'s idiom).
fn engine_holding_one_btc() -> Engine<vike_execution::testing::RecordingClient> {
    let mut engine_under_test = engine();
    let seed = ReconcileClientFake {
        fills: vec![FillReport {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            trade_id: "seed-t1".into(),
            venue_order_id: "v-seed".into(),
            order_client_id: None,
            side: 1,
            last_qty: 1.0,
            last_price: 100.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            timestamp: 5,
        }],
        ..Default::default()
    };
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = std::collections::HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local = ReconcileLocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        quantity_tolerance: 1e-9,
        cash: Default::default(),
    };
    let seeded =
        run_pass(&seed, 0, &local, None, &ReconcilePolicy::default(), None, false).unwrap();
    for e in &seeded.events {
        engine_under_test.on_event(e, &mut EventBusOutbox::default());
    }
    assert_eq!(
        engine_under_test
            .local_view()
            .positions
            .get(&(SYMBOL.to_string(), vike_model::events::PositionSide::Both))
            .copied(),
        Some(1.0),
        "fixture precondition: the engine really holds +1 BTCUSDT"
    );
    engine_under_test
}

/// The venue's report carries rows (so the fetch demonstrably works) but none for the symbol local
/// is long: under `hybrid` that surfaces exactly one held, dedup-keyed `OrphanLocalPosition` alert
/// and folds nothing, so the position is never silently flattened.
#[test]
fn venue_report_omitting_a_local_position_raises_a_held_alert_under_hybrid() {
    let mut engine = engine_holding_one_btc();
    let client = ReconcileClientFake {
        positions: vec![position_report("ETHUSDT", 0.0)],
        ..Default::default()
    };

    let pass = sweep_pass(&engine, &client, &ReconcilePolicy::hybrid());

    assert!(pass.events.is_empty(), "never auto-flattens: {:?}", pass.events);
    assert_eq!(pass.alerts.len(), 1, "{:?}", pass.alerts);
    let alert = &pass.alerts[0];
    assert_eq!(alert.kind, ReconcileDivergenceKind::OrphanLocalPosition);
    assert_eq!(alert.dedup_key.as_deref(), Some("position:BTCUSDT:BOTH"));
    assert!(alert.proposed_events.is_empty(), "investigate-only: nothing for a confirm to fold");
    assert!(alert.detail.contains("BTCUSDT"), "{}", alert.detail);
    // ...but it names the local row a CONFIRM drops (`vike_core`'s `confirm_reconcile`).
    assert_eq!(
        alert.orphan_position,
        Some(vike_execution::recon::ReconcileOrphanPosition {
            symbol: SYMBOL.to_string(),
            position_side: vike_model::events::PositionSide::Both,
            local_quantity: 1.0,
            venue_qty: 0.0,
        }),
    );

    // Folding the pass's (empty) event list leaves the position exactly where it was.
    for e in &pass.events {
        engine.on_event(e, &mut EventBusOutbox::default());
    }
    assert_eq!(
        engine
            .local_view()
            .positions
            .get(&(SYMBOL.to_string(), vike_model::events::PositionSide::Both))
            .copied(),
        Some(1.0),
        "an unreported position must not be flattened by the pass"
    );
    assert!(engine.account.closed_pnls.is_empty(), "no fabricated realized PnL");
}

/// THE GATE: an EMPTY position report is indistinguishable from "no position concept / fetch not
/// implemented" (binance SPOT's `fetch_position_status_reports` returns `Ok(Vec::new())`
/// unconditionally), so it sweeps NOTHING; otherwise every spot inventory row would raise a
/// permanent, un-healable alert every pass.
#[test]
fn an_empty_venue_position_report_leaves_a_local_position_untouched() {
    let engine = engine_holding_one_btc();
    let client = ReconcileClientFake::default(); // no orders, no fills, NO position rows
    let pass = sweep_pass(&engine, &client, &ReconcilePolicy::hybrid());
    assert_eq!(pass, ReconcilePassOutcome::default(), "empty position report sweeps nothing");
}

/// NO DOUBLE-REPORTING: a venue row present for the local key, including an explicitly FLAT one
/// (several `ReconcileClient`s emit them rather than filtering zero rows), stays on the `PositionDrift`
/// path: under `hybrid` its healing leg folds and NO `OrphanLocalPosition` alert is raised.
#[test]
fn a_venue_flat_row_for_the_local_symbol_stays_on_the_position_drift_path() {
    let engine = engine_holding_one_btc();
    let client =
        ReconcileClientFake { positions: vec![position_report(SYMBOL, 0.0)], ..Default::default() };
    let pass = sweep_pass(&engine, &client, &ReconcilePolicy::hybrid());

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

/// The sweep is inert on an empty local book; the venue-anchored legs are unaffected.
#[test]
fn an_empty_local_book_with_venue_rows_is_unchanged() {
    let client = ReconcileClientFake {
        positions: vec![position_report("ETHUSDT", 2.0)],
        ..Default::default()
    };
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = std::collections::HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local = ReconcileLocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        quantity_tolerance: 1e-9,
        cash: Default::default(),
    };
    let pass = run_pass(&client, 0, &local, None, &ReconcilePolicy::hybrid(), None, false).unwrap();
    // PositionOnlyExternal is quarantined under hybrid (no local origin).
    assert!(pass.events.is_empty());
    assert_eq!(pass.alerts.len(), 1);
    assert_eq!(pass.alerts[0].kind, ReconcileDivergenceKind::PositionOnlyExternal);
}

/// RECURRING PASSES: nothing folds, so every pass re-raises the IDENTICAL alert and the runtime
/// refreshes the one held row per (venue, symbol, side) via `dedup_key`; equal passes make that
/// refresh a true no-op.
#[test]
fn a_recurring_pass_reproduces_the_identical_dedup_keyed_alert() {
    let engine = engine_holding_one_btc();
    let client = ReconcileClientFake {
        positions: vec![position_report("ETHUSDT", 0.0)],
        ..Default::default()
    };
    let policy = ReconcilePolicy::hybrid();

    let owned = engine.local_view();
    let pass1 = run_pass(&client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    let pass2 = run_pass(&client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    assert_eq!(pass1, pass2, "a recurring pass is identical — the held row refreshes in place");
}

/// The `synthesize` policy's NAMED residual: the kind folds its (empty) event list, so nothing
/// folds AND nothing alerts. An operator who wants this blind spot surfaced runs
/// `hybrid`/`quarantine`.
#[test]
fn synthesize_policy_is_silent_about_an_unreported_local_position() {
    let engine = engine_holding_one_btc();
    let client = ReconcileClientFake {
        positions: vec![position_report("ETHUSDT", 0.0)],
        ..Default::default()
    };
    let pass = sweep_pass(&engine, &client, &ReconcilePolicy::default());
    assert_eq!(
        pass,
        ReconcilePassOutcome::default(),
        "synthesize: a documented no-op, not an alert"
    );
}

/// `quarantine` holds it exactly like `hybrid`: one dedup-keyed, event-free alert.
#[test]
fn quarantine_policy_holds_the_same_orphan_alert() {
    let engine = engine_holding_one_btc();
    let client = ReconcileClientFake {
        positions: vec![position_report("ETHUSDT", 0.0)],
        ..Default::default()
    };
    let policy = ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() };
    let pass = sweep_pass(&engine, &client, &policy);
    assert!(pass.events.is_empty());
    assert_eq!(pass.alerts.len(), 1);
    assert_eq!(pass.alerts[0].kind, ReconcileDivergenceKind::OrphanLocalPosition);
    assert_eq!(pass.alerts[0].dedup_key.as_deref(), Some("position:BTCUSDT:BOTH"));
}
