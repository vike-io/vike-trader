// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-ies-bb-kc-squeeze-release-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Integrated Execution System [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/SxASLpF5-Integrated-Execution-System-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only. A squeeze holds while Bollinger(20, 2) sits inside a channel of EMA(20) with 1.5 ATR(20) either side. Buy on the bar a
//! squeeze ends while flat, close on the bar the close crosses down through the EMA while long.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

struct S {
    bands: IndicatorLine,
    channel: IndicatorLine,
    was_squeezed: bool,
    through_middle: Cross,
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        bands: IndicatorLine::new("bollinger", &[20.0, 2.0]),
        channel: IndicatorLine::new("keltner", &[20.0, 20.0, 1.5]),
        was_squeezed: false,
        through_middle: Cross::new(),
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `bollinger` and `keltner` rows' lines are both upper, mid, lower.
        let bands = self.bands.next(bar);
        let channel = self.channel.next(bar);
        let squeezed = bands[2] > channel[2] && bands[0] < channel[0];
        let released = std::mem::replace(&mut self.was_squeezed, squeezed) && !squeezed;
        let lost_middle = matches!(self.through_middle.step(bar.close, channel[1]), Signal::Short);

        // A bar doing both only closes a long, and only buys when flat. Alone, each is safe for the runner to drop.
        if lost_middle && (self.held > 0 || !released) {
            self.held = 0;
            Signal::Close
        } else if released {
            self.held = 1;
            Signal::Long
        } else {
            Signal::Hold
        }
    }
}
