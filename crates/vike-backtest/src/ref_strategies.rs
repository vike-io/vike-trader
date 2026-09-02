//! MSE reference strategies for the R4 cross-language gate (byte-equivalent Python twins in
//! `scripts/export_r4_fixtures.py`, removed by `751de662`'s Python purge).
//!
//! # Where the strategy/engine line runs in this file
//!
//! [`vike_model::Broker`] is the PORTABLE surface — the common denominator of the backtest and live
//! stacks. [`SimBroker`]'s inherent verbs are the simulator-only extension above it. A strategy here
//! is written `impl<B: Broker> Strategy<B>` when it needs nothing but the portable surface, and
//! `impl Strategy<SimBroker>` when it genuinely needs simulator machinery — and the second form is a
//! STATEMENT, not an oversight: the type system is what documents "this one cannot run live".
//!
//! [`TickPairMse`] is portable. The other four are NOT, and each is blocked on a CONCEPT the live
//! stack does not have, never on a missing accessor — which is why widening [`vike_model::Broker`]
//! would not rescue them. The five engine-only concepts, and who needs which:
//!
//! * the **[`crate::sizing::PositionSizer`]** — [`SimBroker::submit`]'s `raw = false` arm, which
//!   routes the requested size through `size_entry`. The live path is always raw; there is no live
//!   sizer to be portable to. [`CapsSizersMask`] exists to exercise exactly this.
//! * the **cash-gate `weight`** — [`SimBroker::submit`]'s `weight` argument, the priority the
//!   shared-cash gate drops entries in when cash runs out. Live has no shared-cash gate.
//!   [`GatedWeights`] exists to exercise exactly this.
//! * the **attached protective `stop`** armed BY the entry (`submit(.., stop)`), which also feeds
//!   `SizeContext::risk_stop`. Live arms a bracket as separate orders, not as a field on the entry.
//! * the **symbol universe** — the `SimBroker::symbols` FIELD. A live mount pins ONE
//!   `(venue, symbol)` series, so there is no universe to iterate.
//! * the **bar-count schedule** — the `SimBroker::schedule` FIELD plus
//!   [`DateRule::every_n_bars`]. The live twin (`vike_core::schedule::LiveSchedule`) takes
//!   WALL-CLOCK rules supplied as mount CONFIG, and "every 10 bars" has no wall-clock meaning.
//!
//! ⚠ Every strategy in this file is pinned BIT-FOR-BIT by
//! `crates/vike-backtest/tests/parity/r4_parity.rs`'s `mse_runs_bit_parity`, whose `assert_dropped`
//! compares the cash-gate `weight` itself. Changing which verbs one of them calls is a parity
//! question, never a tidiness one — run that gate.

use vike_model::{Bar, QuoteTick};

use vike_model::{Broker, Strategy};

use crate::engine::SimBroker;
use crate::schedule::DateRule;

fn sym_key(instrument: &Option<String>) -> String {
    // Strategy._sym_key: strip ".VENUE" (exporter runs with default_venue=None → bare already)
    let s = instrument.as_deref().unwrap_or("_");
    match s.split_once('.') {
        Some((sym, _)) => sym.to_string(),
        None => s.to_string(),
    }
}

/// Top-k momentum rotation (k=2, EveryNBars(10)): score = last/first of the seen closes;
/// winners get 1/k, held drop-outs get 0.0 — the CrossSectionalStrategy pattern.
///
/// ENGINE-BOUND on the **symbol universe** and the **bar-count schedule** (see the module doc).
/// Both are `SimBroker` FIELDS with no live counterpart — `every_n_bars` is not expressible as a
/// wall-clock `TimeRule`, and a live mount pins one series rather than a universe — so making this
/// portable is a DESIGN question (a params-supplied universe, a rule type both `Schedule` and
/// `LiveSchedule` can consume), not a seam-widening one. Its third engine verb,
/// `strategy_order_target_percent`, IS expressible in portable verbs except for the contract
/// multiplier read, which [`Broker`] does not carry.
#[derive(Default)]
pub struct RotationTopK {
    hist: Vec<(String, Vec<f64>)>,
}

impl RotationTopK {
    fn hist_mut(&mut self, key: &str) -> &mut Vec<f64> {
        if let Some(i) = self.hist.iter().position(|(k, _)| k == key) {
            &mut self.hist[i].1
        } else {
            self.hist.push((key.to_string(), Vec::new()));
            &mut self.hist.last_mut().unwrap().1
        }
    }
}

impl Strategy<SimBroker> for RotationTopK {
    fn on_start(&mut self, ctx: &mut SimBroker) {
        ctx.schedule.on(DateRule::every_n_bars(10), "rebal");
    }

    fn on_bar(&mut self, ctx: &mut SimBroker, bar: &Bar) {
        let _ = ctx;
        self.hist_mut(&sym_key(&bar.symbol)).push(bar.close);
    }

    fn on_schedule(&mut self, ctx: &mut SimBroker, tag: &str) {
        if tag != "rebal" {
            return;
        }
        const K: usize = 2;
        // scores in engine-symbols order (Python: dict insertion order = symbols order)
        let mut scores: Vec<(String, f64)> = Vec::new();
        for sym in ctx.symbols.clone() {
            if let Some((_, h)) = self.hist.iter().find(|(k, _)| *k == sym) {
                if !h.is_empty() {
                    scores.push((sym.clone(), h[h.len() - 1] / h[0]));
                }
            }
        }
        if scores.len() < K {
            return;
        }
        // stable sort desc by score (ties keep insertion order — Python sorted semantics)
        let mut ranked = scores.clone();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let winners: Vec<String> = ranked.iter().take(K).map(|(s, _)| s.clone()).collect();
        let w = 1.0 / winners.len() as f64;
        let mut target: Vec<(String, f64)> = winners.iter().map(|s| (s.clone(), w)).collect();
        for sym in ctx.symbols.clone() {
            if ctx.position_of(&sym).size != 0.0 && !target.iter().any(|(s, _)| *s == sym) {
                target.push((sym, 0.0));
            }
        }
        for (sym, pct) in target {
            ctx.strategy_order_target_percent(&sym, pct);
        }
    }
}

/// Per-symbol bracket cycle: every 7th bar when flat — market entry with a protective stop
/// (-3%) + a resting TP limit (+5%). One symbol runs granular sub-bars (exercises the
/// sub-bar-ordered SL/TP resolution + the OCO sibling cancel); the rest use the coarse path.
///
/// ENGINE-BOUND on the **attached protective `stop`** (see the module doc): its entry arms the
/// bracket as a FIELD on the order, which is the shape the sub-bar SL/TP resolution it exists to
/// exercise reads. Live arms a bracket as separate orders, so a portable rewrite would change what
/// is being tested, not just how it is spelled. It also calls `cancel_all`, whose nearest live
/// sibling (`LiveBroker::mass_cancel`) is account-wide rather than symbol-scoped.
#[derive(Default)]
pub struct BracketPerSymbol;

impl Strategy<SimBroker> for BracketPerSymbol {
    fn on_bar(&mut self, ctx: &mut SimBroker, bar: &Bar) {
        let sym = sym_key(&bar.symbol);
        if ctx.position_of(&sym).size == 0.0 && ctx.index.is_multiple_of(7) {
            ctx.cancel_all(&sym);
            ctx.submit(&sym, 1, 1.5, 0.0, false, Some(bar.close * 0.97));
            ctx.submit_limit(&sym, -1, 1.5, bar.close * 1.05, 0.0, false, None);
        }
    }
}

/// Shared-cash gate exercise (cash_gate=True): every 5th bar submit weighted entries across
/// the whole universe (cash runs out → weight-ordered drops); every 10th+5 close everything
/// (reductions-first path).
///
/// PERMANENTLY ENGINE-BOUND on the **cash-gate `weight`** (see the module doc). This strategy's
/// entire purpose is to drive a mechanism only the simulator has, and r4's `gated` run asserts the
/// resulting `dropped` rows INCLUDING their `weight` — so a portable rewrite would have to invent a
/// weight concept on the live path to keep meaning the same thing. It also reads the symbol
/// universe and calls `submit_close`, which deliberately bypasses both the sizer and the risk gate
/// that [`Broker::submit_market`] runs through.
#[derive(Default)]
pub struct GatedWeights;

impl Strategy<SimBroker> for GatedWeights {
    fn on_bar(&mut self, ctx: &mut SimBroker, bar: &Bar) {
        let sym = sym_key(&bar.symbol);
        let si = ctx.symbols.iter().position(|s| *s == sym).unwrap();
        if ctx.index.is_multiple_of(5) && ctx.position_of(&sym).size == 0.0 {
            let sizes = [30.0, 25.0, 20.0, 15.0];
            let weights = [1.0, 2.0, 3.0, 0.5];
            ctx.submit(&sym, 1, sizes[si % 4], weights[si % 4], false, None);
        } else if ctx.index % 10 == 5 && ctx.position_of(&sym).size != 0.0 {
            ctx.submit_close(&sym);
        }
    }
}

/// Caps + sizers + membership + mixed multipliers + account-level liquidation:
/// PctEquity(0.5) under a DrawdownThrottle, entries every 3rd bar when flat (S2 shorts),
/// protective stop at ±10% — over-notional books trigger the ALL-positions margin call
/// (the #303 mixed-multiplier gate).
///
/// PERMANENTLY ENGINE-BOUND on the **[`crate::sizing::PositionSizer`]** (see the module doc), and
/// its `999.0` size is the proof: that is a SENTINEL, not a quantity — r4's `caps_sizers_mask` run
/// mounts a real `DrawdownThrottleSizer { base: PctEquitySizer }`, whose `size` ignores
/// `SizeContext::intent` entirely and returns `pct·equity/(basis_price·multiplier)`. Submitted
/// through the portable [`Broker::submit_market`] (which is `raw = true`) the sentinel would be
/// taken literally. Live has no sizer to be portable to.
#[derive(Default)]
pub struct CapsSizersMask;

impl Strategy<SimBroker> for CapsSizersMask {
    fn on_bar(&mut self, ctx: &mut SimBroker, bar: &Bar) {
        let sym = sym_key(&bar.symbol);
        let si = ctx.symbols.iter().position(|s| *s == sym).unwrap();
        if ctx.position_of(&sym).size == 0.0 && ctx.index.is_multiple_of(2) {
            let side = if si == 2 { -1 } else { 1 };
            let stop = if side > 0 { bar.close * 0.96 } else { bar.close * 1.04 };
            ctx.submit(&sym, side, 999.0, 0.0, false, Some(stop));
        }
    }
}

/// Two-symbol tick momentum (run_ticks + TickFillModel): per-symbol 3bp flip.
///
/// PORTABLE (`impl<B: Broker> Strategy<B>`) — it needs nothing above the common-denominator
/// surface, so it runs unchanged on the live stack. It reached that form by rewriting two call
/// sites, and both are BYTE-identical here rather than merely equivalent:
///
/// * `SimBroker::position_of(sym).size` → [`Broker::position`]. The two differ ONLY while the
///   opt-in latency gate is armed, when `Broker::position` returns the response-latency SHADOW
///   (`SimBroker::shadow_pos`) and `position_of` returns exchange truth. That shadow is armed only
///   inside `StrategyEngine::run_ticks` with `EngineParams::latency_model = Some(..)`, which no r4
///   run sets — so this read is the frozen line verbatim there. ⚠ A FUTURE latency-armed run of
///   this strategy WOULD see the shadow, and that is the correct read for a strategy (it is what
///   live gives it), but it is a change the r4 gate cannot see.
/// * `SimBroker::submit(sym, side, qty, 0.0, false, None)` → [`Broker::submit_market`], which is the
///   same call with `raw = true`. The only difference is the sizer, and with no
///   `EngineParams::sizer` set the engine installs `PassThroughSizer`, whose `size` returns
///   `SizeContext::intent` — the same number `raw = true` skips the call for. Both forms then cross
///   the identical `gate_market_order`. (On a flip the entry is REDUCING anyway, which `size_entry`
///   passes through regardless of `raw`.)
#[derive(Default)]
pub struct TickPairMse {
    refs: Vec<(String, f64)>,
}

impl Strategy<SimBroker> for TickPairMse {
    fn warmup(&self) -> usize {
        40
    }

    fn on_quote_tick(&mut self, ctx: &mut SimBroker, q: &QuoteTick) {
        let mid = q.mid();
        let sym = q.symbol.clone();
        let Some(i) = self.refs.iter().position(|(s, _)| *s == sym) else {
            self.refs.push((sym, mid));
            return;
        };
        let reference = self.refs[i].1;
        // ⚠ `position_of` (exchange truth), NOT the `Broker::position` shadow. This is why the
        // impl stays SimBroker-bound: `SimBroker::position` returns the response-latency
        // SHADOW whenever the opt-in latency gate is armed, and a profile knob arms it. Reading
        // the shadow here is a different strategy, not a generic version of this one.
        let pos = ctx.position_of(&sym).size;
        if mid > reference * (1.0 + 3e-4) && pos <= 0.0 {
            ctx.submit_market(&sym, 1, 1.0 + pos.abs());
            self.refs[i].1 = mid;
        } else if mid < reference * (1.0 - 3e-4) && pos >= 0.0 {
            ctx.submit_market(&sym, -1, 1.0 + pos.abs());
            self.refs[i].1 = mid;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five strategies here are `Strategy<SimBroker>` DELIBERATELY, and this records why so the
    /// next reader does not "fix" it.
    ///
    /// ⚠ A generic-mount probe once stood here asserting [`TickPairMse`] was portable. It was
    /// removed with the impl it proved: making it generic silently swapped `position_of(&sym).size`
    /// (exchange truth) for `Broker::position` — and `SimBroker::position` returns the
    /// response-latency SHADOW whenever the opt-in latency gate is armed, which a profile knob arms.
    /// Same code, different number, on a reachable path. That is a behaviour change wearing a
    /// refactor's clothes, and the type system was the thing objecting.
    ///
    /// The other four depend on `SimBroker` FIELDS (`symbols`, `schedule`) or on engine-only
    /// concepts with no live counterpart — the `PositionSizer`, the cash-gate `weight` ordering, the
    /// attached protective `stop`. `vike_model::strategy`'s own doc sanctions this: a strategy that
    /// needs backtest-only power is written against the concrete sim broker, "and the type system
    /// documents that it is not live-portable". They are not spaghetti; they are labelled.
    #[test]
    fn the_ref_strategies_are_deliberately_not_portable() {
        // `BuyHold` (harness::registry) is the one that WAS free — it only ever called trait
        // methods, and its own `buy_hold_mounts_on_any_broker` is the generic probe that belongs
        // with a genuinely portable strategy.
        let _ = TickPairMse::default();
    }
}
