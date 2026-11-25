// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Divergence IQ [TradingIQ]" by Trading-IQ, https://www.tradingview.com/script/hgcTVYeX/
// The original's notice: © TradingIQ
// Changed: rewritten by hand in Rust for vike.

//! Divergences between the close and the price at its swing points, each traded with a stop and a target two ATR(14) from the
//! signal bar's close. A swing high of the close is one with 6 bars on each side (the left may tie, the right must be lower),
//! known 6 bars later; its price is that bar's high. When a swing high is found and the one before it exists: a higher high
//! with a lower close is a short ("Short Bearish Divergence"), a lower high with a higher close a short too ("Short Hidden
//! Bearish Divergence"). Swing lows of the close, priced at that bar's low, mirror it: a lower low with a higher close is a long
//! ("Long Bullish Divergence"), a higher low with a lower close a long ("Long Hidden Bullish Divergence"). Each entry is 5% of
//! the account's worth at the bar's close and carries one exit from its fill: a stop at the close less (a short: plus) twice
//! the ATR and a target at the close plus (a short: less) twice the ATR, both rounded to the tick. The orders fill at the close
//! of the bar that placed them. Commission 0.02%, slippage 1 tick; starts with 1,000, language version 6 with no margin stated
//! (100% of a position's value).
//!
//! A new entry waits for the rule's own record of its last trade, not for the position: from the bar after an entry, the trade
//! is over on the first bar that opens at or past its target or stop, or reaches one of them; where a bar reaches both, it is
//! over when one of the 1-minute bars inside it reaches one, or when there is no 1-minute bar. Until then no entry is placed,
//! and the bar it ends on may place the next one. The indicator the divergences are found on is the close (the original's
//! default input), its normalisation off and its method the swing points (both defaults). Its drawings, labels, table, alerts,
//! plots and win/loss tallies are left out: no order reads them.
//!
//! The 1-minute bars are an input of the run's own symbol at that bar length (decision 0271), read at the chart bar's close:
//! every 1-minute bar inside the chart bar, oldest first, and none before the series' first bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, ConfirmedPivot};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{self, Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const ONE_MINUTE: &str = "1m";
/// The bars on each side of a swing point.
const SIDE: usize = 6;
const SHARE: f64 = 5.0;
const ATR_MULTIPLE: f64 = 2.0;

/// A swing point: the close there and that bar's high (a swing high) or low (a swing low).
#[derive(Clone, Copy)]
struct Swing {
    close: f64,
    price: f64,
}

/// The newest swing point of one side and the one before it.
#[derive(Default)]
struct LastTwo {
    before: Option<Swing>,
    last: Option<Swing>,
}

impl LastTwo {
    fn push(&mut self, swing: Swing) {
        self.before = self.last.replace(swing);
    }

    /// The newest and the one before it, once two are in.
    fn pair(&self) -> Option<(Swing, Swing)> {
        Some((self.last?, self.before?))
    }
}

/// The rule's own record of its last trade: its side (`0` for none), target and stop.
struct Record {
    side: i32,
    target: f64,
    stop: f64,
}

impl Record {
    /// Whether the trade is over on `bar`, `minutes` the 1-minute bars inside it.
    fn ends(&self, bar: &Bar, minutes: &[Bar]) -> bool {
        let (target, stop) = (self.target, self.stop);
        if self.side > 0 {
            if bar.open >= target || bar.open <= stop {
                return true;
            }
            if bar.high >= target {
                return bar.low > stop
                    || minutes.is_empty()
                    || minutes.iter().any(|m| m.high >= target || m.low <= stop);
            }
            bar.low <= stop
        } else {
            if bar.open <= target || bar.open >= stop {
                return true;
            }
            if bar.low <= target {
                return bar.high < stop
                    || minutes.is_empty()
                    || minutes.iter().any(|m| m.low <= target || m.high >= stop);
            }
            bar.high >= stop
        }
    }
}

struct S {
    atr: Atr,
    swings: ConfirmedPivot,
    /// This bar's and the `SIDE` bars' before it `(high, low)`, newest first.
    extremes: [(f64, f64); SIDE + 1],
    highs: LastTwo,
    lows: LastTwo,
    record: Record,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: Atr::new(14.0).expect("a valid length"),
        swings: ConfirmedPivot::new(SIDE as f64, SIDE as f64).expect("valid spans"),
        extremes: [(f64::NAN, f64::NAN); SIDE + 1],
        highs: LastTwo::default(),
        lows: LastTwo::default(),
        record: Record { side: 0, target: f64::NAN, stop: f64::NAN },
    })
}

/// `price` rounded to the nearest tick, a tie up; as it is where no tick is known.
fn to_tick(price: f64, tick: Option<f64>) -> f64 {
    tick.map_or(price, |t| (price / t).round() * t)
}

impl S {
    /// No trade is followed and the ATR has a value.
    fn free(&self, atr: f64) -> bool {
        self.record.side == 0 && !atr.is_nan()
    }

    /// An entry of `side` under `entry`, its exit under `exit`, recorded as the trade the rule follows.
    fn open(
        &mut self,
        strategy: &mut Tactics<'_>,
        (side, entry, exit): (OrderSide, &str, &str),
        close: f64,
        atr: f64,
    ) {
        let reach = atr * ATR_MULTIPLE;
        let (stop, target) = match side {
            Long => (close - reach, close + reach),
            Short => (close + reach, close - reach),
        };
        let tick = strategy.tick();
        let (stop, target) = (to_tick(stop, tick), to_tick(target, tick));
        let size = share_of_worth(&strategy.sizing(), SHARE, close);
        strategy.entry(entry, side).size(size);
        strategy.exit(exit).from(entry).stop(stop).take(target);
        let side = if side == Long { 1 } else { -1 };
        self.record = Record { side, target, stop };
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.02), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![ONE_MINUTE.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let atr = self.atr.next(bar);
        let [swing_high, swing_low, _, _] = self.swings.next_hl(bar.close, bar.close);
        self.extremes.rotate_right(1);
        self.extremes[0] = (bar.high, bar.low);
        let (high_then, low_then) = self.extremes[SIDE];
        if !swing_high.is_nan() {
            self.highs.push(Swing { close: swing_high, price: high_then });
        }
        if !swing_low.is_nan() {
            self.lows.push(Swing { close: swing_low, price: low_then });
        }

        if self.record.side != 0
            && self.record.ends(bar, strategy.input(ONE_MINUTE).since(bar.timestamp))
        {
            self.record.side = 0;
        }
        let close = bar.close;
        if let Some((last, before)) = self.highs.pair().filter(|_| !swing_high.is_nan()) {
            if last.price > before.price && last.close < before.close && self.free(atr) {
                let names = (Short, "Short Bearish Divergence", "SB Exit");
                self.open(strategy, names, close, atr);
            }
            if last.price < before.price && last.close > before.close && self.free(atr) {
                let names = (Short, "Short Hidden Bearish Divergence", "SHB Exit");
                self.open(strategy, names, close, atr);
            }
        }
        if let Some((last, before)) = self.lows.pair().filter(|_| !swing_low.is_nan()) {
            if last.price < before.price && last.close > before.close && self.free(atr) {
                let names = (Long, "Long Bullish Divergence", "LB Exit");
                self.open(strategy, names, close, atr);
            }
            if last.price > before.price && last.close < before.close && self.free(atr) {
                let names = (Long, "Long Hidden Bullish Divergence", "LHB Exit");
                self.open(strategy, names, close, atr);
            }
        }
    }
}
