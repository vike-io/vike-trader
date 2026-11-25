//! The reconcile idempotency gate: running the SAME pass twice against the SAME venue reports has
//! no additional effect once pass 1's synthesized events have folded (fills dedup on `trade_id`,
//! `Engine::on_event`'s `seen_trade_ids` guard; a matching position no longer diverges,
//! `diff`'s `quantity_tolerance` check). A retried reconcile driver or a cadence poll re-observing the same
//! venue state must not double-book PnL or re-synthesize events. Under test: `run_pass`
//! (`src/recon/client.rs`), the `fetch -> diff -> resolve` composition the offline
//! `ReconcileClientFake` and the live `ReconcileManager` share.

use std::collections::HashSet;

use vike_execution::recon::{
    ReconcileClientFake, ReconcileLocalView, ReconcilePassOutcome, ReconcilePolicy, run_pass,
};
use vike_execution::{Engine, EventBusOutbox, EventHandler};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

use vike_execution::testing::EngineBuilder;

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";

fn engine() -> Engine<vike_execution::testing::RecordingClient> {
    EngineBuilder {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        // The fixture's external position leg is on ETHUSDT, not the mounted BTCUSDT, and
        // `accepts_symbol` gates which fills fold into `account.positions`: without this the
        // synthesized ETHUSDT fill would be dropped and the proof would hold for the wrong reason.
        extra_symbols: vec!["ETHUSDT".to_string()],
        ..Default::default()
    }
    .build()
}

/// The venue reports: one external fill (never seen locally) plus a position that only exists on
/// the venue side (`PositionOnlyExternal`): a "we reconnected and missed some activity" snapshot.
/// The default (all-Synthesize) policy folds both automatically.
fn client() -> ReconcileClientFake {
    ReconcileClientFake {
        orders: Vec::new(),
        fills: vec![FillReport {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            trade_id: "ext-t1".into(),
            venue_order_id: "v9".into(),
            order_client_id: None,
            side: 1,
            last_qty: 1.0,
            last_price: 100.0,
            commission: 0.0,
            commission_asset: "USDT".into(),
            liquidity_side: LiquiditySide::Taker,
            timestamp: 5,
        }],
        positions: vec![PositionStatusReport {
            venue: VENUE.into(),
            symbol: "ETHUSDT".into(),
            position_side: PositionSide::Both,
            qty: 2.0,
            average_price: 50.0,
            timestamp: 5,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        }],
        balance: None,
    }
}

/// Running the same `run_pass` twice against the same venue reports: pass 1 finds divergences and
/// synthesizes events; folding those into the engine brings local state in line with the venue.
/// Pass 2, run against a `ReconcileLocalView` built AFTER folding pass 1's output, must find NOTHING left
/// to reconcile — `ReconcilePassOutcome { events: [], alerts: [] }` exactly.
#[test]
fn second_pass_against_updated_state_is_empty() {
    let recon_client = client();
    let policy = ReconcilePolicy::default();

    // --- Pass 1: engine starts empty, diverges from the venue on both fill and position. ---
    let mut engine_under_test = engine();
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local_before = ReconcileLocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        quantity_tolerance: 1e-9,
        cash: Default::default(),
    };
    let pass1 = run_pass(&recon_client, 0, &local_before, None, &policy, None, false).unwrap();
    // MissingFill (no order_client_id) synthesizes OrderAccepted + Fill (2 events);
    // PositionOnlyExternal (from-flat, no crossing) synthesizes one Fill. Total 3.
    assert_eq!(pass1.events.len(), 3, "{:?}", pass1.events);
    assert!(pass1.alerts.is_empty(), "default policy synthesizes everything");

    // Fold pass 1's events — this is what brings local state in line with the venue.
    for e in &pass1.events {
        engine_under_test.on_event(e, &mut EventBusOutbox::default());
    }

    // --- Pass 2: same venue reports, but now against the engine's POST-pass-1 local view. ---
    let owned = engine_under_test.local_view();
    let local_after = owned.as_view();
    let pass2 = run_pass(&recon_client, 0, &local_after, None, &policy, None, false).unwrap();

    assert_eq!(
        pass2,
        ReconcilePassOutcome::default(),
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
    let policy = ReconcilePolicy::default();

    let mut engine_under_test = engine();
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local_before = ReconcileLocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        quantity_tolerance: 1e-9,
        cash: Default::default(),
    };
    let pass1 = run_pass(&recon_client, 0, &local_before, None, &policy, None, false).unwrap();
    for e in &pass1.events {
        engine_under_test.on_event(e, &mut EventBusOutbox::default());
    }

    let closed_pnls_after_pass1 = engine_under_test.account.closed_pnls.clone();
    let positions_after_pass1 = engine_under_test.account.positions.clone();

    let owned = engine_under_test.local_view();
    let pass2 = run_pass(&recon_client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    for e in &pass2.events {
        engine_under_test.on_event(e, &mut EventBusOutbox::default());
    }

    assert_eq!(engine_under_test.account.closed_pnls, closed_pnls_after_pass1);
    assert_eq!(engine_under_test.account.positions, positions_after_pass1);
    // Nothing new landed in seen_trade_ids either — pass 2 synthesized zero fills.
    assert_eq!(engine_under_test.local_view().seen_trade_ids.len(), owned.seen_trade_ids.len());
}

// --- generate_missing_orders: UnknownOrder adoption idempotency ---

/// A TERMINAL, already-filled external order whose fills fell outside the lookback (no fill
/// reports at all) — the one case the adoption arm synthesizes for.
fn client_with_unknown_order() -> ReconcileClientFake {
    ReconcileClientFake {
        orders: vec![OrderStatusReport {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            venue_order_id: "v-unk".into(),
            order_client_id: None, // externally-placed — no local order client id to match on
            side: 1,
            order_type: vike_model::OrderType::Limit,
            qty: 1.0,
            filled_qty: 1.0,
            average_price: 100.0,
            status: vike_model::OrderStatus::Filled,
            timestamp: 5,
            price: None,
            trigger_price: None,
            trail: None,
            trail_percent: None,
        }],
        fills: Vec::new(),
        positions: Vec::new(),
        balance: None,
    }
}

/// The recurring-pass no-op invariant, end-to-end through `run_pass`: the venue keeps reporting
/// the adopted order (the registry never learns its order client id, so `diff` re-detects the same
/// `UnknownOrder` every pass), but once pass 1's adoption fill has folded, its deterministic
/// trade_id sits in the engine's seen set and `resolve` skips the divergence entirely
/// (`AdoptionCase::AlreadyAdopted`) — pass 2 is `ReconcilePassOutcome::default()`: nothing re-folds (so the
/// `events_dropped_order_unknown` anomaly counter cannot inflate pass-over-pass) and no alert re-raises.
#[test]
fn a_second_pass_after_adoption_synthesizes_nothing() {
    let recon_client = client_with_unknown_order();
    let policy = ReconcilePolicy::default();

    let mut engine_under_test = engine();
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local_before = ReconcileLocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        quantity_tolerance: 1e-9,
        cash: Default::default(),
    };
    let pass1 = run_pass(&recon_client, 0, &local_before, None, &policy, None, true).unwrap();
    // OrderAccepted (order-client-id-less adoption) + Fill (filled_qty 1.0) — mirrors MissingFill's shape.
    assert_eq!(pass1.events.len(), 2, "{:?}", pass1.events);
    for e in &pass1.events {
        engine_under_test.on_event(e, &mut EventBusOutbox::default());
    }
    let dropped_unknown_after_pass1 = engine_under_test.events_dropped_order_unknown;
    let closed_pnls_after_pass1 = engine_under_test.account.closed_pnls.clone();
    let positions_after_pass1 = engine_under_test.account.positions.clone();

    // Pass 2: the SAME venue report is still an UnknownOrder divergence, but the adoption fill is
    // in seen_trade_ids now — resolve emits NOTHING (events OR alerts).
    let owned = engine_under_test.local_view();
    let pass2 = run_pass(&recon_client, 0, &owned.as_view(), None, &policy, None, true).unwrap();
    assert_eq!(pass2, ReconcilePassOutcome::default(), "recurring pass must be a true no-op");

    // And engine state is untouched by the (empty) second pass — including the anomaly counter.
    assert_eq!(engine_under_test.events_dropped_order_unknown, dropped_unknown_after_pass1);
    assert_eq!(engine_under_test.account.closed_pnls, closed_pnls_after_pass1);
    assert_eq!(engine_under_test.account.positions, positions_after_pass1);
}

/// End to end through `run_pass` + a real engine fold: an external order filling PROGRESSIVELY
/// across passes (its fills inside the lookback, arriving as MissingFill
/// divergences) must converge local state to venue truth with NO phantom close leg and NO
/// double-booked qty — the live UnknownOrder synthesizes nothing while the fill lane books each
/// execution once under its real trade-id.
#[test]
fn progressively_filling_unknown_order_converges_without_phantom_legs() {
    let policy = ReconcilePolicy::default();
    let mut engine_under_test = engine();

    let order_report = |filled: f64, status: vike_model::OrderStatus| OrderStatusReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        venue_order_id: "v-unk".into(),
        order_client_id: None,
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 2.0,
        filled_qty: filled,
        average_price: 100.0,
        status,
        timestamp: 5,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    };
    let fill_report = |trade_id: &'static str, ts: i64| FillReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        trade_id: trade_id.into(),
        venue_order_id: "v-unk".into(),
        order_client_id: None,
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: ts,
    };
    let position_report = |qty: f64| PositionStatusReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        position_side: PositionSide::Both,
        qty,
        average_price: 100.0,
        timestamp: 7,
        margin_mode: Default::default(),
        isolated_margin: None,
        delta: None,
    };

    // Pass 1: order live, filled 1 of 2; the execution is a MissingFill; the venue position (+1)
    // is fully explained by it.
    let pass1_client = ReconcileClientFake {
        orders: vec![order_report(1.0, vike_model::OrderStatus::PartiallyFilled)],
        fills: vec![fill_report("t-real-1", 5)],
        positions: vec![position_report(1.0)],
        balance: None,
    };
    let owned = engine_under_test.local_view();
    let pass1 = run_pass(&pass1_client, 0, &owned.as_view(), None, &policy, None, true).unwrap();
    for e in &pass1.events {
        engine_under_test.on_event(e, &mut EventBusOutbox::default());
    }
    let pos_after_1 =
        engine_under_test.local_view().positions.get(&(SYMBOL.into(), PositionSide::Both)).copied();
    assert_eq!(pos_after_1, Some(1.0), "pass 1 books exactly the real fill");

    // Pass 2: filled 2 of 2 (now FILLED); the second execution arrives as its own MissingFill.
    // Local must move 1 → 2 (venue truth), with no close leg / realized PnL and no EXT-ORD print.
    let pass2_client = ReconcileClientFake {
        orders: vec![order_report(2.0, vike_model::OrderStatus::Filled)],
        fills: vec![fill_report("t-real-1", 5), fill_report("t-real-2", 6)],
        positions: vec![position_report(2.0)],
        balance: None,
    };
    let owned = engine_under_test.local_view();
    let pass2 = run_pass(&pass2_client, 0, &owned.as_view(), None, &policy, None, true).unwrap();
    for e in &pass2.events {
        if let vike_model::events::Event::Fill(f) = e {
            assert!(!f.trade_id.starts_with("EXT-"), "no synthetic legs in pass 2: {e:?}");
        }
        engine_under_test.on_event(e, &mut EventBusOutbox::default());
    }
    let pos_after_2 =
        engine_under_test.local_view().positions.get(&(SYMBOL.into(), PositionSide::Both)).copied();
    assert_eq!(pos_after_2, Some(2.0), "local converges to the venue's 2.0");
    assert!(
        engine_under_test.account.closed_pnls.is_empty(),
        "no phantom close leg / bogus realized PnL"
    );

    // Pass 3: identical venue snapshot — nothing left to reconcile at all.
    let owned = engine_under_test.local_view();
    let pass3 = run_pass(&pass2_client, 0, &owned.as_view(), None, &policy, None, true).unwrap();
    assert_eq!(pass3, ReconcilePassOutcome::default(), "converged: recurring pass is a true no-op");
}

#[test]
fn unknown_order_adoption_is_byte_identical_when_the_flag_is_off() {
    // Inert by default, end to end through run_pass: with generate_missing_orders=false (the
    // default), an UnknownOrder report synthesizes nothing at all.
    let recon_client = client_with_unknown_order();
    let policy = ReconcilePolicy::default();
    let empty_orders = indexmap::IndexMap::new();
    let empty_seen = HashSet::new();
    let empty_positions = indexmap::IndexMap::new();
    let local = ReconcileLocalView {
        venue: VENUE,
        orders: &empty_orders,
        seen_trade_ids: &empty_seen,
        positions: &empty_positions,
        quantity_tolerance: 1e-9,
        cash: Default::default(),
    };
    let recon = run_pass(&recon_client, 0, &local, None, &policy, None, false).unwrap();
    assert_eq!(recon, ReconcilePassOutcome::default(), "flag off: UnknownOrder proposes nothing");
}
