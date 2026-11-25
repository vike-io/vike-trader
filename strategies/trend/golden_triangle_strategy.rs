// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Golden Triangle Strategy" by eemani123, https://www.tradingview.com/script/YC0Fpmus/
// The original's notice: © mohanee
// Changed: rewritten by hand in Rust for vike.

//! Buy the dip after a triangle. A triangle is found on a bar that straddles SMA(50) of the close when the 10 bars before it
//! lay wholly above that SMA and the nearest earlier bar touching the SMA from above (low below it, close above it) is more
//! than one bar back; once found it stays found while less than 1 BTC is held, which is always here. Then long all of the
//! account's worth when the close crosses up through the day's VWAP while a pivot trend is up, at most two entries at once.
//! Both close when the close is above their average price and the VWAP crosses down through the SMA, or when the close crosses
//! down through 94% of that average price while 1 BTC or more is held, which never happens. No costs; starts with 10,000 in a
//! USD account.
//!
//! The look back for the touch stops at a bar wholly below the SMA (open below last bar's SMA, high below its own), which
//! counts as none, and at 200 bars. The pivot trend: a centre line follows each confirmed 5-and-5 pivot, a high before a low,
//! as `(2 * centre + pivot) / 3`; the bands are the centre less and plus 3 times ATR(50), the lower raised to last bar's while
//! last bar closed above it, the upper the mirror. The trend is up when the close is above last bar's upper band, down when
//! below last bar's lower band, else what it was (up at first): vike's `Supertrend` judged against the last bar's bands
//! (`Supertrend::with_previous_bands`) about the centre (`Supertrend::next_centred`). The size is `10% of worth / (6% of close)`, cut to
//! `worth / close` when it would cost the whole account or more, which is always.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Atr, ConfirmedPivot, Sma, Supertrend, Vwap, VwapSource};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "GT LE";
const LOOK_BACK: usize = 200;
const ABOVE_BARS: usize = 10;
const RISK_PERCENT: f64 = 10.0;
const STOP_PERCENT: f64 = 6.0;
const ATR_FACTOR: f64 = 3.0;

/// One bar as the look backs read it.
#[derive(Clone, Copy)]
struct Seen {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    sma: f64,
}

struct S {
    sma: Sma,
    /// This bar first, then the ones before it, `LOOK_BACK + 1` at most.
    seen: VecDeque<Seen>,
    found: bool,
    vwap: Vwap,
    pivots: ConfirmedPivot,
    atr: Atr,
    centre: f64,
    line: Supertrend,
    /// `1` up, `-1` down, `0` before the first bar.
    trend: i32,
    over_vwap: Cross,
    vwap_under_sma: Cross,
    under_stop: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        sma: Sma::new(50.0).expect("a valid length"),
        seen: VecDeque::with_capacity(LOOK_BACK + 1),
        found: false,
        vwap: Vwap::default().with_source(VwapSource::TypicalDivided),
        pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid lengths"),
        atr: Atr::new(50.0).expect("a valid length"),
        centre: f64::NAN,
        line: Supertrend::new(50.0, ATR_FACTOR)
            .expect("valid parameters")
            .with_previous_bands(true),
        trend: 0,
        over_vwap: Cross::new(),
        vwap_under_sma: Cross::new(),
        under_stop: Cross::new(),
    })
}

impl S {
    /// How many bars back the nearest touch of the SMA from above lies, 0 for none.
    fn touch_back(&self) -> usize {
        let sma_before = self.seen.get(1).map_or(f64::NAN, |b| b.sma);
        for (i, b) in self.seen.iter().enumerate().skip(1).take(LOOK_BACK) {
            if b.low < b.sma && b.close > b.sma {
                return i;
            }
            if b.open < sma_before && b.high < b.sma {
                return 0;
            }
        }
        0
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let sma = self.sma.next(bar);
        let (open, high, low, close) = (bar.open, bar.high, bar.low, bar.close);
        self.seen.push_front(Seen { open, high, low, close, sma });
        self.seen.truncate(LOOK_BACK + 1);
        let above =
            (1..=ABOVE_BARS).all(|i| self.seen.get(i).is_some_and(|b| b.high > sma && b.low > sma));
        let straddles = above && high > sma && low < sma;
        if straddles && self.touch_back() > 1 && strategy.position() < 1.0 {
            self.found = true;
        }

        let vwap = self.vwap.next(bar);
        let [pivot_high, pivot_low, _, _] = self.pivots.next(bar);
        let pivot = if pivot_high.is_nan() { pivot_low } else { pivot_high };
        if !pivot.is_nan() {
            self.centre =
                if self.centre.is_nan() { pivot } else { (self.centre * 2.0 + pivot) / 3.0 };
        }
        let atr = self.atr.next(bar);
        self.trend = if self.line.next_centred(close, self.centre, atr)[1] > 0.0 { 1 } else { -1 };

        let worth = sizing.equity();
        let mut size = (worth * RISK_PERCENT / 100.0) / (close * STOP_PERCENT / 100.0);
        if size * close >= worth {
            size = worth / close;
        }
        let crossed_vwap = matches!(self.over_vwap.step(close, vwap), Signal::Long);
        if self.found && self.trend == 1 && crossed_vwap {
            strategy.entry(NAME, Long).size(sizing.floor(size));
        }

        let held = strategy.position();
        if held >= 1.0 {
            self.found = false;
        }
        let stop =
            if held >= 1.0 { strategy.avg_cost() * (1.0 - STOP_PERCENT * 0.01) } else { 0.0 };
        let vwap_down = matches!(self.vwap_under_sma.step(vwap, sma), Signal::Short);
        if close > strategy.avg_cost() && vwap_down {
            strategy.close(NAME);
        }
        if matches!(self.under_stop.step(close, stop), Signal::Short) {
            strategy.close(NAME);
        }
    }
}
