//! Rolling-window primitives that cost O(1) a bar, for the indicator steppers.
//!
//! A windowed indicator used to re-fold its whole window on every bar (20 dependent adds for a
//! 20-bar mean, 200 for a 200-bar one). These keep the window's running statistics and update them
//! with the one value that enters and the one that leaves, which is what makes a stepper cost a few
//! nanoseconds whatever the period.
//!
//! # Numerics
//!
//! A running sum is a plain sum with a plain subtraction, so its rounding error would grow with the
//! length of the series. Two things keep that bounded, and both are cheap:
//!
//! * **Rebuild.** Every `n` pushes the sums are recomputed from the window itself, oldest to
//!   newest, so no error survives more than one window. That costs `n` adds once per `n` bars, which
//!   is one add a bar.
//! * **Shift.** Variance and the higher moments are accumulated on `x - shift`, where `shift` is
//!   the window's oldest value at the last rebuild, never on `x`. A running `E[x^2] - E[x]^2` of
//!   raw prices loses every digit once the level is large against the spread (a price near a
//!   million with moves of a hundredth has a variance a million times smaller than `x^2`); on shifted
//!   values the two terms are the size of the spread.
//!
//! Measured against the exact value (prices on a 2^-30 grid, so sums are exact in `i128`), the
//! shifted, rebuilt standard deviation is within 3e-14 relative at its worst on a level-100 series, on
//! a level-1,000,000 series with steps of 0.01 and on a nearly flat one, at 5 ns a bar; the plain
//! running sums other libraries use cost 4 ns and are wrong by 100% to 950% on the last two.
//!
//! NaN never enters a sum: it is counted, the statistic is `NaN` while any NaN is in the window, and
//! it is exact again the bar after the last one leaves.

use std::collections::VecDeque;

/// A fixed-capacity ring of the last `cap` values.
#[derive(Clone)]
pub(crate) struct Ring {
    buf: Vec<f64>,
    head: usize,
    len: usize,
}

impl Ring {
    pub(crate) fn new(cap: usize) -> Self {
        Ring { buf: vec![0.0; cap], head: 0, len: 0 }
    }

    pub(crate) fn cap(&self) -> usize {
        self.buf.len()
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_full(&self) -> bool {
        self.len == self.buf.len()
    }

    pub(crate) fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// Append `x`; returns the value that fell off the back when the ring was already full.
    pub(crate) fn push(&mut self, x: f64) -> Option<f64> {
        let cap = self.buf.len();
        if cap == 0 {
            return Some(x);
        }
        // `head` only leaves 0 once the ring is full, so a filling ring appends at `len`; the steady
        // state wraps by compare, because `% cap` on a runtime `cap` is a ~25-cycle division a bar.
        if self.len < cap {
            self.buf[self.len] = x;
            self.len += 1;
            None
        } else {
            let old = self.buf[self.head];
            self.buf[self.head] = x;
            self.head += 1;
            if self.head == cap {
                self.head = 0;
            }
            Some(old)
        }
    }

    /// The oldest value (index 0 of [`Ring::iter`]).
    pub(crate) fn oldest(&self) -> f64 {
        self.buf[self.head]
    }

    /// The value `back` pushes ago (`0` is the newest).
    pub(crate) fn back(&self, back: usize) -> f64 {
        let cap = self.buf.len();
        self.buf[(self.head + self.len - 1 - back) % cap]
    }

    /// The contents oldest to newest, as the (at most) two contiguous pieces of the buffer.
    pub(crate) fn slices(&self) -> (&[f64], &[f64]) {
        let cap = self.buf.len();
        if self.len < cap || self.head == 0 {
            (&self.buf[..self.len], &[])
        } else {
            (&self.buf[self.head..], &self.buf[..self.head])
        }
    }

    /// The contents oldest to newest.
    pub(crate) fn iter(&self) -> impl Iterator<Item = f64> + '_ {
        let (a, b) = self.slices();
        a.iter().chain(b).copied()
    }
}

/// Running sum (and mean) of the last `n` values.
#[derive(Clone)]
pub(crate) struct RollSum {
    ring: Ring,
    sum: f64,
    since_rebuild: usize,
    nans: usize,
}

impl RollSum {
    pub(crate) fn new(n: usize) -> Self {
        RollSum { ring: Ring::new(n), sum: 0.0, since_rebuild: 0, nans: 0 }
    }

    pub(crate) fn clear(&mut self) {
        self.ring.clear();
        self.sum = 0.0;
        self.since_rebuild = 0;
        self.nans = 0;
    }

    /// Push `x`; the mean once `n` values are in, `NaN` before and while a NaN is in the window.
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let n = self.ring.cap();
        if n == 0 {
            return f64::NAN;
        }
        let evicted = self.ring.push(x);
        if x.is_nan() {
            self.nans += 1;
        } else {
            self.sum += x;
        }
        if let Some(old) = evicted {
            if old.is_nan() {
                self.nans -= 1;
            } else {
                self.sum -= old;
            }
        }
        self.since_rebuild += 1;
        if self.since_rebuild >= n && self.ring.is_full() {
            self.sum = self.ring.iter().filter(|v| !v.is_nan()).fold(0.0, |a, v| a + v);
            self.since_rebuild = 0;
        }
        self.mean()
    }

    pub(crate) fn is_full(&self) -> bool {
        self.ring.is_full()
    }

    pub(crate) fn sum(&self) -> f64 {
        if self.ring.is_full() && self.nans == 0 { self.sum } else { f64::NAN }
    }

    pub(crate) fn mean(&self) -> f64 {
        self.sum() / self.ring.cap() as f64
    }

    pub(crate) fn ring(&self) -> &Ring {
        &self.ring
    }
}

/// Running mean and variance of the last `n` values, accumulated on a shifted copy of them.
#[derive(Clone)]
pub(crate) struct RollMoments {
    ring: Ring,
    shift: f64,
    s1: f64,
    s2: f64,
    since_rebuild: usize,
    nans: usize,
}

impl RollMoments {
    pub(crate) fn new(n: usize) -> Self {
        RollMoments { ring: Ring::new(n), shift: 0.0, s1: 0.0, s2: 0.0, since_rebuild: 0, nans: 0 }
    }

    pub(crate) fn clear(&mut self) {
        self.ring.clear();
        self.shift = 0.0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.since_rebuild = 0;
        self.nans = 0;
    }

    pub(crate) fn push(&mut self, x: f64) {
        let n = self.ring.cap();
        if n == 0 {
            return;
        }
        let evicted = self.ring.push(x);
        if x.is_nan() {
            self.nans += 1;
        } else {
            let d = x - self.shift;
            self.s1 += d;
            self.s2 += d * d;
        }
        if let Some(old) = evicted {
            if old.is_nan() {
                self.nans -= 1;
            } else {
                let d = old - self.shift;
                self.s1 -= d;
                self.s2 -= d * d;
            }
        }
        self.since_rebuild += 1;
        if self.ring.is_full() && (self.since_rebuild >= n || self.nans == 0 && self.s2 < 0.0) {
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        self.shift = self.ring.iter().find(|v| !v.is_nan()).unwrap_or(0.0);
        let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
        for v in self.ring.iter() {
            if !v.is_nan() {
                let d = v - self.shift;
                s1 += d;
                s2 += d * d;
            }
        }
        self.s1 = s1;
        self.s2 = s2;
        self.since_rebuild = 0;
    }

    pub(crate) fn is_full(&self) -> bool {
        self.ring.is_full()
    }

    fn ready(&self) -> bool {
        self.ring.is_full() && self.nans == 0
    }

    pub(crate) fn mean(&self) -> f64 {
        if !self.ready() {
            return f64::NAN;
        }
        self.shift + self.s1 / self.ring.cap() as f64
    }

    /// Population variance (divide by `n`), never negative.
    pub(crate) fn var_pop(&self) -> f64 {
        if !self.ready() {
            return f64::NAN;
        }
        let n = self.ring.cap() as f64;
        let m = self.s1 / n;
        (self.s2 / n - m * m).max(0.0)
    }

    /// Sample variance (divide by `n - 1`).
    pub(crate) fn var_sample(&self) -> f64 {
        let n = self.ring.cap() as f64;
        self.var_pop() * n / (n - 1.0)
    }

    pub(crate) fn ring(&self) -> &Ring {
        &self.ring
    }
}

/// Running linearly-weighted sum of the last `n` values: weights `1..=n`, the newest heaviest.
#[derive(Clone)]
pub(crate) struct RollWeighted {
    ring: Ring,
    sum: f64,
    weighted: f64,
    since_rebuild: usize,
    nans: usize,
}

impl RollWeighted {
    pub(crate) fn new(n: usize) -> Self {
        RollWeighted { ring: Ring::new(n), sum: 0.0, weighted: 0.0, since_rebuild: 0, nans: 0 }
    }

    pub(crate) fn clear(&mut self) {
        self.ring.clear();
        self.sum = 0.0;
        self.weighted = 0.0;
        self.since_rebuild = 0;
        self.nans = 0;
    }

    /// Push `x`; the weighted mean `sum(k * y_k) / (n (n + 1) / 2)` once `n` values are in, else `NaN`.
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let n = self.ring.cap();
        if n == 0 {
            return f64::NAN;
        }
        let was_full = self.ring.is_full();
        let evicted = self.ring.push(x);
        if x.is_nan() {
            self.nans += 1;
        }
        if let Some(old) = evicted
            && old.is_nan()
        {
            self.nans -= 1;
        }
        let xv = if x.is_nan() { 0.0 } else { x };
        if was_full {
            let old = evicted.map_or(0.0, |o| if o.is_nan() { 0.0 } else { o });
            self.weighted = (self.weighted - self.sum) + n as f64 * xv;
            self.sum = self.sum - old + xv;
        } else {
            self.sum += xv;
            self.weighted += self.ring.len() as f64 * xv;
        }
        self.since_rebuild += 1;
        if self.ring.is_full() && self.since_rebuild >= n {
            let (mut s, mut w) = (0.0_f64, 0.0_f64);
            for (k, v) in self.ring.iter().enumerate() {
                let v = if v.is_nan() { 0.0 } else { v };
                s += v;
                w += (k + 1) as f64 * v;
            }
            self.sum = s;
            self.weighted = w;
            self.since_rebuild = 0;
        }
        if self.ring.is_full() && self.nans == 0 {
            self.weighted / (n * (n + 1) / 2) as f64
        } else {
            f64::NAN
        }
    }

    pub(crate) fn ring(&self) -> &Ring {
        &self.ring
    }
}

/// Running sums for a least-squares line through the last `n` values against `0..n`: the sum of
/// `y`, of `i * y` and of `y^2`, all on `y - shift`.
#[derive(Clone)]
pub(crate) struct RollRegression {
    ring: Ring,
    shift: f64,
    sy: f64,
    sxy: f64,
    syy: f64,
    since_rebuild: usize,
    nans: usize,
}

impl RollRegression {
    pub(crate) fn new(n: usize) -> Self {
        RollRegression {
            ring: Ring::new(n),
            shift: 0.0,
            sy: 0.0,
            sxy: 0.0,
            syy: 0.0,
            since_rebuild: 0,
            nans: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.ring.clear();
        self.shift = 0.0;
        self.sy = 0.0;
        self.sxy = 0.0;
        self.syy = 0.0;
        self.since_rebuild = 0;
        self.nans = 0;
    }

    pub(crate) fn push(&mut self, y: f64) {
        let n = self.ring.cap();
        if n == 0 {
            return;
        }
        let was_full = self.ring.is_full();
        let evicted = self.ring.push(y);
        if y.is_nan() {
            self.nans += 1;
        }
        if let Some(old) = evicted
            && old.is_nan()
        {
            self.nans -= 1;
        }
        let d = if y.is_nan() { 0.0 } else { y - self.shift };
        if was_full {
            let o = evicted.map_or(0.0, |o| if o.is_nan() { 0.0 } else { o - self.shift });
            self.sxy = self.sxy - (self.sy - o) + (n - 1) as f64 * d;
            self.sy = self.sy - o + d;
            self.syy += d * d - o * o;
        } else {
            self.sxy += (self.ring.len() - 1) as f64 * d;
            self.sy += d;
            self.syy += d * d;
        }
        self.since_rebuild += 1;
        if self.ring.is_full() && self.since_rebuild >= n {
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        self.shift = self.ring.iter().find(|v| !v.is_nan()).unwrap_or(0.0);
        let (mut sy, mut sxy, mut syy) = (0.0_f64, 0.0_f64, 0.0_f64);
        for (i, v) in self.ring.iter().enumerate() {
            let d = if v.is_nan() { 0.0 } else { v - self.shift };
            sy += d;
            sxy += i as f64 * d;
            syy += d * d;
        }
        self.sy = sy;
        self.sxy = sxy;
        self.syy = syy;
        self.since_rebuild = 0;
    }

    fn ready(&self) -> bool {
        self.ring.is_full() && self.nans == 0
    }

    /// `(slope, intercept)` of the fit with `x = 0..n`, the intercept at `x = 0` (the oldest value),
    /// or `NaN`s before the window is full.
    pub(crate) fn fit(&self) -> (f64, f64) {
        if !self.ready() || self.ring.cap() < 2 {
            return (f64::NAN, f64::NAN);
        }
        let n = self.ring.cap() as f64;
        let sx = n * (n - 1.0) / 2.0;
        let sxx = (n - 1.0) * n * (2.0 * n - 1.0) / 6.0;
        let slope = (n * self.sxy - sx * self.sy) / (n * sxx - sx * sx);
        let intercept = self.shift + (self.sy - slope * sx) / n;
        (slope, intercept)
    }

    /// Squared Pearson correlation of the window with its index.
    pub(crate) fn r_squared(&self) -> f64 {
        if !self.ready() || self.ring.cap() < 2 {
            return f64::NAN;
        }
        let n = self.ring.cap() as f64;
        let sx = n * (n - 1.0) / 2.0;
        let sxx = (n - 1.0) * n * (2.0 * n - 1.0) / 6.0;
        let cov = n * self.sxy - sx * self.sy;
        let vx = n * sxx - sx * sx;
        let vy = n * self.syy - self.sy * self.sy;
        if vy <= 0.0 { f64::NAN } else { (cov * cov / (vx * vy)).min(1.0) }
    }

    pub(crate) fn ring(&self) -> &Ring {
        &self.ring
    }
}

/// Running maximum or minimum of the last `n` values, by a monotonic deque of `(index, value)`.
///
/// `prefer_recent` decides ties: when two equal values are in the window, the position reported by
/// [`RollExtreme::age`] is the more recent one (`true`) or the older one (`false`).
#[derive(Clone)]
pub(crate) struct RollExtreme {
    n: usize,
    is_max: bool,
    prefer_recent: bool,
    q: VecDeque<(u64, f64)>,
    t: u64,
}

impl RollExtreme {
    pub(crate) fn new(n: usize, is_max: bool, prefer_recent: bool) -> Self {
        RollExtreme { n, is_max, prefer_recent, q: VecDeque::with_capacity(n + 1), t: 0 }
    }

    pub(crate) fn clear(&mut self) {
        self.q.clear();
        self.t = 0;
    }

    /// Push `x` (NaN is skipped as if absent); the extreme of the window once `n` values were seen.
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        if !x.is_nan() {
            while let Some(&(_, back)) = self.q.back() {
                let dominated = if self.is_max { back < x } else { back > x };
                let tie = back == x;
                if dominated || (tie && self.prefer_recent) {
                    self.q.pop_back();
                } else {
                    break;
                }
            }
            self.q.push_back((self.t, x));
        }
        self.t += 1;
        while let Some(&(i, _)) = self.q.front() {
            if i + (self.n as u64) < self.t {
                self.q.pop_front();
            } else {
                break;
            }
        }
        self.value()
    }

    /// The extreme of the current window, `NaN` before `n` values were pushed or if all were NaN.
    pub(crate) fn value(&self) -> f64 {
        if self.t < self.n as u64 {
            return f64::NAN;
        }
        self.q.front().map_or(f64::NAN, |&(_, v)| v)
    }

    /// How many pushes ago the extreme was pushed (`0` is the newest value).
    pub(crate) fn age(&self) -> Option<usize> {
        if self.t < self.n as u64 {
            return None;
        }
        self.q.front().map(|&(i, _)| (self.t - 1 - i) as usize)
    }
}

/// The last `n` values kept sorted, for rolling medians and ranks.
#[derive(Clone)]
pub(crate) struct RollSorted {
    ring: Ring,
    sorted: Vec<f64>,
}

impl RollSorted {
    pub(crate) fn new(n: usize) -> Self {
        RollSorted { ring: Ring::new(n), sorted: Vec::with_capacity(n) }
    }

    pub(crate) fn clear(&mut self) {
        self.ring.clear();
        self.sorted.clear();
    }

    pub(crate) fn push(&mut self, x: f64) {
        if self.ring.cap() == 0 {
            return;
        }
        if let Some(old) = self.ring.push(x) {
            let at = self.sorted.partition_point(|v| v.total_cmp(&old).is_lt());
            self.sorted.remove(at);
        }
        let at = self.sorted.partition_point(|v| v.total_cmp(&x).is_lt());
        self.sorted.insert(at, x);
    }

    pub(crate) fn is_full(&self) -> bool {
        self.ring.is_full()
    }

    /// The window in ascending order (NaNs last).
    pub(crate) fn sorted(&self) -> &[f64] {
        &self.sorted
    }

    pub(crate) fn ring(&self) -> &Ring {
        &self.ring
    }
}

/// The batch result of a stepper: a clone reset to its initial state, folded over `bars`. A batch
/// computed this way costs what a stream does (a few ns a bar) and equals the stream bit for bit by
/// construction, because it IS the stream.
pub(crate) fn batch_scalar<S: crate::Indicator + Clone>(
    this: &S,
    bars: &[vike_marketdata::Bar],
    mut step: impl FnMut(&mut S, &vike_marketdata::Bar) -> f64,
) -> Vec<Vec<f64>> {
    let mut s = this.clone();
    s.reset();
    // Filled first, then written by index (see [`batch_array`]).
    let mut out = vec![f64::NAN; bars.len()];
    for (o, b) in out.iter_mut().zip(bars) {
        *o = step(&mut s, b);
    }
    vec![out]
}

/// [`batch_scalar`] for a stepper with `N` output lines.
pub(crate) fn batch_array<S: crate::Indicator + Clone, const N: usize>(
    this: &S,
    bars: &[vike_marketdata::Bar],
    mut step: impl FnMut(&mut S, &vike_marketdata::Bar) -> [f64; N],
) -> Vec<Vec<f64>> {
    let mut s = this.clone();
    s.reset();
    // Filled first, then written by index: the page faults of fresh buffers land in the fill, not
    // inside the recurrence (measured on a 50,000-value EMA: 180 us written by `push`, 77 us this way).
    let mut cols: Vec<Vec<f64>> = (0..N).map(|_| vec![f64::NAN; bars.len()]).collect();
    for (i, b) in bars.iter().enumerate() {
        let v = step(&mut s, b);
        for (col, x) in cols.iter_mut().zip(v) {
            col[i] = x;
        }
    }
    cols
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prices on a 2^-30 grid so every sum below is exact in i128.
    fn series(n: usize, level: f64, step: f64) -> Vec<f64> {
        let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
        let q = (1u64 << 30) as f64;
        let mut p = level;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                p += ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * step;
                (p * q).round() / q
            })
            .collect()
    }

    fn scaled(x: f64) -> i128 {
        (x * (1u64 << 30) as f64) as i128
    }

    fn exact_mean_var(w: &[f64]) -> (f64, f64) {
        let n = w.len() as i128;
        let v: Vec<i128> = w.iter().map(|&x| scaled(x)).collect();
        let s1: i128 = v.iter().sum();
        let s2: i128 = v.iter().map(|x| x * x).sum();
        let q = (1u64 << 30) as f64;
        let mean = s1 as f64 / n as f64 / q;
        let var = (n * s2 - s1 * s1) as f64 / (n * n) as f64 / q / q;
        (mean, var)
    }

    #[test]
    fn ring_keeps_the_last_n_in_order() {
        let mut r = Ring::new(3);
        assert_eq!(r.push(1.0), None);
        assert_eq!(r.push(2.0), None);
        assert_eq!(r.push(3.0), None);
        assert_eq!(r.push(4.0), Some(1.0));
        assert_eq!(r.push(5.0), Some(2.0));
        assert_eq!(r.iter().collect::<Vec<_>>(), vec![3.0, 4.0, 5.0]);
        assert_eq!((r.oldest(), r.back(0), r.back(2)), (3.0, 5.0, 3.0));
        let (a, b) = r.slices();
        assert_eq!(a.len() + b.len(), 3);
    }

    #[test]
    fn mean_and_variance_track_the_exact_values_on_easy_and_hard_series() {
        for (level, step) in [(100.0, 2.0), (1_000_000.0, 0.02), (100.0, 2e-6)] {
            let c = series(5_000, level, step);
            for n in [2usize, 5, 20, 200] {
                let (mut sum, mut mom) = (RollSum::new(n), RollMoments::new(n));
                for i in 0..c.len() {
                    let m = sum.push(c[i]);
                    mom.push(c[i]);
                    if i + 1 < n {
                        assert!(m.is_nan() && mom.mean().is_nan() && mom.var_pop().is_nan());
                        continue;
                    }
                    let (em, ev) = exact_mean_var(&c[i + 1 - n..=i]);
                    assert!(
                        (m - em).abs() <= 1e-12 * em.abs().max(1.0),
                        "mean n={n} i={i}: {m} vs {em}"
                    );
                    assert!(
                        (mom.mean() - em).abs() <= 1e-12 * em.abs().max(1.0),
                        "moments mean n={n} i={i}"
                    );
                    let (got, want) = (mom.var_pop().sqrt(), ev.sqrt());
                    assert!(
                        (got - want).abs() <= 1e-9 * want.max(1e-12) + 1e-15,
                        "sd n={n} i={i} level={level}: {got} vs {want}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_nan_poisons_the_window_and_is_forgotten_the_bar_after_it_leaves() {
        let n = 4;
        let (mut sum, mut mom) = (RollSum::new(n), RollMoments::new(n));
        let xs = [1.0, 2.0, 3.0, 4.0, f64::NAN, 6.0, 7.0, 8.0, 9.0, 10.0];
        let mut means = Vec::new();
        for &x in &xs {
            means.push(sum.push(x));
            mom.push(x);
        }
        assert!(means[4].is_nan() && means[5].is_nan() && means[6].is_nan() && means[7].is_nan());
        assert_eq!(means[8], (6.0 + 7.0 + 8.0 + 9.0) / 4.0);
        assert!((mom.mean() - 8.5).abs() < 1e-12 && mom.var_pop() > 0.0);
    }

    #[test]
    fn a_flat_window_has_exactly_zero_variance() {
        let mut mom = RollMoments::new(5);
        for _ in 0..40 {
            mom.push(123.456);
        }
        assert_eq!(mom.var_pop(), 0.0);
    }

    #[test]
    fn weighted_mean_matches_the_definition() {
        let c = series(2_000, 100.0, 2.0);
        for n in [1usize, 2, 7, 20] {
            let mut w = RollWeighted::new(n);
            for i in 0..c.len() {
                let got = w.push(c[i]);
                if i + 1 < n {
                    assert!(got.is_nan());
                    continue;
                }
                let denom = (n * (n + 1) / 2) as f64;
                let want: f64 = c[i + 1 - n..=i]
                    .iter()
                    .enumerate()
                    .map(|(k, v)| (k + 1) as f64 * v)
                    .sum::<f64>()
                    / denom;
                assert!(
                    (got - want).abs() <= 1e-11 * want.abs(),
                    "wma n={n} i={i}: {got} vs {want}"
                );
            }
        }
    }

    #[test]
    fn regression_matches_a_direct_fit() {
        let c = series(2_000, 100.0, 2.0);
        for n in [2usize, 5, 14, 50] {
            let mut r = RollRegression::new(n);
            for i in 0..c.len() {
                r.push(c[i]);
                if i + 1 < n {
                    continue;
                }
                let w = &c[i + 1 - n..=i];
                let nf = n as f64;
                let mean_y = w.iter().sum::<f64>() / nf;
                let mean_x = (nf - 1.0) / 2.0;
                let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
                for (k, y) in w.iter().enumerate() {
                    sxy += (k as f64 - mean_x) * (y - mean_y);
                    sxx += (k as f64 - mean_x) * (k as f64 - mean_x);
                    syy += (y - mean_y) * (y - mean_y);
                }
                let slope = sxy / sxx;
                let intercept = mean_y - slope * mean_x;
                let (s, b) = r.fit();
                assert!(
                    (s - slope).abs() <= 1e-9 * slope.abs().max(1e-3),
                    "slope n={n} i={i}: {s} vs {slope}"
                );
                assert!(
                    (b - intercept).abs() <= 1e-9 * intercept.abs().max(1.0),
                    "intercept n={n} i={i}"
                );
                let r2 = (sxy * sxy / (sxx * syy)).min(1.0);
                assert!(
                    (r.r_squared() - r2).abs() <= 1e-8,
                    "r2 n={n} i={i}: {} vs {r2}",
                    r.r_squared()
                );
            }
        }
    }

    #[test]
    fn extremes_and_their_age_match_a_scan() {
        let c = series(3_000, 100.0, 2.0);
        for n in [1usize, 3, 14, 50] {
            for prefer_recent in [true, false] {
                let (mut hi, mut lo) = (
                    RollExtreme::new(n, true, prefer_recent),
                    RollExtreme::new(n, false, prefer_recent),
                );
                for i in 0..c.len() {
                    let (h, l) = (hi.push(c[i]), lo.push(c[i]));
                    if i + 1 < n {
                        assert!(h.is_nan() && l.is_nan());
                        continue;
                    }
                    let w = &c[i + 1 - n..=i];
                    assert_eq!(h, w.iter().copied().fold(f64::MIN, f64::max));
                    assert_eq!(l, w.iter().copied().fold(f64::MAX, f64::min));
                    let pos = if prefer_recent {
                        w.iter().rposition(|v| *v == h)
                    } else {
                        w.iter().position(|v| *v == h)
                    }
                    .unwrap();
                    assert_eq!(
                        hi.age().unwrap(),
                        n - 1 - pos,
                        "age n={n} i={i} recent={prefer_recent}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_sorted_window_gives_the_median_and_ranks() {
        let c = series(1_000, 100.0, 2.0);
        let n = 9;
        let mut s = RollSorted::new(n);
        for i in 0..c.len() {
            s.push(c[i]);
            if i + 1 >= n {
                let mut w = c[i + 1 - n..=i].to_vec();
                w.sort_by(|a, b| a.total_cmp(b));
                assert_eq!(s.sorted(), &w[..]);
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// The two hottest indicators get a core of their own: no NaN counters, no `Option`, no `chain`.
// ------------------------------------------------------------------------------------------------

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const SMA_REBUILD: usize = 512;

/// The largest ring a core allocates. The registry caps every window at 1,000, so this is never reached
/// through a checked constructor; it only stops a degenerate raw `with_params` (an infinity reads as
/// `usize::MAX`) from asking the allocator for the address space.
pub(crate) const RING_CAP: usize = 1 << 16;

/// A simple moving average in a few instructions a value: a ring, a running sum, one multiply.
///
/// The ring starts as `n` zeros, so the carried sum (`sum += x - old`) is right from the first
/// value and the hot path has no fill branch. The first `n - 1` outputs are `NaN` because the
/// multiplier `inv` is `NaN` until the first lap of the ring ends (`sum * NaN`), which costs no
/// branch per value: the switch to `1 / n` sits in the lap-end branch that runs once per `n`
/// values anyway.
///
/// The sum is rebuilt from the ring every `ceil(SMA_REBUILD / n)` laps, so rounding error cannot
/// grow past that many steps. A `NaN` result (a `NaN` entered, or an infinity LEFT: `inf - inf`) is
/// rebuilt on the spot, which makes the mean `NaN` while a `NaN` is in the window and exact again
/// the update after it left; an infinity IN the window is a legitimate infinite mean.
#[derive(Clone)]
pub(crate) struct SmaCore {
    inv: f64,
    inv_n: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    sum: f64,
}

impl SmaCore {
    /// Laps of the ring between two rebuilds of the sum.
    #[inline]
    fn laps_per(n: usize) -> usize {
        SMA_REBUILD.div_ceil(n.max(1)).max(1)
    }

    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        SmaCore {
            inv: f64::NAN,
            // A window of 0 never leaves warm-up: every output is NaN.
            inv_n: if n == 0 { f64::NAN } else { 1.0 / n as f64 },
            buf: vec![0.0; n.clamp(1, RING_CAP)],
            head: 0,
            laps: 0,
            laps_per: Self::laps_per(n),
            sum: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.sum = 0.0;
        self.inv = f64::NAN;
    }

    /// Push `x`; the mean of the last `n` values, `NaN` until `n` have been pushed.
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        self.sum += x - old;
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                // The head is 0, so the ring is already oldest-to-newest.
                self.sum = self.buf.iter().fold(0.0, |a, &v| a + v);
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let mut out = self.sum * self.inv;
        if out.is_nan() {
            // Warming up (or a window of 0), a NaN in the window, or a stale carried sum. The rebuild
            // is a free function over a shared slice, NOT a `&mut self` method: a call that could
            // write the struct keeps `sum`, `head` and `inv` in memory across every iteration of a
            // caller's loop (measured: 1.25 ns a value became 2.8 ns).
            if self.inv.is_nan() {
                return f64::NAN;
            }
            self.sum = ring_sum(&self.buf, self.head);
            out = self.sum * self.inv;
            if out.is_nan() {
                out = f64::NAN;
            }
        }
        out
    }
}

/// The ring summed oldest to newest (the oldest value is at `head`).
#[cold]
#[inline(never)]
fn ring_sum(buf: &[f64], head: usize) -> f64 {
    buf[head..].iter().chain(&buf[..head]).fold(0.0, |a, &v| a + v)
}

/// Wilder's RSI in a few instructions a value (see [`SmaCore`] for the shape).
#[derive(Clone)]
pub(crate) struct RsiCore {
    n: usize,
    inv: f64,
    keep: f64,
    prev: f64,
    seen: usize,
    ag: f64,
    al: f64,
}

impl RsiCore {
    pub(crate) fn new(n: usize) -> Self {
        RsiCore {
            n,
            inv: 1.0 / n as f64,
            keep: (n as f64 - 1.0) / n as f64,
            prev: f64::NAN,
            seen: 0,
            ag: 0.0,
            al: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.prev = f64::NAN;
        self.seen = 0;
        self.ag = 0.0;
        self.al = 0.0;
    }

    /// `100 * ag / (ag + al)`, or 100 when there were no losses.
    #[inline]
    pub(crate) fn rsi(ag: f64, al: f64) -> f64 {
        if al == 0.0 { 100.0 } else { 100.0 * ag / (ag + al) }
    }

    /// Push a close; the RSI once `n + 1` closes are in (`n` moves), else `NaN`.
    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        if self.n == 0 {
            return f64::NAN;
        }
        let i = self.seen;
        self.seen = i + 1;
        let d = c - self.prev;
        self.prev = c;
        if i == 0 {
            return f64::NAN;
        }
        // `max` returns the other operand for a NaN, so a NaN move counts as no move.
        let g = d.max(0.0);
        let l = (-d).max(0.0);
        if i < self.n {
            self.ag += g;
            self.al += l;
            f64::NAN
        } else if i == self.n {
            self.ag = (self.ag + g) * self.inv;
            self.al = (self.al + l) * self.inv;
            Self::rsi(self.ag, self.al)
        } else {
            self.ag = self.ag * self.keep + g * self.inv;
            self.al = self.al * self.keep + l * self.inv;
            Self::rsi(self.ag, self.al)
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Whole-series loops. The input IS the window, so there is no ring, and the running state lives in
// locals (registers) instead of struct fields. They run the exact operations `SmaCore::push` and
// `RsiCore::push` run, in the same order, so a stream and a batch agree bit for bit.
// ------------------------------------------------------------------------------------------------

/// `out` = the simple moving average of `get(0..len)` over `n` values, `NaN` for the first `n - 1`.
///
/// The operations and their order are `SmaCore::push`'s, so the two agree bit for bit.
#[inline]
pub(crate) fn sma_run(len: usize, n: usize, get: impl Fn(usize) -> f64, out: &mut Vec<f64>) {
    out.clear();
    // Filled first, then written by index (see `series`).
    out.resize(len, f64::NAN);
    if n == 0 || len < n {
        return;
    }
    let inv = 1.0 / n as f64;
    let laps_per = SmaCore::laps_per(n);
    let rebuilt = |end: usize| (end + 1 - n..=end).fold(0.0, |a, k| a + get(k));
    let mut sum = 0.0;
    for i in 0..n {
        sum += get(i);
    }
    // The first lap ends at the first mean. A rebuild there equals the fold just made.
    let mut laps = 1usize;
    if laps >= laps_per {
        laps = 0;
    }
    let mut o = sum * inv;
    if o.is_nan() {
        sum = rebuilt(n - 1);
        o = sum * inv;
        if o.is_nan() {
            o = f64::NAN;
        }
    }
    out[n - 1] = o;
    let mut i = n;
    while i < len {
        let end = (i + n).min(len);
        // A full lap ends with the ring's head wrapping: the stream counts the lap and may rebuild
        // BEFORE it multiplies and emits that element, so the last element of a lap is done apart.
        let body_end = if end - i == n { end - 1 } else { end };
        #[expect(clippy::needless_range_loop, reason = "j also indexes the input through `get`")]
        for j in i..body_end {
            sum += get(j) - get(j - n);
            let mut o = sum * inv;
            if o.is_nan() {
                sum = rebuilt(j);
                o = sum * inv;
                if o.is_nan() {
                    o = f64::NAN;
                }
            }
            out[j] = o;
        }
        if body_end < end {
            let j = body_end;
            sum += get(j) - get(j - n);
            laps += 1;
            if laps >= laps_per {
                sum = rebuilt(j);
                laps = 0;
            }
            let mut o = sum * inv;
            if o.is_nan() {
                sum = rebuilt(j);
                o = sum * inv;
                if o.is_nan() {
                    o = f64::NAN;
                }
            }
            out[j] = o;
        }
        i = end;
    }
}

/// `out` = Wilder's RSI of `get(0..len)` over `n` moves, `NaN` for the first `n` values.
#[inline]
pub(crate) fn rsi_run(len: usize, n: usize, get: impl Fn(usize) -> f64, out: &mut Vec<f64>) {
    out.clear();
    out.resize(len, f64::NAN);
    if n == 0 || len <= n {
        return;
    }
    let inv = 1.0 / n as f64;
    let keep = (n as f64 - 1.0) / n as f64;
    let mut prev = get(0);
    let (mut ag, mut al) = (0.0_f64, 0.0_f64);
    for i in 1..n {
        let c = get(i);
        let d = c - prev;
        prev = c;
        ag += d.max(0.0);
        al += (-d).max(0.0);
    }
    let c = get(n);
    let d = c - prev;
    prev = c;
    ag = (ag + d.max(0.0)) * inv;
    al = (al + (-d).max(0.0)) * inv;
    out[n] = RsiCore::rsi(ag, al);
    #[expect(clippy::needless_range_loop, reason = "i also indexes the input through `get`")]
    for i in n + 1..len {
        let c = get(i);
        let d = c - prev;
        prev = c;
        ag = ag * keep + d.max(0.0) * inv;
        al = al * keep + (-d).max(0.0) * inv;
        out[i] = RsiCore::rsi(ag, al);
    }
}

// ------------------------------------------------------------------------------------------------
// Cores for EMA, MACD, Bollinger and ATR: small `#[inline]` structs of plain numbers whose `push`
// calls nothing that takes `&mut self`, so a caller's loop (a stream, a batch fold, a slice call)
// keeps the state in registers.
// ------------------------------------------------------------------------------------------------

/// An exponential moving average seeded with the mean of the first `n` values, then
/// `alpha * x + (1 - alpha) * prev`; `NaN` until `n` values are in.
#[derive(Clone)]
pub(crate) struct EmaCore {
    n: usize,
    alpha: f64,
    keep: f64,
    inv: f64,
    count: usize,
    seed: f64,
    prev: f64,
}

impl EmaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let alpha = 2.0 / (n as f64 + 1.0);
        EmaCore {
            n,
            alpha,
            keep: 1.0 - alpha,
            inv: 1.0 / n as f64,
            count: 0,
            seed: 0.0,
            prev: f64::NAN,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.count = 0;
        self.seed = 0.0;
        self.prev = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        self.count += 1;
        if self.count < self.n {
            self.seed += x;
            f64::NAN
        } else if self.count == self.n {
            self.seed += x;
            self.prev = self.seed * self.inv;
            self.prev
        } else {
            self.prev = self.alpha * x + self.keep * self.prev;
            self.prev
        }
    }
}

/// MACD: a fast and a slow EMA of the close, their difference, and an EMA of that difference
/// (started with the first `signal` live differences). `[macd, signal, histogram]`, `NaN` while a
/// line is not defined yet.
#[derive(Clone)]
pub(crate) struct MacdCore {
    fast: EmaCore,
    slow: EmaCore,
    sig: EmaCore,
    /// Pushes still to go before all three averages are seeded; 0 is the branch-free steady state.
    warm: usize,
}

impl MacdCore {
    #[inline]
    pub(crate) fn new(fast: usize, slow: usize, signal: usize) -> Self {
        MacdCore {
            fast: EmaCore::new(fast),
            slow: EmaCore::new(slow),
            sig: EmaCore::new(signal),
            // The line is defined once both averages are seeded (push `max(fast, slow)`), and the
            // signal seeds with the `signal`-th live value.
            warm: fast.max(slow).saturating_add(signal).saturating_sub(1),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.fast.clear();
        self.slow.clear();
        self.sig.clear();
        self.warm = self.fast.n.max(self.slow.n).saturating_add(self.sig.n).saturating_sub(1);
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> [f64; 3] {
        if self.warm == 0 {
            // Steady state: three recurrences and nothing else, in the exact expressions
            // [`EmaCore::push`] uses past its seed, so the stream and the batch agree bit for bit.
            let f = self.fast.alpha * c + self.fast.keep * self.fast.prev;
            let s = self.slow.alpha * c + self.slow.keep * self.slow.prev;
            self.fast.prev = f;
            self.slow.prev = s;
            let m = f - s;
            let g = self.sig.alpha * m + self.sig.keep * self.sig.prev;
            self.sig.prev = g;
            return [m, g, m - g];
        }
        self.warm -= 1;
        let m = self.fast.push(c) - self.slow.push(c);
        if m.is_nan() {
            return [f64::NAN; 3];
        }
        let s = self.sig.push(m);
        [m, s, m - s]
    }
}

/// Bollinger Bands: a `n`-value mean and `mult` population standard deviations either side, from
/// running sums of the window shifted by its oldest value at the last rebuild (so a price near a
/// million with moves of a hundredth keeps its digits), rebuilt every `ceil(512 / n)` laps.
///
/// Warm-up is the `NaN` multiplier of [`SmaCore`]; a `NaN` mean, or a variance that came out
/// negative, takes the cold rebuild path.
#[derive(Clone)]
pub(crate) struct BollingerCore {
    inv: f64,
    inv_n: f64,
    mult: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    s1: f64,
    s2: f64,
}

impl BollingerCore {
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        BollingerCore {
            inv: f64::NAN,
            inv_n: if n == 0 { f64::NAN } else { 1.0 / n as f64 },
            mult,
            buf: vec![0.0; n.clamp(1, RING_CAP)],
            head: 0,
            laps: 0,
            laps_per: SmaCore::laps_per(n),
            shift: 0.0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.shift = 0.0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.inv = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> [f64; 3] {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        self.s1 += d - o;
        self.s2 += d * d - o * o;
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0 on raw prices.
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let (shift, s1, s2) = shifted_sums(&self.buf, 0);
                self.shift = shift;
                self.s1 = s1;
                self.s2 = s2;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let mut m = self.s1 * self.inv;
        let mut var = self.s2 * self.inv - m * m;
        // `NaN` while warming up or with a `NaN` in the window; negative is cancellation. Cold, and
        // written inline over a shared slice: a `&mut self` method call here would keep the sums in
        // memory across every iteration of a caller's loop.
        if var.is_nan() || var < 0.0 {
            if self.inv.is_nan() {
                return [f64::NAN; 3];
            }
            let (shift, s1, s2) = shifted_sums(&self.buf, self.head);
            self.shift = shift;
            self.s1 = s1;
            self.s2 = s2;
            m = s1 * self.inv;
            var = (s2 * self.inv - m * m).max(0.0);
            if var.is_nan() {
                return [f64::NAN; 3];
            }
        }
        let mean = self.shift + m;
        let sd = var.sqrt();
        [mean + self.mult * sd, mean, mean - self.mult * sd]
    }
}

/// The window (oldest value at `head`) as `(shift, sum(x - shift), sum((x - shift)^2))`, `shift`
/// being the oldest value when it is finite.
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

/// Average true range: the true range through Wilder's smoothing, seeded with the mean of the
/// first `n` ranges; the first bar's range is `high - low`. `NaN` until `n` bars are in.
#[derive(Clone)]
pub(crate) struct AtrCore {
    n: usize,
    inv: f64,
    keep: f64,
    count: usize,
    prev_close: f64,
    seed: f64,
    prev: f64,
}

impl AtrCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        AtrCore {
            n,
            inv: 1.0 / n as f64,
            keep: (n as f64 - 1.0) / n as f64,
            count: 0,
            prev_close: f64::NAN,
            seed: 0.0,
            prev: f64::NAN,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.count = 0;
        self.prev_close = f64::NAN;
        self.seed = 0.0;
        self.prev = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let tr = if self.count == 0 {
            high - low
        } else {
            (high - low).max((high - self.prev_close).abs()).max((low - self.prev_close).abs())
        };
        self.prev_close = close;
        self.count += 1;
        if self.count < self.n {
            self.seed += tr;
            f64::NAN
        } else if self.count == self.n {
            self.seed += tr;
            self.prev = self.seed * self.inv;
            self.prev
        } else {
            self.prev = self.prev * self.keep + tr * self.inv;
            self.prev
        }
    }
}
