// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "888 BOT #backtest" by UnknownUnicorn2151907, https://www.tradingview.com/script/futPZepe/
// The original's notice: © Xaviz
// Changed: rewritten by hand in Rust for vike.

//! Seven filters of `hlc3`, all agreeing: the Jurik average (30, phase 40, power 2.5) rising, the range filter (20, 1.7) broken
//! upward, ADX(22) over 20 with +DI over -DI, the parabolic SAR (0.25, 0.25, 0.13) under the close, a volume-weighted RSI(34)
//! over 45, the MAC-Z histogram (9, 45, 47, 13, 14) over 0, and the volume over 1.4 times its 61-bar SMA. Long when they agree
//! on a bar that does not continue a long the rule's own book already counts; short on the mirror. Each entry buys 8% of the
//! account's worth times the volume against its 61-bar SMA (rounded, 1 to 8); up to ten add, and the rule's book re-enters at
//! the lower Bollinger band (20, 1.9 deviations of `hlc3`) 0.5% under its last entry when ADX does not point the other way.
//! Each trade closes at a target 1.7% (a short 1.8%) and a stop 3.7% from the average cost (less when the leverage times the
//! book's entries passes 11, the rule's 3.5% risk); while the book counts no entry, at those distances from the fill. 0.04%
//! commission; starts with 10,000. Orders only within 180 days before the rule's run.
//!
//! The rule keeps its own book of entries, closes and re-entries a bar behind the orders (a filter that agreed on the bar
//! before, after the opposite one last did); it is followed here step for step.
//!
//! The account is kept in USD on a chart quoted in USDT.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Ema, Psar, Sma, Smma, Stddev, Vwma};
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const DAY_MS: i64 = 86_400_000;
/// The days before the run's clock (`timenow`, the close of the run's last bar) the rule places orders in.
const ORDER_DAYS: i64 = 180;
const TP_LONG: f64 = 1.7 / 100.0;
const TP_SHORT: f64 = 1.8 / 100.0;
const SL: f64 = 3.7;
const RISK: f64 = 3.5;
const THRESHOLD: f64 = 20.0;

/// The Jurik average as the rule writes it, every state starting at 0.
// Left: its method was never published; the three-stage form here is known only from scripts' code, so vike has no row.
struct Jurik {
    phase_ratio: f64,
    beta: f64,
    alpha: f64,
    e0: f64,
    e1: f64,
    e2: f64,
    jma: f64,
}

impl Jurik {
    fn new(length: f64, phase: f64, power: f64) -> Self {
        let beta = 0.45 * (length - 1.0) / (0.45 * (length - 1.0) + 2.0);
        Jurik {
            phase_ratio: phase / 100.0 + 1.5,
            beta,
            alpha: libm::pow(beta, power),
            e0: 0.0,
            e1: 0.0,
            e2: 0.0,
            jma: 0.0,
        }
    }

    fn next(&mut self, src: f64) -> f64 {
        let (alpha, beta) = (self.alpha, self.beta);
        self.e0 = (1.0 - alpha) * src + alpha * self.e0;
        self.e1 = (src - self.e0) * (1.0 - beta) + beta * self.e1;
        self.e2 = (self.e0 + self.phase_ratio * self.e1 - self.jma) * libm::pow(1.0 - alpha, 2.0)
            + libm::pow(alpha, 2.0) * self.e2;
        self.jma += self.e2;
        self.jma
    }
}

/// The range filter: `[smoothed range, filter, bars up, bars down]`.
// Left: its smoothed range is one script's own definition, published nowhere else, so vike has no row.
struct RangeFilter {
    last_source: f64,
    average: Ema,
    smooth: Ema,
    mult: f64,
    filt: f64,
    upward: f64,
    downward: f64,
}

impl RangeFilter {
    fn new(per: f64, mult: f64) -> Self {
        RangeFilter {
            last_source: f64::NAN,
            average: Ema::new(per).expect("a valid length").with_skip_nan(true),
            smooth: Ema::new(per * 2.0 - 1.0).expect("a valid length").with_skip_nan(true),
            mult,
            filt: f64::NAN,
            upward: 0.0,
            downward: 0.0,
        }
    }

    fn next(&mut self, src: f64) -> (f64, f64, f64, f64) {
        let avrng = self.average.next_value((src - self.last_source).abs());
        self.last_source = src;
        let rng = self.smooth.next_value(avrng) * self.mult;
        let last = nan_to_zero(self.filt);
        let filt = if src > last {
            if src - rng < last { last } else { src - rng }
        } else if src + rng > last {
            last
        } else {
            src + rng
        };
        let was = self.filt;
        self.upward = if filt > was {
            self.upward + 1.0
        } else if filt < was {
            0.0
        } else {
            self.upward
        };
        self.downward = if filt < was {
            self.downward + 1.0
        } else if filt > was {
            0.0
        } else {
            self.downward
        };
        self.filt = filt;
        (rng, filt, self.upward, self.downward)
    }
}

/// The rule's weighted average: `(x + previous * (n - 1)) / n` from 0 (vike's in its own rounding).
fn wima(n: f64) -> Smma {
    Smma::new(n).expect("a valid length").with_zero_seed(true)
}

/// An SMA fed from its source's first number on, NaN before: the running sum would keep a NaN.
struct LateSma(Sma);

impl LateSma {
    fn new(n: f64) -> Self {
        LateSma(Sma::new(n).expect("a valid length"))
    }

    fn next(&mut self, x: f64) -> f64 {
        if x.is_nan() { f64::NAN } else { self.0.next_value(x) }
    }
}

/// One side of the rule's book. Times are bar open times in ms as `f64`, 0 for none; `NaN` is the rule's `na`.
#[derive(Default)]
struct Book {
    cond: bool,
    ini: f64,
    last_open: f64,
    last_signal: f64,
    last_final: f64,
    count: f64,
    sum: f64,
    last_tp: f64,
    last_sl: f64,
    last_bb: f64,
    final_bb: bool,
    leverage: f64,
}

/// What a side reads on one bar.
struct Side {
    /// `1` long, `-1` short.
    sign: f64,
    target: f64,
    /// The filters agree this bar.
    cond: bool,
    /// ADX points against this side.
    against: bool,
    /// The Bollinger band this side re-enters at, and the close is through it.
    band_through: bool,
}

struct S {
    jma: Jurik,
    last_jma: f64,
    range: RangeFilter,
    adx: Adx,
    sar: Psar,
    last_source: f64,
    up_volume: Smma,
    down_volume: Smma,
    mean: Vwma,
    spread: LateSma,
    fast: Sma,
    slow: Sma,
    deviation: Stddev,
    signal: LateSma,
    volume: Sma,
    basis: Sma,
    band: Stddev,
    long: Book,
    short: Book,
    position_price: f64,
    last_close: f64,
    /// Each side's exit as the last close stated it, `None` where it was cancelled: what an entry filling before the next
    /// close carries from its fill. And the position the last look saw.
    plans: [Option<Exit>; 2],
    held_seen: f64,
}

/// An exit as the rule states it: at prices, or at tick distances from the entry (`(target, stop)` both ways).
#[derive(Clone, Copy)]
enum Exit {
    At(f64, f64),
    From(f64, f64),
}

impl Exit {
    /// While the rule's book counts an entry and something is held, at its distances from the average cost; else at tick
    /// distances from the entry, taken off its last entry price.
    fn of(book: &Book, side: &Side, cost: f64, sl: f64, tick: f64) -> Exit {
        let s = side.sign;
        if book.count >= 1.0 && !cost.is_nan() {
            Exit::At(cost * (1.0 + s * side.target), (1.0 - s * sl / 100.0) * cost)
        } else {
            let open = book.last_open;
            let profit = (open * (1.0 + s * side.target) - open).abs() / tick;
            let loss = (open * (1.0 - s * sl / 100.0) - open).abs() / tick;
            Exit::From(profit, loss)
        }
    }

    /// The guard of a position of `sign` whose entry, or average cost, is `cost`.
    fn guard(self, sign: f64, cost: f64, tick: f64) -> (f64, f64) {
        match self {
            Exit::At(take, cut) => (take, cut),
            Exit::From(profit, loss) => (cost + sign * profit * tick, cost - sign * loss * tick),
        }
    }
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let mut long = Book::default();
    let mut short = Book::default();
    for b in [&mut long, &mut short] {
        b.leverage = f64::NAN;
        b.last_open = f64::NAN;
        b.count = f64::NAN;
    }
    Box::new(S {
        jma: Jurik::new(30.0, 40.0, 2.5),
        last_jma: f64::NAN,
        range: RangeFilter::new(20.0, 1.7),
        adx: Adx::new(22.0).expect("a valid length").with_held_lines(true),
        sar: Psar::new(0.25, 0.13)
            .and_then(|p| p.with_start(0.25))
            .expect("valid factors")
            .with_test_first(true)
            .with_direction_from_closes(true)
            .with_textbook_steps(true),
        last_source: f64::NAN,
        up_volume: wima(34.0),
        down_volume: wima(34.0),
        mean: Vwma::new(9.0).expect("a valid length"),
        spread: LateSma::new(9.0),
        fast: Sma::new(45.0).expect("a valid length"),
        slow: Sma::new(47.0).expect("a valid length"),
        deviation: Stddev::new(14.0).expect("a valid length"),
        signal: LateSma::new(13.0),
        volume: Sma::new(61.0).expect("a valid length"),
        basis: Sma::new(20.0).expect("a valid length"),
        band: Stddev::new(20.0).expect("a valid length"),
        long,
        short,
        position_price: f64::NAN,
        last_close: f64::NAN,
        plans: [None, None],
        held_seen: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.04), pyramiding: 10, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let time = bar.timestamp as f64;
        let src = (bar.high + bar.low + bar.close) / 3.0;

        let jma = self.jma.next(src);
        let (rising, falling) = (jma > self.last_jma, jma < self.last_jma);
        self.last_jma = jma;
        let (rng, filt, upward, downward) = self.range.next(src);
        let [adx, plus, minus] = self.adx.next(bar);
        let sar = self.sar.next(bar);
        let (up, down) = if src > self.last_source {
            ((src - self.last_source).abs() * bar.volume, 0.0)
        } else if src < self.last_source {
            (0.0, (src - self.last_source).abs() * bar.volume)
        } else {
            (0.0, 0.0)
        };
        self.last_source = src;
        let upt = self.up_volume.next_value(up);
        let dnt = self.down_volume.next_value(down);
        let rsi = 100.0 * (upt / (upt + dnt));
        let mean = self.mean.next(bar);
        let spread = self.spread.next((bar.close - mean) * (bar.close - mean)).sqrt();
        let z = (bar.close - mean) / spread;
        let macd = self.fast.next_value(src) - self.slow.next_value(src);
        let macz = z + macd / self.deviation.next_value(src);
        let hist = macz - self.signal.next(macz);
        let average_volume = self.volume.next_value(bar.volume);
        let loud = bar.volume > average_volume * 1.4;
        let basis = self.basis.next_value(src);
        let width = 1.9 * self.band.next_value(src);

        let trend_up = plus > minus && adx > THRESHOLD;
        let trend_down = plus < minus && adx > THRESHOLD;
        let long_side = Side {
            sign: 1.0,
            target: TP_LONG,
            cond: rising
                && bar.high > filt + rng
                && upward > 0.0
                && trend_up
                && sar < bar.close
                && rsi > 45.0
                && hist > 0.0
                && loud,
            against: trend_down,
            band_through: bar.close <= basis - width,
        };
        let short_side = Side {
            sign: -1.0,
            target: TP_SHORT,
            cond: falling
                && bar.low < filt - rng
                && downward > 0.0
                && trend_down
                && sar > bar.close
                && rsi < 45.0
                && hist < 0.0
                && loud,
            against: trend_up,
            band_through: bar.close >= basis + width,
        };

        // The rule's book, in its own order: each `[1]` is the value the last bar ended with.
        let (l, s) = (&mut self.long, &mut self.short);
        let (was_long, was_short) = (l.cond, s.cond);
        l.cond = long_side.cond;
        s.cond = short_side.cond;
        let (l_ini, s_ini) = (l.ini, s.ini);
        let next_ini = |ini: f64| {
            if was_long {
                1.0
            } else if was_short {
                -1.0
            } else {
                ini
            }
        };
        l.ini = next_ini(l_ini);
        s.ini = next_ini(s_ini);
        let long_condition = was_long && l_ini == -1.0;
        let short_condition = was_short && s_ini == 1.0;
        let long_open = long_condition || l.final_bb;
        let short_open = short_condition || s.final_bb;
        let previous_close = self.last_close;
        self.last_close = bar.close;
        l.last_open = if long_open { previous_close } else { nan_to_zero(l.last_open) };
        s.last_open = if short_open { previous_close } else { nan_to_zero(s.last_open) };
        l.last_signal = if long_open { time } else { l.last_signal };
        s.last_signal = if short_open { time } else { s.last_signal };
        let in_long = l.last_signal > s.last_signal;
        let in_short = s.last_signal > l.last_signal;
        if long_condition {
            l.last_final = time;
        }
        if short_condition {
            s.last_final = time;
        }
        let (l_sum, s_sum) = (l.sum, s.sum);
        l.count = nan_to_zero(l.count);
        s.count = nan_to_zero(s.count);
        if long_open {
            l.count += 1.0;
            s.count = 0.0;
            l.sum = nan_to_zero(l.last_open) + nan_to_zero(l_sum);
            s.sum = 0.0;
        }
        if short_open {
            l.count = 0.0;
            s.count += 1.0;
            s.sum = nan_to_zero(s.last_open) + nan_to_zero(s_sum);
            l.sum = 0.0;
        }
        let price = if long_open {
            l.sum / l.count
        } else if short_open {
            s.sum / s.count
        } else {
            f64::NAN
        };
        if !price.is_nan() {
            self.position_price = price;
        }
        let pp = self.position_price;

        let long_tp = bar.high > pp * (1.0 + TP_LONG) && in_long;
        let short_tp = bar.low < pp * (1.0 - TP_SHORT) && in_short;
        let (l_tp, s_tp, l_sl, s_sl) = (l.last_tp, s.last_tp, l.last_sl, s.last_sl);
        if long_tp {
            l.last_tp = time;
        }
        if short_tp {
            s.last_tp = time;
        }
        let final_long_tp = long_tp && l.last_signal > l_tp && l.last_signal > l_sl;
        let final_short_tp = short_tp && s.last_signal > s_tp && s.last_signal > s_sl;

        let equity = sizing.equity();
        let factor = 100.0 / (equity / bar.close);
        let quantity = 8.0 / factor;
        let room = |book: &Book| {
            ((RISK / factor) * 100.0) / (quantity * max(1.0, book.leverage) * 1f64.max(book.count))
        };
        let sl = min(SL, if in_long { room(l) } else { room(s) });
        let long_sl = in_long && bar.low <= (1.0 - sl / 100.0) * pp;
        let short_sl = in_short && bar.high >= (1.0 + sl / 100.0) * pp;
        if long_sl {
            l.last_sl = time;
        }
        if short_sl {
            s.last_sl = time;
        }
        let final_long_sl =
            long_sl && l.last_signal > l_sl && l.last_signal > l_tp && !final_long_tp;
        let final_short_sl =
            short_sl && s.last_signal > s_sl && s.last_signal > s_tp && !final_short_tp;

        let bb_long = in_long
            && !long_side.against
            && long_side.band_through
            && bar.close < l.last_open * (1.0 - 0.5 / 100.0);
        let bb_short = in_short
            && !short_side.against
            && short_side.band_through
            && bar.close > s.last_open * (1.0 + 0.5 / 100.0);
        let (l_bb, s_bb) = (l.last_bb, s.last_bb);
        if bb_long {
            l.last_bb = time;
        }
        if bb_short {
            s.last_bb = time;
        }
        l.final_bb = bb_long
            && l.last_final > l_bb
            && l.last_signal > l_tp
            && l.last_signal > l_sl
            && !final_long_sl;
        s.final_bb = bb_short
            && s.last_final > s_bb
            && s.last_signal > s_tp
            && s.last_signal > s_sl
            && !final_short_sl;

        if final_long_tp || final_long_sl {
            l.ini = -1.0;
            l.sum = 0.0;
            l.count = f64::NAN;
        }
        if final_short_tp || final_short_sl {
            s.ini = 1.0;
            s.sum = 0.0;
            s.count = f64::NAN;
        }
        let go_long = long_side.cond
            && (!in_long
                || final_long_tp
                || final_long_sl
                || (!long_condition && l.last_tp >= l.last_signal)
                || (!long_condition && l.last_sl >= l.last_signal));
        let go_short = short_side.cond
            && (!in_short
                || final_short_tp
                || final_short_sl
                || (!short_condition && s.last_tp >= s.last_signal)
                || (!short_condition && s.last_sl >= s.last_signal));
        let leverage = 8f64.min(1f64.max((bar.volume / average_volume).round()));
        l.leverage = if go_long { leverage } else { nan_to_zero(l.leverage) };
        s.leverage = if go_short { leverage } else { nan_to_zero(s.leverage) };

        // The bars after the one opening `ORDER_DAYS` before the run's last bar: from the clock less those days, the clock
        // being that bar's close. A run with no window (a paper mount) places them on every bar.
        let live =
            strategy.window().is_none_or(|(_, last)| bar.timestamp > last - ORDER_DAYS * DAY_MS);
        if live {
            if go_long {
                strategy.entry(UP, Long).size(sizing.floor(leverage * quantity));
            }
            if l.final_bb {
                strategy.entry(UP, Long).size(sizing.floor(l.leverage * quantity));
            }
            if go_short {
                strategy.entry(DOWN, Short).size(sizing.floor(leverage * quantity));
            }
            if s.final_bb {
                strategy.entry(DOWN, Short).size(sizing.floor(s.leverage * quantity));
            }
        }

        // Both exits are stated every bar with this bar's one stop distance, and cancelled on a re-entry bar.
        let cost = strategy.avg_cost();
        let held = strategy.position();
        let long_exit = Exit::of(l, &long_side, cost, sl, tick);
        let short_exit = Exit::of(s, &short_side, cost, sl, tick);
        self.plans = [(!l.final_bb).then_some(long_exit), (!s.final_bb).then_some(short_exit)];
        self.held_seen = held;
        if held > 0.0 {
            let (take, cut) = long_exit.guard(1.0, cost, tick);
            strategy.exit("Tpl").from(UP).take(take).stop(cut);
        }
        if l.final_bb {
            strategy.cancel("Tpl");
        }
        if held < 0.0 {
            let (take, cut) = short_exit.guard(-1.0, cost, tick);
            strategy.exit("Tps").from(DOWN).take(take).stop(cut);
        }
        if s.final_bb {
            strategy.cancel("Tps");
        }
    }

    /// An entry that opens a side from flat, or adds to it, carries the exit the last close stated from its fill. A reversal's
    /// new side waits for the next close.
    // The exits' levels are measured from the entry's fill, so they are issued here, after it; under the simulator's price
    // order (`[engine.in_bar] price_order`) they work from the fill's own level on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let held = strategy.position();
        let grew = held.abs() > self.held_seen.abs() && held * self.held_seen >= 0.0;
        self.held_seen = held;
        if !grew {
            return;
        }
        let (name, lots, sign, plan) = if held > 0.0 {
            ("Tpl", UP, 1.0, self.plans[0])
        } else {
            ("Tps", DOWN, -1.0, self.plans[1])
        };
        if let Some(plan) = plan {
            let (take, cut) = plan.guard(sign, strategy.avg_cost(), tick);
            strategy.exit(name).from(lots).take(take).stop(cut);
        }
    }
}
