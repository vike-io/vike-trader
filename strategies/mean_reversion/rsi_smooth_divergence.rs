// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-vcp-rsi-smooth-divergence-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Vortex Confluence Protocol [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/XtP27UK6-Vortex-Confluence-Protocol-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only, on RSI(14) smoothed by an EMA(3). Buy when the smoothed RSI is under 40 and both it and the close are above their
//! values 5 bars ago. Close when it is over 60 and both are below their values 5 bars ago.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_indicators::indicators::Ema;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const LOOKBACK: usize = 5;

/// Pushes the newest value and returns the one `LOOKBACK` bars back, NaN until the history is that long.
fn then(history: &mut VecDeque<f64>, value: f64) -> f64 {
    history.push_back(value);
    if history.len() > LOOKBACK { history.pop_front().unwrap_or(f64::NAN) } else { f64::NAN }
}

struct S {
    rsi: IndicatorLine,
    smoother: Ema,
    closes: VecDeque<f64>,
    smoothed: VecDeque<f64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        // The average seeds from the first defined values: the RSI's warm-up NaNs are skipped, not pushed.
        smoother: Ema::new(3.0).expect("a valid period").with_skip_nan(true),
        closes: VecDeque::new(),
        smoothed: VecDeque::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        let old_close = then(&mut self.closes, bar.close);
        let now = self.smoother.next_value(rsi);
        let old = then(&mut self.smoothed, now);
        if now < 40.0 && bar.close > old_close && now > old {
            Signal::Long
        } else if now > 60.0 && bar.close < old_close && now < old {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
