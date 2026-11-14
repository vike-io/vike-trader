// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Enhanced OB Retest Strategy v7.0" by averagemouse954, https://www.tradingview.com/script/jsQQPsfa/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence, so TradingView's Terms of Use make it MPL-2.0.

//! Order-block zones from the hourly candle, retraded on the chart. The hourly open, high, low and close a bar carries are those of
//! the hour it lies in, at their final values on both of its bars (`request.security(.., "60", .., lookahead_on)`: the rule
//! looks ahead, the hour's second half-bar included on the first). A swing high (low) is the highest high (lowest low) of those
//! hourly highs (lows) over the five bars before this one; the hourly close beyond it by 0.02% or more breaks the structure.
//!
//! A break up looks back over the last 6 chart bars for the nearest that closed under its open and takes that bar's hourly open
//! and close as a zone (a bull zone), if it is at least half an ATR(14) deep; a break down takes the nearest bar that closed over
//! its open (a bear zone). A new zone replaces the one held, unless they overlap, when the zone is their union. A bear zone the
//! close rises 3 ATR over (a bull zone it falls 3 ATR under) turns into a bull (bear) zone and the zone of the other side goes.
//! Wick zones are the high-to-body span of a bar whose high is the highest of the last 20 (strictly: an older equal high wins),
//! and the body-to-low span of one whose low is the lowest; a wick zone outlives the pullback that clears its price levels.
//!
//! Entries (all 0.1% of the account's worth at the close, at the next open): long with the close inside a bull zone between 25%
//! and 100% of its depth below its top, short in a bear zone likewise; long on the bar a high wick zone first overlaps a bear zone
//! and short on the bar a low wick zone first overlaps a bull zone; long when the close crosses up through a low wick zone's top
//! having traded under it, short crossing down through a high wick zone's bottom having traded over it. Only when flat, no
//! trade within two bars and no overlap of the two zones as the bar began. A trailing stop of 1.5 ATR from the close, which
//! only ratchets, rests from the first bar held. 0.05% commission; version 5 starts with 1,000,000.
//!
//! `import TradingView/Strategy/3` is imported and never called. The hour is `timeframe::Higher::looking_ahead`'s, from the
//! window's first bar, since the first half-bar of an hour needs the second.

use crate::desk::{Plan, Terms};
use crate::timeframe::Higher;
use vike_indicators::indicators::{Atr, Donchian};
use vike_marketdata::Bar;
use vike_model::OrderSide;
use vike_model::OrderSide::{Long, Short};
use vike_tactics::HigherTimeframePeriod;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{RuleDefaultsFee, Tactics};

const SWING: f64 = 5.0;
const LOOKBACK_OB: usize = 6;
const MIN_BREAK_PERCENT: f64 = 0.02;
const MIN_OB_ATRS: f64 = 0.5;
const MIN_RETRACE: f64 = 0.25;
const ENTRY_DEPTH: f64 = 1.0;
const TRAIL_ATRS: f64 = 1.5;
const FLIP_ATRS: f64 = 3.0;
const WICK_LOOKBACK: f64 = 20.0;
const MIN_BARS_BETWEEN: usize = 2;

/// A zone's `(top, bottom)`.
type Zone = (f64, f64);

fn overlap(a: Option<Zone>, b: Option<Zone>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => !(a.1 > b.0 || a.0 < b.1),
        _ => false,
    }
}

/// One chart bar's values and the hour's it lies in.
#[derive(Clone, Copy)]
struct Past {
    open: f64,
    close: f64,
    hour_open: f64,
    hour_close: f64,
}

struct S {
    hours: Higher,
    /// The hour this bar lies in, at its final values: `(open, high, low, close)`.
    hour: (f64, f64, f64, f64),
    atr: Atr,
    /// The bars before this one, newest first.
    past: Vec<Past>,
    bar_index: usize,
    bull: Option<Zone>,
    bear: Option<Zone>,
    high_wick: Option<Zone>,
    low_wick: Option<Zone>,
    /// The levels the wick-cross entries read, as `var`s with na after invalidation.
    high_wick_bottom: f64,
    low_wick_top: f64,
    last_close: f64,
    last_low_wick_top: f64,
    last_high_wick_bottom: f64,
    /// The overlap of each wick zone with its counterpart a bar ago.
    low_over_bull_before: bool,
    high_over_bear_before: bool,
    /// The five-bar channel of the hourly highs and lows, and its value as the last bar left it.
    swing: Donchian,
    swing_before: [f64; 3],
    /// The channel of the 19 bars before a bar: a wick zone needs a high over all of them (an older equal high wins) or a low under.
    wick: Donchian,
    wick_before: [f64; 3],
    last_trade_bar: Option<usize>,
    long_trail: f64,
    short_trail: f64,
}

pub fn new() -> Box<dyn Plan> {
    Box::new(S {
        hours: Higher::looking_ahead(HigherTimeframePeriod::Minutes(60)),
        hour: (f64::NAN, f64::NAN, f64::NAN, f64::NAN),
        atr: Atr::new(14.0).expect("a valid length"),
        past: Vec::new(),
        bar_index: 0,
        bull: None,
        bear: None,
        high_wick: None,
        low_wick: None,
        high_wick_bottom: f64::NAN,
        low_wick_top: f64::NAN,
        last_close: f64::NAN,
        last_low_wick_top: f64::NAN,
        last_high_wick_bottom: f64::NAN,
        low_over_bull_before: false,
        high_over_bear_before: false,
        swing: Donchian::new(SWING).expect("a valid length"),
        swing_before: [f64::NAN; 3],
        wick: Donchian::new(WICK_LOOKBACK - 1.0).expect("a valid length"),
        wick_before: [f64::NAN; 3],
        last_trade_bar: None,
        long_trail: f64::NAN,
        short_trail: f64::NAN,
    })
}

impl S {
    /// A value `i` bars back, NaN before the first.
    fn back(&self, i: usize) -> Option<Past> {
        self.past.get(i - 1).copied()
    }

    /// The first bar of the last 6 that closed against the direction, as how many bars back.
    fn block(&self, bearish: bool) -> Option<usize> {
        (1..=LOOKBACK_OB).find(|&i| {
            self.back(i).is_some_and(|p| if bearish { p.close < p.open } else { p.close > p.open })
        })
    }
}

impl Plan for S {
    fn terms(&self) -> Terms {
        Terms { cash: 1_000_000.0, commission: RuleDefaultsFee::Percent(0.05), ..Terms::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let atr = self.atr.next(bar);
        for hour in self.hours.next(bar) {
            self.hour = (hour.open, hour.high, hour.low, hour.close);
        }
        let (hour_open, hour_high, hour_low, hour_close) = self.hour;

        // Structure: the highest hourly high / lowest hourly low of the five bars before this one, which is the channel as the
        // last bar left it; this bar's hour goes in after.
        let [swing_high, _, swing_low] = self.swing_before;
        self.swing_before = self.swing.next_hl(hour_high, hour_low);
        let percent = |a: f64, b: f64| (a - b).abs() / b * 100.0;
        let break_up =
            hour_close > swing_high && percent(hour_close, swing_high) >= MIN_BREAK_PERCENT;
        let break_down =
            hour_close < swing_low && percent(hour_close, swing_low) >= MIN_BREAK_PERCENT;

        let union = |new: Zone, old: Option<Zone>| match old {
            Some(old) if overlap(Some(new), Some(old)) => (new.0.max(old.0), new.1.min(old.1)),
            _ => new,
        };
        if break_up && let Some(i) = self.block(true) {
            let p = self.back(i).expect("found above");
            let (top, bottom) = (p.hour_open.max(p.hour_close), p.hour_open.min(p.hour_close));
            if top - bottom >= MIN_OB_ATRS * atr {
                self.bull = Some(union((top, bottom), self.bull));
            }
        }
        if break_down && let Some(i) = self.block(false) {
            let p = self.back(i).expect("found above");
            let (top, bottom) = (p.hour_close.max(p.hour_open), p.hour_close.min(p.hour_open));
            if top - bottom >= MIN_OB_ATRS * atr {
                self.bear = Some(union((top, bottom), self.bear));
            }
        }
        let zones_overlap = overlap(self.bull, self.bear);

        // A zone the price runs through turns round.
        let flip = FLIP_ATRS * atr;
        if let Some(bear) = self.bear.filter(|z| close > z.0 + flip) {
            self.bear = None;
            self.bull = Some(bear);
        }
        if let Some(bull) = self.bull.filter(|z| close < z.1 - flip) {
            self.bull = None;
            self.bear = Some(bull);
        }

        let held = strategy.position();
        let can_trade = !zones_overlap
            && self.last_trade_bar.is_none_or(|b| self.bar_index - b >= MIN_BARS_BETWEEN)
            && held == 0.0;
        let enter = |this: &mut Self, strategy: &mut Tactics<'_>, name: &str, side: OrderSide| {
            strategy.entry(name, side).size(share_of_worth(&sizing, 0.1, close));
            this.last_trade_bar = Some(this.bar_index);
        };
        let depth_long = |z: Zone| (z.0 - close) / (z.0 - z.1);
        let depth_short = |z: Zone| (close - z.1) / (z.0 - z.1);
        if let Some(z) = self.bull.filter(|z| can_trade && close > z.1 && close < z.0)
            && (MIN_RETRACE..=ENTRY_DEPTH).contains(&depth_long(z))
        {
            enter(self, strategy, "Long", Long);
        }
        if let Some(z) = self.bear.filter(|z| can_trade && close > z.1 && close < z.0)
            && (MIN_RETRACE..=ENTRY_DEPTH).contains(&depth_short(z))
        {
            enter(self, strategy, "Short", Short);
        }

        // Wick zones: a new extreme over the last 20 bars (an older equal one wins).
        let [highest_before, _, lowest_before] = self.wick_before;
        self.wick_before = self.wick.next_hl(bar.high, bar.low);
        let (new_high, new_low) = (bar.high > highest_before, bar.low < lowest_before);
        if new_high {
            let bottom = bar.open.max(close);
            self.high_wick = Some((bar.high, bottom));
            self.high_wick_bottom = bottom;
        }
        if new_low {
            let top = bar.open.min(close);
            self.low_wick = Some((top, bar.low));
            self.low_wick_top = top;
        }
        let low_over_bull = overlap(self.low_wick, self.bull);
        let high_over_bear = overlap(self.high_wick, self.bear);
        let short_overlap = low_over_bull && !self.low_over_bull_before;
        let long_overlap = high_over_bear && !self.high_over_bear_before;
        self.low_over_bull_before = low_over_bull;
        self.high_over_bear_before = high_over_bear;

        let flat = held == 0.0;
        if long_overlap && flat && can_trade {
            enter(self, strategy, "Long", Long);
        }
        if short_overlap && flat && can_trade {
            enter(self, strategy, "Short", Short);
        }
        let crossed_up = close > self.low_wick_top && self.last_close <= self.last_low_wick_top;
        let crossed_down =
            close < self.high_wick_bottom && self.last_close >= self.last_high_wick_bottom;
        if bar.low < self.low_wick_top && crossed_up && flat && can_trade {
            enter(self, strategy, "Long", Long);
        }
        if bar.high > self.high_wick_bottom && crossed_down && flat && can_trade {
            enter(self, strategy, "Short", Short);
        }

        // The stop belongs to the position: gone with it, ratcheting while it is held.
        if held == 0.0 {
            (self.long_trail, self.short_trail) = (f64::NAN, f64::NAN);
        } else if held > 0.0 {
            let trail = close - TRAIL_ATRS * atr;
            self.long_trail =
                if self.long_trail.is_nan() { trail } else { self.long_trail.max(trail) };
            strategy.exit("Close Long").from("Long").stop(self.long_trail);
        } else {
            let trail = close + TRAIL_ATRS * atr;
            self.short_trail =
                if self.short_trail.is_nan() { trail } else { self.short_trail.min(trail) };
            strategy.exit("Close Short").from("Short").stop(self.short_trail);
        }

        // A wick zone is cleared when the close runs past it; its box, and so the overlaps, stay.
        if self.high_wick.is_some() && close > self.high_wick.map_or(f64::NAN, |z| z.0) {
            self.high_wick_bottom = f64::NAN;
        }
        if self.low_wick.is_some() && close < self.low_wick.map_or(f64::NAN, |z| z.1) {
            self.low_wick_top = f64::NAN;
        }
        (self.last_close, self.last_low_wick_top, self.last_high_wick_bottom) =
            (close, self.low_wick_top, self.high_wick_bottom);

        self.past.insert(0, Past { open: bar.open, close, hour_open, hour_close });
        self.past.truncate(LOOKBACK_OB);
        self.bar_index += 1;
    }
}
