// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MACD Strategy" by WunderTrading, https://www.tradingview.com/script/2jDHQAJc/
// The original's notice: © Deobald
// Changed: rewritten by hand in Rust for vike.

//! Long 100 of cash, up to two entries, when the rule's MACD(3, 5) crosses up through its 2-bar mean with SMA(34) under EMA(34).
//! Each entry has two targets on the average cost, re-set on every bar: 10% of the entry at 1% over it and 50% of the entry at 5%
//! over it, each firing once. Everything is closed when the close falls under the last signal bar's close less 2.2 times the
//! rule's ATR(17). 0.04% commission; starts with 100.
//!
//! The rule's EMA starts at its first value and its ATR starts at the first true range and moves by a seventeenth of the gap
//! (`Atr::with_seed_first`).
//! A target's size is its share of the entry, floored to the quantity step but never under one step. The targets close 60% of
//! an entry and leave the rest open, and an entry with a part still open on the last bar has paid its whole commission in the
//! net already: the first row that closes any of an entry takes all of its commission (`[engine] whole_entry_fee`).
//!
//! Each target belongs to its entry, in a run with bound exits (`vike_tactics::RuleRun::bound_exits`): a target's fill takes the
//! oldest rows first, and an entry stays counted against the two allowed until its own quantity is spent, which only the close
//! does, as the targets take 60% of it. So a second entry is the last until the close, however much of the first the targets
//! have taken. A target of an entry still to fill, beside one already held, is priced from the cost held now and works from
//! its fill. The account is kept in USD.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Ema};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::{cash_worth, floor_share};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const CASH: f64 = 100.0;
const ENTRIES: usize = 2;

struct S {
    fast: Ema,
    slow: Ema,
    signal: IndicatorLine,
    ribbon_ema: IndicatorLine,
    ribbon_sma: IndicatorLine,
    cross: Cross,
    entry_close: f64,
    atr: Atr,
    /// The entries placed so far, which names the next one.
    placed: u64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(3.0).expect("a valid length").with_seed_first(true),
        slow: Ema::new(5.0).expect("a valid length").with_seed_first(true),
        signal: IndicatorLine::new("sma", &[2.0]),
        ribbon_ema: IndicatorLine::new("ema", &[34.0]),
        ribbon_sma: IndicatorLine::new("sma", &[34.0]),
        cross: Cross::new(),
        entry_close: f64::NAN,
        atr: Atr::new(17.0).expect("a valid length").with_seed_first(true),
        placed: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults {
            fee: RuleDefaultsFee::Percent(0.04),
            pyramiding: ENTRIES,
            ..RuleDefaults::PLAIN
        }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let macd = self.fast.next(bar) - self.slow.next(bar);
        let signal = self.signal.next(&Bar { close: macd, ..bar.clone() })[0];
        let ribbon_ema = self.ribbon_ema.next(bar)[0];
        let ribbon_sma = self.ribbon_sma.next(bar)[0];
        let entry =
            matches!(self.cross.step(macd, signal), Signal::Long) && ribbon_sma < ribbon_ema;
        if entry {
            self.entry_close = bar.close;
        }
        let exit = bar.close < self.entry_close - 2.2 * self.atr.next(bar);

        let mut coming = None;
        if entry {
            self.placed += 1;
            let (name, qty) =
                (format!("{NAME} {}", self.placed), cash_worth(&sizing, CASH, bar.close));
            strategy.entry(&name, Long).size(qty);
            coming = (strategy.position() > 0.0).then_some((name, qty));
        }
        let cost = strategy.avg_cost();
        if !cost.is_nan() {
            let held = strategy.bindings().iter().map(|b| (b.name.clone(), b.size));
            for (name, size) in held.collect::<Vec<_>>().into_iter().chain(coming) {
                for (k, share, over) in [(1, 0.1, 1.01), (2, 0.5, 1.05)] {
                    let size = floor_share(size * share, size, sizing.qty_step());
                    strategy
                        .exit(&format!("TP{k} {name}"))
                        .from(&name)
                        .size(size)
                        .take(cost * over);
                }
            }
        }
        if exit && strategy.position() > 0.0 {
            strategy.close_all();
        }
    }
}
