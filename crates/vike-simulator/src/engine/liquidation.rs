//! Membership exits and margin-call liquidation, on the bar path and the tick path.
//!
//! # The two margin calls: one fold, one plan execution, two walks
//!
//! [`fold_exposure`], [`fold_maintenance`] and [`StrategyEngine::run_cross_plan`] are the arithmetic
//! and the plan's execution both paths share. What stays each path's own is what it hands them, and
//! that difference is the behaviour:
//!
//! | | bar ([`StrategyEngine::check_liquidation`]) | tick ([`StrategyEngine::check_liquidation_tick`]) |
//! |---|---|---|
//! | mark | every held position at its adverse extreme | the triggering symbol at adverse, the others at their last price |
//! | equity and notional | `cross_section()` order | the triggering symbol FIRST, the others in `symbols` order |
//! | maintenance and candidates | `cross_section()` order | `symbols` order, the triggering symbol IN PLACE |
//! | `venue_style_liquidation` | closes every held position | closes the triggering symbol only |
//!
//! ⚠ **The order is an INPUT of the folds, not a detail of them.** They are naive `+=` on purpose (the
//! parity rule), so a different order differs in the last bits, and those bits decide an inequality.
//! The tick path's two walks really are two different orders, so neither fold may pick one.
//! `crates/vike-simulator/src/engine/tests/liquidation_fold.rs` holds both paths to verbatim copies of the
//! code they replaced, by `to_bits`.
//!
//! The money is the LEDGER's: every term goes through the symbol's `vike_model::ContractValue`, whose
//! linear arms are the products these folds always spelled (bit for bit, held by
//! `crates/vike-simulator/src/engine/tests/liquidation_fold.rs`) and whose inverse arms are the settle coin
//! (decision 0258). On a coin ledger the pool's equity, notional and maintenance are coin amounts at
//! the judged mark, so an isolated-sized long breaches where the venues' liquidation price
//! `e·(1+mm)/(1+initial_margin)` says (binance COIN-M's formula at `cum = 0`).
//!
//! # The opt-in third: [`ShortfallCover`]
//!
//! `StrategyEngineConfig::shortfall_cover` replaces both on the bar path: the account against the rule's own long
//! and short margin rates, cut by a multiple of the shortfall, the type's doc carrying the rule. A step
//! with no walk is judged once, at the bar's open and then its adverse extremes
//! ([`StrategyEngine::cover_shortfalls`]); a step with one is judged after each sub-bar's fills, at that
//! sub-bar's close ([`StrategyEngine::cover_at_node`], called from the granular fill loop, and
//! `OrderBrokerSimulator::walked_step` is the one question both ask). Its fill gate is
//! [`OrderBrokerSimulator::shortfall_refuses`], which `OrderBrokerSimulator::fill_terms` asks, and whose stacking exemption
//! reads the step's start (`OrderBrokerSimulator::note_bar_start`). It reads neither fold above (its margin is
//! weighted by each position's side), so with it unset not one of their bits moves; its money is the
//! ledger's too, through the same `ContractValue`, though only linear rows have measured it. It has no
//! tick walk: `run_ticks` refuses it, for the reason the type's doc gives.

use vike_marketdata::Bar;
use vike_model::{ContractValue, LiquidationCandidate, Position, Strategy};

use super::{OrderBrokerSimulator, ShortfallCover, StrategyEngine, settle_unrealized_pnl};

/// One held position as a fold reads it, at the mark the calling path judges it at.
#[derive(Clone, Copy)]
struct Judged {
    si: usize,
    pos: Position,
    mark: f64,
    value: ContractValue,
}

/// A held position's gross notional at `mark` in the ledger's money: `gross_notional`'s product on a
/// linear symbol, its coin worth on an inverse one.
fn settle_gross(value: ContractValue, size: f64, mark: f64) -> f64 {
    value.in_settle(value.gross_notional(size, mark), mark)
}

/// The pool's equity and gross notional: `seed` plus each position of `walk` in the order given, by
/// naive `+=` (the module doc's ⚠). The seed is an input because the tick path's first term IS its
/// seed: it never adds to a `0.0`, and `0.0 + -0.0` is not `-0.0`.
fn fold_exposure(seed: (f64, f64), walk: impl Iterator<Item = Judged>) -> (f64, f64) {
    let (mut equity, mut notional) = seed;
    for j in walk {
        equity += j.value.cash_leg(j.pos.size, j.mark);
        notional += settle_gross(j.value, j.pos.size, j.mark);
    }
    (equity, notional)
}

/// The pool's maintenance requirement, folded per row (`|size|·mark·mult·rate`, the same fold shape as
/// `Account::margin_in_use_priced`), and the plan's candidates in the order given: the plan's
/// losers-first sort breaks a tie on that order.
///
/// `cross_liquidation_plan` prices one unit's maintenance as `unit · rate`, the unit one unit's gross
/// notional in the ledger's money at the judged mark ([`settle_gross`] of one unit): `mark · u` on a
/// linear symbol, `u/mark` coins on an inverse one.
fn fold_maintenance(
    walk: impl Iterator<Item = Judged>,
    maint_margin: f64,
) -> (f64, Vec<LiquidationCandidate>) {
    let mut maintenance = 0.0;
    let mut candidates = Vec::new();
    for j in walk {
        let v = j.value;
        maintenance +=
            v.in_settle(v.maintenance_margin(j.mark, j.pos.size, 1.0, maint_margin), j.mark);
        candidates.push(LiquidationCandidate {
            id: j.si,
            size: j.pos.size,
            unit: settle_gross(v, 1.0, j.mark),
            unrealized_pnl: settle_unrealized_pnl(v, &j.pos, j.mark),
        });
    }
    (maintenance, candidates)
}

impl OrderBrokerSimulator {
    /// `si`'s adverse extreme of bar `current_index` (low for a long, high for a short) and that bar's time.
    pub(super) fn bar_adverse(&self, si: usize, current_index: usize) -> (f64, i64) {
        let bar = &self.bars[si][current_index];
        (if self.symbol[si].pos.size > 0.0 { bar.low } else { bar.high }, bar.timestamp)
    }

    /// The bar path's walk: `held` in the order given, each at its adverse extreme.
    fn bar_walk<'a>(
        &'a self,
        held: &'a [usize],
        current_index: usize,
    ) -> impl Iterator<Item = Judged> + 'a {
        held.iter().map(move |&si| Judged {
            si,
            pos: self.symbol[si].pos,
            mark: self.bar_adverse(si, current_index).0,
            value: self.value_of_index(si),
        })
    }

    /// The bar path's equity and gross notional, over `held` in `cross_section()` order.
    pub(super) fn bar_exposure(&self, held: &[usize], current_index: usize) -> (f64, f64) {
        fold_exposure((self.cash, 0.0), self.bar_walk(held, current_index))
    }

    /// The account as [`ShortfallCover`] judges it, over `held` in `cross_section()` order, each
    /// position at `mark_of(si)`: its equity, and the margin `rule` requires of it.
    fn shortfall_account(
        &self,
        held: &[usize],
        rule: &ShortfallCover,
        mark_of: impl Fn(usize) -> f64,
    ) -> (f64, f64) {
        let (mut equity, mut margin) = (self.cash, 0.0);
        for &si in held {
            let (size, mark, value) =
                (self.symbol[si].pos.size, mark_of(si), self.value_of_index(si));
            equity += value.cash_leg(size, mark);
            margin += settle_gross(value, size, mark) * rule.rate_for(size);
        }
        (equity, margin)
    }

    /// Whether [`ShortfallCover`]'s fill gate refuses a fill of `size` at `price` on `side` (`false`
    /// when it is unset): a fill that leaves a position on its own side whose margin at that price
    /// exceeds the account's equity now, unless an earlier fill of this step on this symbol and side
    /// reduced the position (the entry of a close-then-enter reversal, which its reference fills and
    /// margin-calls), or an earlier fill of this step changed the position and this fill fits against
    /// the position and the equity the step began with (an entry stacked on one that filled before it
    /// in the step, which its reference fills and margin-calls).
    pub(super) fn shortfall_refuses(&self, si: usize, side: i32, size: f64, price: f64) -> bool {
        let Some(rule) = &self.shortfall_cover else { return false };
        let after = self.symbol[si].pos.size + f64::from(side) * size;
        let rate = rule.rate_for(after);
        let opens = after != 0.0 && (after > 0.0) == (side > 0);
        if !opens || rate <= 0.0 || self.shortfall_reduced[si] == Some((self.step, side)) {
            return false;
        }
        let value = self.value_of_index(si);
        if settle_gross(value, after, price) * rate <= self.equity_now() {
            return false;
        }
        // The same fill against the step's start: where it fits, only the entries before it overshoot.
        let start = self.shortfall_bar_size[si];
        if self.symbol[si].pos.size == start {
            return true;
        }
        let from_start = start + f64::from(side) * size;
        let needed = settle_gross(value, from_start, price) * rule.rate_for(from_start);
        // A relative 1e-11 is float noise only.
        from_start != 0.0
            && needed - self.shortfall_bar_equity > self.shortfall_bar_equity.abs() * 1e-11
    }

    /// The step's start, as the fill gate's stacking exemption reads it (a no-op unless the rule is set).
    pub(super) fn note_bar_start(&mut self) {
        if self.shortfall_cover.is_none() {
            return;
        }
        for si in 0..self.symbol.len() {
            self.shortfall_bar_size[si] = self.symbol[si].pos.size;
        }
        self.shortfall_bar_equity = self.equity_now();
    }

    /// Whether the step's bars are walked in sub-bars on this run's lane: each symbol has a walk, and the granular
    /// lane is the one that fills (the cash-gated lane fills a step whole).
    fn walked_step(&self, current_index: usize) -> bool {
        !(self.cash_gate || self.decide.is_simultaneous())
            && (0..self.symbols.len()).all(|si| !self.symbol[si].sub[current_index].is_empty())
    }

    /// The bar path's maintenance and candidates, over `held` in `cross_section()` order.
    pub(super) fn bar_maintenance(
        &self,
        held: &[usize],
        current_index: usize,
    ) -> (f64, Vec<LiquidationCandidate>) {
        fold_maintenance(self.bar_walk(held, current_index), self.maint_margin)
    }

    /// The tick path's equity and gross notional: `si` first, at `adverse`, then every other held
    /// symbol in `symbols` order at its last price.
    pub(super) fn tick_exposure(&self, si: usize, adverse: f64) -> (f64, f64) {
        let pos = self.symbol[si].pos;
        let value = self.value_of_index(si);
        let seed =
            (self.cash + value.cash_leg(pos.size, adverse), settle_gross(value, pos.size, adverse));
        let others = (0..self.symbols.len())
            .filter(|&other| other != si && self.symbol[other].pos.size != 0.0)
            .map(|other| Judged {
                si: other,
                pos: self.symbol[other].pos,
                mark: self.symbol[other].price,
                value: self.value_of_index(other),
            });
        fold_exposure(seed, others)
    }

    /// The tick path's maintenance and candidates: every held symbol in `symbols` order, `si` in its
    /// own place at `adverse`, the rest at their last price. NOT the order of [`Self::tick_exposure`].
    pub(super) fn tick_maintenance(
        &self,
        si: usize,
        adverse: f64,
    ) -> (f64, Vec<LiquidationCandidate>) {
        let walk = (0..self.symbols.len()).filter(|&index| self.symbol[index].pos.size != 0.0).map(
            |index| Judged {
                si: index,
                pos: self.symbol[index].pos,
                mark: if index == si { adverse } else { self.symbol[index].price },
                value: self.value_of_index(index),
            },
        );
        fold_maintenance(walk, self.maint_margin)
    }
}

impl<S: Strategy<OrderBrokerSimulator>> StrategyEngine<S> {
    /// WL removal-day exit: force-close positions now out of membership, at the bar's OPEN.
    pub(super) fn close_inactive(&mut self, current_index: usize) {
        if self.core.active_mask.is_none() {
            return;
        }
        for si in 0..self.core.symbols.len() {
            if self.core.symbol[si].pos.size != 0.0 && !self.core.is_active_index(si) {
                let pos_size = self.core.symbol[si].pos.size;
                let side = vike_model::closing_side(pos_size);
                let bar = self.core.bars[si][current_index].clone();
                if let Some(fill) = self.core.apply_fill(
                    si,
                    side,
                    pos_size.abs(),
                    bar.open,
                    bar.timestamp,
                    false,
                    false,
                ) {
                    self.fire_on_fill(fill);
                }
            }
        }
    }

    /// Account-level margin call at the bar's intrabar ADVERSE marks — the ONE
    /// scope-parameterized liquidation law (`vike_model::money::liquidation`), shared with the live
    /// watchdog (`vike_execution::check_margin_call_priced`). The backtest's positions have no
    /// per-position margin mode, so the whole account IS the cross pool.
    ///
    /// DEFAULT path (`venue_style_liquidation == false`): the LEAN cross-pool workflow —
    /// `pool_breached` (the `pool_equity ≤ maintenance` law PLUS the `liquidation_buffer` grace arm,
    /// default 0.10 = the live watchdog's buffer) then `cross_liquidation_plan`: biggest losers
    /// first, close only the EXCESS over the line, stop as soon as healthy. **This is a real
    /// behavior change for margined backtests** (they previously wiped the whole account at the
    /// bare trigger); the old model is preserved bit-for-bit behind the opt-in stress knob.
    ///
    /// STRESS KNOB (`venue_style_liquidation == true`): the pre-law behavior, verbatim —
    /// trigger `eq_adv ≤ maint_margin · notional_adv` and force-close ALL positions at their
    /// adverse marks.
    ///
    /// OPT-IN THIRD MODE (`shortfall_cover`): replaces both of the above, see
    /// [`Self::cover_shortfalls`].
    pub(super) fn check_liquidation(&mut self, current_index: usize) {
        if let Some(rule) = self.core.shortfall_cover {
            // A walked step was judged at each end of its walk (`Self::cover_at_node`), as it filled.
            if !self.core.walked_step(current_index) {
                self.cover_shortfalls(current_index, &rule);
            }
            return;
        }
        if self.core.maint_margin <= 0.0 {
            return;
        }
        // Held positions in CROSS-SECTION order (`symbols` order under the default
        // `DecideMode::Sequential`, so this is the frozen list index for index). It is routed
        // through the canonical order because THREE separate answers below are functions of this
        // sequence: `eq_adv`/`notional_adv`/`maint_adv` are naive `+=` folds (deliberately naive
        // — the parity rule — and therefore order-sensitive in the last bits, which decides an
        // inequality), and `candidates` is handed to `vike_model::cross_liquidation_plan`, whose
        // losers-first sort breaks a TIE on candidate order. Two positions with equal unrealized
        // loss would otherwise be liquidated in the order the operator typed them.
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
        let (eq_adv, notional_adv) = self.core.bar_exposure(&held, current_index);
        if self.core.venue_style_liquidation {
            // opt-in stress model: the retired default, byte-identical — total wipe at trigger
            if eq_adv <= self.core.maint_margin * notional_adv {
                for &si in &held {
                    let pos = self.core.symbol[si].pos;
                    let (adverse, ts) = self.core.bar_adverse(si, current_index);
                    let side = vike_model::closing_side(pos.size);
                    if let Some(fill) =
                        self.core.apply_fill(si, side, pos.size.abs(), adverse, ts, false, false)
                    {
                        self.fire_on_fill(fill);
                    }
                }
            }
            return;
        }
        let (maint_adv, candidates) = self.core.bar_maintenance(&held, current_index);
        self.run_cross_plan(eq_adv, maint_adv, candidates, |core, si| {
            core.bar_adverse(si, current_index)
        });
    }

    /// [`ShortfallCover`]'s margin call on bar `current_index`, for a step with no walk: at the bar's open, then at
    /// each position's adverse extreme, each held position in `cross_section()` order against the account with
    /// every held position at that checkpoint, re-judged after each cut.
    fn cover_shortfalls(&mut self, current_index: usize, rule: &ShortfallCover) {
        let held = self.held_in_order();
        for at_open in [true, false] {
            self.cover_checkpoint(
                &held,
                rule,
                |core, si| {
                    if at_open {
                        core.bars[si][current_index].open
                    } else {
                        core.bar_adverse(si, current_index).0
                    }
                },
                |core, si| core.bars[si][current_index].timestamp,
            );
        }
    }

    /// [`ShortfallCover`]'s margin call at one node of a bar's walk: the sub-bar that just filled ended at `price`.
    /// Each held position is judged with `si` there and the others at their last price, so a position is judged
    /// where the walk had it: not at an extreme it met before it was opened, and at the extremes it met before a
    /// later fill of the same bar closed it.
    pub(super) fn cover_at_node(&mut self, si: usize, price: f64, ts: i64) {
        let Some(rule) = self.core.shortfall_cover else { return };
        let held = self.held_in_order();
        self.cover_checkpoint(
            &held,
            &rule,
            |core, s| if s == si { price } else { core.symbol[s].price },
            |_, _| ts,
        );
    }

    /// The held positions in `cross_section()` order.
    fn held_in_order(&self) -> Vec<usize> {
        self.core
            .cross_section()
            .iter()
            .copied()
            .filter(|&si| self.core.symbol[si].pos.size != 0.0)
            .collect()
    }

    /// One checkpoint of [`ShortfallCover`]: each position of `held`, in order, judged against the account with
    /// every held position at `mark`, cut when it is short of margin, and the account judged again after the cut.
    fn cover_checkpoint(
        &mut self,
        held: &[usize],
        rule: &ShortfallCover,
        mark: impl Fn(&OrderBrokerSimulator, usize) -> f64,
        ts_of: impl Fn(&OrderBrokerSimulator, usize) -> i64,
    ) {
        for &si in held {
            let size = self.core.symbol[si].pos.size;
            let rate = rule.rate_for(size);
            if size == 0.0 || rate <= 0.0 {
                continue;
            }
            let (equity, margin) = self.core.shortfall_account(held, rule, |s| mark(&self.core, s));
            let available = equity - margin;
            if available >= 0.0 {
                continue;
            }
            let (price, ts) = (mark(&self.core, si), ts_of(&self.core, si));
            let unit = settle_gross(self.core.value_of_index(si), 1.0, price);
            let just_enough = -available / rate / unit;
            let qty = if rule.step > 0.0 {
                let steps = (just_enough / rule.step).floor();
                // Below one step its reference closes one whole unit.
                if steps == 0.0 { rule.step.max(1.0) } else { rule.cover * steps * rule.step }
            } else {
                rule.cover * just_enough
            };
            let side = vike_model::closing_side(size);
            if let Some(fill) =
                self.core.apply_fill(si, side, qty.min(size.abs()), price, ts, false, false)
            {
                self.core.shortfall_cover_fills += 1;
                self.fire_on_fill(fill);
            }
        }
    }

    /// Tick-path margin call (`_check_liquidation_tick` twin): the triggering symbol is judged
    /// at the tick's ADVERSE print, every other held symbol at its last seen price. Same
    /// law/knob split as [`Self::check_liquidation`]:
    ///
    /// - DEFAULT: the shared cross-pool law — `pool_breached` (+ `liquidation_buffer`) then the
    ///   losers-first `cross_liquidation_plan` over ALL held positions (the pool is shared, so
    ///   the plan may partially close positions other than the triggering symbol, each at the
    ///   mark it was judged at). Previously a breach here closed ONLY the triggering symbol,
    ///   in full — a per-symbol wipe the one law replaces.
    /// - STRESS KNOB (`venue_style_liquidation`): the pre-law behavior verbatim — bare
    ///   trigger, close only the triggering symbol, full size.
    pub(super) fn check_liquidation_tick(&mut self, si: usize, event: &Bar) {
        if self.core.maint_margin <= 0.0 {
            return;
        }
        let pos = self.core.symbol[si].pos;
        if pos.size == 0.0 {
            return;
        }
        let adverse = if pos.size > 0.0 { event.low } else { event.high };
        let (eq_adv, notional_adv) = self.core.tick_exposure(si, adverse);
        if self.core.venue_style_liquidation {
            // opt-in stress model: the retired default, byte-identical — close ONLY `si`, whole
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
        // shared law over the whole pool at the judged marks (si: adverse; others: last price)
        let (maint_adv, candidates) = self.core.tick_maintenance(si, adverse);
        self.run_cross_plan(eq_adv, maint_adv, candidates, |core, index| {
            (if index == si { adverse } else { core.symbol[index].price }, event.timestamp)
        });
    }

    /// The law's verdict and the plan's execution: `pool_breached`, then each close of
    /// `cross_liquidation_plan` at the mark and time `mark_of` answers. `mark_of` and the side are
    /// read per close, after the previous close's `on_fill` has run, as each path always read them.
    fn run_cross_plan(
        &mut self,
        eq_adv: f64,
        maint_adv: f64,
        candidates: Vec<LiquidationCandidate>,
        mark_of: impl Fn(&OrderBrokerSimulator, usize) -> (f64, i64),
    ) {
        if !vike_model::pool_breached(eq_adv, maint_adv, self.core.liquidation_buffer) {
            return;
        }
        let excess = maint_adv - eq_adv;
        for (si, qty) in
            vike_model::cross_liquidation_plan(candidates, excess, self.core.maint_margin)
        {
            let (mark, ts) = mark_of(&self.core, si);
            let side = vike_model::closing_side(self.core.symbol[si].pos.size);
            if let Some(fill) = self.core.apply_fill(si, side, qty, mark, ts, false, false) {
                self.fire_on_fill(fill);
            }
        }
    }
}
