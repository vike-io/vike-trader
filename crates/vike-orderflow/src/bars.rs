//! Tick- and volume-interval bar aggregation with a buy/sell/delta split. Both reuse the one
//! [`vike_marketdata::aggressor::signed`] buy/sell semantic.
use vike_marketdata::TickTrade;
use vike_marketdata::aggressor::signed;

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
    pub first_timestamp: i64,
    pub last_timestamp: i64,
}

pub struct TickBarBuilder {
    n: u32,
    accumulator: BarAccumulator,
}

/// Accumulates the running fields of one open bar. `count` tracks contributing trades.
struct BarAccumulator {
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    buy: f64,
    sell: f64,
    count: u32,
    first_timestamp: i64,
    last_timestamp: i64,
    open: bool,
}

impl BarAccumulator {
    fn new() -> Self {
        BarAccumulator {
            o: 0.0,
            h: 0.0,
            l: 0.0,
            c: 0.0,
            buy: 0.0,
            sell: 0.0,
            count: 0,
            first_timestamp: 0,
            last_timestamp: 0,
            open: false,
        }
    }

    /// Add the full `t` (size assumed > 0) at its price.
    fn add(&mut self, t: &TickTrade) {
        let (b, s) = signed(t);
        if !self.open {
            self.o = t.price;
            self.h = t.price;
            self.l = t.price;
            self.first_timestamp = t.timestamp;
            self.open = true;
        }
        self.h = self.h.max(t.price);
        self.l = self.l.min(t.price);
        self.c = t.price;
        self.buy += b;
        self.sell += s;
        self.count += 1;
        self.last_timestamp = t.timestamp;
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
            first_timestamp: self.first_timestamp,
            last_timestamp: self.last_timestamp,
        }
    }
}

impl TickBarBuilder {
    pub fn new(n: u32) -> Self {
        assert!(n >= 1, "TickBarBuilder n must be >= 1");
        TickBarBuilder { n, accumulator: BarAccumulator::new() }
    }

    pub fn push(&mut self, t: &TickTrade) -> Option<OrderflowBar> {
        if t.size <= 0.0 {
            return None;
        }
        self.accumulator.add(t);
        if self.accumulator.count == self.n {
            let bar = self.accumulator.finish();
            self.accumulator = BarAccumulator::new();
            Some(bar)
        } else {
            None
        }
    }

    /// Snapshot of the OPEN (unclosed) bar — the chart's forming bar. Pure read.
    pub fn forming(&self) -> Option<OrderflowBar> {
        self.accumulator.open.then(|| self.accumulator.finish())
    }

    pub fn from_trades(trades: &[TickTrade], n: u32) -> Vec<OrderflowBar> {
        assert!(n >= 1);
        let mut out = Vec::new();
        let mut accumulator = BarAccumulator::new();
        for t in trades.iter().filter(|t| t.size > 0.0) {
            accumulator.add(t);
            if accumulator.count == n {
                out.push(accumulator.finish());
                accumulator = BarAccumulator::new();
            }
        }
        out // trailing partial bar dropped
    }
}

/// Fold one (size>0) trade into `accumulator`, closing bars whenever `accumulator.buy+accumulator.sell` reaches
/// `threshold`; push closed bars to `out`. Splits the trade across boundaries by volume.
fn vol_fold(
    accumulator: &mut BarAccumulator,
    out: &mut Vec<OrderflowBar>,
    t: &TickTrade,
    threshold: f64,
) {
    let mut remaining = t.size;
    while remaining > 0.0 {
        let have = accumulator.buy + accumulator.sell;
        let room = threshold - have;
        let take = remaining.min(room);
        // a synthetic sub-trade of `take` at the trade's price/side/ts:
        let sub = TickTrade {
            timestamp: t.timestamp,
            local_timestamp: t.local_timestamp,
            price: t.price,
            size: take,
            is_buyer_maker: t.is_buyer_maker,
            symbol: String::new(),
        };
        accumulator.add(&sub);
        remaining -= take;
        if accumulator.buy + accumulator.sell >= threshold {
            out.push(accumulator.finish());
            *accumulator = BarAccumulator::new();
        }
    }
}

pub struct VolumeBarBuilder {
    threshold: f64,
    accumulator: BarAccumulator,
}

impl VolumeBarBuilder {
    pub fn new(threshold: f64) -> Self {
        assert!(threshold > 0.0, "VolumeBarBuilder threshold must be > 0");
        VolumeBarBuilder { threshold, accumulator: BarAccumulator::new() }
    }

    pub fn push(&mut self, t: &TickTrade) -> Vec<OrderflowBar> {
        let mut out = Vec::new();
        if t.size > 0.0 {
            vol_fold(&mut self.accumulator, &mut out, t, self.threshold);
        }
        out
    }

    /// Snapshot of the OPEN (unclosed) bar — the chart's forming bar. Pure read.
    pub fn forming(&self) -> Option<OrderflowBar> {
        self.accumulator.open.then(|| self.accumulator.finish())
    }

    pub fn from_trades(trades: &[TickTrade], threshold: f64) -> Vec<OrderflowBar> {
        assert!(threshold > 0.0);
        let mut out = Vec::new();
        let mut accumulator = BarAccumulator::new();
        for t in trades.iter().filter(|t| t.size > 0.0) {
            vol_fold(&mut accumulator, &mut out, t, threshold);
        }
        out
    }
}

#[path = "bars_tests.rs"]
#[cfg(test)]
mod bars_tests;
