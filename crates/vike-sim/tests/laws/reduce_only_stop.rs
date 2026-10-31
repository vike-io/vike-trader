//! `SimBroker::submit_stop_reduce` — the backtest twin of `LiveBroker::submit_stop_reduce`: a stop that
//! may only REDUCE the position it meets.
//!
//! What each test pins:
//!
//! - a flat book, or a position on the order's own side, REFUSES the verb at once, in the live core's
//!   own words (`SimBroker::dropped`), and nothing ever rests;
//! - over a position it rests, is capped to that position and closes it (it never exceeds it);
//! - the case the verb exists for: the position is closed ANOTHER way while the stop rests, and the stop
//!   that fires afterwards drops in words instead of OPENING a new position (the plain `submit_stop`
//!   beside it does open one, which is the defect).

use vike_marketdata::Bar;
use vike_model::{Fill, Strategy};
use vike_sim::{EngineParams, SimBroker, StrategyEngine};

const SYM: &str = "TEST";
/// The live core's words for a reduce-only stop that meets no position on its side
/// (`vike-core`'s `core_held_fire_qty`), which `SimBroker::dropped` carries as the reason.
const REFUSED: &str = "reduce-only: there is no position on its side to reduce";

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    crate::bars::bar(ts, o, h, l, c, 100.0)
}

fn params() -> EngineParams {
    EngineParams { cash: 100_000.0, ..EngineParams::default() }
}

/// One scripted action at one bar index.
#[derive(Clone, Copy)]
enum Act {
    /// a market order of `side`/`size`
    Enter(i32, f64),
    /// a reduce-only stop (`SimBroker::submit_stop_reduce`)
    StopReduce(i32, f64, f64),
    /// the PLAIN stop verb, for the contrast
    StopPlain(i32, f64, f64),
    /// close the whole position with a market order
    Close,
}

/// Plays `script` (bar index, action) in order and collects fills.
struct Scripted {
    script: Vec<(usize, Act)>,
    fills: Vec<Fill>,
}

impl Strategy<SimBroker> for Scripted {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        for (at, act) in self.script.clone() {
            if at != ctx.index {
                continue;
            }
            match act {
                Act::Enter(side, size) => ctx.submit(SYM, side, size, 0.0, true, None),
                Act::StopReduce(side, size, trigger) => {
                    ctx.submit_stop_reduce(SYM, side, size, trigger)
                }
                Act::StopPlain(side, size, trigger) => {
                    ctx.submit_stop(SYM, side, size, trigger, 0.0, true)
                }
                Act::Close => ctx.submit_close(SYM),
            }
        }
    }
    fn on_fill(&mut self, _ctx: &mut SimBroker, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

/// Flat until bar 2 (which crosses 95), then a calm one: nothing here may open a position.
fn crossing_bars() -> Vec<Bar> {
    vec![
        bar(1_000, 100.0, 101.0, 99.0, 100.0),
        bar(2_000, 100.0, 101.0, 99.0, 100.0),
        bar(3_000, 98.0, 99.0, 90.0, 92.0), // crosses a 95 sell stop
        bar(4_000, 92.0, 93.0, 91.0, 92.0),
    ]
}

/// Calm until bar 3, which crosses 95: a market close submitted on bar 1 has filled (bar 2's open)
/// before the crossing arrives.
fn late_crossing_bars() -> Vec<Bar> {
    vec![
        bar(1_000, 100.0, 101.0, 99.0, 100.0),
        bar(2_000, 100.0, 101.0, 99.0, 100.0),
        bar(3_000, 100.0, 101.0, 99.0, 100.0),
        bar(4_000, 98.0, 99.0, 90.0, 92.0), // crosses a 95 sell stop, position already flat
        bar(5_000, 92.0, 93.0, 91.0, 92.0),
    ]
}

fn run(script: Vec<(usize, Act)>, bars: Vec<Bar>) -> StrategyEngine<Scripted> {
    let mut eng = StrategyEngine::new(
        vec![(SYM.into(), bars)],
        Scripted { script, fills: Vec::new() },
        params(),
    );
    eng.run();
    eng
}

fn refusals(eng: &StrategyEngine<Scripted>) -> Vec<(String, f64)> {
    eng.core
        .dropped
        .iter()
        .filter(|(_, why, _, _)| why == REFUSED)
        .map(|(sym, _, size, _)| (sym.clone(), *size))
        .collect()
}

#[test]
fn a_reduce_only_stop_on_a_flat_book_is_refused_in_words_and_never_opens_anything() {
    let eng = run(vec![(0, Act::StopReduce(-1, 1.0, 95.0))], crossing_bars());

    assert_eq!(
        refusals(&eng),
        vec![(SYM.to_string(), 1.0)],
        "refused at the submit, in the core's words"
    );
    assert!(
        eng.strategy.fills.is_empty(),
        "the 95 crossing on bar 2 fills nothing: {:?}",
        eng.strategy.fills
    );
    assert_eq!(eng.core.position_of(SYM).size, 0.0, "no position was opened");
}

#[test]
fn a_reduce_only_stop_on_the_positions_own_side_is_refused_too() {
    // long 1 (entered bar 0, filled at bar 1's open), then a reduce-only BUY stop: it would ADD.
    let eng = run(
        vec![(0, Act::Enter(1, 1.0)), (1, Act::StopReduce(1, 1.0, 105.0))],
        vec![
            bar(1_000, 100.0, 100.0, 100.0, 100.0),
            bar(2_000, 100.0, 100.0, 100.0, 100.0),
            bar(3_000, 104.0, 108.0, 103.0, 107.0), // would fill a 105 buy stop
        ],
    );

    assert_eq!(refusals(&eng).len(), 1, "refused: {:?}", eng.core.dropped);
    assert_eq!(eng.strategy.fills.len(), 1, "only the entry filled: {:?}", eng.strategy.fills);
    assert_eq!(eng.core.position_of(SYM).size, 1.0, "the position did not grow");
}

#[test]
fn over_a_position_it_rests_is_capped_to_it_and_closes_it() {
    // long 1; a reduce-only sell stop for 5 at 95 is capped to the 1 held, and the bar that crosses it closes.
    let eng =
        run(vec![(0, Act::Enter(1, 1.0)), (1, Act::StopReduce(-1, 5.0, 95.0))], crossing_bars());

    let fills = &eng.strategy.fills;
    assert_eq!(fills.len(), 2, "entry + the stop: {fills:?}");
    assert_eq!(
        (fills[1].side, fills[1].size),
        (-1, 1.0),
        "capped to the position, not the 5 asked"
    );
    assert!(fills[1].price <= 95.0, "filled at the crossing: {}", fills[1].price);
    assert_eq!(eng.core.position_of(SYM).size, 0.0, "flat, not short");
    assert!(refusals(&eng).is_empty(), "nothing was refused: {:?}", eng.core.dropped);
}

#[test]
fn a_stop_that_fires_after_the_position_was_closed_another_way_drops_instead_of_opening_one() {
    // long 1; bar 1: the reduce-only stop rests AND the strategy closes by market (fills at bar 2's open, no
    // crossing); bar 3 crosses 95 with the position already flat.
    let script = |stop: Act| vec![(0, Act::Enter(1, 1.0)), (1, stop), (1, Act::Close)];

    let reduce = run(script(Act::StopReduce(-1, 1.0, 95.0)), late_crossing_bars());
    assert_eq!(reduce.core.position_of(SYM).size, 0.0, "still flat: the stop did not open a short");
    assert_eq!(
        reduce.strategy.fills.len(),
        2,
        "entry + the close only: {:?}",
        reduce.strategy.fills
    );
    assert_eq!(
        refusals(&reduce),
        vec![(SYM.to_string(), 1.0)],
        "and it said why: {:?}",
        reduce.core.dropped
    );

    // The contrast, and the defect: the PLAIN stop of the same terms opens a short.
    let plain = run(script(Act::StopPlain(-1, 1.0, 95.0)), late_crossing_bars());
    assert_eq!(
        plain.core.position_of(SYM).size,
        -1.0,
        "a plain stop fires onto the flat book and OPENS"
    );
}
