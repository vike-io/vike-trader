// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "BTFD strategy [3min]" by wielkieef, https://www.tradingview.com/script/qRwx46ZQ/
// The original's notice: © wielkieef
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth, up to five entries, when the volume is over 2.5 times its 70-bar SMA and the rule's RSI(20)
//! is at or under 30. Each entry carries five exits, each a target and a stop: the stop 5% under the entry, the targets 0.4%,
//! 0.6%, 0.8%, 1.0% and 1.2% over it, both as whole ticks of the average cost, for 20%, 40%, 60%, 80% and 100% of the entry,
//! each slice taken off what the slices before it left. 0.03% commission; starts with 10,000.
//!
//! The RSI is the rule's own: Wilder averages of the close's rises and falls (the plain mean of the first 20, then alpha
//! steps), missing on the first bar, 100 while the mean fall is 0, else 0 while the mean rise is, else
//! `100 - 100 / (1 + up / down)`: vike's `Rsi` in the RS form, saturated at 0.
//!
//! Each entry is its own name and carries its own exits, in a run with bound exits (`vike_tactics::RuleRun::bound_exits`): a
//! slice of the entry's own size, levels from its own fill, spent with it, while every fill closes the oldest trades first and
//! the limit of five counts the entries still bound. An entry added while held gets its exits on its fill, at the distances of
//! the close before.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Rsi, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "BUY";
const TARGETS: [f64; 5] = [0.4, 0.6, 0.8, 1.0, 1.2];
const SHARES: [f64; 5] = [20.0, 40.0, 60.0, 80.0, 100.0];
const STOP: f64 = 5.0;
/// The quantity steps in one BTC.
const STEPS: f64 = 100_000.0;

struct S {
    volume: Sma,
    rsi: Rsi,
    /// The entries placed so far, which names the next one.
    placed: u64,
    /// The entries whose exits have been issued since the position opened.
    protected: Vec<String>,
    /// The tick distances set at the last close: the five targets and the stop.
    ticks: Option<([f64; 5], f64)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        volume: Sma::new(70.0).expect("a valid length"),
        rsi: Rsi::new(20.0).expect("a valid length").with_rs_form(true).with_zero_saturation(true),
        placed: 0,
        protected: Vec::new(),
        ticks: None,
    })
}

impl S {
    /// Issues the exits of every entry still bound, or with `fresh_only` of those not yet protected.
    fn protect(&mut self, strategy: &mut Tactics<'_>, fresh_only: bool) {
        let sizing = strategy.sizing();
        if strategy.position() == 0.0 {
            self.protected.clear();
            return;
        }
        let (Some((targets, stop)), Some(tick)) = (self.ticks, strategy.tick()) else { return };
        let entries: Vec<(String, f64, f64)> =
            strategy.bindings().iter().map(|b| (b.name.clone(), b.price, b.size)).collect();
        for (name, price, size) in entries {
            if fresh_only && self.protected.contains(&name) {
                continue;
            }
            // In whole steps, so the slices add up to the entry exactly.
            let whole = (size * STEPS).round();
            let mut reserved = 0.0;
            for (k, share) in SHARES.iter().enumerate() {
                let want =
                    (floor_share(size * share / 100.0, size, sizing.qty_step()) * STEPS).round();
                let slice = want.min(whole - reserved);
                if slice <= 0.0 {
                    continue;
                }
                reserved += slice;
                strategy
                    .exit(&format!("TP {} {name}", k + 1))
                    .from(&name)
                    .size(slice / STEPS)
                    .take(price + targets[k] * tick)
                    .stop(price - stop * tick);
            }
            if !self.protected.contains(&name) {
                self.protected.push(name);
            }
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.03), pyramiding: 5, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let rsi = self.rsi.next_value(bar.close);
        let busy = bar.volume > self.volume.next_value(bar.volume) * 2.5;
        if busy && rsi <= 30.0 {
            self.placed += 1;
            strategy
                .entry(&format!("{NAME} {}", self.placed), Long)
                .size(share_of_worth(&sizing, 100.0, bar.close));
        }
        let cost = strategy.avg_cost();
        self.ticks = (strategy.position() > 0.0).then(|| {
            (
                TARGETS.map(|p| (p / 100.0 * cost / tick).round()),
                (STOP / 100.0 * cost / tick).round(),
            )
        });
        self.protect(strategy, false);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        self.protect(strategy, true);
    }
}
