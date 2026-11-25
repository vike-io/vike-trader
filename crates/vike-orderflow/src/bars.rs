//! Tick-, volume- and dollar-interval bar aggregation with a buy/sell/delta split, plus the opt-in
//! López de Prado information-driven bars ([`ImbalanceBarBuilder`], [`RunsBarBuilder`], AFML
//! §2.3.2). All reuse the one [`vike_marketdata::aggressor::signed`] buy/sell semantic, and the
//! volume and dollar bars are ONE fold (`threshold_fold`) that differs only in what fills a bar.
//! Each builder is constructed explicitly by its caller: one that builds none leaves the others
//! byte-identical.
//!
//! ⚠ **This crate prices no trade.** A trade's money value depends on its contract (an inverse
//! contract's size is already quote money, a linear one's is base units times a face), and all
//! contract money goes through `vike_model::ContractValue` (owner, 2026-11-09), which this crate does
//! not name. So a dollar-sampled bar takes each trade's NOTIONAL from its caller
//! (`ContractValue::order_notional(size, price)`), and never multiplies a price by a size itself.
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
/// `notional` (the running money value of what the bar took, from each trade's notional as its
/// CALLER priced it) is kept only by [`Fill::Value`]; it stays 0 in the other builders.
struct BarAccumulator {
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    buy: f64,
    sell: f64,
    notional: f64,
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
            notional: 0.0,
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

/// What fills a threshold bar: its traded volume ([`VolumeBarBuilder`]) or its traded value
/// ([`DollarBarBuilder`]), which carries the trade's notional as its caller priced it.
#[derive(Clone, Copy)]
enum Fill {
    Volume,
    Value(f64),
}

impl Fill {
    fn filled(self, accumulator: &BarAccumulator) -> f64 {
        match self {
            Fill::Volume => accumulator.buy + accumulator.sell,
            Fill::Value(_) => accumulator.notional,
        }
    }
}

/// Fold one (size>0) trade into `accumulator`, closing bars whenever what fills them (`fill`)
/// reaches `threshold`; push closed bars to `out`. The boundary trade is SPLIT: the slice that fills
/// the room closes the bar, the rest opens the next.
fn threshold_fold(
    accumulator: &mut BarAccumulator,
    out: &mut Vec<OrderflowBar>,
    t: &TickTrade,
    threshold: f64,
    fill: Fill,
) {
    // A trade's notional is proportional to its size on a linear and an inverse contract alike, so a
    // slice of it is worth its share. A notional that is not a positive number adds no value.
    let value_per_size = match fill {
        Fill::Value(notional) if notional > 0.0 && notional.is_finite() => notional / t.size,
        _ => 0.0,
    };
    let mut remaining = t.size;
    while remaining > 0.0 {
        let room = threshold - fill.filled(accumulator);
        // The slice this bar takes, and whether the trade's value FILLS the room (that slice then
        // closes the bar whatever the rounding of its value; the volume bars decide by their sum).
        let (take, fills) = match fill {
            Fill::Volume => (remaining.min(room), false),
            Fill::Value(_) => {
                let fills = value_per_size > 0.0 && remaining * value_per_size >= room;
                let take = if fills { (room / value_per_size).min(remaining) } else { remaining };
                // A slice too small to move `remaining` (a threshold below the size's resolution)
                // takes the rest, so the fold cannot loop. A trade worth nothing never fills: it
                // adds volume but no value.
                (if remaining - take == remaining { remaining } else { take }, fills)
            }
        };
        // a synthetic sub-trade of `take` at the trade's price/side/ts:
        let sub = TickTrade {
            timestamp: t.timestamp,
            local_timestamp: t.local_timestamp,
            price: t.price,
            size: take,
            is_buyer_maker: t.is_buyer_maker,
            symbol: Default::default(),
        };
        accumulator.add(&sub);
        if !fills && matches!(fill, Fill::Value(_)) {
            accumulator.notional += take * value_per_size;
        }
        remaining -= take;
        if fills || fill.filled(accumulator) >= threshold {
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
            threshold_fold(&mut self.accumulator, &mut out, t, self.threshold, Fill::Volume);
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
            threshold_fold(&mut accumulator, &mut out, t, threshold, Fill::Volume);
        }
        out
    }
}

/// Dollar bars: close when the bar's traded value reaches `threshold`. Each trade's value is its
/// NOTIONAL as the caller priced it, `vike_model::ContractValue::order_notional(t.size, t.price)`
/// for the contract the tape is of (an inverse contract's size is already quote money, so its value
/// is not `price * size`); this crate prices nothing. [`VolumeBarBuilder`]'s fold with the value as
/// what fills a bar: the boundary trade is split by the share of its size that fills the remaining
/// value, its filling slice closing the bar. A notional that is not a positive number adds volume
/// but no value.
pub struct DollarBarBuilder {
    threshold: f64,
    accumulator: BarAccumulator,
}

impl DollarBarBuilder {
    pub fn new(threshold: f64) -> Self {
        assert!(threshold > 0.0, "DollarBarBuilder threshold must be > 0");
        DollarBarBuilder { threshold, accumulator: BarAccumulator::new() }
    }

    /// Fold one trade worth `notional`; the bars it closed.
    pub fn push(&mut self, t: &TickTrade, notional: f64) -> Vec<OrderflowBar> {
        let mut out = Vec::new();
        if t.size > 0.0 {
            let fill = Fill::Value(notional);
            threshold_fold(&mut self.accumulator, &mut out, t, self.threshold, fill);
        }
        out
    }

    /// Snapshot of the OPEN (unclosed) bar. Pure read.
    pub fn forming(&self) -> Option<OrderflowBar> {
        self.accumulator.open.then(|| self.accumulator.finish())
    }

    /// The batch twin of [`DollarBarBuilder::push`]: `notional` prices each trade.
    pub fn from_trades(
        trades: &[TickTrade],
        threshold: f64,
        notional: impl Fn(&TickTrade) -> f64,
    ) -> Vec<OrderflowBar> {
        assert!(threshold > 0.0);
        let mut out = Vec::new();
        let mut accumulator = BarAccumulator::new();
        for t in trades.iter().filter(|t| t.size > 0.0) {
            threshold_fold(&mut accumulator, &mut out, t, threshold, Fill::Value(notional(t)));
        }
        out
    }
}

mod info_driven;
pub use info_driven::{ImbalanceBarBuilder, RunsBarBuilder};

#[path = "bars_tests.rs"]
#[cfg(test)]
mod bars_tests;
