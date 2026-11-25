// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-vcp-vol-zscore-anomaly-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Vortex Confluence Protocol [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/XtP27UK6-Vortex-Confluence-Protocol-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only, on volume more than 2 standard deviations from its SMA(20), both over the last 20 bars. Buy when such a bar closes
//! above its open, close when one closes below its open.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Sma, Stddev};
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    average: Sma,
    deviation: Stddev,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        average: Sma::new(20.0).expect("a valid period"),
        deviation: Stddev::new(20.0).expect("a valid period"),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let average = self.average.next_value(bar.volume);
        let deviation = self.deviation.next_value(bar.volume);
        // A NaN compares false, so nothing fires while the window fills.
        let anomaly = ((bar.volume - average) / deviation).abs() > 2.0;
        if anomaly && bar.close > bar.open {
            Signal::Long
        } else if anomaly && bar.close < bar.open {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
