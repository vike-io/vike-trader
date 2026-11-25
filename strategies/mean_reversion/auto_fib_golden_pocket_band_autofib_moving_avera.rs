// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Auto Fib Golden Pocket Band - Autofib Moving Average" by imal_max, https://www.tradingview.com/script/zXerPHbi/
// The original's notice: © imal_max reeee
// Changed: rewritten by hand in Rust for vike.

//! Three bands of fibonacci levels over the highest high and lowest low of 400, 900 and 4000 bars, each smoothed by an EMA
//! (120, 400, 150 bars): the strong band follows its bullish level (0.236 down from the high), the middle band a bullish
//! (0.618) and a bearish (0.382) one, the sideways band a bullish (0.6) and a bearish (0.5) one. A band's line is its bullish
//! EMA while that or the bearish one rises or holds, its bearish EMA while either falls (missing for the strong band, which
//! has no bearish level). A buy of a band: the line more than 50 bars past its last fall, the price's low under the line and
//! its close over it, the EMA of the low (10, 20 of the close, 80 bars) over it, and the line not far under the lowest low
//! (the line's lowest of 15, 10 or 150 bars less the lowest low of that many under 0.5%, 0.65%, 0.65% of the close), with the
//! bands' EMAs agreeing: all three trending or holding, the middle and sideways ones, or the sideways one. A sell is the mirror
//! with the high over the line, the close under it, the SMA of the high under it, and more than 50 bars past its last rise.
//! 100% of the account's worth per entry, up to 15 entries a side, the opposite side reversing. 0.05% commission, 2 ticks of
//! slippage, orders at the close of the bar that placed them; starts with 10,000 in a USD account.
//!
//! The exit is issued only while a buy (a sell) is at most one bar old and the last open trade at most one bar old, which on
//! a market entry at the close is once, on the bar after the entry's: for the long a target 4% over the position's average
//! price and a stop 1.4% under the lowest low of 5 bars; for the short, 1.6% under and 1.4% over the highest high. The levels
//! use the position as it is when the rule runs, whichever side, as the rule does.
//!
//! A `lowest` or `highest` of a series with gaps (the line is missing while it falls, for the strong band) restarts at each
//! missing value: NaN there, and over the numbers since it after (`Donchian::with_restart_at_missing`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_tactics::sizing::share_of_worth;
/// The length of the 30-minute bar the rule was written on.
const BAR_MS: i64 = 1_800_000;
use vike_indicators::indicators::{Donchian, Ema, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// The reference's comparison tolerance.
const EPS: f64 = 1e-10;

/// The `lowest` and `highest` of the last `length` values of a series with gaps, restarting at each missing value.
fn tail(length: usize) -> Donchian {
    Donchian::new(length as f64).expect("a valid length").with_restart_at_missing(true)
}

/// How a band's EMAs moved this bar.
#[derive(Clone, Copy, Default)]
struct Moves {
    up: bool,
    side: bool,
    down: bool,
}

/// One band: its levels, its EMAs and what its buy and its sell look at.
struct Band {
    /// The highest high and lowest low of the band's look-back (up to 4000 bars).
    range: Donchian,
    bull_fib: f64,
    bear_fib: Option<f64>,
    bull: Ema,
    bear: Ema,
    last: (f64, f64),
    /// The EMA the buy compares with, of `low` or `close`, and the SMA of the high the sell does.
    average: Ema,
    average_low: bool,
    high_average: Sma,
    /// The two tails of the line, `lowest` and `highest`, and the strengths of `low` and `high` over the same bars.
    line_low: Donchian,
    line_high: Donchian,
    lows: Donchian,
    highs: Donchian,
    buy_slack: f64,
    sell_slack: f64,
    since_down: Option<usize>,
    since_up: Option<usize>,
}

struct Spec {
    lookback: usize,
    length: f64,
    bull_fib: f64,
    bear_fib: Option<f64>,
    average: f64,
    average_low: bool,
    high_average: f64,
    buy_window: usize,
    sell_line_window: usize,
    sell_high_window: f64,
    slack: f64,
}

const STRONG: Spec = Spec {
    lookback: 400,
    length: 120.0,
    bull_fib: 0.236,
    bear_fib: None,
    average: 10.0,
    average_low: true,
    high_average: 10.0,
    buy_window: 15,
    sell_line_window: 15,
    sell_high_window: 15.0,
    slack: 0.005,
};
const MIDDLE: Spec = Spec {
    lookback: 900,
    length: 400.0,
    bull_fib: 0.618,
    bear_fib: Some(0.382),
    average: 20.0,
    average_low: false,
    high_average: 9.0,
    buy_window: 10,
    sell_line_window: 10,
    sell_high_window: 10.0,
    slack: 0.0065,
};
const SIDEWAYS: Spec = Spec {
    lookback: 4000,
    length: 150.0,
    bull_fib: 0.6,
    bear_fib: Some(0.5),
    average: 80.0,
    average_low: true,
    high_average: 20.0,
    buy_window: 150,
    sell_line_window: 15,
    sell_high_window: 20.0,
    slack: 0.0065,
};

fn ema(length: f64) -> Ema {
    Ema::new(length).expect("a valid length").with_skip_nan(true)
}

/// A bar's flags for the signs of a change, with the reference's tolerance; a missing change is none of them.
fn sign(change: f64) -> (bool, bool, bool) {
    (change > EPS, change.abs() <= EPS, change < -EPS)
}

impl Band {
    fn new(spec: &Spec) -> Self {
        Band {
            range: Donchian::new(spec.lookback as f64).expect("a valid length"),
            bull_fib: spec.bull_fib,
            bear_fib: spec.bear_fib,
            bull: ema(spec.length),
            bear: ema(spec.length),
            last: (f64::NAN, f64::NAN),
            average: Ema::new(spec.average).expect("a valid length"),
            average_low: spec.average_low,
            high_average: Sma::new(spec.high_average).expect("a valid length"),
            line_low: tail(spec.buy_window),
            line_high: tail(spec.sell_line_window),
            lows: Donchian::new(spec.buy_window as f64).expect("a valid length"),
            highs: Donchian::new(spec.sell_high_window).expect("a valid length"),
            buy_slack: spec.slack,
            sell_slack: spec.slack,
            since_down: None,
            since_up: None,
        }
    }

    /// Takes the bar and gives how the band moved, its line, and whether its own buy and sell conditions hold apart from the
    /// agreement between bands.
    fn next(&mut self, bar: &Bar) -> (Moves, f64, bool, bool) {
        let [high, _, low] = self.range.next_hl(bar.high, bar.low);
        let bull_price = (high - low) * (1.0 - self.bull_fib) + low;
        let bear_price = self.bear_fib.map_or(f64::NAN, |fib| (high - low) * (1.0 - fib) + low);
        let (bull, bear) = (self.bull.next_value(bull_price), self.bear.next_value(bear_price));
        let (bull_up, bull_side, bull_down) = sign(bull - self.last.0);
        let (bear_up, bear_side, bear_down) = sign(bear - self.last.1);
        self.last = (bull, bear);
        let moves = Moves {
            up: bull_up || bear_up,
            side: bull_side || bear_side,
            down: bull_down || bear_down,
        };
        let mut line = f64::NAN;
        if moves.up || moves.side {
            line = bull;
        }
        if moves.down {
            line = bear;
        }
        self.since_down = if moves.down { Some(0) } else { self.since_down.map(|n| n + 1) };
        self.since_up = if moves.up { Some(0) } else { self.since_up.map(|n| n + 1) };

        let source = if self.average_low { bar.low } else { bar.close };
        let average = self.average.next_value(source);
        let high_average = self.high_average.next_value(bar.high);
        let line_low = self.line_low.next_hl(line, line)[2];
        let line_high = self.line_high.next_hl(line, line)[0];
        let lowest_low = self.lows.next_hl(bar.low, bar.low)[2];
        let highest_high = self.highs.next_hl(bar.high, bar.high)[0];

        let not_fresh = self.since_down.is_none_or(|n| n > 50);
        let buy = not_fresh
            && line_low - lowest_low < bar.close * self.buy_slack
            && average > line
            && bar.low < line
            && bar.close > line;
        let sell_fresh = self.since_up.is_some_and(|n| n > 50);
        let sell = high_average < line
            && highest_high - line_high < bar.close * self.sell_slack
            && sell_fresh
            && bar.high > line
            && bar.close < line;
        (moves, line, buy, sell)
    }
}

struct S {
    bands: [Band; 3],
    since_buy: Option<usize>,
    since_sell: Option<usize>,
    lows: Donchian,
    highs: Donchian,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bands: [Band::new(&STRONG), Band::new(&MIDDLE), Band::new(&SIDEWAYS)],
        since_buy: None,
        since_sell: None,
        lows: Donchian::new(5.0).expect("a valid length"),
        highs: Donchian::new(5.0).expect("a valid length"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), pyramiding: 15, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [
            (strong, _, strong_buy, strong_sell),
            (middle, _, middle_buy, middle_sell),
            (sideways, _, sideways_buy, sideways_sell),
        ] = [0, 1, 2].map(|i| self.bands[i].next(bar));
        let up = [strong.up, middle.up, sideways.up];
        let side = [strong.side, middle.side, sideways.side];
        let down = [strong.down, middle.down, sideways.down];
        let any_of = |flags: [bool; 3]| flags.iter().any(|&f| f);
        let trend_or_side = any_of(side) || (any_of(up) && !any_of(down));
        let all_down = down.iter().all(|&f| f);
        let mid_side_trend_or_side = (middle.side || sideways.side)
            || ((middle.up || sideways.up) && !(middle.down || sideways.down));
        let mid_side_down = middle.down && sideways.down;
        let side_trend_or_side = sideways.side || (sideways.up && !sideways.down);

        let buy = (strong_buy && trend_or_side)
            || (middle_buy && mid_side_trend_or_side)
            || (sideways_buy && side_trend_or_side);
        let sell = (strong_sell && all_down && !trend_or_side)
            || (middle_sell && mid_side_down && !mid_side_trend_or_side)
            || (sideways_sell && sideways.down && !side_trend_or_side);
        self.since_buy = if buy { Some(0) } else { self.since_buy.map(|n| n + 1) };
        self.since_sell = if sell { Some(0) } else { self.since_sell.map(|n| n + 1) };
        let lowest = self.lows.next_hl(bar.low, bar.low)[2] * (1.0 - 0.014);
        let highest = self.highs.next_hl(bar.high, bar.high)[0] * (1.0 + 0.014);

        if buy {
            strategy.entry("Bull", Long).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Long, bar.close),
            ));
        }
        if sell {
            strategy.entry("Bear", Short).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Short, bar.close),
            ));
        }

        let held = strategy.position();
        let entered =
            strategy.lots().last().map(|lot| (bar.timestamp - lot.entry_timestamp) / BAR_MS);
        let fresh = entered.is_some_and(|n| n < 2);
        let average = strategy.avg_cost();
        // The position's side decides the levels' direction, whichever entry they are for; flat, there is no target.
        let level = |pct: f64| average * (1.0 + pct * held.signum());
        if self.since_buy.is_some_and(|n| n < 2) && fresh {
            let exit = strategy.exit("longTP-SL").from("Bull").stop(lowest);
            if held != 0.0 {
                exit.take(level(0.04));
            }
        }
        if self.since_sell.is_some_and(|n| n < 2) && fresh {
            let exit = strategy.exit("shortTP-SL").from("Bear").stop(highest);
            if held != 0.0 {
                exit.take(level(0.016));
            }
        }
    }
}
