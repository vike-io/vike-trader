//! Price-transform indicators — faithful port of vike-trader-app `core/indicators/price.py`. Each is
//! a per-bar arithmetic combination of OHLC with NO warm-up (every bar is defined) and no params.
//!
//! The arithmetic is written once, in `crate::fast_o` (`avgprice`, `medprice`, `typprice`,
//! `wclprice`), and every way in runs it: `next(&Bar)` and `Indicator::on_bar`, the plain-number
//! entries (`next_ohlc`, `next_hl`, `next_hlc`), `vectorize`, and the slice calls in `crate::series`.
//! They agree bit for bit, and with the formulas as they were first written.

use super::macros::typed_new;
use crate::Indicator;
use crate::fast_o;
use vike_marketdata::Bar;

/// The impl items every type here shares: `Default`, `next(&Bar)`, the `Indicator` impl and the
/// registry test. `$step` maps a `&Bar` to the value.
macro_rules! price_type {
    ($Name:ident, $key:literal, |$b:ident| $step:expr) => {
        impl Default for $Name {
            fn default() -> Self {
                Self { last: f64::NAN }
            }
        }
        impl $Name {
            /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
            #[inline]
            pub fn next(&mut self, bar: &Bar) -> f64 {
                let $b = bar;
                self.last = $step;
                self.last
            }
        }
        typed_new!($Name, $key, []);
        impl Indicator for $Name {
            fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
                vec![self.next(bar)]
            }
            fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
                fast_o::price_bars(bars, |$b: &Bar| $step)
            }
            fn value(&self) -> Vec<f64> {
                vec![self.last]
            }
            fn reset(&mut self) {
                self.last = f64::NAN;
            }
            fn name(&self) -> &str {
                $key
            }
            fn lookback(&self) -> usize {
                0
            }
            fn lookback_exact(&self) -> bool {
                true
            }
        }
    };
}

/// Average price of the bar, `(O + H + L + C) / 4` (registry key `avgprice`, one line, no
/// parameters). Built with `Default`.
#[derive(Clone)]
pub struct Avgprice {
    last: f64,
}
impl Avgprice {
    /// Feed the next bar's `open`, `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the
    /// same answer as [`Avgprice::next`] for a bar with those values.
    #[inline]
    pub fn next_ohlc(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
        self.last = fast_o::avgprice(open, high, low, close);
        self.last
    }
}
price_type!(Avgprice, "avgprice", |b| fast_o::avgprice(b.open, b.high, b.low, b.close));

/// Median price of the bar, `(H + L) / 2` (registry key `medprice`, one line, no parameters). Built
/// with `Default`.
#[derive(Clone)]
pub struct Medprice {
    last: f64,
}
impl Medprice {
    /// Feed the next bar's `high` and `low` as plain numbers, no [`Bar`] needed; the same answer as
    /// [`Medprice::next`] for a bar with those values.
    #[inline]
    pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
        self.last = fast_o::medprice(high, low);
        self.last
    }
}
price_type!(Medprice, "medprice", |b| fast_o::medprice(b.high, b.low));

/// Typical price of the bar, `(H + L + C) / 3` (registry key `typprice`, one line, no parameters).
/// Built with `Default`.
#[derive(Clone)]
pub struct Typprice {
    last: f64,
}
impl Typprice {
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Typprice::next`] for a bar with those values.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = fast_o::typprice(high, low, close);
        self.last
    }
}
price_type!(Typprice, "typprice", |b| fast_o::typprice(b.high, b.low, b.close));

/// Weighted close of the bar, `(H + L + 2C) / 4` (registry key `wclprice`, one line, no parameters).
/// Built with `Default`.
#[derive(Clone)]
pub struct Wclprice {
    last: f64,
}
impl Wclprice {
    /// Feed the next bar's `high`, `low` and `close` as plain numbers, no [`Bar`] needed; the same
    /// answer as [`Wclprice::next`] for a bar with those values.
    #[inline]
    pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
        self.last = fast_o::wclprice(high, low, close);
        self.last
    }
}
price_type!(Wclprice, "wclprice", |b| fast_o::wclprice(b.high, b.low, b.close));
