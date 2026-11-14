//! The shared margin-call folds against the code they replaced, by `to_bits`.
//!
//! `old_*` below are the two paths as they stood before `crates/vike-simulator/src/engine/liquidation.rs`
//! shared anything, kept verbatim as test-only references. Random positions (signs, wide magnitudes,
//! flat rows, a rare NaN or negative price, a shuffled symbol list, both `DecideMode`s) go through the
//! old and the new, and everything that comes out must carry the same bits: the folded numbers, the
//! candidates, then a whole margin call's cash, positions, trades and fills.
//!
//! It lives here, not in `tests/`, because the folds and the two `check_*` paths are private to the
//! engine. So a seed, if a run ever finds one, goes where proptest puts a lib unit test's: this crate's
//! `proptest-regressions/` folder, not beside the file as an integration test's does.
//!
//! A NaN compares equal to a NaN here and to nothing else: Rust does not pin a NaN's payload, and
//! every path reads a NaN the same way (`pool_breached` refuses it, the plan skips it).

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::TestRunner;
use vike_marketdata::Bar;
use vike_model::{ContractValue, Fill, LiquidationCandidate, Position};

use crate::engine::{DecideMode, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const TIMESTAMP: i64 = 1_700_000_000_000;

/// `f64` to bits, every NaN one bit pattern (the module doc).
fn bits(x: f64) -> u64 {
    if x.is_nan() { f64::NAN.to_bits() } else { x.to_bits() }
}

/// A candidate's bits. `unit` replaced the `mark` and `mult` the plan multiplied (`mark · mult`, the
/// product the copies below hand it), so the comparison holds the plan's per-unit term too.
fn candidate_bits(c: &LiquidationCandidate) -> (usize, u64, u64, u64) {
    (c.id, bits(c.size), bits(c.unit), bits(c.unrealized_pnl))
}

/// Records every fill the engine delivers, so a whole run's order of fills is compared too.
#[derive(Default)]
struct Recorder {
    fills: Vec<Fill>,
}

impl vike_model::Strategy<OrderBrokerSimulator> for Recorder {
    fn on_fill(&mut self, _broker: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

// ---- the old code, verbatim ----------------------------------------------------------------------

/// The bar path's folds as they were written inline in `check_liquidation`.
fn old_bar_folds(
    core: &OrderBrokerSimulator,
    current_index: usize,
) -> (f64, f64, f64, Vec<LiquidationCandidate>) {
    let held: Vec<usize> = core
        .cross_section()
        .iter()
        .copied()
        .filter(|&si| core.symbol[si].pos.size != 0.0)
        .collect();
    let mut eq_adv = core.cash;
    let mut notional_adv = 0.0;
    for &si in &held {
        let pos = core.symbol[si].pos;
        let bar = &core.bars[si][current_index];
        let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
        eq_adv += vike_model::signed_notional(pos.size, adverse, core.multiplier_of_index(si));
        notional_adv += vike_model::gross_notional(pos.size, adverse, core.multiplier_of_index(si));
    }
    let mut maint_adv = 0.0;
    let mut candidates: Vec<LiquidationCandidate> = Vec::with_capacity(held.len());
    for &si in &held {
        let pos = core.symbol[si].pos;
        let bar = &core.bars[si][current_index];
        let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
        let mult = core.multiplier_of_index(si);
        maint_adv += pos.size.abs() * adverse * mult * core.maint_margin;
        candidates.push(LiquidationCandidate {
            id: si,
            size: pos.size,
            unit: adverse * mult,
            unrealized_pnl: pos.size * (adverse - pos.average_price) * mult,
        });
    }
    (eq_adv, notional_adv, maint_adv, candidates)
}

/// The tick path's folds as they were written inline in `check_liquidation_tick`.
fn old_tick_folds(
    core: &OrderBrokerSimulator,
    si: usize,
    event: &Bar,
) -> (f64, f64, f64, Vec<LiquidationCandidate>) {
    let pos = core.symbol[si].pos;
    let adverse = if pos.size > 0.0 { event.low } else { event.high };
    let mut eq_adv =
        core.cash + vike_model::signed_notional(pos.size, adverse, core.multiplier_of_index(si));
    let mut notional_adv =
        vike_model::gross_notional(pos.size, adverse, core.multiplier_of_index(si));
    for other in 0..core.symbols.len() {
        if other == si {
            continue;
        }
        let p = core.symbol[other].pos;
        if p.size != 0.0 {
            eq_adv += vike_model::signed_notional(
                p.size,
                core.symbol[other].price,
                core.multiplier_of_index(other),
            );
            notional_adv += vike_model::gross_notional(
                p.size,
                core.symbol[other].price,
                core.multiplier_of_index(other),
            );
        }
    }
    let mut maint_adv = 0.0;
    let mut candidates: Vec<LiquidationCandidate> = Vec::new();
    for index in 0..core.symbols.len() {
        let p = core.symbol[index].pos;
        if p.size == 0.0 {
            continue;
        }
        let mark = if index == si { adverse } else { core.symbol[index].price };
        let mult = core.multiplier_of_index(index);
        maint_adv += p.size.abs() * mark * mult * core.maint_margin;
        candidates.push(LiquidationCandidate {
            id: index,
            size: p.size,
            unit: mark * mult,
            unrealized_pnl: p.size * (mark - p.average_price) * mult,
        });
    }
    (eq_adv, notional_adv, maint_adv, candidates)
}

impl<S: vike_model::Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    /// `check_liquidation` as it stood.
    fn old_check_liquidation(&mut self, current_index: usize) {
        if self.core.maint_margin <= 0.0 {
            return;
        }
        let held: Vec<usize> = self
            .core
            .cross_section()
            .iter()
            .copied()
            .filter(|&si| self.core.symbol[si].pos.size != 0.0)
            .collect();
        if held.is_empty() {
            return;
        }
        let mut eq_adv = self.core.cash;
        let mut notional_adv = 0.0;
        for &si in &held {
            let pos = self.core.symbol[si].pos;
            let bar = &self.core.bars[si][current_index];
            let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
            eq_adv +=
                vike_model::signed_notional(pos.size, adverse, self.core.multiplier_of_index(si));
            notional_adv +=
                vike_model::gross_notional(pos.size, adverse, self.core.multiplier_of_index(si));
        }
        if self.core.venue_style_liquidation {
            if eq_adv <= self.core.maint_margin * notional_adv {
                for &si in &held {
                    let pos = self.core.symbol[si].pos;
                    let bar = self.core.bars[si][current_index].clone();
                    let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
                    let side = vike_model::closing_side(pos.size);
                    if let Some(fill) = self.core.apply_fill(
                        si,
                        side,
                        pos.size.abs(),
                        adverse,
                        bar.timestamp,
                        false,
                        false,
                    ) {
                        self.fire_on_fill(fill);
                    }
                }
            }
            return;
        }
        let mut maint_adv = 0.0;
        let mut candidates: Vec<vike_model::LiquidationCandidate> = Vec::with_capacity(held.len());
        for &si in &held {
            let pos = self.core.symbol[si].pos;
            let bar = &self.core.bars[si][current_index];
            let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
            let mult = self.core.multiplier_of_index(si);
            maint_adv += pos.size.abs() * adverse * mult * self.core.maint_margin;
            candidates.push(vike_model::LiquidationCandidate {
                id: si,
                size: pos.size,
                unit: adverse * mult,
                unrealized_pnl: pos.size * (adverse - pos.average_price) * mult,
            });
        }
        if !vike_model::pool_breached(eq_adv, maint_adv, self.core.liquidation_buffer) {
            return;
        }
        let excess = maint_adv - eq_adv;
        for (si, qty) in
            vike_model::cross_liquidation_plan(candidates, excess, self.core.maint_margin)
        {
            let pos = self.core.symbol[si].pos;
            let bar = self.core.bars[si][current_index].clone();
            let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
            let side = vike_model::closing_side(pos.size);
            if let Some(fill) =
                self.core.apply_fill(si, side, qty, adverse, bar.timestamp, false, false)
            {
                self.fire_on_fill(fill);
            }
        }
    }

    /// `check_liquidation_tick` as it stood.
    fn old_check_liquidation_tick(&mut self, si: usize, event: &Bar) {
        if self.core.maint_margin <= 0.0 {
            return;
        }
        let pos = self.core.symbol[si].pos;
        if pos.size == 0.0 {
            return;
        }
        let adverse = if pos.size > 0.0 { event.low } else { event.high };
        let mut eq_adv = self.core.cash
            + vike_model::signed_notional(pos.size, adverse, self.core.multiplier_of_index(si));
        let mut notional_adv =
            vike_model::gross_notional(pos.size, adverse, self.core.multiplier_of_index(si));
        for other in 0..self.core.symbols.len() {
            if other == si {
                continue;
            }
            let p = self.core.symbol[other].pos;
            if p.size != 0.0 {
                eq_adv += vike_model::signed_notional(
                    p.size,
                    self.core.symbol[other].price,
                    self.core.multiplier_of_index(other),
                );
                notional_adv += vike_model::gross_notional(
                    p.size,
                    self.core.symbol[other].price,
                    self.core.multiplier_of_index(other),
                );
            }
        }
        if self.core.venue_style_liquidation {
            if notional_adv > 0.0 && eq_adv <= self.core.maint_margin * notional_adv {
                let side = vike_model::closing_side(pos.size);
                if let Some(fill) = self.core.apply_fill(
                    si,
                    side,
                    pos.size.abs(),
                    adverse,
                    event.timestamp,
                    false,
                    false,
                ) {
                    self.fire_on_fill(fill);
                }
            }
            return;
        }
        let mut maint_adv = 0.0;
        let mut candidates: Vec<vike_model::LiquidationCandidate> = Vec::new();
        for index in 0..self.core.symbols.len() {
            let p = self.core.symbol[index].pos;
            if p.size == 0.0 {
                continue;
            }
            let mark = if index == si { adverse } else { self.core.symbol[index].price };
            let mult = self.core.multiplier_of_index(index);
            maint_adv += p.size.abs() * mark * mult * self.core.maint_margin;
            candidates.push(vike_model::LiquidationCandidate {
                id: index,
                size: p.size,
                unit: mark * mult,
                unrealized_pnl: p.size * (mark - p.average_price) * mult,
            });
        }
        if !vike_model::pool_breached(eq_adv, maint_adv, self.core.liquidation_buffer) {
            return;
        }
        let excess = maint_adv - eq_adv;
        for (index, qty) in
            vike_model::cross_liquidation_plan(candidates, excess, self.core.maint_margin)
        {
            let p = self.core.symbol[index].pos;
            let mark = if index == si { adverse } else { self.core.symbol[index].price };
            let side = vike_model::closing_side(p.size);
            if let Some(fill) =
                self.core.apply_fill(index, side, qty, mark, event.timestamp, false, false)
            {
                self.fire_on_fill(fill);
            }
        }
    }
}

// ---- the scenarios -------------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Row {
    size: f64,
    avg: f64,
    last: f64,
    low: f64,
    high: f64,
    mult: f64,
}

#[derive(Clone, Debug)]
struct Scenario {
    rows: Vec<Row>,
    names: Vec<&'static str>,
    /// Cash is this times the pool's signed notional at the last prices, so a position carried on
    /// debt (a factor near -1) sits near the maintenance line and the law really is asked.
    cash_factor: f64,
    maint: f64,
    buffer: f64,
    fee: f64,
    venue_style: bool,
    simultaneous: bool,
    trigger: usize,
}

impl Scenario {
    fn bar_of(&self, i: usize) -> Bar {
        let r = &self.rows[i];
        Bar {
            timestamp: TIMESTAMP,
            open: r.last,
            high: r.high,
            low: r.low,
            close: r.last,
            volume: 1e12,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    fn engine(&self) -> StrategyEngine<Recorder> {
        let bars: Vec<(String, Vec<Bar>)> = (0..self.rows.len())
            .map(|i| (self.names[i].to_string(), vec![self.bar_of(i)]))
            .collect();
        let params = StrategyEngineConfig {
            fee_rate: self.fee,
            maint_margin: self.maint,
            liquidation_buffer: self.buffer,
            venue_style_liquidation: self.venue_style,
            decide: if self.simultaneous {
                DecideMode::Simultaneous
            } else {
                DecideMode::Sequential
            },
            multipliers: self
                .rows
                .iter()
                .zip(&self.names)
                .map(|(r, n)| (n.to_string(), r.mult))
                .collect(),
            ..StrategyEngineConfig::default()
        };
        let mut engine_instance = StrategyEngine::new(bars, Recorder::default(), params);
        let mut held = 0.0;
        for (i, r) in self.rows.iter().enumerate() {
            engine_instance.core.symbol[i].pos = Position { size: r.size, average_price: r.avg };
            engine_instance.core.symbol[i].price = r.last;
            held += vike_model::signed_notional(r.size, r.last, r.mult);
        }
        engine_instance.core.cash = self.cash_factor * held;
        engine_instance
    }
}

/// `1.0..10.0` times a power of ten, so the folds meet the magnitudes whose sums round.
fn magnitude() -> impl Strategy<Value = f64> {
    (1.0..10.0f64, prop::sample::select(vec![1e-3, 1e-2, 0.1, 1.0, 10.0, 1e2, 1e3, 1e4, 1e5, 1e6]))
        .prop_map(|(m, scale)| m * scale)
}

fn price() -> impl Strategy<Value = f64> {
    prop_oneof![
        40 => magnitude(),
        1 => Just(f64::NAN),
        1 => magnitude().prop_map(|p| -p),
    ]
}

fn row() -> impl Strategy<Value = Row> {
    let size = prop_oneof![
        1 => Just(0.0),
        3 => magnitude(),
        3 => magnitude().prop_map(|s| -s),
    ];
    let mult = prop::sample::select(vec![1.0, 1.0, 1.0, 0.5, 10.0, 50.0, 1e-4]);
    (size, price(), price(), price(), price(), mult)
        .prop_map(|(size, avg, last, low, high, mult)| Row { size, avg, last, low, high, mult })
}

fn scenario() -> impl Strategy<Value = Scenario> {
    (
        prop::collection::vec(row(), 1..=6),
        Just(vec!["D", "A", "C", "F", "B", "E"]).prop_shuffle(),
        -1.3..0.3f64,
        prop::sample::select(vec![0.0, 0.001, 0.005, 0.05, 0.1, 0.5]),
        prop::sample::select(vec![0.0, 0.1, 0.25]),
        prop::sample::select(vec![0.0, 0.0005]),
        any::<bool>(),
        any::<bool>(),
        any::<prop::sample::Index>(),
    )
        .prop_map(
            |(rows, names, cash_factor, maint, buffer, fee, venue_style, simultaneous, trigger)| {
                let trigger = trigger.index(rows.len());
                Scenario {
                    rows,
                    names,
                    cash_factor,
                    maint,
                    buffer,
                    fee,
                    venue_style,
                    simultaneous,
                    trigger,
                }
            },
        )
}

/// Everything a margin call can move, as bits, in the order it happened.
fn digest(engine: &StrategyEngine<Recorder>) -> Vec<u64> {
    let core = &engine.core;
    let mut out = vec![bits(core.cash), bits(core.fees_paid), core.dropped.len() as u64];
    for st in &core.symbol {
        out.extend([
            bits(st.pos.size),
            bits(st.pos.average_price),
            bits(st.realized),
            bits(st.entry_fee),
            st.entry_timestamp as u64,
            bits(st.hi_since),
            bits(st.lo_since),
        ]);
    }
    for t in &core.trades {
        let si = core.symbols.iter().position(|s| *s == t.symbol).expect("a run's symbol");
        out.extend([
            si as u64,
            bits(t.entry_price),
            bits(t.exit_price),
            bits(t.size),
            bits(t.pnl),
            bits(t.fees),
            bits(t.mae),
            bits(t.mfe),
            u64::from(t.is_long),
        ]);
    }
    for f in &engine.strategy.fills {
        let si = core.symbols.iter().position(|s| *s == f.symbol).expect("a run's symbol");
        out.extend([
            si as u64,
            f.side as u64,
            bits(f.size),
            bits(f.price),
            bits(f.fee),
            f.timestamp as u64,
            u64::from(f.is_maker),
        ]);
    }
    out
}

/// Whether the new code's run filled anything, on either path: the control below counts these.
fn fills_of(s: &Scenario) -> (usize, usize) {
    let mut bar = s.engine();
    bar.check_liquidation(0);
    let mut tick = s.engine();
    tick.check_liquidation_tick(s.trigger, &s.bar_of(s.trigger));
    (bar.strategy.fills.len(), tick.strategy.fills.len())
}

proptest! {
    #[test]
    fn the_bar_folds_are_the_old_expressions_bit_for_bit(s in scenario()) {
        let engine = s.engine();
        let core = &engine.core;
        let held: Vec<usize> = core
            .cross_section()
            .iter()
            .copied()
            .filter(|&si| core.symbol[si].pos.size != 0.0)
            .collect();
        let (old_eq, old_notional, old_maint, old_candidates) = old_bar_folds(core, 0);
        let (eq, notional) = core.bar_exposure(&held, 0);
        let (maint, candidates) = core.bar_maintenance(&held, 0);
        prop_assert_eq!(bits(eq), bits(old_eq));
        prop_assert_eq!(bits(notional), bits(old_notional));
        prop_assert_eq!(bits(maint), bits(old_maint));
        prop_assert_eq!(
            candidates.iter().map(candidate_bits).collect::<Vec<_>>(),
            old_candidates.iter().map(candidate_bits).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_tick_folds_are_the_old_expressions_bit_for_bit(s in scenario()) {
        let engine = s.engine();
        let core = &engine.core;
        let si = s.trigger;
        let event = s.bar_of(si);
        let adverse = if core.symbol[si].pos.size > 0.0 { event.low } else { event.high };
        let (old_eq, old_notional, old_maint, old_candidates) = old_tick_folds(core, si, &event);
        let (eq, notional) = core.tick_exposure(si, adverse);
        let (maint, candidates) = core.tick_maintenance(si, adverse);
        prop_assert_eq!(bits(eq), bits(old_eq));
        prop_assert_eq!(bits(notional), bits(old_notional));
        prop_assert_eq!(bits(maint), bits(old_maint));
        prop_assert_eq!(
            candidates.iter().map(candidate_bits).collect::<Vec<_>>(),
            old_candidates.iter().map(candidate_bits).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_whole_bar_margin_call_is_the_old_code_bit_for_bit(s in scenario()) {
        let mut old = s.engine();
        old.old_check_liquidation(0);
        let mut new = s.engine();
        new.check_liquidation(0);
        prop_assert_eq!(digest(&new), digest(&old));
    }

    #[test]
    fn a_whole_tick_margin_call_is_the_old_code_bit_for_bit(s in scenario()) {
        let event = s.bar_of(s.trigger);
        let mut old = s.engine();
        old.old_check_liquidation_tick(s.trigger, &event);
        let mut new = s.engine();
        new.check_liquidation_tick(s.trigger, &event);
        prop_assert_eq!(digest(&new), digest(&old));
    }

    /// The same position, the simulator's fold against what live reads off a LINEAR
    /// [`ContractValue`]. Every method here multiplies in an order that gives these bits:
    /// `maintenance_margin(p, s, 1.0, mm)` is `|s|·p·u·1.0·mm` and `x·1.0` is `x`;
    /// `unrealized_pnl(e, p, s)` is `(p−e)·s·u` against the fold's `s·(p−e)·u`, and a product of two
    /// is commutative.
    #[test]
    fn the_simulator_and_a_live_linear_contract_value_agree_bit_for_bit(
        r in row(),
        maint in 0.001..0.5f64,
    ) {
        let s = Scenario {
            rows: vec![r.clone()],
            names: vec!["ONE"],
            cash_factor: 0.0,
            maint,
            buffer: 0.0,
            fee: 0.0,
            venue_style: false,
            simultaneous: false,
            trigger: 0,
        };
        prop_assume!(r.size != 0.0);
        let engine = s.engine();
        let live = ContractValue::linear(r.mult);
        let mark = if r.size > 0.0 { r.low } else { r.high };

        let (eq, notional) = engine.core.bar_exposure(&[0], 0);
        prop_assert_eq!(bits(eq), bits(engine.core.cash + live.signed_notional(r.size, mark)));
        prop_assert_eq!(bits(notional), bits(0.0 + live.gross_notional(r.size, mark)));

        let (maintenance, candidates) = engine.core.bar_maintenance(&[0], 0);
        prop_assert_eq!(
            bits(maintenance),
            bits(0.0 + live.maintenance_margin(mark, r.size, 1.0, maint))
        );
        prop_assert_eq!(bits(candidates[0].unrealized_pnl), bits(live.unrealized_pnl(r.avg, mark, r.size)));
    }
}

/// A sample of the generator breaches on both paths and under both knob arms, so the two whole-run
/// properties above compare real liquidations and not two engines that did nothing.
#[test]
fn the_generator_really_liquidates_on_both_paths() {
    let mut runner = TestRunner::deterministic();
    let strategy = scenario();
    let (mut bar_lean, mut bar_wipe, mut tick_lean, mut tick_wipe) = (0, 0, 0, 0);
    for _ in 0..512 {
        let s = strategy.new_tree(&mut runner).expect("a scenario").current();
        let (bar, tick) = fills_of(&s);
        if s.venue_style {
            bar_wipe += usize::from(bar > 0);
            tick_wipe += usize::from(tick > 0);
        } else {
            bar_lean += usize::from(bar > 0);
            tick_lean += usize::from(tick > 0);
        }
    }
    for (name, n) in [
        ("bar, law", bar_lean),
        ("bar, knob", bar_wipe),
        ("tick, law", tick_lean),
        ("tick, knob", tick_wipe),
    ] {
        assert!(n >= 16, "{name}: only {n} of 512 scenarios filled anything");
    }
}

/// Three rows of 1, 1 and 1e16 at a mark of 1 and a rate of 1, the trigger the third. `1e16 + 1` ties
/// and rounds back to `1e16`, so the sum says which walk ran: `si` first keeps it at `1e16`, and the
/// other order adds `1 + 1` first and lands on `1e16 + 2`.
fn order_scenario(names: Vec<&'static str>, sizes: [f64; 3], simultaneous: bool) -> Scenario {
    let rows = sizes
        .iter()
        .map(|&size| Row { size, avg: 1.0, last: 1.0, low: 1.0, high: 1.0, mult: 1.0 })
        .collect();
    Scenario {
        rows,
        names,
        cash_factor: 0.0,
        maint: 1.0,
        buffer: 0.0,
        fee: 0.0,
        venue_style: false,
        simultaneous,
        trigger: 2,
    }
}

/// ⚠ **The tick path's two folds walk in two different orders, and this is the pin.** Equity and
/// notional take the trigger FIRST; maintenance and the candidates keep it in its `symbols` place. A
/// helper that picks one order for both turns one of these red.
#[test]
fn the_tick_path_folds_in_two_orders() {
    let s = order_scenario(vec!["A", "B", "C"], [1.0, 1.0, 1e16], false);
    let engine = s.engine();
    let (eq, notional) = engine.core.tick_exposure(2, 1.0);
    assert_eq!(eq.to_bits(), 1e16f64.to_bits(), "the trigger is summed first");
    assert_eq!(notional.to_bits(), 1e16f64.to_bits(), "the trigger is summed first");
    let (maintenance, candidates) = engine.core.tick_maintenance(2, 1.0);
    assert_eq!(
        maintenance.to_bits(),
        (1e16f64 + 2.0).to_bits(),
        "maintenance walks `symbols` with the trigger in place"
    );
    assert_eq!(candidates.iter().map(|c| c.id).collect::<Vec<_>>(), [0, 1, 2]);
}

/// The bar path's order is `cross_section()`, whichever mode decides it: under `Simultaneous` the
/// sorted names (A, B, C are rows 1, 2, 0) and the two ones meet first.
#[test]
fn the_bar_path_folds_in_cross_section_order() {
    let names = vec!["C", "A", "B"];
    let sizes = [1e16, 1.0, 1.0];
    for (simultaneous, want) in [(false, 1e16f64), (true, 1e16f64 + 2.0)] {
        let s = order_scenario(names.clone(), sizes, simultaneous);
        let engine = s.engine();
        let held = engine.core.cross_section().to_vec();
        let (eq, notional) = engine.core.bar_exposure(&held, 0);
        assert_eq!(eq.to_bits(), want.to_bits(), "simultaneous = {simultaneous}");
        assert_eq!(notional.to_bits(), want.to_bits(), "simultaneous = {simultaneous}");
        let (maintenance, _) = engine.core.bar_maintenance(&held, 0);
        assert_eq!(maintenance.to_bits(), want.to_bits(), "simultaneous = {simultaneous}");
    }
}

/// The knob's real difference, side by side: on a breach the bar path closes every held position, the
/// tick path only the trigger.
#[test]
fn the_stress_knob_closes_all_on_the_bar_path_and_only_the_trigger_on_the_tick_path() {
    let row = |size| Row { size, avg: 100.0, last: 100.0, low: 90.0, high: 110.0, mult: 1.0 };
    let s = Scenario {
        rows: vec![row(1.0), row(-2.0), row(3.0)],
        names: vec!["A", "B", "C"],
        cash_factor: 0.0,
        maint: 0.5,
        buffer: 0.0,
        fee: 0.0,
        venue_style: true,
        simultaneous: false,
        trigger: 1,
    };
    let mut bar = s.engine();
    bar.check_liquidation(0);
    assert_eq!(bar.strategy.fills.len(), 3, "the bar path wipes the book");
    let mut tick = s.engine();
    tick.check_liquidation_tick(1, &s.bar_of(1));
    assert_eq!(tick.strategy.fills.len(), 1, "the tick path closes the trigger alone");
    assert_eq!(tick.strategy.fills[0].symbol, "B");
    assert_eq!(tick.core.symbol[0].pos.size, 1.0, "another position stands");
}
