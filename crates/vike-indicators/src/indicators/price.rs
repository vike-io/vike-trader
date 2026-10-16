//! Price-transform indicators — faithful port of vike-trader-app
//! `core/indicators/price.py`. Each is a per-bar arithmetic combination of OHLC
//! with NO warm-up (every bar is defined) and no params. All stream O(1) and are
//! trivially equal to their batch kernel.
#![allow(clippy::needless_range_loop)]

use crate::Indicator;
use crate::math::{Columns, Need};
use vike_marketdata::Bar;

fn batch_avgprice(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::OPEN | Need::HIGH | Need::LOW | Need::CLOSE);
    vec![(0..x.c.len()).map(|i| (x.o[i] + x.h[i] + x.l[i] + x.c[i]) / 4.0).collect()]
}

fn batch_medprice(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::HIGH | Need::LOW | Need::CLOSE);
    vec![(0..x.c.len()).map(|i| (x.h[i] + x.l[i]) / 2.0).collect()]
}

fn batch_typprice(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::HIGH | Need::LOW | Need::CLOSE);
    vec![(0..x.c.len()).map(|i| (x.h[i] + x.l[i] + x.c[i]) / 3.0).collect()]
}

fn batch_wclprice(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::needing(bars, Need::HIGH | Need::LOW | Need::CLOSE);
    vec![(0..x.c.len()).map(|i| (x.h[i] + x.l[i] + 2.0 * x.c[i]) / 4.0).collect()]
}

/// Average price of the bar, `(O + H + L + C) / 4` (registry key `avgprice`, one line, no
/// parameters). Built with `Default`.
#[derive(Clone, Default)]
pub struct Avgprice {
    last: Option<f64>,
}
impl Avgprice {
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    pub fn next(&mut self, bar: &Bar) -> f64 {
        let v = Indicator::on_bar(self, bar);
        v[0]
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

/// Median price of the bar, `(H + L) / 2` (registry key `medprice`, one line, no parameters). Built
/// with `Default`.
#[derive(Clone, Default)]
pub struct Medprice {
    last: Option<f64>,
}
impl Medprice {
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    pub fn next(&mut self, bar: &Bar) -> f64 {
        let v = Indicator::on_bar(self, bar);
        v[0]
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

/// Typical price of the bar, `(H + L + C) / 3` (registry key `typprice`, one line, no parameters).
/// Built with `Default`.
#[derive(Clone, Default)]
pub struct Typprice {
    last: Option<f64>,
}
impl Typprice {
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    pub fn next(&mut self, bar: &Bar) -> f64 {
        let v = Indicator::on_bar(self, bar);
        v[0]
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

/// Weighted close of the bar, `(H + L + 2C) / 4` (registry key `wclprice`, one line, no parameters).
/// Built with `Default`.
#[derive(Clone, Default)]
pub struct Wclprice {
    last: Option<f64>,
}
impl Wclprice {
    /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
    pub fn next(&mut self, bar: &Bar) -> f64 {
        let v = Indicator::on_bar(self, bar);
        v[0]
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

#[cfg(test)]
mod typed_tests {
    use super::*;
    use crate::typed::tests::assert_matches_registry;

    #[test]
    fn typed_avgprice_matches_registry() {
        assert_matches_registry("avgprice", Box::new(Avgprice::default()));
    }

    #[test]
    fn typed_medprice_matches_registry() {
        assert_matches_registry("medprice", Box::new(Medprice::default()));
    }

    #[test]
    fn typed_typprice_matches_registry() {
        assert_matches_registry("typprice", Box::new(Typprice::default()));
    }

    #[test]
    fn typed_wclprice_matches_registry() {
        assert_matches_registry("wclprice", Box::new(Wclprice::default()));
    }
}
