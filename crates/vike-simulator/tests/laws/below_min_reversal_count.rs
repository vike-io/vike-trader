//! ⚠ **A below-min REVERSAL is split at the fill: its closing half executes, its opening half is
//! refused** — the cure `crates/vike-execution/src/risk.rs` names for what used to be the one floor
//! divergence. `OrderBrokerSimulator::below_min_reversals` counts the splits, and this file proves it can FIRE:
//! without that, a zero on a real profile would mean nothing.
//!
//! The case: the order opposes the position, EXCEEDS it (so it flips through flat and
//! `vike_model::is_covered_reduce` is false), and falls below a venue floor. The live `RiskGate`
//! denies the WHOLE order; the backtest used to fill it whole and now flattens. So neither engine
//! opens the far side. They still differ in the closing half, which the fill lane must not strand.

use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderPlacement, Strategy, SymbolProperties};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "SYM0";
const VENUE: &str = "TEST";
const PX: f64 = 100.0;

/// ⚠ **Three legs, and the middle one is why this is not a two-step test.**
///
/// A flip must EXCEED the position, so a below-min flip needs `|position| < flip < min_qty` — the
/// position must ALREADY be below the venue floor. An opening order cannot create that state: it
/// would be refused by the same floor. So the position has to get there another way, and the two
/// real ways are a COVERED REDUCE (floors are bypassed for those, deliberately — a closing fill must
/// always execute so a position is never stranded) or a venue RAISING its minimum over a position's
/// life, which `apply_fill`'s own comment anticipates.
///
/// This drives the first: open above the floor, scale out until what remains is below it, then
/// reverse. That is an ordinary scale-out-then-flip, not a contrived shape.
struct OpenReduceReverse {
    open: f64,
    reduce: f64,
    flip: f64,
    step: usize,
}

impl Strategy<OrderBrokerSimulator> for OpenReduceReverse {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        self.step += 1;
        match self.step {
            1 => b.place(SYM, "", 1, self.open, OrderPlacement::Market, false),
            2 => b.place(SYM, "", -1, self.reduce, OrderPlacement::Market, false),
            3 => b.place(SYM, "", -1, self.flip, OrderPlacement::Market, false),
            _ => {}
        }
    }
}

/// `min_qty` sits above the flip size, so the flip is below-min while the OPENING leg clears it.
fn run(open: f64, reduce: f64, flip: f64, min_qty: f64) -> (u64, f64, Vec<(String, f64)>) {
    let grid = SymbolProperties {
        tick_size: 0.01,
        step_size: 0.0001,
        min_qty,
        max_qty: 1e9,
        min_notional: 0.0,
        ..Default::default()
    };
    // The PIT grid source is a closure `(venue, symbol, ts) -> Option<SymbolProperties>`; this one
    // is constant in time, which is all the measurement needs.
    let params = StrategyEngineConfig {
        cash: 1_000_000.0,
        default_venue: Some(VENUE.to_string()),
        properties: Some(std::sync::Arc::new(move |_v: &str, _s: &str, _ts: i64| Some(grid))),
        ..Default::default()
    };
    let mut e = StrategyEngine::new(
        vec![(SYM.to_string(), vike_simulator::test_support::flat_minute_bars(6, PX, 1_000_000.0))],
        OpenReduceReverse { open, reduce, flip, step: 0 },
        params,
    );
    let _ = e.run();
    let dropped = e.core.dropped.iter().map(|d| (d.reason.clone(), d.size)).collect();
    (e.core.below_min_reversals, e.core.symbol[0].pos.size, dropped)
}

/// ⚠ **THE NON-VACUITY PROOF.** Open LONG 3.0 (clears the 2.0 floor), scale out 2.5 — a COVERED
/// reduce, so floors are bypassed and it fills, leaving 0.5 — then SELL 1.5. That last order opposes
/// the position, EXCEEDS it (0.5), and is below the 2.0 floor. Live denies it. The backtest closes
/// the 0.5 and refuses the 1.0 that would have opened SHORT, in `OrderBrokerSimulator::dropped`, so nothing
/// vanishes in silence.
///
/// The position assertion is what shows the far side stays shut: before the split this ended SHORT
/// 1.0, a book live could not have reached from that order.
#[test]
fn a_below_min_reversal_flattens_and_its_opening_half_is_refused() {
    let (count, pos, dropped) = run(3.0, 2.5, 1.5, 2.0);
    assert_eq!(count, 1, "the split must be counted");
    assert_eq!(pos, 0.0, "the closing half fills and the far side never opens");
    assert_eq!(dropped.len(), 1, "the opening half is refused in words: {dropped:?}");
    assert_eq!(dropped[0].0, "min_qty");
    assert!((dropped[0].1 - 1.0).abs() < 1e-9, "1.5 less the 0.5 it closed: {dropped:?}");
}

/// A COVERED reduce is not a reversal — it shrinks the position without crossing flat, so
/// `is_covered_reduce` is true and the live gate's floor bypass applies to it too. Both engines
/// agree, and nothing is counted. (Here the third leg reduces 0.4 of the remaining 0.5.)
#[test]
fn a_covered_reduce_is_not_counted() {
    let (count, pos, dropped) = run(3.0, 2.5, 0.4, 2.0);
    assert_eq!(count, 0, "a covered reduce is not a divergence — live permits it too");
    assert!((pos - 0.1).abs() < 1e-9, "position shrinks without flipping: {pos}");
    assert!(dropped.is_empty(), "{dropped:?}");
}

/// A reversal ABOVE the floor is legal on both sides. This is the guard that stops the counter
/// firing on every flip — without it a non-zero reading on a real profile would mean nothing.
#[test]
fn a_reversal_above_the_floor_is_not_counted() {
    let (count, pos, dropped) = run(3.0, 2.5, 1.5, 0.5);
    assert_eq!(count, 0, "above the floor both engines fill it");
    assert!((pos - -1.0).abs() < 1e-9, "same flip, same end state: {pos}");
    assert!(dropped.is_empty(), "{dropped:?}");
}

/// A flip whose WHOLE size clears the floor fills whole even when its opening half alone would not:
/// the venue sees one order of that size, and the live gate admits it. Only a below-min order is
/// split, which is what lets a run with `below_min_reversals == 0` stay bit-identical.
#[test]
fn a_flip_above_the_floor_fills_whole_whatever_its_opening_half_is() {
    let (count, pos, dropped) = run(3.0, 2.0, 2.5, 2.0);
    assert_eq!(count, 0, "2.5 clears the 2.0 floor");
    assert!(
        (pos - -1.5).abs() < 1e-9,
        "the 1.5 opening half is below the floor and still fills: {pos}"
    );
    assert!(dropped.is_empty(), "{dropped:?}");
}

/// ⚠ **THE REACHABILITY FACT this measurement exists to record.** An OPENING order can never create
/// the state the divergence needs: a flip must exceed the position, so `|pos| < flip < min_qty`
/// requires the position to be below the floor already — and an opening order that small is refused
/// by the same floor. Both legs drop, no position is ever held, and nothing is counted.
///
/// That is why the condition is narrow, and why a zero on a real profile is meaningful rather than
/// lucky: it takes a prior covered reduce (or a venue raising its minimum) to get there.
#[test]
fn an_opening_order_cannot_create_the_below_floor_position() {
    let (count, pos, _dropped) = run(1.0, 0.0, 1.5, 2.0);
    assert_eq!(count, 0, "no position was ever established — both legs are below the floor");
    assert_eq!(pos, 0.0, "the book stays flat: {pos}");
}
