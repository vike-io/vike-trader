// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Trend Surfers - Premium Breakout" by TrendSurfersSignals, https://www.tradingview.com/script/w91kwvnX/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-04-04, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A buy stop at the 168-bar highest high of the bars before this one when the close crosses up through it, a sell stop at the
//! 168-bar lowest low when it crosses down, each sized to risk 2% of the account's worth between this close and the last close
//! less (plus) 2 ATR(10) of the last bar, and each reversing the other. A trade state follows the rule's own bookkeeping: a
//! signal from flat starts it on that side, the opposite signal or a touch of its stop line ends it, and while it runs the
//! fixed line sits 2 ATR(10) of its first bar from that bar's close and the trailing line follows the close by 8 ATR(10),
//! ratcheting. While held, on every bar, the stop is the farther of the two lines for a long (`max`) and the nearer for a short
//! (`min`); a line the state has not set is undefined and leaves the position with no stop. 0.04% commission; starts with
//! 100,000.
//!
//! The exit names no entry, so it is one stop per entry held. When a reversing stop entry and the held position's stop both gap
//! at one open, the entry fills first and the stop after it, opening its size the other way, under a run's committed stops
//! (`[engine.in_bar] committed_stops`): the position is then two entries at one price, each with a stop of its own, the one
//! the stop opened first, and a close of both is two fills that split the older entry in two rows.
//!
//! The rule's leverage arithmetic (`max(1, ceil(size / (worth / close))) * 2` up and back down) gives the risk size again, and
//! is kept as written for its last bits.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Donchian;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_indicators::strategy_kit::max;
use vike_indicators::strategy_kit::min;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics, TacticsSizing};

const UP: &str = "long";
const DOWN: &str = "short";
const RISK: f64 = 2.0;

struct S {
    range: Donchian,
    atr: IndicatorLine,
    /// `(highest high, lowest low)` through the last bar.
    last_range: (f64, f64),
    last_close: f64,
    last_fix: f64,
    up: Cross,
    down: Cross,
    in_trade: i32,
    long_trail: f64,
    short_trail: f64,
    fixed: f64,
    first: (f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range: Donchian::new(168.0).expect("a valid length"),
        atr: IndicatorLine::new("atr", &[10.0]),
        last_range: (f64::NAN, f64::NAN),
        last_close: f64::NAN,
        last_fix: f64::NAN,
        up: Cross::new(),
        down: Cross::new(),
        in_trade: 0,
        long_trail: 0.0,
        short_trail: 999_999.9,
        fixed: f64::NAN,
        first: (f64::NAN, f64::NAN),
    })
}

/// The rule's size for a risk between `close` and `stop`.
fn size(sizing: &TacticsSizing, worth: f64, close: f64, stop: f64) -> f64 {
    let most = worth / close;
    let risked = (RISK / 100.0 * worth) / (close - stop).abs();
    let leverage = max(1.0, (risked / most).ceil()) * 2.0;
    let percent = (risked * close / worth) * 100.0 / leverage;
    sizing.floor(percent / 100.0 * worth * leverage / close)
}

fn some(x: f64) -> Option<f64> {
    (!x.is_nan()).then_some(x)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (highest, lowest) = self.last_range;
        let [upper, _, lower] = self.range.next(bar);
        self.last_range = (upper, lower);
        let atr = self.atr.next(bar)[0];
        let trailing = atr * 8.0;
        let fix = atr * 2.0;
        let long = matches!(self.up.step(bar.close, highest), Signal::Long);
        let short = matches!(self.down.step(bar.close, lowest), Signal::Short);
        let long_stop = self.last_close - self.last_fix;
        let short_stop = self.last_close + self.last_fix;
        (self.last_close, self.last_fix) = (bar.close, fix);
        let worth = strategy.equity();
        // The long's risk is `close - stop`, the short's `stop - close`, each signed as the rule writes it.
        if long {
            let risked = (RISK / 100.0 * worth) / (bar.close - long_stop);
            if risked > 0.0 {
                strategy
                    .entry(UP, Long)
                    .size(size(&sizing, worth, bar.close, long_stop))
                    .stop(highest);
            }
        }
        if short {
            let risked = (RISK / 100.0 * worth) / (short_stop - bar.close);
            if risked > 0.0 {
                strategy
                    .entry(DOWN, Short)
                    .size(size(&sizing, worth, bar.close, short_stop))
                    .stop(lowest);
            }
        }

        let was = self.in_trade;
        if (long || short) && self.in_trade == 0 {
            self.in_trade = if long { 1 } else { -1 };
        }
        let fixed_before = self.fixed;
        if self.in_trade == 1 && (short || bar.low <= max(fixed_before, self.long_trail)) {
            self.in_trade = 0;
        }
        if self.in_trade == -1 && (long || bar.high >= min(fixed_before, self.short_trail)) {
            self.in_trade = 0;
        }
        self.long_trail =
            if self.in_trade == 1 { max(bar.close - trailing, self.long_trail) } else { 0.0 };
        self.short_trail = if self.in_trade == -1 {
            min(bar.close + trailing, self.short_trail)
        } else {
            999_999.0
        };
        self.fixed = f64::NAN;
        if self.in_trade != 0 {
            if self.in_trade != was {
                self.first = (bar.close, fix);
            }
            let (price, distance) = self.first;
            self.fixed = if self.in_trade == 1 { price - distance } else { price + distance };
        }

        let held = strategy.position();
        if held != 0.0 {
            let (name, own, cut) = if held > 0.0 {
                ("long stop", UP, some(max(self.fixed, self.long_trail)))
            } else {
                ("short stop", DOWN, some(min(self.fixed, self.short_trail)))
            };
            // One stop per entry held, the entry another stop opened ahead of the position's own.
            let mut entries: Vec<String> = Vec::new();
            for lot in strategy.lots() {
                if !entries.contains(&lot.name) {
                    entries.push(lot.name.clone());
                }
            }
            entries.sort_by_key(|lots| lots == own);
            for lots in entries {
                let exit = strategy.exit(&format!("{name} {lots}")).from(&lots);
                if let Some(cut) = cut {
                    exit.stop(cut);
                }
            }
        }
    }
}
