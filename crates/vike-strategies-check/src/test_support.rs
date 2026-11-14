//! The broker doubles the shipped taker files' OWN tests name as `crate::test_support` — copies of
//! the `OrderBrokerRecording` vike-strategy's own unit tests shared (deleted with the library's
//! strategies) and of the two `OrderBrokerMock` readers in
//! `crates/vike-strategy/src/position_executor/machine_tests.rs`, so
//! the tests copied into each file read exactly as they did beside the library's twin.
//!
//! [`OrderBrokerRecording`] RECORDS every order verb and answers every read from a settable field.
//! Not a fill model: an untagged market order folds into [`OrderBrokerRecording::positions`] as an
//! instant full fill, and nothing else moves a position, so a test sets [`OrderBrokerRecording::pos`]
//! to stand for "already holding". The library's copy also implements the tagged-quote seam the
//! files do not name; this one implements only the order seam (`OrderBroker`) and the read side it
//! extends.

use std::collections::HashMap;

use vike_marketdata::Bar;
use vike_model::strategy::OrderBrokerMock;
use vike_model::{OrderBroker, OrderListLeg, OrderPlacement};

/// Records every order verb; every read answers from a field (zero / empty by default).
#[derive(Default)]
pub(crate) struct OrderBrokerRecording {
    /// Every UNTAGGED market or limit order ([`OrderBroker::place`] under the tag `""`), as
    /// `(symbol, side, qty)`.
    pub(crate) orders: Vec<(String, i32, f64)>,
    /// Every other [`OrderBroker::place`], as `(symbol, tag, side, qty, order, reduce_only)`.
    pub(crate) placed: Vec<(String, String, i32, f64, OrderPlacement, bool)>,
    /// Every [`OrderBroker::amend`], as `(tag, new_qty, new_price)`.
    pub(crate) modified: Vec<(String, Option<f64>, Option<f64>)>,
    /// Every [`OrderBroker::cancel`], by tag.
    pub(crate) cancelled: Vec<String>,
    /// Net position per symbol, folded from untagged market orders alone (a resting limit is not a
    /// fill).
    pub(crate) positions: HashMap<String, f64>,
    /// The position of a symbol `positions` does not hold.
    pub(crate) pos: f64,
    /// [`OrderBroker::price`] of every symbol.
    pub(crate) price: f64,
    /// [`OrderBroker::bars`] of every symbol.
    pub(crate) bars: Vec<Bar>,
    /// [`OrderBroker::instrument_grid`] of every symbol.
    pub(crate) grid: Option<vike_model::Grid>,
    /// [`OrderBroker::now`], engine milliseconds.
    pub(crate) now: i64,
    /// [`OrderBroker::contract_value`] of every symbol.
    pub(crate) value: Option<vike_model::ContractValue>,
}

/// The seam: an untagged market or limit takes the untagged lane above (recorded in `orders`, a
/// market folded into `positions`), as both real brokers route it; everything else is `placed`.
impl OrderBroker for OrderBrokerRecording {
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
                self.orders.push((symbol.to_string(), side, qty));
                *self.positions.entry(symbol.to_string()).or_insert(0.0) += f64::from(side) * qty;
            }
            OrderPlacement::Limit { .. } if tag.is_empty() && !reduce_only => {
                self.orders.push((symbol.to_string(), side, qty));
            }
            _ => self.placed.push((
                symbol.to_string(),
                tag.to_string(),
                side,
                qty,
                order,
                reduce_only,
            )),
        }
    }
    fn place_group(&mut self, _symbol: &str, _legs: Vec<(String, OrderListLeg)>) {}
    fn amend(&mut self, tag: &str, qty: Option<f64>, price: Option<f64>, _trigger: Option<f64>) {
        self.modified.push((tag.to_string(), qty, price));
    }
    fn cancel(&mut self, tag: &str) {
        self.cancelled.push(tag.to_string());
    }
    fn deny(&mut self, _symbol: &str, _tag: &str, _quantity: f64, _reason: &str) {}
    fn average_price(&self, _symbol: &str) -> f64 {
        0.0
    }
    fn position(&self, symbol: &str) -> f64 {
        self.positions.get(symbol).copied().unwrap_or(self.pos)
    }
    fn price(&self, _symbol: &str) -> f64 {
        self.price
    }
    fn equity(&self) -> f64 {
        0.0
    }
    fn bars(&self, _symbol: &str) -> &[Bar] {
        &self.bars
    }
    fn index(&self) -> usize {
        0
    }
    fn now(&self) -> i64 {
        self.now
    }
    fn instrument_grid(&self, _symbol: &str) -> Option<vike_model::Grid> {
        self.grid
    }
    fn contract_value(&self, _symbol: &str) -> Option<vike_model::ContractValue> {
        self.value
    }
}

/// The UNTAGGED market orders `OrderBrokerMock` heard through the seam, as `(symbol, side, qty)`.
pub(crate) fn untagged_markets(b: &OrderBrokerMock) -> Vec<(String, i32, f64)> {
    let rows = b.placed.iter().filter(|(_, tag, _, _, order, reduce_only)| {
        tag.is_empty() && *order == OrderPlacement::Market && !reduce_only
    });
    rows.map(|(symbol, _, side, qty, ..)| (symbol.clone(), *side, *qty)).collect()
}

/// The UNTAGGED limits `OrderBrokerMock` heard through the seam, as `(symbol, side, qty, price)`.
pub(crate) fn untagged_limits(b: &OrderBrokerMock) -> Vec<(String, i32, f64, f64)> {
    let rows = b.placed.iter().filter(|(_, tag, ..)| tag.is_empty());
    rows.filter_map(|(symbol, _, side, qty, order, reduce_only)| match order {
        OrderPlacement::Limit { price } if !reduce_only => {
            Some((symbol.clone(), *side, *qty, *price))
        }
        _ => None,
    })
    .collect()
}
