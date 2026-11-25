//! López de Prado's information-driven bars: the imbalance and runs folds and builders.
//! Restored 2026-11-16 (book W30) from `main` at `937912d083`, where they had been deleted for want
//! of a caller; the research study `microstructure` is that caller now.
//!
//! Each trade enters with a WEIGHT its caller gives: `1.0` for the tick flavour of the bars, its
//! `size` for the volume flavour, and its NOTIONAL for the dollar flavour ("Advances in Financial
//! Machine Learning", 2018, §2.3.2), priced by the caller through
//! `vike_model::ContractValue::order_notional(t.size, t.price)` for the contract the tape is of. This
//! crate prices no trade (`bars.rs`'s module doc says why). The buy/sell/delta volume split of an
//! emitted bar does not depend on the weight; a weight that is not a number never closes a bar.
use super::{BarAccumulator, OrderflowBar};
#[cfg(doc)]
use super::{DollarBarBuilder, VolumeBarBuilder};
use vike_marketdata::TickTrade;
use vike_marketdata::aggressor::{TickAggressor, classify};

/// Fold one (size > 0) trade into an imbalance accumulator: add it to the bar, then move the running
/// signed imbalance `theta` by `+weight` for a buy aggressor and `-weight` for a sell. When `|theta|`
/// reaches `threshold` the crossing trade is INCLUDED (no split: the book samples AT the crossing
/// trade), the bar is finished and `accumulator`/`theta` reset.
fn imbalance_fold(
    accumulator: &mut BarAccumulator,
    theta: &mut f64,
    t: &TickTrade,
    weight: f64,
    threshold: f64,
) -> Option<OrderflowBar> {
    accumulator.add(t);
    *theta += match classify(t) {
        TickAggressor::Buy => weight,
        TickAggressor::Sell => -weight,
    };
    if theta.abs() >= threshold {
        let bar = accumulator.finish();
        *accumulator = BarAccumulator::new();
        *theta = 0.0;
        Some(bar)
    } else {
        None
    }
}

/// Fold one (size > 0) trade into a runs accumulator: add it to the bar, then grow its side's run
/// (`buy_run` or `sell_run`) by its weight. When the larger run reaches `threshold` the crossing
/// trade is INCLUDED, the bar is finished and all state resets. This is the book's runs statistic
/// `theta = max(buy run, sell run)` (§2.3.2.2): the cumulative weight per side, NOT the longest
/// streak of consecutive same-side trades.
fn runs_fold(
    accumulator: &mut BarAccumulator,
    buy_run: &mut f64,
    sell_run: &mut f64,
    t: &TickTrade,
    weight: f64,
    threshold: f64,
) -> Option<OrderflowBar> {
    accumulator.add(t);
    match classify(t) {
        TickAggressor::Buy => *buy_run += weight,
        TickAggressor::Sell => *sell_run += weight,
    }
    if buy_run.max(*sell_run) >= threshold {
        let bar = accumulator.finish();
        *accumulator = BarAccumulator::new();
        *buy_run = 0.0;
        *sell_run = 0.0;
        Some(bar)
    } else {
        None
    }
}

/// Imbalance bars (López de Prado's information-driven bars, §2.3.2): close a bar when the absolute
/// cumulative signed weight `|sum(+-weight)|` (buys `+`, sells `-`, the weight per the module doc)
/// reaches `threshold`. The fixed-threshold form: the crossing trade is included whole (no split,
/// unlike [`VolumeBarBuilder`]/[`DollarBarBuilder`]). The book's adaptive threshold
/// (`E[T] |2 P(buy) - 1|`, from earlier bars' length and buy share) is not built.
pub struct ImbalanceBarBuilder {
    threshold: f64,
    theta: f64,
    accumulator: BarAccumulator,
}

impl ImbalanceBarBuilder {
    pub fn new(threshold: f64) -> Self {
        assert!(threshold > 0.0, "ImbalanceBarBuilder threshold must be > 0");
        ImbalanceBarBuilder { threshold, theta: 0.0, accumulator: BarAccumulator::new() }
    }

    /// Fold one trade of `weight`; the closed bar when this trade crossed the imbalance threshold. A
    /// zero or negative size is skipped (it never opens or advances a bar).
    pub fn push(&mut self, t: &TickTrade, weight: f64) -> Option<OrderflowBar> {
        if t.size <= 0.0 {
            return None;
        }
        imbalance_fold(&mut self.accumulator, &mut self.theta, t, weight, self.threshold)
    }

    /// Snapshot of the OPEN (unclosed) bar. Pure read.
    pub fn forming(&self) -> Option<OrderflowBar> {
        self.accumulator.open.then(|| self.accumulator.finish())
    }

    /// The batch twin of [`ImbalanceBarBuilder::push`]: `weight` weighs each trade.
    pub fn from_trades(
        trades: &[TickTrade],
        threshold: f64,
        weight: impl Fn(&TickTrade) -> f64,
    ) -> Vec<OrderflowBar> {
        assert!(threshold > 0.0);
        let mut out = Vec::new();
        let mut accumulator = BarAccumulator::new();
        let mut theta = 0.0;
        for t in trades.iter().filter(|t| t.size > 0.0) {
            if let Some(bar) = imbalance_fold(&mut accumulator, &mut theta, t, weight(t), threshold)
            {
                out.push(bar);
            }
        }
        out // trailing partial bar dropped
    }
}

/// Runs bars (López de Prado's information-driven bars, §2.3.2.2): close a bar when the larger of the
/// two cumulative per-side runs (the buy run is the sum of the weights of buy aggressors, the sell
/// run of sells, the weight per the module doc) reaches `threshold`. The fixed-threshold form; the
/// crossing trade is included whole. The book's adaptive threshold (`E[T] max(P, 1 - P)`) is not
/// built.
pub struct RunsBarBuilder {
    threshold: f64,
    buy_run: f64,
    sell_run: f64,
    accumulator: BarAccumulator,
}

impl RunsBarBuilder {
    pub fn new(threshold: f64) -> Self {
        assert!(threshold > 0.0, "RunsBarBuilder threshold must be > 0");
        RunsBarBuilder {
            threshold,
            buy_run: 0.0,
            sell_run: 0.0,
            accumulator: BarAccumulator::new(),
        }
    }

    /// Fold one trade of `weight`; the closed bar when `max(buy run, sell run)` crossed `threshold`.
    /// A zero or negative size is skipped.
    pub fn push(&mut self, t: &TickTrade, weight: f64) -> Option<OrderflowBar> {
        if t.size <= 0.0 {
            return None;
        }
        runs_fold(
            &mut self.accumulator,
            &mut self.buy_run,
            &mut self.sell_run,
            t,
            weight,
            self.threshold,
        )
    }

    /// Snapshot of the OPEN (unclosed) bar. Pure read.
    pub fn forming(&self) -> Option<OrderflowBar> {
        self.accumulator.open.then(|| self.accumulator.finish())
    }

    /// The batch twin of [`RunsBarBuilder::push`]: `weight` weighs each trade.
    pub fn from_trades(
        trades: &[TickTrade],
        threshold: f64,
        weight: impl Fn(&TickTrade) -> f64,
    ) -> Vec<OrderflowBar> {
        assert!(threshold > 0.0);
        let mut out = Vec::new();
        let mut accumulator = BarAccumulator::new();
        let (mut buy_run, mut sell_run) = (0.0, 0.0);
        for t in trades.iter().filter(|t| t.size > 0.0) {
            if let Some(bar) =
                runs_fold(&mut accumulator, &mut buy_run, &mut sell_run, t, weight(t), threshold)
            {
                out.push(bar);
            }
        }
        out // trailing partial bar dropped
    }
}

#[path = "info_driven_tests.rs"]
#[cfg(test)]
mod info_driven_tests;
