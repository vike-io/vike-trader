//! Fast cores for group `l` of the fourth wave of indicators (see `_followups/perf-conv4-addendum.md`):
//! `linearreg`, `linearreg_slope`, `linearreg_angle`, `linearreg_intercept`, `tsf`, `std_error`,
//! `std_error_bands`, `r_squared` (one regression core, [`RegCore`]), `var`, `zscore`, `skew`,
//! `kurtosis` ([`MomentCore`]), `mad` ([`MadCore`]), `rank_correlation` ([`RankCore`]), `hurst`
//! ([`HurstCore`]) and `autocorr` ([`AutoCore`]).
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds no
//! `&mut self` call in the hot part of its steady state, and is used by BOTH the streaming stepper and
//! the whole-series functions, so a stream and a batch agree bit for bit.
//!
//! What replaced what. Before this wave every statistic here re-ran its batch kernel over a retained
//! tail of history on every bar (a fold over the whole window, a sort for the rank correlation, an
//! allocation for the ranks). Now:
//!
//! - the regression, the moments and the lagged correlation keep running sums of the window SHIFTED by
//!   its mean at the last rebuild (so a price near a million with moves of a hundredth keeps
//!   its digits), updated in O(1) as the window slides and rebuilt from it every `ceil(512 / n)` laps;
//! - `mad` keeps the running mean and folds the absolute deviations over the ring on four lanes;
//! - `rank_correlation` keeps the window sorted (value, age) and reads the rank sum off it;
//! - `hurst` computes the rescaled range of ONE new block per size a bar and keeps it in a ring, because
//!   the blocks of the old kernel, counted back from the newest return, repeat every `size` bars.
//!
//! Two laws of the old kernels are kept on purpose, both by DECIDING the case instead of hoping the
//! arithmetic gets there. A statistic whose running sums cannot resolve the answer takes a cold path
//! that rebuilds the sums from the window on a fresh shift and, if they still cannot resolve it (a
//! window that is flat or nearly flat to the rounding of the sums, a `NaN` or an infinity in it, one bad
//! tick wide), answers with the OLD kernel's formulas evaluated on the window's differences from its
//! first value (the old kernels ran on the raw values, which loses seven digits at a price near a
//! million). So a window of identical values has a variance of exactly zero ([`is_constant_window`]),
//! and a window with a `NaN` in it answers `NaN` while the `NaN` is inside and exactly the textbook
//! number the update after it left.
//!
//! The running sums of the k-th powers carry a rounding error of about `eps * peak^k` that stays until
//! the next rebuild, where `peak` is the largest shifted value that went through them; a core therefore
//! also tracks `peak2` and asks for a rebuild when the window's own spread is small against it (a bad
//! tick, or a shift the window has drifted away from). See [`VAR_RATIO`].

use crate::math::{cube, quart, sq};
use crate::roll::{LEAVE, RING_CAP};
use crate::window::is_constant_window;

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const REBUILD: usize = 512;

/// Laps of a ring between two rebuilds of a running sum.
#[inline]
fn laps_per(n: usize) -> usize {
    REBUILD.div_ceil(n.max(1)).max(1)
}

/// The sums are trusted while the largest shifted value that went through them since they were last
/// rebuilt (its square is `peak2`) stays within this many standard deviations of the window: the running
/// sums of the k-th powers carry a rounding error that grows like `eps * peak^k` and stays until the
/// next rebuild, so a bad tick a thousand times its neighbours would otherwise leave a fourth-power
/// sum wrong by far more than the whole of the quantity it is the sum of (measured: a kurtosis off by
/// 3000x for a lap after such a tick). These are the squares of that many deviations for the variance
/// (and z-score), the skewness and the kurtosis: past them the answer is decided from the window.
const VAR_RATIO: f64 = 1.0e3;
const SKEW_RATIO: f64 = 3.0e1;
const KURT_RATIO: f64 = 1.5e1;
/// A residual (or a centred sum of squares) under this fraction of the sum of squares of the shifted
/// values is below what the running sums resolve.
const FIT_FLOOR: f64 = 1.0e-4;
/// The same for the lagged correlation, which divides by two of them.
const AUTO_FLOOR: f64 = 1.0e-3;
/// The sums of squares of the regression and the lagged correlation are trusted while a residual
/// (or centred sum of squares) is at least this times `n^2` times `peak2` (see [`VAR_RATIO`]): their
/// rounding error is about `eps * n^2 * peak2`, and this keeps it under 1e-10 of the residual.
const PEAK_GUARD: f64 = 1.0e-6;
/// A slope numerator under this fraction of the terms it is the difference of is decided from the window.
const SLOPE_FLOOR: f64 = 1.0e-4;
/// The slope numerator `n * sxy - sx * sy` carries a rounding error of about `eps * n^2 * peak` (`peak` the
/// largest shifted value that went through the sums since they were last rebuilt), and it stays until the
/// next rebuild: it is trusted while it is at least this times `n^2 * peak`, which keeps the error under
/// 1e-10 of it.
const SLOPE_PEAK: f64 = 1.0e-5;
/// A mean absolute deviation under this fraction of the mean's offset from the shift is rounding.
const MAD_FLOOR: f64 = 1.0e-12;
/// `math.degrees` as the old kernel spelled it: the radians times `180 / pi`.
const DEG: f64 = 180.0 / std::f64::consts::PI;

/// The window oldest to newest (the oldest value is at `head`).
#[inline]
fn ordered(buf: &[f64], head: usize) -> impl Iterator<Item = &f64> + Clone {
    buf[head..].iter().chain(&buf[..head])
}

/// The window oldest to newest as a vector: for the cold paths that need a contiguous slice.
#[cold]
#[inline(never)]
fn window_vec(buf: &[f64], head: usize) -> Vec<f64> {
    ordered(buf, head).copied().collect()
}

/// [`is_constant_window`] over a ring whose oldest value is at `head`.
fn constant(buf: &[f64], head: usize) -> bool {
    if head == 0 {
        is_constant_window(buf)
    } else {
        is_constant_window(&buf[head..]) && is_constant_window(&buf[..head]) && buf[head] == buf[0]
    }
}

/// The shift the sums are taken on: the mean of the window when it is finite (else `0`). A shift at the
/// centre of the window keeps the largest shifted value near two standard deviations, so the running
/// sums have room to drift before the guards (see [`VAR_RATIO`]) ask for a rebuild; the oldest value,
/// which a first version used, sits up to five deviations off the centre.
#[inline]
fn shift_of(buf: &[f64]) -> f64 {
    let m = buf.iter().sum::<f64>() / buf.len() as f64;
    if m.is_finite() { m } else { 0.0 }
}

// ------------------------------------------------------------------------------------------------
// the regression family: linearreg, slope, angle, intercept, tsf, std_error, std_error_bands, r_squared
// ------------------------------------------------------------------------------------------------

/// The value of the least-squares line at the newest bar.
pub(crate) const LINREG: u8 = 0;
/// The slope of the line.
pub(crate) const SLOPE: u8 = 1;
/// The slope as an angle in degrees.
pub(crate) const ANGLE: u8 = 2;
/// The line at the oldest bar of the window.
pub(crate) const INTERCEPT: u8 = 3;
/// The line projected one bar past the newest.
pub(crate) const TSF: u8 = 4;
/// The standard error of the estimate.
pub(crate) const STDERR: u8 = 5;
/// The squared correlation of the window with its index.
pub(crate) const RSQ: u8 = 6;
/// The regression value with bands a multiple of the standard error either side.
pub(crate) const BANDS: u8 = 7;

/// The shifted sums of a window of `n` values (oldest at `head`).
struct Fit {
    shift: f64,
    sy: f64,
    sxy: f64,
    syy: f64,
    /// The largest `d^2` of the window.
    peak2: f64,
}

impl Fit {
    /// The window as `(shift, sum(d), sum(i * d), sum(d^2))` with `d = y - shift`, `shift` being the
    /// mean when it is finite and `i` the place in the window from the oldest.
    #[cold]
    #[inline(never)]
    fn of(buf: &[f64], head: usize) -> Fit {
        let shift = shift_of(buf);
        let (mut sy, mut sxy, mut syy, mut peak2) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
        for (i, &v) in ordered(buf, head).enumerate() {
            let d = v - shift;
            sy += d;
            sxy += i as f64 * d;
            syy += d * d;
            peak2 = peak2.max(d * d);
        }
        Fit { shift, sy, sxy, syy, peak2 }
    }
}

/// The window oldest to newest as differences from its oldest value. A difference of two numbers of
/// the same order is exact, so a price near a million keeps its digits; the cold paths below are the
/// OLD kernels' formulas evaluated on these.
#[inline]
fn diffs(buf: &[f64], head: usize) -> impl Iterator<Item = f64> + Clone {
    let first = buf[head];
    ordered(buf, head).map(move |&v| v - first)
}

/// `statistics.py`'s `_ols` over the ordered window, on its differences from the oldest value:
/// `(slope, intercept at the oldest bar relative to it)` from the closed form
/// `b = (p * sum(xy) - sum(x) * sum(y)) / (p * sum(x^2) - sum(x)^2)`, `a = (sum(y) - b * sum(x)) / p`.
/// The integer `sum(x)` and `sum(x^2)` are computed exactly and then cast; the `sum(y)` and `sum(xy)`
/// are naive left folds. The old kernel's arithmetic, expression for expression, except that it ran
/// on the raw values, which at a price near a million loses seven digits to `p * sum(xy) - sum(x) *
/// sum(y)`.
fn ols_ordered(buf: &[f64], head: usize) -> (f64, f64) {
    let p = buf.len();
    let sx = (p * (p - 1)) as f64 / 2.0;
    let sx2 = (p * (p - 1) * (2 * p - 1)) as f64 / 6.0;
    let sy: f64 = diffs(buf, head).sum();
    let mut sxy = 0.0;
    for (i, d) in diffs(buf, head).enumerate() {
        sxy += i as f64 * d;
    }
    let denom = p as f64 * sx2 - sx * sx;
    if denom == 0.0 {
        return (0.0, sy / p as f64);
    }
    let b = (p as f64 * sxy - sx * sy) / denom;
    let a = (sy - b * sx) / p as f64;
    (b, a)
}

/// The old `std_error` of the ordered window: `sqrt(sum(resid^2) / (p - 2)) / sqrt(p)`, residuals taken
/// from the fitted line directly (`b` and `a` from [`ols_ordered`]); `0` for a window of 2 or fewer.
fn std_error_ordered(buf: &[f64], head: usize, b: f64, a: f64) -> f64 {
    let p = buf.len();
    let mut residuals_sq = 0.0;
    for (j, d) in diffs(buf, head).enumerate() {
        residuals_sq += sq(d - (a + b * j as f64));
    }
    if p > 2 {
        let mse = residuals_sq / (p - 2) as f64;
        mse.sqrt() / (p as f64).sqrt()
    } else {
        0.0
    }
}

/// The old `r_squared` of the ordered window: the product of the two regression slopes of the centred
/// sums, `NaN` for a window of identical values.
fn r_squared_ordered(buf: &[f64], head: usize) -> f64 {
    if constant(buf, head) {
        return f64::NAN;
    }
    let n = buf.len() as f64;
    // The index grid 0..n has the exact sum n * (n - 1) / 2.
    let mx = (n * (n - 1.0) / 2.0) / n;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the old kernel folded.
    let my = diffs(buf, head).sum::<f64>() / n;
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (k, v) in diffs(buf, head).enumerate() {
        let dx = k as f64 - mx;
        let dy = v - my;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    if sxx > 0.0 && syy > 0.0 {
        let r2 = (sxy / sxx) * (sxy / syy);
        if r2 > 1.0 { 1.0 } else { r2 }
    } else {
        f64::NAN
    }
}

/// What the OLD kernels answered for the ordered window, for the regression kind `K`.
fn reg_old<const K: u8>(buf: &[f64], head: usize, mult: f64) -> [f64; 3] {
    let p = buf.len();
    if K == RSQ {
        return [r_squared_ordered(buf, head), f64::NAN, f64::NAN];
    }
    let first = buf[head];
    let (b, a) = ols_ordered(buf, head);
    let lr = (a + b * (p - 1) as f64) + first;
    match K {
        LINREG => [lr, f64::NAN, f64::NAN],
        SLOPE => [b, f64::NAN, f64::NAN],
        ANGLE => [libm::atan(b) * DEG, f64::NAN, f64::NAN],
        INTERCEPT => [a + first, f64::NAN, f64::NAN],
        TSF => [(a + b * p as f64) + first, f64::NAN, f64::NAN],
        STDERR => [std_error_ordered(buf, head, b, a), f64::NAN, f64::NAN],
        _ => {
            let se = std_error_ordered(buf, head, b, a);
            if !lr.is_nan() && !se.is_nan() {
                [lr + mult * se, lr, lr - mult * se]
            } else {
                [f64::NAN, lr, f64::NAN]
            }
        }
    }
}

/// Rebuild the sums from the window and answer with the old kernel's formulas.
#[cold]
#[inline(never)]
fn reg_cold<const K: u8>(buf: &[f64], head: usize, mult: f64) -> (Fit, [f64; 3]) {
    (Fit::of(buf, head), reg_old::<K>(buf, head, mult))
}

/// The least-squares line through the last `n` values against `0..n`, and what is read off it: kind `K`
/// is one of [`LINREG`], [`SLOPE`], [`ANGLE`], [`INTERCEPT`], [`TSF`], [`STDERR`], [`RSQ`], [`BANDS`].
///
/// Running sums of the window SHIFTED by its mean at the last rebuild: `sum(y)`, `sum(i * y)`
/// with `i` the place in the window, updated in O(1) as the window slides
/// (`sxy' = sxy - (sy - y_old) + (n - 1) * y_new`), and `sum(y^2)` for the kinds that read the residual.
/// The slope and intercept are closed forms of them; the residual sum of squares is
/// `syy - sy^2 / n - slope * (sxy - sx * sy / n)`.
///
/// A result the sums cannot resolve takes the cold path, which rebuilds them and answers with the OLD
/// kernel's arithmetic over the window: a `NaN` in the window (the answer is `NaN` while it is inside
/// and exact the update after it left); a slope numerator that is the difference of two nearly equal
/// terms (the value, slope, angle, intercept and forecast kinds: a flat window above all); a residual
/// or centred sum of squares under [`FIT_FLOOR`] of the sum of squares (the standard error and
/// r-squared kinds: a window that is a straight line, or flat above all).
///
/// Warm-up is a `NaN` `1 / denominator` until the first lap of the ring ends. A window of fewer than two
/// values never answers.
#[derive(Clone)]
pub(crate) struct RegCore<const K: u8> {
    nf: f64,
    n1: f64,
    sx: f64,
    inv_n: f64,
    inv_denom: f64,
    inv_denom_n: f64,
    c_se: f64,
    mult: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    sy: f64,
    sxy: f64,
    syy: f64,
    peak2: f64,
    guard: f64,
    /// The largest `|d|` that went through the sums since their last rebuild, for the slope kinds (the kinds
    /// that read the residual keep `peak2`).
    apeak: f64,
    /// `SLOPE_PEAK * n^2`: the slope numerator must beat this times `apeak`.
    slope_guard: f64,
}

impl<const K: u8> RegCore<K> {
    /// A core over `n` values; `mult` is read only by [`BANDS`].
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        let n = n.min(RING_CAP);
        let nf = n as f64;
        let n2 = nf * nf;
        RegCore {
            nf,
            n1: nf - 1.0,
            sx: nf * (nf - 1.0) / 2.0,
            inv_n: 1.0 / nf,
            inv_denom: f64::NAN,
            // A window under two values never leaves warm-up: every output is NaN.
            inv_denom_n: if n >= 2 { 1.0 / (n2 * (n2 - 1.0) / 12.0) } else { f64::NAN },
            // A window of 2 or fewer has a standard error of exactly zero (the cold path says so).
            c_se: if n > 2 { 1.0 / (nf * (nf - 2.0)) } else { 0.0 },
            mult,
            buf: vec![0.0; n.clamp(1, RING_CAP)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shift: 0.0,
            sy: 0.0,
            sxy: 0.0,
            syy: 0.0,
            peak2: 0.0,
            guard: PEAK_GUARD * n2,
            apeak: 0.0,
            slope_guard: SLOPE_PEAK * n2,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.shift = 0.0;
        self.sy = 0.0;
        self.sxy = 0.0;
        self.syy = 0.0;
        self.peak2 = 0.0;
        self.apeak = 0.0;
        self.inv_denom = f64::NAN;
    }

    #[inline(always)]
    fn step(&mut self, x: f64) -> [f64; 3] {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        self.sxy += self.n1 * d - (self.sy - o);
        self.sy += d - o;
        if K >= STDERR {
            let dd = d * d;
            self.syy += dd - o * o;
            self.peak2 = self.peak2.max(dd);
        } else {
            let ad = d.abs();
            self.apeak = if ad > self.apeak { ad } else { self.apeak };
        }
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0 on raw values.
            if self.laps >= self.laps_per || self.inv_denom.is_nan() {
                let f = Fit::of(&self.buf, 0);
                self.shift = f.shift;
                self.sy = f.sy;
                self.sxy = f.sxy;
                self.syy = f.syy;
                self.peak2 = f.peak2;
                self.apeak = f.peak2.sqrt();
                self.laps = 0;
            }
            self.inv_denom = self.inv_denom_n;
        }
        let slope = (self.nf * self.sxy - self.sx * self.sy) * self.inv_denom;
        // The centred sums of squares and cross products, for the kinds that read the residual.
        let (cxy, cyy, ssr) = if K >= STDERR {
            let cxy = self.sxy - self.sx * self.sy * self.inv_n;
            let cyy = self.syy - self.sy * self.sy * self.inv_n;
            (cxy, cyy, cyy - slope * cxy)
        } else {
            (0.0, 0.0, 0.0)
        };
        let resolved = if K == STDERR || K == BANDS {
            ssr > FIT_FLOOR * self.syy && ssr > self.guard * self.peak2
        } else if K == RSQ {
            cyy > FIT_FLOOR * self.syy && cyy > self.guard * self.peak2
        } else {
            // The slope kinds: a numerator that is the difference of two nearly equal terms is a
            // window that is flat (or a line through the origin of the shift) to the rounding of the
            // sums, and a flat window must give exactly what the old kernel gave. The numerator is also
            // trusted only while it beats the rounding error of the largest shifted value that went
            // through the sums since their last rebuild (a bad tick that has left; see [`SLOPE_PEAK`]).
            let (t1, t2) = (self.nf * self.sxy, self.sx * self.sy);
            let num = t1 - t2;
            (num.abs() > SLOPE_FLOOR * (t1.abs() + t2.abs()))
                & (num.abs() > self.slope_guard * self.apeak)
        };
        if !resolved {
            // Warming up, a `NaN` in the window, a stale sum, or a window the sums cannot resolve.
            // Cold, and over a shared slice: a `&mut self` method call here would keep the sums in
            // memory across every iteration of a caller's loop.
            if self.inv_denom.is_nan() {
                return [f64::NAN; 3];
            }
            let (f, out) = reg_cold::<K>(&self.buf, self.head, self.mult);
            self.shift = f.shift;
            self.sy = f.sy;
            self.sxy = f.sxy;
            self.syy = f.syy;
            self.peak2 = f.peak2;
            self.apeak = f.peak2.sqrt();
            return out;
        }
        let a = (self.sy - slope * self.sx) * self.inv_n;
        match K {
            LINREG => [self.shift + (a + slope * self.n1), f64::NAN, f64::NAN],
            SLOPE => [slope, f64::NAN, f64::NAN],
            ANGLE => [libm::atan(slope) * DEG, f64::NAN, f64::NAN],
            INTERCEPT => [self.shift + a, f64::NAN, f64::NAN],
            TSF => [self.shift + (a + slope * self.nf), f64::NAN, f64::NAN],
            STDERR => [(ssr * self.c_se).sqrt(), f64::NAN, f64::NAN],
            // 1 / (n (n^2 - 1) / 12), NaN until the first lap ends.
            RSQ => {
                let r2 = (cxy * (self.inv_denom * self.nf)) * (cxy / cyy);
                // Not `f64::min`: it returns the other operand for a `NaN`, and the warm-up is `NaN`.
                [if r2 > 1.0 { 1.0 } else { r2 }, f64::NAN, f64::NAN]
            }
            _ => {
                let mid = self.shift + (a + slope * self.n1);
                let se = (ssr * self.c_se).sqrt();
                [mid + self.mult * se, mid, mid - self.mult * se]
            }
        }
    }

    /// Push `x`; the statistic of the last `n` values, `NaN` until `n` have been pushed. For every kind
    /// but [`BANDS`].
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        self.step(x)[0]
    }

    /// Push `x`; `[upper, mid, lower]` of the last `n` values. For [`BANDS`].
    #[inline(always)]
    pub(crate) fn push3(&mut self, x: f64) -> [f64; 3] {
        self.step(x)
    }
}

// ------------------------------------------------------------------------------------------------
// the moments of a window: var, zscore, skew, kurtosis
// ------------------------------------------------------------------------------------------------

/// The population variance of the window.
pub(crate) const VAR: u8 = 0;
/// The newest value's z-score against the window.
pub(crate) const ZSCORE: u8 = 1;
/// The bias-corrected sample skewness.
pub(crate) const SKEW: u8 = 2;
/// The bias-corrected excess kurtosis.
pub(crate) const KURT: u8 = 3;

/// The window as `shift` and `sum(d)`, `sum(d^2)`, `sum(d^3)`, `sum(d^4)` with `d = y - shift`, the shift
/// being the mean of the window when it is finite.
struct Power {
    shift: f64,
    s1: f64,
    s2: f64,
    s3: f64,
    s4: f64,
    /// The largest `d^2` of the window.
    peak2: f64,
}

impl Power {
    #[cold]
    #[inline(never)]
    fn of(buf: &[f64], head: usize) -> Power {
        let shift = shift_of(buf);
        let (mut s1, mut s2, mut s3, mut s4, mut peak2) =
            (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
        for &v in ordered(buf, head) {
            let d = v - shift;
            let d2 = d * d;
            s1 += d;
            s2 += d2;
            s3 += d2 * d;
            s4 += d2 * d2;
            peak2 = peak2.max(d2);
        }
        Power { shift, s1, s2, s3, s4, peak2 }
    }
}

/// The old rolling population variance of the ordered window: `0` for a window of identical values,
/// else the mean and then `sum((x - mean)^2) / n`, on the differences from the oldest value.
fn var_ordered(buf: &[f64], head: usize) -> f64 {
    if constant(buf, head) {
        return 0.0;
    }
    let n = buf.len() as f64;
    let mean = diffs(buf, head).sum::<f64>() / n;
    diffs(buf, head).map(|v| sq(v - mean)).sum::<f64>() / n
}

/// The old rolling z-score of `x` against the ordered window.
fn zscore_ordered(buf: &[f64], head: usize, x: f64) -> f64 {
    let sd = var_ordered(buf, head).sqrt();
    if sd.is_nan() || sd == 0.0 {
        return f64::NAN;
    }
    let mean = diffs(buf, head).sum::<f64>() / buf.len() as f64;
    let z = ((x - buf[head]) - mean) / sd;
    if z.is_nan() { f64::NAN } else { z }
}

/// The old bias-corrected sample skewness of the ordered window (`c1 = sqrt(p (p - 1)) / (p - 2)`).
fn skew_ordered(buf: &[f64], head: usize, c1: f64) -> f64 {
    let p = buf.len() as f64;
    let mean = diffs(buf, head).sum::<f64>() / p;
    let m2 = diffs(buf, head).map(|v| (v - mean) * (v - mean)).sum::<f64>() / p;
    if m2 == 0.0 {
        return 0.0;
    }
    let sd = m2.sqrt();
    let m3 = diffs(buf, head).map(|v| cube(v - mean)).sum::<f64>() / p;
    (m3 / cube(sd)) * c1
}

/// The old bias-corrected excess kurtosis of the ordered window.
fn kurtosis_ordered(buf: &[f64], head: usize, c1: f64, c2: f64) -> f64 {
    let p = buf.len() as f64;
    let mean = diffs(buf, head).sum::<f64>() / p;
    let m2 = diffs(buf, head).map(|v| (v - mean) * (v - mean)).sum::<f64>() / p;
    if m2 == 0.0 {
        return 0.0;
    }
    let sd = m2.sqrt();
    let m4 = diffs(buf, head).map(|v| quart(v - mean)).sum::<f64>() / p;
    let pop_kurt = m4 / quart(sd);
    c1 * pop_kurt - c2
}

/// The moment of kind `K` from the central pieces of the shifted power sums: `m` the mean offset from the
/// shift, `ms` the mean square, `var` the variance, `s3` and `s4` the third and fourth power sums, `inv`
/// the reciprocal of the window and `xd` the newest value minus the shift.
#[inline(always)]
fn moment_eval<const K: u8>(
    m: f64,
    ms: f64,
    var: f64,
    s3: f64,
    s4: f64,
    inv: f64,
    xd: f64,
    c1: f64,
    c2: f64,
) -> f64 {
    match K {
        VAR => var,
        ZSCORE => (xd - m) / var.sqrt(),
        SKEW => {
            let m3 = s3 * inv - 3.0 * m * ms + 2.0 * m * m * m;
            (m3 / (var * var.sqrt())) * c1
        }
        _ => {
            let m4 = s4 * inv - 4.0 * m * (s3 * inv) + 6.0 * m * m * ms - 3.0 * m * m * m * m;
            c1 * (m4 / (var * var)) - c2
        }
    }
}

/// Rebuild the power sums on a fresh shift and answer from them when they now resolve the window (a
/// stale shift or a bad tick that has left); otherwise answer with the old kernel's formulas (a flat
/// window, a `NaN` or an infinity in it, a window one bad tick wide).
#[cold]
#[inline(never)]
fn moments_cold<const K: u8>(
    buf: &[f64],
    head: usize,
    x: f64,
    c1: f64,
    c2: f64,
    inv: f64,
    ratio: f64,
) -> (Power, f64) {
    let p = Power::of(buf, head);
    let m = p.s1 * inv;
    let ms = p.s2 * inv;
    let var = ms - m * m;
    if var * ratio > p.peak2 {
        let out = moment_eval::<K>(m, ms, var, p.s3, p.s4, inv, x - p.shift, c1, c2);
        return (p, out);
    }
    let out = match K {
        VAR => var_ordered(buf, head),
        ZSCORE => zscore_ordered(buf, head, x),
        SKEW => skew_ordered(buf, head, c1),
        _ => kurtosis_ordered(buf, head, c1, c2),
    };
    (p, out)
}

/// A moment of the last `n` values, kind `K` one of [`VAR`], [`ZSCORE`], [`SKEW`], [`KURT`]: running
/// power sums of the window shifted by its mean at the last rebuild, rebuilt from the ring
/// every `ceil(512 / n)` laps and on the spot when the result cannot be resolved from them.
///
/// Warm-up is the `NaN` multiplier of [`crate::roll::SmaCore`]: `NaN` until the first lap of the ring
/// ends. A variance that the sums cannot resolve (negative, `NaN`, or under a fraction of the largest
/// squared shifted value that went through them, [`VAR_RATIO`] and its siblings: a flat window, a
/// bad tick) is not trusted and not clamped; the cold path rebuilds the sums and answers with the old kernel's two passes, whose
/// constant-window law gives EXACTLY zero for a window of bit-identical values. A skewness needs 3 values
/// and a kurtosis 4: a shorter window never answers.
#[derive(Clone)]
pub(crate) struct MomentCore<const K: u8> {
    inv: f64,
    inv_n: f64,
    c1: f64,
    c2: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    s1: f64,
    s2: f64,
    s3: f64,
    s4: f64,
    peak2: f64,
    ratio: f64,
}

impl<const K: u8> MomentCore<K> {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.min(RING_CAP);
        let min = match K {
            SKEW => 3,
            KURT => 4,
            _ => 1,
        };
        let p = n as f64;
        let (c1, c2) = if n < min {
            (f64::NAN, f64::NAN)
        } else if K == SKEW {
            (((n * (n - 1)) as f64).sqrt() / (n - 2) as f64, 0.0)
        } else if K == KURT {
            let denom = ((n - 2) * (n - 3)) as f64;
            (((n + 1) * (n - 1)) as f64 / denom, 3.0 * ((n - 1) * (n - 1)) as f64 / denom)
        } else {
            (0.0, 0.0)
        };
        MomentCore {
            inv: f64::NAN,
            // A window too short never leaves warm-up: every output is NaN.
            inv_n: if n < min { f64::NAN } else { 1.0 / p },
            c1,
            c2,
            buf: vec![0.0; n.clamp(1, RING_CAP)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shift: 0.0,
            s1: 0.0,
            s2: 0.0,
            s3: 0.0,
            s4: 0.0,
            peak2: 0.0,
            ratio: match K {
                VAR | ZSCORE => VAR_RATIO,
                SKEW => SKEW_RATIO,
                _ => KURT_RATIO,
            },
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.shift = 0.0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.s3 = 0.0;
        self.s4 = 0.0;
        self.peak2 = 0.0;
        self.inv = f64::NAN;
    }

    /// Push `x`; the moment of the last `n` values, `NaN` until `n` have been pushed.
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        let (d2, o2) = (d * d, o * o);
        self.peak2 = self.peak2.max(d2);
        self.s1 += d - o;
        self.s2 += d2 - o2;
        if K >= SKEW {
            self.s3 += d2 * d - o2 * o;
        }
        if K == KURT {
            self.s4 += d2 * d2 - o2 * o2;
        }
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0 on raw values.
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let p = Power::of(&self.buf, 0);
                self.shift = p.shift;
                self.s1 = p.s1;
                self.s2 = p.s2;
                self.s3 = p.s3;
                self.s4 = p.s4;
                self.peak2 = p.peak2;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let m = self.s1 * self.inv;
        let ms = self.s2 * self.inv;
        let var = ms - m * m;
        let resolved = var * self.ratio > self.peak2;
        if !resolved {
            // Warming up, a `NaN` in the window, a stale sum, or a window flat to the rounding of the
            // sums. Cold, and over a shared slice (see `RegCore`).
            if self.inv.is_nan() {
                return f64::NAN;
            }
            let (p, out) =
                moments_cold::<K>(&self.buf, self.head, x, self.c1, self.c2, self.inv, self.ratio);
            self.shift = p.shift;
            self.s1 = p.s1;
            self.s2 = p.s2;
            self.s3 = p.s3;
            self.s4 = p.s4;
            self.peak2 = p.peak2;
            return out;
        }
        // The newest value against the window, both on the shift the sums are on NOW (a rebuild
        // above may have moved it).
        moment_eval::<K>(m, ms, var, self.s3, self.s4, self.inv, x - self.shift, self.c1, self.c2)
    }
}

// ------------------------------------------------------------------------------------------------
// mad
// ------------------------------------------------------------------------------------------------

/// Re-centre the ring: the shift becomes the mean of the raw window (when finite), `dev` is every raw
/// value minus it, and the sum of `dev` is returned with the shift.
#[cold]
#[inline(never)]
fn mad_rebuild(raw: &[f64], dev: &mut [f64]) -> (f64, f64) {
    let shift = shift_of(raw);
    let mut s1 = 0.0_f64;
    for (d, &v) in dev.iter_mut().zip(raw) {
        *d = v - shift;
    }
    // Summed in ring order, which only the rebuild sees; the ring is read in storage order throughout.
    for &d in dev.iter() {
        s1 += d;
    }
    (shift, s1)
}

/// The old mean absolute deviation of the ordered window, on the differences from the oldest value.
fn mad_ordered(buf: &[f64], head: usize) -> f64 {
    let p = buf.len() as f64;
    let mean = diffs(buf, head).sum::<f64>() / p;
    diffs(buf, head).map(|x| (x - mean).abs()).sum::<f64>() / p
}

/// Rebuild the shifted ring and answer with the old kernel's formulas.
#[cold]
#[inline(never)]
fn mad_cold(raw: &[f64], dev: &mut [f64], head: usize) -> (f64, f64, f64) {
    let (shift, s1) = mad_rebuild(raw, dev);
    (shift, s1, mad_ordered(raw, head))
}

/// `sum(|d - m1|)` over the shifted ring on four lanes (`m1` is the mean's offset from the shift). The
/// ring is read in storage order: the sum does not care about the order, and a fixed slot to lane map
/// keeps every way in bit-identical. The lanes are four named accumulators: an array of them indexed in a
/// loop was kept in memory (measured: 9.8 ns against 6.3 ns for a ring of 20).
#[inline]
fn abs_dev(dev: &[f64], m1: f64) -> f64 {
    let (mut a0, mut a1, mut a2, mut a3) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    let mut chunks = dev.chunks_exact(4);
    for c in &mut chunks {
        a0 += (c[0] - m1).abs();
        a1 += (c[1] - m1).abs();
        a2 += (c[2] - m1).abs();
        a3 += (c[3] - m1).abs();
    }
    let mut tail = 0.0_f64;
    for &d in chunks.remainder() {
        tail += (d - m1).abs();
    }
    ((a0 + a1) + (a2 + a3)) + tail
}

/// The mean absolute deviation about the mean of the last `n` values: the mean is a running sum of the
/// window shifted by its mean at the last rebuild, the deviations are folded over a second ring that holds
/// the window shifted (`|(y - shift) - (mean - shift)|`, so a price near a million keeps its digits and
/// the fold has one subtraction a value). The raw ring is only read when the shift is renewed.
///
/// Warm-up is the `NaN` multiplier of [`crate::roll::SmaCore`]. A result that is `NaN`, or is below
/// [`MAD_FLOOR`] of the mean's offset from the shift (a window flat to the rounding of the sum), takes
/// the cold path: rebuild the sum and answer with the old kernel's two passes.
#[derive(Clone)]
pub(crate) struct MadCore {
    inv: f64,
    inv_n: f64,
    raw: Vec<f64>,
    dev: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    s1: f64,
}

impl MadCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.min(RING_CAP);
        MadCore {
            inv: f64::NAN,
            inv_n: if n == 0 { f64::NAN } else { 1.0 / n as f64 },
            raw: vec![0.0; n.clamp(1, RING_CAP)],
            dev: vec![0.0; n.clamp(1, RING_CAP)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shift: 0.0,
            s1: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.raw.fill(0.0);
        self.dev.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.shift = 0.0;
        self.s1 = 0.0;
        self.inv = f64::NAN;
    }

    /// Push `x`; the mean absolute deviation of the last `n` values, `NaN` until `n` have been pushed.
    ///
    /// The fold over the shifted ring runs BEFORE the new value is stored into it and the term of the
    /// leaving value is swapped for the new one afterwards: a vector load that overlaps a store made a few
    /// cycles earlier cannot be forwarded and waits for it (measured: the fold of a ring of 20 cost
    /// 13 ns after the store and 5 ns before it).
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let h = self.head;
        let d = x - self.shift;
        self.raw[h] = x;
        self.head = h + 1;
        let wrap = self.head == self.raw.len();
        if wrap {
            self.head = 0;
            self.laps += 1;
        }
        let (sum, m1, gone) = if wrap && (self.laps >= self.laps_per || self.inv.is_nan()) {
            // Renew the shift: `dev` is rebuilt from `raw`, which already holds `x`.
            let (shift, s1) = mad_rebuild(&self.raw, &mut self.dev);
            self.shift = shift;
            self.s1 = s1;
            self.laps = 0;
            self.inv = self.inv_n;
            let m1 = s1 * self.inv;
            (abs_dev(&self.dev, m1), m1, 0.0)
        } else {
            if wrap {
                self.inv = self.inv_n;
            }
            let old = self.dev[h];
            self.s1 += d - old;
            let m1 = self.s1 * self.inv;
            let sum = abs_dev(&self.dev, m1) - (old - m1).abs() + (d - m1).abs();
            self.dev[h] = d;
            (sum, m1, old)
        };
        if self.inv.is_nan() {
            return f64::NAN;
        }
        let out = sum * self.inv;
        // Joined with `&`: and the value that has just left must not dwarf the deviation (a bad tick, gone:
        // its rounding error is in the mean).
        let resolved = (out > MAD_FLOOR * m1.abs()) & (gone.abs() * self.inv <= out * LEAVE);
        if !resolved {
            let (shift, s1, out) = mad_cold(&self.raw, &mut self.dev, self.head);
            self.shift = shift;
            self.s1 = s1;
            return out;
        }
        out
    }
}

// ------------------------------------------------------------------------------------------------
// rank_correlation
// ------------------------------------------------------------------------------------------------

/// Spearman's rank correlation of the last `n` values against their place in the window, scaled to
/// `[-100, 100]`: `100 * (1 - 6 * sum(d^2) / (n (n^2 - 1)))`, `d` the difference of the two ranks,
/// ties ranked by age (the old kernel's stable sort).
///
/// The window is kept sorted by (value, age) in two parallel arrays and moves ONE element a bar. With
/// ranks a permutation of `1..=n` the sum of squared rank differences is `2 * sum(k^2) - 2 * sum(rank *
/// place)`, an integer, so the answer is exactly the old kernel's. A `NaN` never enters the arrays;
/// `nans` counts the numbers missing from the window (the not yet filled slots included), and the
/// answer is `NaN` while any is missing. (The old kernel panicked on a `NaN` in the window.)
#[derive(Clone)]
pub(crate) struct RankCore {
    n: usize,
    ring: Vec<f64>,
    head: usize,
    vals: Vec<f64>,
    ids: Vec<i64>,
    cnt: usize,
    nans: usize,
    /// The number of values pushed.
    t: i64,
    sum_r: i64,
    sum_r2x2: i64,
    denom: i64,
    denom_f: f64,
}

impl RankCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.min(RING_CAP);
        let p = n as i64;
        let denom = p.wrapping_mul(p.wrapping_mul(p).wrapping_sub(1));
        RankCore {
            n,
            ring: vec![f64::NAN; n.clamp(1, RING_CAP)],
            head: 0,
            vals: vec![0.0; n.clamp(1, RING_CAP)],
            ids: vec![0; n.clamp(1, RING_CAP)],
            cnt: 0,
            // A window under two values never answers.
            nans: if n >= 2 { n } else { usize::MAX / 2 },
            t: 0,
            sum_r: p.wrapping_mul(p + 1) / 2,
            sum_r2x2: p.wrapping_mul(p + 1).wrapping_mul(2 * p + 1) / 3,
            denom,
            denom_f: denom as f64,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.ring.fill(f64::NAN);
        self.head = 0;
        self.cnt = 0;
        self.nans = if self.n >= 2 { self.n } else { usize::MAX / 2 };
        self.t = 0;
    }

    /// A `NaN` is leaving (a not yet filled slot included) or arriving.
    #[cold]
    #[inline(never)]
    fn nan_step(&mut self, old: f64, x: f64, id: i64) {
        if old.is_nan() {
            self.nans = self.nans.saturating_sub(1);
        } else {
            let at = self.vals[..self.cnt].partition_point(|&v| v < old);
            self.vals.copy_within(at + 1..self.cnt, at);
            self.ids.copy_within(at + 1..self.cnt, at);
            self.cnt -= 1;
        }
        if x.is_nan() {
            self.nans += 1;
        } else {
            let at = self.vals[..self.cnt].partition_point(|&v| v <= x);
            self.vals.copy_within(at..self.cnt, at + 1);
            self.ids.copy_within(at..self.cnt, at + 1);
            self.vals[at] = x;
            self.ids[at] = id;
            self.cnt += 1;
        }
    }

    /// Push `x`; the rank correlation of the last `n` values, `NaN` until `n` have been pushed and
    /// while a `NaN` is inside the window.
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.ring[self.head], x);
        self.head += 1;
        if self.head == self.ring.len() {
            self.head = 0;
        }
        let id = self.t;
        self.t += 1;
        if !(old.is_nan() | x.is_nan()) {
            // Take `old` (the first of its equals: it is the oldest) out and put `x` (after its equals:
            // it is the newest) in, moving only the elements between them.
            let (vals, ids) = (&mut self.vals[..self.cnt], &mut self.ids[..self.cnt]);
            let i = vals.partition_point(|&v| v < old);
            let j = vals.partition_point(|&v| v <= x);
            if j > i {
                vals.copy_within(i + 1..j, i);
                ids.copy_within(i + 1..j, i);
                vals[j - 1] = x;
                ids[j - 1] = id;
            } else {
                vals.copy_within(j..i, j + 1);
                ids.copy_within(j..i, j + 1);
                vals[j] = x;
                ids[j] = id;
            }
        } else {
            self.nan_step(old, x, id);
        }
        if self.nans != 0 {
            return f64::NAN;
        }
        // sum(rank * id) over the sorted window; the places are `id - (t - n - 1)`. The true answer is
        // small, so wrapping arithmetic is exact.
        let mut s = 0i64;
        for (r, &i) in self.ids.iter().enumerate() {
            s = s.wrapping_add((r as i64 + 1).wrapping_mul(i));
        }
        let s = s.wrapping_sub((self.t - self.n as i64 - 1).wrapping_mul(self.sum_r));
        let sum_d2 = self.sum_r2x2.wrapping_sub(s.wrapping_mul(2));
        if self.denom != 0 { 100.0 * (1.0 - 6.0 * sum_d2 as f64 / self.denom_f) } else { 0.0 }
    }
}

// ------------------------------------------------------------------------------------------------
// hurst
// ------------------------------------------------------------------------------------------------

/// The rescaled range R/S of one block of returns: the range of the running sum of deviations from
/// the block mean (the starting zero included) over the block's population standard deviation.
/// `None` for a block that never moves, whose deviation is zero. The old kernel's, expression for
/// expression.
fn rescaled_range(block: &[f64]) -> Option<f64> {
    if is_constant_window(block) {
        return None;
    }
    let n = block.len() as f64;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the old kernel folded.
    let mean = block.iter().sum::<f64>() / n;
    let (mut run, mut hi, mut lo, mut ss) = (0.0, 0.0f64, 0.0f64, 0.0);
    for r in block {
        let dev = r - mean;
        run += dev;
        hi = hi.max(run);
        lo = lo.min(run);
        ss += dev * dev;
    }
    let sd = (ss / n).sqrt();
    if sd > 0.0 { Some((hi - lo) / sd) } else { None }
}

/// The least-squares slope of `y` on `x` over `(x, y)` points, `NaN` for fewer than two points or
/// points sharing one `x`. The old kernel's, expression for expression.
fn log_log_slope(points: &[(f64, f64)]) -> f64 {
    if points.len() < 2 {
        return f64::NAN;
    }
    let n = points.len() as f64;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the old kernel folded.
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the old kernel folded.
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let (mut sxy, mut sxx) = (0.0, 0.0);
    for &(x, y) in points {
        let dx = x - mx;
        sxy += dx * (y - my);
        sxx += dx * dx;
    }
    if sxx > 0.0 { sxy / sxx } else { f64::NAN }
}

/// The R/S values of the last `nb` blocks of one size, the block ending at each return being the
/// newest of its phase: the blocks the old kernel counted back from the newest return are the blocks
/// that ended `size` returns earlier, so each is computed once, when it ends.
#[derive(Clone)]
struct HurstSize {
    size: usize,
    nb: usize,
    /// `size * nb`, the ring's length.
    len: usize,
    /// The slot of the newest block: the number of returns pushed, modulo `len`.
    pos: usize,
    ln_size: f64,
    /// The R/S of the block that ended at slot `i` (`NaN` for a block that never moves).
    rs: Vec<f64>,
}

/// The Hurst exponent by rescaled-range analysis of the last `n - 1` log returns of the close:
/// the slope of ln(mean R/S) on ln(block size) over block sizes 8, 16, 32, ... that fit, each splitting
/// the newest returns into whole blocks counted back from the latest return.
///
/// The blocks of one size repeat every `size` returns, so a return computes the R/S of ONE block per
/// size (the block that ends at it) and a ring of `blocks` values per size keeps the rest. The mean of
/// them is folded oldest block first, as the old kernel folded it, so the answer is bit for bit the old
/// one. A return that is not finite poisons the window until it leaves.
#[derive(Clone)]
pub(crate) struct HurstCore {
    /// Returns per window, `n - 1`.
    m: usize,
    dead: bool,
    sizes: Vec<HurstSize>,
    /// The last `kmax` returns, written twice (at `i` and `i + kmax`) so the newest `size` are one
    /// contiguous slice.
    ring: Vec<f64>,
    kmax: usize,
    rpos: usize,
    prev: f64,
    started: bool,
    /// Returns pushed.
    t: usize,
    /// Returns still to come before the last non-finite one has left the window (0: none inside).
    bad: usize,
}

impl HurstCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.min(RING_CAP);
        // 17 closes are 16 returns: the old kernel answers `NaN` everywhere below that.
        let dead = n < 17;
        let m = n.saturating_sub(1);
        let mut sizes = Vec::new();
        let mut size = 8usize;
        while !dead && size <= m {
            let nb = m / size;
            sizes.push(HurstSize {
                size,
                nb,
                len: size * nb,
                pos: 0,
                ln_size: libm::log(size as f64),
                rs: vec![f64::NAN; size * nb],
            });
            size *= 2;
        }
        let kmax = sizes.last().map_or(1, |s| s.size);
        HurstCore {
            m,
            dead,
            sizes,
            ring: vec![0.0; 2 * kmax],
            kmax,
            rpos: 0,
            prev: f64::NAN,
            started: false,
            t: 0,
            bad: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        for s in &mut self.sizes {
            s.pos = 0;
            s.rs.fill(f64::NAN);
        }
        self.ring.fill(0.0);
        self.rpos = 0;
        self.prev = f64::NAN;
        self.started = false;
        self.t = 0;
        self.bad = 0;
    }

    /// Push the next close; the Hurst exponent of the last `n` closes, `NaN` for the first `n - 1`, while
    /// a return in the window is not finite, and when fewer than two block sizes produced a value.
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        if self.dead {
            return f64::NAN;
        }
        if !self.started {
            self.started = true;
            self.prev = close;
            return f64::NAN;
        }
        let r = libm::log(close / self.prev);
        self.prev = close;
        self.ring[self.rpos] = r;
        self.ring[self.rpos + self.kmax] = r;
        self.rpos += 1;
        if self.rpos == self.kmax {
            self.rpos = 0;
        }
        let end = self.rpos + self.kmax;
        self.t += 1;
        if !r.is_finite() {
            self.bad = self.m;
        } else if self.bad > 0 {
            self.bad -= 1;
        }
        let have = self.t;
        for s in &mut self.sizes {
            let rs = if have >= s.size {
                rescaled_range(&self.ring[end - s.size..end]).unwrap_or(f64::NAN)
            } else {
                f64::NAN
            };
            s.pos += 1;
            if s.pos == s.len {
                s.pos = 0;
            }
            s.rs[s.pos] = rs;
        }
        if have < self.m || self.bad != 0 {
            return f64::NAN;
        }
        // The blocks counted back from the newest return, oldest first, per size.
        let mut points = [(0.0f64, 0.0f64); 64];
        let mut count_points = 0usize;
        for s in &self.sizes {
            let (mut total, mut count) = (0.0, 0usize);
            let mut at = s.pos + s.size;
            if at >= s.len {
                at -= s.len;
            }
            for _ in 0..s.nb {
                let v = s.rs[at];
                if !v.is_nan() {
                    total += v;
                    count += 1;
                }
                at += s.size;
                if at >= s.len {
                    at -= s.len;
                }
            }
            if count > 0 {
                let mean_rs = total / count as f64;
                if mean_rs > 0.0 {
                    points[count_points] = (s.ln_size, libm::log(mean_rs));
                    count_points += 1;
                }
            }
        }
        log_log_slope(&points[..count_points])
    }
}

// ------------------------------------------------------------------------------------------------
// autocorr
// ------------------------------------------------------------------------------------------------

/// The three sums a Pearson correlation of `x` against `y` is built from, `(sum(dx*dy), sum(dx^2),
/// sum(dy^2))` with the deviations taken from each series' own mean, or `None` when either series is
/// constant. The old kernel's, expression for expression.
fn pair_moments(x: &[f64], y: &[f64]) -> Option<(f64, f64, f64)> {
    if is_constant_window(x) || is_constant_window(y) {
        return None;
    }
    let n = x.len() as f64;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the old kernel folded.
    let mx = x.iter().sum::<f64>() / n;
    // Net-new Rust, no Python twin: a naive left fold in the exact order the old kernel folded.
    let my = y.iter().sum::<f64>() / n;
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (xk, yk) in x.iter().zip(y) {
        let dx = xk - mx;
        let dy = yk - my;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    Some((sxy, sxx, syy))
}

/// The old autocorrelation of the ordered window at lag `k`.
#[cold]
#[inline(never)]
fn autocorr_ordered(buf: &[f64], head: usize, k: usize) -> f64 {
    let mut w = window_vec(buf, head);
    let first = w[0];
    for v in &mut w {
        *v -= first;
    }
    let pairs = w.len() - k;
    if let Some((sxy, sxx, syy)) = pair_moments(&w[..pairs], &w[k..])
        && sxx > 0.0
        && syy > 0.0
    {
        return (sxy / (sxx * syy).sqrt()).clamp(-1.0, 1.0);
    }
    f64::NAN
}

/// The ordered window's `shift` and the sums of `x`, `y`, `x^2`, `y^2` and `x*y` on it, `x` being its
/// first `n - k` values and `y` its last `n - k`.
struct Lag {
    shift: f64,
    sx: f64,
    sy: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
    /// The largest `d^2` of the window.
    peak2: f64,
}

impl Lag {
    #[cold]
    #[inline(never)]
    fn of(buf: &[f64], head: usize, k: usize) -> Lag {
        let n = buf.len();
        let shift = shift_of(buf);
        let at = |i: usize| {
            let j = head + i;
            buf[if j >= n { j - n } else { j }] - shift
        };
        let (mut sx, mut sy, mut sxx, mut syy, mut sxy, mut peak2) =
            (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
        for i in 0..n - k {
            let (a, b) = (at(i), at(i + k));
            sx += a;
            sy += b;
            sxx += a * a;
            syy += b * b;
            sxy += a * b;
            peak2 = peak2.max(a * a).max(b * b);
        }
        Lag { shift, sx, sy, sxx, syy, sxy, peak2 }
    }
}

/// The autocorrelation of the last `n` values at lag `k`: the Pearson correlation, over the `n - k`
/// overlapping pairs of the window, of each value with the value `k` bars later, each side with its own
/// mean. Running sums of the window SHIFTED by its mean at the last rebuild, five of them
/// (`x`, `y`, `x^2`, `y^2`, `x*y`), each updated with the pair that enters and the pair that leaves.
///
/// A correlation whose `x` or `y` variance is below [`AUTO_FLOOR`] of its sum of squares (a window flat
/// to the rounding of the sums), and a `NaN` in the window, take the cold path: rebuild the sums and
/// answer with the old kernel's folds. A lag of 0, or a window under `lag + 2` values, never answers.
#[derive(Clone)]
pub(crate) struct AutoCore {
    inv: f64,
    inv_n: f64,
    n: usize,
    k: usize,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    sx: f64,
    sy: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
    peak2: f64,
    guard: f64,
}

impl AutoCore {
    #[inline]
    pub(crate) fn new(n: usize, k: usize) -> Self {
        let n = n.clamp(1, RING_CAP);
        let live = k != 0 && n >= k.saturating_add(2);
        let k = if live { k } else { 0 };
        AutoCore {
            inv: f64::NAN,
            // A window that cannot hold two pairs never leaves warm-up: every output is NaN.
            inv_n: if live { 1.0 / (n - k) as f64 } else { f64::NAN },
            n,
            k,
            buf: vec![0.0; n],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shift: 0.0,
            sx: 0.0,
            sy: 0.0,
            sxx: 0.0,
            syy: 0.0,
            sxy: 0.0,
            peak2: 0.0,
            guard: PEAK_GUARD * ((n - k) as f64) * ((n - k) as f64),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.shift = 0.0;
        self.sx = 0.0;
        self.sy = 0.0;
        self.sxx = 0.0;
        self.syy = 0.0;
        self.sxy = 0.0;
        self.peak2 = 0.0;
        self.inv = f64::NAN;
    }

    /// Push `x`; the autocorrelation of the last `n` values, `NaN` until `n` have been pushed.
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let (n, k, h) = (self.n, self.k, self.head);
        let old = std::mem::replace(&mut self.buf[h], x);
        // The value `k` back from the newest, and the value `n - k` back from it (the oldest `y`).
        let back = self.buf[if h >= k { h - k } else { h + n - k }];
        let fwd = self.buf[if h + k >= n { h + k - n } else { h + k }];
        self.head = h + 1;
        let c = self.shift;
        let (a, d, o, f) = (back - c, x - c, old - c, fwd - c);
        self.sx += a - o;
        self.sy += d - f;
        self.sxx += a * a - o * o;
        self.syy += d * d - f * f;
        self.sxy += a * d - o * f;
        self.peak2 = self.peak2.max(d * d);
        if self.head == n {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0 on raw values.
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let l = Lag::of(&self.buf, 0, k);
                self.shift = l.shift;
                self.sx = l.sx;
                self.sy = l.sy;
                self.sxx = l.sxx;
                self.syy = l.syy;
                self.sxy = l.sxy;
                self.peak2 = l.peak2;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let (mx, my) = (self.sx * self.inv, self.sy * self.inv);
        let cxx = self.sxx - self.sx * mx;
        let cyy = self.syy - self.sy * my;
        let cxy = self.sxy - self.sx * my;
        let guard = self.guard * self.peak2;
        let resolved = (cxx > AUTO_FLOOR * self.sxx)
            & (cyy > AUTO_FLOOR * self.syy)
            & (cxx > guard)
            & (cyy > guard);
        if !resolved {
            // Warming up, a `NaN` in the window, a stale sum, or a window flat to the rounding of the
            // sums. Cold, and over a shared slice (see `RegCore`).
            if self.inv.is_nan() {
                return f64::NAN;
            }
            let l = Lag::of(&self.buf, self.head, k);
            self.shift = l.shift;
            self.sx = l.sx;
            self.sy = l.sy;
            self.sxx = l.sxx;
            self.syy = l.syy;
            self.sxy = l.sxy;
            self.peak2 = l.peak2;
            return autocorr_ordered(&self.buf, self.head, k);
        }
        (cxy / (cxx * cyy).sqrt()).clamp(-1.0, 1.0)
    }
}

// ------------------------------------------------------------------------------------------------
// the typed steppers of a one-parameter statistic
// ------------------------------------------------------------------------------------------------

/// A one-line statistic of the close with one parameter, `period`: the struct, its constructors, `next`,
/// `next_value` and the [`crate::Indicator`] impl. `core` is built from the period read as a ring length
/// (`|n| core expression`) and must have `push(f64) -> f64` and `clear()`.
macro_rules! stat1 {
    (
        $(#[$m:meta])*
        $Name:ident, $key:literal, $def:expr, $Core:ty, |$n:ident| $make:expr
    ) => {
        $(#[$m])*
        #[derive(Clone)]
        pub struct $Name {
            period: f64,
            core: $Core,
            last: f64,
        }
        impl $Name {
            /// The registry defaults. Private: `Default` is the public spelling, `new(..)` takes them.
            #[inline]
            fn defaults() -> Self {
                Self::build($def)
            }
            #[inline]
            fn build(period: f64) -> Self {
                let $n = $crate::fast_g::period(period);
                Self { period, core: $make, last: f64::NAN }
            }
            #[inline]
            pub(crate) fn with_params(p: &[f64]) -> Self {
                Self::build(p.first().copied().unwrap_or($def))
            }
            /// Feed the next bar and get this bar's value: `NaN` while warming up. This IS
            /// `Indicator::on_bar`, bit for bit, without the `Vec`.
            #[inline]
            pub fn next(&mut self, bar: &vike_marketdata::Bar) -> f64 {
                self.next_value(bar.close)
            }
            /// Feed the next close as a plain number, no `Bar` needed; the same answer as `next` for a
            /// bar with that close.
            #[inline]
            pub fn next_value(&mut self, close: f64) -> f64 {
                self.last = self.core.push(close);
                self.last
            }
        }
        impl Default for $Name {
            #[inline]
            fn default() -> Self {
                Self::defaults()
            }
        }
        $crate::fast_g::typed_new_inline!($Name, $key, [period = $def]);
        impl $crate::Indicator for $Name {
            fn on_bar(&mut self, bar: &vike_marketdata::Bar) -> Vec<f64> {
                vec![self.next(bar)]
            }
            fn vectorize(&self, bars: &[vike_marketdata::Bar]) -> Vec<Vec<f64>> {
                $crate::roll::batch_scalar(self, bars, Self::next)
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
            fn lookback(&self) -> usize {
                $crate::indicators::pback(self.period, 1)
            }
            fn lookback_exact(&self) -> bool {
                true
            }
        }
    };
}
pub(crate) use stat1;
