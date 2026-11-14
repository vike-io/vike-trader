//! Fast cores for group `c` of the second wave of indicators (see `_followups/perf-conv2-brief.md`):
//! `roc`, `obv`, `psar`, `ad`, `adx` and `stochrsi`.
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds
//! no `&mut self` call in its hot part, and is used by BOTH the streaming stepper and the
//! whole-series functions, so a stream and a batch agree bit for bit.
//!
//! The recurrences multiply by a precomputed `1/n` or `(n-1)/n` instead of dividing in the steady
//! state, so the last bits differ from the textbook division form by the rounding noise any such
//! rewrite has (`tests/fast/fast_c.rs` holds them to a relative 1e-9).

use crate::roll::{RsiCore, SmaCore};
use vike_marketdata::Bar;

// ------------------------------------------------------------------------------------------------
// ROC
// ------------------------------------------------------------------------------------------------

/// Rate of change in percent over `n` values: `100 * (c - c[n back]) / c[n back]`, `NaN` while the
/// value `n` back does not exist yet and where it is exactly zero.
///
/// The ring starts as `NaN`, so the first `n` values come out `NaN` by arithmetic and the hot path
/// has no warm-up branch. A `NaN` close is simply `NaN` while it is the newest or the oldest value of
/// a pair: nothing is carried, so there is nothing to recover.
#[derive(Clone)]
pub(crate) struct RocCore {
    buffer: Vec<f64>,
    head: usize,
}

impl RocCore {
    /// A zero lag is not an indicator (the registry range starts at 1); it is read as 1.
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        RocCore { buffer: vec![f64::NAN; n.max(1)], head: 0 }
    }

    pub(crate) fn clear(&mut self) {
        self.buffer.fill(f64::NAN);
        self.head = 0;
    }

    /// The one expression: the whole-series loops evaluate it on two slice reads, the stream on a
    /// ring read, so they agree bit for bit.
    #[inline(always)]
    pub(crate) fn one(c: f64, back: f64) -> f64 {
        // The division is evaluated for every value and the zero test only selects: a division behind
        // a branch would keep a whole-series loop from using packed divisions.
        let r = 100.0 * (c - back) / back;
        if back != 0.0 { r } else { f64::NAN }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        let back = std::mem::replace(&mut self.buffer[self.head], c);
        self.head += 1;
        if self.head == self.buffer.len() {
            self.head = 0;
        }
        Self::one(c, back)
    }
}

/// The ROC column of `close`: `NaN` for the first `n` values, then `RocCore::one` of each value and the
/// one `n` back. Built by `extend` over a zip, so every element is written once (no fill, no second
/// pass) and the loop is free to use packed divisions.
#[inline]
pub(crate) fn roc_vec(n: usize, close: &[f64]) -> Vec<f64> {
    let n = n.max(1);
    let mut out = Vec::with_capacity(close.len());
    out.resize(n.min(close.len()), f64::NAN);
    if close.len() > n {
        out.extend(close[n..].iter().zip(close).map(|(&c, &back)| RocCore::one(c, back)));
    }
    out
}

/// The ROC column of `bars`, the stepper's `vectorize`.
#[inline]
pub(crate) fn roc_bars(n: usize, bars: &[Bar]) -> Vec<Vec<f64>> {
    let n = n.max(1);
    let mut out = Vec::with_capacity(bars.len());
    out.resize(n.min(bars.len()), f64::NAN);
    if bars.len() > n {
        out.extend(bars[n..].iter().zip(bars).map(|(b, back)| RocCore::one(b.close, back.close)));
    }
    vec![out]
}

// ------------------------------------------------------------------------------------------------
// OBV
// ------------------------------------------------------------------------------------------------

/// On-balance volume: a running sum of volume, added on an up-close and subtracted on a down-close.
///
/// The previous close starts as `NaN`, so the first bar compares false both ways and adds `0.0`:
/// the first value is `0.0` with no first-bar branch. A `NaN` close adds nothing, on that bar and
/// on the next one (its comparison with the carried `NaN` is false too).
#[derive(Clone)]
pub(crate) struct ObvCore {
    previous: f64,
    obv: f64,
}

impl ObvCore {
    #[inline]
    pub(crate) fn new() -> Self {
        ObvCore { previous: f64::NAN, obv: 0.0 }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64, v: f64) -> f64 {
        // Two masked terms instead of a three-way branch: the sign of a close-to-close move is
        // unpredictable, and a mispredicted branch costs more than the whole recurrence.
        let d =
            (if c > self.previous { v } else { 0.0 }) - (if c < self.previous { v } else { 0.0 });
        self.previous = c;
        self.obv += d;
        self.obv
    }
}

/// The OBV column of `bars`, the stepper's `vectorize`.
#[inline]
pub(crate) fn obv_bars(bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut core = ObvCore::new();
    let mut out = vec![f64::NAN; bars.len()];
    for (o, b) in out.iter_mut().zip(bars) {
        *o = core.push(b.close, b.volume);
    }
    vec![out]
}

// ------------------------------------------------------------------------------------------------
// A/D line
// ------------------------------------------------------------------------------------------------

/// Chaikin's accumulation/distribution line: a running sum of `clv * volume`, where
/// `clv = ((c - l) - (h - c)) / (h - l)` (`0.0` for a bar with no range).
///
/// The sum starts at `-0.0`, so adding the first flow gives that flow exactly (`-0.0 + x == x` for
/// every `x`, `-0.0` itself included) and there is no first-bar branch.
#[derive(Clone)]
pub(crate) struct AdCore {
    accumulator: f64,
}

impl AdCore {
    #[inline]
    pub(crate) fn new() -> Self {
        AdCore { accumulator: -0.0 }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    /// One bar's money-flow volume. Both ways in evaluate this one expression.
    #[inline(always)]
    fn flow(h: f64, l: f64, c: f64, v: f64) -> f64 {
        let rng = h - l;
        let clv = if rng != 0.0 { ((c - l) - (h - c)) / rng } else { 0.0 };
        clv * v
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64, v: f64) -> f64 {
        self.accumulator += Self::flow(h, l, c, v);
        self.accumulator
    }
}

/// The A/D column of `bars`, the stepper's `vectorize`.
#[inline]
pub(crate) fn ad_bars(bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut core = AdCore::new();
    let mut out = vec![f64::NAN; bars.len()];
    for (o, b) in out.iter_mut().zip(bars) {
        *o = core.push(b.high, b.low, b.close, b.volume);
    }
    vec![out]
}

// ------------------------------------------------------------------------------------------------
// Parabolic SAR
// ------------------------------------------------------------------------------------------------

/// Wilder's parabolic stop-and-reverse. The first bar assumes an up-trend from its low (value =
/// that low, extreme point = that high); every later bar moves the stop `af` of the way to the
/// extreme point, clamps it to the last two bars' lows (up-trend) or highs (down-trend), and
/// reverses when the bar crosses it; the reversal bar's stop (the old extreme) is lifted or dropped into the range of yesterday's and today's bars.
///
/// What keeps it short:
/// * The stop is `sar * (1 - af) + af * ep`: a multiply and an add on the loop-carried path (the
///   textbook `sar + af * (ep - sar)` carries a subtract as well).
/// * The acceleration factor is not carried. It is `min(step * k, max_af)` for the `k`-th extreme of
///   the trend (`step` for the first), and `k` is an integer that counts new extremes, so the float
///   chain has no `af = (af + step).min(max_af)` in it.
/// * The extreme point is a `max`/`min` (one instruction), and whether a bar made a new extreme is
///   a coin toss, so it is never a branch. A reversal is rare and mostly predicted: that one is.
/// * Only the side the trend needs is kept up to date: `m` is the lowest low (up-trend) or highest
///   high (down-trend) of the last two bars, and the other side is built from `pl1` / `ph1` at the
///   reversal. The first bar seeds both history slots, so there is no `i >= 2` test.
/// * The clamp is `if s < m { s } else { m }`, one `minsd` on the loop-carried path where `f64::min`
///   adds a `NaN` fix-up. It lets a `NaN` clamp through (two consecutive `NaN` lows make the stop
///   `NaN` for those bars; the next finite clamp replaces it), where `f64::min` ignored it.
///
/// Options, all off in [`PsarCore::new`]: a `start` factor for the first extreme of a trend apart
/// from `step` (the `k`-th extreme then carries `min(start + step * (k - 1), max_af)`); `test_first`,
/// which tests the reversal against the stop BEFORE it is clamped; `from_closes`, which starts the
/// trend on the SECOND bar, up when its close is above the first's; and `textbook`, which carries the
/// factor (`af = min(af + step, max_af)` on each new extreme) and moves the stop as
/// `sar + af * (ep - sar)`. Every option but `start` takes [`PsarCore::push_options`]. Apart from
/// those, `on_close` is a law of its own ([`PsarCore::push_on_close`]), optionally stepping by an
/// ATR (`atr`), and so is `extreme_first` ([`PsarCore::push_extreme_first`]).
#[derive(Clone)]
pub(crate) struct PsarCore {
    step: f64,
    max_af: f64,
    /// The first extreme's factor: `step` unless set apart.
    start: f64,
    /// `start - step`, added to `step * k` for a later extreme: `+0.0` unless `start` is set apart,
    /// and `x + 0.0` is `x` for every positive `x`, so the unset factor keeps its bits.
    start_off: f64,
    test_first: bool,
    from_closes: bool,
    textbook: bool,
    /// The reversal judged on the close ([`PsarCore::push_on_close`]); the three above do not apply.
    on_close: bool,
    /// `on_close` only: Wilder's ATR the stop steps by instead of the distance to the extreme.
    atr: Option<crate::roll::AtrCore>,
    /// `atr` only: the last bar's close, for the true range standing in while the ATR has no value.
    c1: f64,
    /// The bar's extreme and factor taken before the stop moves ([`PsarCore::push_extreme_first`]).
    extreme_first: bool,
    /// `extreme_first` only: the last bar reversed the trend, so this one's factor is `start`.
    flipped: bool,
    /// Any of the five above: past the first bar, [`PsarCore::push_options`] answers.
    options: bool,
    first: bool,
    /// `from_closes` only: the next bar starts the trend.
    second: bool,
    up: bool,
    sar: f64,
    ep: f64,
    /// The extreme of the current trend that `af` is for: 1 for the first, one more per new extreme.
    k: i64,
    /// `textbook` only: the factor, carried.
    af_c: f64,
    /// `from_closes` only: the first bar's close.
    pc: f64,
    /// Last bar's low and high.
    pl1: f64,
    ph1: f64,
    /// The low and high of the bar before that, kept by [`PsarCore::push_options`] only.
    pl2: f64,
    ph2: f64,
    /// The clamp: the lowest low of the last two bars in an up-trend, the highest high in a down-trend.
    m: f64,
}

impl PsarCore {
    #[inline]
    pub(crate) fn new(step: f64, max_af: f64) -> Self {
        PsarCore {
            step,
            max_af,
            start: step,
            start_off: 0.0,
            test_first: false,
            from_closes: false,
            textbook: false,
            on_close: false,
            atr: None,
            c1: f64::NAN,
            extreme_first: false,
            flipped: false,
            options: false,
            first: true,
            second: false,
            up: true,
            sar: 0.0,
            ep: 0.0,
            k: 1,
            af_c: step,
            pc: f64::NAN,
            pl1: 0.0,
            ph1: 0.0,
            pl2: 0.0,
            ph2: 0.0,
            m: 0.0,
        }
    }

    /// The first extreme of a trend carries `start` instead of `step`.
    pub(crate) fn set_start(&mut self, start: f64) {
        self.start = start;
        self.start_off = start - self.step;
    }

    /// The three options [`PsarCore::push_options`] reads (see the type).
    pub(crate) fn set_options(&mut self, test_first: bool, from_closes: bool, textbook: bool) {
        self.test_first = test_first;
        self.from_closes = from_closes;
        self.textbook = textbook;
        self.options = test_first | from_closes | textbook | self.on_close | self.extreme_first;
    }

    /// The bar's extreme and factor before the move ([`PsarCore::push_extreme_first`]).
    pub(crate) fn set_extreme_first(&mut self, extreme_first: bool) {
        self.extreme_first = extreme_first;
        self.set_options(self.test_first, self.from_closes, self.textbook);
    }

    /// `(test_first, from_closes, textbook)`.
    pub(crate) fn options(&self) -> (bool, bool, bool) {
        (self.test_first, self.from_closes, self.textbook)
    }

    /// The reversal judged on the close ([`PsarCore::push_on_close`]).
    pub(crate) fn set_on_close(&mut self, on_close: bool) {
        self.on_close = on_close;
        self.set_options(self.test_first, self.from_closes, self.textbook);
    }

    /// Under `on_close`, the stop steps by Wilder's ATR over `n` bars.
    pub(crate) fn set_atr(&mut self, n: usize) {
        self.atr = Some(crate::roll::AtrCore::new(n));
    }

    /// The first bar has no value: the trend starts on the second.
    pub(crate) fn starts_on_second(&self) -> bool {
        self.on_close | (self.from_closes & !self.extreme_first)
    }

    /// The trend after the last bar, `None` before one has started.
    pub(crate) fn trend(&self) -> Option<bool> {
        (!self.first && !self.second).then_some(self.up)
    }

    /// The stop the next bar meets as this bar leaves it: moved, before that bar's reversal test and
    /// clamp (under `on_close`, the value itself). `NaN` before a trend has started, and under
    /// `extreme_first`, whose move waits for the next bar's own extreme.
    pub(crate) fn stop_ahead(&self) -> f64 {
        if self.first || self.second || (self.extreme_first && !self.on_close) {
            f64::NAN
        } else if self.on_close {
            self.sar
        } else {
            self.moved()
        }
    }

    pub(crate) fn clear(&mut self) {
        let mut fresh = Self::new(self.step, self.max_af);
        fresh.set_start(self.start);
        fresh.set_options(self.test_first, self.from_closes, self.textbook);
        fresh.set_on_close(self.on_close);
        fresh.set_extreme_first(self.extreme_first);
        fresh.atr = self.atr.clone();
        if let Some(atr) = fresh.atr.as_mut() {
            atr.clear();
        }
        *self = fresh;
    }

    /// The acceleration factor of the `k`-th extreme of the trend: the first carries `start` as it
    /// stands (even above `max_af`); later ones are capped.
    #[inline(always)]
    fn af(&self) -> f64 {
        if self.k == 1 {
            self.start
        } else {
            let t = self.start_off + self.step * self.k as f64;
            if t < self.max_af { t } else { self.max_af }
        }
    }

    /// [`PsarCore::push_hlc`] without a close, which only `from_closes` reads (a `NaN` close starts
    /// that trend down).
    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64) -> f64 {
        self.push_hlc(h, l, f64::NAN)
    }

    /// [`PsarCore::push_ohlc`] without an open, which only `on_close` reads (a `NaN` open starts
    /// that trend down).
    #[inline(always)]
    pub(crate) fn push_hlc(&mut self, h: f64, l: f64, c: f64) -> f64 {
        self.push_ohlc(f64::NAN, h, l, c)
    }

    #[inline(always)]
    pub(crate) fn push_ohlc(&mut self, o: f64, h: f64, l: f64, c: f64) -> f64 {
        if self.first {
            self.first = false;
            self.k = 1;
            self.af_c = self.start;
            self.pl1 = l;
            self.ph1 = h;
            if self.on_close {
                // No trend yet: the next bar starts it from this bar's body.
                self.atr_step(h, l, c);
                self.up = c > o;
                self.second = true;
                self.sar = f64::NAN;
                return f64::NAN;
            }
            if self.from_closes && !self.extreme_first {
                // No trend yet: the next bar starts it from the two closes. The bar before this one
                // does not exist, and `f64::min` / `f64::max` skip its `NaN` in the clamps.
                self.pl2 = f64::NAN;
                self.ph2 = f64::NAN;
                self.pc = c;
                self.second = true;
                self.sar = f64::NAN;
                return f64::NAN;
            }
            self.up = true;
            self.sar = l;
            self.ep = h;
            self.pl2 = l;
            self.ph2 = h;
            self.m = l;
            return l;
        }
        if self.options {
            return self.push_options(h, l, c);
        }
        let af = self.af();
        let mut s = self.sar * (1.0 - af) + af * self.ep;
        if self.up {
            s = if s < self.m { s } else { self.m };
            if l < s {
                self.up = false;
                // The old extreme becomes the stop, and a stop may not sit inside yesterday's or today's
                // range (TA-Lib clamps it so too): a bar that spiked above the extreme and then fell through
                // the stop would otherwise leave its dot below its own high.
                s = self.ep;
                if s < self.ph1 {
                    s = self.ph1;
                }
                if s < h {
                    s = h;
                }
                self.ep = l;
                self.k = 1;
                self.m = h.max(self.ph1);
            } else {
                self.k += i64::from(h > self.ep);
                self.ep = if h > self.ep { h } else { self.ep };
                self.m = l.min(self.pl1);
            }
        } else {
            s = if s > self.m { s } else { self.m };
            if h > s {
                self.up = true;
                s = self.ep;
                if s > self.pl1 {
                    s = self.pl1;
                }
                if s > l {
                    s = l;
                }
                self.ep = h;
                self.k = 1;
                self.m = l.min(self.pl1);
            } else {
                self.k += i64::from(l < self.ep);
                self.ep = if l < self.ep { l } else { self.ep };
                self.m = h.max(self.ph1);
            }
        }
        self.sar = s;
        self.pl1 = l;
        self.ph1 = h;
        s
    }

    /// [`PsarCore::push_hlc`] past the first bar under the options. The clamps and the reversal level
    /// use `f64::min` / `f64::max`, which skip a `NaN` (the bar before the first, under
    /// `from_closes`).
    ///
    /// * `from_closes`: the second bar starts the trend, up when its close is above the first's,
    ///   from the first bar's low (high) toward this bar's high (low), and is then stepped like any
    ///   bar (a trend's opening bar never makes a new extreme, its extreme being its own).
    /// * `test_first`: a bar that crosses the stop as it moved this bar reverses, even when the
    ///   clamp to the last two bars would have lifted (dropped) the stop past it; the reversal's stop
    ///   is the old extreme raised (lowered) to this bar's high (low) and to the last two bars'.
    ///   Unset, the clamp comes first and the reversal's stop meets this bar and the last, as in
    ///   `push`.
    /// * `textbook`: the stop moves as `sar + af * (ep - sar)` and the factor is carried, `start` at
    ///   a trend's opening, `min(af + step, max_af)` at each new extreme. Unset, `push`'s
    ///   expressions.
    #[inline(always)]
    fn push_options(&mut self, h: f64, l: f64, c: f64) -> f64 {
        if self.on_close {
            return self.push_on_close(h, l, c);
        }
        if self.extreme_first {
            return self.push_extreme_first(h, l);
        }
        if self.second {
            self.second = false;
            self.up = c > self.pc;
            self.ep = if self.up { h } else { l };
            self.sar = if self.up { self.pl1 } else { self.ph1 };
            self.m = if self.up { self.pl1.min(self.pl2) } else { self.ph1.max(self.ph2) };
        }
        let mut s = self.moved();
        if self.up {
            let clamped = s.min(self.m);
            if l < if self.test_first { s } else { clamped } {
                self.up = false;
                s = self.ep.max(h).max(self.ph1);
                if self.test_first {
                    s = s.max(self.ph2);
                }
                self.ep = l;
                self.k = 1;
                self.af_c = self.start;
                self.m = h.max(self.ph1);
            } else {
                s = clamped;
                if h > self.ep {
                    self.ep = h;
                    self.k += 1;
                    self.af_c = (self.af_c + self.step).min(self.max_af);
                }
                self.m = l.min(self.pl1);
            }
        } else {
            let clamped = s.max(self.m);
            if h > if self.test_first { s } else { clamped } {
                self.up = true;
                s = self.ep.min(l).min(self.pl1);
                if self.test_first {
                    s = s.min(self.pl2);
                }
                self.ep = h;
                self.k = 1;
                self.af_c = self.start;
                self.m = l.min(self.pl1);
            } else {
                s = clamped;
                if l < self.ep {
                    self.ep = l;
                    self.k += 1;
                    self.af_c = (self.af_c + self.step).min(self.max_af);
                }
                self.m = h.max(self.ph1);
            }
        }
        self.sar = s;
        self.pl2 = self.pl1;
        self.ph2 = self.ph1;
        self.pl1 = l;
        self.ph1 = h;
        s
    }

    /// The stop moved toward the extreme under `textbook` or not, before the clamp.
    #[inline(always)]
    fn moved(&self) -> f64 {
        if self.textbook {
            self.sar + self.af_c * (self.ep - self.sar)
        } else {
            let af = self.af();
            self.sar * (1.0 - af) + af * self.ep
        }
    }

    /// [`PsarCore::push_ohlc`] past the first bar under `on_close`, whose value is the level the NEXT
    /// bar's close is judged against. The second bar starts the trend (its direction fixed on the
    /// first bar by that bar's body) at the first bar's low (high), unmoved, its extreme this bar's.
    /// Later, a close at or through the last value reverses the trend: the value is then the old
    /// extreme, unmoved, and the factor `start`. Otherwise the extreme and the factor (carried, as
    /// `textbook` carries it) take this bar first and the value moves toward the extreme as
    /// `sar + af * (ep - sar)`, or by `af` times `atr`'s distance, with no clamp.
    #[inline(always)]
    fn push_on_close(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let d = self.atr_step(h, l, c);
        if self.second {
            self.second = false;
            self.sar = if self.up { self.pl1 } else { self.ph1 };
            self.ep = if self.up { h } else { l };
            return self.sar;
        }
        if if self.up { c <= self.sar } else { c >= self.sar } {
            self.up = !self.up;
            self.sar = self.ep;
            self.ep = if self.up { h } else { l };
            self.af_c = self.start;
            return self.sar;
        }
        if if self.up { h > self.ep } else { l < self.ep } {
            self.ep = if self.up { h } else { l };
            self.af_c = (self.af_c + self.step).min(self.max_af);
        }
        self.sar = match self.atr {
            None => self.sar + self.af_c * (self.ep - self.sar),
            Some(_) if self.up => self.sar + self.af_c * d,
            Some(_) => self.sar - self.af_c * d,
        };
        self.sar
    }

    /// [`PsarCore::push_ohlc`] past the first bar under `extreme_first`. The bar first takes its own
    /// extreme (`max(h, ep)` up, `min(l, ep)` down) and its factor (`start` on the bar after a
    /// reversal, else raised by `step` to at most `max_af` when the extreme moved), then moves the
    /// stop `sar + af * (ep - sar)` toward it and clamps it to the last two bars' lows (highs). A low
    /// under (high over) the clamped stop reverses the trend: the stop is the extreme just taken,
    /// the new extreme the lower low (higher high) of this bar and the last.
    #[inline(always)]
    fn push_extreme_first(&mut self, h: f64, l: f64) -> f64 {
        let previous_ep = self.ep;
        self.ep = if self.up { h.max(previous_ep) } else { l.min(previous_ep) };
        if self.flipped {
            self.af_c = self.start;
        } else if self.ep != previous_ep {
            self.af_c = (self.af_c + self.step).min(self.max_af);
        }
        let mut s = self.sar + self.af_c * (self.ep - self.sar);
        self.flipped = false;
        if self.up {
            s = s.min(self.pl1).min(self.pl2);
            if s > l {
                (self.up, self.flipped, s) = (false, true, self.ep);
                self.ep = l.min(self.pl1);
            }
        } else {
            s = s.max(self.ph1).max(self.ph2);
            if s < h {
                (self.up, self.flipped, s) = (true, true, self.ep);
                self.ep = h.max(self.ph1);
            }
        }
        self.sar = s;
        self.pl2 = self.pl1;
        self.ph2 = self.ph1;
        self.pl1 = l;
        self.ph1 = h;
        s
    }

    /// `atr`'s distance for this bar: the ATR, or this bar's true range while the ATR has none (the
    /// first bar's is its own range). `NaN` without `atr`.
    #[inline(always)]
    fn atr_step(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let Some(atr) = self.atr.as_mut() else { return f64::NAN };
        let a = atr.push(h, l, c);
        let tr = (h - l).max((h - self.c1).abs()).max((l - self.c1).abs());
        self.c1 = c;
        if a.is_nan() { tr } else { a }
    }
}

/// The SAR column of `bars`, the stepper's `vectorize`.
#[inline]
pub(crate) fn psar_bars(core: &PsarCore, bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut core = core.clone();
    core.clear();
    let mut out = vec![f64::NAN; bars.len()];
    for (o, b) in out.iter_mut().zip(bars) {
        *o = core.push_ohlc(b.open, b.high, b.low, b.close);
    }
    vec![out]
}

// ------------------------------------------------------------------------------------------------
// ADX
// ------------------------------------------------------------------------------------------------

/// Wilder's directional movement system: `[adx, plus_di, minus_di]`.
///
/// Bars `1..=n` seed the three Wilder sums (true range, +DM, -DM) as plain sums; later bars decay
/// them (`s = s * (n-1)/n + x`). The DI lines are `100 * dm / tr` from bar `n`; DX is
/// `100 * |+DI - -DI| / (+DI + -DI)`, and ADX is the mean of the first `n` DX values, then Wilder's
/// smoothing of DX. `NaN` before each line exists.
///
/// The first `2n` bars take the slow path (counters and seeds); after that `push` is the
/// branch-light steady state. The two DI lines share one reciprocal `100 / tr`.
///
/// A `NaN` price poisons the sums for good (a Wilder recurrence never forgets), exactly as before
/// this core: `tr > 0` is then false and the lines read `0`.
///
/// [`AdxCore::with_adx_period`] smooths the ADX over `m` DX values instead of `n` (the mean of the
/// first `m`, then Wilder's smoothing over `m`), the DI lines keeping `n`; [`AdxCore::new`] is
/// `m == n`, which runs the same expressions on the same constants as before the split.
///
/// `first_moves` (`crate::indicators::Adx::with_first_bar_moves`): the first bar's moves (0, there
/// being no bar before to beat) seed the two DM sums with bars `0..n`, so at bar `n` they decay once
/// where the range's sum, seeded with bars `1..=n`, does not.
///
/// `hold` (`crate::indicators::Adx::with_held_lines`): a bar whose range sum is not positive gives
/// the DI lines the last bar left (`held`) instead of 0; while there are none its DX is `NaN`, which
/// the ADX skips (`dx_base` and `end` move a bar on). `missing`
/// (`crate::indicators::Adx::with_missing_ratios`, overruled by `hold`): such a bar's DI lines are
/// `NaN`, and so is a DX whose DI lines sum to 0, skipped the same way.
///
/// `start` (`crate::indicators::Adx::with_first_bar_range`): 1 when the first bar counts as any
/// other (its moves 0, its true range `high - low`), so the counter starts as if a bar came before
/// and every sum seeds with bars `0..n` (`first_moves` then reads nothing). `dx_lead`
/// (`crate::indicators::Adx::with_dx_from_first_bar`, overruled by `hold` and `missing`): each bar
/// before the DI lines has a DX of 0, so the ADX averages from the first bar.
#[derive(Clone)]
pub(crate) struct AdxCore {
    n: usize,
    pub(crate) first_moves: bool,
    pub(crate) hold: bool,
    pub(crate) missing: bool,
    start: usize,
    dx_lead: bool,
    /// The counter value of the ADX's first DX (`n`, or `start` under `dx_lead`).
    dx_base: usize,
    /// The DI lines the last bar left, read under `hold` only.
    held: (f64, f64),
    /// The DI sums' decay `(n - 1) / n`.
    keep: f64,
    /// The ADX smoothing period and its two constants (`n`'s unless set apart).
    m: usize,
    inverse_m: f64,
    keep_m: f64,
    /// Bars seen on the slow path, and the count at which the steady state starts.
    i: usize,
    end: usize,
    ph: f64,
    pl: f64,
    pc: f64,
    atr_s: f64,
    pdm_s: f64,
    mdm_s: f64,
    dx_sum: f64,
    adx: f64,
}

impl AdxCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        Self::with_adx_period(n, n)
    }

    /// DI lines over `n` bars, the ADX smoothed over `m` DX values (at least 1).
    #[inline]
    pub(crate) fn with_adx_period(n: usize, m: usize) -> Self {
        let m = m.max(1);
        AdxCore {
            n,
            first_moves: false,
            hold: false,
            missing: false,
            start: 0,
            dx_lead: false,
            dx_base: n,
            held: (f64::NAN, f64::NAN),
            keep: (n as f64 - 1.0) / n as f64,
            m,
            inverse_m: 1.0 / m as f64,
            keep_m: (m as f64 - 1.0) / m as f64,
            i: 0,
            // A period of 0 never leaves the slow path, which answers NaN.
            end: if n == 0 { usize::MAX } else { n + m },
            ph: f64::NAN,
            pl: f64::NAN,
            pc: f64::NAN,
            atr_s: -0.0,
            pdm_s: -0.0,
            mdm_s: -0.0,
            dx_sum: -0.0,
            adx: f64::NAN,
        }
    }

    pub(crate) fn clear(&mut self) {
        let (first_moves, hold, missing, start, dx_lead) =
            (self.first_moves, self.hold, self.missing, self.start, self.dx_lead);
        let fresh = Self::with_adx_period(self.n, self.m);
        *self = Self { first_moves, hold, missing, start, dx_lead, ..fresh };
        self.i = start;
        if self.lead() {
            self.dx_base = start;
            // The sums decay in the steady state from counter `n + 1`, the ADX from its `m`-th DX.
            self.end = if self.n == 0 { usize::MAX } else { (self.n + 1).max(start + self.m) };
        }
    }

    /// `first_bar_range` and `dx_from_first_bar` (see the type), on a fresh core.
    pub(crate) fn set_start(&mut self, first_bar_range: bool, dx_from_first_bar: bool) {
        (self.start, self.dx_lead) = (usize::from(first_bar_range), dx_from_first_bar);
        self.clear();
    }

    /// `(first_bar_range, dx_from_first_bar)`.
    pub(crate) fn start(&self) -> (bool, bool) {
        (self.start == 1, self.dx_lead)
    }

    /// `dx_lead` in effect: `hold` and `missing` overrule it.
    #[inline(always)]
    pub(crate) fn lead(&self) -> bool {
        self.dx_lead && !self.hold && !self.missing
    }

    /// `(+DI, -DI, DX)` of this bar: [`AdxCore::lines`]; under `hold` with a range sum that is not
    /// positive, the DI lines the last bar left and their DX (`NaN` while there are none); under
    /// `missing`, `NaN` for what had nothing to divide by.
    #[inline(always)]
    fn read(&mut self) -> (f64, f64, f64) {
        let lines = Self::lines(self.atr_s, self.pdm_s, self.mdm_s);
        if !self.hold {
            if !self.missing || lines.0 + lines.1 > 0.0 {
                return lines;
            }
            let nan = f64::NAN;
            return if self.atr_s > 0.0 { (lines.0, lines.1, nan) } else { (nan, nan, nan) };
        }
        if self.atr_s > 0.0 {
            self.held = (lines.0, lines.1);
            return lines;
        }
        let (pdi, mdi) = self.held;
        let denominator = pdi + mdi;
        let dx = if denominator > 0.0 {
            100.0 * (pdi - mdi).abs() / denominator
        } else if denominator.is_nan() {
            f64::NAN
        } else {
            0.0
        };
        (pdi, mdi, dx)
    }

    /// The ADX after the DX of counter value `i`: `NaN` until the `m`-th DX after `dx_base`, then
    /// their mean, then Wilder's smoothing.
    #[inline(always)]
    fn adx_step(&mut self, i: usize, dx: f64) -> f64 {
        let k = i + 1 - self.dx_base;
        if k <= self.m {
            self.dx_sum += dx;
            if k < self.m {
                return f64::NAN;
            }
            self.adx = self.dx_sum * self.inverse_m;
        } else {
            self.adx = self.adx * self.keep_m + dx * self.inverse_m;
        }
        self.adx
    }

    /// `(+DI, -DI, DX)` from the three smoothed sums.
    #[inline(always)]
    fn lines(atr_s: f64, pdm_s: f64, mdm_s: f64) -> (f64, f64, f64) {
        let (pdi, mdi) = if atr_s > 0.0 {
            let r = 100.0 / atr_s;
            (pdm_s * r, mdm_s * r)
        } else {
            (0.0, 0.0)
        };
        let denominator = pdi + mdi;
        let dx = if denominator > 0.0 { 100.0 * (pdi - mdi).abs() / denominator } else { 0.0 };
        (pdi, mdi, dx)
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 3] {
        let up = h - self.ph;
        let down = self.pl - l;
        // `up > down && up > 0` as one masked select on `up > down` of `max(up, 0)`: a short-circuit `&&`
        // would be a branch, and which of the two moves is larger is a coin toss.
        let upp = if up > 0.0 { up } else { 0.0 };
        let dnp = if down > 0.0 { down } else { 0.0 };
        let pdm = if up > down { upp } else { 0.0 };
        let mdm = if down > up { dnp } else { 0.0 };
        let tr = (h - l).max((h - self.pc).abs()).max((l - self.pc).abs());
        self.ph = h;
        self.pl = l;
        self.pc = c;
        if self.i >= self.end {
            // Steady state: three decays, the DI/DX lines and the ADX decay, nothing else.
            self.atr_s = self.atr_s * self.keep + tr;
            self.pdm_s = self.pdm_s * self.keep + pdm;
            self.mdm_s = self.mdm_s * self.keep + mdm;
            let (pdi, mdi, dx) = self.read();
            self.adx = self.adx * self.keep_m + dx * self.inverse_m;
            return [self.adx, pdi, mdi];
        }
        let i = self.i;
        self.i = i + 1;
        // Bar 0 has no previous bar to difference; a period of 0 has no lines.
        if self.n == 0 {
            return [f64::NAN; 3];
        }
        if i < self.n {
            if i > 0 {
                self.atr_s += tr;
                self.pdm_s += pdm;
                self.mdm_s += mdm;
            }
            let adx = if self.lead() { self.adx_step(i, 0.0) } else { f64::NAN };
            return [adx, f64::NAN, f64::NAN];
        }
        if i == self.n && self.first_moves && self.start == 0 {
            // The DM seeds (bars 0..n) are complete a bar before the range's (bars 1..=n).
            self.atr_s += tr;
            self.pdm_s = self.pdm_s * self.keep + pdm;
            self.mdm_s = self.mdm_s * self.keep + mdm;
        } else if i == self.n {
            // The seed is the sum of bars 1..=n, this bar included; no decay yet.
            self.atr_s += tr;
            self.pdm_s += pdm;
            self.mdm_s += mdm;
        } else {
            self.atr_s = self.atr_s * self.keep + tr;
            self.pdm_s = self.pdm_s * self.keep + pdm;
            self.mdm_s = self.mdm_s * self.keep + mdm;
        }
        let (pdi, mdi, dx) = self.read();
        if dx.is_nan() && (self.hold || self.missing) {
            // No DI lines yet to hold, or nothing to divide by: this bar's DX is not one of the ADX's.
            self.dx_base += 1;
            self.end += 1;
            return [f64::NAN, pdi, mdi];
        }
        // DX number `i + 1 - dx_base`; unset the slow path ends at DX number `m`, the mean of them.
        [self.adx_step(i, dx), pdi, mdi]
    }
}

/// The directional movement system of the form that starts its sums at 0
/// (`crate::indicators::Adx::with_zero_seeded_sums`), `[adx, plus_di, minus_di]`: the bar before the
/// first reads as 0 (its high, low and close), so the first bar has moves and a true range; each of
/// the three sums runs `s - s / n + x` from 0; the DI lines are `sum / range_sum * 100`, DX is
/// `|+DI - -DI| / (+DI + -DI) * 100`, and the ADX is the plain mean of the last `m` DX values (a fresh
/// left fold, oldest first), a `NaN` DX skipped and counted in none. `NaN` until `m` DX values are in.
#[derive(Clone)]
pub(crate) struct ZeroSeededAdxCore {
    n: f64,
    /// The last bar's high, low and close.
    previous: (f64, f64, f64),
    /// The range, +DM and -DM sums.
    sums: (f64, f64, f64),
    dx: SmaCore,
}

impl ZeroSeededAdxCore {
    #[inline]
    pub(crate) fn new(n: usize, m: usize) -> Self {
        ZeroSeededAdxCore {
            n: n as f64,
            previous: (0.0, 0.0, 0.0),
            sums: (0.0, 0.0, 0.0),
            dx: SmaCore::new(m.max(1)),
        }
    }

    #[inline]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 3] {
        let (ph, pl, pc) = std::mem::replace(&mut self.previous, (h, l, c));
        let (up, down) = (h - ph, pl - l);
        let tr = (h - l).max((h - pc).abs()).max((l - pc).abs());
        let pdm = if up > down { up.max(0.0) } else { 0.0 };
        let mdm = if down > up { down.max(0.0) } else { 0.0 };
        let n = self.n;
        let smooth = |s: f64, x: f64| s - s / n + x;
        let (range, plus, minus) = self.sums;
        let (range, plus, minus) = (smooth(range, tr), smooth(plus, pdm), smooth(minus, mdm));
        self.sums = (range, plus, minus);
        let (pdi, mdi) = (plus / range * 100.0, minus / range * 100.0);
        let dx = (pdi - mdi).abs() / (pdi + mdi) * 100.0;
        if !dx.is_nan() {
            self.dx.push(dx);
        }
        [self.dx.fresh_mean(), pdi, mdi]
    }
}

/// The three ADX columns of `bars`, the stepper's `vectorize`.
#[inline]
pub(crate) fn adx_bars(core: &AdxCore, bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut core = core.clone();
    core.clear();
    let len = bars.len();
    let mut cols = [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, p, m] = &mut cols;
    for (((x, y), z), b) in a.iter_mut().zip(p.iter_mut()).zip(m.iter_mut()).zip(bars) {
        let v = core.push(b.high, b.low, b.close);
        *x = v[0];
        *y = v[1];
        *z = v[2];
    }
    Vec::from(cols)
}

// ------------------------------------------------------------------------------------------------
// Stochastic RSI
// ------------------------------------------------------------------------------------------------

/// Stochastic RSI: Wilder's RSI of the close, `%K` = that RSI placed inside its own `k`-value
/// range (`0` where the range is flat), `%D` = the mean of the last `d` defined `%K`. `[%K, %D]`.
///
/// `%K` is `NaN` until `rsi_p + k - 1` values are in and wherever the RSI window holds a `NaN`:
/// the RSI ring starts as `NaN` and a counter of `NaN` entries makes warm-up and recovery one test.
/// The window's highest and lowest are carried and rescanned (cold, over a shared slice) only when
/// the value that leaves was one of them, or after a `NaN` was inside. `%D` runs [`SmaCore`] over
/// the defined `%K` values only.
#[derive(Clone)]
pub(crate) struct StochRsiCore {
    rsi: RsiCore,
    buffer: Vec<f64>,
    head: usize,
    /// `NaN` entries in `buffer`.
    bad: usize,
    /// `hh` and `ll` do not describe `buffer` (start, or a `NaN` was inside).
    stale: bool,
    hh: f64,
    ll: f64,
    smooth: SmaCore,
}

/// `(highest, lowest)` of the window, the same folds the textbook kernels use.
#[cold]
#[inline(never)]
fn extremes(buffer: &[f64]) -> (f64, f64) {
    (
        buffer.iter().copied().fold(f64::MIN, f64::max),
        buffer.iter().copied().fold(f64::MAX, f64::min),
    )
}

impl StochRsiCore {
    #[inline]
    pub(crate) fn new(rsi_p: usize, k: usize, d: usize) -> Self {
        let k = k.max(1);
        StochRsiCore {
            rsi: RsiCore::new(rsi_p),
            buffer: vec![f64::NAN; k],
            head: 0,
            bad: k,
            stale: true,
            hh: f64::NAN,
            ll: f64::NAN,
            smooth: SmaCore::new(d),
        }
    }

    /// `crate::indicators::Stochrsi::with_rs_form`.
    pub(crate) fn set_rs_form(&mut self, rs_form: bool) {
        self.rsi.set_rs_form(rs_form);
    }

    pub(crate) fn clear(&mut self) {
        self.rsi.clear();
        self.buffer.fill(f64::NAN);
        self.head = 0;
        self.bad = self.buffer.len();
        self.stale = true;
        self.smooth.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> [f64; 2] {
        let r = self.rsi.push(c);
        let old = std::mem::replace(&mut self.buffer[self.head], r);
        self.head += 1;
        if self.head == self.buffer.len() {
            self.head = 0;
        }
        self.bad = self.bad + usize::from(r.is_nan()) - usize::from(old.is_nan());
        if self.bad != 0 {
            self.stale = true;
            return [f64::NAN, f64::NAN];
        }
        if self.stale || old == self.hh || old == self.ll {
            let (hh, ll) = extremes(&self.buffer);
            self.hh = hh;
            self.ll = ll;
            self.stale = false;
        } else {
            self.hh = self.hh.max(r);
            self.ll = self.ll.min(r);
        }
        let rng = self.hh - self.ll;
        let k = if rng != 0.0 { 100.0 * (r - self.ll) / rng } else { 0.0 };
        [k, self.smooth.push(k)]
    }
}

/// The `%K` and `%D` columns of `bars`, the stepper's `vectorize`.
#[inline]
pub(crate) fn stochrsi_bars(core: &StochRsiCore, bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut core = core.clone();
    core.clear();
    let len = bars.len();
    let mut cols = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [k, d] = &mut cols;
    for ((x, y), b) in k.iter_mut().zip(d.iter_mut()).zip(bars) {
        let v = core.push(b.close);
        *x = v[0];
        *y = v[1];
    }
    Vec::from(cols)
}
