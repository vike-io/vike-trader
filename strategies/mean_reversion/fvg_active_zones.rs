// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-vcp-fvg-active-zones-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Vortex Confluence Protocol [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/XtP27UK6-Vortex-Confluence-Protocol-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only, on fair value gaps: a bar whose low clears the high two bars back after an up bar makes a bull zone, the mirror a
//! bear zone, each only if the gap is at least 0.3 ATR(14). The newest 30 zones stay active. Buy when flat and the bar's range
//! touches a bull zone, close when long and it touches a bear zone.
//!
//! The ATR seed remembers its first bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_indicators::indicators::Atr;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const MIN_GAP_IN_ATRS: f64 = 0.3;
const MAX_ZONES: usize = 30;

struct Zone {
    top: f64,
    bottom: f64,
    bullish: bool,
}

/// The active zones, also copied into `combo_9`.
struct GapZones {
    atr: Atr,
    zones: VecDeque<Zone>,
    // Newest first. A missing bar is a NaN, which compares false.
    highs: [f64; 2],
    lows: [f64; 2],
    last_up: bool,
    last_down: bool,
}

impl GapZones {
    fn new() -> Self {
        GapZones {
            atr: Atr::new(14.0).expect("a valid period"),
            zones: VecDeque::new(),
            highs: [f64::NAN; 2],
            lows: [f64::NAN; 2],
            last_up: false,
            last_down: false,
        }
    }

    /// Records the zone this bar makes, if any, then says whether the bar touches a bull zone and a bear zone.
    fn step(&mut self, bar: &Bar) -> (bool, bool) {
        let min_gap = self.atr.next(bar) * MIN_GAP_IN_ATRS;
        let (high_2, low_2) = (self.highs[1], self.lows[1]);
        // A low above an old high and a high below an old low cannot both hold, so a bar makes one zone at most.
        if bar.low > high_2 && self.last_up && bar.low - high_2 >= min_gap {
            self.zones.push_back(Zone { top: bar.low, bottom: high_2, bullish: true });
        } else if bar.high < low_2 && self.last_down && low_2 - bar.high >= min_gap {
            self.zones.push_back(Zone { top: low_2, bottom: bar.high, bullish: false });
        }
        if self.zones.len() > MAX_ZONES {
            self.zones.pop_front();
        }
        self.highs = [bar.high, self.highs[0]];
        self.lows = [bar.low, self.lows[0]];
        (self.last_up, self.last_down) = (bar.close > bar.open, bar.close < bar.open);

        let touches = |bullish: bool| {
            self.zones.iter().any(|zone| {
                zone.bullish == bullish && bar.low <= zone.top && bar.high >= zone.bottom
            })
        };
        (touches(true), touches(false))
    }
}

struct S {
    zones: GapZones,
    // The position this bar opened with: a bar touching both zones only closes a long and only buys when flat.
    long: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { zones: GapZones::new(), long: false }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (in_bull, in_bear) = self.zones.step(bar);
        if in_bull && !self.long {
            self.long = true;
            Signal::Long
        } else if in_bear && self.long {
            self.long = false;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
