// SPDX-License-Identifier: Apache-2.0 AND MPL-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-vcp-manual-adx-regime-01/strategy.pine
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived in part from "Vortex Confluence Protocol [JOAT]", © officialjackofalltrades, https://www.tradingview.com/script/XtP27UK6-Vortex-Confluence-Protocol-JOAT/
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only, on the ADX(14) regime: bullish means ADX above 25 with +DI above -DI. Buy on the bar it turns bullish, close on the
//! bar it stops being so.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    adx: IndicatorLine,
    was_bullish: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { adx: IndicatorLine::new("adx", &[14.0]), was_bullish: false }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `adx` row's lines are adx, +DI, -DI.
        let lines = self.adx.next(bar);
        let bullish = lines[0] > 25.0 && lines[1] > lines[2];
        match (bullish, std::mem::replace(&mut self.was_bullish, bullish)) {
            (true, false) => Signal::Long,
            (false, true) => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
