// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MACD, RSI, & RVOL Strategy" by BobBarker42069, https://www.tradingview.com/script/7GmXKfig/
// The original's notice: © BobBarker42069
// Changed: rewritten by hand in Rust for vike.

//! Long 100,000 worth when two of three cross up on one bar: RSI(14) through 30, the MACD histogram (EMA(12) less EMA(26) of
//! the close, less its EMA(9)) through 0, and the relative volume (the volume over its 14-bar SMA) through 2. Out when two
//! of three cross down: RSI through 70, the histogram through 0, the relative volume through 5. The rule may enter long
//! only, so its short entry closes the long. No costs; starts with 100,000.
//!
//! The histogram's and the volume's crosses are read only once the RSI exists, so on its first bar they have no bar before.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Rsi, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    rsi: Rsi,
    fast: Ema,
    slow: Ema,
    signal: Ema,
    volume: Sma,
    rsi_low: Cross,
    rsi_high: Cross,
    delta: Cross,
    realized_volatility_low: Cross,
    realized_volatility_high: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(14.0).expect("a valid length"),
        fast: Ema::new(12.0).expect("a valid length"),
        slow: Ema::new(26.0).expect("a valid length"),
        signal: Ema::new(9.0).expect("a valid length").with_skip_nan(true),
        volume: Sma::new(14.0).expect("a valid length"),
        rsi_low: Cross::new(),
        rsi_high: Cross::new(),
        delta: Cross::new(),
        realized_volatility_low: Cross::new(),
        realized_volatility_high: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar);
        let macd = self.fast.next(bar) - self.slow.next(bar);
        let delta = macd - self.signal.next_value(macd);
        let realized_volatility = bar.volume / self.volume.next_value(bar.volume);
        let co = matches!(self.rsi_low.step(rsi, 30.0), Signal::Long);
        let cu = matches!(self.rsi_high.step(rsi, 70.0), Signal::Short);
        if rsi.is_nan() {
            return;
        }
        let delta = self.delta.step(delta, 0.0);
        let (up, down) = (matches!(delta, Signal::Long), matches!(delta, Signal::Short));
        let realized_volatility_up =
            matches!(self.realized_volatility_low.step(realized_volatility, 2.0), Signal::Long);
        let realized_volatility_down =
            matches!(self.realized_volatility_high.step(realized_volatility, 5.0), Signal::Short);
        if (co && up) || (co && realized_volatility_up) || (up && realized_volatility_up) {
            strategy
                .entry("MACD & RSI BUY Long", Long)
                .size(cash_worth(&sizing, 100_000.0, bar.close));
        }
        if ((cu && down) || (cu && realized_volatility_down) || (down && realized_volatility_down))
            && strategy.position() > 0.0
        {
            strategy.close_all();
        }
    }
}
