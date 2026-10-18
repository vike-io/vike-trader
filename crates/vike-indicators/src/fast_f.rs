//! Fast cores for group `f` of the third wave of indicators (see `_followups/perf-conv3-brief.md`).
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds
//! no `&mut self` call in its hot part (the one cold call, once a lap, is a free function over a
//! shared slice), and is used by BOTH the streaming stepper and the whole-series functions, so a
//! stream and a batch agree bit for bit.
//!
//! Ten indicators: the rate-of-change family (`rocp`, `rocr`, `rocr100`: [`LagCore`] and the three
//! `*_of` expressions), the balance of power ([`bop_of`]), the detrended price oscillator
//! ([`DpoCore`]), the accelerator oscillator ([`AcCore`]), the Aroon oscillator ([`AroonOscCore`]),
//! Elder ray ([`ElderRayCore`]), the ultimate oscillator ([`UltoscCore`]) and the Vortex indicator
//! ([`VortexCore`]).
//!
//! The windowed sums behind `ultosc` and `vortex` are EXACT window functions, never carried
//! (`crate::fast_d1::PairSums`, and [`TriSums`] here, which is the same scheme with three lanes): a
//! flat stretch gives exactly zero, so the ratios the kernels guard against a zero total see a true
//! zero rather than a rounding residue, and a `NaN` leaves no trace once it is out of the window.

use crate::fast_d1::{LapArgMax2, PairSums};
use crate::roll::{EmaCore, SmaCore};

// ------------------------------------------------------------------------------------------------
// Rate of change.
// ------------------------------------------------------------------------------------------------

/// The value pushed `n` pushes ago. The ring starts as `NaN`, so the first `n` answers are `NaN`
/// with no warm-up branch.
#[derive(Clone)]
pub(crate) struct LagCore {
    buf: Vec<f64>,
    head: usize,
}

impl LagCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        LagCore { buf: vec![f64::NAN; n.max(1)], head: 0 }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(f64::NAN);
        self.head = 0;
    }

    /// Push `x`; the value pushed `n` pushes before it (`NaN` before there was one).
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        if self.head == self.buf.len() {
            self.head = 0;
        }
        old
    }
}

/// `(x - old) / old`; `NaN` where `old` is zero.
#[inline(always)]
pub(crate) fn rocp_of(x: f64, old: f64) -> f64 {
    // The division is made either way and the answer selected, so the loop has no branch to keep.
    let q = (x - old) / old;
    if old != 0.0 { q } else { f64::NAN }
}

/// `x / old`; `NaN` where `old` is zero.
#[inline(always)]
pub(crate) fn rocr_of(x: f64, old: f64) -> f64 {
    let q = x / old;
    if old != 0.0 { q } else { f64::NAN }
}

/// `x / old * 100`; `NaN` where `old` is zero.
#[inline(always)]
pub(crate) fn rocr100_of(x: f64, old: f64) -> f64 {
    let q = x / old * 100.0;
    if old != 0.0 { q } else { f64::NAN }
}

/// One rate-of-change core: a [`LagCore`] and the expression that turns the close and the close
/// `n` values ago into the answer.
macro_rules! roc_core {
    ($(#[$m:meta])* $Core:ident, $of:ident) => {
        $(#[$m])*
        #[derive(Clone)]
        pub(crate) struct $Core {
            lag: LagCore,
        }

        impl $Core {
            #[inline]
            pub(crate) fn new(n: usize) -> Self {
                $Core { lag: LagCore::new(n) }
            }

            pub(crate) fn clear(&mut self) {
                self.lag.clear();
            }

            #[inline(always)]
            pub(crate) fn push(&mut self, x: f64) -> f64 {
                $of(x, self.lag.push(x))
            }
        }
    };
}

roc_core! {
    /// Rate-of-change proportion: `(x[i] - x[i - n]) / x[i - n]`; `NaN` for the first `n` values.
    RocpCore, rocp_of
}
roc_core! {
    /// Rate-of-change ratio: `x[i] / x[i - n]`; `NaN` for the first `n` values.
    RocrCore, rocr_of
}
roc_core! {
    /// Rate-of-change ratio times 100: `x[i] / x[i - n] * 100`; `NaN` for the first `n` values.
    Rocr100Core, rocr100_of
}

// ------------------------------------------------------------------------------------------------
// Balance of power.
// ------------------------------------------------------------------------------------------------

/// Balance of power: `(close - open) / (high - low)`, `0` on a bar with no range. No state, so no
/// warm-up: the first bar has a value.
#[inline(always)]
pub(crate) fn bop_of(open: f64, high: f64, low: f64, close: f64) -> f64 {
    let rng = high - low;
    // The division is made either way and the answer selected, so the loop has no branch to keep.
    let q = (close - open) / rng;
    if rng != 0.0 { q } else { 0.0 }
}

// ------------------------------------------------------------------------------------------------
// Detrended price oscillator.
// ------------------------------------------------------------------------------------------------

/// Detrended price oscillator: the close `n / 2 + 1` bars back minus the `n`-bar mean of the close.
/// `NaN` until both are defined.
#[derive(Clone)]
pub(crate) struct DpoCore {
    mean: SmaCore,
    lag: LagCore,
}

impl DpoCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        DpoCore { mean: SmaCore::new(n), lag: LagCore::new(n / 2 + 1) }
    }

    pub(crate) fn clear(&mut self) {
        self.mean.clear();
        self.lag.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let then = self.lag.push(x);
        then - self.mean.push(x)
    }
}

// ------------------------------------------------------------------------------------------------
// Accelerator oscillator.
// ------------------------------------------------------------------------------------------------

/// Accelerator oscillator: the awesome oscillator (`SMA5 - SMA34` of the median price) minus its
/// own 5-value mean. `NaN` for the first 37 values.
///
/// The 5-value mean runs over the DEFINED values of the awesome oscillator only, as the kernel's
/// `smooth_defined` did: a `NaN` awesome value (a `NaN` in the 34-bar window) is skipped, not
/// averaged, so the window of five reaches over the gap.
#[derive(Clone)]
pub(crate) struct AcCore {
    fast: SmaCore,
    slow: SmaCore,
    sig: SmaCore,
}

impl AcCore {
    #[inline]
    pub(crate) fn new() -> Self {
        AcCore { fast: SmaCore::new(5), slow: SmaCore::new(34), sig: SmaCore::new(5) }
    }

    pub(crate) fn clear(&mut self) {
        self.fast.clear();
        self.slow.clear();
        self.sig.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> f64 {
        let median = (high + low) / 2.0;
        let ao = self.fast.push(median) - self.slow.push(median);
        if ao.is_nan() {
            return f64::NAN;
        }
        ao - self.sig.push(ao)
    }
}

// ------------------------------------------------------------------------------------------------
// Aroon oscillator.
// ------------------------------------------------------------------------------------------------

/// Aroon oscillator: aroon up minus aroon down, `100 * (age of the lowest low - age of the highest
/// high) / n`, from the same windows as `Aroon` (`crate::fast_d1::LapArgMax2`). The two lines are
/// never formed: the ages are whole numbers, so the difference is exact and one multiplication by
/// `100 / n` replaces two divisions. `NaN` for the first `n` values.
#[derive(Clone)]
pub(crate) struct AroonOscCore {
    win: LapArgMax2,
    n: usize,
    scale: f64,
    warm: usize,
}

impl AroonOscCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.max(1);
        AroonOscCore { win: LapArgMax2::new(n + 1), n, scale: 100.0 / n as f64, warm: n }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
        self.warm = self.n;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> f64 {
        // The lowest low is the highest of the negated lows.
        let [_, hs, _, ls] = self.win.push(high, -low);
        if self.warm != 0 {
            self.warm -= 1;
            return f64::NAN;
        }
        // A window with no number in it has an age of `NaN`.
        (ls - hs) * self.scale
    }
}

// ------------------------------------------------------------------------------------------------
// Elder ray.
// ------------------------------------------------------------------------------------------------

/// Elder ray: bull power (`high - EMA(close)`) and bear power (`low - EMA(close)`), the EMA seeded
/// with the mean of its first `n` closes. `NaN` until `n` closes are in; a `NaN` close poisons the
/// average for good, like every EMA.
#[derive(Clone)]
pub(crate) struct ElderRayCore {
    ema: EmaCore,
}

impl ElderRayCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        ElderRayCore { ema: EmaCore::new(n) }
    }

    pub(crate) fn clear(&mut self) {
        self.ema.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        let e = self.ema.push(close);
        [high - e, low - e]
    }
}

// ------------------------------------------------------------------------------------------------
// Ultimate oscillator.
// ------------------------------------------------------------------------------------------------

/// Ultimate oscillator: `100 * (4 * a1 + 2 * a2 + a3) / 7`, `a` being the buying pressure over the
/// true range summed across `p1`, `p2` and `p3` bars (`0` where a window has no range). The first
/// bar has no previous close and is not part of any reported window. `NaN` for the first
/// `max(p1, p2, p3)` values.
///
/// The true low and high use `f64::min` / `f64::max`, so a `NaN` high or low is replaced by the
/// previous close, as the kernel's was.
#[derive(Clone)]
pub(crate) struct UltoscCore {
    prev: f64,
    s1: PairSums,
    s2: PairSums,
    s3: PairSums,
}

impl UltoscCore {
    #[inline]
    pub(crate) fn new(p1: usize, p2: usize, p3: usize) -> Self {
        UltoscCore {
            prev: f64::NAN,
            s1: PairSums::new(p1, true),
            s2: PairSums::new(p2, true),
            s3: PairSums::new(p3, true),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.prev = f64::NAN;
        self.s1.clear(true);
        self.s2.clear(true);
        self.s3.clear(true);
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let true_low = low.min(self.prev);
        let true_high = high.max(self.prev);
        self.prev = close;
        let bp = close - true_low;
        let tr = true_high - true_low;
        let [b1, t1] = self.s1.push(bp, tr);
        let [b2, t2] = self.s2.push(bp, tr);
        let [b3, t3] = self.s3.push(bp, tr);
        // One division for the three ratios over a common denominator. Normal in the steady state;
        // a window with no range (a zero total), a `NaN` total (the window is not complete, or holds
        // a `NaN`) or a product out of the normal range takes the three-ratio form, whose
        // zero guards and `NaN` are the kernel's.
        let p = t1 * t2 * t3;
        if p.is_normal() {
            let num = 4.0 * b1 * (t2 * t3) + 2.0 * b2 * (t1 * t3) + b3 * (t1 * t2);
            return 100.0 * num / (7.0 * p);
        }
        ratios(b1, t1, b2, t2, b3, t3)
    }
}

/// The ultimate oscillator from the three sums of buying pressure and of true range, one division a
/// ratio, `0` for a ratio whose range total is zero. Cold: a free function over plain numbers, so
/// the caller's state stays in registers.
#[cold]
#[inline(never)]
fn ratios(b1: f64, t1: f64, b2: f64, t2: f64, b3: f64, t3: f64) -> f64 {
    let a1 = if t1 != 0.0 { b1 / t1 } else { 0.0 };
    let a2 = if t2 != 0.0 { b2 / t2 } else { 0.0 };
    let a3 = if t3 != 0.0 { b3 / t3 } else { 0.0 };
    100.0 * (4.0 * a1 + 2.0 * a2 + a3) / 7.0
}

// ------------------------------------------------------------------------------------------------
// Vortex.
// ------------------------------------------------------------------------------------------------

/// The sums of three per-value quantities over the last `n` values: [`PairSums`] with a third lane
/// (see there for the scheme). The first value pushed is not part of any window that is reported.
#[derive(Clone)]
struct TriSums {
    n: usize,
    head: usize,
    /// `n + 1` entries; `ring[n]` is the zero the last suffix sum is read against.
    ring: Vec<[f64; 3]>,
    back: [f64; 3],
}

impl TriSums {
    #[inline]
    fn new(n: usize) -> Self {
        let n = n.max(1);
        let mut s = TriSums { n, head: 0, ring: vec![[0.0; 3]; n + 1], back: [0.0; 3] };
        s.clear();
        s
    }

    fn clear(&mut self) {
        self.head = 0;
        self.ring.fill([0.0; 3]);
        self.back = [f64::NAN; 3];
    }

    #[inline(always)]
    fn push(&mut self, a: f64, b: f64, c: f64) -> [f64; 3] {
        let p = self.head;
        let tail = self.ring[p + 1];
        self.ring[p] = [a, b, c];
        let back = [self.back[0] + a, self.back[1] + b, self.back[2] + c];
        self.back = back;
        self.head = p + 1;
        if self.head == self.n {
            self.head = 0;
            self.back = [0.0; 3];
            suffix_sums3(&mut self.ring[..self.n]);
        }
        [tail[0] + back[0], tail[1] + back[1], tail[2] + back[2]]
    }
}

/// `lap[k]` becomes the sum of `lap[k..]`, per lane. Once per lap; a free function over a slice
/// the caller's state does not alias.
#[inline(never)]
fn suffix_sums3(lap: &mut [[f64; 3]]) {
    let (mut a, mut b, mut c) = (0.0_f64, 0.0_f64, 0.0_f64);
    for v in lap.iter_mut().rev() {
        a += v[0];
        b += v[1];
        c += v[2];
        *v = [a, b, c];
    }
}

/// Vortex indicator: `VI+ = sum(|high - previous low|) / sum(true range)` and
/// `VI- = sum(|low - previous high|) / sum(true range)` over the last `n` bars; both `NaN` where the
/// true range sums to nothing (or is `NaN`). The first bar has no previous bar and is not part of any
/// reported window. `NaN` for the first `n` values.
#[derive(Clone)]
pub(crate) struct VortexCore {
    prev_high: f64,
    prev_low: f64,
    prev_close: f64,
    sums: TriSums,
}

impl VortexCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        VortexCore {
            prev_high: f64::NAN,
            prev_low: f64::NAN,
            prev_close: f64::NAN,
            sums: TriSums::new(n),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.prev_high = f64::NAN;
        self.prev_low = f64::NAN;
        self.prev_close = f64::NAN;
        self.sums.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        // `max` returns the other operand for a `NaN`, so the first bar's range is `high - low`.
        let tr =
            (high - low).max((high - self.prev_close).abs()).max((low - self.prev_close).abs());
        let vp = (high - self.prev_low).abs();
        let vm = (low - self.prev_high).abs();
        self.prev_high = high;
        self.prev_low = low;
        self.prev_close = close;
        let [st, sp, sm] = self.sums.push(tr, vp, vm);
        if st > 0.0 {
            let inv = 1.0 / st;
            [sp * inv, sm * inv]
        } else {
            [f64::NAN; 2]
        }
    }
}
