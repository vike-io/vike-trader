//! Gate for the opt-in shortfall-cover margin model (`StrategyEngineConfig::shortfall_cover`, the rule on
//! `vike_simulator::ShortfallCover`'s doc). The margin call: the trigger at the bar's adverse extreme and
//! at its open, the size (`cover` 4 and 1), the floor to the quantity step and the whole unit below
//! it, the cap at the position, the fill price with slippage, a short, the counter a strategy reads
//! in `on_fill`. The fill gate: an entry the account cannot margin refused, a flip refused whole,
//! the entry of a close-then-enter reversal let through, an entry stacked on one that filled before it in
//! the step let through when it fits the step's start. The walk: a bar with sub-bars judged at each end of
//! its walk. And the default left as it was, and the tick path's refusal.
//!
//! Most tests: one symbol, 1,000 of cash, 45 units entered at bar 1's open of 100 (4.5 times the
//! account), a 20% margin. Bar 1 is flat at 100: equity 1,000 against a margin of 900, so nothing
//! fires there. Bar 2 opens at 99 (equity 955 against 891, still carried) and its adverse extreme
//! decides.

use vike_marketdata::{Bar, TickQuote};
use vike_model::{Fill, OrderDrop, Strategy};
use vike_simulator::{
    OrderBrokerSimulator, ShortfallCover, StrategyEngine, StrategyEngineConfig, TickEvent,
};

const SYM: &str = "AAA";
const T0: i64 = 1_700_000_000_000;
const SIZE: f64 = 45.0;

fn bar(i: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    vike_simulator::test_support::bar(T0 + i * 60_000, o, h, l, c, 0.0)
}

fn flat(i: i64) -> Bar {
    bar(i, 100.0, 100.0, 100.0, 100.0)
}

/// Submits each `(step, side, size)` market order on its step (filled at the next bar's open) and
/// records every fill with the counter as `on_fill` saw it.
struct Orders {
    plan: Vec<(usize, i32, f64)>,
    fills: Vec<(Fill, u64)>,
}

impl Strategy<OrderBrokerSimulator> for Orders {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        for &(step, side, size) in &self.plan {
            if step == ctx.index {
                ctx.submit(SYM, side, size, 0.0, true, None);
            }
        }
    }

    fn on_fill(&mut self, ctx: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push((fill.clone(), ctx.shortfall_cover_fills));
    }
}

fn rule(cover: f64, step: f64) -> ShortfallCover {
    ShortfallCover { cover, margin_long: 0.2, margin_short: 0.2, step }
}

fn run_tape(
    plan: Vec<(usize, i32, f64)>,
    tape: Vec<Bar>,
    params: StrategyEngineConfig,
) -> StrategyEngine<Orders> {
    let params = StrategyEngineConfig { cash: 1_000.0, ..params };
    let mut engine = StrategyEngine::new(
        vec![(SYM.to_string(), tape)],
        Orders { plan, fills: Vec::new() },
        params,
    );
    engine.run();
    engine
}

/// 45 units on `side`; bar 2 opens at 99 and reaches `low` and `high`.
fn run(side: i32, low: f64, high: f64, params: StrategyEngineConfig) -> StrategyEngine<Orders> {
    run_tape(vec![(0, side, SIZE)], vec![flat(0), flat(1), bar(2, 99.0, high, low, 99.0)], params)
}

fn with(rule: ShortfallCover) -> StrategyEngineConfig {
    StrategyEngineConfig { shortfall_cover: Some(rule), ..Default::default() }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn a_long_the_account_still_carries_is_not_cut() {
    // low 98: equity -3500 + 4410 = 910, margin 882, available +28.
    let engine = run(1, 98.0, 99.0, with(rule(4.0, 0.01)));
    assert_eq!(engine.core.position_of(SYM).size, SIZE);
    assert!(engine.core.trades.is_empty());
    assert_eq!(engine.core.shortfall_cover_fills, 0);
}

#[test]
fn a_long_short_of_margin_at_its_low_is_cut_four_times_the_shortfall() {
    // low 97: equity -3500 + 4365 = 865, margin 873, available -8. Just enough: 8 / 0.2 / 97 =
    // 0.41237…, floored to 0.01 is 0.41, four times that 1.64, sold at the low.
    let engine = run(1, 97.0, 99.0, with(rule(4.0, 0.01)));
    let left = engine.core.position_of(SYM).size;
    assert!(close(left, SIZE - 1.64), "left {left}");
    assert_eq!(engine.core.trades.len(), 1, "the cut is a closed trade of its own");
    let t = &engine.core.trades[0];
    assert!(close(t.size, 1.64), "cut {}", t.size);
    assert_eq!(t.exit_price, 97.0);
    assert_eq!(t.exit_timestamp, T0 + 120_000);
    assert_eq!(engine.core.shortfall_cover_fills, 1);
}

#[test]
fn cover_one_closes_just_enough() {
    let engine = run(1, 97.0, 99.0, with(rule(1.0, 0.01)));
    assert!(close(engine.core.position_of(SYM).size, SIZE - 0.41));
}

#[test]
fn the_shortfall_floors_to_the_step_before_the_cover() {
    // 0.41237… floors to 0.4 at a step of 0.1, so four times is 1.6; with no step it is 4 · 0.41237….
    let engine = run(1, 97.0, 99.0, with(rule(4.0, 0.1)));
    assert!(close(engine.core.position_of(SYM).size, SIZE - 1.6));
    let raw = 4.0 * (8.0 / 0.2 / 97.0);
    let engine = run(1, 97.0, 99.0, with(rule(4.0, 0.0)));
    assert!(close(engine.core.position_of(SYM).size, SIZE - raw));
}

#[test]
fn a_shortfall_below_one_step_cuts_one_whole_unit() {
    // 0.41237… is below a step of 1, so the cut is one unit, not four times nothing.
    let engine = run(1, 97.0, 99.0, with(rule(4.0, 1.0)));
    assert_eq!(engine.core.position_of(SYM).size, SIZE - 1.0);
    assert_eq!(engine.core.shortfall_cover_fills, 1);
}

#[test]
fn the_cut_never_exceeds_the_position() {
    // low 80: available 100 - 720 = -620, just enough 38.75, four times 155 > 45.
    let engine = run(1, 80.0, 99.0, with(rule(4.0, 0.01)));
    assert_eq!(engine.core.position_of(SYM).size, 0.0);
    assert!(close(engine.core.trades[0].size, SIZE));
}

#[test]
fn the_open_entry_commission_counts_against_the_margin() {
    // At 0.1% the entry pays 4.5, so cash is -3504.5. At a low of 97.3 the account is 1.7 short
    // (-3504.5 + 36 · 97.3) where it would be 2.8 over without the commission: just enough
    // 1.7 / 0.2 / 97.3 = 0.0873…, floored 0.08, four times 0.32.
    let engine =
        run(1, 97.3, 99.0, StrategyEngineConfig { fee_rate: 0.001, ..with(rule(4.0, 0.01)) });
    assert!(close(engine.core.position_of(SYM).size, SIZE - 0.32));
    assert_eq!(engine.core.shortfall_cover_fills, 1);
}

#[test]
fn a_bar_that_opens_short_of_margin_is_cut_at_its_open() {
    // open 96: equity -3500 + 4320 = 820, margin 864, available -44. Just enough 44 / 0.2 / 96 =
    // 2.2916…, floored 2.29, four times 9.16, sold at the open. At the low of 95 the rest carries:
    // cash -2620.64 + 35.84 · 95 = 784.16 against 680.96.
    let tape = vec![flat(0), flat(1), bar(2, 96.0, 96.0, 95.0, 95.5)];
    let engine = run_tape(vec![(0, 1, SIZE)], tape, with(rule(4.0, 0.01)));
    assert!(close(engine.core.position_of(SYM).size, SIZE - 9.16));
    assert_eq!(engine.core.trades.len(), 1);
    assert_eq!(engine.core.trades[0].exit_price, 96.0);
}

#[test]
fn the_cut_fills_at_the_extreme_less_the_slippage_and_pays_the_commission() {
    let params =
        StrategyEngineConfig { slippage_amount: 0.5, fee_rate: 0.001, ..with(rule(4.0, 0.01)) };
    let engine = run(1, 97.0, 99.0, params);
    let (cut, counted) = engine.strategy.fills.last().expect("the cut reached on_fill");
    assert_eq!(cut.side, -1);
    assert_eq!(cut.price, 96.5, "at the low, less the fixed slippage");
    assert!(close(cut.fee, cut.size * 96.5 * 0.001), "fee {}", cut.fee);
    assert_eq!(*counted, 1, "counted before on_fill saw it");
    assert_eq!(engine.strategy.fills[0].1, 0, "the entry is no margin call");
}

#[test]
fn a_short_is_judged_at_its_high_and_bought_back_there() {
    // high 103: equity 5500 - 4635 = 865, margin 927, available -62. Just enough: 62 / 0.2 / 103 =
    // 3.0097…, floored to 3.00, four times 12, bought at the high.
    let engine = run(-1, 99.0, 103.0, with(rule(4.0, 0.01)));
    assert!(close(engine.core.position_of(SYM).size, -(SIZE - 12.0)));
    let t = &engine.core.trades[0];
    assert!(!t.is_long);
    assert_eq!(t.exit_price, 103.0);
    // With slippage the entry sells at 99.5, so the account is 842.5 against 927 and the cut
    // 4 · 4.10, bought at the high plus the slippage.
    let params = StrategyEngineConfig { slippage_amount: 0.5, ..with(rule(4.0, 0.01)) };
    let engine = run(-1, 99.0, 103.0, params);
    assert!(close(engine.core.position_of(SYM).size, -(SIZE - 16.4)));
    assert_eq!(engine.core.trades[0].exit_price, 103.5);
    // The long rate is not the short's: a short margined at 0 is never called.
    let free = ShortfallCover { margin_short: 0.0, ..rule(4.0, 0.01) };
    let engine = run(-1, 99.0, 103.0, with(free));
    assert_eq!(engine.core.position_of(SYM).size, -SIZE);
}

/// A 100% margin on a flat tape at 100, 1,000 of cash.
fn full_margin() -> StrategyEngineConfig {
    with(ShortfallCover { margin_long: 1.0, margin_short: 1.0, ..rule(4.0, 0.01) })
}

fn flat_tape() -> Vec<Bar> {
    (0..4).map(flat).collect()
}

#[test]
fn an_entry_the_account_cannot_margin_is_refused_at_its_fill() {
    // 11 at 100 needs 1,100 against 1,000; 10 needs exactly 1,000 and fills.
    let engine = run_tape(vec![(0, 1, 11.0)], flat_tape(), full_margin());
    assert_eq!(engine.core.position_of(SYM).size, 0.0);
    let OrderDrop { symbol: symbol_name, reason, size, .. } =
        engine.core.dropped.last().expect("the refusal is recorded");
    assert_eq!((symbol_name.as_str(), reason.as_str(), *size), (SYM, "insufficient-margin", 11.0));
    let engine = run_tape(vec![(0, 1, 10.0)], flat_tape(), full_margin());
    assert_eq!(engine.core.position_of(SYM).size, 10.0);
    // Unset, the same entry fills.
    let engine = run_tape(vec![(0, 1, 11.0)], flat_tape(), StrategyEngineConfig::default());
    assert_eq!(engine.core.position_of(SYM).size, 11.0);
}

#[test]
fn a_flip_the_account_cannot_margin_is_refused_whole() {
    // Long 9 (cash 100, equity 1,000), then a sell of 20 that would leave a short of 11 needing
    // 1,100: refused whole, the long kept.
    let engine = run_tape(vec![(0, 1, 9.0), (1, -1, 20.0)], flat_tape(), full_margin());
    assert_eq!(engine.core.position_of(SYM).size, 9.0);
    assert!(engine.core.trades.is_empty());
}

#[test]
fn the_entry_of_a_close_then_enter_reversal_fills_and_is_margin_called() {
    // Long 9, then in one step a sell of 9 that closes it and a sell of 11 that opens a short needing
    // 1,100 against 1,000. The close came first, so the entry fills, and the open's margin call cuts
    // it: available 1,000 - 1,100 = -100, just enough 1.0, four times 4.
    let engine =
        run_tape(vec![(0, 1, 9.0), (1, -1, 9.0), (1, -1, 11.0)], flat_tape(), full_margin());
    assert!(close(engine.core.position_of(SYM).size, -7.0));
    assert_eq!(engine.core.shortfall_cover_fills, 1);
}

#[test]
fn unset_it_leaves_the_bar_path_as_it_was() {
    let engine = run(1, 80.0, 99.0, StrategyEngineConfig::default());
    assert_eq!(engine.core.position_of(SYM).size, SIZE);
    assert!(engine.core.trades.is_empty());
}

struct Idle;

impl Strategy<OrderBrokerSimulator> for Idle {
    fn on_quote_tick(&mut self, _context: &mut OrderBrokerSimulator, _q: &TickQuote) {}
}

#[test]
#[should_panic(expected = "bar-path margin call")]
fn the_tick_path_refuses_it() {
    let mut engine =
        StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], Idle, with(rule(4.0, 0.01)));
    let ticks: Vec<TickEvent> =
        vec![vike_simulator::test_support::quote(SYM, 1, 100.0, 100.0, 1.0, 1.0)];
    engine.run_ticks(&[(SYM.to_string(), ticks)]);
}

/// A rule built by hand is held to the numbers a profile is (`ShortfallCover::validate`): a cover of
/// zero would cut nothing, and a negative margin would carry an account below nothing.
#[test]
#[should_panic(expected = "StrategyEngineConfig::shortfall_cover: cover must be > 0, got 0")]
fn a_rule_with_a_number_it_cannot_be_is_refused_at_construction() {
    StrategyEngine::new(vec![(SYM.to_string(), vec![flat(0)])], Idle, with(rule(0.0, 0.01)));
}

#[test]
fn every_number_of_a_rule_is_checked() {
    assert_eq!(rule(4.0, 0.01).validate(), Ok(()));
    assert_eq!(rule(4.0, 0.0).validate(), Ok(()), "a step of 0 floors nothing, as documented");
    assert!(rule(f64::NAN, 0.01).validate().unwrap_err().starts_with("cover must be > 0"));
    assert!(rule(4.0, -0.01).validate().unwrap_err().starts_with("step must be a number >= 0"));
    let short = ShortfallCover { margin_short: f64::INFINITY, ..rule(4.0, 0.01) };
    assert!(short.validate().unwrap_err().starts_with("margin_short must be a number >= 0"));
}

// ---- the walk inside a bar, and a stack of entries ----

/// A bar's walk as the lab's harness feeds it: `[open]`, `[open → near]`, `[near → far]`, `[far → close]`, all on the
/// bar's own time, near being the extreme nearer the open.
fn walk_of(b: &Bar) -> Vec<Bar> {
    let [_, near, far, _] = b.path_points();
    let step = |from: f64, to: f64| {
        vike_simulator::test_support::bar(b.timestamp, from, from.max(to), from.min(to), to, 0.0)
    };
    vec![step(b.open, b.open), step(b.open, near), step(near, far), step(far, b.close)]
}

/// Market orders and resting limits on their step: `(step, side, size, limit)`.
struct Scripted {
    plan: Vec<(usize, i32, f64, Option<f64>)>,
}

impl Strategy<OrderBrokerSimulator> for Scripted {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        for &(step, side, size, limit) in &self.plan {
            if step != ctx.index {
                continue;
            }
            match limit {
                None => ctx.submit(SYM, side, size, 0.0, true, None),
                Some(px) => ctx.submit_limit(SYM, side, size, px, 0.0, true, None),
            }
        }
    }
}

fn run_walked(
    plan: Vec<(usize, i32, f64, Option<f64>)>,
    tape: Vec<Bar>,
    walked: bool,
    rule: ShortfallCover,
) -> StrategyEngine<Scripted> {
    let subs: Vec<Bar> = if walked { tape.iter().flat_map(walk_of).collect() } else { Vec::new() };
    let params = StrategyEngineConfig {
        cash: 1_000.0,
        shortfall_cover: Some(rule),
        granular_by_symbol: if walked { vec![(SYM.to_string(), subs)] } else { Vec::new() },
        ..Default::default()
    };
    let mut engine = StrategyEngine::new(vec![(SYM.to_string(), tape)], Scripted { plan }, params);
    engine.run();
    engine
}

#[test]
fn a_walked_bar_is_judged_at_each_end_of_its_walk() {
    // 45 long from bar 1's open of 100, a sell limit for 43 at 101. Bar 2 opens at 99, walks down to its low of 97
    // (the extreme nearer the open) and then up to 103. At the low the account is 8 short, as in the first law: the
    // cut is 1.64 at 97. The limit then sells 43 of the 43.36 left at 101.
    let tape = vec![flat(0), flat(1), bar(2, 99.0, 103.0, 97.0, 102.0)];
    let plan = vec![(0, 1, SIZE, None), (1, -1, 43.0, Some(101.0))];
    let engine = run_walked(plan.clone(), tape.clone(), true, rule(4.0, 0.01));
    assert_eq!(engine.core.shortfall_cover_fills, 1);
    let cut = &engine.core.trades[0];
    assert!(close(cut.size, 1.64), "cut {}", cut.size);
    assert_eq!(cut.exit_price, 97.0);
    assert_eq!(engine.core.trades.len(), 2, "the cut, then the target");
    assert_eq!(engine.core.trades[1].exit_price, 101.0);
    assert!(close(engine.core.position_of(SYM).size, SIZE - 1.64 - 43.0));
    // Without a walk the bar is judged once, with the target already filled: nothing is left to call.
    let engine = run_walked(plan, tape, false, rule(4.0, 0.01));
    assert_eq!(engine.core.shortfall_cover_fills, 0);
    assert_eq!(engine.core.trades.len(), 1);
}

#[test]
fn a_second_entry_that_fits_against_the_start_of_the_bar_is_filled_and_called() {
    // Two buys of 6 at bar 1's open on 1,000 at a 100% margin: the first needs 600, the second would leave 12 and
    // need 1,200. It fits against the bar's start (6 against 1,000), so it fills, and the open's margin call cuts
    // the stack: 200 short, just enough 2.0, four times that is 8.
    let engine = run_tape(vec![(0, 1, 6.0), (0, 1, 6.0)], flat_tape(), full_margin());
    assert!(close(engine.core.position_of(SYM).size, 4.0), "{}", engine.core.position_of(SYM).size);
    assert_eq!(engine.core.shortfall_cover_fills, 1);
    // A single entry of 12 does not fit against the start of its own bar and is refused whole.
    let engine = run_tape(vec![(0, 1, 12.0)], flat_tape(), full_margin());
    assert_eq!(engine.core.position_of(SYM).size, 0.0);
}
