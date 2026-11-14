// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-engulfing-candle-pattern-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on a bullish candle (engulfing, hammer or morning star) while the close is below SMA(20), short on a bearish one
//! (engulfing or shooting star) while the close is above it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

/// A wick and a multiple of a body that are equal in decimal are equal: subtracting two-decimal prices leaves about 1e-12 of
/// float noise, which is not a difference.
const TIE: f64 = 1e-9;

fn below(a: f64, b: f64) -> bool {
    a < b - TIE
}

fn above(a: f64, b: f64) -> bool {
    a > b + TIE
}

struct Candle {
    open: f64,
    close: f64,
}

impl Candle {
    fn body(&self) -> f64 {
        (self.close - self.open).abs()
    }
    fn is_up(&self) -> bool {
        self.close > self.open
    }
    fn is_down(&self) -> bool {
        self.close < self.open
    }
}

struct S {
    trend: IndicatorLine,
    // NaN until two bars have passed, and a NaN candle matches no pattern.
    one_back: Candle,
    two_back: Candle,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let unseen = || Candle { open: f64::NAN, close: f64::NAN };
    Box::new(SignalRule(S {
        trend: IndicatorLine::new("sma", &[20.0]),
        one_back: unseen(),
        two_back: unseen(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let trend = self.trend.next(bar)[0];
        let now = Candle { open: bar.open, close: bar.close };
        let previous = &self.one_back;
        let older = &self.two_back;

        let body = now.body();
        let upper_wick = bar.high - bar.open.max(bar.close);
        let lower_wick = bar.open.min(bar.close) - bar.low;

        let engulfs_up = now.is_up()
            && previous.is_down()
            && now.close > previous.open
            && now.open < previous.close;
        let engulfs_down = now.is_down()
            && previous.is_up()
            && now.close < previous.open
            && now.open > previous.close;
        let hammer = above(lower_wick, body * 2.0) && below(upper_wick, body * 0.5) && body > 0.0;
        let shooting_star =
            above(upper_wick, body * 2.0) && below(lower_wick, body * 0.5) && body > 0.0;
        let morning_star = older.is_down()
            && below(previous.body(), older.body() * 0.3)
            && now.is_up()
            && above(now.close, (older.open + older.close) / 2.0);

        let bullish = (engulfs_up || hammer || morning_star) && bar.close < trend;
        let bearish = (engulfs_down || shooting_star) && bar.close > trend;

        self.two_back = std::mem::replace(&mut self.one_back, now);

        if bullish {
            Signal::Long
        } else if bearish {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
