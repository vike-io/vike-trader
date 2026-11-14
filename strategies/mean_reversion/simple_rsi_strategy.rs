// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Simple RSI strategy" by RafaelZioni, https://www.tradingview.com/script/CqjfRFhX/
// The original's notice: © RafaelZioni
// Changed: rewritten by hand in Rust for vike.

//! Long a fifth of the account's worth at the next open while RSI(5) of the close is at or under 25, one entry a bar, up to
//! five entries held. While long, on every bar, each entry gets a target 3% of this close above its own fill and a stop 10% of
//! this close under it, one cancelling the other; an entry signalled while already long gets them from its fill on, with the
//! distances of its signal bar. 0.075% commission; starts with 10,000.
//!
//! Each exit belongs to its own entry, in a run with bound exits (`vike_tactics::RuleRun::bound_exits`): it keeps that entry's
//! size and price and dies when that entry's size is spent, while the trade rows close first in, first out whichever exit
//! fills. The rule names an entry for the open trade rows when it is signalled, so two entries held can share a name; each is
//! named apart here, its exit tied to it alone.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const PYRAMIDING: usize = 5;
const ENTRY_IDS: usize = 7;
const OVERSOLD: f64 = 25.0;
const SHARE: f64 = 20.0;
const PROFIT: f64 = 3.0;
const LOSS: f64 = 10.0;

struct S {
    rsi: IndicatorLine,
    /// The entry waiting to fill: the distances its exit takes from the fill, when it was signalled while long.
    coming: Option<(f64, f64)>,
    /// The entries placed so far, which names the next one apart.
    placed: u64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { rsi: IndicatorLine::new("rsi", &[5.0]), coming: None, placed: 0 })
}

impl S {
    /// `(target, stop)` distances from an entry's fill, as this close makes them, counted in ticks of `tick`.
    fn distances(close: f64, tick: f64) -> (f64, f64) {
        let ticks = |percent: f64| close * (percent / 100.0) / tick;
        (tick * ticks(PROFIT), tick * ticks(LOSS))
    }

    /// The exit of the entry `name` filled at `price`.
    fn protect(strategy: &mut Tactics<'_>, name: &str, price: f64, (take, cut): (f64, f64)) {
        strategy.exit(&format!("Exit {name}")).from(name).take(price + take).stop(price - cut);
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults {
            fee: RuleDefaultsFee::Percent(0.075),
            pyramiding: PYRAMIDING,
            ..RuleDefaults::PLAIN
        }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        self.coming = None;
        let rsi = self.rsi.next(bar)[0];
        let open = strategy.lots().len();
        let held = strategy.position();
        if rsi <= OVERSOLD && open < ENTRY_IDS {
            let size = share_of_worth(&sizing, SHARE, bar.close);
            self.placed += 1;
            strategy.entry(&format!("Entry {} #{}", open + 1, self.placed), Long).size(size);
            self.coming = (held > 0.0).then(|| Self::distances(bar.close, tick));
        }
        if held > 0.0 {
            let distances = Self::distances(bar.close, tick);
            let entries: Vec<(String, f64)> =
                strategy.bindings().iter().map(|b| (b.name.clone(), b.price)).collect();
            for (name, price) in entries {
                Self::protect(strategy, &name, price, distances);
            }
        }
    }

    /// An entry signalled while long gets its exit from its fill on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(distances) = self.coming else { return };
        let Some(b) = strategy.bindings().last().filter(|b| b.entry_ms == bar.timestamp) else {
            return;
        };
        let (name, price) = (b.name.clone(), b.price);
        self.coming = None;
        Self::protect(strategy, &name, price, distances);
    }
}
