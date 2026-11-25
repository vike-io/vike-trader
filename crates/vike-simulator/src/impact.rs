//! impact — the rule for how much of the opt-in **market-impact slippage model** each fill lane is
//! missing, and the tick lane's market context.
//!
//! The model itself — [`vike_fills::market_impact::ImpactModel`], the Almgren et al. (2005)
//! [`vike_fills::market_impact::ImpactModelAlmgrenChriss`], its constants, its units and its
//! bar-window market context — lives ONE layer down in `vike_fills::market_impact` (moved
//! unchanged to vike-analytics on 2026-11-17, book FORWARD-IMPROVE, and on to vike-fills beside the
//! fill price on 2026-11-18), so the paper book (`vike_paper`) charges the very
//! model this engine charges and a calibration reads it exactly. What stays here is the engine's: the
//! LANES.
//!
//! The frozen cost model applies one FLAT adverse move per fill
//! ([`vike_fills::broker_sim::adverse_fill_price`]): a 5bp `slippage` costs the same whether the
//! order is 1 share or 10% of the day's volume. That is wrong in the direction that flatters a
//! backtest — size is exactly what a real book charges for.
//!
//! ## ONE model, three lanes, two answers
//!
//! This module shipped bar-mode-only, on the claim that "where L2 depth is RECORDED, vike already
//! does better than any model". That claim is HALF true, and the half it gets wrong is the reason
//! this module now reaches the replay lanes. What the L2 walk supplies is the TEMPORARY
//! concession — the ladder a taker eats for demanding liquidity NOW — measured off real displayed
//! depth, which no model can beat. What it supplies is NOTHING of the PERMANENT footprint,
//! because a replayed book is a RECORDING: it was written by a market that never saw our order,
//! so the next snapshot, every subsequent mark and our eventual exit are all priced as though we
//! had not traded. That error is one-directional (always in the strategy's favour) and it scales
//! with size — the same shape as the error [`vike_fills::fill_model::FillModelL2Book`] itself was
//! built to remove, one term further out.
//!
//! So the lane does not choose a MODEL, it chooses which TERMS it is still missing
//! ([`ImpactTerms`], selected at the wire-in site by [`crate::OrderBrokerSimulator::slippage_for`]):
//!
//! | lane | what its price law already charges for size | terms charged here |
//! |---|---|---|
//! | bar (`FillModelKind::Bar`) | nothing — one OHLC price plus a flat haircut | [`ImpactTerms::Both`] |
//! | tick (`FillModelKind::Tick`) | nothing — buy@ask / sell@bid fills ANY size at the top of book | [`ImpactTerms::Both`] |
//! | L2 (`FillModelKind::L2Book`, book present) | the temporary term, from the real ladder | [`ImpactTerms::PermanentOnly`] |
//! | L2 with no book (the documented degrade to the tick tier) | nothing | [`ImpactTerms::Both`] |
//!
//! There is deliberately no second model and no second coefficient set: the paper identifies the
//! two terms SEPARATELY, so asking this one model for its permanent half is reading it, not
//! forking it. Two cost models that can disagree is the failure the root `CLAUDE.md` has a whole
//! convention against.
//!
//! ## The maker exemption is a property of the LANE, not of `OrderKind`
//!
//! Impact is the cost of DEMANDING liquidity, so a fill that supplied it is exempt. Reading that
//! off the engine's `is_maker` flag alone was correct on the one lane this module used to reach
//! and is FALSE on both of the lanes it now reaches, because that flag is not a statement about
//! aggressiveness at all — `crates/vike-simulator/src/engine.rs`'s `dispatch_fill` sets it from
//! the order KIND and says so in its own comment ("a MARKETABLE limit books as a maker fill"),
//! which it does because the FEE side needs it that way. What a `Limit` fill is actually priced
//! at differs per lane:
//!
//! | lane | what a `Limit` fill is priced at | supplied liquidity? |
//! |---|---|---|
//! | bar (`FillModelKind::Bar`) | `vike_model::order_fill_price`'s `Limit` arm: `price.min(bar.open)` for a buy — at-or-better than the limit, reached because the market came DOWN to a resting order | YES → exempt |
//! | tick (`FillModelKind::Tick`) | `vike_fills::fill_model::FillModelTick` fills a buy limit ONLY when `ask <= price`, AT THE ASK, for ANY size: it crossed the spread and took the touch | NO → charged |
//! | L2 (`FillModelKind::L2Book`, book present) | `vike_marketdata::book_taker_price` capped at the limit — a WALK down real resting levels for the order's own size | NO → charged |
//!
//! So [`crate::OrderBrokerSimulator::slippage_for`] exempts a maker on the BAR lane only
//! ([`crate::OrderBrokerSimulator::charges_impact`]), and the two replay lanes charge every fill. That is not
//! a widening for its own sake: leaving the `OrderKind` exemption in place on those lanes made the
//! whole charge AVOIDABLE — spell a taker as a marketable limit one tick through the touch and it
//! fills at the same price for the same size and pays nothing, which is the free lunch this module
//! exists to remove, wearing a different order kind.
//!
//! ⚠ **The accepted cost of charging a crossing limit: the fill can land past its own limit
//! price.** A buy limit at 103 that filled at the ask of 102 has `1/102` of headroom — just under
//! 1% — and an estimate larger than that reports a fill above 103, a price no venue would have
//! given. The honest reading of that case is that the order would not have filled THAT SIZE at
//! that price at all, which is a fill-SIZE consequence no price law here can express. Of the three
//! available answers this is the least bad, and the other two are worse in stateable ways: NOT
//! charging is the avoidance hole above (optimistic, in the operator's favour); CAPPING at the
//! limit makes the cost SATURATE exactly where it is largest — the fill that most moved the book
//! is the one whose estimate most exceeds the limit and is truncated — so the charge stops being
//! monotone in size at precisely the sizes it exists to penalise, flattering big makers while
//! looking like it penalises them. Charging it uncapped errs PESSIMISTIC, which is the safe
//! direction for a backtest, and the extreme tail is already counted
//! ([`crate::OrderBrokerSimulator::slippage_saturations`]).
//!
//! ⚠ **What is still missing on the maker side is not a price effect at all**, and it is
//! deliberately not built here. Our resting order is not in the replayed book, so the level it
//! joins is shallower on the tape than it would really have been, and a recorded sweep that
//! `crates/vike-simulator/src/engine/queued.rs`'s `queue_gate` treats as a strict cross (fill in
//! full) would in reality have stopped INSIDE our order. That is a fill-SIZE law and it belongs to
//! [`crate::queue_model`], beside the rest of the queue arithmetic — putting it here would be the
//! second-model failure this module just argued against.
//!
//! OPT-IN: activated by `StrategyEngineConfig::impact = Some(model)`. `None` (the default) leaves the
//! fill path byte-identical — the wire-in site ([`crate::OrderBrokerSimulator::slippage_for`]) returns the
//! flat `slippage` field unchanged, with no arithmetic performed on it, so every parity and
//! golden fixture (r1/r3/r4/r7_gate/engine_kernel_parity/properties_fills) is untouched.
//!
//! ⚠ **On the tick lane the period is one TRADE PRINT, and that is a far smaller unit than a
//! bar** — [`ImpactModelTickWindow`] measures `sigma` over consecutive print-to-print returns and
//! `average_volume` as the mean printed size. Both move with the measurement period in opposite
//! directions (`sigma` falls as the period shortens, `qty/avg_volume` rises), so a context
//! measured per PRINT rather than per DAY inflates the published estimate by a large multiple —
//! at the paper's own exponents, of order `N^(alpha - 1/2)` for `N` prints in a day, i.e. tens.
//! The estimate can then exceed 1.0 and SATURATE
//! ([`crate::OrderBrokerSimulator::slippage_saturations`] is the counter that says so).
//!
//! ⚠ **`exec_time` is NOT the knob that answers this, and a previous revision of this paragraph
//! said it was.** `exec_time` scales the TEMPORARY term ONLY: it cancels out of the permanent
//! term exactly (`nu * exec_time == qty / average_volume`), which is the paper's own
//! horizon-independence and is pinned three times
//! ([`vike_fills::market_impact::ImpactModelAlmgrenChriss::exec_time`]'s own doc,
//! `execution_time_scales_only_the_temporary_term`, `permanent_impact_does_not_move_with_the_horizon`).
//! So on the L2 lane — which charges [`ImpactTerms::PermanentOnly`] and nothing else — `exec_time`
//! is PROVABLY INERT, and on the tick lane it moves only the smaller of the two addends at a high
//! participation rate. **The knob that does move it, on every lane, is the coefficient pair**
//! `gamma` and `eta` (settable in a strategy's stated slippage table, the `market_impact` model of
//! `vike_model::ProfileEngineSlippage`).

use std::collections::VecDeque;

#[cfg(doc)]
use vike_fills::market_impact::ImpactTerms;
use vike_fills::market_impact::{ImpactModelMarketStatistics, measure_series};

// The bar window's measurement (`ImpactModelMarketStatistics`, `window_stats`, `window_stats_before`
// and the `measure_series` both windows fold through) moved, unchanged, to vike-analytics on
// 2026-11-16 (book W26) and on to `crates/vike-fills/src/market_impact.rs` on 2026-11-18.

/// Upper bound on what [`ImpactModelTickWindow::with_capacity`] pre-reserves; a larger `cap` still holds
/// `cap` prints, it just grows into them.
const RESERVE_CEILING: usize = 4_096;

/// The TICK lane's market context: a bounded rolling window of the tape's own TRADE PRINTS,
/// measured by the same `measure_series` the bar window uses.
///
/// **Trade prints only, and that is the honest choice rather than a convenience.** A quote tick
/// carries no traded volume at all (`vike_marketdata::quote_tick_to_bar` projects `volume = 0.0`), so
/// feeding quotes here would drive `average_volume` toward zero and the participation rate toward
/// infinity — an impact estimate that grows because the tape is quote-heavy, which is the
/// opposite of what a deep quoting market means. A tape with no trade prints therefore measures
/// nothing and [`Self::stats`] returns `None`, which the wire-in site treats exactly as it treats
/// a too-short bar window: charge the flat slippage alone rather than guess.
///
/// BOUNDED BY CONSTRUCTION. The window holds at most `cap` observations (the engine passes
/// `StrategyEngineConfig::impact_window`) and evicts the oldest, so a 100M-tick replay costs `cap`
/// observations per symbol rather than growing with the tape — the same discipline
/// `StrategyEngineConfig::equity_sampling` exists to enforce on the equity curve.
///
/// ⚠ `cap` counts PRINTS here, not bars. [`vike_fills::market_impact::DEFAULT_IMPACT_WINDOW`] is 21, chosen as one
/// trading month of DAILY bars; 21 prints is a fraction of a second on a liquid tape, so a tick
/// run should set `impact_window` deliberately. Nothing scales it automatically — a lane that
/// silently reinterpreted the operator's number would be worse than one that spends it literally.
#[derive(Debug, Clone)]
pub struct ImpactModelTickWindow {
    cap: usize,
    /// `(price, size)` per print, oldest first
    obs: VecDeque<(f64, f64)>,
}

impl ImpactModelTickWindow {
    /// A window holding at most `cap` prints. `cap == 0` is a window that never records
    /// anything and always measures `None` — the same inert answer `window_stats` gives a
    /// zero-length bar window, rather than a panic or an unbounded buffer.
    pub fn with_capacity(cap: usize) -> Self {
        // Reserve up to a sane ceiling rather than `cap` outright: `impact_window` is operator
        // input and the deque grows on demand anyway, so an absurd number must not turn into an
        // absurd allocation PER SYMBOL at the top of a replay.
        ImpactModelTickWindow { cap, obs: VecDeque::with_capacity(cap.min(RESERVE_CEILING)) }
    }

    /// Record one trade print. Non-finite or non-positive prices/sizes are recorded as given —
    /// `measure_series` is the one place that decides a window is unmeasurable, so a
    /// degenerate print is never silently dropped into a window that then looks healthy.
    pub fn push(&mut self, price: f64, size: f64) {
        if self.cap == 0 {
            return;
        }
        // `while`, not `if`: the invariant this must restore is `len < cap` before the push, and
        // an equality check would silently stop enforcing it if the window were ever handed a
        // smaller capacity than it already holds.
        while self.obs.len() >= self.cap {
            self.obs.pop_front();
        }
        self.obs.push_back((price, size));
    }

    /// Measure this window, or `None` when it cannot support an estimate (see
    /// `measure_series`).
    pub fn stats(&self) -> Option<ImpactModelMarketStatistics> {
        measure_series(self.obs.iter().map(|o| o.0), self.obs.iter().map(|o| o.1), self.obs.len())
    }

    /// Prints currently held (at most the capacity it was built with).
    pub fn len(&self) -> usize {
        self.obs.len()
    }

    /// No print recorded yet — the run's warmup, during which [`Self::stats`] measures `None`.
    pub fn is_empty(&self) -> bool {
        self.obs.is_empty()
    }
}

#[cfg(test)]
mod tests;
