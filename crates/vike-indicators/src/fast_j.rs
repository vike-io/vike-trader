//! Fast cores for group `j` of the third wave of indicators (see `_followups/perf-conv3-brief.md`):
//! `bbands_pctb`, `bbands_width`, `donchian_width`, `ulcer`, `chop` and the six realized-volatility
//! rows (`rvol_ewma`, `rvol_close_to_close`, `rvol_parkinson`, `rvol_garman_klass`,
//! `rvol_rogers_satchell`, `rvol_yang_zhang`).
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds no
//! `&mut self` call in its hot part, and is used by BOTH the streaming stepper and the whole-series
//! functions, so a stream and a batch agree bit for bit.
//!
//! The shape is the earlier waves' (`crate::roll::SmaCore`, `BollingerCore`, `crate::fast_d2`): rings
//! that start as zeros so the carried sums are right from the first value; running sums rebuilt from
//! the rings every `ceil(512 / n)` laps; a multiplier that is `NaN` until the first lap of the ring
//! ends, which makes the warm-up outputs `NaN` without a branch; and a `NaN` RESULT (a `NaN` in the
//! window, or a stale carried sum after one left) takes a cold rebuild over a shared slice, so the
//! result is `NaN` while a `NaN` is inside the window and exact again the update after it left.
//!
//! The realized-volatility cores differ in one respect: a bar whose term does not exist (an unusable
//! price) is not pushed at all, so their rings hold the last `n` EXISTING terms, as the estimators'
//! contract says (`crate::realized_vol`'s "Input rules").

use std::f64::consts::LN_2;

use crate::fast_a::HiLo;
use crate::fast_d2::true_range;

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const REBUILD: usize = 512;

/// Laps of a ring of `n` values between two rebuilds of its sums.
#[inline]
fn laps_per(n: usize) -> usize {
    REBUILD.div_ceil(n.max(1)).max(1)
}

/// `1 / n`, and `NaN` for a window of 0 (which never leaves warm-up).
#[inline]
fn inverse(n: usize) -> f64 {
    if n == 0 { f64::NAN } else { 1.0 / n as f64 }
}

/// A price a logarithm may be taken of: finite and strictly positive.
#[inline(always)]
pub(crate) fn usable(p: f64) -> bool {
    p > 0.0 && p < f64::INFINITY
}

/// The sum of a ring read oldest to newest (the oldest value is at `head`).
#[cold]
#[inline(never)]
fn ring_sum(buf: &[f64], head: usize) -> f64 {
    buf[head..].iter().chain(&buf[..head]).fold(0.0, |a, &v| a + v)
}

/// The window (oldest value at `head`) as `(shift, sum(x - shift), sum((x - shift)^2))`, `shift` being
/// the oldest value when it is finite.
#[cold]
#[inline(never)]
fn shifted_sums(buf: &[f64], head: usize) -> (f64, f64, f64) {
    let shift = if buf[head].is_finite() { buf[head] } else { 0.0 };
    let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
    for &v in buf[head..].iter().chain(&buf[..head]) {
        let d = v - shift;
        s1 += d;
        s2 += d * d;
    }
    (shift, s1, s2)
}

// ============================== BBANDS %B and WIDTH ============================

/// The mean and the population standard deviation of the last `n` values, from running sums of the
/// window shifted by its oldest value at the last rebuild (so a price near a million with moves of a
/// hundredth keeps its digits), rebuilt every `ceil(512 / n)` laps. This is
/// [`crate::roll::BollingerCore`]'s arithmetic with [`crate::fast_d2::StddevCore`]'s guard on top.
///
/// `%B` and the band width are RATIOS of the deviation, so they need its relative accuracy, not just
/// its accuracy next to the price. The sums' rounding error scales with the largest second moment
/// `smax` since the last rebuild (the shift is up to a rebuild old, so a window that has become tight
/// beside it keeps few digits: measured, a 2-close window lost up to four digits of `%B` without the
/// guard). One compare catches every case that matters: a variance that is `NaN` (warming up, or a
/// `NaN` in the window), or under a hundred thousandth of `smax / n`, takes the cold path, which
/// re-centres the shift on the window's oldest value and sums again. That also makes a window of
/// bit-equal values have EXACTLY zero deviation (re-centred on the repeated value every deviation is
/// `0.0`, and the compare is then `0 >= 0`), which is the flat-window law of `bbands_pctb`
/// (`NaN`: the band has no width) and `bbands_width` (`0`), decided on the INPUTS and never by a
/// threshold on the output.
#[derive(Clone)]
pub(crate) struct BandCore {
    inv: f64,
    inv_n: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    s1: f64,
    s2: f64,
    /// The largest `s2` since the last rebuild.
    smax: f64,
}

impl BandCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        BandCore {
            inv: f64::NAN,
            inv_n: inverse(n),
            buf: vec![0.0; n.max(1)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shift: 0.0,
            s1: 0.0,
            s2: 0.0,
            smax: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.shift = 0.0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.smax = 0.0;
        self.inv = f64::NAN;
    }

    /// Push a value; `[shift, mean - shift, standard deviation]` of the last `n`, `NaN` until `n` are in.
    /// The mean comes as two parts so a caller can form `x - mean` without first rounding the mean to
    /// the price level.
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> [f64; 3] {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        self.s1 += d - o;
        self.s2 += d * d - o * o;
        if self.s2 > self.smax {
            self.smax = self.s2;
        }
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0.
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let (shift, s1, s2) = shifted_sums(&self.buf, 0);
                self.shift = shift;
                self.s1 = s1;
                self.s2 = s2;
                self.smax = s2;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let m = self.s1 * self.inv;
        let var = self.s2 * self.inv - m * m;
        // A comparison with a `NaN` is false, so a `NaN` variance (warming up, or a `NaN` in the
        // window) falls through to the cold path with the too-small ones.
        if var >= self.smax * self.inv * 1.0e-5 {
            return [self.shift, m, var.sqrt()];
        }
        // Cold, and written inline over a shared slice (see `BollingerCore::push`).
        if self.inv.is_nan() {
            return [f64::NAN; 3];
        }
        let (shift, s1, s2) = shifted_sums(&self.buf, self.head);
        self.shift = shift;
        self.s1 = s1;
        self.s2 = s2;
        self.smax = s2;
        let m = s1 * self.inv;
        // Not `.max(0.0)`: that returns 0 for a NaN, and a window holding a NaN is NaN.
        let mut var = s2 * self.inv - m * m;
        if var < 0.0 {
            var = 0.0;
        }
        if var.is_nan() { [f64::NAN; 3] } else { [shift, m, var.sqrt()] }
    }
}

/// Bollinger %B: `(close - lower) / (upper - lower)` of the bands `mean -+ mult * deviation`, `NaN`
/// where the band has no width.
#[derive(Clone)]
pub(crate) struct PctbCore {
    win: BandCore,
    mult: f64,
}

impl PctbCore {
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        PctbCore { win: BandCore::new(n), mult }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let [shift, m, sd] = self.win.push(x);
        // `((x - mean) / sd + mult) / (2 mult)`: the band edges are never rounded to the price level, so
        // a band a few ulps wide (two closes one ulp apart) still has a width.
        if sd == 0.0 { f64::NAN } else { (((x - shift) - m) / sd + self.mult) / (2.0 * self.mult) }
    }
}

/// Bollinger band width: `(upper - lower) / mean` of the bands `mean -+ mult * deviation`, `NaN`
/// where the mean is zero.
#[derive(Clone)]
pub(crate) struct WidthCore {
    win: BandCore,
    mult: f64,
}

impl WidthCore {
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        WidthCore { win: BandCore::new(n), mult }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let [shift, m, sd] = self.win.push(x);
        let mean = shift + m;
        if mean == 0.0 { f64::NAN } else { (2.0 * self.mult * sd) / mean }
    }
}

// ================================ DONCHIAN WIDTH ===============================

/// Donchian channel width: the highest high minus the lowest low of the last `n` bars. A `NaN` high or
/// low is skipped as if absent ([`HiLo`]). A window with no number in it answers `NaN`.
#[derive(Clone)]
pub(crate) struct DonchianWidthCore {
    win: HiLo,
}

impl DonchianWidthCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        DonchianWidthCore { win: HiLo::new(n) }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> f64 {
        let [hh, ll] = self.win.push(high, low);
        // A window with no number in it has no extreme: `HiLo` answers its empty identities, which are
        // not a price. `NaN`, the crate's "no value", is what comes out.
        let hh = if hh == f64::NEG_INFINITY { f64::NAN } else { hh };
        let ll = if ll == f64::INFINITY { f64::NAN } else { ll };
        hh - ll
    }
}

/// The highest value of the last `n` in O(1) a bar with no deque: [`HiLo`]'s block method
/// (blocks of `n`, a running maximum inside the block, suffix maxima of the previous block computed in
/// place once per block) on one array instead of pairs. A `NaN` is skipped as if absent; the result is
/// `NaN` until the first block ends.
#[derive(Clone)]
pub(crate) struct Peak {
    n: usize,
    buf: Vec<f64>,
    pos: usize,
    pre: f64,
}

impl Peak {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.max(1);
        let mut p = Peak { n, buf: vec![f64::NAN; n + 1], pos: 0, pre: f64::NEG_INFINITY };
        p.buf[n] = f64::NEG_INFINITY;
        p
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(f64::NAN);
        self.buf[self.n] = f64::NEG_INFINITY;
        self.pos = 0;
        self.pre = f64::NEG_INFINITY;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let s = self.buf[self.pos + 1];
        self.buf[self.pos] = x;
        let p = if x > self.pre { x } else { self.pre };
        self.pre = p;
        let hh = if p >= s { p } else { s };
        self.pos += 1;
        if self.pos == self.n {
            self.pos = 0;
            suffix_maxima(&mut self.buf);
            self.pre = f64::NEG_INFINITY;
        }
        hh
    }
}

/// Turn a finished block (`buf[..n]`, with the sentinel in `buf[n]`) into its suffix maxima in place.
#[inline(never)]
fn suffix_maxima(buf: &mut [f64]) {
    let mut hi = f64::NEG_INFINITY;
    for v in buf.iter_mut().rev().skip(1) {
        if *v > hi {
            hi = *v;
        }
        *v = hi;
    }
}
// =================================== ULCER ====================================

/// Ulcer Index: `sqrt(mean over the window of the squared percentage drawdowns from the window's
/// highest close)`.
///
/// Every close of the window is measured against the window's PEAK, which moves, so the squared sum
/// is carried as the two moments of the drawdowns `c - peak` themselves: `Q = sum (c - peak)^2` and
/// `L = sum (c - peak)`. A new peak `peak + dp` rebases both exactly
/// (`Q' = Q - 2 dp L + n dp^2`, `L' = L - n dp`), a close that leaves and one that enters are one term
/// each, and the result is `sqrt(Q / n) * 100 / |peak|`. The peak is a block-wise window maximum
/// ([`Peak`]). All the moments are of drawdowns, which are
/// as small as the market's moves, so a price near a million with moves of a hundredth keeps its
/// digits.
///
/// `Q` is rebuilt from the ring every `ceil(512 / n)` laps, and at once when it came out `NaN` (a `NaN`
/// close is inside the window, or one has just left) or fell under a hundred thousandth of the
/// largest `Q` since the last rebuild (the carried value has lost its digits next to that history,
/// and a window flat at one price is EXACTLY `0` after the rebuild).
#[derive(Clone)]
pub(crate) struct UlcerCore {
    inv: f64,
    inv_n: f64,
    nm1: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    hi: Peak,
    p: f64,
    q: f64,
    l: f64,
    /// The largest `q` since the last rebuild.
    qmax: f64,
}

impl UlcerCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        UlcerCore {
            inv: f64::NAN,
            inv_n: inverse(n),
            nm1: n.saturating_sub(1) as f64,
            buf: vec![0.0; n.max(1)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            hi: Peak::new(n),
            p: f64::NAN,
            q: 0.0,
            l: 0.0,
            qmax: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.hi.clear();
        self.p = f64::NAN;
        self.q = 0.0;
        self.l = 0.0;
        self.qmax = 0.0;
        self.inv = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let p2 = self.hi.push(x);
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        // Rebase onto the new peak, drop the leaving close and add the entering one. Written as
        // changes to `q` and `l`, so each carried sum is one add long on its dependent chain.
        let (p, l0) = (self.p, self.l);
        let d_old = old - p;
        let dp = p2 - p;
        let dx = x - p2;
        self.l = l0 + ((dx - d_old) - self.nm1 * dp);
        let mut q =
            self.q + ((dx * dx - d_old * d_old) + (self.nm1 * dp * dp - 2.0 * dp * (l0 - d_old)));
        self.p = p2;
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the peak was `NaN`.
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let (q2, l2) = drawdown_sums(&self.buf, 0, p2);
                q = q2;
                self.l = l2;
                self.qmax = q2;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        if q > self.qmax {
            self.qmax = q;
        }
        self.q = q;
        let scale = if p2 == 0.0 { 0.0 } else { 100.0 / p2.abs() };
        // A comparison with a `NaN` is false, so a `NaN` `q` (warming up, or a `NaN` in the window)
        // falls through to the cold path with the too-small ones.
        if q >= self.qmax * 1.0e-5 {
            return (q * self.inv).sqrt() * scale;
        }
        // Cold, and written inline over a shared slice (see `BollingerCore::push`).
        if self.inv.is_nan() {
            return f64::NAN;
        }
        let (q2, l2) = drawdown_sums(&self.buf, self.head, p2);
        self.q = q2;
        self.l = l2;
        self.qmax = q2;
        // Not `.max(0.0)`: that returns 0 for a NaN, and a window holding a NaN is NaN.
        let q2 = if q2 < 0.0 { 0.0 } else { q2 };
        let out = (q2 * self.inv).sqrt() * scale;
        if out.is_nan() { f64::NAN } else { out }
    }
}

/// The window (oldest value at `head`) as `(sum (c - peak)^2, sum (c - peak))`.
#[cold]
#[inline(never)]
fn drawdown_sums(buf: &[f64], head: usize, peak: f64) -> (f64, f64) {
    let (mut q, mut l) = (0.0_f64, 0.0_f64);
    for &v in buf[head..].iter().chain(&buf[..head]) {
        let d = v - peak;
        q += d * d;
        l += d;
    }
    (q, l)
}

// =================================== CHOP =====================================

/// Choppiness Index: `100 * log10(sum of true range / (highest high - lowest low)) / log10(n)` over
/// `n` bars, `NaN` where the range or the sum of true ranges is not positive.
///
/// The true ranges sit in a ring with a running sum, rebuilt every `ceil(512 / n)` laps and on the
/// spot when the result came out `NaN` after warm-up (a `NaN` bar, one that just left, or a flat
/// window, which is `NaN` by definition and is simply rebuilt each bar it lasts). The extremes are
/// [`HiLo`]'s, which is also what makes the warm-up `NaN`: a window with no range is not positive.
/// The one `log10` is `libm`'s, as it always was, so the value is the same number on every platform.
#[derive(Clone)]
pub(crate) struct ChopCore {
    win: HiLo,
    prev_close: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    sum: f64,
    seen: bool,
    live: bool,
    /// `100 / log10(n)`.
    scale: f64,
}

impl ChopCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        ChopCore {
            win: HiLo::new(n),
            prev_close: f64::NAN,
            buf: vec![0.0; n.max(1)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            sum: 0.0,
            seen: false,
            live: n != 0,
            scale: 100.0 / libm::log10(n as f64),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
        self.prev_close = f64::NAN;
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.sum = 0.0;
        self.seen = false;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let tr = true_range(high, low, self.prev_close);
        self.prev_close = close;
        let [hh, ll] = self.win.push(high, low);
        let old = std::mem::replace(&mut self.buf[self.head], tr);
        self.head += 1;
        self.sum += tr - old;
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                // The head is 0, so the ring is already oldest-to-newest.
                self.sum = self.buf.iter().fold(0.0, |a, &v| a + v);
                self.laps = 0;
            }
            self.seen = self.live;
        }
        let rng = hh - ll;
        if rng > 0.0 && self.sum > 0.0 {
            return self.scale * libm::log10(self.sum / rng);
        }
        // Warming up, a flat window, a `NaN` in it, or a stale carried sum after one left.
        if !self.seen {
            return f64::NAN;
        }
        self.sum = ring_sum(&self.buf, self.head);
        if rng > 0.0 && self.sum > 0.0 {
            return self.scale * libm::log10(self.sum / rng);
        }
        f64::NAN
    }
}

// ============================ REALIZED VOLATILITY =============================

/// RiskMetrics EWMA volatility (zero mean): the first return seeds the variance at `r^2`, every later
/// one folds `lambda * v + (1 - lambda) * r^2`; the value is `sqrt(v)`. A bar with an unusable close,
/// or after one, has no return: it is `NaN` and leaves the variance as it was.
#[derive(Debug, Clone)]
pub(crate) struct EwmaVolCore {
    lambda: f64,
    one_minus: f64,
    prev: f64,
    v: f64,
    seeded: bool,
}

impl EwmaVolCore {
    #[inline]
    pub(crate) fn new(lambda: f64) -> Self {
        EwmaVolCore { lambda, one_minus: 1.0 - lambda, prev: f64::NAN, v: 0.0, seeded: false }
    }

    pub(crate) fn clear(&mut self) {
        self.prev = f64::NAN;
        self.v = 0.0;
        self.seeded = false;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        let prev = self.prev;
        let ok = usable(close);
        self.prev = if ok { close } else { f64::NAN };
        // `prev` is a usable close or `NaN`, and a comparison with `NaN` is false.
        if !(ok & (prev > 0.0)) {
            return f64::NAN;
        }
        let r = libm::log(close / prev);
        let r2 = r * r;
        let v = if self.seeded { self.lambda * self.v + self.one_minus * r2 } else { r2 };
        self.seeded = true;
        self.v = v;
        v.sqrt()
    }
}

/// The last `n` terms of a window estimator with a running sum, `sqrt(scale * sum / n)`: the ring
/// starts as zeros and the multiplier `scale / n` is `NaN` until the first lap ends. A sum that is
/// `NaN` (a bad term in the window, or one that just left), negative (the mean of an OHLC bar out of
/// order is `NaN` by the square root, as reported) or under a hundred thousandth of the largest sum
/// since the last rebuild (the carried value has lost its digits next to that history, and a window of
/// zero terms must be EXACTLY `0`) takes the cold rebuild. Terms are pushed only when they exist.
#[derive(Debug, Clone)]
pub(crate) struct MeanRoot {
    inv: f64,
    inv_n: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    sum: f64,
    /// The largest `sum` since the last rebuild.
    smax: f64,
}

impl MeanRoot {
    #[inline]
    pub(crate) fn new(n: usize, scale: f64) -> Self {
        MeanRoot {
            inv: f64::NAN,
            inv_n: scale * inverse(n),
            buf: vec![0.0; n.max(1)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            sum: 0.0,
            smax: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.sum = 0.0;
        self.smax = 0.0;
        self.inv = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, term: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], term);
        self.head += 1;
        self.sum += term - old;
        if self.sum > self.smax {
            self.smax = self.sum;
        }
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                self.sum = self.buf.iter().fold(0.0, |a, &v| a + v);
                self.smax = self.sum;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        // A comparison with a `NaN` is false, so a `NaN` sum falls through to the cold path with the
        // negative and the too-small ones.
        if self.sum >= self.smax * 1.0e-5 {
            let out = (self.sum * self.inv).sqrt();
            if !out.is_nan() {
                return out;
            }
        }
        if self.inv.is_nan() {
            return f64::NAN;
        }
        self.sum = ring_sum(&self.buf, self.head);
        self.smax = self.sum;
        let out = (self.sum * self.inv).sqrt();
        if out.is_nan() { f64::NAN } else { out }
    }
}

/// Parkinson's range estimator: `sqrt(mean(ln(H/L)^2) / (4 ln 2))` over the last `n` bars.
#[derive(Debug, Clone)]
pub(crate) struct ParkinsonCore {
    win: MeanRoot,
}

impl ParkinsonCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        ParkinsonCore { win: MeanRoot::new(n, 1.0 / (4.0 * LN_2)) }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> f64 {
        if !(usable(high) & usable(low)) {
            return f64::NAN;
        }
        let hl = libm::log(high / low);
        self.win.push(hl * hl)
    }
}

/// `2 ln 2 - 1`, Garman-Klass's weight on the squared open-to-close return.
const GARMAN_KLASS_CLOSE_WEIGHT: f64 = 2.0 * LN_2 - 1.0;

/// Garman-Klass: `sqrt(mean(0.5 ln(H/L)^2 - (2 ln 2 - 1) ln(C/O)^2))` over the last `n` bars.
#[derive(Debug, Clone)]
pub(crate) struct GarmanKlassCore {
    win: MeanRoot,
}

impl GarmanKlassCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        GarmanKlassCore { win: MeanRoot::new(n, 1.0) }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
        if !(usable(open) & usable(high) & usable(low) & usable(close)) {
            return f64::NAN;
        }
        let hl = libm::log(high / low);
        let co = libm::log(close / open);
        self.win.push(0.5 * (hl * hl) - GARMAN_KLASS_CLOSE_WEIGHT * (co * co))
    }
}

/// Rogers-Satchell's per-bar term `ln(H/C) ln(H/O) + ln(L/C) ln(L/O)` from the three logs against the
/// open that a bar needs anyway (`ho = ln(H/O)`, `lo = ln(L/O)`, `co = ln(C/O)`), with
/// `ln(H/C) = ho - co` and `ln(L/C) = lo - co`: three logarithms instead of four. The differences are
/// exact to the logs' own rounding (each log of a ratio already carries the ratio's half ulp of 1, so
/// the relative error of a term is that over the move, as for the ratio form).
#[inline(always)]
fn rogers_satchell_from_logs(ho: f64, lo: f64, co: f64) -> f64 {
    (ho - co) * ho + (lo - co) * lo
}

/// Rogers-Satchell: `sqrt(mean(ln(H/C) ln(H/O) + ln(L/C) ln(L/O)))` over the last `n` bars.
#[derive(Debug, Clone)]
pub(crate) struct RogersSatchellCore {
    win: MeanRoot,
}

impl RogersSatchellCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        RogersSatchellCore { win: MeanRoot::new(n, 1.0) }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
        if !(usable(open) & usable(high) & usable(low) & usable(close)) {
            return f64::NAN;
        }
        let co = libm::log(close / open);
        let (ho, lo) = (libm::log(high / open), libm::log(low / open));
        self.win.push(rogers_satchell_from_logs(ho, lo, co))
    }
}

/// Close-to-close volatility: the SAMPLE standard deviation of the last `n` log returns. A window of
/// bit-equal returns is exactly `0.0`.
///
/// The variance is [`StddevCore`](crate::fast_d2::StddevCore)'s: running sums of the returns shifted
/// by the window's oldest at the last rebuild, rebuilt every `ceil(512 / n)` laps, and at once when
/// the sum of squared deviations is `NaN` or under a hundred thousandth of the largest second moment
/// since the last rebuild (which is also what makes a constant window exactly `0`: re-centred on the
/// repeated value every deviation is `0`). Only existing returns are pushed.
#[derive(Debug, Clone)]
pub(crate) struct CloseToCloseCore {
    prev: f64,
    win: SampleVar,
}

impl CloseToCloseCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        CloseToCloseCore { prev: f64::NAN, win: SampleVar::new(n) }
    }

    pub(crate) fn clear(&mut self) {
        self.prev = f64::NAN;
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        let prev = self.prev;
        let ok = usable(close);
        self.prev = if ok { close } else { f64::NAN };
        if !(ok & (prev > 0.0)) {
            return f64::NAN;
        }
        self.win.push(libm::log(close / prev))
    }
}

/// The sample standard deviation of the last `n` pushed values (see [`CloseToCloseCore`]).
#[derive(Debug, Clone)]
pub(crate) struct SampleVar {
    inv: f64,
    inv_n: f64,
    inv_nm1: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    s1: f64,
    s2: f64,
    smax: f64,
}

impl SampleVar {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        SampleVar {
            inv: f64::NAN,
            inv_n: inverse(n),
            inv_nm1: 1.0 / n.saturating_sub(1) as f64,
            buf: vec![0.0; n.max(1)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shift: 0.0,
            s1: 0.0,
            s2: 0.0,
            smax: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.shift = 0.0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.smax = 0.0;
        self.inv = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        self.s1 += d - o;
        self.s2 += d * d - o * o;
        if self.s2 > self.smax {
            self.smax = self.s2;
        }
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0.
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let (shift, s1, s2) = shifted_sums(&self.buf, 0);
                self.shift = shift;
                self.s1 = s1;
                self.s2 = s2;
                self.smax = s2;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let ss = self.s2 - self.s1 * (self.s1 * self.inv);
        if ss >= self.smax * 1.0e-5 {
            return (ss * self.inv_nm1).sqrt();
        }
        if self.inv.is_nan() {
            return f64::NAN;
        }
        let (shift, s1, s2) = shifted_sums(&self.buf, self.head);
        self.shift = shift;
        self.s1 = s1;
        self.s2 = s2;
        self.smax = s2;
        let ss = s2 - s1 * (s1 * self.inv);
        // Not `.max(0.0)`: that returns 0 for a NaN, and a window holding a NaN is NaN.
        let ss = if ss < 0.0 { 0.0 } else { ss };
        let out = (ss * self.inv_nm1).sqrt();
        if out.is_nan() { f64::NAN } else { out }
    }
}

/// Yang-Zhang: `sqrt(var_o + k var_c + (1 - k) mean(rs))` over the last `n` bars, `var_o` the sample
/// variance of the overnight returns `ln(O / previous C)`, `var_c` that of the open-to-close returns
/// `ln(C / O)`, `rs` the Rogers-Satchell terms, `k = 0.34 / (1.34 + (n + 1) / (n - 1))`.
///
/// One ring of `[overnight, open-to-close, rs]` triples with one head and one lap count; the two
/// variances carry shifted running sums as [`SampleVar`] does (a window of bit-equal returns is
/// exactly `0.0`), the Rogers-Satchell mean a plain running sum, and a cold rebuild re-centres both
/// shifts and re-sums all three. Only existing terms are pushed.
#[derive(Debug, Clone)]
pub(crate) struct YangZhangCore {
    prev: f64,
    inv: f64,
    inv_n: f64,
    inv_nm1: f64,
    k: f64,
    one_minus_k: f64,
    buf: Vec<[f64; 3]>,
    head: usize,
    laps: usize,
    laps_per: usize,
    sx: Shifted,
    sy: Shifted,
    sz: f64,
    /// The largest `sz` since the last rebuild.
    zmax: f64,
}

/// Shifted running sums of one stream: `shift`, `sum(x - shift)`, `sum((x - shift)^2)` and the
/// largest second moment since the last rebuild.
#[derive(Debug, Clone, Copy)]
struct Shifted {
    shift: f64,
    s1: f64,
    s2: f64,
    smax: f64,
}

impl Shifted {
    const ZERO: Shifted = Shifted { shift: 0.0, s1: 0.0, s2: 0.0, smax: 0.0 };

    /// The new value enters and the old one leaves.
    #[inline(always)]
    fn step(&mut self, x: f64, old: f64) {
        let (d, o) = (x - self.shift, old - self.shift);
        self.s1 += d - o;
        self.s2 += d * d - o * o;
        if self.s2 > self.smax {
            self.smax = self.s2;
        }
    }

    /// Sum of squared deviations from the mean, and whether it still has its digits.
    #[inline(always)]
    fn spread(&self, inv: f64) -> (f64, bool) {
        let ss = self.s2 - self.s1 * (self.s1 * inv);
        (ss, ss >= self.smax * 1.0e-5)
    }
}

/// A ring of triples (oldest value at `head`) as the two shifted streams and the plain sum of the
/// third.
#[cold]
#[inline(never)]
fn triple_sums(buf: &[[f64; 3]], head: usize) -> (Shifted, Shifted, f64) {
    let pick = |v: f64| if v.is_finite() { v } else { 0.0 };
    let (shx, shy) = (pick(buf[head][0]), pick(buf[head][1]));
    let (mut x, mut y, mut z) = (Shifted::ZERO, Shifted::ZERO, 0.0_f64);
    x.shift = shx;
    y.shift = shy;
    for v in buf[head..].iter().chain(&buf[..head]) {
        let (dx, dy) = (v[0] - shx, v[1] - shy);
        x.s1 += dx;
        x.s2 += dx * dx;
        y.s1 += dy;
        y.s2 += dy * dy;
        z += v[2];
    }
    x.smax = x.s2;
    y.smax = y.s2;
    (x, y, z)
}

impl YangZhangCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let nf = n as f64;
        let k = 0.34 / (1.34 + (nf + 1.0) / (nf - 1.0));
        YangZhangCore {
            prev: f64::NAN,
            inv: f64::NAN,
            inv_n: inverse(n),
            inv_nm1: 1.0 / n.saturating_sub(1) as f64,
            k,
            one_minus_k: 1.0 - k,
            buf: vec![[0.0; 3]; n.max(1)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            sx: Shifted::ZERO,
            sy: Shifted::ZERO,
            sz: 0.0,
            zmax: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.prev = f64::NAN;
        self.buf.fill([0.0; 3]);
        self.head = 0;
        self.laps = 0;
        self.sx = Shifted::ZERO;
        self.sy = Shifted::ZERO;
        self.sz = 0.0;
        self.zmax = 0.0;
        self.inv = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
        let prev = self.prev;
        let ok_close = usable(close);
        self.prev = if ok_close { close } else { f64::NAN };
        if !((prev > 0.0) & usable(open) & usable(high) & usable(low) & ok_close) {
            return f64::NAN;
        }
        let x = libm::log(open / prev);
        let y = libm::log(close / open);
        let (ho, lo) = (libm::log(high / open), libm::log(low / open));
        let z = rogers_satchell_from_logs(ho, lo, y);
        let old = std::mem::replace(&mut self.buf[self.head], [x, y, z]);
        self.head += 1;
        self.sx.step(x, old[0]);
        self.sy.step(y, old[1]);
        self.sz += z - old[2];
        if self.sz > self.zmax {
            self.zmax = self.sz;
        }
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let (sx, sy, sz) = triple_sums(&self.buf, 0);
                self.sx = sx;
                self.sy = sy;
                self.sz = sz;
                self.zmax = sz;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let (ssx, okx) = self.sx.spread(self.inv);
        let (ssy, oky) = self.sy.spread(self.inv);
        if okx & oky & (self.sz >= self.zmax * 1.0e-5) {
            let var = ssx * self.inv_nm1
                + self.k * (ssy * self.inv_nm1)
                + self.one_minus_k * (self.sz * self.inv);
            let out = var.sqrt();
            // A `NaN` is a negative mean (an OHLC bar out of order, reported as it is) or a stale
            // sum after a non-finite term left: both are decided by the exact rebuild below.
            if !out.is_nan() {
                return out;
            }
        }
        if self.inv.is_nan() {
            return f64::NAN;
        }
        let (sx, sy, sz) = triple_sums(&self.buf, self.head);
        self.sx = sx;
        self.sy = sy;
        self.sz = sz;
        self.zmax = sz;
        let (ssx, _) = sx.spread(self.inv);
        let (ssy, _) = sy.spread(self.inv);
        // Not `.max(0.0)`: that returns 0 for a NaN, and a window holding a NaN is NaN.
        let ssx = if ssx < 0.0 { 0.0 } else { ssx };
        let ssy = if ssy < 0.0 { 0.0 } else { ssy };
        let var =
            ssx * self.inv_nm1 + self.k * (ssy * self.inv_nm1) + self.one_minus_k * (sz * self.inv);
        let out = var.sqrt();
        if out.is_nan() { f64::NAN } else { out }
    }
}
