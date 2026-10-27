//! `HonestyCounters`: what a run deferred, skipped, could not price and refused.

use serde::{Deserialize, Serialize};
use std::fmt;

use super::BacktestResult;

/// What the run DEFERRED, skipped, could not price and refused — the counters that tell an
/// AMBIGUOUS result apart from a clean one.
///
/// # The defect this closes
///
/// Every one of these was already accumulated on [`BacktestResult`] and every one of them reached
/// the report through exactly one door: [`crate::zero_trade::ZeroTradeReport`], which
/// `crate::zero_trade::ZeroTradeReport::analyze` emits ONLY when the run closed no trades and its
/// equity never moved. So a run in which two fills in five were resolved by an intrabar coin-flip,
/// or in which a configured impact model priced nothing because the bars carried `volume = 0`,
/// reported a number indistinguishable from a run where none of that happened — and the counters
/// that would have said so were sitting on the result, dying when `run` returned.
///
/// # This is a MEASUREMENT channel, not a refusal
///
/// A non-zero counter is not automatically a fault: the opening fills of a run legitimately precede
/// a measurable impact window, and a session gate is supposed to skip a closed venue. What they buy
/// is the ability to ASK. Every field's own authority is the matching
/// [`BacktestResult`] field, which carries the argument for what its non-zero readings mean.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HonestyCounters {
    /// See [`BacktestResult::intrabar_both_hit`]: fills where stop and target were both inside one
    /// bar and the engine had to choose. The one counter whose non-zero reading is a statement
    /// about the DATA's resolution rather than about the configuration.
    pub intrabar_both_hit: u32,
    /// See [`BacktestResult::stale_deferrals`].
    pub stale_deferrals: u64,
    /// See [`BacktestResult::session_deferrals`].
    pub session_deferrals: u64,
    /// See [`BacktestResult::impact_unpriced`] — fills a CONFIGURED impact model charged nothing
    /// for. A count equal to the fill count means the model was never applied at all.
    pub impact_unpriced: u64,
    /// See [`BacktestResult::below_min_reversals`] — fills this backtest executed that the LIVE
    /// gate would have denied. Expected to stay zero above dust sizes; a non-zero reading falsifies
    /// that argument, which is the whole reason the counter exists.
    pub below_min_reversals: u64,
    /// The warm-up the run GATED on, in bars/ticks — the EFFECTIVE number, not `Strategy::warmup()`
    /// (see [`BacktestResult::warmup`]).
    pub warmup: usize,
    /// The gate-drop ledger aggregated by reason, first-seen order preserved — exactly
    /// [`crate::zero_trade::aggregate_denials`] over [`BacktestResult::dropped`].
    ///
    /// ⚠ **The aggregation is UNCONDITIONAL here, and that is the fix.** `aggregate_denials` is a
    /// free function and was always callable on any run; the only caller was
    /// `crate::zero_trade::ZeroTradeReport::analyze`, which is gated on a zero-trade flat-equity
    /// run — so the ledger of a run that traded 400 times and had 3,000 orders refused by the
    /// margin gate reached no document. Nothing about the function needed changing: the gate was
    /// never in it.
    pub denials: Vec<(String, u64)>,
}

impl HonestyCounters {
    /// Mirror the counters off a finished [`BacktestResult`]. A pure copy plus one
    /// [`crate::zero_trade::aggregate_denials`] fold — no counting, and nothing on any fill lane.
    pub fn from_result(r: &BacktestResult) -> Self {
        HonestyCounters {
            intrabar_both_hit: r.intrabar_both_hit,
            stale_deferrals: r.stale_deferrals,
            session_deferrals: r.session_deferrals,
            impact_unpriced: r.impact_unpriced,
            below_min_reversals: r.below_min_reversals,
            warmup: r.warmup,
            denials: crate::zero_trade::aggregate_denials(&r.dropped),
        }
    }

    /// Whether anything happened worth reporting. `false` is the ordinary clean run, and it is what
    /// keeps a normal human table byte-identical to before this block existed.
    ///
    /// ⚠ `warmup` is deliberately NOT part of this: every strategy with an indicator declares one,
    /// so counting it would make the block print on essentially every run and the signal would be
    /// gone. It is recorded because a zero-trade diagnosis needs it, not because it is an anomaly.
    pub fn is_noteworthy(&self) -> bool {
        self.intrabar_both_hit > 0
            || self.stale_deferrals > 0
            || self.session_deferrals > 0
            || self.impact_unpriced > 0
            || self.below_min_reversals > 0
            || !self.denials.is_empty()
    }
}

impl fmt::Display for HonestyCounters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "intrabar_both_hit:   {}", self.intrabar_both_hit)?;
        writeln!(f, "stale_deferrals:     {}", self.stale_deferrals)?;
        writeln!(f, "session_deferrals:   {}", self.session_deferrals)?;
        writeln!(f, "impact_unpriced:     {}", self.impact_unpriced)?;
        writeln!(f, "below_min_reversals: {}", self.below_min_reversals)?;
        writeln!(f, "warmup:              {}", self.warmup)?;
        if self.denials.is_empty() {
            writeln!(f, "denials:             (none)")?;
        } else {
            writeln!(f, "denials:")?;
            for (reason, count) in &self.denials {
                writeln!(f, "  {reason}: {count}")?;
            }
        }
        Ok(())
    }
}
