// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Ultimate Bullish Divergence for RSI MFI RVSI OBV TRIXRSI w/ Buy Signals" by imal_max, https://www.tradingview.com/script/d92hvFQx/
// Changed: rewritten by hand in Rust for vike.

//! A regular bullish divergence on RSI(14) of the LOW (the header's oscillator is RSI, with every delay, range and oversold
//! filter off, and hidden divergences off). For each of 30 left strengths `2, 3, 4, ... 54` the oscillator has a pivot low on a
//! bar when it is not above any of the bars before it within that strength (a pivot confirms with no bars to its right, on its
//! own bar). On such a bar that strength signals when the oscillator is above its value at that strength's previous pivot, no
//! more than 201 bars back, while the low is under the low there. Long all of the account's worth on a signal of any strength,
//! one long at a time.
//!
//! The exit is issued only while a signal is at most one bar old and the last open trade at most one bar old, which on a
//! market entry at the close is once, on the bar after the entry's: the target 2% over the position's average price and the
//! stop 0.35% under the lowest low of the last 5 bars. Orders fill at the close of the bar that placed them. 0.05% commission
//! and 2 ticks of slippage; starts with 1,000 in a USD account.
//!
//! The header's `request.earnings`, `dividends` and `splits` read nothing on a crypto pair and feed no rule.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_tactics::sizing::share_of_worth;
/// The length of the 30-minute bar the rule was written on.
const BAR_MS: i64 = 1_800_000;
use vike_indicators::indicators::{ConfirmedPivot, Donchian, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

/// The left strengths of the 30 look-back periods.
const LEFT: [usize; 30] = [
    2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 22, 25, 28, 31, 34, 37, 39,
    42, 45, 48, 51, 54,
];
/// The most bars between two pivots the rule accepts, `Max Lookback`.
const RANGE: usize = 200;
const TAKE: f64 = 0.02;
const BUFFER: f64 = 0.0035;
const NAME: &str = "Bull";

/// The oscillator and the low at a strength's last pivot, and the bar it was on.
#[derive(Clone, Copy)]
struct Pivot {
    osc: f64,
    low: f64,
    bar: usize,
}

struct S {
    rsi: Rsi,
    /// The oscillator's pivot lows at each left strength, confirmed on their own bar (`right` 0).
    pivots: [ConfirmedPivot; 30],
    last: [Option<Pivot>; 30],
    lows: Donchian,
    index: usize,
    /// Bars since a signal, `None` before the first.
    since_signal: Option<usize>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(14.0).expect("a valid length").with_rs_form(true),
        pivots: LEFT.map(|left| ConfirmedPivot::new(left as f64, 0.0).expect("a valid strength")),
        last: [None; 30],
        lows: Donchian::new(5.0).expect("a valid length"),
        index: 0,
        since_signal: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let osc = self.rsi.next_value(bar.low);
        let mut signal = false;
        for (i, pivots) in self.pivots.iter_mut().enumerate() {
            // The oscillator's missing values are its warm-up only, so a window holding one starts with one: none confirms.
            if pivots.next_hl(osc, osc)[1].is_nan() {
                continue;
            }
            if let Some(last) = self.last[i] {
                // `_inRange(plFound[1])`: bars since the previous pivot, not counting this bar.
                let in_range = self.index - last.bar - 1 <= RANGE;
                signal |= bar.low < last.low && osc > last.osc && in_range;
            }
            self.last[i] = Some(Pivot { osc, low: bar.low, bar: self.index });
        }
        self.since_signal = if signal { Some(0) } else { self.since_signal.map(|n| n + 1) };
        let stop = self.lows.next_hl(bar.low, bar.low)[2] * (1.0 - BUFFER);
        self.index += 1;

        if signal {
            strategy.entry(NAME, Long).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Long, bar.close),
            ));
        }
        let entered =
            strategy.lots().last().map(|lot| (bar.timestamp - lot.entry_timestamp) / BAR_MS);
        if self.since_signal.is_some_and(|n| n < 2) && entered.is_some_and(|n| n < 2) {
            let take = strategy.avg_cost() * (1.0 + TAKE);
            strategy.exit("longTP-SL").from(NAME).take(take).stop(stop);
        }
    }
}
