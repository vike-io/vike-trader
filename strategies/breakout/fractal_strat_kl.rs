// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Fractal Strat [KL]" by DojiEmoji, https://www.tradingview.com/script/agyP7x3R/
// The original's notice: © DojiEmoji
// Changed: rewritten by hand in Rust for vike.

//! Long 5% of the account's worth when flat, the close at or above the last fractal support, and ATR(5) far enough under
//! ATR(50): their difference over the 5-bar deviation of the true range, divided by the root of 5, below -1.96. A trailing stop
//! follows the low less 2 ATR(14), never lowered while held. A third of the position, rounded down to a whole unit, closes at
//! the next open the first time the close reaches the entry close plus 1, 2 and 3 times the 2 ATR(14) of the signal bar, each
//! while the close is above the average cost. No costs; starts with 1,000,000,000.
//!
//! A fractal support is the low 3 bars back when it is under the two lows before it, each falling, and under the two after it,
//! each rising, on a volume above that bar's 6-bar average. The rule resets its levels on the bar the position goes flat, after
//! the bar's entry has read them, so an entry on that bar works with a profit ladder from 0.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Atr, Sma, Stddev, TrueRange};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "Long";
const SHARE: f64 = 5.0;
const STOP_ATRS: f64 = 2.0;
const FAST: f64 = 5.0;
const CRITICAL: f64 = 1.96;
/// Bars back to the fractal's middle, and the oldest bar it reads.
const MIDDLE: usize = 3;
const OLDEST: usize = 5;

/// One bar as the fractal of a support reads it: low, volume and that bar's 6-bar average volume.
#[derive(Clone, Copy)]
struct Seen {
    low: f64,
    volume: f64,
    average: f64,
}

struct S {
    stop_atr: Atr,
    fast: Atr,
    slow: Atr,
    deviation: Stddev,
    volume: Sma,
    range: TrueRange,
    /// Newest first, `OLDEST + 1` bars.
    seen: VecDeque<Seen>,
    support: f64,
    stop: f64,
    entry: f64,
    risk: f64,
    taken: u32,
    last_held: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        stop_atr: Atr::new(14.0).expect("a valid length"),
        fast: Atr::new(FAST).expect("a valid length"),
        slow: Atr::new(50.0).expect("a valid length"),
        deviation: Stddev::new(FAST).expect("a valid length"),
        volume: Sma::new(6.0).expect("a valid length"),
        range: TrueRange::default().with_first_bar_missing(true),
        seen: VecDeque::with_capacity(OLDEST + 1),
        support: f64::NAN,
        stop: 0.0,
        entry: 0.0,
        risk: 0.0,
        taken: 0,
        last_held: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let held = strategy.position();
        let trail = self.stop_atr.next(bar) * STOP_ATRS;
        if held == 0.0 {
            self.stop = bar.low - trail;
        } else if held > 0.0 {
            self.stop = self.stop.max(bar.low - trail);
        }

        let average = self.volume.next_value(bar.volume);
        self.seen.push_front(Seen { low: bar.low, volume: bar.volume, average });
        self.seen.truncate(OLDEST + 1);
        if self.seen.len() > OLDEST {
            let s = &self.seen;
            let m = s[MIDDLE];
            let loud = m.volume > m.average;
            if m.low < s[4].low
                && s[4].low < s[5].low
                && s[2].low > m.low
                && s[1].low > s[2].low
                && loud
            {
                self.support = m.low;
            }
        }

        // The rule's true range is undefined on the first bar.
        let error = self.deviation.next_value(self.range.next(bar)) / libm::pow(FAST, 0.5);
        let test = (self.fast.next(bar) - self.slow.next(bar)) / error;
        let signal = bar.close >= self.support && test < -CRITICAL;

        if held > 0.0 {
            strategy.exit("SL").from(NAME).stop(self.stop);
        }
        if signal && held == 0.0 {
            self.entry = bar.close;
            self.risk = trail;
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if held != 0.0 && bar.close > strategy.avg_cost() {
            let reached = |k: f64| bar.close >= self.entry + k * self.risk;
            let third = (held / 3.0).floor();
            if (reached(3.0) && self.taken == 2)
                || (reached(2.0) && self.taken == 1)
                || (reached(1.0) && self.taken == 0)
            {
                strategy.close(NAME).size(third);
                self.taken += 1;
            }
        }
        if held != self.last_held && held == 0.0 {
            self.taken = 0;
            self.entry = 0.0;
            self.risk = 0.0;
            self.stop = 0.0;
        }
        self.last_held = held;
    }
}
