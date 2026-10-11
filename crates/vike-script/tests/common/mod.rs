//! The ONE `MockBroker` and `drive` this crate's integration binaries share, and (as `test_support`)
//! the fixtures `src/test_support.rs` owns. Each declares it with `mod common;`: a `tests/` SUBDIRECTORY module is not a test target, so this
//! file builds no binary and runs no test of its own. `drive` pushes each close as a `bar`, then
//! feeds it to the strategy's `on_bar`.
//!
//! `MockBroker` records each market order as `(side, qty)`, which is why it is not
//! `vike_model::strategy::MockBroker` (that one records `(symbol, side, qty)`). `price()` is the
//! last pushed bar's close and `index()` that bar's index; `position()`, `equity()` and `now()` are
//! constants. The seven copies it replaced differed only where no assertion looks: three answered
//! `price()` with `0.0` (no script in them calls `price()`), one kept a `pos` field nothing wrote
//! (so `position()` was `0.0` there too), and two declared the fields in the other order.
#![allow(dead_code)] // each test binary compiles this file and uses a subset of it

use vike_model::{Bar, Broker, Strategy};
use vike_script::{RhaiIndicator, RhaiStrategy};

#[path = "../../src/test_support.rs"]
pub mod test_support;
use test_support::bar;

#[derive(Default)]
pub struct MockBroker {
    pub bars: Vec<Bar>,
    pub submits: Vec<(i32, f64)>,
}

impl Broker for MockBroker {
    fn submit_market(&mut self, _s: &str, side: i32, qty: f64) {
        self.submits.push((side, qty));
    }
    fn submit_limit(&mut self, _s: &str, _side: i32, _qty: f64, _p: f64) {}
    fn position(&self, _s: &str) -> f64 {
        0.0
    }
    fn price(&self, _s: &str) -> f64 {
        self.bars.last().map(|b| b.close).unwrap_or(0.0)
    }
    fn equity(&self) -> f64 {
        10_000.0
    }
    fn bars(&self, _s: &str) -> &[Bar] {
        &self.bars
    }
    fn index(&self) -> usize {
        self.bars.len().saturating_sub(1)
    }
    fn now(&self) -> i64 {
        0
    }
}

pub fn drive(strat: &mut RhaiStrategy<MockBroker>, broker: &mut MockBroker, closes: &[f64]) {
    for &c in closes {
        broker.bars.push(bar(c));
        let b = broker.bars.last().unwrap().clone();
        strat.on_bar(broker, &b);
    }
}

/// Compiles `src` with `inds` bound beside the built-ins, drives it over `closes` and hands back the
/// broker, so a test asserts on `submits` without repeating the compile-and-drive scaffold.
pub fn run_with_indicators(src: &str, inds: &[RhaiIndicator], closes: &[f64]) -> MockBroker {
    let mut s =
        RhaiStrategy::compile_with_indicators(src, Default::default(), inds).expect("compiles");
    let mut b = MockBroker::default();
    drive(&mut s, &mut b, closes);
    b
}
