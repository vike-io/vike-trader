// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "NoNonsense Forex" by Peter_O, https://www.tradingview.com/script/XnvfXLPF/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-11-08, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! An Aroon pair over 9 bars, `100 * (8 + bars since the highest high) / 8` and the same for the lowest low, the older bar
//! taken on a tie. Long 100,000 when the high line crosses up through the low line while the parabolic SAR (0.03 start,
//! 0.018 step, 0.11 cap) is under the close, RSI(14) is above 50 and the MACD (13, 26, 9) histogram above 0, unless the close is
//! above the baseline, the 20-bar Donchian middle, plus ATR(14). Short on the mirror. The opposite entry reverses. Each entry
//! carries a stop 50 ticks and a target half a tick from its fill price. No costs; starts with 100.
//!
//! The levels need the fill, so they are set right after it (`Rule::after_fill`) and are live for the rest of that bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Aroon, Donchian, Ema, Psar};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SIZE: f64 = 100_000.0;
/// The stop's and the target's distances from the entry, in ticks of the instrument.
const STOP_TICKS: f64 = 50.0;
const TARGET_TICKS: f64 = 0.5;

struct S {
    baseline: Donchian,
    atr: IndicatorLine,
    rsi: IndicatorLine,
    fast: Ema,
    slow: Ema,
    signal: Ema,
    sar: Psar,
    /// The Aroon pair, a tie going to the older bar.
    lines: Aroon,
    aroon: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        baseline: Donchian::new(20.0).expect("a valid length"),
        atr: IndicatorLine::new("atr", &[14.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        fast: Ema::new(13.0).expect("a valid length"),
        slow: Ema::new(26.0).expect("a valid length"),
        signal: Ema::new(9.0).expect("a valid length").with_skip_nan(true),
        sar: Psar::new(0.018, 0.11)
            .expect("valid factors")
            .with_start(0.03)
            .expect("a valid start")
            .with_test_first(true)
            .with_direction_from_closes(true)
            .with_textbook_steps(true),
        lines: Aroon::new(8.0).expect("a valid length").with_older_ties(true),
        aroon: Cross::new(),
    })
}

impl S {
    fn protect(strategy: &mut Tactics<'_>) {
        let Some(tick) = strategy.tick() else { return };
        let (stop, target) = (STOP_TICKS * tick, TARGET_TICKS * tick);
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if held > 0.0 {
            strategy.exit("long exit").from("long").take(cost + target).stop(cost - stop);
        } else if held < 0.0 {
            strategy.exit("short exit").from("short").take(cost - target).stop(cost + stop);
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if strategy.tick().is_none() {
            return; // its stop and target count in ticks
        }
        let baseline = self.baseline.next(bar)[1];
        let atr = self.atr.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let macd = self.fast.next_value(bar.close) - self.slow.next_value(bar.close);
        let histogram = macd - self.signal.next_value(macd);
        let sar = self.sar.next(bar);
        let [up, down] = self.lines.next(bar);
        let turn = self.aroon.step(up, down);
        // Outside the cave of one ATR around the baseline; a NaN band is inside it.
        let too_high = bar.close > baseline + atr;
        let too_low = bar.close < baseline - atr;
        let go_long = matches!(turn, Signal::Long)
            && bar.close > sar
            && rsi > 50.0
            && histogram > 0.0
            && !too_high;
        let go_short = matches!(turn, Signal::Short)
            && bar.close < sar
            && rsi < 50.0
            && histogram < 0.0
            && !too_low;
        if go_long {
            strategy.entry("long", Long).size(SIZE);
        }
        if go_short {
            strategy.entry("short", Short).size(SIZE);
        }
    }

    // The exits' levels are measured from the entry's fill, so they are issued here, after it; under the simulator's price
    // order (`[engine.in_bar] price_order`) they work from the fill's own level on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        Self::protect(strategy);
    }
}
