// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MACD Pullback Sniper | Trend + ADX Filtered Strategy" by blitz_locked, https://www.tradingview.com/script/V87nRt9v/
// The original's notice: © blitz_locked
// Changed: rewritten by hand in Rust for vike.

//! MACD(12, 26, 9) signal-line crosses, filtered: long on a cross up below the zero line with the close above EMA(200), short
//! on a cross down above zero with the close below it, both only while ADX(14) is over 20. The size risks 1% of the account's
//! worth over a stop 2 ATR(14) away, at most the whole worth at the close, floored to the quantity step. An entry carries a
//! stop 2 ATR from the signal close and a target twice as far; once held, both move to the same distances from the entry
//! price, with the ATR of the signal bar. A cross the other way that is not itself an entry closes the position at the next
//! open, and an entry reverses an opposite position. 0.05% commission, 2 ticks of slippage; starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Atr, Ema, Macd};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SL_MULTIPLIER: f64 = 2.0;
const RR: f64 = 2.0;
const RISK_PERCENT: f64 = 1.0;
const ADX_MIN: f64 = 20.0;
const STEP: f64 = 0.00001;

struct S {
    macd: Macd,
    trend: Ema,
    adx: Adx,
    atr: Atr,
    cross: Cross,
    entry_atr: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        trend: Ema::new(200.0).expect("a valid length"),
        adx: Adx::new(14.0).expect("a valid length"),
        atr: Atr::new(14.0).expect("a valid length"),
        cross: Cross::new(),
        entry_atr: f64::NAN,
    })
}

/// A size floored to 0.00001 as the shortest decimal that reads back as `qty` says, not as `qty * 100000` rounds.
fn floor_decimal(qty: f64) -> f64 {
    if qty <= 0.0 || !qty.is_finite() {
        return 0.0;
    }
    let text = format!("{qty}");
    let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
    let frac: String = frac.chars().chain(std::iter::repeat('0')).take(5).collect();
    let lots: u64 = format!("{int}{frac}").parse().expect("digits");
    lots as f64 / 100_000.0
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [macd, signal, _] = self.macd.next(bar);
        let trend = self.trend.next(bar);
        let [adx, _, _] = self.adx.next(bar);
        let atr = self.atr.next(bar);
        let crossed = self.cross.step(macd, signal);
        let bull = matches!(crossed, Signal::Long);
        let bear = matches!(crossed, Signal::Short);
        let adx_ok = adx > ADX_MIN;
        let long_signal = bull && bar.close > trend && macd < 0.0 && adx_ok;
        let short_signal = bear && bar.close < trend && macd > 0.0 && adx_ok;

        let held = strategy.position();
        let stop = self.entry_atr * SL_MULTIPLIER;
        if held != 0.0 {
            let side = held.signum();
            let avg = strategy.avg_cost();
            let (name, lots) =
                if held > 0.0 { ("Long Exit", "Long") } else { ("Short Exit", "Short") };
            let cut = avg - side * stop;
            strategy.exit(name).from(lots).take(avg + side * stop * RR).stop(cut);
        }
        if bear && held > 0.0 && !short_signal {
            strategy.close("Long");
        }
        if bull && held < 0.0 && !long_signal {
            strategy.close("Short");
        }

        let worth = strategy.equity();
        let qty = |dist: f64| {
            let q = if dist > 0.0 { worth * RISK_PERCENT / 100.0 / dist } else { 0.0 };
            let q = q.min(worth / bar.close);
            floor_decimal((q / STEP).floor() * STEP)
        };
        let dist = atr * SL_MULTIPLIER;
        if long_signal && held <= 0.0 {
            let q = qty(dist);
            if q > 0.0 {
                self.entry_atr = atr;
                strategy.entry("Long", Long).size(q);
                strategy
                    .exit("Long Exit")
                    .from("Long")
                    .take(bar.close + dist * RR)
                    .stop(bar.close - dist);
            }
        }
        if short_signal && held >= 0.0 {
            let q = qty(dist);
            if q > 0.0 {
                self.entry_atr = atr;
                strategy.entry("Short", Short).size(q);
                strategy
                    .exit("Short Exit")
                    .from("Short")
                    .take(bar.close - dist * RR)
                    .stop(bar.close + dist);
            }
        }
    }
}
