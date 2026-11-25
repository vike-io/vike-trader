// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-4emarsi-session-window-nbar-exit-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat inside the 10:00 to 22:00 UTC window, long on EMA(5) crossing up through EMA(20) and short on crossing down. Every
//! trade closes exactly 8 bars after it fills, whatever else happens, and only that expiry closes one.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const DAY_MS: i64 = 86_400_000;
const MINUTE_MS: i64 = 60_000;
const WINDOW_MINUTES: std::ops::Range<i64> = 10 * 60..22 * 60;
const EXPIRY_BARS: i64 = 8;

struct S {
    fast: Ema,
    slow: Ema,
    cross: Cross,
    /// The position this bar opened with: an order sent on a bar fills at the next bar's open.
    in_trade: bool,
    bars_in_trade: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        fast: Ema::new(5.0).expect("a valid period"),
        slow: Ema::new(20.0).expect("a valid period"),
        cross: Cross::new(),
        in_trade: false,
        bars_in_trade: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let crossed = self.cross.step(self.fast.next(bar), self.slow.next(bar));
        if self.in_trade {
            self.bars_in_trade += 1;
            if self.bars_in_trade < EXPIRY_BARS {
                return Signal::Hold;
            }
            self.in_trade = false;
            self.bars_in_trade = 0;
            return Signal::Close;
        }
        if !WINDOW_MINUTES.contains(&(bar.timestamp.rem_euclid(DAY_MS) / MINUTE_MS)) {
            return Signal::Hold;
        }
        self.in_trade = matches!(crossed, Signal::Long | Signal::Short);
        crossed
    }
}
