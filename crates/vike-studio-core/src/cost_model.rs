//! **What a Studio run's cost stamp declares beyond the profile it ran**: the terms a slice cannot
//! price ([`NOT_MODELLED`]) and the fills it realised ([`FillMix`]).
//!
//! Ruling: `docs/decisions/0063-the-studio-optimizer-derives-its-cost-model-and-declares-what-it-cannot.md`.
//!
//! WHAT a run is costed under is not decided here: a Studio run is a profile run
//! (`vike_backtest::run::profile::ProfileBacktest::for_slice`), and its commission and slippage are the
//! strategy's params, else what the strategy states, else none (the owner's ruling of 2026-11-18: no
//! venue table). The backtesthub's `to_wire_cost_model` reads the stamp off that profile with the
//! harness's own `vike_backtest::run::costs::run_costs`, so there is one cost resolution in the
//! workspace, the harness's.
//!
//! # What deriving buys on day one, stated rather than oversold
//!
//! The engine classifies a fill as maker or taker by the order KIND, not crossing aggressiveness,
//! and every strategy `crate::templates` ships is a market order, so the shipped population realises
//! a 100% taker mix: what a commission buys TODAY is its taker side, not maker/taker
//! discrimination. [`FillMix`] is what makes that visible to the person reading
//! a report rather than only to a reader of this module.

use vike_analytics::TradingRecord;

/// **Every term `0063` ruled on that this run did NOT price** — carried INTO the result rather than
/// left in the record, because a statement that says only what IS modelled invites a reader to
/// infer that nothing else exists.
///
/// ⚠ **Two CLASSES, and the entries say which, because the difference is what a reader would do
/// about it.** The first three are `0063`'s DECLARED RESIDUALS: they can flip a winner and nothing
/// in a slice implies them, so no amount of wire surface fixes them. The last is a DTO OMISSION —
/// `0063` ruled the information is server-side already and a boolean would reach it, and that
/// boolean is NOT BUILT HERE. When the boolean ships, its row LEAVES this list. `attach_funding`'s
/// row left on 2026-11-08: a slice is charged its stored funding when its request turns it on
/// (`attach_funding`, OFF when absent since 2026-11-21).
///
/// ⚠ This is deliberately not a list of everything an `[engine]` table can express: the authority
/// for that surface is `ProfileEngine`'s field set, gated by
/// `crates/vike-backtest/tests/engine_cfg_reaches_the_engine.rs`, and a Studio slice leaves every one
/// of them at the profile default. These are named because `0063` RULED on them one at a time.
pub const NOT_MODELLED: [&str; 4] = [
    "impact — the market_impact slippage model's coefficients are a CALIBRATION, not a venue \
     fact; nothing in a slice implies them, so a strategy that states the model without them runs \
     the published pair. (0047 measured this arm and it did NOT reorder its grid, even at 5,000x.)",
    "risk — the [risk] budget is an operator's, not a property of the data, and its gate REFUSES \
     orders outright: admissibility, which is strictly stronger than re-weighting",
    "resolution — the binary-outcome winners map is a server-side path or a hand-written table, \
     and a path must not cross this wire",
    "snap_to_properties (DTO omission, not a residual) — the PIT instrument grid is server-side \
     already and a boolean would reach it, but none is built: fills are NOT quantized to tick/step \
     and an opening fill below min_qty/min_notional is NOT dropped, so small-size candidates keep \
     trades a real grid would delete. 0063 also warns it is not a blind switch-on — snapping \
     re-tags bars with the venue, which can break a symbol-inferring strategy",
];

/// The line a walk-forward stamp adds to [`NOT_MODELLED`]: its [`FillMix`] measures nothing.
pub const WALKFORWARD_FILL_MIX: &str = "fill mix (walk-forward) — the profile walk-forward counts \
     no fills per out-of-sample window, so this stamp's maker_fills, taker_fills and fees_paid are \
     0 and measure nothing; the fees ARE charged and are in the equity curve";

/// The REALISED maker/taker mix and the commission a run actually paid, accumulated off the
/// `TradingRecord`s a run produced.
///
/// ⚠ **`0063` calls this the highest-value field in the stamp**: a schedule that never touched its
/// maker side and one that mattered look identical without it.
///
/// It sums the ENGINE's own counters (`TradingRecordSimulator::maker_fills` / `taker_fills` and
/// `TradingRecord::fees_paid`), never a re-derivation — `0063`'s "computed, never restated" constraint applied one
/// type down.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FillMix {
    /// Fills the engine booked as MAKER.
    pub maker_fills: u64,
    /// Fills the engine booked as TAKER.
    pub taker_fills: u64,
    /// Total commission charged (signed — a rebate-bearing schedule contributes negative terms).
    pub fees_paid: f64,
}

impl FillMix {
    /// Fold one run's counters in — once per sweep point, so the mix describes every run the
    /// answer reports.
    pub fn add(&mut self, result: &TradingRecord) {
        // Every run a sweep folds is the simulator's, so a record without its counters adds none.
        if let Some(simulator) = result.simulator() {
            self.maker_fills += simulator.maker_fills;
            self.taker_fills += simulator.taker_fills;
        }
        self.fees_paid += result.fees_paid;
    }

    /// This run's counters as a fresh mix — the single-run spelling of [`Self::add`].
    pub fn of(result: &TradingRecord) -> Self {
        let mut mix = FillMix::default();
        mix.add(result);
        mix
    }
}

#[path = "cost_model_tests.rs"]
#[cfg(test)]
mod cost_model_tests;
