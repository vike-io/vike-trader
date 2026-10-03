//! Tick-, volume- and dollar-interval bar aggregation with a buy/sell/delta split, plus the
//! opt-in López-de-Prado information-driven bars ([`ImbalanceBarBuilder`]/[`RunsBarBuilder`],
//! AFML §2.3.2). All reuse the one [`crate::classify::signed`] buy/sell semantic. New builders
//! are constructed explicitly by the caller — a default path that builds none leaves the
//! tick/volume/dollar bar behavior byte-identical.
use crate::classify::{Side, classify, signed};
use vike_marketdata::TradeTick;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct OrderflowBar {
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub buy_vol: f64,
    pub sell_vol: f64,
    pub delta: f64,
    pub trade_count: u32,
    pub first_ts: i64,
    pub last_ts: i64,
}

pub struct TickBarBuilder {
    n: u32,
    acc: BarAcc,
}

/// Accumulates the running fields of one open bar. `count` tracks contributing trades.
/// `notional` (running Σ price·size) is only consulted by `DollarBarBuilder`; the tick/
/// volume builders carry it inertly.
struct BarAcc {
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    buy: f64,
    sell: f64,
    notional: f64,
    count: u32,
    first_ts: i64,
    last_ts: i64,
    open: bool,
}

impl BarAcc {
    fn new() -> Self {
        BarAcc {
            o: 0.0,
            h: 0.0,
            l: 0.0,
            c: 0.0,
            buy: 0.0,
            sell: 0.0,
            notional: 0.0,
            count: 0,
            first_ts: 0,
            last_ts: 0,
            open: false,
        }
    }

    /// Add the full `t` (size assumed > 0) at its price.
    fn add(&mut self, t: &TradeTick) {
        let (b, s) = signed(t);
        if !self.open {
            self.o = t.price;
            self.h = t.price;
            self.l = t.price;
            self.first_ts = t.ts;
            self.open = true;
        }
        self.h = self.h.max(t.price);
        self.l = self.l.min(t.price);
        self.c = t.price;
        self.buy += b;
        self.sell += s;
        self.notional += t.price * t.size;
        self.count += 1;
        self.last_ts = t.ts;
    }

    fn finish(&self) -> OrderflowBar {
        OrderflowBar {
            o: self.o,
            h: self.h,
            l: self.l,
            c: self.c,
            buy_vol: self.buy,
            sell_vol: self.sell,
            delta: self.buy - self.sell,
            trade_count: self.count,
            first_ts: self.first_ts,
            last_ts: self.last_ts,
        }
    }
}

impl TickBarBuilder {
    pub fn new(n: u32) -> Self {
        assert!(n >= 1, "TickBarBuilder n must be >= 1");
        TickBarBuilder { n, acc: BarAcc::new() }
    }

    pub fn push(&mut self, t: &TradeTick) -> Option<OrderflowBar> {
        if t.size <= 0.0 {
            return None;
        }
        self.acc.add(t);
        if self.acc.count == self.n {
            let bar = self.acc.finish();
            self.acc = BarAcc::new();
            Some(bar)
        } else {
            None
        }
    }

    /// Snapshot of the OPEN (unclosed) bar — the chart's forming bar. Pure read.
    pub fn forming(&self) -> Option<OrderflowBar> {
        self.acc.open.then(|| self.acc.finish())
    }

    pub fn from_trades(trades: &[TradeTick], n: u32) -> Vec<OrderflowBar> {
        assert!(n >= 1);
        let mut out = Vec::new();
        let mut acc = BarAcc::new();
        for t in trades.iter().filter(|t| t.size > 0.0) {
            acc.add(t);
            if acc.count == n {
                out.push(acc.finish());
                acc = BarAcc::new();
            }
        }
        out // trailing partial bar dropped
    }
}

/// Fold one (size>0) trade into `acc`, closing bars whenever `acc.buy+acc.sell` reaches
/// `threshold`; push closed bars to `out`. Splits the trade across boundaries by volume.
fn vol_fold(acc: &mut BarAcc, out: &mut Vec<OrderflowBar>, t: &TradeTick, threshold: f64) {
    let mut remaining = t.size;
    while remaining > 0.0 {
        let have = acc.buy + acc.sell;
        let room = threshold - have;
        let take = remaining.min(room);
        // a synthetic sub-trade of `take` at the trade's price/side/ts:
        let sub = TradeTick {
            ts: t.ts,
            local_ts: t.local_ts,
            price: t.price,
            size: take,
            is_buyer_maker: t.is_buyer_maker,
            symbol: String::new(),
        };
        acc.add(&sub);
        remaining -= take;
        if acc.buy + acc.sell >= threshold {
            out.push(acc.finish());
            *acc = BarAcc::new();
        }
    }
}

pub struct VolumeBarBuilder {
    threshold: f64,
    acc: BarAcc,
}

impl VolumeBarBuilder {
    pub fn new(threshold: f64) -> Self {
        assert!(threshold > 0.0, "VolumeBarBuilder threshold must be > 0");
        VolumeBarBuilder { threshold, acc: BarAcc::new() }
    }

    pub fn push(&mut self, t: &TradeTick) -> Vec<OrderflowBar> {
        let mut out = Vec::new();
        if t.size > 0.0 {
            vol_fold(&mut self.acc, &mut out, t, self.threshold);
        }
        out
    }

    /// Snapshot of the OPEN (unclosed) bar — the chart's forming bar. Pure read.
    pub fn forming(&self) -> Option<OrderflowBar> {
        self.acc.open.then(|| self.acc.finish())
    }

    pub fn from_trades(trades: &[TradeTick], threshold: f64) -> Vec<OrderflowBar> {
        assert!(threshold > 0.0);
        let mut out = Vec::new();
        let mut acc = BarAcc::new();
        for t in trades.iter().filter(|t| t.size > 0.0) {
            vol_fold(&mut acc, &mut out, t, threshold);
        }
        out
    }
}

/// Fold one (size>0) trade into `acc`, closing bars whenever `acc.notional` (running
/// Σ price·size) reaches `threshold`; push closed bars to `out`. The dollar twin of
/// `vol_fold`: splits the trade across a boundary by the SIZE that fills the remaining
/// dollar room (`room / price`). Rounding-stall guard: if the computed take is
/// non-positive or too small to advance `remaining` (sub-ulp), the whole remainder is
/// absorbed into the open bar instead of looping — also the path a degenerate
/// (`price <= 0`) trade lands on (it contributes volume but no notional, so it can never
/// close a bar on its own).
fn dollar_fold(acc: &mut BarAcc, out: &mut Vec<OrderflowBar>, t: &TradeTick, threshold: f64) {
    let mut remaining = t.size;
    while remaining > 0.0 {
        let room = threshold - acc.notional;
        let mut take = remaining.min(room / t.price);
        if take.is_nan() || take <= 0.0 || remaining - take == remaining {
            take = remaining;
        }
        let sub = TradeTick {
            ts: t.ts,
            local_ts: t.local_ts,
            price: t.price,
            size: take,
            is_buyer_maker: t.is_buyer_maker,
            symbol: String::new(),
        };
        acc.add(&sub);
        remaining -= take;
        if acc.notional >= threshold {
            out.push(acc.finish());
            *acc = BarAcc::new();
        }
    }
}

/// Dollar bars: close when the bar's cumulative traded notional (Σ price·size) reaches
/// `threshold`. Same interface and boundary convention as [`VolumeBarBuilder`] — the
/// boundary trade is split, its filling slice included in the closing bar.
pub struct DollarBarBuilder {
    threshold: f64,
    acc: BarAcc,
}

impl DollarBarBuilder {
    pub fn new(threshold: f64) -> Self {
        assert!(threshold > 0.0, "DollarBarBuilder threshold must be > 0");
        DollarBarBuilder { threshold, acc: BarAcc::new() }
    }

    pub fn push(&mut self, t: &TradeTick) -> Vec<OrderflowBar> {
        let mut out = Vec::new();
        if t.size > 0.0 {
            dollar_fold(&mut self.acc, &mut out, t, self.threshold);
        }
        out
    }

    /// Snapshot of the OPEN (unclosed) bar — the chart's forming bar. Pure read.
    pub fn forming(&self) -> Option<OrderflowBar> {
        self.acc.open.then(|| self.acc.finish())
    }

    pub fn from_trades(trades: &[TradeTick], threshold: f64) -> Vec<OrderflowBar> {
        assert!(threshold > 0.0);
        let mut out = Vec::new();
        let mut acc = BarAcc::new();
        for t in trades.iter().filter(|t| t.size > 0.0) {
            dollar_fold(&mut acc, &mut out, t, threshold);
        }
        out
    }
}

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

#[path = "bars_tests.rs"]
#[cfg(test)]
mod bars_tests;
