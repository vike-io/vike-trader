// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Triple EMA + QQE Trend Following Strategy [TradeDots]" by tradedots, https://www.tradingview.com/script/M4y5HT3h/
// The original's notice: © tradedots
// Changed: rewritten by hand in Rust for vike.

//! Two triple EMAs of the close (20 and 40) set the trend, a QQE line on RSI(14) times it. The QQE: RSI smoothed by EMA(5),
//! its bar-to-bar move smoothed by EMA(27), and a trailing level 4.238 such moves from the smoothed RSI that follows it from
//! one side and restarts on the other when crossed. With no trade open and not on a Monday (UTC), long 80% of the account's
//! worth when the close is above TEMA(20), TEMA(20) above TEMA(40), TEMA(40) rising and the smoothed RSI crossing up through
//! the level; short the mirror. The open trade's stop starts 120 ticks from the signal close and trails each close by as
//! much; a close beyond it closes the trade at the next open. 0.01% commission; starts with 10,000 in a USD account.
//!
//! The rule's cross is taken twice: once before this bar moves the level, where the level's last value is the level itself
//! (the last smoothed RSI against it), which moves it, and once against the moved level, which the entry reads.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Rsi, Tema};
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_model::time::utc_weekday;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// Not vike's `Qqe`: this script's level steps by one EMA(27) of the move, starts at 0 and turns on a cross of itself.
const FACTOR: f64 = 4.238;
/// The stop's distance from the close, in ticks of the instrument.
const STOP_TICKS: f64 = 120.0;
const SHARE: f64 = 80.0;

struct S {
    tema1: Tema,
    tema2: Tema,
    last_tema2: f64,
    rsi: Rsi,
    rsi_ma: Ema,
    last_rsi_ma: f64,
    move_ma: Ema,
    timestamp: f64,
    /// Smoothed RSI against the moved level.
    after: Cross,
    stop: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |length: f64| Ema::new(length).expect("a valid length").with_skip_nan(true);
    Box::new(S {
        tema1: Tema::new(20.0).expect("a valid length"),
        tema2: Tema::new(40.0).expect("a valid length"),
        last_tema2: f64::NAN,
        rsi: Rsi::new(14.0).expect("a valid length"),
        rsi_ma: ema(5.0),
        last_rsi_ma: f64::NAN,
        move_ma: ema(27.0),
        timestamp: 0.0,
        after: Cross::new(),
        stop: f64::NAN,
    })
}

fn is_monday(ts: i64) -> bool {
    utc_weekday(ts) == 0
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let width = STOP_TICKS * tick;
        let sizing = strategy.sizing();
        let tema1 = self.tema1.next_value(bar.close);
        let tema2 = self.tema2.next_value(bar.close);
        let last_tema2 = std::mem::replace(&mut self.last_tema2, tema2);
        let rsi_ma = self.rsi_ma.next_value(self.rsi.next(bar));
        let last_rsi_ma = std::mem::replace(&mut self.last_rsi_ma, rsi_ma);
        let ma = self.move_ma.next_value((last_rsi_ma - rsi_ma).abs());

        // Read before the level moves, so its history is the same level: a cross of the last smoothed RSI against it too.
        let ts = self.timestamp;
        let moved = if rsi_ma > ts && last_rsi_ma <= ts {
            rsi_ma - ma * FACTOR
        } else if rsi_ma < ts && last_rsi_ma >= ts {
            rsi_ma + ma * FACTOR
        } else if rsi_ma > ts {
            max(rsi_ma - ma * FACTOR, ts)
        } else {
            min(rsi_ma + ma * FACTOR, ts)
        };
        self.timestamp = if moved.is_nan() { rsi_ma } else { moved };
        let cross = self.after.step(rsi_ma, self.timestamp);

        let weekday = !is_monday(bar.timestamp);
        let long = bar.close > tema1
            && tema1 > tema2
            && tema2 > last_tema2
            && matches!(cross, Signal::Long)
            && weekday;
        let short = bar.close < tema1
            && tema1 < tema2
            && tema2 < last_tema2
            && matches!(cross, Signal::Short)
            && weekday;

        let held = strategy.position();
        if held == 0.0 {
            if long {
                strategy.entry("Long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
                self.stop = bar.close - width;
            } else if short {
                strategy.entry("Short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
                self.stop = bar.close + width;
            } else {
                self.stop = f64::NAN;
            }
        } else if held > 0.0 {
            if bar.close < self.stop {
                strategy.close("Long");
            }
            if bar.close - width > self.stop {
                self.stop = bar.close - width;
            }
        } else {
            if bar.close > self.stop {
                strategy.close("Short");
            }
            if bar.close + width < self.stop {
                self.stop = bar.close + width;
            }
        }
    }
}
