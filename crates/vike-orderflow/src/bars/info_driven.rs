//! López-de-Prado information-driven bars: `FlowUnit`, the imbalance and runs folds and builders.
use super::{BarAcc, OrderflowBar};
#[cfg(doc)]
use super::{DollarBarBuilder, VolumeBarBuilder};
use vike_marketdata::TradeTick;
use vike_model::aggressor::{Side, classify};

/// The per-trade quantity an [`ImbalanceBarBuilder`]/[`RunsBarBuilder`] weights each signed
/// trade by — the tick/volume/dollar flavor of López de Prado's information-driven bars
/// (AFML §2.3.2). `Tick` weights every trade as 1, `Volume` as its `size`, `Dollar` as
/// `price·size`. A `price ≤ 0` trade therefore contributes ≤ 0 under `Dollar`, so it can never
/// advance a `Dollar` close-threshold on its own (real trades price > 0); the ordinary
/// buy/sell/delta volume split on the emitted bar is unaffected by the chosen unit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlowUnit {
    Tick,
    Volume,
    Dollar,
}

impl FlowUnit {
    /// The unsigned quantity trade `t` contributes under this unit (`t.size` assumed > 0).
    fn weight(self, t: &TradeTick) -> f64 {
        match self {
            FlowUnit::Tick => 1.0,
            FlowUnit::Volume => t.size,
            FlowUnit::Dollar => t.price * t.size,
        }
    }
}

/// Fold one (size>0) trade into an imbalance accumulator: add it to the OHLC/volume `acc`,
/// then advance the running signed imbalance `theta` by ±`kind.weight(t)` (+ for a buy
/// aggressor, − for a sell). When `|theta|` reaches `threshold` the crossing trade is INCLUDED
/// (no split — LdP samples AT the crossing tick), the bar is finished, and `acc`/`theta` reset.
fn imbalance_fold(
    acc: &mut BarAcc,
    theta: &mut f64,
    t: &TradeTick,
    kind: FlowUnit,
    threshold: f64,
) -> Option<OrderflowBar> {
    acc.add(t);
    let w = kind.weight(t);
    *theta += match classify(t) {
        Side::Buy => w,
        Side::Sell => -w,
    };
    if theta.abs() >= threshold {
        let bar = acc.finish();
        *acc = BarAcc::new();
        *theta = 0.0;
        Some(bar)
    } else {
        None
    }
}

/// Fold one (size>0) trade into a runs accumulator: add it to the OHLC/volume `acc`, then grow
/// the same-side run (`buy_run` or `sell_run`) by `kind.weight(t)`. When the larger of the two
/// runs reaches `threshold` the crossing trade is INCLUDED, the bar is finished, and all state
/// resets. This is LdP's runs statistic θ = max(buy-run, sell-run) (AFML §2.3.2.2) — the
/// cumulative per-side accumulation, NOT the longest CONSECUTIVE same-signed streak.
fn runs_fold(
    acc: &mut BarAcc,
    buy_run: &mut f64,
    sell_run: &mut f64,
    t: &TradeTick,
    kind: FlowUnit,
    threshold: f64,
) -> Option<OrderflowBar> {
    acc.add(t);
    let w = kind.weight(t);
    match classify(t) {
        Side::Buy => *buy_run += w,
        Side::Sell => *sell_run += w,
    }
    if buy_run.max(*sell_run) >= threshold {
        let bar = acc.finish();
        *acc = BarAcc::new();
        *buy_run = 0.0;
        *sell_run = 0.0;
        Some(bar)
    } else {
        None
    }
}

/// Imbalance bars (López de Prado information-driven bars, AFML §2.3.2): close a bar when the
/// absolute cumulative signed imbalance `|Σ ±unit|` (buy +, sell −; `unit` per [`FlowUnit`])
/// reaches `threshold`. Fixed-threshold deterministic form — the crossing trade is fully
/// included (no split, unlike [`VolumeBarBuilder`]/[`DollarBarBuilder`]); the buy/sell/delta
/// split on the emitted [`OrderflowBar`] is always the ordinary traded volume. OPT-IN: a caller
/// that never constructs this leaves every existing bar path byte-identical. The EWMA-adaptive
/// threshold (`E[T]·|2P(buy)−1|`, tracked from prior bars' length + buy-probability) is a
/// documented future extension; this is the deterministic fixed-threshold variant.
pub struct ImbalanceBarBuilder {
    kind: FlowUnit,
    threshold: f64,
    theta: f64,
    acc: BarAcc,
}

impl ImbalanceBarBuilder {
    pub fn new(kind: FlowUnit, threshold: f64) -> Self {
        assert!(threshold > 0.0, "ImbalanceBarBuilder threshold must be > 0");
        ImbalanceBarBuilder { kind, threshold, theta: 0.0, acc: BarAcc::new() }
    }

    /// Fold one trade; returns the closed bar iff this trade crossed the imbalance threshold.
    /// Zero/negative-size trades are skipped entirely (never open or advance a bar).
    pub fn push(&mut self, t: &TradeTick) -> Option<OrderflowBar> {
        if t.size <= 0.0 {
            return None;
        }
        imbalance_fold(&mut self.acc, &mut self.theta, t, self.kind, self.threshold)
    }

    /// Snapshot of the OPEN (unclosed) bar — the chart's forming bar. Pure read.
    pub fn forming(&self) -> Option<OrderflowBar> {
        self.acc.open.then(|| self.acc.finish())
    }

    pub fn from_trades(trades: &[TradeTick], kind: FlowUnit, threshold: f64) -> Vec<OrderflowBar> {
        assert!(threshold > 0.0);
        let mut out = Vec::new();
        let mut acc = BarAcc::new();
        let mut theta = 0.0;
        for t in trades.iter().filter(|t| t.size > 0.0) {
            if let Some(bar) = imbalance_fold(&mut acc, &mut theta, t, kind, threshold) {
                out.push(bar);
            }
        }
        out // trailing partial bar dropped
    }
}

/// Runs bars (López de Prado information-driven bars, AFML §2.3.2.2): close a bar when the
/// larger of the two cumulative per-side runs — buy-run = Σ unit over buy aggressors, sell-run
/// = Σ unit over sells (`unit` per [`FlowUnit`]) — reaches `threshold`, i.e. when
/// θ = max(buy-run, sell-run) ≥ threshold. This is LdP's cumulative-side statistic, NOT the
/// longest CONSECUTIVE same-signed streak. Fixed-threshold deterministic form; crossing trade
/// fully included; the emitted [`OrderflowBar`]'s buy/sell/delta split is the ordinary traded
/// volume. OPT-IN — constructing none leaves the existing bars byte-identical. The
/// EWMA-adaptive threshold (`E[T]·max{P, 1−P}`) is the noted extension.
pub struct RunsBarBuilder {
    kind: FlowUnit,
    threshold: f64,
    buy_run: f64,
    sell_run: f64,
    acc: BarAcc,
}

impl RunsBarBuilder {
    pub fn new(kind: FlowUnit, threshold: f64) -> Self {
        assert!(threshold > 0.0, "RunsBarBuilder threshold must be > 0");
        RunsBarBuilder { kind, threshold, buy_run: 0.0, sell_run: 0.0, acc: BarAcc::new() }
    }

    /// Fold one trade; returns the closed bar iff `max(buy-run, sell-run)` crossed `threshold`.
    /// Zero/negative-size trades are skipped entirely.
    pub fn push(&mut self, t: &TradeTick) -> Option<OrderflowBar> {
        if t.size <= 0.0 {
            return None;
        }
        runs_fold(
            &mut self.acc,
            &mut self.buy_run,
            &mut self.sell_run,
            t,
            self.kind,
            self.threshold,
        )
    }

    /// Snapshot of the OPEN (unclosed) bar — the chart's forming bar. Pure read.
    pub fn forming(&self) -> Option<OrderflowBar> {
        self.acc.open.then(|| self.acc.finish())
    }

    pub fn from_trades(trades: &[TradeTick], kind: FlowUnit, threshold: f64) -> Vec<OrderflowBar> {
        assert!(threshold > 0.0);
        let mut out = Vec::new();
        let mut acc = BarAcc::new();
        let mut buy_run = 0.0;
        let mut sell_run = 0.0;
        for t in trades.iter().filter(|t| t.size > 0.0) {
            if let Some(bar) = runs_fold(&mut acc, &mut buy_run, &mut sell_run, t, kind, threshold)
            {
                out.push(bar);
            }
        }
        out
    }
}

#[path = "info_driven_tests.rs"]
#[cfg(test)]
mod info_driven_tests;
