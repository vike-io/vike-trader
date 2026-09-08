//! Price-transform indicators — faithful port of vike-trader-app
//! `core/indicators/price.py`. Each is a per-bar arithmetic combination of OHLC
//! with NO warm-up (every bar is defined) and no params. All stream O(1) and are
//! trivially equal to their batch kernel.
#![allow(clippy::needless_range_loop)]

use crate::Indicator;
use crate::math::Columns;
use vike_model::Bar;

fn batch_avgprice(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    vec![(0..x.c.len()).map(|i| (x.o[i] + x.h[i] + x.l[i] + x.c[i]) / 4.0).collect()]
}

fn batch_medprice(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    vec![(0..x.c.len()).map(|i| (x.h[i] + x.l[i]) / 2.0).collect()]
}

fn batch_typprice(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    vec![(0..x.c.len()).map(|i| (x.h[i] + x.l[i] + x.c[i]) / 3.0).collect()]
}

fn batch_wclprice(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    vec![(0..x.c.len()).map(|i| (x.h[i] + x.l[i] + 2.0 * x.c[i]) / 4.0).collect()]
}

/// `avgprice` — `(O + H + L + C) / 4`.
#[derive(Clone, Default)]
pub struct Avgprice {
    last: Option<f64>,
}
impl Avgprice {
    pub fn new() -> Self {
        Self { last: None }
    }
}
impl Indicator for Avgprice {
    fn on_bar(&mut self, b: &Bar) -> Vec<f64> {
        let v = (b.open + b.high + b.low + b.close) / 4.0;
        self.last = Some(v);
        vec![v]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_avgprice(bars)
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last.unwrap_or(f64::NAN)]
    }
    fn reset(&mut self) {
        self.last = None;
    }
    fn name(&self) -> &str {
        "avgprice"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// `medprice` — `(H + L) / 2`.
#[derive(Clone, Default)]
pub struct Medprice {
    last: Option<f64>,
}
impl Medprice {
    pub fn new() -> Self {
        Self { last: None }
    }
}
impl Indicator for Medprice {
    fn on_bar(&mut self, b: &Bar) -> Vec<f64> {
        let v = (b.high + b.low) / 2.0;
        self.last = Some(v);
        vec![v]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_medprice(bars)
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last.unwrap_or(f64::NAN)]
    }
    fn reset(&mut self) {
        self.last = None;
    }
    fn name(&self) -> &str {
        "medprice"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// `typprice` — typical price `(H + L + C) / 3`.
#[derive(Clone, Default)]
pub struct Typprice {
    last: Option<f64>,
}
impl Typprice {
    pub fn new() -> Self {
        Self { last: None }
    }
}
impl Indicator for Typprice {
    fn on_bar(&mut self, b: &Bar) -> Vec<f64> {
        let v = (b.high + b.low + b.close) / 3.0;
        self.last = Some(v);
        vec![v]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_typprice(bars)
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last.unwrap_or(f64::NAN)]
    }
    fn reset(&mut self) {
        self.last = None;
    }
    fn name(&self) -> &str {
        "typprice"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// `wclprice` — weighted close `(H + L + 2C) / 4`.
#[derive(Clone, Default)]
pub struct Wclprice {
    last: Option<f64>,
}
impl Wclprice {
    pub fn new() -> Self {
        Self { last: None }
    }
}
impl Indicator for Wclprice {
    fn on_bar(&mut self, b: &Bar) -> Vec<f64> {
        let v = (b.high + b.low + 2.0 * b.close) / 4.0;
        self.last = Some(v);
        vec![v]
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_wclprice(bars)
    }
    fn value(&self) -> Vec<f64> {
        vec![self.last.unwrap_or(f64::NAN)]
    }
    fn reset(&mut self) {
        self.last = None;
    }
    fn name(&self) -> &str {
        "wclprice"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}
