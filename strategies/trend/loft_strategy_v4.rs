// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Loft Strategy V4" by BigCoinHunter, https://www.tradingview.com/script/Laq4rDWw/
// The original's notice: © BigCoinHunter
// Changed: rewritten by hand in Rust for vike.

//! A Kalman line of the close (gain 100: each step moves it by the gap times the square root of 0.02, plus a velocity that
//! gathers a hundredth of each gap) with a stop line under it while the side is long, 5% away, kept from falling, and
//! tightened by 0.001 percent each bar it rises, down to 1%; when the Kalman line crosses its stop line the side turns and the
//! line starts 5% the other way. On the first bar of each new side inside the window (2021-01-01 to 2022-12-30 23:59), all
//! is closed and an entry goes that way at the close. Its size is a martingale: 100 units, times 3 after each trade whose
//! second target was not reached (the first trade is already 300), back to 100 past 2700 and after a trade that reached it.
//! Three targets close 40%, 30% and 30% of the trade's size at the close of the first bar closing 1%, 3% and 5% past the
//! average entry; once the second is taken, a stop rests 0.2% past the entry on the profitable side. Held trades go on
//! after the window. 0.05% commission; starts with 10,000.
//!
//! Read while porting: the original keeps its account in USD, and its window starts before the first USDT-USD rate
//! (2021-05-04); before it one USDT is one dollar, the owner's ruling of 2026-11-16 (decision 0243 amended). So converted, all
//! 183 trades match and the net is 4465108.12 against the reference's 4464607, 501.12 over: taken on the owner's word of
//! 2026-11-16 ("OK") with exactly its own numbers. Booked in USDT throughout it is 4465760.05. The size deviation's profit factor is 1, so the targets
//! never move from 1%, 3% and 5%; the safe stop after the first target and the initial stop are off by default, as are the
//! labels and plots. Every target is a close of the entry's name: two that come on one bar are one order, the later's size.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG: &str = "LONG";
const SHORT: &str = "SHORT";
const GAIN: f64 = 100.0;
const STOP_PERCENT_BASE: f64 = 5.0;
const STOP_PERCENT_MIN: f64 = 1.0;
const STOP_PERCENT_STEP: f64 = 0.001;
const BASE_QTY: f64 = 100.0;
const QTY_DEVIATION: f64 = 3.0;
/// `BASE_QTY` times the deviation to the power of the safe orders (4) less one.
const MAX_ORDER_SIZE: f64 = BASE_QTY * QTY_DEVIATION * QTY_DEVIATION * QTY_DEVIATION;
const MAX_ORDER_QTY: f64 = 10_000.0;
/// The targets in percent of the average entry and the share of the trade's size each closes, in percent.
const TARGETS: [(f64, f64); 3] = [(1.0, 40.0), (3.0, 30.0), (5.0, 30.0)];
const SAFE_STOP_PERCENT: f64 = 0.2;

struct S {
    kalman: f64,
    velocity: f64,
    first: bool,
    long: bool,
    previous_long: Option<bool>,
    stop_line: f64,
    previous_stop_line: f64,
    stop_percent: f64,
    stopped_out_long: bool,
    stopped_out_short: bool,
    order_qty: f64,
    taken: [bool; 3],
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        kalman: 0.0,
        velocity: 0.0,
        first: true,
        long: true,
        previous_long: None,
        stop_line: 0.0,
        previous_stop_line: f64::NAN,
        stop_percent: STOP_PERCENT_BASE,
        stopped_out_long: false,
        stopped_out_short: false,
        order_qty: BASE_QTY,
        taken: [false; 3],
        from: timestamp(2021, 1, 1, 0, 0),
        to: timestamp(2022, 12, 30, 23, 59),
    })
}

impl S {
    /// The Kalman line's step on `source`; the line before the first bar is the source itself.
    fn kalman_step(&mut self, source: f64) -> f64 {
        let before = if self.first { source } else { self.kalman };
        self.first = false;
        let gap = source - before;
        let smooth = before + gap * (GAIN / 10000.0 * 2.0).sqrt();
        self.velocity += GAIN / 10000.0 * gap;
        self.kalman = smooth + self.velocity;
        self.kalman
    }

    /// The stop line's step: kept from moving against the side, tightened as it moves with it, and turned when crossed.
    fn stop_line_step(&mut self, kalman: f64) {
        let held = self.previous_long == Some(self.long);
        if self.long {
            self.stop_line = kalman - kalman * (self.stop_percent / 100.0);
            if held && self.stop_line <= self.previous_stop_line {
                self.stop_line = self.previous_stop_line;
            } else if held {
                self.tighten();
            }
            if kalman < self.stop_line {
                self.long = false;
                self.stop_percent = STOP_PERCENT_BASE;
                self.stop_line = kalman + kalman * (self.stop_percent / 100.0);
            }
        } else {
            self.stop_line = kalman + kalman * (self.stop_percent / 100.0);
            if held && self.stop_line >= self.previous_stop_line {
                self.stop_line = self.previous_stop_line;
            } else if held {
                self.tighten();
            }
            if kalman > self.stop_line {
                self.long = true;
                self.stop_percent = STOP_PERCENT_BASE;
                self.stop_line = kalman - kalman * (self.stop_percent / 100.0);
            }
        }
        self.previous_long = Some(self.long);
        self.previous_stop_line = self.stop_line;
    }

    fn tighten(&mut self) {
        self.stop_percent -= STOP_PERCENT_STEP;
        if self.stop_percent < STOP_PERCENT_MIN {
            self.stop_percent = STOP_PERCENT_MIN;
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let position = strategy.position();
        let average = strategy.avg_cost();
        let kalman = self.kalman_step(bar.close);
        self.stop_line_step(kalman);
        let in_window = bar.timestamp >= self.from && bar.timestamp <= self.to;
        let buy = in_window && self.long && !self.stopped_out_long;
        let sell = in_window && !self.long && !self.stopped_out_short;
        let last_profit = self.taken[1];
        if buy || sell {
            if last_profit {
                self.order_qty = BASE_QTY;
            } else {
                self.order_qty *= QTY_DEVIATION;
                if self.order_qty > MAX_ORDER_SIZE {
                    self.order_qty = BASE_QTY;
                }
            }
        }
        let size = self.order_qty.min(MAX_ORDER_QTY);
        if self.long {
            if buy {
                strategy.close_all();
                strategy.entry(LONG, Long).size(size);
            }
            (self.stopped_out_long, self.stopped_out_short) = (true, false);
        } else {
            if sell {
                strategy.close_all();
                strategy.entry(SHORT, Short).size(size);
            }
            (self.stopped_out_long, self.stopped_out_short) = (false, true);
        }
        let (name, guard, way) = if position > 0.0 {
            (LONG, "long safe stop", 1.0)
        } else {
            (SHORT, "short safe stop", -1.0)
        };
        if position != 0.0 {
            for (i, &(target, share)) in TARGETS.iter().enumerate() {
                let level = average * (1.0 + way * target * 0.01);
                if way * bar.close > way * level && !self.taken[i] {
                    strategy.close(name).size(size * share / 100.0);
                    self.taken[i] = true;
                }
            }
            if self.taken[1] {
                let stop = average * (1.0 + way * SAFE_STOP_PERCENT * 0.01);
                strategy.exit(guard).from(name).stop(stop);
            }
        }
        if buy || sell {
            self.taken = [false; 3];
        }
    }
}
