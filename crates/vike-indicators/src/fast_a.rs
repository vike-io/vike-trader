//! Fast cores for group `a` of the second wave of indicators (see `_followups/perf-conv2-brief.md`).
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds
//! no `&mut self` call in its hot part, and is used by BOTH the streaming stepper and the
//! whole-series functions, so a stream and a batch agree bit for bit.
//!
//! The seven: [`WmaCore`] (a running weighted sum), [`CciCore`] (a shifted running mean and one
//! contiguous scan for the mean absolute deviation), [`HiLo`] (the window extremes of two series by
//! the van Herk / Gil-Werman block method) under [`WilliamsCore`], [`StochasticCore`] and
//! [`DonchianCore`], [`AwesomeCore`] (both averages from ONE ring, on a shared shift) and
//! [`KeltnerCore`] (an EMA and a Wilder average of the true range with a branch-free steady state).
//!
//! # NaN
//!
//! The averages follow [`crate::roll::SmaCore`]: a NaN in the window makes the result NaN while it is
//! inside, and the result is exact the update after it left (a cold rebuild from the ring). The
//! extremes skip a NaN as if it were absent, like `f64::max`; a window with no number at all gives
//! `-inf` / `+inf` for its extremes. Keltner's recurrences have no recovery, as before (a NaN
//! poisons them for good).

use crate::roll::{LEAVE, PEAK_SHARE, SmaCore};

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const REBUILD: usize = 512;

/// Laps of a ring of `n` between two rebuilds of its running sums.
#[inline]
fn laps_per(n: usize) -> usize {
    REBUILD.div_ceil(n.max(1)).max(1)
}

// ------------------------------------------------------------------------------------------------
// WMA
// ------------------------------------------------------------------------------------------------

/// Linearly weighted moving average (weights `1..=n`, the newest heaviest) from a running weighted
/// sum: `W' = W - S + n x`, `S' = S - old + x`.
///
/// Warm-up is the zero-filled ring plus a `NaN` multiplier until the first lap ends
/// (see [`SmaCore`]): the zeros are exactly the weights of values that never existed.
#[derive(Clone)]
pub(crate) struct WmaCore {
    n_f: f64,
    inv: f64,
    inv_n: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    sum: f64,
    weighted: f64,
    /// The largest `|weighted|` since the last rebuild.
    peak: f64,
}

impl WmaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let nf = n as f64;
        WmaCore {
            n_f: nf,
            inv: f64::NAN,
            // A window of 0 never leaves warm-up: every output is NaN.
            inv_n: if n == 0 { f64::NAN } else { 2.0 / (nf * (nf + 1.0)) },
            buf: vec![0.0; n.max(1)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            sum: 0.0,
            weighted: 0.0,
            peak: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.sum = 0.0;
        self.weighted = 0.0;
        self.peak = 0.0;
        self.inv = f64::NAN;
    }

    /// Push `x`; the weighted mean of the last `n` values, `NaN` until `n` have been pushed.
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        // The weights all drop by one (the oldest value leaves at weight 0) and `x` enters at `n`.
        self.weighted = (self.weighted - self.sum) + self.n_f * x;
        self.sum += x - old;
        let a = self.weighted.abs();
        self.peak = if a > self.peak { a } else { self.peak };
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                // The head is 0, so the ring is already oldest-to-newest.
                (self.sum, self.weighted) = weighted_sums(&self.buf, 0);
                self.peak = self.weighted.abs();
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let mut out = self.weighted * self.inv;
        // Joined with `|`, so that the two tests are one branch.
        if out.is_nan() | (self.weighted.abs() < self.peak * PEAK_SHARE) {
            // Warming up, a NaN in the window, a carried sum a leaving infinity left NaN, or a term that
            // has just left and left the weighted sum far under its peak (a bad tick).
            if self.inv.is_nan() {
                return f64::NAN;
            }
            (self.sum, self.weighted) = weighted_sums(&self.buf, self.head);
            self.peak = self.weighted.abs();
            out = self.weighted * self.inv;
            if out.is_nan() {
                out = f64::NAN;
            }
        }
        out
    }
}

/// The ring (oldest value at `head`) as `(sum, sum of k * value)` with `k = 1` for the oldest.
#[cold]
#[inline(never)]
fn weighted_sums(buf: &[f64], head: usize) -> (f64, f64) {
    let (mut s, mut w) = (0.0_f64, 0.0_f64);
    for (k, &v) in buf[head..].iter().chain(&buf[..head]).enumerate() {
        s += v;
        w += (k + 1) as f64 * v;
    }
    (s, w)
}

// ------------------------------------------------------------------------------------------------
// CCI
// ------------------------------------------------------------------------------------------------

/// Commodity channel index of the typical price `(h + l + c) / 3`.
///
/// The window is a LINEAR sliding window, not a ring: `buf[p..p + n]` is the last `n` typical prices
/// oldest to newest, each stored minus `shift`; a new one is appended at `p + n` and `p` moves up,
/// and every `max(n, 512)` values the window is copied back to the front (where it is re-centred on
/// its oldest value and the sums are rebuilt, which is the periodic rebuild). The mean is a running
/// sum of those shifted values, so a price near a million with moves of a hundredth keeps its digits;
/// the mean absolute deviation is one scan of the window, in a fixed order (pairs of lanes, four
/// accumulators), so a stream and a series agree bit for bit.
///
/// Why not a ring: a ring is scanned by wide loads, and the slot just written is part of every
/// scan. A wide load over a narrower store still in flight cannot be forwarded, so it waits for the
/// store to commit and the values stop overlapping (measured: 15.5 ns a value for the ring, 8.6 for
/// this). The newest value is summed from its register for the same reason.
#[derive(Clone)]
pub(crate) struct CciCore {
    n: usize,
    /// Window starts at which the window is moved to the front (`max(n, 512)`).
    r: usize,
    inv: f64,
    inv_n: f64,
    /// `n / 0.015`: `(tp - mean) / (0.015 * md)` with `md = deviations / n`.
    scale: f64,
    buf: Vec<f64>,
    p: usize,
    /// The next `p` at which the cold path runs: the end of warm-up (`n`), then every `r`.
    event: usize,
    shift: f64,
    s1: f64,
}

impl CciCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        // A window of 0 never leaves warm-up: every output is NaN.
        let inv_n = if n == 0 { f64::NAN } else { 1.0 / n as f64 };
        let (nn, r) = (n.max(1), n.max(REBUILD));
        CciCore {
            n: nn,
            r,
            inv: f64::NAN,
            inv_n,
            scale: n as f64 / 0.015,
            buf: vec![0.0; nn + r],
            p: 0,
            event: nn,
            shift: 0.0,
            s1: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.p = 0;
        self.event = self.n;
        self.shift = 0.0;
        self.s1 = 0.0;
        self.inv = f64::NAN;
    }

    /// Push a bar; the CCI once `n` bars are in, else `NaN`. A window with no spread gives 0.
    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let n = self.n;
        let tp = (high + low + close) / 3.0;
        let mut d = tp - self.shift;
        let old = self.buf[self.p];
        self.buf[self.p + n] = d;
        self.p += 1;
        self.s1 += d - old;
        if self.p == self.event {
            // End of warm-up, or the window reached the end of the buffer. Cold, and a free function
            // over a shared slice (see [`SmaCore`]).
            let copy = self.p == self.r;
            let (by, s1) = recentre(&mut self.buf, self.p, n, copy);
            if copy {
                self.p = 0;
            }
            self.shift += by;
            self.s1 = s1;
            d -= by;
            self.inv = self.inv_n;
            self.event = self.r;
        }
        let mut m = self.s1 * self.inv;
        if m.is_nan() {
            // Warming up, or a NaN in the window, or a carried sum a leaving infinity left NaN.
            if self.inv.is_nan() {
                return f64::NAN;
            }
            let (by, s1) = recentre(&mut self.buf, self.p, n, false);
            self.shift += by;
            self.s1 = s1;
            d -= by;
            m = s1 * self.inv;
            if m.is_nan() {
                return f64::NAN;
            }
        }
        // All but the newest value by wide loads of slots stored earlier; the newest from `d`.
        let mut dev = abs_dev_sum(&self.buf[self.p..self.p + n - 1], m) + (d - m).abs();
        if old.abs() > dev * LEAVE {
            // The value that has just left dwarfs the spread of the window (a bad tick, gone): its
            // rounding error is in the sum, so the mean is renewed.
            let (by, s1) = recentre(&mut self.buf, self.p, n, false);
            self.shift += by;
            self.s1 = s1;
            d -= by;
            m = s1 * self.inv;
            dev = abs_dev_sum(&self.buf[self.p..self.p + n - 1], m) + (d - m).abs();
        }
        if dev > 0.0 { (d - m) * self.scale / dev } else { 0.0 }
    }
}

/// Re-centre the window `buf[p..p + n]` (after moving it to the front when `copy`) on its oldest
/// value when that is finite; returns that value and the sum of the re-centred window.
#[cold]
#[inline(never)]
fn recentre(buf: &mut [f64], p: usize, n: usize, copy: bool) -> (f64, f64) {
    let start = if copy {
        buf.copy_within(p..p + n, 0);
        0
    } else {
        p
    };
    let w = &mut buf[start..start + n];
    let by = if w[0].is_finite() { w[0] } else { 0.0 };
    let mut s1 = 0.0_f64;
    for v in w.iter_mut() {
        *v -= by;
        s1 += *v;
    }
    (by, s1)
}

/// `sum |v - m|` over `w` in a fixed order: four accumulators of two lanes each (each lane a
/// stride of eight values), then the remainder one by one.
#[inline(always)]
fn abs_dev_sum(w: &[f64], m: f64) -> f64 {
    let mut acc = [[0.0_f64; 2]; 4];
    let mut chunks = w.chunks_exact(8);
    for c in &mut chunks {
        for (a, p) in acc.iter_mut().zip(c.chunks_exact(2)) {
            a[0] += (p[0] - m).abs();
            a[1] += (p[1] - m).abs();
        }
    }
    let mut tail = 0.0_f64;
    for &v in chunks.remainder() {
        tail += (v - m).abs();
    }
    (((acc[0][0] + acc[0][1]) + (acc[1][0] + acc[1][1]))
        + ((acc[2][0] + acc[2][1]) + (acc[3][0] + acc[3][1])))
        + tail
}

// ------------------------------------------------------------------------------------------------
// Window extremes (Williams %R, Stochastic, Donchian)
// ------------------------------------------------------------------------------------------------

/// The highest high and the lowest low of the last `n` bars in O(1) a bar with no deque (the van
/// Herk / Gil-Werman block method).
///
/// Bars are taken in blocks of `n`. Inside a block the running extreme of the block so far is one
/// scalar; the extreme of the part of the PREVIOUS block that is still in the window is a suffix
/// extreme, and those are computed in place over the finished block once per `n` bars (about two
/// compares a bar, amortised). The window extreme is the larger of the two. The slot a bar is
/// written to is the slot its suffix extreme was just read from, which is why one array of `n + 1`
/// pairs (`[high, low]`; the last pair is a `-inf / +inf` sentinel) is enough.
///
/// A NaN is skipped as if absent (`x > best` is false for it). Warm-up costs nothing: the suffix
/// slots of the first block start as `NaN`, and the larger-of-two is written `if a >= b { a } else
/// { b }`, which hands a `NaN` `b` through; so the returned pair is `NaN` until the first block ends
/// (its last bar reads the sentinel instead).
///
/// Measured: a serial suffix pass and raw stores are faster than a grouped pass over sanitised
/// stores (3.5 against 4.8 ns a value for Williams %R(14)), and a plain scan of the window costs 13.
#[derive(Clone)]
pub(crate) struct HiLo {
    n: usize,
    buf: Vec<[f64; 2]>,
    pos: usize,
    pre: [f64; 2],
}

impl HiLo {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.max(1);
        let mut h = HiLo { n, buf: vec![[f64::NAN; 2]; n + 1], pos: 0, pre: Self::EMPTY };
        h.buf[n] = Self::EMPTY;
        h
    }

    /// The identity of `[max, min]`, and the sentinel after a block.
    const EMPTY: [f64; 2] = [f64::NEG_INFINITY, f64::INFINITY];

    pub(crate) fn clear(&mut self) {
        self.buf.fill([f64::NAN; 2]);
        self.buf[self.n] = Self::EMPTY;
        self.pos = 0;
        self.pre = Self::EMPTY;
    }

    /// Push a bar; `[highest high, lowest low]` of the last `n` bars, `NaN` until `n` are in.
    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> [f64; 2] {
        let s = self.buf[self.pos + 1];
        self.buf[self.pos] = [high, low];
        let ph = if high > self.pre[0] { high } else { self.pre[0] };
        let pl = if low < self.pre[1] { low } else { self.pre[1] };
        self.pre = [ph, pl];
        let hh = if ph >= s[0] { ph } else { s[0] };
        let ll = if pl <= s[1] { pl } else { s[1] };
        self.pos += 1;
        if self.pos == self.n {
            self.pos = 0;
            suffix_extremes(&mut self.buf);
            self.pre = Self::EMPTY;
        }
        [hh, ll]
    }
}

/// Turn a finished block (`buf[..n]`, with the sentinel in `buf[n]`) into its suffix extremes in place.
#[inline(never)]
fn suffix_extremes(buf: &mut [[f64; 2]]) {
    let mut hi = f64::NEG_INFINITY;
    let mut lo = f64::INFINITY;
    for v in buf.iter_mut().rev().skip(1) {
        if v[0] > hi {
            hi = v[0];
        }
        if v[1] < lo {
            lo = v[1];
        }
        *v = [hi, lo];
    }
}

/// Williams %R: `-100 * (highest high - close) / (highest high - lowest low)`, 0 when the window
/// has no range.
#[derive(Clone)]
pub(crate) struct WilliamsCore {
    win: HiLo,
}

impl WilliamsCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        WilliamsCore { win: HiLo::new(n) }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let [hh, ll] = self.win.push(high, low);
        raw_williams(hh, ll, close)
    }
}

/// `-100 * (hh - c) / (hh - ll)`; `NaN` stays `NaN` (the warm-up), an empty range is 0.
#[inline(always)]
fn raw_williams(hh: f64, ll: f64, close: f64) -> f64 {
    let rng = hh - ll;
    if rng <= 0.0 { 0.0 } else { -100.0 * (hh - close) / rng }
}

/// Slow stochastic: the raw `%K = 100 * (close - lowest low) / range` over `n` bars, averaged over
/// `smooth_k` for `%K` and again over `smooth_d` for `%D`. `[%K, %D]`.
#[derive(Clone)]
pub(crate) struct StochasticCore {
    win: HiLo,
    k: SmaCore,
    d: SmaCore,
}

impl StochasticCore {
    #[inline]
    pub(crate) fn new(n: usize, smooth_k: usize, smooth_d: usize) -> Self {
        StochasticCore { win: HiLo::new(n), k: SmaCore::new(smooth_k), d: SmaCore::new(smooth_d) }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
        self.k.clear();
        self.d.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        let [hh, ll] = self.win.push(high, low);
        let rng = hh - ll;
        let raw = if rng <= 0.0 { 0.0 } else { 100.0 * (close - ll) / rng };
        let k = self.k.push(raw);
        let d = self.d.push(k);
        [k, d]
    }
}

/// Donchian channel: `[highest high, midpoint, lowest low]` of the last `n` bars.
#[derive(Clone)]
pub(crate) struct DonchianCore {
    win: HiLo,
}

impl DonchianCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        DonchianCore { win: HiLo::new(n) }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> [f64; 3] {
        let [hh, ll] = self.win.push(high, low);
        let mid = (hh + ll) * 0.5;
        // `NaN` while warming up, and for a window with no number in it (`-inf + inf`).
        if mid.is_nan() { [f64::NAN; 3] } else { [hh, mid, ll] }
    }
}

// ------------------------------------------------------------------------------------------------
// Awesome oscillator
// ------------------------------------------------------------------------------------------------

/// Awesome oscillator: the `fast`-bar mean minus the `slow`-bar mean of the median price.
///
/// Both means come from ONE ring (as long as the longer window) and ONE shift, so the difference is
/// `sum_fast / fast - sum_slow / slow` over `x - shift` and keeps its digits however far the price
/// is from zero. A rebuild re-derives the shift and both sums.
#[derive(Clone)]
pub(crate) struct AwesomeCore {
    fast_n: usize,
    slow_n: usize,
    inv_fast: f64,
    inv_slow: f64,
    inv_fast_n: f64,
    inv_slow_n: f64,
    buf: Vec<f64>,
    head: usize,
    /// Where the value that leaves the fast (slow) window as the next one enters is.
    fast_at: usize,
    slow_at: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    s_fast: f64,
    s_slow: f64,
}

impl AwesomeCore {
    #[inline]
    pub(crate) fn new(fast: usize, slow: usize) -> Self {
        let cap = fast.max(slow).max(1);
        let inv = |n: usize| if n == 0 { f64::NAN } else { 1.0 / n as f64 };
        AwesomeCore {
            fast_n: fast,
            slow_n: slow,
            inv_fast: f64::NAN,
            inv_slow: f64::NAN,
            inv_fast_n: inv(fast),
            inv_slow_n: inv(slow),
            buf: vec![0.0; cap],
            head: 0,
            fast_at: Self::leaving(cap, fast),
            slow_at: Self::leaving(cap, slow),
            laps: 0,
            laps_per: laps_per(cap),
            shift: 0.0,
            s_fast: 0.0,
            s_slow: 0.0,
        }
    }

    /// The ring slot holding the value that leaves a window of `n` when the head slot is written.
    #[inline]
    fn leaving(cap: usize, n: usize) -> usize {
        if n == 0 || n >= cap { 0 } else { cap - n }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.fast_at = Self::leaving(self.buf.len(), self.fast_n);
        self.slow_at = Self::leaving(self.buf.len(), self.slow_n);
        self.laps = 0;
        self.shift = 0.0;
        self.s_fast = 0.0;
        self.s_slow = 0.0;
        self.inv_fast = f64::NAN;
        self.inv_slow = f64::NAN;
    }

    /// Push a bar; the oscillator once the longer window is full, else `NaN`.
    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> f64 {
        let x = (high + low) * 0.5;
        let cap = self.buf.len();
        let leaves_fast = self.buf[self.fast_at];
        let leaves_slow = self.buf[self.slow_at];
        self.buf[self.head] = x;
        self.head += 1;
        self.fast_at += 1;
        if self.fast_at == cap {
            self.fast_at = 0;
        }
        self.slow_at += 1;
        if self.slow_at == cap {
            self.slow_at = 0;
        }
        self.s_fast += x - leaves_fast;
        self.s_slow += x - leaves_slow;
        if self.head == cap {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0.
            if self.laps >= self.laps_per || self.inv_fast.is_nan() {
                (self.shift, self.s_fast, self.s_slow) =
                    two_shifted_sums(&self.buf, 0, self.fast_n, self.slow_n);
                self.laps = 0;
            }
            self.inv_fast = self.inv_fast_n;
            self.inv_slow = self.inv_slow_n;
        }
        let mut out = self.s_fast * self.inv_fast - self.s_slow * self.inv_slow;
        // A value that has just left either window and dwarfs the answer left its rounding error in the
        // sum. Joined with `|`, so that the tests are one branch.
        let gone = ((leaves_fast - self.shift).abs() * self.inv_fast_n > out.abs() * LEAVE)
            | ((leaves_slow - self.shift).abs() * self.inv_slow_n > out.abs() * LEAVE);
        if out.is_nan() | gone {
            if self.inv_fast.is_nan() || self.inv_slow.is_nan() {
                return f64::NAN;
            }
            (self.shift, self.s_fast, self.s_slow) =
                two_shifted_sums(&self.buf, self.head, self.fast_n, self.slow_n);
            out = self.s_fast * self.inv_fast - self.s_slow * self.inv_slow;
            if out.is_nan() {
                out = f64::NAN;
            }
        }
        out
    }
}

/// The ring (oldest value at `head`) as `(shift, sum of the newest fast values - shift, sum of the
/// newest slow values - shift)`, `shift` being the oldest value when it is finite.
#[cold]
#[inline(never)]
fn two_shifted_sums(buf: &[f64], head: usize, fast: usize, slow: usize) -> (f64, f64, f64) {
    let cap = buf.len();
    let shift = if buf[head].is_finite() { buf[head] } else { 0.0 };
    let (mut sf, mut ss) = (0.0_f64, 0.0_f64);
    for (k, &v) in buf[head..].iter().chain(&buf[..head]).enumerate() {
        let d = v - shift;
        if k + fast >= cap {
            sf += d;
        }
        if k + slow >= cap {
            ss += d;
        }
    }
    (shift, sf, ss)
}

// ------------------------------------------------------------------------------------------------
// Keltner channel
// ------------------------------------------------------------------------------------------------

/// Keltner channel: an EMA of the close (seeded with the mean of the first `ema_n` closes) and
/// `mult` Wilder-smoothed true ranges (seeded with the mean of the first `atr_n`) either side.
/// `[upper, mid, lower]`; the mid line is defined from bar `ema_n`, the bands from the later of the
/// two periods.
///
/// Like [`crate::roll::MacdCore`]: `warm == 0` is a branch-free steady state, the seed ladders run
/// for the first `max(ema_n, atr_n)` bars only.
#[derive(Clone)]
pub(crate) struct KeltnerCore {
    ema_n: usize,
    atr_n: usize,
    mult: f64,
    alpha: f64,
    keep_e: f64,
    inv_e: f64,
    inv_a: f64,
    keep_a: f64,
    /// Bars still to go before both averages are seeded; 0 is the steady state.
    warm: usize,
    count: usize,
    prev_close: f64,
    seed_e: f64,
    seed_a: f64,
    ema: f64,
    atr: f64,
}

impl KeltnerCore {
    #[inline]
    pub(crate) fn new(ema_n: usize, atr_n: usize, mult: f64) -> Self {
        let alpha = 2.0 / (ema_n as f64 + 1.0);
        KeltnerCore {
            ema_n,
            atr_n,
            mult,
            alpha,
            keep_e: 1.0 - alpha,
            inv_e: 1.0 / ema_n as f64,
            inv_a: 1.0 / atr_n as f64,
            keep_a: (atr_n as f64 - 1.0) / atr_n as f64,
            warm: ema_n.max(atr_n),
            count: 0,
            prev_close: f64::NAN,
            seed_e: 0.0,
            seed_a: 0.0,
            ema: f64::NAN,
            atr: f64::NAN,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.warm = self.ema_n.max(self.atr_n);
        self.count = 0;
        self.prev_close = f64::NAN;
        self.seed_e = 0.0;
        self.seed_a = 0.0;
        self.ema = f64::NAN;
        self.atr = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> [f64; 3] {
        let mid;
        let a;
        if self.warm == 0 {
            // Steady state: the two recurrences and nothing else, in the expressions the seed
            // ladder below uses past each seed.
            let tr =
                (high - low).max((high - self.prev_close).abs()).max((low - self.prev_close).abs());
            self.prev_close = close;
            self.ema = self.alpha * close + self.keep_e * self.ema;
            self.atr = self.atr * self.keep_a + tr * self.inv_a;
            mid = self.ema;
            a = self.atr;
        } else {
            self.warm -= 1;
            let tr = if self.count == 0 {
                high - low
            } else {
                (high - low).max((high - self.prev_close).abs()).max((low - self.prev_close).abs())
            };
            self.prev_close = close;
            self.count += 1;
            mid = if self.count < self.ema_n {
                self.seed_e += close;
                f64::NAN
            } else if self.count == self.ema_n {
                self.seed_e += close;
                self.ema = self.seed_e * self.inv_e;
                self.ema
            } else {
                self.ema = self.alpha * close + self.keep_e * self.ema;
                self.ema
            };
            a = if self.count < self.atr_n {
                self.seed_a += tr;
                f64::NAN
            } else if self.count == self.atr_n {
                self.seed_a += tr;
                self.atr = self.seed_a * self.inv_a;
                self.atr
            } else {
                self.atr = self.atr * self.keep_a + tr * self.inv_a;
                self.atr
            };
        }
        [mid + self.mult * a, mid, mid - self.mult * a]
    }
}
