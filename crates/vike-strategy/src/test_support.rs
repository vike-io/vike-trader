//! The ONE hand-written broker double the crate's unit tests share: a [`Broker`] + [`HftBroker`]
//! that RECORDS every order verb and answers every read from a settable field.
//!
//! Not a fill model: a `submit_market` folds into [`RecordingBroker::positions`] as an instant full
//! fill, and nothing else moves a position, so a test sets [`RecordingBroker::pos`] to stand for
//! "already holding". It exists because `vike_model`'s `MockBroker` is `Broker`-only. The
//! integration tests' `TraceBroker` (real matching) is a different double and stays in `tests/`.

use std::collections::HashMap;

use vike_model::{Bar, Broker, HftBroker};

/// Records every order verb; every read answers from a field (zero / empty by default).
#[derive(Default)]
pub(crate) struct RecordingBroker {
    /// Every UNTAGGED `submit_market` / `submit_limit`, as `(symbol, side, qty)`.
    pub(crate) orders: Vec<(String, i32, f64)>,
    /// Every `submit_limit_tagged`, as `(tag, side, qty, price)`.
    pub(crate) tagged: Vec<(String, i32, f64, f64)>,
    /// Every `modify_tagged`, as `(tag, new_qty, new_price)`.
    pub(crate) modified: Vec<(String, Option<f64>, Option<f64>)>,
    /// Every `cancel_tagged`, by tag.
    pub(crate) cancelled: Vec<String>,
    /// Net position per symbol, folded from `submit_market` alone (a resting limit is not a fill).
    pub(crate) positions: HashMap<String, f64>,
    /// The position of a symbol `positions` does not hold, and the [`HftBroker::position`].
    pub(crate) pos: f64,
    /// [`Broker::price`] of every symbol.
    pub(crate) price: f64,
    /// [`Broker::bars`] of every symbol.
    pub(crate) bars: Vec<Bar>,
}

impl RecordingBroker {
    /// Every submission, tagged or not: "did the strategy route ANY order".
    pub(crate) fn submits(&self) -> usize {
        self.orders.len() + self.tagged.len()
    }
}

impl Broker for RecordingBroker {
    fn submit_market(&mut self, symbol: &str, side: i32, qty: f64) {
        self.orders.push((symbol.to_string(), side, qty));
        *self.positions.entry(symbol.to_string()).or_insert(0.0) += f64::from(side) * qty;
    }
    fn submit_limit(&mut self, symbol: &str, side: i32, qty: f64, _price: f64) {
        self.orders.push((symbol.to_string(), side, qty));
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
        0
    }
}

impl HftBroker for RecordingBroker {
    fn position(&self) -> f64 {
        self.pos
    }
    fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
        self.tagged.push((tag.to_string(), side, qty, price));
    }
    fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
        self.modified.push((tag.to_string(), new_qty, new_price));
    }
    fn cancel_tagged(&mut self, tag: &str) {
        self.cancelled.push(tag.to_string());
    }
}
