//! Task 18 (P4): the zero-crossing PnL-parity gate. `recon::resolve` synthesizes a
//! `PositionDrift`/`PositionOnlyExternal` divergence that crosses zero into TWO fill legs (close
//! then open — see `synth_position_legs` in `src/recon/resolve.rs`) so `Account::fold`
//! (`compute_fill`) books realized PnL on the close leg, not the open leg. This test proves that
//! 2-leg synthesis books the SAME realized PnL and ends in the SAME position as the economically
//! equivalent REAL fill sequence a venue could have reported instead (a single sell that itself
//! crosses zero, `compute_fill`'s `FillKind::Flip` path). A second-oracle gate, mirroring
//! `crates/vike-backtest/tests/parity/engine_kernel_parity.rs`.

use vike_exec::recon::{Divergence, ReconPolicy, resolve};
use vike_exec::{Account, BalanceMode, ExecutionEngine, Outbox, RiskGate, RiskLimits};
use vike_model::PositionStatusReport;
use vike_model::events::{Event, FillEvent, LiquiditySide, PositionSide};

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

/// Fold one real fill (a plain buy/sell, no order lifecycle needed for PnL booking).
fn fold_real_fill(
    eng: &mut ExecutionEngine<vike_exec::testing::RecordingClient>,
    outbox: &mut Outbox,
    trade_id: &'static str,
    side: i32,
    qty: f64,
    px: f64,
    ts: i64,
) {
    use vike_exec::EventHandler;
    let fill = FillEvent {
        trade_id: trade_id.into(),
        client_order_id: format!("real-{trade_id}"),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: LiquiditySide::Taker,
        ts,
        mark_price: None,
        position_side: PositionSide::Both,
    };
    eng.on_event(&Event::Fill(fill), outbox);
}

/// Fold every synthesized event from a `resolve()` output through the engine (Fill events only —
/// the zero-crossing legs synth_position_legs produces are always bare Fill events, never
/// lifecycle wraps).
fn fold_events(
    eng: &mut ExecutionEngine<vike_exec::testing::RecordingClient>,
    outbox: &mut Outbox,
    events: &[Event],
) {
    use vike_exec::EventHandler;
    for e in events {
        eng.on_event(e, outbox);
    }
}

/// Zero-crossing synthesis: local holds LONG 2 @ 100 (a real fill); the venue reports the net
/// position is now SHORT -1 @ avg_px 110 — a sign crossing. `resolve` synthesizes two legs: close
/// +2 @ 110 (realizes PnL), then open -1 @ 110 (flat cost basis). Fold those into engine A.
///
/// Engine B folds the ECONOMICALLY EQUIVALENT real fill sequence instead: buy 2 @ 100, then a
/// single real SELL of 3 @ 110 — which itself crosses zero inside `compute_fill` (`FillKind::Flip`):
/// it closes the 2 long units at 110 (same realized formula) and opens -1 @ 110 in one fold call.
///
/// Both must end with identical `account.closed_pnls` and identical net position — proving the
/// 2-leg synthesis books realized PnL exactly like the single real fill a venue actually would
/// report.
#[test]
fn zero_crossing_synthesis_matches_equivalent_real_fill_pnl() {
    let mut outbox = Outbox::default();

    // --- Engine A: real buy, then the synthesized zero-crossing legs. ---
    let mut eng_a = engine();
    fold_real_fill(&mut eng_a, &mut outbox, "real-buy", 1, 2.0, 100.0, 1);

    let divergence = Divergence::PositionDrift {
        report: PositionStatusReport {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            position_side: PositionSide::Both,
            qty: -1.0,
            avg_px: 110.0,
            ts: 99,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        },
        local_qty: 2.0,
    };
    let recon = resolve(vec![divergence], &ReconPolicy::default(), None, None);
    // Sanity: this IS the zero-crossing 2-leg path, not the 1-leg same-sign path.
    assert_eq!(recon.events.len(), 2, "zero-crossing must synthesize exactly two legs");
    if let Event::Fill(f) = &recon.events[0] {
        assert_eq!(f.last_qty, 2.0, "first leg closes the full local long");
        assert_eq!(f.side, -1);
        assert_eq!(f.last_px, 110.0);
    } else {
        panic!("expected a Fill event for the close leg");
    }
    if let Event::Fill(f) = &recon.events[1] {
        assert_eq!(f.last_qty, 1.0, "second leg opens the venue-reported short");
        assert_eq!(f.side, -1);
        assert_eq!(f.last_px, 110.0);
    } else {
        panic!("expected a Fill event for the open leg");
    }
    fold_events(&mut eng_a, &mut outbox, &recon.events);

    // --- Engine B: real buy, then one real sell of 3 @ 110 (close 2 + open 1 short). ---
    let mut eng_b = engine();
    fold_real_fill(&mut eng_b, &mut outbox, "real-buy", 1, 2.0, 100.0, 1);
    fold_real_fill(&mut eng_b, &mut outbox, "real-sell-3", -1, 3.0, 110.0, 99);

    // Parity: identical realized PnL history, exact f64 equality — no tolerance.
    assert_eq!(
        eng_a.account.closed_pnls, eng_b.account.closed_pnls,
        "2-leg zero-crossing synthesis must book the SAME realized PnL as the equivalent real fill"
    );
    // Both should have realized exactly (110-100)*2 = 20.0 on the close.
    assert_eq!(eng_a.account.closed_pnls, vec![20.0]);

    // Parity: identical resulting position (symbol, side) -> (size, avg_px).
    let pos_a = eng_a.account.positions.get(&(VENUE.into(), SYMBOL.into(), "BOTH".into()));
    let pos_b = eng_b.account.positions.get(&(VENUE.into(), SYMBOL.into(), "BOTH".into()));
    assert_eq!(pos_a, pos_b);
    let pos_a = pos_a.expect("engine A must have a position after the flip");
    assert_eq!(pos_a.size, -1.0);
    assert_eq!(pos_a.avg_px, 110.0);
}

/// Same-sign drift (no zero crossing) is already covered structurally by
/// `resolve::tests::same_sign_drift_is_one_fill`; this asserts the PnL-parity property for it too
/// (add-in-direction never realizes PnL, either via synthesis or a real add fill).
#[test]
fn same_sign_drift_matches_equivalent_real_add_fill() {
    let mut outbox = Outbox::default();

    let mut eng_a = engine();
    fold_real_fill(&mut eng_a, &mut outbox, "real-buy", 1, 1.0, 100.0, 1);
    let divergence = Divergence::PositionDrift {
        report: PositionStatusReport {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            position_side: PositionSide::Both,
            qty: 3.0,
            avg_px: 100.0, // same price as the reference test in resolve.rs
            ts: 7,
            margin_mode: Default::default(),
            isolated_margin: None,
            delta: None,
        },
        local_qty: 1.0,
    };
    let recon = resolve(vec![divergence], &ReconPolicy::default(), None, None);
    assert_eq!(recon.events.len(), 1, "same-sign drift is one leg, not a zero-crossing");
    fold_events(&mut eng_a, &mut outbox, &recon.events);

    let mut eng_b = engine();
    fold_real_fill(&mut eng_b, &mut outbox, "real-buy", 1, 1.0, 100.0, 1);
    fold_real_fill(&mut eng_b, &mut outbox, "real-add", 1, 2.0, 100.0, 7);

    assert_eq!(eng_a.account.closed_pnls, eng_b.account.closed_pnls);
    assert!(eng_a.account.closed_pnls.is_empty(), "an add never realizes PnL");
    let pos_a = eng_a.account.positions.get(&(VENUE.into(), SYMBOL.into(), "BOTH".into()));
    let pos_b = eng_b.account.positions.get(&(VENUE.into(), SYMBOL.into(), "BOTH".into()));
    assert_eq!(pos_a, pos_b);
}
