//! Tick- and volume-interval bar aggregation with a buy/sell/delta split. Both reuse the one
//! [`vike_marketdata::aggressor::signed`] buy/sell semantic.
use vike_marketdata::TradeTick;
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
    pub first_ts: i64,
    pub last_ts: i64,
}

pub struct TickBarBuilder {
    n: u32,
    acc: BarAcc,
}

/// Accumulates the running fields of one open bar. `count` tracks contributing trades.
struct BarAcc {
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    buy: f64,
    sell: f64,
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

#[path = "bars_tests.rs"]
#[cfg(test)]
mod bars_tests;
