//! The adverse move one paper fill pays: the flat fraction, plus the opt-in market-impact model's
//! charge, the `config.paper_slippage_table` row's slippage table resolved by ONE function
//! (`vike_fills::market_impact::slippage_terms`) and charged by ONE model, so a paper fill pays what
//! the simulator's bar lane charges for the same fill and bars
//! (`crates/vike-simulator/src/engine/sim_broker/slippage.rs`'s `slippage_for`).
//!
//! - **The bar lane's rules**: this book prices every fill by the bar law (`FillModelBar` on the bar's,
//!   its sides' or a quote's path), so a fill that books MAKER (`OrderResting::books_as_maker`, a combo
//!   priced by a limit) supplied liquidity and pays the flat fraction alone, a taker pays the model's
//!   whole cost function ([`ImpactTerms::Both`]).
//! - **The market context is the bars that CLOSED BEFORE the fill**
//!   (`vike_fills::market_impact::window_stats_before`, the simulator's one rule) over the last
//!   `window + 1` bars of the fill's symbol this book was handed, kept ONLY while a model is configured.
//!   ⚠ A fill on a QUOTE (decision 0285's amendment) at a time past the last bar's close reads one bar
//!   fewer than it could: the rule takes the last bar as the one the time falls inside. A context too
//!   short or degenerate to measure (the first bars, a volume-less series) charges the flat fraction alone.
//! - **No model, no change**: without one, or for a maker, the fraction is the `slippage` field itself,
//!   the same `f64` with no arithmetic, so every book built without [`ExecutionClientPaper::with_slippage_terms`]
//!   fills byte for byte as before.

use std::collections::VecDeque;
use std::sync::Arc;

use indexmap::IndexMap;
use vike_fills::market_impact::{ImpactInputs, ImpactModel, ImpactTerms, window_stats_before};
use vike_marketdata::Bar;

use super::ExecutionClientPaper;

/// The impact model a paper book charges a taker fill, the window it measures over, and the bars it
/// measures: at most `window + 1` per symbol, the newest last.
pub(super) struct PaperImpact {
    model: Arc<dyn ImpactModel>,
    window: usize,
    bars: IndexMap<String, VecDeque<Bar>>,
}

impl PaperImpact {
    pub(super) fn new(model: Arc<dyn ImpactModel>, window: usize) -> Self {
        PaperImpact { model, window, bars: IndexMap::new() }
    }
}

impl ExecutionClientPaper {
    /// Keep `bar` (of `symbol`) for the impact model's market context, BEFORE the bar judges a fill:
    /// a fill stamped at its open must find it, so the rule leaves out the bar it falls inside and not
    /// the one before. Nothing without a model.
    pub(super) fn keep_bar_for_impact(&mut self, symbol: &str, bar: &Bar) {
        let Some(impact) = self.impact.as_mut() else { return };
        let bars = impact.bars.entry(symbol.to_string()).or_default();
        bars.push_back(bar.clone());
        // the bar a fill falls inside is left out, so the window needs one bar more than it measures
        while bars.len() > impact.window + 1 {
            bars.pop_front();
        }
    }

    /// The adverse-move fraction one fill of `size` on `symbol` at `ts` pays: the `slippage` field
    /// itself for a maker, without a model or without a measurable context, else that plus the
    /// model's cost of the fill.
    pub(super) fn slippage_for(&mut self, symbol: &str, size: f64, ts: i64, is_maker: bool) -> f64 {
        let flat = self.slippage;
        if is_maker {
            return flat;
        }
        let Some(impact) = self.impact.as_mut() else { return flat };
        let Some(bars) = impact.bars.get_mut(symbol) else { return flat };
        let Some(stats) = window_stats_before(bars.make_contiguous(), ts, impact.window) else {
            return flat;
        };
        // shortcut: the size itself is compared with the bars' volume; a series counting the settle
        // coin (the simulator's `coin_volume`) is converted there and not here, upgrade if a paper
        // account trades an inverse contract on such bars.
        let extra = impact.model.impact_fraction_for(
            &ImpactInputs {
                qty: size.abs(),
                average_volume: stats.average_volume,
                sigma: stats.sigma,
            },
            ImpactTerms::Both,
        );
        flat + extra
    }
}
