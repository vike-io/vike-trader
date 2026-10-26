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
//! A NaN in the window makes the result `NaN`, which the cores below notice on the result and answer
//! with a rebuild, so the statistic is exact again the bar after the last one leaves. (The general
//! `Ring` / `Roll*` primitives that used to open this file had no user and are gone; the cores below
//! are what the steppers run.)

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

// ------------------------------------------------------------------------------------------------
// The two hottest indicators get a core of their own: no NaN counters, no `Option`, no `chain`.
// ------------------------------------------------------------------------------------------------

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const SMA_REBUILD: usize = 512;

/// A running sum keeps the rounding error of every term that went through it (about `eps` times the
/// largest sum it has been), until the next rebuild. So when the sum has fallen to under this fraction
/// of the largest it has been since its last rebuild (a bad tick, gone), it is rebuilt on the spot: the
/// carried error would otherwise be a visible share of the answer. It keeps the error under about 1e-11
/// of the sum. (A bad tick usually makes TWO big terms in a derived series, the bar and the next one,
/// so the test is against the peak of the sum and not against the one term that is leaving.)
pub(crate) const PEAK_SHARE: f64 = 1.0e-5;

/// The same for a value that is the only big term a bad tick makes (a typical price, a median price):
/// the rebuild runs when the value that has just left is more than this many times the answer.
pub(crate) const LEAVE: f64 = 1.0e5;

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

/// [`SmaCore`] with the peak test of [`PEAK_SHARE`]: the mean of a series that is small beside its own spikes
/// (a difference such as `close - open`, an ease-of-movement term), whose running sum a bad tick has
/// left a visible rounding error in. One more compare a value than [`SmaCore`], which the plain
/// price averages (`sma` and the rest) do not need and do not pay.
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
pub(crate) struct PeakSmaCore {
    inv: f64,
    inv_n: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    sum: f64,
    /// The largest `|sum|` since the last rebuild.
    peak: f64,
}

impl PeakSmaCore {
    /// Laps of the ring between two rebuilds of the sum.
    #[inline]
    fn laps_per(n: usize) -> usize {
        SmaCore::laps_per(n)
    }

    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        PeakSmaCore {
            inv: f64::NAN,
            // A window of 0 never leaves warm-up: every output is NaN.
            inv_n: if n == 0 { f64::NAN } else { 1.0 / n as f64 },
            buf: vec![0.0; n.clamp(1, RING_CAP)],
            head: 0,
            laps: 0,
            laps_per: Self::laps_per(n),
            sum: 0.0,
            peak: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.sum = 0.0;
        self.peak = 0.0;
        self.inv = f64::NAN;
    }

    /// Push `x`; the mean of the last `n` values, `NaN` until `n` have been pushed.
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        self.sum += x - old;
        let a = self.sum.abs();
        self.peak = if a > self.peak { a } else { self.peak };
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                // The head is 0, so the ring is already oldest-to-newest.
                self.sum = self.buf.iter().fold(0.0, |a, &v| a + v);
                self.peak = self.sum.abs();
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let mut out = self.sum * self.inv;
        // Joined with `|`, so that the two tests are one branch.
        if out.is_nan() | (self.sum.abs() < self.peak * PEAK_SHARE) {
            // Warming up (or a window of 0), a NaN in the window, a stale carried sum, or a term that
            // has just left and dwarfs the sum (a bad tick). The rebuild
            // is a free function over a shared slice, NOT a `&mut self` method: a call that could
            // write the struct keeps `sum`, `head` and `inv` in memory across every iteration of a
            // caller's loop (measured: 1.25 ns a value became 2.8 ns).
            if self.inv.is_nan() {
                return f64::NAN;
            }
            self.sum = ring_sum(&self.buf, self.head);
            self.peak = self.sum.abs();
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
/// negative, takes the cold rebuild path. So does a variance under a hundred thousandth of the mean
/// square of the value that has just LEFT the window: the sums keep the rounding error of every term that
/// went through them (about `eps` times its square), so a bad tick of a thousand times the price level
/// would otherwise keep its error in the deviation until the scheduled rebuild (measured: 1e-2 relative
/// at worst); one compare against a term the update already formed catches it.
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
    /// `inv * 1e-5` once the first lap of the ring has ended, `NaN` before: the share of the leaving
    /// term's square under which the variance is not trusted.
    kp: f64,
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
            kp: f64::NAN,
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
        self.kp = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> [f64; 3] {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        let (d2, o2) = (d * d, o * o);
        self.s1 += d - o;
        self.s2 += d2 - o2;
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
            self.kp = self.inv_n * 1.0e-5;
        }
        let m = self.s1 * self.inv;
        let var = self.s2 * self.inv - m * m;
        // A comparison with a `NaN` is false, so a `NaN` variance (warming up, or a `NaN` in the window)
        // falls through to the cold path with the negative ones (cancellation) and the ones under a
        // hundred thousandth of the mean square of the term that has just left (a bad tick).
        if var >= o2 * self.kp {
            let mean = self.shift + m;
            let sd = var.sqrt();
            return [mean + self.mult * sd, mean, mean - self.mult * sd];
        }
        // Cold, and written inline over a shared slice: a `&mut self` method call here would keep the
        // sums in memory across every iteration of a caller's loop.
        if self.inv.is_nan() {
            return [f64::NAN; 3];
        }
        let (shift, s1, s2) = shifted_sums(&self.buf, self.head);
        self.shift = shift;
        self.s1 = s1;
        self.s2 = s2;
        let m = s1 * self.inv;
        let var = (s2 * self.inv - m * m).max(0.0);
        if var.is_nan() {
            return [f64::NAN; 3];
        }
        let mean = shift + m;
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
