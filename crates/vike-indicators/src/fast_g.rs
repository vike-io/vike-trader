//! Fast cores for group `g` of the third wave of indicators (see `_followups/perf-conv3-brief.md`):
//! `dx`, `plus_di`, `minus_di`, `plus_dm`, `minus_dm`, `chaikin_volatility`, `chandelier`, `starc`,
//! `smma`, `supertrend`, `adxr` and `natr`.
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds no
//! `&mut self` call that could write the struct in its hot part, and is used by BOTH the streaming
//! stepper and the whole-series functions, so a stream and a batch agree bit for bit.
//!
//! The recurrences multiply by a precomputed `1/n` or `(n-1)/n` instead of dividing, so the last bits
//! differ from the textbook division form by the rounding noise any such rewrite has
//! (`tests/fast/fast_g.rs` holds them to a relative 1e-9). Warm-up costs nothing in the steady state: a
//! counter compared with a precomputed end is the only test, and the seed ladders sit behind it.

use crate::fast_a::HiLo;
use crate::fast_c::AdxCore;
use crate::roll::{RING_CAP, SmaCore};
use std::collections::VecDeque;

/// A period read from a raw parameter: rounded, and capped so a degenerate value (an infinity reads
/// as `usize::MAX`) cannot ask the allocator for the address space. A `NaN` or negative reads as 0.
#[inline]
pub(crate) fn period(v: f64) -> usize {
    v.round().min(RING_CAP as f64) as usize
}

// ------------------------------------------------------------------------------------------------
// Wilder / EMA smoothing
// ------------------------------------------------------------------------------------------------

/// A recurrence `previous = previous * keep + x * gain` seeded with the plain mean of its first `n` values:
/// Wilder's smoothing (`gain = 1/n`, `keep = (n-1)/n`) or an EMA (`gain = 2/(n+1)`, `keep = 1-gain`).
/// `NaN` until the seed is complete.
///
/// `SKIP` ignores the very first value (a true range needs a previous close, so ATRs that start at
/// bar 1 seed on values `1..=n`); the seed is then complete at value `n`, otherwise at value `n - 1`.
/// A seed starts at `-0.0`, as a left fold of `Iterator::sum` does. A period of 0 never emits.
#[derive(Clone)]
pub(crate) struct SmoothCore<const SKIP: bool> {
    gain: f64,
    keep: f64,
    inverse: f64,
    /// Values seen on the slow path; the steady state starts at `end` and leaves it alone.
    i: usize,
    end: usize,
    /// The index of the value that completes the seed.
    last: usize,
    seed: f64,
    previous: f64,
}

impl<const SKIP: bool> SmoothCore<SKIP> {
    #[inline]
    fn build(n: usize, gain: f64, keep: f64) -> Self {
        let skip = usize::from(SKIP);
        SmoothCore {
            gain,
            keep,
            // A period of 0 seeds with `NaN`, so it can never emit a number.
            inverse: if n == 0 { f64::NAN } else { 1.0 / n as f64 },
            i: 0,
            end: if n == 0 { usize::MAX } else { n + skip },
            last: (n + skip).wrapping_sub(1),
            seed: -0.0,
            previous: f64::NAN,
        }
    }

    /// Wilder's smoothing over `n` values.
    #[inline]
    pub(crate) fn wilder(n: usize) -> Self {
        Self::build(n, 1.0 / n as f64, (n as f64 - 1.0) / n as f64)
    }

    /// An exponential moving average over `n` values.
    #[inline]
    pub(crate) fn ema(n: usize) -> Self {
        let alpha = vike_math::smoothing::exponential_moving_average_alpha(n);
        Self::build(n, alpha, 1.0 - alpha)
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        if self.i >= self.end {
            self.previous = self.previous * self.keep + x * self.gain;
            return self.previous;
        }
        let i = self.i;
        self.i = i + 1;
        if SKIP && i == 0 {
            return f64::NAN;
        }
        self.seed += x;
        if i < self.last {
            return f64::NAN;
        }
        self.previous = self.seed * self.inverse;
        self.previous
    }
}

/// The true range of a bar against the previous close; a `NaN` previous close (the first bar) leaves
/// `high - low`, because `f64::max` hands back the number.
#[inline(always)]
fn true_range(h: f64, l: f64, pc: f64) -> f64 {
    (h - l).max((h - pc).abs()).max((l - pc).abs())
}

/// Wilder's average true range. With `SKIP` the first bar has no range (the ATR starts at bar `n`,
/// seeded on the ranges of bars `1..=n`); without it the first range is `high - low` (the ATR starts
/// at bar `n - 1`).
#[derive(Clone)]
pub(crate) struct AtrG<const SKIP: bool> {
    rma: SmoothCore<SKIP>,
    /// The previous close, `NaN` before the first bar.
    pc: f64,
}

impl<const SKIP: bool> AtrG<SKIP> {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        AtrG { rma: SmoothCore::wilder(n), pc: f64::NAN }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let tr = true_range(h, l, self.pc);
        self.pc = c;
        self.rma.push(tr)
    }
}

// ------------------------------------------------------------------------------------------------
// SMMA
// ------------------------------------------------------------------------------------------------

/// Wilder's smoothed moving average of a series: `NaN` for the first `n - 1` values.
///
/// `alpha` (`crate::indicators::Smma::with_alpha_steps`): the seed's sum (from `+0.0`) divided by
/// `n`, each step `alpha * x + (1 - alpha) * previous` with `alpha = 1 / n`.
///
/// `zero` (`crate::indicators::Smma::with_zero_seed`): no seed, every value a step (the form's own)
/// from the last, a missing last read as 0.
///
/// [`SmmaCore::push_period`] takes the length with each value.
#[derive(Clone)]
pub(crate) struct SmmaCore {
    rma: SmoothCore<false>,
    pub(crate) alpha: bool,
    pub(crate) zero: bool,
    /// [`SmmaCore::push_period`] only: the last `n` values before the seed, oldest first.
    recent: VecDeque<f64>,
}

impl SmmaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        SmmaCore { rma: SmoothCore::wilder(n), alpha: false, zero: false, recent: VecDeque::new() }
    }

    /// [`SmmaCore::push`] (or, with `first`, [`SmmaCore::push_from_first`]) with this value's own
    /// length `m`, 1 up to the built `n`: each step weighs by `m`, and the seed (unless `zero` or
    /// `first`) is the mean of the last `m` values on the first value that has that many, the last
    /// `n` kept until then. With `m == n` on every value it IS `push`, bit for bit.
    #[inline]
    pub(crate) fn push_period(&mut self, x: f64, m: usize, first: bool) -> f64 {
        let r = &mut self.rma;
        (r.gain, r.keep) = (1.0 / m as f64, (m as f64 - 1.0) / m as f64);
        if first {
            return self.push_from_first(x);
        }
        if self.zero || r.i >= r.end {
            return self.push(x);
        }
        self.recent.push_back(x);
        if self.recent.len() > r.end {
            self.recent.pop_front();
        }
        if self.recent.len() < m {
            return f64::NAN;
        }
        r.i = r.end;
        // The sum `push` would have begun: from `+0.0` under `alpha`, `-0.0` otherwise.
        let start = if self.alpha { 0.0 } else { -0.0 };
        let sum = self.recent.range(self.recent.len() - m..).fold(start, |s, &v| s + v);
        r.previous = if self.alpha { sum / m as f64 } else { sum * (1.0 / m as f64) };
        r.previous
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        if self.zero {
            return self.push_zero(x);
        }
        if self.alpha {
            return self.push_alpha(x);
        }
        self.rma.push(x)
    }

    /// [`SmmaCore::push`] under `zero`.
    #[inline]
    fn push_zero(&mut self, x: f64) -> f64 {
        let r = &mut self.rma;
        let previous = if r.previous.is_nan() { 0.0 } else { r.previous };
        r.previous = if self.alpha {
            r.gain * x + (1.0 - r.gain) * previous
        } else {
            previous * r.keep + x * r.gain
        };
        r.previous
    }

    /// [`SmmaCore::push`] under `alpha`.
    #[inline]
    fn push_alpha(&mut self, x: f64) -> f64 {
        let r = &mut self.rma;
        if r.i >= r.end {
            r.previous = r.gain * x + (1.0 - r.gain) * r.previous;
            return r.previous;
        }
        let i = r.i;
        r.i = i + 1;
        r.seed = if i == 0 { 0.0 } else { r.seed } + x;
        if i < r.last {
            return f64::NAN;
        }
        // `end` is `n` here (no value skipped); a period of 0 never gets this far.
        r.previous = r.seed / r.end as f64;
        r.previous
    }

    /// [`SmmaCore::push`] seeded with the FIRST value, which is then the first output; Wilder's
    /// recurrence runs from the second value on.
    #[inline(always)]
    pub(crate) fn push_from_first(&mut self, x: f64) -> f64 {
        if self.rma.i == 0 && !self.zero {
            // Past the seed ladder at once: every later value takes the steady recurrence.
            self.rma.i = self.rma.end;
            self.rma.previous = x;
            return x;
        }
        self.push(x)
    }
}

// ------------------------------------------------------------------------------------------------
// The directional movement family
// ------------------------------------------------------------------------------------------------

/// Which line of the directional movement system a [`DmCore`] answers.
pub(crate) const DX: u8 = 0;
pub(crate) const PLUS_DI: u8 = 1;
pub(crate) const MINUS_DI: u8 = 2;
pub(crate) const PLUS_DM: u8 = 3;
pub(crate) const MINUS_DM: u8 = 4;

/// Wilder's directional movement, one line of it: the smoothed sums of true range, +DM and -DM
/// (bars `1..=n` seed them as plain sums, later bars decay them, `s = s * (n-1)/n + x`), and the line
/// `K` picked from them.
///
/// Only the sums the line needs are carried, so `plus_dm` runs one recurrence and `dx` three.
/// `NaN` for the first `n` bars. `dx` is `100 * |+DM - -DM| / (+DM + -DM)`, the same number as
/// `100 * |+DI - -DI| / (+DI + -DI)` with the shared true-range factor cancelled, and `0` while the
/// range is not positive or both sums are zero.
///
/// The first `n + 1` bars take the slow path; after that `push` is the steady state.
#[derive(Clone)]
pub(crate) struct DmCore<const K: u8> {
    n: usize,
    keep: f64,
    /// Bars seen on the slow path, and the count at which the steady state starts.
    i: usize,
    end: usize,
    ph: f64,
    pl: f64,
    pc: f64,
    atr_s: f64,
    pdm_s: f64,
    mdm_s: f64,
}

impl<const K: u8> DmCore<K> {
    /// The true-range sum is needed by `dx` and by both DI lines.
    const TR: bool = K == DX || K == PLUS_DI || K == MINUS_DI;
    /// The +DM sum is needed by `dx`, `plus_di` and `plus_dm`.
    const P: bool = K == DX || K == PLUS_DI || K == PLUS_DM;
    /// The -DM sum is needed by `dx`, `minus_di` and `minus_dm`.
    const M: bool = K == DX || K == MINUS_DI || K == MINUS_DM;

    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        DmCore {
            n,
            keep: (n as f64 - 1.0) / n as f64,
            i: 0,
            // A period of 0 never leaves the slow path, which answers NaN.
            end: if n == 0 { usize::MAX } else { n + 1 },
            ph: f64::NAN,
            pl: f64::NAN,
            pc: f64::NAN,
            atr_s: -0.0,
            pdm_s: -0.0,
            mdm_s: -0.0,
        }
    }

    /// The line from the three sums.
    #[inline(always)]
    fn line(atr_s: f64, p: f64, m: f64) -> f64 {
        match K {
            DX => {
                let s = p + m;
                let d = 100.0 * (p - m).abs() / s;
                if atr_s > 0.0 && s > 0.0 { d } else { 0.0 }
            }
            PLUS_DI => {
                let v = p * (100.0 / atr_s);
                if atr_s > 0.0 { v } else { 0.0 }
            }
            MINUS_DI => {
                let v = m * (100.0 / atr_s);
                if atr_s > 0.0 { v } else { 0.0 }
            }
            PLUS_DM => p,
            _ => m,
        }
    }

    /// One bar. The close is read only by the lines that use the true range.
    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let up = h - self.ph;
        let down = self.pl - l;
        // `up > down && up > 0` as masked selects: which of the two moves is larger is a coin toss, and
        // a short-circuit `&&` would be a branch.
        let upp = if up > 0.0 { up } else { 0.0 };
        let dnp = if down > 0.0 { down } else { 0.0 };
        let pdm = if up > down { upp } else { 0.0 };
        let mdm = if down > up { dnp } else { 0.0 };
        let tr = if Self::TR { true_range(h, l, self.pc) } else { 0.0 };
        self.ph = h;
        self.pl = l;
        self.pc = c;
        if self.i >= self.end {
            if Self::TR {
                self.atr_s = self.atr_s * self.keep + tr;
            }
            if Self::P {
                self.pdm_s = self.pdm_s * self.keep + pdm;
            }
            if Self::M {
                self.mdm_s = self.mdm_s * self.keep + mdm;
            }
            return Self::line(self.atr_s, self.pdm_s, self.mdm_s);
        }
        let i = self.i;
        self.i = i + 1;
        // Bar 0 has no previous bar to difference; a period of 0 has no line.
        if self.n == 0 || i == 0 {
            return f64::NAN;
        }
        // Bars 1..=n are summed plainly: the seed is the sum of bars 1..=n, this bar included.
        if Self::TR {
            self.atr_s += tr;
        }
        if Self::P {
            self.pdm_s += pdm;
        }
        if Self::M {
            self.mdm_s += mdm;
        }
        if i < self.n {
            return f64::NAN;
        }
        Self::line(self.atr_s, self.pdm_s, self.mdm_s)
    }
}

// ------------------------------------------------------------------------------------------------
// Chaikin volatility
// ------------------------------------------------------------------------------------------------

/// Chaikin volatility: the percentage change over `roc` bars of the EMA (over `ema` bars) of the
/// high-low range. `NaN` until the EMA and the value it is compared with exist, and where that
/// earlier EMA is exactly zero.
///
/// The ring of smoothed ranges starts as `NaN`, so the first `roc` values come out `NaN` by arithmetic.
/// A lag of 0 is read as 1 (the registry range starts at 1).
#[derive(Clone)]
pub(crate) struct ChaikinCore {
    ema: SmoothCore<false>,
    buffer: Vec<f64>,
    head: usize,
}

impl ChaikinCore {
    #[inline]
    pub(crate) fn new(ema: usize, roc: usize) -> Self {
        ChaikinCore {
            ema: SmoothCore::ema(ema),
            buffer: vec![f64::NAN; roc.clamp(1, RING_CAP)],
            head: 0,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64) -> f64 {
        let smooth = self.ema.push(h - l);
        let old = std::mem::replace(&mut self.buffer[self.head], smooth);
        self.head += 1;
        if self.head == self.buffer.len() {
            self.head = 0;
        }
        // The division runs for every value and the zero test only selects (a `NaN` on either side
        // is a `NaN` result by itself).
        let r = (smooth - old) / old * 100.0;
        if old != 0.0 { r } else { f64::NAN }
    }
}

// ------------------------------------------------------------------------------------------------
// Chandelier exit, STARC bands
// ------------------------------------------------------------------------------------------------

/// Chandelier exit: `[highest high - mult * ATR, lowest low + mult * ATR]` over the last `n` bars,
/// the ATR Wilder's with the first range `high - low`. `NaN` for the first `n - 1` bars.
///
/// `closes` takes the extremes of the closes instead; `ratchet` carries each stop the last bar left
/// (`nz`'d to this bar's) while the last close stayed beyond it, the long stop up and the short one
/// down. `up` is the trend either way: up after a close over the last bar's short stop, down after
/// one under its long stop, else held, up from the first bar.
#[derive(Clone)]
pub(crate) struct ChandelierCore {
    win: HiLo,
    atr: AtrG<false>,
    mult: f64,
    pub(crate) closes: bool,
    pub(crate) ratchet: bool,
    long: f64,
    short: f64,
    pub(crate) up: bool,
}

impl ChandelierCore {
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        ChandelierCore {
            win: HiLo::new(n),
            atr: AtrG::new(n),
            mult,
            closes: false,
            ratchet: false,
            long: f64::NAN,
            short: f64::NAN,
            up: true,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 2] {
        let pc = self.atr.pc;
        let [hh, ll] = if self.closes { self.win.push(c, c) } else { self.win.push(h, l) };
        // A window with no number in it has no extreme: `HiLo` answers its empty identities, which are
        // not a price. `NaN`, the crate's "no value", is what comes out.
        let hh = if hh == f64::NEG_INFINITY { f64::NAN } else { hh };
        let ll = if ll == f64::INFINITY { f64::NAN } else { ll };
        let m = self.mult * self.atr.push(h, l, c);
        let (mut long, mut short) = (hh - m, ll + m);
        let long_previous = if self.long.is_nan() { long } else { self.long };
        let short_previous = if self.short.is_nan() { short } else { self.short };
        // A `NaN` stop stays `NaN`: the carry never stands in for a stop that has no value.
        if self.ratchet && pc > long_previous && !long.is_nan() {
            long = long.max(long_previous);
        }
        if self.ratchet && pc < short_previous && !short.is_nan() {
            short = short.min(short_previous);
        }
        self.up = if c > short_previous {
            true
        } else if c < long_previous {
            false
        } else {
            self.up
        };
        (self.long, self.short) = (long, short);
        [long, short]
    }
}

/// STARC bands: the mean of the last `sma` closes and `mult` ATRs either side, `[upper, mid, lower]`;
/// all `NaN` until both the mean and the ATR exist.
#[derive(Clone)]
pub(crate) struct StarcCore {
    mean: SmaCore,
    atr: AtrG<false>,
    mult: f64,
}

impl StarcCore {
    #[inline]
    pub(crate) fn new(sma: usize, atr: usize, mult: f64) -> Self {
        StarcCore { mean: SmaCore::new(sma), atr: AtrG::new(atr), mult }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 3] {
        let mid = self.mean.push(c);
        let m = self.mult * self.atr.push(h, l, c);
        let upper = mid + m;
        // A `NaN` in either the mean or the ATR takes the whole triple with it.
        if upper.is_nan() { [f64::NAN; 3] } else { [upper, mid, mid - m] }
    }
}

// ------------------------------------------------------------------------------------------------
// Supertrend
// ------------------------------------------------------------------------------------------------

/// Supertrend: ATR bands that only tighten while the trend holds. `[supertrend, direction]` with the
/// direction `1.0` up and `-1.0` down, both `NaN` until the ATR exists (bar `n`).
///
/// The first bar with an ATR seeds both bands from its own midpoint and the direction from the close
/// against it; after that the upper band is carried down (and the lower one up) while the previous
/// close stayed on its side of it, and the direction flips when the close crosses the opposite band.
/// The carry rules are selects, so the loop-carried part is a compare and a select. A bar whose ATR is
/// `NaN` leaves the state untouched.
///
/// `SKIP` is the registry row's start. Without it (`crate::indicators::Supertrend::with_down_start`)
/// the ATR's first range is `high - low` (the line starts at bar `n - 1`), the direction is down
/// from the first bar, the first bar with an ATR carries its bands from 0 (the bands the bar before
/// lacked) by the same rules, a missing last close breaking neither, and reads down, and a bar after
/// one whose bands met reads the trend as down (its line was the upper band), unless `held`
/// (`crate::indicators::Supertrend::with_held_direction`): then the trend is the one the last bar left.
///
/// `on_trend` (`crate::indicators::Supertrend::with_carry_on_trend`): the upper band is carried while
/// the last bar's trend was down and the lower one while it was up, whatever the last close; with
/// `SKIP` the first bar with an ATR reads up unless its close is under its own lower band.
///
/// [`SupertrendCore::push_centred`] runs the same law on a price, a centre and an ATR the caller gives.
#[derive(Clone)]
pub(crate) struct SupertrendCore<const SKIP: bool> {
    atr: AtrG<SKIP>,
    /// Set per bar by `crate::indicators::Supertrend::next_centred_with_mult`.
    pub(crate) mult: f64,
    started: bool,
    up: bool,
    fu: f64,
    fl: f64,
    pub(crate) held: bool,
    pub(crate) on_trend: bool,
    /// [`SupertrendCore::push_centred`] only: the last price it was given.
    last: f64,
}

impl<const SKIP: bool> SupertrendCore<SKIP> {
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        SupertrendCore {
            atr: AtrG::new(n),
            mult,
            started: false,
            up: false,
            fu: f64::NAN,
            fl: f64::NAN,
            held: false,
            on_trend: false,
            last: f64::NAN,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 2] {
        let pc = self.atr.pc;
        let atr = self.atr.push(h, l, c);
        self.step(pc, c, (h + l) * 0.5, atr)
    }

    /// [`SupertrendCore::push`] with the close, the midpoint and the ATR given: `price` is judged
    /// and carried as the close is, the bands sit `mult * atr` about `centre`, and the core's own ATR
    /// does not move.
    #[inline]
    pub(crate) fn push_centred(&mut self, price: f64, centre: f64, atr: f64) -> [f64; 2] {
        let pc = std::mem::replace(&mut self.last, price);
        self.step(pc, price, centre, atr)
    }

    /// One bar of the law: `pc` the last close, `c` this one, `hl2` the centre the bands sit about.
    #[inline(always)]
    fn step(&mut self, pc: f64, c: f64, hl2: f64, atr: f64) -> [f64; 2] {
        if atr.is_nan() {
            return [f64::NAN, if SKIP { f64::NAN } else { -1.0 }];
        }
        let band = self.mult * atr;
        let bu = hl2 + band;
        let bl = hl2 - band;
        if !self.started && !SKIP {
            // The bands the bar before lacked read 0: the upper band stays 0 unless it is under 0 or
            // the last close was over 0 (false for a missing close), the lower band the mirror.
            self.started = true;
            self.fu = if bu < 0.0 || pc > 0.0 { bu } else { 0.0 };
            self.fl = if bl > 0.0 || pc < 0.0 { bl } else { 0.0 };
            self.up = false;
        } else if !self.started {
            // The first bar with an ATR seeds the bands from its own midpoint.
            self.started = true;
            self.fu = bu;
            self.fl = bl;
            // Under `on_trend` the trend starts up, so only a close under the lower band turns it.
            let under = c < bl;
            self.up = if self.on_trend { !under } else { c >= hl2 };
        } else {
            // Without `SKIP` the last line was the upper band, so the trend down, when the bands met
            // (unless `held`).
            let was_up = self.up && (SKIP || self.held || self.fl != self.fu);
            // Upper band: tighten only while the previous close stayed at or under it (under
            // `on_trend`, while the trend was down). Both compares are selects; a `NaN` band leaves
            // the carried one (as `f64::min` does).
            let tight = if bu < self.fu { bu } else { self.fu };
            let carry = if self.on_trend { !was_up } else { pc <= self.fu };
            self.fu = if carry { tight } else { bu };
            // Lower band: ratchet only up while the previous close stayed at or over it (under
            // `on_trend`, while the trend was up).
            let tight = if bl > self.fl { bl } else { self.fl };
            let carry = if self.on_trend { was_up } else { pc >= self.fl };
            self.fl = if carry { tight } else { bl };
            let crossed_down = c < self.fl;
            let crossed_up = c > self.fu;
            self.up = if was_up { !crossed_down } else { crossed_up };
        }
        if self.up { [self.fl, 1.0] } else { [self.fu, -1.0] }
    }
}

/// The Supertrend judged against the PREVIOUS bar's bands
/// (`crate::indicators::Supertrend::with_previous_bands`): the ATR's first range is `high - low`, the
/// trend is up from the first bar, each band is carried while the last close was strictly beyond it
/// (`f64::max` / `f64::min`, so a band with no value takes the carried one), and the close is judged
/// against the bands the last bar left, the upper one first. `[supertrend, direction]`, the line
/// `NaN` until the ATR exists, the direction never.
///
/// `gated` (`crate::indicators::Supertrend::with_gated_turns`): a band the last bar lacked reads as
/// this bar's, and the close is judged against the band the trend would cross only: an up trend
/// turns down on a close under the last lower band, a down trend up on one over the last upper band.
/// `on_close` (`crate::indicators::Supertrend::with_carry_on_close`): each band is carried while THIS
/// close is beyond the last band, not the last close.
/// `on_trend` (`crate::indicators::Supertrend::with_carry_on_trend`): a band the last bar lacked reads
/// as this bar's, the turn is the ungated one, and after it the lower band is carried while the trend
/// is up, the upper one while it is down, whatever the close (`gated` and `on_close` read nothing).
/// [`PreviousBandsCore::push_centred`] runs the law on a price, a centre and an ATR the caller gives.
#[derive(Clone)]
pub(crate) struct PreviousBandsCore {
    atr: AtrG<false>,
    /// Set per bar by `crate::indicators::Supertrend::next_centred_with_mult`.
    pub(crate) mult: f64,
    up: bool,
    fu: f64,
    fl: f64,
    pub(crate) gated: bool,
    pub(crate) on_close: bool,
    pub(crate) on_trend: bool,
    /// [`PreviousBandsCore::push_centred`] only: the last price it was given.
    last: f64,
}

impl PreviousBandsCore {
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        PreviousBandsCore {
            atr: AtrG::new(n),
            mult,
            up: true,
            fu: f64::NAN,
            fl: f64::NAN,
            gated: false,
            on_close: false,
            on_trend: false,
            last: f64::NAN,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 2] {
        let pc = self.atr.pc;
        let atr = self.atr.push(h, l, c);
        self.step(pc, c, (h + l) * 0.5, atr)
    }

    /// [`PreviousBandsCore::push`] with the close, the midpoint and the ATR given, as
    /// [`SupertrendCore::push_centred`] is.
    #[inline]
    pub(crate) fn push_centred(&mut self, price: f64, centre: f64, atr: f64) -> [f64; 2] {
        let pc = std::mem::replace(&mut self.last, price);
        self.step(pc, price, centre, atr)
    }

    /// One bar of the law: `pc` the last close, `c` this one, `hl2` the centre the bands sit about.
    #[inline(always)]
    fn step(&mut self, pc: f64, c: f64, hl2: f64, atr: f64) -> [f64; 2] {
        // The price the bands carry on: the last close, or under `on_close` this one.
        let pc = if self.on_close { c } else { pc };
        let band = self.mult * atr;
        let (bu, bl) = (hl2 + band, hl2 - band);
        if self.on_trend {
            let fu = if self.fu.is_nan() { bu } else { self.fu };
            let fl = if self.fl.is_nan() { bl } else { self.fl };
            self.up = if c > fu {
                true
            } else if c < fl {
                false
            } else {
                self.up
            };
            self.fl = if self.up { bl.max(fl) } else { bl };
            self.fu = if self.up { bu } else { bu.min(fu) };
            return if self.up { [self.fl, 1.0] } else { [self.fu, -1.0] };
        }
        if self.gated {
            let fu = if self.fu.is_nan() { bu } else { self.fu };
            let fl = if self.fl.is_nan() { bl } else { self.fl };
            self.up = if !self.up && c > fu {
                true
            } else if self.up && c < fl {
                false
            } else {
                self.up
            };
            self.fl = if pc > fl { bl.max(fl) } else { bl };
            self.fu = if pc < fu { bu.min(fu) } else { bu };
            return if self.up { [self.fl, 1.0] } else { [self.fu, -1.0] };
        }
        self.up = if c > self.fu {
            true
        } else if c < self.fl {
            false
        } else {
            self.up
        };
        self.fl = if pc > self.fl { bl.max(self.fl) } else { bl };
        self.fu = if pc < self.fu { bu.min(self.fu) } else { bu };
        if self.up { [self.fl, 1.0] } else { [self.fu, -1.0] }
    }
}

// ------------------------------------------------------------------------------------------------
// ADXR, NATR
// ------------------------------------------------------------------------------------------------

/// ADX rating: the mean of the ADX line and its value `n` bars earlier. `NaN` until both exist
/// (bar `3n - 1`).
///
/// The ring of ADX values starts as `NaN`, so the warm-up is arithmetic.
#[derive(Clone)]
pub(crate) struct AdxrCore {
    adx: AdxCore,
    buffer: Vec<f64>,
    head: usize,
}

impl AdxrCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        AdxrCore { adx: AdxCore::new(n), buffer: vec![f64::NAN; n.clamp(1, RING_CAP)], head: 0 }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let adx = self.adx.push(h, l, c)[0];
        let old = std::mem::replace(&mut self.buffer[self.head], adx);
        self.head += 1;
        if self.head == self.buffer.len() {
            self.head = 0;
        }
        (adx + old) * 0.5
    }
}

/// Normalized ATR: `100 * ATR / close`, the ATR Wilder's over the ranges of bars `1..=n` (the first
/// bar has no previous close). `NaN` for the first `n` bars and on a bar that closes at exactly zero.
#[derive(Clone)]
pub(crate) struct NatrCore {
    atr: AtrG<true>,
}

impl NatrCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        NatrCore { atr: AtrG::new(n) }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let a = self.atr.push(h, l, c);
        let v = 100.0 * a / c;
        if c != 0.0 { v } else { f64::NAN }
    }
}

/// `typed_new!` with an `#[inline]` constructor, for the types of this group.
///
/// A constructor that is not `#[inline]` is an out-of-line call across the crate boundary: the struct
/// it returns is built through a pointer the caller can no longer see through, so a caller's loop
/// keeps the state in memory (and a literal period is not a constant to the loop). Inlined, the
/// state lives in registers (measured on `smma`: 3.0 against 1.5 ns a value).
macro_rules! typed_new_inline {
    ($Name:ident, $key:literal, [ $( $pf:ident = $pdef:expr ),+ ]) => {
        impl $Name {
            /// Build with these parameters, in the order the registry row lists them. A value the
            /// registry would not accept (out of range, or `NaN`) is a [`crate::IndicatorParamError`] naming
            /// it; [`crate::make_with`] clamps instead. `Default` gives the registry defaults.
            #[inline]
            pub fn new( $( $pf: f64 ),+ ) -> Result<Self, $crate::IndicatorParamError> {
                let raw: &[f64] = &[ $( $pf ),+ ];
                $crate::typed::checked($key, raw)?;
                Ok(Self::with_params(raw))
            }
        }
        #[cfg(test)]
        #[test]
        #[allow(non_snake_case, clippy::allow_attributes)]
        fn $Name() {
            let typed = <$Name>::new( $( $pdef ),+ ).expect("the defaults are inside the registry range");
            $crate::typed::tests::assert_matches_registry($key, Box::new(typed));
        }
    };
}
pub(crate) use typed_new_inline;
