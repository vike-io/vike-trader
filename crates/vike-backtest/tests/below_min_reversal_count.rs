//! ⚠ **Proves `SimBroker::below_min_reversals` can FIRE** — without this, a zero on a real profile
//! means nothing, and the whole point of the counter is that a zero should be trustworthy enough to
//! close a ledger entry with.
//!
//! The divergence: a below-min REVERSAL. The order opposes the position, EXCEEDS it (so it flips
//! through flat and `vike_model::is_covered_reduce` is false), and falls below a venue floor.
//! `SimBroker::apply_fill`'s opening/closing split is DIRECTION-ONLY, so it calls the whole thing
//! closing and executes it entire — while the live `RiskGate` requires COVERAGE and denies. The
//! backtest is the permissive side.
//!
//! ⚠ This counts, it does not refuse. `crates/vike-exec/src/risk.rs`'s "KNOWN RESIDUAL DIVERGENCE"
//! is the other end, and the fix is deferred because it changes fill decisions AND the obvious
//! repair does not converge the two engines (live denies the whole order; splitting the flip makes
//! the backtest flatten). Measure first, decide after.

use vike_backtest::{EngineParams, SimBroker, StrategyEngine};
use vike_model::{Bar, Broker, Strategy, SymbolProperties};

const SYM: &str = "SYM0";
const VENUE: &str = "TEST";
const PX: f64 = 100.0;

fn bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| Bar {
            ts: 60_000 * (i as i64 + 1),
            open: PX,
            high: PX,
            low: PX,
            close: PX,
            volume: 1_000_000.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

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

impl Strategy<SimBroker> for OpenReduceReverse {
    fn on_bar(&mut self, b: &mut SimBroker, _bar: &Bar) {
        self.step += 1;
        match self.step {
            1 => b.submit_market(SYM, 1, self.open),
            2 => b.submit_market(SYM, -1, self.reduce),
            3 => b.submit_market(SYM, -1, self.flip),
            _ => {}
        }
    }
}

/// `min_qty` sits above the flip size, so the flip is below-min while the OPENING leg clears it.
fn run(open: f64, reduce: f64, flip: f64, min_qty: f64) -> (u64, f64) {
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
    let params = EngineParams {
        cash: 1_000_000.0,
        default_venue: Some(VENUE.to_string()),
        properties: Some(std::sync::Arc::new(move |_v: &str, _s: &str, _ts: i64| Some(grid))),
        ..Default::default()
    };
    let mut e = StrategyEngine::new(
        vec![(SYM.to_string(), bars(6))],
        OpenReduceReverse { open, reduce, flip, step: 0 },
        params,
    );
    let _ = e.run();
    (e.core.below_min_reversals, e.core.sym[0].pos.size)
}

/// ⚠ **THE NON-VACUITY PROOF.** Open LONG 3.0 (clears the 2.0 floor), scale out 2.5 — a COVERED
/// reduce, so floors are bypassed and it fills, leaving 0.5 — then SELL 1.5. That last order opposes
/// the position, EXCEEDS it (0.5), and is below the 2.0 floor. Live denies it; the backtest fills it
/// whole and ends SHORT 1.0.
///
/// The position assertion is what shows the divergence is REAL rather than merely counted: the book
/// reaches a state live could not have reached from that order.
#[test]
fn a_below_min_reversal_is_counted_and_still_fills() {
    let (count, pos) = run(3.0, 2.5, 1.5, 2.0);
    assert_eq!(count, 1, "the below-min reversal must be counted");
    assert!(
        (pos - -1.0).abs() < 1e-9,
        "and it still FILLS WHOLE (that is the divergence, not the counter): position {pos}"
    );
}

/// A COVERED reduce is not a reversal — it shrinks the position without crossing flat, so
/// `is_covered_reduce` is true and the live gate's floor bypass applies to it too. Both engines
/// agree, and nothing is counted. (Here the third leg reduces 0.4 of the remaining 0.5.)
#[test]
fn a_covered_reduce_is_not_counted() {
    let (count, pos) = run(3.0, 2.5, 0.4, 2.0);
    assert_eq!(count, 0, "a covered reduce is not a divergence — live permits it too");
    assert!((pos - 0.1).abs() < 1e-9, "position shrinks without flipping: {pos}");
}

/// A reversal ABOVE the floor is legal on both sides. This is the guard that stops the counter
/// firing on every flip — without it a non-zero reading on a real profile would mean nothing.
#[test]
fn a_reversal_above_the_floor_is_not_counted() {
    let (count, pos) = run(3.0, 2.5, 1.5, 0.5);
    assert_eq!(count, 0, "above the floor both engines fill it");
    assert!((pos - -1.0).abs() < 1e-9, "same flip, same end state: {pos}");
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
    let (count, pos) = run(1.0, 0.0, 1.5, 2.0);
    assert_eq!(count, 0, "no position was ever established — both legs are below the floor");
    assert_eq!(pos, 0.0, "the book stays flat: {pos}");
}
