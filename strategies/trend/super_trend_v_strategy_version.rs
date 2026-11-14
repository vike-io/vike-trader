// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Super trend V Strategy version" by RafaelZioni, https://www.tradingview.com/script/7g8MF7f7/
// The original's notice: © theCrypster 2020
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-07-28, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A SuperTrend about a volume-weighted price line. Each bar's spread volume is `(close - open) * volume / (high - low)` (the
//! hundreds cancel); with its running total added, the series is smoothed over 14 and its distance from that, in units of its 28-bar
//! deviation and times the deviation of the high-low range, is the shadow. The line is the high plus the shadow when the shadow is
//! positive, otherwise the low plus it, averaged by an EMA of 168 bars (720 minutes over 30, times 7). Bands of that line, 1 ATR(10)
//! either side, ratchet on the last close; the trend turns up when the close passes the lower band's last value and down under the
//! upper's: vike's `Supertrend` judged against the last bar's bands (`Supertrend::with_previous_bands`) about the line
//! (`Supertrend::next_centred`). Long all of the account's worth when the close crosses up through the SuperTrend line over the EMA(168) of the open;
//! a cross down under the EMA closes the long (`allow_entry_in(long)`: the short entry only closes). 0.075% commission; starts
//! with 1,000.
//!
//! The rule's other lines (a regression of the VWAP, its deviation bands) only draw.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Ema, Sma, Stddev, Supertrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "buy";
const WINDOW: f64 = 28.0;
const SMOOTH: f64 = 14.0;
const LENGTH: f64 = 168.0;

struct S {
    total: f64,
    smooth: Sma,
    range_dev: Stddev,
    spread_dev: Stddev,
    line: Ema,
    open: Ema,
    atr: Atr,
    trend: Supertrend,
    cross_up: Cross,
    cross_down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        total: 0.0,
        smooth: Sma::new(SMOOTH).expect("a valid length").with_fresh_sum(true),
        range_dev: Stddev::new(WINDOW).expect("a valid length"),
        spread_dev: Stddev::new(WINDOW).expect("a valid length"),
        line: Ema::new(LENGTH).expect("a valid length").with_skip_nan(true),
        open: Ema::new(LENGTH).expect("a valid length"),
        atr: Atr::new(10.0).expect("a valid length"),
        trend: Supertrend::new(10.0, 1.0).expect("valid parameters").with_previous_bands(true),
        cross_up: Cross::new(),
        cross_down: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let spread_volume =
            (bar.close - bar.open) * 100.0 * (bar.volume / ((bar.high - bar.low) * 100.0));
        self.total += spread_volume;
        let v = spread_volume + self.total;
        let price_spread = self.range_dev.next_value(bar.high - bar.low);
        let smooth = self.smooth.next_value(v);
        let v_spread = self.spread_dev.next_value(v - smooth);
        let shadow = (v - smooth) / v_spread * price_spread;
        let out = if shadow > 0.0 { bar.high + shadow } else { bar.low + shadow };
        let vpt = self.line.next_value(out);
        let open = self.open.next_value(bar.open);
        let atr = self.atr.next(bar);

        let [line, _] = self.trend.next_centred(bar.close, vpt, atr);

        let rose = matches!(self.cross_up.step(bar.close, line), Signal::Long);
        let fell = matches!(self.cross_down.step(line, bar.close), Signal::Long);
        if rose && bar.close > open {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if fell && bar.close < open {
            strategy.close(NAME);
        }
    }
}
