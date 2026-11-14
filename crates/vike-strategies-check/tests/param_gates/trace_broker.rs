//! The recording exchange: the `OrderBroker` double `trace` drives every strategy against.

use std::collections::BTreeMap;

use vike_marketdata::Bar;
use vike_model::{Fill, OrderBroker, OrderListLeg, OrderPlacement};

/// One resting limit order, in submission order (a `Vec`, never a map, so a replay cannot reorder
/// fills).
#[derive(Debug, Clone)]
struct Resting {
    /// `Some` for the TAGGED lane; `None` for the untagged lane (no cancel).
    tag: Option<String>,
    symbol: String,
    side: i32,
    qty: f64,
    price: f64,
}

/// An `OrderBroker` double that RECORDS every call and matches resting limits against the
/// price the driver just published: a real rest → fill → react lifecycle without the simulator.
///
/// A PROBE, not a fill model (a marketable limit fills whole, at its own price): both traces come
/// from the SAME rules, so any deterministic matcher is sound. ⚠ It must never be lossy: a key
/// that changes only a cancel or a modify is exactly what a submissions-only log would call inert.
#[derive(Default)]
pub(super) struct TraceBroker {
    pub(super) price: f64,
    pub(super) now: i64,
    /// Signed position per symbol (the portable `OrderBroker::position`).
    pos: BTreeMap<String, f64>,
    /// Bars delivered so far, per symbol: `FundingCarryController::evaluate` reads its funding book
    /// off `OrderBroker::bars(symbol).last()`, so no bars would make every carry knob inert.
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
            timestamp: self.now,
            is_maker,
            symbol: symbol.to_string(),
            tag: None,
        });
    }

    /// Every resting order the current price has crossed, removed and turned into a fill: a BUY
    /// fills once the market trades at or below its limit, a SELL at or above.
    pub(super) fn match_resting(&mut self) {
        let px = self.price;
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
        }
        fills
    }
}

/// The seam: an untagged market or limit is the untagged lane, logged and matched (both real
/// brokers route it there); a tagged plain limit, an amend with no trigger and a cancel are the tagged
/// lane, as both real brokers route a maker's quote (decision 0277's step 6). No strategy `trace` drives places anything else through
/// the seam yet, so any other order fails the trace loudly rather than being logged as something it
/// is not.
impl OrderBroker for TraceBroker {
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    ) {
        match order {
            OrderPlacement::Market if tag.is_empty() && !reduce_only => {
                self.log.push(format!("market {symbol} {side} {qty}"));
                let px = self.price;
                self.fill_now(symbol, side, qty, px, false);
            }
            OrderPlacement::Limit { price } if tag.is_empty() && !reduce_only => {
                self.log.push(format!("limit {symbol} {side} {qty} {price}"));
                self.rest(None, symbol, side, qty, price);
            }
            OrderPlacement::Limit { price } if symbol.is_empty() && !reduce_only => {
                self.log.push(format!("tagged {tag} {side} {qty} {price}"));
                self.rest(Some(tag), "", side, qty, price);
            }
            other => panic!("TraceBroker has no lane for {other:?} under {tag:?}"),
        }
    }
    fn place_group(&mut self, _symbol: &str, legs: Vec<(String, OrderListLeg)>) {
        panic!("TraceBroker has no order-list lane ({} legs)", legs.len());
    }
    fn amend(&mut self, tag: &str, qty: Option<f64>, price: Option<f64>, trigger: Option<f64>) {
        assert!(trigger.is_none(), "TraceBroker has no trigger amend ({tag})");
        self.log.push(format!("modify {tag} {qty:?} {price:?}"));
        for r in self.resting.iter_mut().filter(|r| r.tag.as_deref() == Some(tag)) {
            if let Some(q) = qty {
                r.qty = q;
            }
            if let Some(p) = price {
                r.price = p;
            }
        }
    }
    fn cancel(&mut self, tag: &str) {
        self.log.push(format!("cancel {tag}"));
        self.resting.retain(|r| r.tag.as_deref() != Some(tag));
    }
    fn deny(&mut self, _symbol: &str, tag: &str, _quantity: f64, reason: &str) {
        panic!("TraceBroker has no seam denial ({tag}: {reason})");
    }
    fn average_price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn position(&self, symbol: &str) -> f64 {
        self.pos.get(symbol).copied().unwrap_or(0.0)
    }
    fn price(&self, _symbol: &str) -> f64 {
        self.price
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
