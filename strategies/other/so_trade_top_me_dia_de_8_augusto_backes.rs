// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Só Trade Top - Média de 8 - Augusto Backes" by Valente_F, https://www.tradingview.com/script/cGX0aCYh/
// The original's notice: © Valente_F
// Changed: rewritten by hand in Rust for vike.

//! Long only, against the weekly EMA(8) of the week before's close as of the last week closed. Long all of the account's worth
//! on a rising engulfing, hammer or reversal close bar that closes and opens above that average; closed on the falling mirror
//! below it. Orders fill at the close that placed them. No costs; starts with 10,000.
//!
//! The engulfing bar closes above its open after a falling bar, at or above that bar's open, having opened at or under its
//! close; the hammer's lower shadow is more than twice its body and its upper shadow less than the body; the reversal close
//! makes a low under the last two and closes above the last close and its own open. A bar not there yet compares false.
//!
//! The weeks are built from the symbol's daily input, its history before the window included (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

const NAME: &str = "Compra";

/// A bar's `(open, high, low, close)`.
type Ohlc = (f64, f64, f64, f64);

struct S {
    weeks: HigherTimeframeSourceCalendar,
    ema: Ema,
    /// The last closed week's close, the EMA's next input once the week after it closes.
    week_close: f64,
    average: f64,
    /// The last two bars, newest first.
    last: [Ohlc; 2],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        weeks: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Week),
        ema: Ema::new(8.0).expect("a valid length"),
        week_close: f64::NAN,
        average: f64::NAN,
        last: [(f64::NAN, f64::NAN, f64::NAN, f64::NAN); 2],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for week in self.weeks.next(bar, strategy.input(higher::DAY)) {
            if !self.week_close.is_nan() {
                self.average = self.ema.next_value(self.week_close);
            }
            self.week_close = week.close;
        }
        let (o, h, l, c, m) = (bar.open, bar.high, bar.low, bar.close, self.average);
        let [(o1, h1, l1, c1), (_, h2, l2, _)] = self.last;
        self.last = [(o, h, l, c), self.last[0]];
        let body = (c - o).abs();
        let buy = (c > o && c1 < o1 && c >= o1 && o <= c1)
            || (2.0 * body < c.min(o) - l && h - c.max(o) < body && c > o)
            || (l < l1 && l < l2 && c > c1 && c > o);
        let sell = (c < o && c1 > o1 && c <= o1 && o >= c1)
            || (2.0 * body < h - c.max(o) && c.min(o) - l < body && c < o)
            || (h > h1 && h > h2 && c < c1 && c < o);
        if buy && c > m && o > m {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, c));
        }
        if sell && c < m && o < m {
            strategy.close(NAME);
        }
    }
}
