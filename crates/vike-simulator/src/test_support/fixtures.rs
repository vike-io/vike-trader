//! The four SIMULATOR TEST FIXTURES (the owner's answer Q1 of 2026-11-10), MSE reference strategies
//! for the R4 cross-language gate (byte-equivalent Python twins in `scripts/export_r4_fixtures.py`,
//! removed by `751de662`'s Python purge). Test support, behind `test-support`: no profile can name
//! one, and no program links one.
//!
//! Each exists to drive simulator machinery live trading does not have, so each is
//! `impl Strategy<OrderBrokerSimulator>` on purpose and the order seam learns none of it. The machinery, and who
//! drives which:
//!
//! * the **[`vike_portfolio::sizing::PositionSizer`]** — [`OrderBrokerSimulator::submit`]'s `raw = false` arm, which
//!   routes the requested size through `size_entry`. The live path is always raw; there is no live
//!   sizer to be portable to. [`CapabilitiesSizersMask`] exists to exercise exactly this.
//! * the **cash-gate `weight`** — [`OrderBrokerSimulator::submit`]'s `weight` argument, the priority the
//!   shared-cash gate drops entries in when cash runs out. Live has no shared-cash gate.
//!   [`GatedWeights`] exists to exercise exactly this.
//! * the **attached protective `stop`** armed BY the entry (`submit(.., stop)`), which also feeds
//!   `PositionSizerContext::risk_stop`. Live arms a bracket as separate orders, not as a field on the entry.
//!   [`BracketPerSymbol`] exists to exercise exactly this.
//! * the **exchange truth under the response-latency shadow** — [`OrderBrokerSimulator::position_of`], where the
//!   strategy's read ([`vike_model::OrderBroker::position`]) answers the shadow while the opt-in latency
//!   gate is armed. [`TickPairMse`] reads it.
//!
//! ⚠ Every fixture in this file is pinned BIT-FOR-BIT by
//! `crates/vike-simulator/tests/parity/r4_parity.rs`'s `mse_runs_bit_parity`, whose `assert_dropped`
//! compares the cash-gate `weight` itself. Changing which verbs one of them calls is a parity
//! question, never a tidiness one — run that gate.

use vike_marketdata::{Bar, Symbol, TickQuote};

use vike_model::{OrderBroker, OrderPlacement, Strategy};

use crate::engine::OrderBrokerSimulator;

fn symbol_key(instrument: &Option<Symbol>) -> String {
    // Strategy._sym_key: strip ".VENUE" (exporter runs with default_venue=None → bare already)
    let s = instrument.as_deref().unwrap_or("_");
    match s.split_once('.') {
        Some((symbol, _)) => symbol.to_string(),
        None => s.to_string(),
    }
}

/// Per-symbol bracket cycle: every 7th bar when flat — market entry with a protective stop
/// (-3%) + a resting TP limit (+5%). One symbol runs granular sub-bars (exercises the
/// sub-bar-ordered SL/TP resolution + the OCO sibling cancel); the rest use the coarse path.
///
/// A SIMULATOR TEST FIXTURE, of the **attached protective `stop`** (see the module doc): its entry arms the
/// bracket as a FIELD on the order, which is the shape the sub-bar SL/TP resolution it exists to
/// exercise reads. Live arms a bracket as separate orders, so a portable rewrite would change what
/// is being tested, not just how it is spelled. It also calls `cancel_all`, whose nearest live
/// sibling (`LiveBroker::mass_cancel`) is account-wide rather than symbol-scoped.
#[derive(Default)]
pub struct BracketPerSymbol;

impl Strategy<OrderBrokerSimulator> for BracketPerSymbol {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        let symbol = symbol_key(&bar.symbol);
        if ctx.position_of(&symbol).size == 0.0 && ctx.index.is_multiple_of(7) {
            ctx.cancel_all(&symbol);
            ctx.submit(&symbol, 1, 1.5, 0.0, false, Some(bar.close * 0.97));
            ctx.submit_limit(&symbol, -1, 1.5, bar.close * 1.05, 0.0, false, None);
        }
    }
}

/// Shared-cash gate exercise (cash_gate=True): every 5th bar submit weighted entries across
/// the whole universe (cash runs out → weight-ordered drops); every 10th+5 close everything
/// (reductions-first path).
///
/// A SIMULATOR TEST FIXTURE, of the **cash-gate `weight`** (see the module doc). This strategy's
/// entire purpose is to drive a mechanism only the simulator has, and r4's `gated` run asserts the
/// resulting `dropped` rows INCLUDING their `weight` — so a portable rewrite would have to invent a
/// weight concept on the live path to keep meaning the same thing. It also reads the symbol
/// universe and calls `submit_close`, which deliberately bypasses both the sizer and the risk gate
/// that an untagged market [`OrderBroker::place`] runs through.
#[derive(Default)]
pub struct GatedWeights;

impl Strategy<OrderBrokerSimulator> for GatedWeights {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        let symbol = symbol_key(&bar.symbol);
        let si = ctx.symbols.iter().position(|s| *s == symbol).unwrap();
        if ctx.index.is_multiple_of(5) && ctx.position_of(&symbol).size == 0.0 {
            let sizes = [30.0, 25.0, 20.0, 15.0];
            let weights = [1.0, 2.0, 3.0, 0.5];
            ctx.submit(&symbol, 1, sizes[si % 4], weights[si % 4], false, None);
        } else if ctx.index % 10 == 5 && ctx.position_of(&symbol).size != 0.0 {
            ctx.submit_close(&symbol);
        }
    }
}

/// Caps + sizers + membership + mixed multipliers + account-level liquidation:
/// PctEquity(0.5) under a DrawdownThrottle, entries every 3rd bar when flat (S2 shorts),
/// protective stop at ±10% — over-notional books trigger the ALL-positions margin call
/// (the #303 mixed-multiplier gate).
///
/// A SIMULATOR TEST FIXTURE, of the **[`vike_portfolio::sizing::PositionSizer`]** (see the module doc), and
/// its `999.0` size is the proof: that is a SENTINEL, not a quantity — r4's `caps_sizers_mask` run
/// mounts a real `PositionSizerDrawdownThrottle { base: PositionSizerPercentEquity }`, whose `size` ignores
/// `PositionSizerContext::intent` entirely and returns `pct·equity/(basis_price·multiplier)`. Submitted
/// through the portable untagged market [`OrderBroker::place`] (`raw = true`) the sentinel would be
/// taken literally. Live has no sizer to be portable to.
#[derive(Default)]
pub struct CapabilitiesSizersMask;

impl Strategy<OrderBrokerSimulator> for CapabilitiesSizersMask {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, bar: &Bar) {
        let symbol = symbol_key(&bar.symbol);
        let si = ctx.symbols.iter().position(|s| *s == symbol).unwrap();
        if ctx.position_of(&symbol).size == 0.0 && ctx.index.is_multiple_of(2) {
            let side = if si == 2 { -1 } else { 1 };
            let stop = if side > 0 { bar.close * 0.96 } else { bar.close * 1.04 };
            ctx.submit(&symbol, side, 999.0, 0.0, false, Some(stop));
        }
    }
}

/// Two-symbol tick momentum (run_ticks + FillModelTick): per-symbol 3bp flip.
///
/// A SIMULATOR TEST FIXTURE, of the **exchange truth under the response-latency shadow** (see the
/// module doc): it reads `OrderBrokerSimulator::position_of`, never the strategy's
/// [`vike_model::OrderBroker::position`], which answers the shadow (`OrderBrokerSimulator::shadow_pos`) while the
/// opt-in latency gate is armed. Its orders are untagged market [`OrderBroker::place`]s,
/// `OrderBrokerSimulator::submit` with `raw = true`; with no
/// `StrategyEngineConfig::sizer` set the engine installs `PositionSizerPassThrough`, whose `size` returns
/// `PositionSizerContext::intent`, the number `raw = true` skips the call for (and a flip REDUCES first,
/// which `size_entry` passes through regardless of `raw`).
#[derive(Default)]
pub struct TickPairMse {
    refs: Vec<(String, f64)>,
}

impl Strategy<OrderBrokerSimulator> for TickPairMse {
    fn warmup(&self) -> usize {
        40
    }

    fn on_quote_tick(&mut self, ctx: &mut OrderBrokerSimulator, q: &TickQuote) {
        let mid = q.mid();
        let symbol = q.symbol.to_string();
        let Some(i) = self.refs.iter().position(|(s, _)| *s == symbol) else {
            self.refs.push((symbol, mid));
            return;
        };
        let reference = self.refs[i].1;
        // ⚠ `position_of` (exchange truth), NOT the `OrderBroker::position` shadow. This is why the
        // impl stays OrderBrokerSimulator-bound: `OrderBrokerSimulator::position` returns the response-latency
        // SHADOW whenever the opt-in latency gate is armed, and a profile knob arms it. Reading
        // the shadow here is a different strategy, not a generic version of this one.
        let pos = ctx.position_of(&symbol).size;
        if mid > reference * (1.0 + 3e-4) && pos <= 0.0 {
            ctx.place(&symbol, "", 1, 1.0 + pos.abs(), OrderPlacement::Market, false);
            self.refs[i].1 = mid;
        } else if mid < reference * (1.0 - 3e-4) && pos >= 0.0 {
            ctx.place(&symbol, "", -1, 1.0 + pos.abs(), OrderPlacement::Market, false);
            self.refs[i].1 = mid;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four simulator test fixtures here are `Strategy<OrderBrokerSimulator>` DELIBERATELY, and this records
    /// why so the next reader does not "fix" it.
    ///
    /// ⚠ A generic-mount probe once stood here asserting [`TickPairMse`] was portable. It was
    /// removed with the impl it proved: making it generic silently swapped `position_of(&symbol).size`
    /// (exchange truth) for `OrderBroker::position` — and `OrderBrokerSimulator::position` returns the
    /// response-latency SHADOW whenever the opt-in latency gate is armed, which a profile knob arms.
    /// Same code, different number, on a reachable path. That is a behaviour change wearing a
    /// refactor's clothes, and the type system was the thing objecting.
    ///
    /// The other three drive engine-only machinery with no live counterpart — the `PositionSizer`,
    /// the cash-gate `weight` ordering, the attached protective `stop`. `vike_model::strategy`'s own
    /// doc sanctions this: a strategy that needs backtest-only power is written against the concrete
    /// sim broker, "and the type system documents that it is not live-portable". They are not
    /// spaghetti; they are labelled.
    #[test]
    fn the_fixtures_are_deliberately_not_portable() {
        let _ = TickPairMse::default();
    }
}
