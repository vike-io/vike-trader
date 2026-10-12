//! The recording exchange: the `Broker` + `HftBroker` double `trace` drives every strategy against.

use std::collections::BTreeMap;

use vike_model::{Bar, Broker, Fill, HftBroker};

/// One resting limit order, in submission order (a `Vec`, never a map, so a replay cannot reorder
/// fills).
#[derive(Debug, Clone)]
struct Resting {
    /// `Some` for the TAGGED (`HftBroker`) lane; `None` for the portable `Broker` lane (no cancel).
    tag: Option<String>,
    symbol: String,
    side: i32,
    qty: f64,
    price: f64,
}

/// A `Broker` + `HftBroker` double that RECORDS every call and matches resting limits against the
/// price the driver just published: a real rest → fill → react lifecycle without the simulator.
///
/// A PROBE, not a fill model (a marketable limit fills whole, at its own price): both traces come
/// from the SAME rules, so any deterministic matcher is sound. ⚠ It must never be lossy: a key
/// that changes only a cancel or a modify is exactly what a submissions-only log would call inert.
#[derive(Default)]
pub(super) struct TraceBroker {
    pub(super) px: f64,
    pub(super) now: i64,
    /// Signed position per symbol (the portable `Broker::position`).
    pos: BTreeMap<String, f64>,
    /// The single signed position the TAGGED lane reports (`HftBroker::position`).
    net: f64,
    /// Bars delivered so far, per symbol: `FundingCarryController::evaluate` reads its funding book
    /// off `Broker::bars(symbol).last()`, so no bars would make every carry knob inert.
    pub(super) bars: BTreeMap<String, Vec<Bar>>,
    /// Every call, in order. THIS is the observable the whole file compares.
    pub(super) log: Vec<String>,
    resting: Vec<Resting>,
    /// Fills produced but not yet folded into the strategy (market orders fill on submission).
    pending: Vec<Fill>,
}

impl TraceBroker {
    fn rest(&mut self, tag: Option<&str>, symbol: &str, side: i32, qty: f64, price: f64) {
        self.resting.push(Resting {
            tag: tag.map(str::to_string),
            symbol: symbol.to_string(),
            side,
            qty,
            price,
        });
    }

    fn fill_now(&mut self, symbol: &str, side: i32, qty: f64, price: f64, is_maker: bool) {
        self.pending.push(Fill {
            side,
            size: qty,
            price,
            fee: 0.0,
            ts: self.now,
            is_maker,
            symbol: symbol.to_string(),
        });
    }

    /// Every resting order the current price has crossed, removed and turned into a fill: a BUY
    /// fills once the market trades at or below its limit, a SELL at or above.
    pub(super) fn match_resting(&mut self) {
        let px = self.px;
        let mut i = 0;
        while i < self.resting.len() {
            let r = self.resting[i].clone();
            let crossed = (r.side > 0 && px <= r.price) || (r.side < 0 && px >= r.price);
            if crossed {
                self.resting.remove(i);
                self.fill_now(&r.symbol, r.side, r.qty, r.price, true);
            } else {
                i += 1;
            }
        }
    }

    /// Drain the fills produced so far, applying each to the position books FIRST:
    /// `TrailingScalper` reads `position()` inside its own `on_fill` and must see this fill.
    pub(super) fn take_fills(&mut self) -> Vec<Fill> {
        let fills = std::mem::take(&mut self.pending);
        for f in &fills {
            *self.pos.entry(f.symbol.clone()).or_insert(0.0) += f.side as f64 * f.size;
            self.net += f.side as f64 * f.size;
        }
        fills
    }
}

impl Broker for TraceBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.log.push(format!("market {symbol} {side} {qty}"));
        let px = self.px;
        self.fill_now(symbol, side, qty, px, false);
    }
    fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, price: f64) {
        self.log.push(format!("limit {symbol} {side} {qty} {price}"));
        self.rest(None, symbol, side, qty, price);
    }
    fn position(&self, symbol: &str) -> f64 {
        self.pos.get(symbol).copied().unwrap_or(0.0)
    }
    fn price(&self, _symbol: &str) -> f64 {
        self.px
    }
    fn equity(&self) -> f64 {
        1_000_000.0
    }
    fn bars(&self, symbol: &str) -> &[Bar] {
        self.bars.get(symbol).map(Vec::as_slice).unwrap_or(&[])
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        self.now
    }
}

impl HftBroker for TraceBroker {
    fn position(&self) -> f64 {
        self.net
    }
    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        self.log.push(format!("tagged {tag} {side} {qty} {price}"));
        self.rest(Some(tag), "", side, qty, price);
    }
    fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        self.log.push(format!("modify {tag} {new_qty:?} {new_price:?}"));
        for r in self.resting.iter_mut().filter(|r| r.tag.as_deref() == Some(tag)) {
            if let Some(q) = new_qty {
                r.qty = q;
            }
            if let Some(p) = new_price {
                r.price = p;
            }
        }
    }
    fn cancel_tagged(&mut self, tag: &str) {
        self.log.push(format!("cancel {tag}"));
        self.resting.retain(|r| r.tag.as_deref() != Some(tag));
    }
}
