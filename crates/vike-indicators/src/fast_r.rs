//! Fast cores for group `r` of the fourth wave of indicators (see `_followups/perf-conv4-addendum.md`):
//! the eight PAIR indicators of `crate::pairs` (`ratio`, `spread`, `spread_zscore`, `beta`, `correl`,
//! `correl_log`, `half_life`, `kalman_beta`).
//!
//! Each core is a small `Clone` struct of plain numbers whose `push(primary, benchmark)` is
//! `#[inline(always)]` and is the ONE implementation of the arithmetic: the streaming pair types
//! (`on_pair`, `on_pair_values`), `vectorize_pair` and the slice calls (`series::*`) all run it, so
//! they agree bit for bit.
//!
//! The windowed statistics are the shape of `crate::fast_d2`'s `StddevCore`, with two series: a ring
//! that starts as zeros, so the carried sums are right from the first value; sums taken on
//! `x - shift` (the window's oldest value at the last rebuild), so a price near a million with moves of
//! a hundredth keeps its digits; a rebuild from the ring every `ceil(512 / n)` laps. A second moment
//! that is `NaN` (warming up, or a `NaN` in the window), or under a hundred thousandth of the largest
//! one seen since the last rebuild, takes the cold path, which re-centres the shift on the window's
//! oldest value and sums again. That also makes a window of bit-equal values EXACTLY zero
//! variance (the decision `crate::window::is_constant_window` records).
//!
//! What the old pair kernels did at an edge, kept:
//! * `spread_zscore` and `correl` were infinite-memory running sums that were never rebuilt, so one
//!   non-finite close made every later output `NaN`. That is CHANGED on purpose (owner): like `beta`,
//!   `correl_log` and `half_life` they are `NaN` while the bad value is inside the window and exact
//!   again the update after it left (a `NaN` result after warm-up takes the cold rebuild).
//! * `beta` and `correl_log` clear their window on an undefined return (a zero or `NaN` previous
//!   close, a non-positive close for the log) and need `period` fresh returns again.
//! * `half_life` keeps a `NaN` in its window (the output is `NaN` while it is inside, and exact once
//!   it left).
//! * `kalman_beta` holds its state and answers `NaN` on a bar whose innovation variance is unusable,
//!   and is poisoned for good by a non-finite primary close.

use crate::roll::RING_CAP;

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const REBUILD: usize = 512;

/// Laps of a ring of `n` values between two rebuilds of its sums.
#[inline]
fn laps_per(n: usize) -> usize {
    REBUILD.div_ceil(n.max(1)).max(1)
}

/// Runs `$core.push(primary, benchmark)` over two slices into a buffer filled first and written by index
/// (the page faults of a fresh buffer land in the fill, not inside the recurrence). The output is as
/// long as the shorter slice. One loop for `series::*` and for `vectorize_pair`, so that a stream, a
/// batch and a slice call run the same arithmetic.
macro_rules! run_slices {
    ($core:ident, $primary:expr, $benchmark:expr) => {{
        let (primary, benchmark): (&[f64], &[f64]) = ($primary, $benchmark);
        let mut out = vec![f64::NAN; primary.len().min(benchmark.len())];
        for ((o, &a), &b) in out.iter_mut().zip(primary).zip(benchmark) {
            *o = $core.push(a, b);
        }
        out
    }};
}
pub(crate) use run_slices;

// ================================== RATIO ==================================

/// `primary / benchmark`, `NaN` where the benchmark is `0`.
#[derive(Clone)]
pub(crate) struct RatioCore;

impl RatioCore {
    #[inline]
    pub(crate) fn new() -> Self {
        RatioCore
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> f64 {
        if b != 0.0 { a / b } else { f64::NAN }
    }
}

// ================================== SPREAD =================================

/// `primary - benchmark`, or with `log` set `ln(primary) - ln(benchmark)` (`NaN` unless both are
/// positive). The logarithm is `libm::log`, so the numbers are the same on every platform.
#[derive(Clone)]
pub(crate) struct SpreadCore {
    log: bool,
}

impl SpreadCore {
    /// `log` as the old kernel read it: rounded, and any value but `0` selects the log spread.
    #[inline]
    pub(crate) fn new(log: f64) -> Self {
        SpreadCore { log: (log.round() as i64) != 0 }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> f64 {
        if !self.log {
            a - b
        } else if a > 0.0 && b > 0.0 {
            libm::log(a) - libm::log(b)
        } else {
            f64::NAN
        }
    }
}

// =============================== SPREAD Z-SCORE ============================

/// Rolling z-score of the hedge-ratio spread `s = primary - beta * benchmark` over `n` values:
/// `(s - mean) / sd` with the population standard deviation, `NaN` for the first `n - 1` values and
/// wherever the window has zero standard deviation.
#[derive(Clone)]
pub(crate) struct ZscoreCore {
    beta: f64,
    inv: f64,
    inv_n: f64,
    /// `inv * 1e-5`, the share of the largest second moment under which the cold path runs.
    ks: f64,
    ks_n: f64,
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

impl ZscoreCore {
    #[inline]
    pub(crate) fn new(n: usize, beta: f64) -> Self {
        // Capped, so that an infinite raw period cannot ask the allocator for the address space.
        let n = n.min(RING_CAP);
        let inv_n = if n == 0 { f64::NAN } else { 1.0 / n as f64 };
        ZscoreCore {
            beta,
            inv: f64::NAN,
            inv_n,
            ks: f64::NAN,
            ks_n: inv_n * 1.0e-5,
            buf: vec![0.0; n.clamp(1, RING_CAP)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shift: 0.0,
            s1: 0.0,
            s2: 0.0,
            smax: 0.0,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> f64 {
        let x = a - self.beta * b;
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        self.s1 += d - o;
        self.s2 += d * d - o * o;
        self.smax = if self.s2 > self.smax { self.s2 } else { self.smax };
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0 on raw values.
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let (shift, s1, s2) = shifted_sums(&self.buf, 0);
                self.shift = shift;
                self.s1 = s1;
                self.s2 = s2;
                self.smax = s2;
                self.laps = 0;
            }
            self.inv = self.inv_n;
            self.ks = self.ks_n;
        }
        let m = self.s1 * self.inv;
        let var = self.s2 * self.inv - m * m;
        // A comparison with a `NaN` is false, so a `NaN` variance (warming up, or a `NaN` in the
        // window) falls through to the cold path with the too-small ones.
        if var >= self.smax * self.ks {
            let sd = var.sqrt();
            // Relative to the shift the sums now have (a lap end may have re-centred it).
            return if sd != 0.0 { ((x - self.shift) - m) / sd } else { f64::NAN };
        }
        // Cold, and written inline over a shared slice (see `StddevCore::push`).
        if self.inv.is_nan() {
            return f64::NAN;
        }
        let (shift, s1, s2) = shifted_sums(&self.buf, self.head);
        self.shift = shift;
        self.s1 = s1;
        self.s2 = s2;
        self.smax = s2;
        let m = s1 * self.inv;
        let mut var = s2 * self.inv - m * m;
        if var < 0.0 {
            var = 0.0;
        }
        if var.is_nan() {
            // A non-finite value in the window: `NaN` while it is inside, exact once it left.
            return f64::NAN;
        }
        let sd = var.sqrt();
        if sd != 0.0 { ((x - shift) - m) / sd } else { f64::NAN }
    }
}

/// The window (oldest value at `head`) as `(shift, sum(x - shift), sum((x - shift)^2))`, `shift`
/// being the oldest value when it is finite.
#[inline(always)]
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

// ============================== TWO-SERIES WINDOW ==========================

/// The window sums of a pair `(u, v)` over `n` values, shifted by the oldest pair at the last rebuild.
#[derive(Clone, Copy)]
struct CoSums {
    shu: f64,
    shv: f64,
    su: f64,
    sv: f64,
    suu: f64,
    svv: f64,
    suv: f64,
}

/// The window (oldest pair at `head`) of the ring, re-centred on its oldest pair (when finite) and
/// summed from scratch. `#[cold]` and over a shared slice, so that the hot part of a `push` calls
/// nothing that can write the state. The ring holds the RAW pairs, so that a window of bit-equal
/// values is exactly constant again after any history (a ring of shifted values, written under
/// several shifts, could not be recovered bit for bit; it is 7% faster and was measured).
#[cold]
#[inline(never)]
fn co_sums(buf: &[[f64; 2]], head: usize) -> CoSums {
    let shu = if buf[head][0].is_finite() { buf[head][0] } else { 0.0 };
    let shv = if buf[head][1].is_finite() { buf[head][1] } else { 0.0 };
    let (mut su, mut sv, mut suu, mut svv, mut suv) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for &[u, v] in buf[head..].iter().chain(&buf[..head]) {
        let (du, dv) = (u - shu, v - shv);
        su += du;
        sv += dv;
        suu += du * du;
        svv += dv * dv;
        suv += du * dv;
    }
    CoSums { shu, shv, su, sv, suu, svv, suv }
}

/// The moments of the window, each scaled by `n * n` (`n * sum(x^2) - sum(x)^2`, so that no step of
/// a push multiplies by `1 / n`): the variances of `u` and `v` and the covariance.
#[derive(Clone, Copy)]
struct Moments {
    vu: f64,
    vv: f64,
    cov: f64,
}

impl Moments {
    /// `true` when any moment is `NaN`: a non-finite value is in the window.
    #[inline(always)]
    fn is_nan(&self) -> bool {
        self.vu.is_nan() || self.vv.is_nan() || self.cov.is_nan()
    }
}

/// A ring of the last `n` pairs with their shifted sums. Used by `correl` and `correl_log`
/// (both variances and the covariance), `beta` (the covariance and the benchmark's variance) and
/// `half_life` (the covariance and the regressor's variance).
///
/// One push is 22 floating-point operations beyond the loads and stores, and nothing divides: the
/// squares are carried as `x * (new + old)` of the difference already formed for the plain sum, and
/// the moments are scaled by `n * n` instead of divided by `n`.
#[derive(Clone)]
struct CoWindow {
    /// `n` as a float, `NaN` until the first lap of the ring ends (and for ever for `n == 0`): every
    /// moment is `NaN` until then, which is the warm-up, and it costs the hot path nothing.
    pp: f64,
    pp_n: f64,
    /// `pp * 1e-5`, the share of the largest second moment under which the cold path runs.
    kp: f64,
    kp_n: f64,
    /// The last `n` pairs.
    buf: Vec<[f64; 2]>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shu: f64,
    shv: f64,
    su: f64,
    sv: f64,
    suu: f64,
    svv: f64,
    suv: f64,
    /// The largest `suu` and `svv` since the last rebuild.
    maxu: f64,
    maxv: f64,
}

impl CoWindow {
    #[inline]
    fn new(n: usize) -> Self {
        let n = n.min(RING_CAP);
        // A window of 0 never leaves warm-up: every output is `NaN`.
        let pp_n = if n == 0 { f64::NAN } else { n as f64 };
        CoWindow {
            pp: f64::NAN,
            pp_n,
            kp: f64::NAN,
            kp_n: pp_n * 1.0e-5,
            buf: vec![[0.0; 2]; n.clamp(1, RING_CAP)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shu: 0.0,
            shv: 0.0,
            su: 0.0,
            sv: 0.0,
            suu: 0.0,
            svv: 0.0,
            suv: 0.0,
            maxu: 0.0,
            maxv: 0.0,
        }
    }

    #[inline(always)]
    fn set(&mut self, r: CoSums) {
        self.shu = r.shu;
        self.shv = r.shv;
        self.su = r.su;
        self.sv = r.sv;
        self.suu = r.suu;
        self.svv = r.svv;
        self.suv = r.suv;
        self.maxu = r.suu;
        self.maxv = r.svv;
    }

    #[inline(always)]
    fn push(&mut self, u: f64, v: f64) {
        let [ou, ov] = std::mem::replace(&mut self.buf[self.head], [u, v]);
        self.head += 1;
        let (du, dv) = (u - self.shu, v - self.shv);
        let (eu, ev) = (ou - self.shu, ov - self.shv);
        let (xu, xv) = (du - eu, dv - ev);
        self.su += xu;
        self.sv += xv;
        self.suu += xu * (du + eu);
        self.svv += xv * (dv + ev);
        self.suv += du * dv - eu * ev;
        self.maxu = if self.suu > self.maxu { self.suu } else { self.maxu };
        self.maxv = if self.svv > self.maxv { self.svv } else { self.maxv };
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0 on raw values.
            if self.laps >= self.laps_per || self.pp.is_nan() {
                let r = co_sums(&self.buf, 0);
                self.set(r);
                self.laps = 0;
            }
            self.pp = self.pp_n;
            self.kp = self.kp_n;
        }
    }

    /// The moments of the sums as they stand.
    #[inline(always)]
    fn moments(&self) -> Moments {
        Moments {
            vu: self.pp * self.suu - self.su * self.su,
            vv: self.pp * self.svv - self.sv * self.sv,
            cov: self.pp * self.suv - self.su * self.sv,
        }
    }

    /// The cold path: re-centre on the oldest pair, sum the ring again, and answer the moments
    /// (negative variances, which are cancellation, read `0`). `None` while warming up.
    #[inline(always)]
    fn resum(&mut self) -> Option<Moments> {
        if self.pp.is_nan() {
            return None;
        }
        let r = co_sums(&self.buf, self.head);
        self.set(r);
        let mut m = self.moments();
        // Not `.max(0.0)`: that returns 0 for a `NaN` variance, and a window holding a `NaN` is `NaN`.
        if m.vu < 0.0 {
            m.vu = 0.0;
        }
        if m.vv < 0.0 {
            m.vv = 0.0;
        }
        Some(m)
    }

    /// Start the window again from empty (a return that is undefined breaks the run of returns).
    /// Cold, and called only on a bar whose input was undefined.
    #[inline(always)]
    fn reset_window(&mut self) {
        // Anything to forget? A window that was just reset has an empty ring, no sums, and `pp`
        // still `NaN`; one with pushes in it has a non-zero head, or has completed a lap.
        if self.head != 0 || self.laps != 0 || !self.pp.is_nan() {
            self.buf.fill([0.0; 2]);
            self.head = 0;
            self.laps = 0;
            self.shu = 0.0;
            self.shv = 0.0;
            self.su = 0.0;
            self.sv = 0.0;
            self.suu = 0.0;
            self.svv = 0.0;
            self.suv = 0.0;
            self.maxu = 0.0;
            self.maxv = 0.0;
            self.pp = f64::NAN;
            self.kp = f64::NAN;
        }
    }
}

// ================================== CORREL =================================

/// The Pearson correlation of `(u, v)` from the moments, in `[-1, 1]`, `NaN` where either variance is
/// zero.
#[inline(always)]
fn pearson(m: Moments) -> f64 {
    let d = (m.vu * m.vv).sqrt();
    if d != 0.0 { (m.cov / d).clamp(-1.0, 1.0) } else { f64::NAN }
}

/// Rolling Pearson correlation of the two series (price levels) over `n` values, in `[-1, 1]`; `NaN`
/// for the first `n - 1` values and where a window has no variance.
#[derive(Clone)]
pub(crate) struct CorrelCore {
    w: CoWindow,
}

impl CorrelCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        CorrelCore { w: CoWindow::new(n) }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> f64 {
        let w = &mut self.w;
        w.push(a, b);
        let m = w.moments();
        // A comparison with a `NaN` is false: warming up and a `NaN` in the window go cold too. The
        // two tests are joined with `&`, so that they are one branch.
        if (m.vu >= w.maxu * w.kp) & (m.vv >= w.maxv * w.kp) {
            return pearson(m);
        }
        let Some(m) = w.resum() else {
            return f64::NAN;
        };
        if m.is_nan() {
            return f64::NAN;
        }
        pearson(m)
    }
}

/// Rolling Pearson correlation of the log returns `ln(c / c_prev)` over `n` returns, in `[-1, 1]`.
/// A return is undefined unless both closes are positive, and an undefined return of either series
/// empties the window. The logarithm is `libm::log`.
///
/// The returns are a stage of their own ([`CorrelLogCore::push_returns`] takes them): a call to
/// `libm::log` clobbers every vector register, so a loop that holds the window in registers and
/// calls it twice a bar spills and reloads the whole state around each call. [`run_correl_log`] takes
/// the logs of a chunk first and runs the window over them second, which is the same arithmetic in
/// the same order and so the same bits.
#[derive(Clone)]
pub(crate) struct CorrelLogCore {
    w: CoWindow,
    prev_a: f64,
    prev_b: f64,
}

/// The log return of `c` after `prev`, `NaN` unless both are positive.
#[inline(always)]
fn log_return(c: f64, prev: f64) -> f64 {
    if c > 0.0 && prev > 0.0 { libm::log(c / prev) } else { f64::NAN }
}

impl CorrelLogCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        CorrelLogCore { w: CoWindow::new(n), prev_a: f64::NAN, prev_b: f64::NAN }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> f64 {
        let (ra, rb) = (log_return(a, self.prev_a), log_return(b, self.prev_b));
        self.prev_a = a;
        self.prev_b = b;
        self.push_returns(ra, rb)
    }

    /// The window stage: one pair of log returns in (`NaN` if undefined), one correlation out.
    #[inline(always)]
    pub(crate) fn push_returns(&mut self, ra: f64, rb: f64) -> f64 {
        let w = &mut self.w;
        if ra.is_nan() || rb.is_nan() {
            w.reset_window();
            return f64::NAN;
        }
        w.push(ra, rb);
        let m = w.moments();
        if (m.vu >= w.maxu * w.kp) & (m.vv >= w.maxv * w.kp) {
            return pearson(m);
        }
        let Some(m) = w.resum() else {
            return f64::NAN;
        };
        if m.is_nan() {
            return f64::NAN;
        }
        pearson(m)
    }
}

/// `correl_log` over two slices (as long as the shorter): the logs of a chunk of 256 bars first, the
/// window over them second. Bit-identical to folding [`CorrelLogCore::push`].
pub(crate) fn run_correl_log(primary: &[f64], benchmark: &[f64], n: usize) -> Vec<f64> {
    const CHUNK: usize = 256;
    let mut core = CorrelLogCore::new(n);
    let mut out = vec![f64::NAN; primary.len().min(benchmark.len())];
    let (mut ra, mut rb) = ([0.0_f64; CHUNK], [0.0_f64; CHUNK]);
    let (mut pa, mut pb) = (f64::NAN, f64::NAN);
    for ((oc, ac), bc) in
        out.chunks_mut(CHUNK).zip(primary.chunks(CHUNK)).zip(benchmark.chunks(CHUNK))
    {
        for (((x, y), &a), &b) in ra.iter_mut().zip(rb.iter_mut()).zip(ac).zip(bc) {
            *x = log_return(a, pa);
            *y = log_return(b, pb);
            pa = a;
            pb = b;
        }
        for ((o, &x), &y) in oc.iter_mut().zip(&ra).zip(&rb) {
            *o = core.push_returns(x, y);
        }
    }
    out
}

// =================================== BETA ==================================

/// Rolling beta of simple returns, `cov(r_primary, r_benchmark) / var(r_benchmark)` over `n` returns.
/// A return is undefined where the previous close is `0` (or either is `NaN`), and an undefined
/// return of either series empties the window; the output is `NaN` while the window is not full and
/// where the benchmark's returns are constant.
#[derive(Clone)]
pub(crate) struct BetaCore {
    w: CoWindow,
    prev_a: f64,
    prev_b: f64,
}

impl BetaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        BetaCore { w: CoWindow::new(n), prev_a: f64::NAN, prev_b: f64::NAN }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> f64 {
        let (pa, pb) = (self.prev_a, self.prev_b);
        self.prev_a = a;
        self.prev_b = b;
        let ra = if pa != 0.0 { (a - pa) / pa } else { f64::NAN };
        let rb = if pb != 0.0 { (b - pb) / pb } else { f64::NAN };
        let w = &mut self.w;
        if ra.is_nan() || rb.is_nan() {
            w.reset_window();
            return f64::NAN;
        }
        w.push(ra, rb);
        let m = w.moments();
        // The covariance carries the rounding of the PRIMARY's largest second moment too, so a bad tick
        // in the primary that has left the window is caught by `vu`, not only one in the benchmark by
        // `vv`. The two tests are joined with `&`, so that they are one branch.
        if (m.vv >= w.maxv * w.kp) & (m.vu >= w.maxu * w.kp) {
            return if m.vv != 0.0 { m.cov / m.vv } else { f64::NAN };
        }
        let Some(m) = w.resum() else {
            return f64::NAN;
        };
        if m.is_nan() || m.vv == 0.0 {
            return f64::NAN;
        }
        m.cov / m.vv
    }
}

// ================================ HALF-LIFE ================================

/// Rolling Ornstein-Uhlenbeck half-life, in bars, of the hedge-ratio spread
/// `s = primary - beta * benchmark`: over the last `n` pairs `(s[t-1], s[t] - s[t-1])` the slope
/// `lambda = cov(lag, diff) / var(lag)` and `-ln 2 / lambda`. `NaN` unless `lambda < 0` (a
/// mean-reverting window), while the window is not full, and where the lagged spread is constant.
/// A `NaN` in the window makes the output `NaN` while it is inside.
#[derive(Clone)]
pub(crate) struct HalfLifeCore {
    w: CoWindow,
    beta: f64,
    prev_s: f64,
    started: bool,
}

impl HalfLifeCore {
    #[inline]
    pub(crate) fn new(n: usize, beta: f64) -> Self {
        HalfLifeCore { w: CoWindow::new(n), beta, prev_s: f64::NAN, started: false }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> f64 {
        let s = a - self.beta * b;
        if !self.started {
            self.started = true;
            self.prev_s = s;
            return f64::NAN;
        }
        let lag = self.prev_s;
        self.prev_s = s;
        let w = &mut self.w;
        w.push(lag, s - lag);
        let m = w.moments();
        if m.vu >= w.maxu * w.kp {
            return half_life_of(m);
        }
        let Some(m) = w.resum() else {
            return f64::NAN;
        };
        if m.is_nan() {
            return f64::NAN;
        }
        half_life_of(m)
    }
}

/// `-ln 2 / lambda` for the slope `lambda = cov / var(lag)`, only for a reverting (`lambda < 0`)
/// window with a non-zero lag variance.
#[inline(always)]
fn half_life_of(m: Moments) -> f64 {
    if m.vu != 0.0 {
        let lambda = m.cov / m.vu;
        if lambda < 0.0 {
            return -std::f64::consts::LN_2 / lambda;
        }
    }
    f64::NAN
}

// ============================== KALMAN BETA ================================

/// Time-varying hedge ratio from a two-state Kalman filter, `x = [beta, alpha]`, observation
/// `primary = beta * benchmark + alpha + v`. One bar is the same few operations the batch kernel
/// always ran (so the numbers are bit-identical to it); a bar whose innovation variance is not finite
/// or is zero holds the state and answers `NaN`.
#[derive(Clone)]
pub(crate) struct KalmanCore {
    vw: f64,
    ve: f64,
    x_beta: f64,
    x_alpha: f64,
    p00: f64,
    p01: f64,
    p10: f64,
    p11: f64,
}

impl KalmanCore {
    #[inline]
    pub(crate) fn new(delta: f64, ve: f64) -> Self {
        let denom = 1.0 - delta;
        let vw = if denom != 0.0 { delta / denom } else { 0.0 };
        KalmanCore { vw, ve, x_beta: 0.0, x_alpha: 0.0, p00: 0.0, p01: 0.0, p10: 0.0, p11: 0.0 }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> f64 {
        // R = P + Vw·I
        let (r00, r01, r10, r11) = (self.p00 + self.vw, self.p01, self.p10, self.p11 + self.vw);
        // F = [benchmark, 1]
        let yhat = b * self.x_beta + self.x_alpha;
        // R·Fᵀ, then the scalar innovation variance Q.
        let rf0 = r00 * b + r01;
        let rf1 = r10 * b + r11;
        let q = b * rf0 + rf1 + self.ve;
        if !q.is_finite() || q == 0.0 {
            return f64::NAN; // no usable gain: hold state, this bar is NaN
        }
        let e = a - yhat;
        let (k0, k1) = (rf0 / q, rf1 / q);
        self.x_beta += k0 * e;
        self.x_alpha += k1 * e;
        // P = R − K·(F·R); R is symmetric so F·R == (R·Fᵀ)ᵀ == [rf0, rf1].
        self.p00 = r00 - k0 * rf0;
        self.p01 = r01 - k0 * rf1;
        self.p10 = r10 - k1 * rf0;
        self.p11 = r11 - k1 * rf1;
        self.x_beta
    }
}
