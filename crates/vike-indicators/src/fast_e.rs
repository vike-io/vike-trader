//! Fast cores for group `e` of the third wave of indicators (see `_followups/perf-conv3-brief.md`):
//! `alma`, `envelopes`, `midpoint`, `midprice`, `vwma`, `atr_trailing_stop`, `evwma`, `frama`,
//! `hilo_activator` and `vidya`.
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]` and holds
//! no `&mut self` call in its hot part. The typed steppers, `vectorize` and the `series::*` calls all
//! run the same `push`, so a stream and a batch agree bit for bit.
//!
//! The shape is the earlier waves' (`crate::roll::SmaCore`, `crate::fast_d2`): rings that start as
//! zeros so a carried sum is right from the first value; sums rebuilt from the ring every
//! `ceil(512 / n)` laps; a `NaN` RESULT (or a sum that has become small beside the largest one since
//! its last rebuild) takes a cold rebuild over a shared slice, so a value is `NaN` while a `NaN` is
//! inside the window and exact again the update after it left. The infinite-memory recurrences
//! (`vidya`, `frama`, `evwma`, `hilo_activator`, `atr_trailing_stop`) are as path dependent as they
//! always were: a `NaN` close poisons `vidya`, `frama` and `evwma` for good.

use crate::fast_a::HiLo;
use crate::math::sq;
use crate::roll::{RING_CAP, SmaCore};

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const REBUILD: usize = 512;

/// A running sum is also rebuilt when it falls under this fraction of the largest value it has taken
/// since its last rebuild: the rounding error of a sum scales with that largest value, so a sum that
/// has become small beside it (a volume spike that left the window) has lost its digits.
const SMALL: f64 = 1.0e-5;

/// Laps of a ring of `n` values between two rebuilds of its sums.
#[inline]
fn laps_per(n: usize) -> usize {
    REBUILD.div_ceil(n.max(1)).max(1)
}

/// The sum of a ring of single values, in any order.
#[inline(always)]
fn ring_total(buf: &[f64]) -> f64 {
    buf.iter().fold(0.0, |a, &v| a + v)
}

/// The two column sums of a ring of pairs, in any order.
#[inline(always)]
fn pair_total(buf: &[[f64; 2]]) -> (f64, f64) {
    buf.iter().fold((0.0, 0.0), |a, v| (a.0 + v[0], a.1 + v[1]))
}

/// [`ring_total`] for the cold path: a free function over a shared slice, never a `&mut self`
/// method (see `crate::roll::SmaCore::push`).
#[cold]
#[inline(never)]
fn ring_total_cold(buf: &[f64]) -> f64 {
    ring_total(buf)
}

/// [`pair_total`] for the cold path.
#[cold]
#[inline(never)]
fn pair_total_cold(buf: &[[f64; 2]]) -> (f64, f64) {
    pair_total(buf)
}

// ================================== ALMA ===================================

/// Arnaud Legoux moving average: a Gaussian-weighted mean of the last `n` values, `NaN` until `n`
/// values are in.
///
/// The weights are the kernel's always: `exp(-(k - m)^2 / 2 s^2)` with `m = offset * (n - 1)` and
/// `s = n / sigma`, divided by their sum. The closes live in a ring written twice (at `head` and
/// `head + n`), so the window is always the contiguous slice `buf[head..head + n]`, oldest first,
/// and one value is the dot product of that slice with the weights, summed in eight independent
/// lanes (the order a naive fold has is not worth a dependent chain of adds). The dot product is
/// recomputed from the window each time, so a `NaN` in the window is `NaN` while it is inside and
/// the value is exact the update after it left.
#[derive(Clone)]
pub(crate) struct AlmaCore {
    n: usize,
    buf: Vec<f64>,
    weights: Vec<f64>,
    head: usize,
    warm: usize,
}

impl AlmaCore {
    #[inline]
    pub(crate) fn new(n: usize, offset: f64, sigma: f64) -> Self {
        let n = n.clamp(1, RING_CAP);
        let m = offset * (n as f64 - 1.0);
        let s = n as f64 / sigma;
        let raw: Vec<f64> = (0..n).map(|k| libm::exp(-sq(k as f64 - m) / (2.0 * s * s))).collect();
        let weight_sum: f64 = raw.iter().sum();
        AlmaCore {
            n,
            buf: vec![0.0; 2 * n],
            weights: raw.iter().map(|w| w / weight_sum).collect(),
            head: 0,
            warm: n - 1,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.warm = self.n - 1;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let n = self.n;
        self.buf[self.head] = x;
        self.buf[self.head + n] = x;
        self.head += 1;
        if self.head == n {
            self.head = 0;
        }
        if self.warm != 0 {
            self.warm -= 1;
            return f64::NAN;
        }
        dot(&self.buf[self.head..self.head + n], &self.weights)
    }
}

/// `sum(a[k] * w[k])` over equal-length slices: eight independent partial sums over the whole
/// groups of eight (a fixed-size group lets the compiler pack them), then the remainder one by one.
/// Measured against four scalar lanes, four pairs and a plain left fold at windows of 20, 64 and 400.
#[inline(always)]
fn dot(a: &[f64], w: &[f64]) -> f64 {
    let (ca, ra) = a.as_chunks::<8>();
    let (cw, rw) = w.as_chunks::<8>();
    let mut acc = [0.0_f64; 8];
    for (x, y) in ca.iter().zip(cw) {
        acc[0] += x[0] * y[0];
        acc[1] += x[1] * y[1];
        acc[2] += x[2] * y[2];
        acc[3] += x[3] * y[3];
        acc[4] += x[4] * y[4];
        acc[5] += x[5] * y[5];
        acc[6] += x[6] * y[6];
        acc[7] += x[7] * y[7];
    }
    let mut tail = 0.0;
    for (x, y) in ra.iter().zip(rw) {
        tail += x * y;
    }
    (((acc[0] + acc[1]) + (acc[2] + acc[3])) + ((acc[4] + acc[5]) + (acc[6] + acc[7]))) + tail
}

// ================================ ENVELOPES ================================

/// Envelopes: a simple moving average and the bands `pct` percent above and below it,
/// `[upper, mid, lower]`, `NaN` until `n` values are in.
#[derive(Clone)]
pub(crate) struct EnvelopesCore {
    sma: SmaCore,
    up: f64,
    down: f64,
}

impl EnvelopesCore {
    #[inline]
    pub(crate) fn new(n: usize, pct: f64) -> Self {
        let factor = pct / 100.0;
        EnvelopesCore { sma: SmaCore::new(n), up: 1.0 + factor, down: 1.0 - factor }
    }

    pub(crate) fn clear(&mut self) {
        self.sma.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> [f64; 3] {
        let mid = self.sma.push(x);
        [mid * self.up, mid, mid * self.down]
    }
}

// ============================ MIDPOINT / MIDPRICE ===========================

/// `(highest high + lowest low) / 2` of the last `n` bars (midpoint feeds the close as both), `NaN`
/// until `n` bars are in. The window extremes are [`HiLo`]'s: O(1) a bar, and a `NaN` is skipped as
/// if absent, as `f64::max` and `f64::min` always skipped it.
#[derive(Clone)]
pub(crate) struct MidCore {
    win: HiLo,
}

impl MidCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        MidCore { win: HiLo::new(n.min(RING_CAP)) }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> f64 {
        let [hh, ll] = self.win.push(high, low);
        (hh + ll) * 0.5
    }
}

/// [`MidCore`] over one series (`midpoint`): the same value as feeding it as both high and low.
#[derive(Clone)]
pub(crate) struct Mid1Core {
    win: HiLo,
}

impl Mid1Core {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        Mid1Core { win: HiLo::new(n.min(RING_CAP)) }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let [hh, ll] = self.win.push(x, x);
        (hh + ll) * 0.5
    }
}

// =================================== VWMA ==================================

/// Volume-weighted moving average: `sum(close * volume) / sum(volume)` over the last `n` bars,
/// `NaN` until `n` bars are in and `NaN` when the window traded no volume.
///
/// Two running sums over one ring of `[close * volume, volume]` pairs, rebuilt from the ring every
/// `ceil(512 / n)` laps and whenever the volume sum has fallen under a hundred thousandth of the
/// largest it has been since the last rebuild (a volume spike that left the window leaves rounding
/// residue as large as the quiet volumes). A window with no volume rebuilds to an exact zero. `smax`
/// is infinite until the first lap ends, which keeps the warm-up out of the hot path.
#[derive(Clone)]
pub(crate) struct VwmaCore {
    buf: Vec<[f64; 2]>,
    head: usize,
    laps: usize,
    laps_per: usize,
    spv: f64,
    sv: f64,
    smax: f64,
}

impl VwmaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.clamp(1, RING_CAP);
        let laps_per = laps_per(n);
        VwmaCore {
            buf: vec![[0.0; 2]; n],
            head: 0,
            // The first lap always rebuilds: that is what ends the warm-up.
            laps: laps_per - 1,
            laps_per,
            spv: 0.0,
            sv: 0.0,
            smax: f64::INFINITY,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill([0.0; 2]);
        self.head = 0;
        self.laps = self.laps_per - 1;
        self.spv = 0.0;
        self.sv = 0.0;
        self.smax = f64::INFINITY;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, close: f64, volume: f64) -> f64 {
        let pv = close * volume;
        let old = std::mem::replace(&mut self.buf[self.head], [pv, volume]);
        self.head += 1;
        self.spv += pv - old[0];
        self.sv += volume - old[1];
        if self.sv > self.smax {
            self.smax = self.sv;
        }
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                (self.spv, self.sv) = pair_total(&self.buf);
                self.smax = self.sv;
                self.laps = 0;
            }
        }
        // A comparison with a `NaN` is false: a `NaN` volume sum falls to the cold path with the
        // too-small ones, and the warm-up (`smax` infinite) with them.
        if self.sv >= self.smax * SMALL && !self.spv.is_nan() {
            return if self.sv != 0.0 { self.spv / self.sv } else { f64::NAN };
        }
        if self.smax == f64::INFINITY {
            return f64::NAN;
        }
        (self.spv, self.sv) = pair_total_cold(&self.buf);
        self.smax = self.sv;
        if self.sv != 0.0 { self.spv / self.sv } else { f64::NAN }
    }
}

// =================================== EVWMA =================================

/// Elastic volume-weighted moving average: a value that steps toward each close by that bar's share
/// of the volume of the last `n` bars. Seeded with the close at index `n - 1`; carries the previous
/// value when the window traded no volume (or its sum is `NaN`).
///
/// The window volume is a running sum on a ring, rebuilt as [`VwmaCore`]'s are. The step is
/// written `prev * (1 - k) + close * k` with `k = volume / total`, which keeps the division off the
/// recurrence's dependent chain.
#[derive(Clone)]
pub(crate) struct EvwmaCore {
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    total: f64,
    smax: f64,
    prev: f64,
    primed: bool,
}

impl EvwmaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.clamp(1, RING_CAP);
        let laps_per = laps_per(n);
        EvwmaCore {
            buf: vec![0.0; n],
            head: 0,
            laps: laps_per - 1,
            laps_per,
            total: 0.0,
            smax: f64::INFINITY,
            prev: f64::NAN,
            primed: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = self.laps_per - 1;
        self.total = 0.0;
        self.smax = f64::INFINITY;
        self.prev = f64::NAN;
        self.primed = false;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, close: f64, volume: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], volume);
        self.head += 1;
        self.total += volume - old;
        if self.total > self.smax {
            self.smax = self.total;
        }
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                self.total = ring_total(&self.buf);
                self.smax = self.total;
                self.laps = 0;
            }
            if !self.primed {
                // The first lap ends: the average is seeded with this close.
                self.primed = true;
                self.prev = close;
                return close;
            }
        }
        if self.total >= self.smax * SMALL {
            let k = volume / self.total;
            let step = self.prev * (1.0 - k) + close * k;
            if self.total > 0.0 {
                self.prev = step;
            }
            return self.prev;
        }
        if !self.primed {
            return f64::NAN;
        }
        // A `NaN` in the window, or a sum that has become small beside its largest: rebuild.
        self.total = ring_total_cold(&self.buf);
        self.smax = self.total;
        if self.total > 0.0 {
            let k = volume / self.total;
            self.prev = self.prev * (1.0 - k) + close * k;
        }
        self.prev
    }
}

// =================================== VIDYA =================================

/// Chande's variable index dynamic average: an EMA of `alpha = 2 / (n + 1)` scaled by the absolute
/// Chande momentum oscillator of the last `cmo` close-to-close changes. Seeded with the close of
/// the bar before the first output (index `cmo`).
///
/// The rises and the magnitudes of the falls of the window are two running sums over one ring of
/// `[rise, fall]` pairs (a `NaN` change counts as neither, as it always did), rebuilt as
/// [`VwmaCore`]'s are; a window with no movement rebuilds to an exact zero and the index is then 0.
/// The ring starts as zeros, so the sums are right from the first change. A `NaN` close poisons the
/// average for good, as it always did.
#[derive(Clone)]
pub(crate) struct VidyaCore {
    alpha: f64,
    buf: Vec<[f64; 2]>,
    head: usize,
    laps: usize,
    laps_per: usize,
    su: f64,
    sd: f64,
    smax: f64,
    prev_close: f64,
    prev: f64,
    warm: usize,
}

impl VidyaCore {
    #[inline]
    pub(crate) fn new(period: usize, cmo: usize) -> Self {
        let period = period.clamp(1, RING_CAP);
        let cmo = cmo.clamp(1, RING_CAP);
        VidyaCore {
            alpha: 2.0 / (period as f64 + 1.0),
            buf: vec![[0.0; 2]; cmo],
            head: 0,
            laps: 0,
            laps_per: laps_per(cmo),
            su: 0.0,
            sd: 0.0,
            smax: 0.0,
            prev_close: f64::NAN,
            prev: f64::NAN,
            warm: cmo,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill([0.0; 2]);
        self.head = 0;
        self.laps = 0;
        self.su = 0.0;
        self.sd = 0.0;
        self.smax = 0.0;
        self.prev_close = f64::NAN;
        self.prev = f64::NAN;
        self.warm = self.buf.len();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let d = x - self.prev_close;
        self.prev_close = x;
        let up = if d > 0.0 { d } else { 0.0 };
        let down = if d < 0.0 { -d } else { 0.0 };
        let old = std::mem::replace(&mut self.buf[self.head], [up, down]);
        self.head += 1;
        self.su += up - old[0];
        self.sd += down - old[1];
        let mut total = self.su + self.sd;
        if total > self.smax {
            self.smax = total;
        }
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                (self.su, self.sd) = pair_total(&self.buf);
                total = self.su + self.sd;
                self.smax = total;
                self.laps = 0;
            }
        }
        if self.warm != 0 {
            self.warm -= 1;
            if self.warm == 0 {
                // The seed is the close of the bar before the first output: this one.
                self.prev = x;
            }
            return f64::NAN;
        }
        let fresh = total >= self.smax * SMALL;
        if !fresh {
            // A non-finite sum, or one that has become small beside its largest: rebuild.
            (self.su, self.sd) = pair_total_cold(&self.buf);
            total = self.su + self.sd;
            self.smax = total;
        }
        let index = if total != 0.0 { ((self.su - self.sd) / total).abs() } else { 0.0 };
        let k = self.alpha * index;
        self.prev = k * x + (1.0 - k) * self.prev;
        self.prev
    }
}

// =================================== FRAMA =================================

/// Ehlers' fractal adaptive moving average over `2 * half` bars. Seeded with the close at index
/// `2 * half - 1`.
///
/// The extremes of the newer half are one [`HiLo`] of `half` bars; the older half's are the same
/// pair `half` bars ago, so a ring of `half` pairs delays them. With `R1`, `R2` the ranges of the two
/// halves and `R3` the range of both, `N1 + N2 = (R1 + R2) / half` and `N3 = R3 / (2 half)`, so the
/// ratio inside the logarithm is `2 (R1 + R2) / R3`: one logarithm, and the exponential is skipped
/// when the smoothing is clamped (it is `1` for an exponent of 0 or more and `0.01` at or under
/// `ln 0.01`). A window with no range at all takes `alpha = 1`.
#[derive(Clone)]
pub(crate) struct FramaCore {
    half: usize,
    win: HiLo,
    delay: Vec<[f64; 2]>,
    head: usize,
    c1: f64,
    prev: f64,
    warm: usize,
}

/// `ln 0.01`: an exponent at or under it is a smoothing of exactly `0.01`.
const LN_ONE_HUNDREDTH: f64 = -4.605_170_185_988_091;

impl FramaCore {
    #[inline]
    pub(crate) fn new(period: usize) -> Self {
        let half = (period / 2).clamp(1, RING_CAP);
        FramaCore {
            half,
            win: HiLo::new(half),
            delay: vec![[f64::NAN; 2]; half],
            head: 0,
            c1: 4.6 / libm::log(2.0),
            prev: f64::NAN,
            warm: 2 * half,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
        self.delay.fill([f64::NAN; 2]);
        self.head = 0;
        self.prev = f64::NAN;
        self.warm = 2 * self.half;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let [hh2, ll2] = self.win.push(high, low);
        let [hh1, ll1] = std::mem::replace(&mut self.delay[self.head], [hh2, ll2]);
        self.head += 1;
        if self.head == self.half {
            self.head = 0;
        }
        if self.warm != 0 {
            self.warm -= 1;
            if self.warm == 0 {
                self.prev = close;
                return close;
            }
            return f64::NAN;
        }
        let num = (hh1 - ll1) + (hh2 - ll2);
        let whole = (if hh1 >= hh2 { hh1 } else { hh2 }) - (if ll1 <= ll2 { ll1 } else { ll2 });
        let alpha = if whole <= 0.0 || num <= 0.0 {
            1.0
        } else {
            let e = 4.6 - self.c1 * libm::log(2.0 * num / whole);
            if e >= 0.0 {
                1.0
            } else if e <= LN_ONE_HUNDREDTH {
                0.01
            } else {
                libm::exp(e)
            }
        };
        self.prev = alpha * close + (1.0 - alpha) * self.prev;
        self.prev
    }
}

// ================================ HILO ACTIVATOR ===========================

/// Gann HiLo Activator: the simple averages of the highs and of the lows, and a trend that turns up
/// when the close is above the PREVIOUS bar's high average and down when it is below the previous
/// low average. The line is the current low average in an uptrend, the high average in a downtrend.
/// The trend is seeded on the first bar that has both averages (up when the close is at or above
/// their midpoint).
#[derive(Clone)]
pub(crate) struct HiloCore {
    high_avg: SmaCore,
    low_avg: SmaCore,
    prev_high: f64,
    prev_low: f64,
    /// `1.0` (up) or `-1.0` (down), once seeded.
    trend: f64,
    warm: usize,
    n: usize,
}

impl HiloCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.clamp(1, RING_CAP);
        HiloCore {
            high_avg: SmaCore::new(n),
            low_avg: SmaCore::new(n),
            prev_high: f64::NAN,
            prev_low: f64::NAN,
            trend: 0.0,
            warm: n,
            n,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.high_avg.clear();
        self.low_avg.clear();
        self.prev_high = f64::NAN;
        self.prev_low = f64::NAN;
        self.trend = 0.0;
        self.warm = self.n;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let ha = self.high_avg.push(high);
        let la = self.low_avg.push(low);
        if self.warm != 0 {
            self.warm -= 1;
            if self.warm != 0 {
                return f64::NAN;
            }
            // The first bar with both averages seeds the trend.
            self.trend = if close >= (ha + la) * 0.5 { 1.0 } else { -1.0 };
        } else {
            // Two selects on one comparison each, the up-turn last so that it wins a tie: the
            // trend turns on a coin flip, and a branch on it mispredicts about once a bar.
            let t = if close < self.prev_low { -1.0 } else { self.trend };
            self.trend = if close > self.prev_high { 1.0 } else { t };
        }
        self.prev_high = ha;
        self.prev_low = la;
        if self.trend > 0.0 { la } else { ha }
    }
}

// ============================= ATR TRAILING STOP ===========================

/// ATR trailing stop: a stop that trails the close at `mult` times Wilder's average true range and
/// only moves in the trend's direction until price crosses it. The first value, at index `n - 1`
/// (the ATR is seeded with the mean of the first `n` true ranges, the first bar's being
/// `high - low`), is `close - loss`.
///
/// Like `crate::roll::AtrCore`, with the stop folded in: `warm == 0` is a branch-free steady state,
/// the seed ladder runs for the first `n` bars only. The true range keeps `f64::max`'s way with a
/// `NaN` (it answers the other operand).
#[derive(Clone)]
pub(crate) struct AtrStopCore {
    n: usize,
    mult: f64,
    inv: f64,
    keep: f64,
    warm: usize,
    prev_close: f64,
    seed: f64,
    atr: f64,
    stop: f64,
}

impl AtrStopCore {
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        let n = n.clamp(1, RING_CAP);
        AtrStopCore {
            n,
            mult,
            inv: 1.0 / n as f64,
            keep: (n as f64 - 1.0) / n as f64,
            warm: n,
            prev_close: f64::NAN,
            seed: 0.0,
            atr: f64::NAN,
            stop: f64::NAN,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.warm = self.n;
        self.prev_close = f64::NAN;
        self.seed = 0.0;
        self.atr = f64::NAN;
        self.stop = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let pc = self.prev_close;
        self.prev_close = close;
        if self.warm == 0 {
            let tr = (high - low).max((high - pc).abs()).max((low - pc).abs());
            self.atr = self.atr * self.keep + tr * self.inv;
            let loss = self.mult * self.atr;
            let prev = self.stop;
            let (above, below) = (close - loss, close + loss);
            let stop = if close > prev && pc > prev {
                if above > prev { above } else { prev }
            } else if close < prev && pc < prev {
                if below < prev { below } else { prev }
            } else if close > prev {
                above
            } else {
                below
            };
            self.stop = stop;
            return stop;
        }
        // The seed ladder: the first bar's range is `high - low`.
        let tr = if self.warm == self.n {
            high - low
        } else {
            (high - low).max((high - pc).abs()).max((low - pc).abs())
        };
        self.warm -= 1;
        self.seed += tr;
        if self.warm != 0 {
            return f64::NAN;
        }
        self.atr = self.seed * self.inv;
        self.stop = close - self.mult * self.atr;
        self.stop
    }
}

// ============================ the typed steppers ===========================

/// Generates the `next_*` door and the private `step` of a one-line stepper, by the prices it reads.
macro_rules! scalar_next {
    ($Name:ident, value) => {
        impl $Name {
            /// Feed the next close as a plain number, no [`Bar`](vike_marketdata::Bar) needed; the
            /// same answer as `next` for a bar with that close.
            #[inline]
            pub fn next_value(&mut self, close: f64) -> f64 {
                self.last = self.core.push(close);
                self.last
            }
            /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it.
            #[inline]
            fn step(&mut self, bar: &vike_marketdata::Bar) -> f64 {
                self.next_value(bar.close)
            }
        }
    };
    ($Name:ident, hl) => {
        impl $Name {
            /// Feed the next high and low as plain numbers, no [`Bar`](vike_marketdata::Bar)
            /// needed; the same answer as `next` for a bar with those.
            #[inline]
            pub fn next_hl(&mut self, high: f64, low: f64) -> f64 {
                self.last = self.core.push(high, low);
                self.last
            }
            /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it.
            #[inline]
            fn step(&mut self, bar: &vike_marketdata::Bar) -> f64 {
                self.next_hl(bar.high, bar.low)
            }
        }
    };
    ($Name:ident, hlc) => {
        impl $Name {
            /// Feed the next high, low and close as plain numbers, no
            /// [`Bar`](vike_marketdata::Bar) needed; the same answer as `next` for a bar with
            /// those.
            #[inline]
            pub fn next_hlc(&mut self, high: f64, low: f64, close: f64) -> f64 {
                self.last = self.core.push(high, low, close);
                self.last
            }
            /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it.
            #[inline]
            fn step(&mut self, bar: &vike_marketdata::Bar) -> f64 {
                self.next_hlc(bar.high, bar.low, bar.close)
            }
        }
    };
    ($Name:ident, cv) => {
        impl $Name {
            /// Feed the next close and volume as plain numbers, no
            /// [`Bar`](vike_marketdata::Bar) needed; the same answer as `next` for a bar with
            /// those.
            #[inline]
            pub fn next_cv(&mut self, close: f64, volume: f64) -> f64 {
                self.last = self.core.push(close, volume);
                self.last
            }
            /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it.
            #[inline]
            fn step(&mut self, bar: &vike_marketdata::Bar) -> f64 {
                self.next_cv(bar.close, bar.volume)
            }
        }
    };
}

/// Generates a one-line, hand-written stepper around a core of this file: the struct, `Default`,
/// `with_params`, the checked `new(..)` and its registry-equality test, `next`, the door for the
/// prices it reads (`input = value | hl | hlc | cv`) and the `Indicator` impl, whose `vectorize` is
/// the stream folded over the bars. `build` is an expression over the parameter locals (each an
/// `f64`) that makes the core; `lookback` is an exact expression over the same.
macro_rules! scalar_indicator {
    (
        $(#[$m:meta])*
        $Name:ident, $key:literal, $Core:ty, input = $input:ident,
        [ $( $pf:ident = $pdef:expr ),+ ],
        build = $build:expr,
        lookback = $lb:expr,
        path_dependent = $pd:literal
    ) => {
        $(#[$m])*
        #[derive(Clone)]
        pub struct $Name {
            $( $pf: f64, )+
            core: $Core,
            last: f64,
        }
        impl $Name {
            /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes
            /// them.
            #[inline]
            fn defaults() -> Self {
                Self::with_params(&[ $( $pdef ),+ ])
            }
            /// Parametric constructor: `p` in registry order, a missing entry the default.
            #[inline]
            pub(crate) fn with_params(p: &[f64]) -> Self {
                let mut given = p.iter().copied();
                $( let $pf = given.next().unwrap_or($pdef); )+
                Self { $( $pf, )+ core: $build, last: f64::NAN }
            }
            /// Feed the next bar and get this bar's value (`NaN` while warming up). This IS
            /// [`crate::Indicator::on_bar`], bit for bit, without the `Vec`.
            #[inline]
            pub fn next(&mut self, bar: &vike_marketdata::Bar) -> f64 {
                self.step(bar)
            }
        }
        $crate::fast_e::scalar_next!($Name, $input);
        impl Default for $Name {
            fn default() -> Self {
                Self::defaults()
            }
        }
        impl $Name {
            /// Build with these parameters, in the order the registry row lists them. A value the
            /// registry would not accept (out of range, or `NaN`) is a [`crate::ParamError`] naming
            /// it; [`crate::make_with`] clamps instead. `Default` gives the registry defaults.
            // Inline, unlike the generated `new` of `hist_indicator!`: a literal period then reaches
            // the loop that feeds the stepper as a compile-time constant.
            #[inline]
            pub fn new( $( $pf: f64 ),+ ) -> Result<Self, $crate::ParamError> {
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
        impl $crate::Indicator for $Name {
            fn on_bar(&mut self, bar: &vike_marketdata::Bar) -> Vec<f64> {
                vec![self.step(bar)]
            }
            fn vectorize(&self, bars: &[vike_marketdata::Bar]) -> Vec<Vec<f64>> {
                $crate::roll::batch_scalar(self, bars, Self::step)
            }
            fn value(&self) -> Vec<f64> {
                vec![self.last]
            }
            fn reset(&mut self) {
                self.core.clear();
                self.last = f64::NAN;
            }
            fn name(&self) -> &str {
                $key
            }
            // allow, not expect: inside macro_rules!: a parameter that does not enter the warm-up is unused in some expansions only.
            #[allow(unused_variables, clippy::allow_attributes)]
            fn lookback(&self) -> usize {
                $( let $pf = self.$pf; )+
                $lb
            }
            fn lookback_exact(&self) -> bool {
                true
            }
            fn warmup_path_dependent(&self) -> bool {
                $pd
            }
        }
    };
}

pub(crate) use scalar_indicator;
pub(crate) use scalar_next;

#[cfg(test)]
mod tests {
    use crate::Indicator;
    use crate::indicators::{
        Alma, AtrTrailingStop, Envelopes, Evwma, Frama, HiloActivator, Midpoint, Midprice, Vidya,
        Vwma,
    };
    use vike_marketdata::Bar;

    /// A raw `with_params` can read an infinity, a `NaN`, a negative or a zero: no constructor may
    /// wrap a length, ask the allocator for the address space, or panic on the first bars.
    #[test]
    fn with_params_degenerate_never_wraps() {
        let bars: Vec<Bar> = (0..40)
            .map(|i| {
                let x = 100.0 + f64::from(i % 7);
                Bar::ohlcv(i64::from(i) * 60_000, x, x + 1.0, x - 1.0, x, 10.0 + f64::from(i))
            })
            .collect();
        macro_rules! probe {
            ($($ty:ty),* $(,)?) => {$(
                for raw in [
                    &[0.0f64][..],
                    &[0.0, 0.0, 0.0][..],
                    &[f64::NAN, f64::NAN, f64::NAN][..],
                    &[-1e9, -1e9, -1e9][..],
                    &[f64::INFINITY, f64::NEG_INFINITY, 0.0][..],
                    &[f64::INFINITY, f64::INFINITY, f64::INFINITY][..],
                ] {
                    let mut ind = <$ty>::with_params(raw);
                    assert!(ind.lookback() < 1_000_000, "{} lookback at {raw:?}", ind.name());
                    for b in &bars {
                        let _ = ind.on_bar(b);
                    }
                    let _ = ind.vectorize(&bars);
                }
            )*};
        }
        probe!(
            Alma,
            AtrTrailingStop,
            Envelopes,
            Evwma,
            Frama,
            HiloActivator,
            Midpoint,
            Midprice,
            Vidya,
            Vwma
        );
    }
}
