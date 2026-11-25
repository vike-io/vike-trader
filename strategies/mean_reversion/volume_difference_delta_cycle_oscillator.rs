// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Volume Difference Delta Cycle Oscillator" by tathal, https://www.tradingview.com/script/SseQ02vi/
// The original's notice: © tathal and special thanks to oakwhiz for his porting of my custom volume indicator
// Changed: rewritten by hand in Rust for vike.

//! The volume difference: each bar's change of on-balance volume less price-volume trend, rescaled to -1..1 by the lowest and
//! highest difference seen so far, then its EMA(20). Its %B in its own bands (EMA(20) of it, 2 population standard deviations of
//! 20 bars) is taken from the close's %B in the Bollinger bands (SMA(20), 2 deviations), and the result smoothed by three EMA(2)
//! in a row is the histogram. Under -0.1 it is long, over 0.4 short, on the bars from 2010-01-01 to before 2021-12-31 (UTC).
//!
//! Each side's entry takes 100% of the account's worth at the close. While flat it is a market order. While a position is held
//! it rests as a stop-limit priced from the position's average entry price: a long's trigger 2% under it and its limit 4% over
//! it, a short's trigger 2% over it and its limit 4% under it. A short signal while long, and a long one while short, also
//! closes the whole position at the next open. No costs; starts with 1,000,000. **Does not reproduce its reference** (145
//! trades, net -458562 on ETHUSDT 4h, 2017-08-17..2026-10-02): this file gives 143 / -453960.089879, two trades fewer and the
//! net 4601.91 over: the venue's 4-hour bars hold 19984 bars in the window where the reference counted 19987; the owner
//! accepted those numbers on 2026-11-15. Plots and colours are left out.
//!
//! Read while porting: the calls go in the original's order (long entry, close of a long, short entry, close of a short). An
//! entry issued while the other side is held rests at its own size plus that position, less a close of it already issued in
//! the same bar (the language's reversal size), so a long entry after a short, issued before its close, fills at about twice
//! the account's worth once the close has taken the short at the open; issued the other way round (close first) the entries
//! rest at their own size, 143 trades and -685588.15. An entry left resting while its signal is gone keeps resting, as the
//! original's does, and so does one after the window.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Bollinger, Ema, Obv, Pvt, Stddev};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 20.0;
const DEVIATIONS: f64 = 2.0;
const SMOOTHING: f64 = 2.0;
const OVERSOLD: f64 = -0.1;
const OVERBOUGHT: f64 = 0.4;
/// The stop and the take-profit level of an entry priced from the position, in percent of its average entry price.
const STOP_PERCENT: f64 = 2.0;
const TAKE_PERCENT: f64 = 4.0;

struct S {
    obv: Obv,
    pvt: Pvt,
    /// On-balance volume less price-volume trend on the bar before, missing before the first bar.
    difference: f64,
    /// The lowest and the highest difference change seen so far.
    lowest: f64,
    highest: f64,
    normalized: Ema,
    basis: Ema,
    deviation: Stddev,
    bands: Bollinger,
    smoothing: [Ema; 3],
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |length: f64| Ema::new(length).expect("a valid length").with_skip_nan(true);
    Box::new(S {
        obv: Obv::default(),
        pvt: Pvt::default(),
        difference: f64::NAN,
        lowest: 10e10,
        highest: -10e10,
        normalized: ema(LENGTH),
        basis: ema(LENGTH),
        deviation: Stddev::new(LENGTH).expect("a valid length"),
        bands: Bollinger::new(LENGTH, DEVIATIONS).expect("valid parameters"),
        smoothing: [ema(SMOOTHING), ema(SMOOTHING), ema(SMOOTHING)],
        from: timestamp(2010, 1, 1, 0, 0),
        to: timestamp(2021, 12, 31, 0, 0),
    })
}

impl S {
    /// The histogram on this bar, missing while it warms up.
    fn histogram(&mut self, bar: &Bar) -> f64 {
        let difference = self.obv.next(bar) - self.pvt.next(bar);
        let change = difference - self.difference;
        self.difference = difference;
        if !change.is_nan() {
            self.lowest = self.lowest.min(change);
            self.highest = self.highest.max(change);
        }
        let rescaled =
            -1.0 + 2.0 * (change - self.lowest) / (self.highest - self.lowest).max(10e-10);
        let line = self.normalized.next_value(rescaled);
        let basis = self.basis.next_value(line);
        let deviation = DEVIATIONS * self.deviation.next_value(line);
        let (upper, lower) = (basis + deviation, basis - deviation);
        let line_b = (line - lower) / (upper - lower);
        let [close_upper, _, close_lower] = self.bands.next(bar);
        let close_b = (bar.close - close_lower) / (close_upper - close_lower);
        self.smoothing.iter_mut().fold(close_b - line_b, |x, ema| ema.next_value(x))
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let histogram = self.histogram(bar);
        if bar.timestamp < self.from || bar.timestamp >= self.to {
            return;
        }
        let (long, short) = (histogram < OVERSOLD, histogram > OVERBOUGHT);
        let held = strategy.position();
        let average = strategy.avg_cost();
        let size = share_of_worth(&strategy.sizing(), 100.0, bar.close);
        if long {
            let entry = strategy.entry("long", Long).size(size);
            if held != 0.0 {
                entry
                    .stop(average * (1.0 - STOP_PERCENT / 100.0))
                    .limit(average * (1.0 + TAKE_PERCENT / 100.0));
            }
        }
        if held > 0.0 && short {
            strategy.close_all();
        }
        if short {
            let entry = strategy.entry("short", Short).size(size);
            if held != 0.0 {
                entry
                    .stop(average * (1.0 + STOP_PERCENT / 100.0))
                    .limit(average * (1.0 - TAKE_PERCENT / 100.0));
            }
        }
        if held < 0.0 && long {
            strategy.close_all();
        }
    }
}
