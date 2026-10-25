//! Fast cores for group `c` of the second wave of indicators (see `_followups/perf-conv2-brief.md`):
//! `roc`, `obv`, `psar`, `ad`, `adx` and `stochrsi`.
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds
//! no `&mut self` call in its hot part, and is used by BOTH the streaming stepper and the
//! whole-series functions, so a stream and a batch agree bit for bit.
//!
//! The recurrences multiply by a precomputed `1/n` or `(n-1)/n` instead of dividing in the steady
//! state, so the last bits differ from the textbook division form by the rounding noise any such
//! rewrite has (`tests/fast_c.rs` holds them to a relative 1e-9).

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
    buf: Vec<f64>,
    head: usize,
}

impl RocCore {
    /// A zero lag is not an indicator (the registry range starts at 1); it is read as 1.
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        RocCore { buf: vec![f64::NAN; n.max(1)], head: 0 }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(f64::NAN);
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
        let back = std::mem::replace(&mut self.buf[self.head], c);
        self.head += 1;
        if self.head == self.buf.len() {
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
    prev: f64,
    obv: f64,
}

impl ObvCore {
    #[inline]
    pub(crate) fn new() -> Self {
        ObvCore { prev: f64::NAN, obv: 0.0 }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64, v: f64) -> f64 {
        // Two masked terms instead of a three-way branch: the sign of a close-to-close move is
        // unpredictable, and a mispredicted branch costs more than the whole recurrence.
        let d = (if c > self.prev { v } else { 0.0 }) - (if c < self.prev { v } else { 0.0 });
        self.prev = c;
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
    acc: f64,
}

impl AdCore {
    #[inline]
    pub(crate) fn new() -> Self {
        AdCore { acc: -0.0 }
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
        self.acc += Self::flow(h, l, c, v);
        self.acc
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
#[derive(Clone)]
pub(crate) struct PsarCore {
    step: f64,
    max_af: f64,
    first: bool,
    up: bool,
    sar: f64,
    ep: f64,
    /// The extreme of the current trend that `af` is for: 1 for the first, one more per new extreme.
    k: i64,
    /// Last bar's low and high.
    pl1: f64,
    ph1: f64,
    /// The clamp: the lowest low of the last two bars in an up-trend, the highest high in a down-trend.
    m: f64,
}

impl PsarCore {
    #[inline]
    pub(crate) fn new(step: f64, max_af: f64) -> Self {
        PsarCore {
            step,
            max_af,
            first: true,
            up: true,
            sar: 0.0,
            ep: 0.0,
            k: 1,
            pl1: 0.0,
            ph1: 0.0,
            m: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new(self.step, self.max_af);
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64) -> f64 {
        if self.first {
            self.first = false;
            self.up = true;
            self.sar = l;
            self.ep = h;
            self.k = 1;
            self.pl1 = l;
            self.ph1 = h;
            self.m = l;
            return l;
        }
        // The first extreme of a trend carries `step` as it stands (even above `max_af`); later ones
        // are capped.
        let af = if self.k == 1 {
            self.step
        } else {
            let t = self.step * self.k as f64;
            if t < self.max_af { t } else { self.max_af }
        };
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
}

/// The SAR column of `bars`, the stepper's `vectorize`.
#[inline]
pub(crate) fn psar_bars(core: &PsarCore, bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut core = core.clone();
    core.clear();
    let mut out = vec![f64::NAN; bars.len()];
    for (o, b) in out.iter_mut().zip(bars) {
        *o = core.push(b.high, b.low);
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
#[derive(Clone)]
pub(crate) struct AdxCore {
    n: usize,
    inv: f64,
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
    dx_sum: f64,
    adx: f64,
}

impl AdxCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        AdxCore {
            n,
            inv: 1.0 / n as f64,
            keep: (n as f64 - 1.0) / n as f64,
            i: 0,
            // A period of 0 never leaves the slow path, which answers NaN.
            end: if n == 0 { usize::MAX } else { 2 * n },
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
        *self = Self::new(self.n);
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
        let denom = pdi + mdi;
        let dx = if denom > 0.0 { 100.0 * (pdi - mdi).abs() / denom } else { 0.0 };
        (pdi, mdi, dx)
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 3] {
        let up = h - self.ph;
        let dn = self.pl - l;
        // `up > dn && up > 0` as one masked select on `up > dn` of `max(up, 0)`: a short-circuit `&&`
        // would be a branch, and which of the two moves is larger is a coin toss.
        let upp = if up > 0.0 { up } else { 0.0 };
        let dnp = if dn > 0.0 { dn } else { 0.0 };
        let pdm = if up > dn { upp } else { 0.0 };
        let mdm = if dn > up { dnp } else { 0.0 };
        let tr = (h - l).max((h - self.pc).abs()).max((l - self.pc).abs());
        self.ph = h;
        self.pl = l;
        self.pc = c;
        if self.i >= self.end {
            // Steady state: three decays, the DI/DX lines and the ADX decay, nothing else.
            self.atr_s = self.atr_s * self.keep + tr;
            self.pdm_s = self.pdm_s * self.keep + pdm;
            self.mdm_s = self.mdm_s * self.keep + mdm;
            let (pdi, mdi, dx) = Self::lines(self.atr_s, self.pdm_s, self.mdm_s);
            self.adx = self.adx * self.keep + dx * self.inv;
            return [self.adx, pdi, mdi];
        }
        let i = self.i;
        self.i = i + 1;
        // Bar 0 has no previous bar to difference; a period of 0 has no lines.
        if self.n == 0 || i == 0 {
            return [f64::NAN; 3];
        }
        if i < self.n {
            self.atr_s += tr;
            self.pdm_s += pdm;
            self.mdm_s += mdm;
            return [f64::NAN; 3];
        }
        if i == self.n {
            // The seed is the sum of bars 1..=n, this bar included; no decay yet.
            self.atr_s += tr;
            self.pdm_s += pdm;
            self.mdm_s += mdm;
        } else {
            self.atr_s = self.atr_s * self.keep + tr;
            self.pdm_s = self.pdm_s * self.keep + pdm;
            self.mdm_s = self.mdm_s * self.keep + mdm;
        }
        let (pdi, mdi, dx) = Self::lines(self.atr_s, self.pdm_s, self.mdm_s);
        // DX number `i - n + 1`; the slow path ends at DX number `n`, where ADX is the mean of them.
        self.dx_sum += dx;
        if i + 1 < 2 * self.n {
            return [f64::NAN, pdi, mdi];
        }
        self.adx = self.dx_sum * self.inv;
        [self.adx, pdi, mdi]
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
    buf: Vec<f64>,
    head: usize,
    /// `NaN` entries in `buf`.
    bad: usize,
    /// `hh` and `ll` do not describe `buf` (start, or a `NaN` was inside).
    stale: bool,
    hh: f64,
    ll: f64,
    smooth: SmaCore,
}

/// `(highest, lowest)` of the window, the same folds the textbook kernels use.
#[cold]
#[inline(never)]
fn extremes(buf: &[f64]) -> (f64, f64) {
    (buf.iter().copied().fold(f64::MIN, f64::max), buf.iter().copied().fold(f64::MAX, f64::min))
}

impl StochRsiCore {
    #[inline]
    pub(crate) fn new(rsi_p: usize, k: usize, d: usize) -> Self {
        let k = k.max(1);
        StochRsiCore {
            rsi: RsiCore::new(rsi_p),
            buf: vec![f64::NAN; k],
            head: 0,
            bad: k,
            stale: true,
            hh: f64::NAN,
            ll: f64::NAN,
            smooth: SmaCore::new(d),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.rsi.clear();
        self.buf.fill(f64::NAN);
        self.head = 0;
        self.bad = self.buf.len();
        self.stale = true;
        self.smooth.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> [f64; 2] {
        let r = self.rsi.push(c);
        let old = std::mem::replace(&mut self.buf[self.head], r);
        self.head += 1;
        if self.head == self.buf.len() {
            self.head = 0;
        }
        self.bad = self.bad + usize::from(r.is_nan()) - usize::from(old.is_nan());
        if self.bad != 0 {
            self.stale = true;
            return [f64::NAN, f64::NAN];
        }
        if self.stale || old == self.hh || old == self.ll {
            let (hh, ll) = extremes(&self.buf);
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
