//! The zero-crossing PnL-parity gate. `recon::resolve` synthesizes a
//! `PositionDrift`/`PositionOnlyExternal` divergence that crosses zero into TWO fill legs (close
//! then open — see `synth_position_legs` in `src/recon/resolve.rs`) so `Account::fold`
//! (`compute_fill`) books realized PnL on the close leg, not the open leg. This test proves that
//! 2-leg synthesis books the SAME realized PnL and ends in the SAME position as the economically
//! equivalent REAL fill sequence a venue could have reported instead (a single sell that itself
//! crosses zero, `compute_fill`'s `FillKind::Flip` path). A second-oracle gate, mirroring
//! `crates/vike-simulator/tests/parity/engine_kernel_parity.rs`.

use vike_execution::recon::{ReconcileDivergence, ReconcilePolicy, resolve};
use vike_execution::{Engine, EventBusOutbox};
use vike_model::PositionStatusReport;
use vike_model::events::{Event, FillEvent, LiquiditySide, PositionSide};

use vike_execution::testing::EngineBuilder;

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";

fn engine() -> Engine<vike_execution::testing::RecordingClient> {
    EngineBuilder { venue: VENUE.into(), symbol: SYMBOL.into(), ..Default::default() }.build()
}

/// Fold one real fill (a plain buy/sell, no order lifecycle needed for PnL booking).
fn fold_real_fill(
    engine: &mut Engine<vike_execution::testing::RecordingClient>,
    outbox: &mut EventBusOutbox,
    trade_id: &'static str,
    side: i32,
    qty: f64,
    px: f64,
    ts: i64,
) {
    use vike_execution::EventHandler;
    let fill = FillEvent {
        trade_id: trade_id.into(),
        order_client_id: format!("real-{trade_id}"),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: ts,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    engine.on_event(&Event::Fill(fill), outbox);
}

/// Fold every synthesized event from a `resolve()` output through the engine (Fill events only —
/// the zero-crossing legs synth_position_legs produces are always bare Fill events, never
/// lifecycle wraps).
fn fold_events(
    engine: &mut Engine<vike_execution::testing::RecordingClient>,
    outbox: &mut EventBusOutbox,
    events: &[Event],
) {
    use vike_execution::EventHandler;
    for e in events {
        engine.on_event(e, outbox);
    }
}

/// Zero-crossing synthesis: local holds LONG 2 @ 100 (a real fill); the venue reports the net
/// position is now SHORT -1 @ average_price 110 — a sign crossing. `resolve` synthesizes two legs: close
/// +2 @ 110 (realizes PnL), then open -1 @ 110 (flat cost basis). Fold those into engine A.
///
/// Engine B folds the ECONOMICALLY EQUIVALENT real fill sequence instead: buy 2 @ 100, then a
/// single real SELL of 3 @ 110 — which itself crosses zero inside `compute_fill` (`FillKind::Flip`):
/// it closes the 2 long units at 110 (same realized formula) and opens -1 @ 110 in one fold call.
/// Both must end with identical `account.closed_pnls` and net position.
#[test]
fn zero_crossing_synthesis_matches_equivalent_real_fill_pnl() {
    let mut outbox = EventBusOutbox::default();

    // --- Engine A: real buy, then the synthesized zero-crossing legs. ---
    let mut engine_a = engine();
    fold_real_fill(&mut engine_a, &mut outbox, "real-buy", 1, 2.0, 100.0, 1);

    let divergence = ReconcileDivergence::PositionDrift {
        report: PositionStatusReport {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            position_side: PositionSide::Both,
            qty: -1.0,
            average_price: 110.0,
            timestamp: 99,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        },
        local_quantity: 2.0,
    };
    let recon = resolve(vec![divergence], &ReconcilePolicy::default(), None, None);
    // Sanity: this IS the zero-crossing 2-leg path, not the 1-leg same-sign path.
    assert_eq!(recon.events.len(), 2, "zero-crossing must synthesize exactly two legs");
    if let Event::Fill(f) = &recon.events[0] {
        assert_eq!(f.last_qty, 2.0, "first leg closes the full local long");
        assert_eq!(f.side, -1);
        assert_eq!(f.last_price, 110.0);
    } else {
        panic!("expected a Fill event for the close leg");
    }
    if let Event::Fill(f) = &recon.events[1] {
        assert_eq!(f.last_qty, 1.0, "second leg opens the venue-reported short");
        assert_eq!(f.side, -1);
        assert_eq!(f.last_price, 110.0);
    } else {
        panic!("expected a Fill event for the open leg");
    }
    fold_events(&mut engine_a, &mut outbox, &recon.events);

    // --- Engine B: real buy, then one real sell of 3 @ 110 (close 2 + open 1 short). ---
    let mut engine_b = engine();
    fold_real_fill(&mut engine_b, &mut outbox, "real-buy", 1, 2.0, 100.0, 1);
    fold_real_fill(&mut engine_b, &mut outbox, "real-sell-3", -1, 3.0, 110.0, 99);

    // Parity: identical realized PnL history, exact f64 equality — no tolerance.
    assert_eq!(
        engine_a.account.closed_pnls, engine_b.account.closed_pnls,
        "2-leg zero-crossing synthesis must book the SAME realized PnL as the equivalent real fill"
    );
    // Both should have realized exactly (110-100)*2 = 20.0 on the close.
    assert_eq!(engine_a.account.closed_pnls, vec![20.0]);

    // Parity: identical resulting position (symbol, side) -> (size, average_price).
    let pos_a = engine_a.account.positions.get(&(VENUE.into(), SYMBOL.into(), PositionSide::Both));
    let pos_b = engine_b.account.positions.get(&(VENUE.into(), SYMBOL.into(), PositionSide::Both));
    assert_eq!(pos_a, pos_b);
    let pos_a = pos_a.expect("engine A must have a position after the flip");
    assert_eq!(pos_a.size, -1.0);
    assert_eq!(pos_a.average_price, 110.0);
}

/// Same-sign drift (no zero crossing) is already covered structurally by
/// `resolve::tests::same_sign_drift_is_one_fill`; this asserts the PnL-parity property for it too
/// (add-in-direction never realizes PnL, either via synthesis or a real add fill).
#[test]
fn same_sign_drift_matches_equivalent_real_add_fill() {
    let mut outbox = EventBusOutbox::default();

    let mut engine_a = engine();
    fold_real_fill(&mut engine_a, &mut outbox, "real-buy", 1, 1.0, 100.0, 1);
    let divergence = ReconcileDivergence::PositionDrift {
        report: PositionStatusReport {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            position_side: PositionSide::Both,
            qty: 3.0,
            average_price: 100.0, // same price as the reference test in resolve.rs
            timestamp: 7,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        },
        local_quantity: 1.0,
    };
    let recon = resolve(vec![divergence], &ReconcilePolicy::default(), None, None);
    assert_eq!(recon.events.len(), 1, "same-sign drift is one leg, not a zero-crossing");
    fold_events(&mut engine_a, &mut outbox, &recon.events);

    let mut engine_b = engine();
    fold_real_fill(&mut engine_b, &mut outbox, "real-buy", 1, 1.0, 100.0, 1);
    fold_real_fill(&mut engine_b, &mut outbox, "real-add", 1, 2.0, 100.0, 7);

    assert_eq!(engine_a.account.closed_pnls, engine_b.account.closed_pnls);
    assert!(engine_a.account.closed_pnls.is_empty(), "an add never realizes PnL");
    let pos_a = engine_a.account.positions.get(&(VENUE.into(), SYMBOL.into(), PositionSide::Both));
    let pos_b = engine_b.account.positions.get(&(VENUE.into(), SYMBOL.into(), PositionSide::Both));
    assert_eq!(pos_a, pos_b);
}
