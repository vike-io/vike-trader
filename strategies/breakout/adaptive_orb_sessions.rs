// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "ADAPTIVE ORB SESSIONS" by VONKAR, https://www.tradingview.com/script/ErwhAFlr/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-02-14, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Three opening-range breakouts a day on Los Angeles time (the US daylight saving rule): London from 00:00 to 06:30, New
//! York from 06:30 to 13:00 and Asia from 15:00 to midnight, a bar belonging to a window by its open time. Each session's
//! opening range is the high and low of its first 15 minutes (on 30-minute bars, its first bar), built every day. Once it is
//! complete, the session's first close past it with the bar before at or inside it is its one trade: long over the high,
//! short under the low. A session trades only on its days, Los Angeles' day of the week (New York Monday, Wednesday,
//! Thursday and Friday; Asia Sunday and Thursday; London Monday, Thursday and Friday, short only), and a signal it does not
//! take leaves the session open for the next.
//!
//! The entry fills at the signal bar's close. Its stop is the range's other side and its risk the distance to it; its size is
//! 500 of risk a contract, floored and kept between 1 and 9, split into three targets as evenly as it goes, the earlier ones
//! taking the remainder (a target with no share is not issued). The targets are 1, 2 and 3 times the risk in New York and
//! 0.5, 1 and 1.5 in Asia and London, each scaled by the mean of the session's last 20 ranges over this one (kept between 0.4
//! and 1.8); each is an exit of its own share under the one stop. The stop climbs a stair as the bar's high reaches the
//! targets (a short: its low): to the entry once the first is reached, to the first at the second, to the second at the third,
//! every exit issued again with it; it stops climbing once a bar's low has reached it, the entry bar's included. At a
//! session's end every order is cancelled and what is held closes at that bar's close, as it does on any bar outside the three
//! windows. 1.2 a contract on each fill, 2 ticks of slippage, 50,000 to start in a USD account, margins of 1%, the exits walked
//! through the 1-minute bars inside each bar. **Does not reproduce its reference** (669 trades, net -1771.633667 on BTCUSDT
//! 30m, 2025-01-01..2026-07-30): this file gives 669 / -1771.610297 with its exits walked through the 1-minute bars (the
//! reference's setting) and the same, to the bit, on each bar's own walk (no fill moves), the net 0.023 over: booked in USDT
//! it is -1770.56, and one day's USDT-USD close moved by a tick brings it inside the reference's tolerance, so the miss is the
//! USD account's conversion; the owner's "ADD" of 2026-11-16 accepted those numbers, and covers both runs.
//!
//! Left out: what the original only draws or reports (the ranges, levels and labels, the previous session's high and low, the
//! profit table) and its alerts; its switches that are off (the re-arm after an exit, the retest entries, the entry buffer,
//! the flat window); and three parts that issue no order on its defaults: the flatten by the session that entered (the
//! session's end has flattened first), the one-bar block after a flatten (it is lifted before that session can enter again)
//! and the second flatten of a bar (it issues the same close again).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::{us_minute, weekday};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const DAY_MS: i64 = 86_400_000;
const MINUTE_MS: i64 = 60_000;
const MINUTES_A_DAY: i64 = 1_440;
/// Los Angeles' standard time, in hours from UTC.
const PACIFIC: i64 = -8;
const MAX_RISK: f64 = 500.0;
const MIN_CONTRACTS: f64 = 1.0;
const MAX_CONTRACTS: f64 = 9.0;
const LOOKBACK: usize = 20;
const SCALE_MIN: f64 = 0.4;
const SCALE_MAX: f64 = 1.8;
const CLOSE: &str = "EOS";

/// One session's settings: its entries' and exits' names, its opening range and trading window in minutes of the Los Angeles
/// day (a window whose end is not after its start runs past midnight), the days it trades (Monday first), the sides it takes
/// (long, short) and its targets in multiples of the risk.
struct Setup {
    long: &'static str,
    short: &'static str,
    exits: [&'static str; 3],
    range: (i64, i64),
    trade: (i64, i64),
    days: [bool; 7],
    sides: (bool, bool),
    multiples: [f64; 3],
}

const NEW_YORK: Setup = Setup {
    long: "NY Long",
    short: "NY Short",
    exits: ["NY TP1", "NY TP2", "NY TP3"],
    range: (390, 405),
    trade: (390, 780),
    days: [true, false, true, true, true, false, false],
    sides: (true, true),
    multiples: [1.0, 2.0, 3.0],
};

const ASIA: Setup = Setup {
    long: "AS Long",
    short: "AS Short",
    exits: ["AS TP1", "AS TP2", "AS TP3"],
    range: (900, 915),
    trade: (900, 0),
    days: [false, false, false, true, false, false, true],
    sides: (true, true),
    multiples: [0.5, 1.0, 1.5],
};

const LONDON: Setup = Setup {
    long: "LDN Long",
    short: "LDN Short",
    exits: ["LDN TP1", "LDN TP2", "LDN TP3"],
    range: (0, 15),
    trade: (0, 390),
    days: [true, false, false, true, true, false, false],
    sides: (false, true),
    multiples: [0.5, 1.0, 1.5],
};

/// Whether `minute` lies in `[from, to)`, a window past midnight when `to` is not after `from`.
fn inside(minute: i64, (from, to): (i64, i64)) -> bool {
    if from < to { (from..to).contains(&minute) } else { minute >= from || minute < to }
}

/// The instant `ts` on the Los Angeles clock, in ms: UTC less the offset `us_minute` applies (8 hours, 7 in summer).
fn local_ms(ts: i64) -> i64 {
    let utc_minute = ts.rem_euclid(DAY_MS) / MINUTE_MS;
    let behind = (utc_minute - us_minute(ts, PACIFIC)).rem_euclid(MINUTES_A_DAY);
    ts - behind * MINUTE_MS
}

/// The contracts a risk of `risk` a contract takes: 500 of it, floored, between 1 and 9.
fn contracts_for(risk: f64) -> f64 {
    (MAX_RISK / risk).floor().clamp(MIN_CONTRACTS, MAX_CONTRACTS)
}

/// `total` split across the three targets as evenly as it goes, the earlier ones taking the remainder.
fn split(total: f64) -> [f64; 3] {
    let base = (total / 3.0).floor();
    let rest = total - base * 3.0;
    [0.0, 1.0, 2.0].map(|k| if rest > k { base + 1.0 } else { base })
}

/// The targets' scale: the mean range over this range, between 0.4 and 1.8, or 1 while either is not above zero.
fn scale(range: f64, mean: f64) -> f64 {
    if range <= 0.0 || mean <= 0.0 {
        return 1.0;
    }
    SCALE_MIN.max(SCALE_MAX.min(1.0 / (range / mean)))
}

/// The bar's place on the Los Angeles clock.
struct Now {
    minute: i64,
    new_day: bool,
    weekday: usize,
}

struct Session {
    setup: &'static Setup,
    trading: bool,
    high: f64,
    low: f64,
    building: bool,
    complete: bool,
    fired: bool,
    /// 1 long, -1 short, 0 when no trade of this session is followed.
    side: f64,
    entry: f64,
    targets: [f64; 3],
    trail: f64,
    stopped: bool,
    sizes: [f64; 3],
    /// The last `LOOKBACK` ranges above zero, oldest first, and their mean.
    ranges: VecDeque<f64>,
    mean_range: f64,
}

impl Session {
    fn new(setup: &'static Setup) -> Self {
        Session {
            setup,
            trading: false,
            high: f64::NAN,
            low: f64::NAN,
            building: false,
            complete: false,
            fired: false,
            side: 0.0,
            entry: f64::NAN,
            targets: [f64::NAN; 3],
            trail: f64::NAN,
            stopped: false,
            sizes: [0.0; 3],
            ranges: VecDeque::new(),
            mean_range: 0.0,
        }
    }

    /// The session's part of the bar; true when its end flattened.
    fn step(
        &mut self,
        strategy: &mut Tactics<'_>,
        bar: &Bar,
        now: &Now,
        held: f64,
        last_close: f64,
    ) -> bool {
        let setup = self.setup;
        let in_range = inside(now.minute, setup.range);
        let trading = inside(now.minute, setup.trade);
        if trading && (!self.trading || now.new_day) {
            (self.high, self.low, self.building, self.complete) =
                (f64::NAN, f64::NAN, false, false);
            (self.fired, self.stopped) = (false, false);
            if held == 0.0 {
                self.side = 0.0;
            }
            (self.entry, self.targets, self.trail) = (f64::NAN, [f64::NAN; 3], f64::NAN);
        }
        let ended = !trading && self.trading;
        if ended {
            flatten(strategy, held);
            self.side = 0.0;
        }
        self.trading = trading;

        if in_range && !self.complete {
            self.building = true;
            if self.high.is_nan() || bar.high > self.high {
                self.high = bar.high;
            }
            if self.low.is_nan() || bar.low < self.low {
                self.low = bar.low;
            }
        }
        if !in_range && self.building && !self.complete && !self.high.is_nan() {
            self.complete = true;
            let range = self.high - self.low;
            if range > 0.0 {
                self.ranges.push_back(range);
                if self.ranges.len() > LOOKBACK {
                    self.ranges.pop_front();
                }
            }
            self.mean_range = if self.ranges.is_empty() {
                0.0
            } else {
                self.ranges.iter().sum::<f64>() / self.ranges.len() as f64
            };
        }

        let c = bar.close;
        let ready = self.complete && trading && !self.fired;
        let up = ready && c > self.high && last_close <= self.high;
        let down = ready && !up && c < self.low && last_close >= self.low;
        let today = setup.days[now.weekday];
        if up && today && setup.sides.0 {
            self.enter(strategy, c, 1.0);
        }
        if down && today && setup.sides.1 {
            self.enter(strategy, c, -1.0);
        }

        if self.side != 0.0 && !self.stopped && held != 0.0 {
            let before = self.trail;
            self.trail = self.stair(bar);
            if self.trail != before {
                self.issue_exits(strategy, self.trail);
            }
        }
        if self.side != 0.0 && !self.stopped {
            let hit = if self.side > 0.0 { bar.low <= self.trail } else { bar.high >= self.trail };
            if hit {
                (self.stopped, self.side) = (true, 0.0);
            }
        }
        ended
    }

    /// The session's trade at this close, `way` 1 long or -1 short. The close is past one side of the range and the stop is
    /// the other, so the risk is above zero.
    fn enter(&mut self, strategy: &mut Tactics<'_>, close: f64, way: f64) {
        let setup = self.setup;
        self.fired = true;
        self.entry = close;
        self.side = way;
        let stop = if way > 0.0 { self.low } else { self.high };
        let risk = if way > 0.0 { (close - stop).abs() } else { (stop - close).abs() };
        let range = self.high - self.low;
        for (target, multiple) in self.targets.iter_mut().zip(setup.multiples) {
            let reach = multiple * scale(range, self.mean_range);
            *target = if way > 0.0 { close + risk * reach } else { close - risk * reach };
        }
        self.trail = stop;
        let contracts = contracts_for(risk);
        self.sizes = split(contracts);
        if way > 0.0 {
            strategy.entry(setup.long, Long).size(contracts);
        } else {
            strategy.entry(setup.short, Short).size(contracts);
        }
        self.issue_exits(strategy, stop);
    }

    /// Each target's exit, of its share and under `stop`; a target with no share is not issued, the first always is.
    fn issue_exits(&self, strategy: &mut Tactics<'_>, stop: f64) {
        let from = if self.side > 0.0 { self.setup.long } else { self.setup.short };
        for (i, name) in self.setup.exits.into_iter().enumerate() {
            if i == 0 || self.sizes[i] > 0.0 {
                strategy.exit(name).from(from).size(self.sizes[i]).take(self.targets[i]).stop(stop);
            }
        }
    }

    /// The stop after this bar's step of the stair: the last target the bar reached moves it to the level before.
    fn stair(&self, bar: &Bar) -> f64 {
        let [first, second, third] = self.targets;
        if self.side > 0.0 {
            let reached = |t: f64| !t.is_nan() && bar.high >= t;
            if reached(third) {
                self.trail.max(second)
            } else if reached(second) {
                self.trail.max(first)
            } else if reached(first) {
                self.trail.max(self.entry)
            } else {
                self.trail
            }
        } else {
            let reached = |t: f64| !t.is_nan() && bar.low <= t;
            if reached(third) {
                self.trail.min(second)
            } else if reached(second) {
                self.trail.min(first)
            } else if reached(first) {
                self.trail.min(self.entry)
            } else {
                self.trail
            }
        }
    }
}

/// Every order cancelled, and what is held closed at this close.
fn flatten(strategy: &mut Tactics<'_>, held: f64) {
    strategy.cancel_all();
    if held > 0.0 {
        strategy.order(CLOSE, Short).size(held.abs());
    } else if held < 0.0 {
        strategy.order(CLOSE, Long).size(held.abs());
    }
}

struct S {
    sessions: [Session; 3],
    day: Option<i64>,
    last_close: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        sessions: [Session::new(&NEW_YORK), Session::new(&ASIA), Session::new(&LONDON)],
        day: None,
        last_close: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_contract(1.2), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let local = local_ms(bar.timestamp);
        let day = local.div_euclid(DAY_MS);
        let now = Now {
            minute: local.rem_euclid(DAY_MS) / MINUTE_MS,
            new_day: self.day.is_some_and(|d| d != day),
            weekday: usize::try_from(weekday(local)).expect("a weekday from 0 to 6"),
        };
        self.day = Some(day);
        let held = strategy.position();
        let mut ended = false;
        for session in &mut self.sessions {
            ended |= session.step(strategy, bar, &now, held, self.last_close);
        }
        if held != 0.0 && !ended && !self.sessions.iter().any(|s| s.trading) {
            flatten(strategy, held);
        }
        self.last_close = bar.close;
    }
}
