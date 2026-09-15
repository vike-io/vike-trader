//! Tick-, volume- and dollar-interval bar aggregation with a buy/sell/delta split, plus the
//! opt-in López-de-Prado information-driven bars ([`ImbalanceBarBuilder`]/[`RunsBarBuilder`],
//! AFML §2.3.2). All reuse the one [`crate::classify::signed`] buy/sell semantic. New builders
//! are constructed explicitly by the caller — a default path that builds none leaves the
//! tick/volume/dollar bar behavior byte-identical.
use crate::classify::{Side, classify, signed};
use vike_model::TradeTick;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn tk(ts: i64, price: f64, size: f64, ibm: bool) -> TradeTick {
        TradeTick { ts, local_ts: 0, price, size, is_buyer_maker: ibm, symbol: String::new() }
    }

    // 4 trades, n=2 → two bars. Bar1: buy 1@100, sell 2@101 → o100 h101 l100 c101, buy1 sell2 delta-1.
    fn seq() -> Vec<TradeTick> {
        vec![
            tk(1, 100.0, 1.0, false),
            tk(2, 101.0, 2.0, true),
            tk(3, 99.0, 3.0, false),
            tk(4, 100.0, 1.0, true),
        ]
    }

    #[test]
    fn tick_bar_expected_values() {
        let bars = TickBarBuilder::from_trades(&seq(), 2);
        assert_eq!(bars.len(), 2);
        assert_eq!(
            bars[0],
            OrderflowBar {
                o: 100.0,
                h: 101.0,
                l: 100.0,
                c: 101.0,
                buy_vol: 1.0,
                sell_vol: 2.0,
                delta: -1.0,
                trade_count: 2,
                first_ts: 1,
                last_ts: 2
            }
        );
        assert_eq!(
            bars[1],
            OrderflowBar {
                o: 99.0,
                h: 100.0,
                l: 99.0,
                c: 100.0,
                buy_vol: 3.0,
                sell_vol: 1.0,
                delta: 2.0,
                trade_count: 2,
                first_ts: 3,
                last_ts: 4
            }
        );
    }

    #[test]
    fn tick_bar_partial_not_emitted_and_zero_size_skipped() {
        let mut trades = seq();
        trades.push(tk(5, 100.0, 5.0, false)); // 5th trade → partial, not emitted
        trades.insert(0, tk(0, 100.0, 0.0, false)); // zero-size → skipped entirely
        let bars = TickBarBuilder::from_trades(&trades, 2);
        assert_eq!(bars.len(), 2); // still exactly two full bars from the 4 non-zero trades before the 5th
    }

    #[test]
    fn tick_bar_streaming_equals_batch() {
        let trades = seq();
        let batch = TickBarBuilder::from_trades(&trades, 2);
        let mut b = TickBarBuilder::new(2);
        let stream: Vec<OrderflowBar> = trades.iter().filter_map(|t| b.push(t)).collect();
        assert_eq!(stream.len(), batch.len());
        for (s, x) in stream.iter().zip(&batch) {
            assert_eq!(bits(s), bits(x)); // bit-identical
        }
    }

    fn bits(b: &OrderflowBar) -> (u64, u64, u64, u64, u64, u64, u64, u32, i64, i64) {
        (
            b.o.to_bits(),
            b.h.to_bits(),
            b.l.to_bits(),
            b.c.to_bits(),
            b.buy_vol.to_bits(),
            b.sell_vol.to_bits(),
            b.delta.to_bits(),
            b.trade_count,
            b.first_ts,
            b.last_ts,
        )
    }

    #[test]
    fn invariant_delta_and_nonneg() {
        // seq bar totals are 3 (buy1+sell2) and 4 (buy3+sell1); assert the real invariants.
        for b in TickBarBuilder::from_trades(&seq(), 2) {
            assert_eq!(b.delta, b.buy_vol - b.sell_vol);
            assert!(b.buy_vol >= 0.0 && b.sell_vol >= 0.0);
        }
    }

    #[test]
    fn forming_exposes_open_accumulator() {
        let mut b = TickBarBuilder::new(2);
        assert_eq!(b.forming(), None);
        b.push(&tk(1, 100.0, 1.0, false));
        let f = b.forming().unwrap();
        assert_eq!((f.o, f.c, f.buy_vol, f.trade_count), (100.0, 100.0, 1.0, 1));
        b.push(&tk(2, 101.0, 2.0, true)); // closes the bar
        assert_eq!(b.forming(), None);

        let mut v = VolumeBarBuilder::new(2.0);
        let closed = v.push(&tk(1, 100.0, 5.0, false)); // 2 closes + remainder 1.0 open
        assert_eq!(closed.len(), 2);
        let vf = v.forming().unwrap();
        assert_eq!((vf.buy_vol, vf.sell_vol), (1.0, 0.0));
    }

    // threshold 2.0. One buy trade 5@100 → 5/2 = two full bars (2 each) + remainder 1 open.
    #[test]
    fn volume_bar_splits_one_big_trade() {
        let trades = vec![tk(1, 100.0, 5.0, false)];
        let bars = VolumeBarBuilder::from_trades(&trades, 2.0);
        assert_eq!(bars.len(), 2);
        for b in &bars {
            assert_eq!(
                b,
                &OrderflowBar {
                    o: 100.0,
                    h: 100.0,
                    l: 100.0,
                    c: 100.0,
                    buy_vol: 2.0,
                    sell_vol: 0.0,
                    delta: 2.0,
                    trade_count: 1,
                    first_ts: 1,
                    last_ts: 1
                }
            );
        }
    }

    // threshold 3.0: buy 2@100, sell 2@101 → bar1 = 2 buy + 1 sell(split) → o100 h101 l100 c101 buy2 sell1 delta1; remainder 1 sell opens bar2.
    #[test]
    fn volume_bar_splits_across_two_trades() {
        let trades = vec![tk(1, 100.0, 2.0, false), tk(2, 101.0, 2.0, true)];
        let bars = VolumeBarBuilder::from_trades(&trades, 3.0);
        assert_eq!(bars.len(), 1);
        assert_eq!(
            bars[0],
            OrderflowBar {
                o: 100.0,
                h: 101.0,
                l: 100.0,
                c: 101.0,
                buy_vol: 2.0,
                sell_vol: 1.0,
                delta: 1.0,
                trade_count: 2,
                first_ts: 1,
                last_ts: 2
            }
        );
    }

    #[test]
    fn volume_bar_streaming_equals_batch() {
        let trades =
            vec![tk(1, 100.0, 5.0, false), tk(2, 101.0, 2.0, true), tk(3, 99.0, 4.0, false)];
        let batch = VolumeBarBuilder::from_trades(&trades, 2.0);
        let mut b = VolumeBarBuilder::new(2.0);
        let mut stream = Vec::new();
        for t in &trades {
            stream.extend(b.push(t));
        }
        assert_eq!(stream.len(), batch.len());
        for (s, x) in stream.iter().zip(&batch) {
            assert_eq!(bits(s), bits(x));
        }
    }

    // threshold $200: buy 1@100 ($100) then sell 2@100 ($200) → bar1 takes 1 of the sell
    // ($100, exactly filling the room) and closes; the remaining 1 sell opens bar2.
    #[test]
    fn dollar_bar_splits_at_notional_boundary() {
        let trades = vec![tk(1, 100.0, 1.0, false), tk(2, 100.0, 2.0, true)];
        let bars = DollarBarBuilder::from_trades(&trades, 200.0);
        assert_eq!(bars.len(), 1);
        assert_eq!(
            bars[0],
            OrderflowBar {
                o: 100.0,
                h: 100.0,
                l: 100.0,
                c: 100.0,
                buy_vol: 1.0,
                sell_vol: 1.0,
                delta: 0.0,
                trade_count: 2,
                first_ts: 1,
                last_ts: 2
            }
        );
    }

    // threshold $100: one buy 5@50 = $250 → two full bars (2 units = $100 each) +
    // remainder 1 unit ($50) forming. The dollar twin of volume_bar_splits_one_big_trade.
    #[test]
    fn dollar_bar_splits_one_big_trade() {
        let trades = vec![tk(1, 50.0, 5.0, false)];
        let bars = DollarBarBuilder::from_trades(&trades, 100.0);
        assert_eq!(bars.len(), 2);
        for b in &bars {
            assert_eq!(
                b,
                &OrderflowBar {
                    o: 50.0,
                    h: 50.0,
                    l: 50.0,
                    c: 50.0,
                    buy_vol: 2.0,
                    sell_vol: 0.0,
                    delta: 2.0,
                    trade_count: 1,
                    first_ts: 1,
                    last_ts: 1
                }
            );
        }
        let mut d = DollarBarBuilder::new(100.0);
        assert_eq!(d.push(&trades[0]).len(), 2);
        let f = d.forming().unwrap();
        assert_eq!((f.buy_vol, f.sell_vol, f.trade_count), (1.0, 0.0, 1));
    }

    #[test]
    fn dollar_bar_partial_not_emitted_and_zero_size_skipped() {
        // $50 of a $100 threshold → no bar; zero-size trades are skipped entirely.
        let trades = vec![tk(0, 100.0, 0.0, false), tk(1, 50.0, 1.0, false)];
        assert!(DollarBarBuilder::from_trades(&trades, 100.0).is_empty());
        let mut d = DollarBarBuilder::new(100.0);
        assert!(d.push(&trades[0]).is_empty());
        assert_eq!(d.forming(), None); // zero-size never opened a bar
        assert!(d.push(&trades[1]).is_empty());
        assert_eq!(d.forming().unwrap().buy_vol, 1.0);
    }

    #[test]
    fn dollar_bar_streaming_equals_batch() {
        let trades =
            vec![tk(1, 100.0, 5.0, false), tk(2, 101.0, 2.0, true), tk(3, 99.0, 4.0, false)];
        let batch = DollarBarBuilder::from_trades(&trades, 250.0);
        let mut b = DollarBarBuilder::new(250.0);
        let mut stream = Vec::new();
        for t in &trades {
            stream.extend(b.push(t));
        }
        assert_eq!(stream.len(), batch.len());
        assert!(!batch.is_empty());
        for (s, x) in stream.iter().zip(&batch) {
            assert_eq!(bits(s), bits(x));
        }
    }

    // A price<=0 trade can never close a dollar bar (no notional) — it must be absorbed
    // without looping, and a later real trade still closes on cumulative notional.
    #[test]
    fn dollar_bar_zero_price_absorbed() {
        let mut d = DollarBarBuilder::new(100.0);
        assert!(d.push(&tk(1, 0.0, 3.0, true)).is_empty());
        assert_eq!(d.forming().unwrap().sell_vol, 3.0);
        let closed = d.push(&tk(2, 100.0, 1.0, false)); // $100 → closes
        assert_eq!(closed.len(), 1);
        assert_eq!((closed[0].buy_vol, closed[0].sell_vol), (1.0, 3.0));
    }

    // ---- López de Prado information-driven bars: imbalance + runs ----

    // A richer signed stream that closes several bars for both builders across all units.
    fn mixed() -> Vec<TradeTick> {
        vec![
            tk(1, 100.0, 1.0, false), // buy 1@100
            tk(2, 101.0, 2.0, false), // buy 2@101
            tk(3, 99.0, 2.0, true),   // sell 2@99
            tk(4, 98.0, 2.0, true),   // sell 2@98
            tk(5, 100.0, 3.0, false), // buy 3@100
            tk(6, 100.0, 1.0, true),  // sell 1@100
        ]
    }

    // Volume imbalance θ = Σ(±size). threshold 3: +1,+3(close),−2,−4(close) → two bars, each
    // closing exactly on the boundary-crossing trade (which is fully included, not split).
    #[test]
    fn imbalance_bar_volume_closes_on_abs_threshold() {
        let trades = vec![
            tk(1, 100.0, 1.0, false),
            tk(2, 101.0, 2.0, false),
            tk(3, 99.0, 2.0, true),
            tk(4, 98.0, 2.0, true),
        ];
        let bars = ImbalanceBarBuilder::from_trades(&trades, FlowUnit::Volume, 3.0);
        assert_eq!(bars.len(), 2);
        assert_eq!(
            bars[0],
            OrderflowBar {
                o: 100.0,
                h: 101.0,
                l: 100.0,
                c: 101.0,
                buy_vol: 3.0,
                sell_vol: 0.0,
                delta: 3.0,
                trade_count: 2,
                first_ts: 1,
                last_ts: 2
            }
        );
        assert_eq!(
            bars[1],
            OrderflowBar {
                o: 99.0,
                h: 99.0,
                l: 98.0,
                c: 98.0,
                buy_vol: 0.0,
                sell_vol: 4.0,
                delta: -4.0,
                trade_count: 2,
                first_ts: 3,
                last_ts: 4
            }
        );
    }

    // Tick imbalance θ = Σ(±1). threshold 3, all size 1: the sell at t3 pushes θ back down so
    // the bar only closes at t5 (net +3) — proves tick imbalance is the SIGNED count.
    #[test]
    fn imbalance_bar_tick_counts_signed_ticks() {
        let trades = vec![
            tk(1, 100.0, 1.0, false), // +1
            tk(2, 100.0, 1.0, false), // +2
            tk(3, 100.0, 1.0, true),  // +1 (sell)
            tk(4, 100.0, 1.0, false), // +2
            tk(5, 100.0, 1.0, false), // +3 → close
        ];
        let bars = ImbalanceBarBuilder::from_trades(&trades, FlowUnit::Tick, 3.0);
        assert_eq!(bars.len(), 1);
        assert_eq!(
            bars[0],
            OrderflowBar {
                o: 100.0,
                h: 100.0,
                l: 100.0,
                c: 100.0,
                buy_vol: 4.0,
                sell_vol: 1.0,
                delta: 3.0,
                trade_count: 5,
                first_ts: 1,
                last_ts: 5
            }
        );
    }

    // Volume runs θ = max(buy-run, sell-run). threshold 3: the sell side reaches 4 at t3
    // (close), then the lone buy 3@100 reaches 3 at t4 (close) → two bars.
    #[test]
    fn runs_bar_volume_closes_on_max_side() {
        let trades = vec![
            tk(1, 100.0, 1.0, false), // buy_run 1
            tk(2, 101.0, 2.0, true),  // sell_run 2
            tk(3, 99.0, 2.0, true),   // sell_run 4 → close
            tk(4, 100.0, 3.0, false), // buy_run 3 → close
        ];
        let bars = RunsBarBuilder::from_trades(&trades, FlowUnit::Volume, 3.0);
        assert_eq!(bars.len(), 2);
        assert_eq!(
            bars[0],
            OrderflowBar {
                o: 100.0,
                h: 101.0,
                l: 99.0,
                c: 99.0,
                buy_vol: 1.0,
                sell_vol: 4.0,
                delta: -3.0,
                trade_count: 3,
                first_ts: 1,
                last_ts: 3
            }
        );
        assert_eq!(
            bars[1],
            OrderflowBar {
                o: 100.0,
                h: 100.0,
                l: 100.0,
                c: 100.0,
                buy_vol: 3.0,
                sell_vol: 0.0,
                delta: 3.0,
                trade_count: 1,
                first_ts: 4,
                last_ts: 4
            }
        );
    }

    // Runs and imbalance are DIFFERENT statistics: an alternating stream keeps |imbalance| ≤ 1
    // yet grows the per-side runs. threshold 3 → runs closes one bar at t5 (buy-run hits 3);
    // imbalance never closes over the same stream.
    #[test]
    fn runs_bar_distinct_from_imbalance() {
        let trades = vec![
            tk(1, 100.0, 1.0, false), // buy
            tk(2, 100.0, 1.0, true),  // sell
            tk(3, 100.0, 1.0, false), // buy
            tk(4, 100.0, 1.0, true),  // sell
            tk(5, 100.0, 1.0, false), // buy → buy_run 3
        ];
        let runs = RunsBarBuilder::from_trades(&trades, FlowUnit::Volume, 3.0);
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0],
            OrderflowBar {
                o: 100.0,
                h: 100.0,
                l: 100.0,
                c: 100.0,
                buy_vol: 3.0,
                sell_vol: 2.0,
                delta: 1.0,
                trade_count: 5,
                first_ts: 1,
                last_ts: 5
            }
        );
        // same stream, imbalance threshold 3 → |θ| peaks at 1, no bar closes.
        assert!(ImbalanceBarBuilder::from_trades(&trades, FlowUnit::Volume, 3.0).is_empty());
    }

    #[test]
    fn imbalance_runs_forming_and_zero_size_skipped() {
        let mut ib = ImbalanceBarBuilder::new(FlowUnit::Volume, 3.0);
        assert_eq!(ib.forming(), None);
        assert_eq!(ib.push(&tk(0, 100.0, 0.0, false)), None); // zero-size skipped
        assert_eq!(ib.forming(), None); // never opened a bar
        assert_eq!(ib.push(&tk(1, 100.0, 1.0, false)), None); // θ=1, forming
        let f = ib.forming().unwrap();
        assert_eq!((f.buy_vol, f.sell_vol, f.trade_count), (1.0, 0.0, 1));
        assert!(ib.push(&tk(2, 101.0, 2.0, false)).is_some()); // θ=3 → closes
        assert_eq!(ib.forming(), None); // reset after close

        let mut rb = RunsBarBuilder::new(FlowUnit::Volume, 3.0);
        assert_eq!(rb.push(&tk(0, 100.0, 0.0, true)), None); // zero-size skipped
        assert_eq!(rb.forming(), None);
        assert_eq!(rb.push(&tk(1, 100.0, 1.0, true)), None); // sell_run=1, forming
        assert!(rb.push(&tk(2, 100.0, 2.0, true)).is_some()); // sell_run=3 → closes
        assert_eq!(rb.forming(), None);
    }

    // Streaming push == batch from_trades, bit-for-bit, for both builders across all units.
    #[test]
    fn imbalance_runs_streaming_equals_batch() {
        let trades = mixed();
        for (kind, th) in
            [(FlowUnit::Tick, 2.0), (FlowUnit::Volume, 3.0), (FlowUnit::Dollar, 100.0)]
        {
            let ibatch = ImbalanceBarBuilder::from_trades(&trades, kind, th);
            let mut ib = ImbalanceBarBuilder::new(kind, th);
            let istream: Vec<OrderflowBar> = trades.iter().filter_map(|t| ib.push(t)).collect();
            assert!(!ibatch.is_empty());
            assert_eq!(istream.len(), ibatch.len());
            for (s, x) in istream.iter().zip(&ibatch) {
                assert_eq!(bits(s), bits(x));
            }

            let rbatch = RunsBarBuilder::from_trades(&trades, kind, th);
            let mut rb = RunsBarBuilder::new(kind, th);
            let rstream: Vec<OrderflowBar> = trades.iter().filter_map(|t| rb.push(t)).collect();
            assert!(!rbatch.is_empty());
            assert_eq!(rstream.len(), rbatch.len());
            for (s, x) in rstream.iter().zip(&rbatch) {
                assert_eq!(bits(s), bits(x));
            }
        }
    }

    // OFF / additive: the LdP builders are new, explicitly-constructed types with their own
    // state — running them over a stream cannot perturb the tick/volume/dollar builders. Pin
    // the existing tick bars bit-for-bit (identical to `tick_bar_expected_values`) to prove the
    // additive change left the shipped bar paths byte-identical.
    #[test]
    fn additive_builders_leave_existing_bars_byte_identical() {
        let trades = seq();
        let _imb = ImbalanceBarBuilder::from_trades(&trades, FlowUnit::Volume, 2.0);
        let _run = RunsBarBuilder::from_trades(&trades, FlowUnit::Volume, 2.0);
        let tick = TickBarBuilder::from_trades(&trades, 2);
        assert_eq!(tick.len(), 2);
        let want = [
            OrderflowBar {
                o: 100.0,
                h: 101.0,
                l: 100.0,
                c: 101.0,
                buy_vol: 1.0,
                sell_vol: 2.0,
                delta: -1.0,
                trade_count: 2,
                first_ts: 1,
                last_ts: 2,
            },
            OrderflowBar {
                o: 99.0,
                h: 100.0,
                l: 99.0,
                c: 100.0,
                buy_vol: 3.0,
                sell_vol: 1.0,
                delta: 2.0,
                trade_count: 2,
                first_ts: 3,
                last_ts: 4,
            },
        ];
        for (g, w) in tick.iter().zip(&want) {
            assert_eq!(bits(g), bits(w));
        }
        // volume/dollar builders stay deterministic and unaffected too.
        assert_eq!(
            VolumeBarBuilder::from_trades(&trades, 3.0),
            VolumeBarBuilder::from_trades(&trades, 3.0)
        );
        assert_eq!(
            DollarBarBuilder::from_trades(&trades, 250.0),
            DollarBarBuilder::from_trades(&trades, 250.0)
        );
    }
}
