// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Triple EMA + RSI + ATR" by fhfsb, https://www.tradingview.com/script/HERuuRt5/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-12-05, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth when EMA(9) > EMA(21) > EMA(55), the close above EMA(9), RSI(14) between 50 and 65 and the
//! volume above 1.3 times its 20-bar SMA; short all of it on the mirror (RSI between 35 and 50), each reversing the other. On
//! every bar each side's exit is issued from the last average cost seen while held: a stop 2 ATR(14) away, a target 4 ATR(14),
//! and a trailing exit with no offset whose distance is 8 ATR(14) read as a count of ticks. 0.1% commission, 3 ticks; starts
//! with 1,000,000.
//!
//! In a run whose no-offset trail rests as a market once touched (`vike_tactics::RuleRun::touch_at_market`), the trailing exit
//! pays the slippage like every other fill that no limit bounds, though it has no offset and rests at its level.
//!
//! The exit is issued while flat as well, so an entry carries the levels its signal bar had: the previous trade's cost, or the
//! position it reverses. Before any trade there is no cost and the exit has no level.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! touch_at_market = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics, TacticsTrail};

const UP: &str = "long";
const DOWN: &str = "short";
const STOP_ATRS: f64 = 2.0;
const FIRST_RR: f64 = 2.0;
const SECOND_RR: f64 = 4.0;

struct S {
    fast: IndicatorLine,
    mid: IndicatorLine,
    slow: IndicatorLine,
    rsi: IndicatorLine,
    atr: IndicatorLine,
    volume: IndicatorLine,
    /// The average cost last seen while held.
    entry: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[9.0]),
        mid: IndicatorLine::new("ema", &[21.0]),
        slow: IndicatorLine::new("ema", &[55.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        volume: IndicatorLine::new("sma", &[20.0]),
        entry: f64::NAN,
    })
}

fn some(x: f64) -> Option<f64> {
    (!x.is_nan()).then_some(x)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let mid = self.mid.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let volume_ma = self.volume.next(&Bar { close: bar.volume, ..bar.clone() })[0];
        let surge = bar.volume > volume_ma * 1.3;
        let long =
            fast > mid && mid > slow && bar.close > fast && rsi > 50.0 && rsi < 65.0 && surge;
        let short =
            fast < mid && mid < slow && bar.close < fast && rsi < 50.0 && rsi > 35.0 && surge;
        if long {
            strategy.entry(UP, Long).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Long, bar.close),
            ));
        }
        if short {
            strategy.entry(DOWN, Short).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Short, bar.close),
            ));
        }
        if strategy.position() != 0.0 {
            self.entry = strategy.avg_cost();
        }
        let entry = self.entry;
        let long_stop = entry - atr * STOP_ATRS;
        let short_stop = entry + atr * STOP_ATRS;
        let long_take = entry + (entry - long_stop) * FIRST_RR;
        let long_far = entry + (entry - long_stop) * SECOND_RR;
        let short_take = entry - (short_stop - entry) * FIRST_RR;
        let short_far = entry - (short_stop - entry) * SECOND_RR;
        for (name, lots, take, stop, far) in [
            ("long exit", UP, long_take, long_stop, long_far - entry),
            ("short exit", DOWN, short_take, short_stop, entry - short_far),
        ] {
            let exit = strategy.exit(name).from(lots);
            if let Some(take) = some(take) {
                exit.take(take);
            }
            if let Some(stop) = some(stop) {
                exit.stop(stop);
            }
            if let Some(ticks) = some(far) {
                exit.trail(TacticsTrail { after_ticks: ticks.ceil(), by_ticks: 0.0 });
            }
        }
    }
}
