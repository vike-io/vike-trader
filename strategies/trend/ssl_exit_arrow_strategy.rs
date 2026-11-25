// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SSL Exit Arrow Strategy" by kevinmck100, https://www.tradingview.com/script/Sso2cdEm/
// The original's notice: Copyright © 2019 to present, Joris Duyck (JD)
// The original's notice: Copyright © 2018 Alex Orekhov (everget)
// The original's notice: Copyright © 2017 Jurik Research and Consulting.
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-09-01, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! The SSL hybrid's exit line: 15-bar Hull averages of the highs and of the lows, the line being the lows' average while the
//! last close outside the two was over the highs' one (or none was yet), the highs' average after a close under the lows'
//! one. When the close crosses up through the line, every short is closed and its unfilled entries pulled, and from
//! 2021-08-01 00:00 (UTC+1) three long limits rest 0.1, 0.3 and 0.7 ATR(14) under that close, each 10% of the account's
//! worth, each with a stop 1.8 ATR under that close for its own size; the long limits of the last signal are pulled first.
//! The line crossing up through the close is the mirror. Up to 10 entries a side. 0.04% commission; starts with 5,000.
//!
//! A limit's size spends 10% of the account's worth at its level on the grid. The stops rest from the signal bar, reducing
//! only, in the order the original issues them, so the run's exits are not bound to their entries: bound, each stop is placed
//! when its entry fills, and a stop placed inside the bar ahead of a resting short limit (a new buy goes ahead of the first
//! resting sell) is hit before the first entry's stop at the same level; first in, first out then closes the first entry in
//! two rows, 609 trades against the reference's 607 with the same net. The original's other lines (the baseline, its
//! channel, the second SSL, the QQE filter and its ATR bands) decide no order with the original's inputs, which leave both
//! filters off, and are left out, as are the labels and alerts.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Atr, Hma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{self, Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// A side's entries, each with its exit and its limit's distance under (over, for a short) the signal's close in ATRs.
const LONGS: [(&str, &str, f64); 3] = [
    ("LongEntry1", "LongExit1", 0.1),
    ("LongEntry2", "LongExit2", 0.3),
    ("LongEntry3", "LongExit3", 0.7),
];
const SHORTS: [(&str, &str, f64); 3] = [
    ("ShortEntry1", "ShortExit1", 0.1),
    ("ShortEntry2", "ShortExit2", 0.3),
    ("ShortEntry3", "ShortExit3", 0.7),
];
const STOP_ATRS: f64 = 1.8;
const SHARE_PERCENT: f64 = 10.0;
const PYRAMIDING: usize = 10;
const HOUR_MS: i64 = 3_600_000;

/// The rule's Hull average: its half length cut to a whole number (7 for 15), its last length `round(sqrt(length))`.
fn hull(length: f64) -> Hma {
    Hma::new(length).expect("a valid length").with_rounded_root(true)
}

struct S {
    exit_high: Hma,
    exit_low: Hma,
    /// 1 after a close over the highs' average, -1 after one under the lows', NaN before either.
    hlv: f64,
    close_over: Cross,
    line_over: Cross,
    atr: Atr,
    /// The original's "From" input, 2021-08-01 00:00 at UTC+1.
    from: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        exit_high: hull(15.0),
        exit_low: hull(15.0),
        hlv: f64::NAN,
        close_over: Cross::new(),
        line_over: Cross::new(),
        atr: Atr::new(14.0).expect("a valid length"),
        from: timestamp(2021, 8, 1, 0, 0) - HOUR_MS,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults {
            fee: Commission::percent(0.04),
            pyramiding: PYRAMIDING,
            ..RuleDefaults::PLAIN
        }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let atr = self.atr.next(bar);
        let (exit_high, exit_low) =
            (self.exit_high.next_value(bar.high), self.exit_low.next_value(bar.low));
        if bar.close > exit_high {
            self.hlv = 1.0;
        } else if bar.close < exit_low {
            self.hlv = -1.0;
        }
        let line = if self.hlv < 0.0 { exit_high } else { exit_low };
        let up = matches!(self.close_over.step(bar.close, line), Signal::Long);
        let down = matches!(self.line_over.step(line, bar.close), Signal::Long);
        let (side, own, other): (OrderSide, _, _) = match (up, down) {
            (true, _) => (Long, LONGS, SHORTS),
            (false, true) => (Short, SHORTS, LONGS),
            (false, false) => return,
        };
        for (entry, _, _) in other {
            strategy.cancel(entry);
        }
        for (entry, _, _) in other {
            strategy.close(entry);
        }
        if bar.timestamp < self.from {
            return;
        }
        for (entry, _, _) in own {
            strategy.cancel(entry);
        }
        let sizing = strategy.sizing();
        let sign = f64::from(side);
        for (entry, _, atrs) in own {
            let level = bar.close - sign * atr * atrs;
            let size = share_of_worth(&sizing, SHARE_PERCENT, strategy.snap_limit(side, level));
            strategy.entry(entry, side).size(size).limit(level);
        }
        let stop = bar.close - sign * atr * STOP_ATRS;
        for (entry, exit, _) in own {
            strategy.exit(exit).from(entry).stop(stop);
        }
    }
}
