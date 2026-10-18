//! Membership exits and margin-call liquidation, on the bar path and the tick path.

use vike_marketdata::Bar;
use vike_model::Strategy;

use super::{SimBroker, StrategyEngine};

impl<S: Strategy<SimBroker>> StrategyEngine<S> {
    /// WL removal-day exit: force-close positions now out of membership, at the bar's OPEN.
    pub(super) fn close_inactive(&mut self, cur_idx: usize) {
        if self.core.active_mask.is_none() {
            return;
        }
        for si in 0..self.core.symbols.len() {
            if self.core.sym[si].pos.size != 0.0 && !self.core.is_active_idx(si) {
                let pos_size = self.core.sym[si].pos.size;
                let side = vike_model::closing_side(pos_size);
                let bar = self.core.bars[si][cur_idx].clone();
                if let Some(fill) =
                    self.core.apply_fill(si, side, pos_size.abs(), bar.open, bar.ts, false)
                {
                    self.fire_on_fill(fill);
                }
            }
        }
    }

    /// Account-level margin call at the bar's intrabar ADVERSE marks — the ONE
    /// scope-parameterized liquidation law (`vike_model::money::liquidation`), shared with the live
    /// watchdog (`vike_exec::check_margin_call_priced`). The backtest's positions have no
    /// per-position margin mode, so the whole account IS the cross pool.
    ///
    /// DEFAULT path (`venue_style_liquidation == false`): the LEAN cross-pool workflow —
    /// `pool_breached` (the `pool_equity ≤ maintenance` law PLUS the `liq_buffer` grace arm,
    /// default 0.10 = the live watchdog's buffer) then `cross_liquidation_plan`: biggest losers
    /// first, close only the EXCESS over the line, stop as soon as healthy. **This is a real
    /// behavior change for margined backtests** (they previously wiped the whole account at the
    /// bare trigger); the old model is preserved bit-for-bit behind the opt-in stress knob.
    ///
    /// STRESS KNOB (`venue_style_liquidation == true`): the pre-law behavior, verbatim —
    /// trigger `eq_adv ≤ maint_margin · notional_adv` and force-close ALL positions at their
    /// adverse marks.
    pub(super) fn check_liquidation(&mut self, cur_idx: usize) {
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
            .filter(|&si| self.core.sym[si].pos.size != 0.0)
            .collect();
        if held.is_empty() {
            return;
        }
        let mut eq_adv = self.core.cash;
        let mut notional_adv = 0.0;
        for &si in &held {
            let pos = self.core.sym[si].pos;
            let bar = &self.core.bars[si][cur_idx];
            let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
            eq_adv +=
                vike_model::signed_notional(pos.size, adverse, self.core.multiplier_of_idx(si));
            notional_adv +=
                vike_model::gross_notional(pos.size, adverse, self.core.multiplier_of_idx(si));
        }
        if self.core.venue_style_liquidation {
            // opt-in stress model: the retired default, byte-identical — total wipe at trigger
            if eq_adv <= self.core.maint_margin * notional_adv {
                for &si in &held {
                    let pos = self.core.sym[si].pos;
                    let bar = self.core.bars[si][cur_idx].clone();
                    let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
                    let side = vike_model::closing_side(pos.size);
                    if let Some(fill) =
                        self.core.apply_fill(si, side, pos.size.abs(), adverse, bar.ts, false)
                    {
                        self.fire_on_fill(fill);
                    }
                }
            }
            return;
        }
        // shared law: maintenance folded per row (|size|·adverse·mult·rate — the same fold
        // shape as `Account::margin_in_use_priced`), then the LEAN losers-first excess-only plan
        let mut maint_adv = 0.0;
        let mut candidates: Vec<vike_model::LiqCandidate> = Vec::with_capacity(held.len());
        for &si in &held {
            let pos = self.core.sym[si].pos;
            let bar = &self.core.bars[si][cur_idx];
            let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
            let mult = self.core.multiplier_of_idx(si);
            maint_adv += pos.size.abs() * adverse * mult * self.core.maint_margin;
            candidates.push(vike_model::LiqCandidate {
                id: si,
                size: pos.size,
                mark: adverse,
                mult,
                upnl: pos.size * (adverse - pos.avg_price) * mult,
            });
        }
        if !vike_model::pool_breached(eq_adv, maint_adv, self.core.liq_buffer) {
            return;
        }
        let excess = maint_adv - eq_adv;
        for (si, qty) in
            vike_model::cross_liquidation_plan(candidates, excess, self.core.maint_margin)
        {
            let pos = self.core.sym[si].pos;
            let bar = self.core.bars[si][cur_idx].clone();
            let adverse = if pos.size > 0.0 { bar.low } else { bar.high };
            let side = vike_model::closing_side(pos.size);
            if let Some(fill) = self.core.apply_fill(si, side, qty, adverse, bar.ts, false) {
                self.fire_on_fill(fill);
            }
        }
    }

    /// Tick-path margin call (`_check_liquidation_tick` twin): the triggering symbol is judged
    /// at the tick's ADVERSE print, every other held symbol at its last seen price. Same
    /// law/knob split as [`Self::check_liquidation`]:
    ///
    /// - DEFAULT: the shared cross-pool law — `pool_breached` (+ `liq_buffer`) then the
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
        let pos = self.core.sym[si].pos;
        if pos.size == 0.0 {
            return;
        }
        let adverse = if pos.size > 0.0 { event.low } else { event.high };
        let mut eq_adv = self.core.cash
            + vike_model::signed_notional(pos.size, adverse, self.core.multiplier_of_idx(si));
        let mut notional_adv =
            vike_model::gross_notional(pos.size, adverse, self.core.multiplier_of_idx(si));
        for other in 0..self.core.symbols.len() {
            if other == si {
                continue;
            }
            let p = self.core.sym[other].pos;
            if p.size != 0.0 {
                eq_adv += vike_model::signed_notional(
                    p.size,
                    self.core.sym[other].price,
                    self.core.multiplier_of_idx(other),
                );
                notional_adv += vike_model::gross_notional(
                    p.size,
                    self.core.sym[other].price,
                    self.core.multiplier_of_idx(other),
                );
            }
        }
        if self.core.venue_style_liquidation {
            // opt-in stress model: the retired default, byte-identical — close ONLY `si`, whole
            if notional_adv > 0.0 && eq_adv <= self.core.maint_margin * notional_adv {
                let side = vike_model::closing_side(pos.size);
                if let Some(fill) =
                    self.core.apply_fill(si, side, pos.size.abs(), adverse, event.ts, false)
                {
                    self.fire_on_fill(fill);
                }
            }
            return;
        }
        // shared law over the whole pool at the judged marks (si: adverse; others: last price)
        let mut maint_adv = 0.0;
        let mut candidates: Vec<vike_model::LiqCandidate> = Vec::new();
        for idx in 0..self.core.symbols.len() {
            let p = self.core.sym[idx].pos;
            if p.size == 0.0 {
                continue;
            }
            let mark = if idx == si { adverse } else { self.core.sym[idx].price };
            let mult = self.core.multiplier_of_idx(idx);
            maint_adv += p.size.abs() * mark * mult * self.core.maint_margin;
            candidates.push(vike_model::LiqCandidate {
                id: idx,
                size: p.size,
                mark,
                mult,
                upnl: p.size * (mark - p.avg_price) * mult,
            });
        }
        if !vike_model::pool_breached(eq_adv, maint_adv, self.core.liq_buffer) {
            return;
        }
        let excess = maint_adv - eq_adv;
        for (idx, qty) in
            vike_model::cross_liquidation_plan(candidates, excess, self.core.maint_margin)
        {
            let p = self.core.sym[idx].pos;
            let mark = if idx == si { adverse } else { self.core.sym[idx].price };
            let side = vike_model::closing_side(p.size);
            if let Some(fill) = self.core.apply_fill(idx, side, qty, mark, event.ts, false) {
                self.fire_on_fill(fill);
            }
        }
    }
}
