//! Fast cores for group `h` of the third wave of indicators (see `_followups/perf-conv3-brief.md`):
//! `gmma`, `alligator`, `relative_vigor`, `high_low_52w`, `fisher`, `chande_kroll_stop`,
//! `connors_rsi`, `hvol`, `relative_volatility`, `median_ma`, `linreg_channel`, `mcginley` and `asi`.
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds no
//! `&mut self` call in the hot part of the steady state, and is used by BOTH the streaming stepper and
//! the whole-series functions, so a stream and a batch agree bit for bit.
//!
//! What replaced what. Before this wave every rolling window here was refolded over a `VecDeque` on
//! every bar (a standard deviation, a rolling extreme, a regression, a sort for the median). Now:
//!
//! - rolling extremes are the lap maxima of [`LapMax2`] (O(1) a bar, no data-dependent branch);
//! - rolling sums are the exact suffix sums of [`PairSums`] (`relative_vigor`) or the shifted running
//!   sums of [`VarCore`] and [`LinregCore`], rebuilt from the window every `ceil(512 / n)` laps;
//! - the recurrences (EMAs, Wilder averages) multiply by a precomputed `1/n` or `(n-1)/n` and carry a
//!   `warm` counter, so the steady state is branch-free;
//! - the median keeps its window sorted and moves ONE element a bar.
//!
//! Two laws of the old kernels are kept on purpose, both by deciding the case instead of hoping the
//! arithmetic gets there: a window of bit-identical values has a standard deviation of EXACTLY zero
//! (`VarCore`), and a window with a `NaN` in it answers `NaN` while the `NaN` is inside and exactly the
//! textbook number the update after it left (`VarCore`, `LinregCore`, `MedianCore`, `PairSums`,
//! `LapMax2`).

#![allow(clippy::needless_range_loop)]

use crate::fast_d1::{LapMax2, PairSums};
use crate::roll::{RING_CAP, RsiCore};

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const REBUILD: usize = 512;

/// Laps of a ring between two rebuilds of a running sum.
#[inline]
fn laps_per(n: usize) -> usize {
    REBUILD.div_ceil(n.max(1)).max(1)
}

/// The window as `(shift, sum(x - shift), sum((x - shift)^2))`, the oldest value (at `head`) being
/// the shift when it is finite.
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

// ------------------------------------------------------------------------------------------------
// gmma
// ------------------------------------------------------------------------------------------------

/// The twelve EMA periods of the Guppy multiple moving average, shortest first.
pub(crate) const GMMA_PERIODS: [usize; 12] = [3, 5, 8, 10, 12, 15, 30, 35, 40, 45, 50, 60];

/// Twelve EMAs of one input, each seeded with the mean of its first `n` values; a line is `NaN` until
/// its own EMA has seeded. Once the longest has seeded the steady state is twelve independent
/// recurrences over three arrays and nothing else, which the compiler runs as packed arithmetic.
#[derive(Clone)]
pub(crate) struct GmmaCore {
    alpha: [f64; 12],
    keep: [f64; 12],
    seed: [f64; 12],
    prev: [f64; 12],
    /// Values pushed so far, up to the longest period.
    count: usize,
}

impl GmmaCore {
    #[inline]
    pub(crate) fn new() -> Self {
        let alpha = GMMA_PERIODS.map(|n| 2.0 / (n as f64 + 1.0));
        GmmaCore {
            alpha,
            keep: alpha.map(|a| 1.0 - a),
            seed: [0.0; 12],
            prev: [f64::NAN; 12],
            count: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.seed = [0.0; 12];
        self.prev = [f64::NAN; 12];
        self.count = 0;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> [f64; 12] {
        if self.count >= GMMA_PERIODS[11] {
            for ((p, &a), &k) in self.prev.iter_mut().zip(&self.alpha).zip(&self.keep) {
                *p = a * x + k * *p;
            }
            return self.prev;
        }
        self.warm(x)
    }

    /// The first 60 values: the seeds. A line is `NaN` (its `prev` is still `NaN`) until it seeds.
    #[inline(never)]
    fn warm(&mut self, x: f64) -> [f64; 12] {
        self.count += 1;
        for k in 0..12 {
            let n = GMMA_PERIODS[k];
            if self.count < n {
                self.seed[k] += x;
            } else if self.count == n {
                self.seed[k] += x;
                self.prev[k] = self.seed[k] / n as f64;
            } else {
                self.prev[k] = self.alpha[k] * x + self.keep[k] * self.prev[k];
            }
        }
        self.prev
    }
}

// ------------------------------------------------------------------------------------------------
// alligator
// ------------------------------------------------------------------------------------------------

/// The Alligator's smoothing periods (jaw, teeth, lips) and how many bars forward each is plotted.
const ALLIGATOR_N: [usize; 3] = [13, 8, 5];
const ALLIGATOR_SHIFT: [usize; 3] = [8, 5, 3];

/// Williams Alligator: Wilder smoothing of the median price over 13, 8 and 5 bars, plotted 8, 5 and 3
/// bars forward. The forward plot is a read of the raw values `shift` bars back from a 16-slot ring
/// (a power of two, so the wrap is a mask); a slot that was never written is `NaN`.
#[derive(Clone)]
pub(crate) struct AlligatorCore {
    inv: [f64; 3],
    keep: [f64; 3],
    seed: [f64; 3],
    prev: [f64; 3],
    count: usize,
    hist: [[f64; 3]; 16],
    idx: usize,
}

impl AlligatorCore {
    #[inline]
    pub(crate) fn new() -> Self {
        AlligatorCore {
            inv: ALLIGATOR_N.map(|n| 1.0 / n as f64),
            keep: ALLIGATOR_N.map(|n| (n as f64 - 1.0) / n as f64),
            seed: [0.0; 3],
            prev: [f64::NAN; 3],
            count: 0,
            hist: [[f64::NAN; 3]; 16],
            idx: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.seed = [0.0; 3];
        self.prev = [f64::NAN; 3];
        self.count = 0;
        self.hist = [[f64::NAN; 3]; 16];
        self.idx = 0;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> [f64; 3] {
        let x = (high + low) / 2.0;
        if self.count >= ALLIGATOR_N[0] {
            for ((p, &k), &i) in self.prev.iter_mut().zip(&self.keep).zip(&self.inv) {
                *p = *p * k + x * i;
            }
        } else {
            self.warm(x);
        }
        let i = self.idx;
        self.idx = i.wrapping_add(1);
        self.hist[i & 15] = self.prev;
        [
            self.hist[i.wrapping_sub(ALLIGATOR_SHIFT[0]) & 15][0],
            self.hist[i.wrapping_sub(ALLIGATOR_SHIFT[1]) & 15][1],
            self.hist[i.wrapping_sub(ALLIGATOR_SHIFT[2]) & 15][2],
        ]
    }

    /// The first 13 values: the seeds.
    #[inline(never)]
    fn warm(&mut self, x: f64) {
        self.count += 1;
        for k in 0..3 {
            let n = ALLIGATOR_N[k];
            if self.count < n {
                self.seed[k] += x;
            } else if self.count == n {
                self.seed[k] += x;
                self.prev[k] = self.seed[k] / n as f64;
            } else {
                self.prev[k] = self.prev[k] * self.keep[k] + x * self.inv[k];
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// relative_vigor
// ------------------------------------------------------------------------------------------------

/// One sixth: the 4-bar symmetric weighted average is `(a + 2b + 2c + d) * SIXTH`.
const SIXTH: f64 = 1.0 / 6.0;

/// Relative Vigor Index: the sum over `n` bars of the 4-bar-smoothed close-minus-open over the sum of
/// the smoothed high-minus-low, and a 4-bar smoothing of that line over its DEFINED values.
///
/// The smoothed numerators and denominators exist from the fourth bar on; before it they are `NaN`,
/// and the exact window sums of [`PairSums`] keep a `NaN` out of the answer for exactly as long as it
/// is in the window, which is the old "first `n` smoothed values" rule with no counter.
#[derive(Clone)]
pub(crate) struct VigorCore {
    sums: PairSums,
    /// The three previous `close - open` and `high - low` values, oldest first.
    co: [f64; 3],
    hl: [f64; 3],
    /// The three previous defined `rvgi` values, oldest first.
    def: [f64; 3],
    /// A window of 0 (only reachable with a raw parameter) never answers.
    dead: bool,
}

impl VigorCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        VigorCore {
            sums: PairSums::new(n, false),
            co: [f64::NAN; 3],
            hl: [f64::NAN; 3],
            def: [f64::NAN; 3],
            dead: n == 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.sums.clear(false);
        self.co = [f64::NAN; 3];
        self.hl = [f64::NAN; 3];
        self.def = [f64::NAN; 3];
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, open: f64, high: f64, low: f64, close: f64) -> [f64; 2] {
        let c = close - open;
        let r = high - low;
        let num = (self.co[0] + 2.0 * self.co[1] + 2.0 * self.co[2] + c) * SIXTH;
        let den = (self.hl[0] + 2.0 * self.hl[1] + 2.0 * self.hl[2] + r) * SIXTH;
        self.co = [self.co[1], self.co[2], c];
        self.hl = [self.hl[1], self.hl[2], r];
        let [ns, ds] = self.sums.push(num, den);
        // `NaN` sums (a `NaN` in the window) make the quotient `NaN` too.
        let rvgi = if ds != 0.0 && !self.dead { ns / ds } else { f64::NAN };
        if rvgi.is_nan() {
            return [f64::NAN; 2];
        }
        let signal = (self.def[0] + 2.0 * self.def[1] + 2.0 * self.def[2] + rvgi) * SIXTH;
        self.def = [self.def[1], self.def[2], rvgi];
        [rvgi, signal]
    }
}

// ------------------------------------------------------------------------------------------------
// high_low_52w
// ------------------------------------------------------------------------------------------------

/// Rolling highest high and lowest low over `n` bars; `NaN` until `n` bars are in. A `NaN` price is
/// ignored (as `f64::max` ignores it); a window with no number in it answers `f64::MIN` / `f64::MAX`,
/// the old fold's initial values.
#[derive(Clone)]
pub(crate) struct HiLoCore {
    win: LapMax2,
    n: usize,
    warm: usize,
}

impl HiLoCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        HiLoCore { win: LapMax2::new(n), n, warm: Self::warm_for(n) }
    }

    /// A window of 0 never answers.
    #[inline]
    fn warm_for(n: usize) -> usize {
        if n == 0 { usize::MAX } else { n - 1 }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
        self.warm = Self::warm_for(self.n);
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> [f64; 2] {
        // The lowest low is the highest of the negated lows.
        let [hh, nl] = self.win.push(high, -low);
        if self.warm != 0 {
            self.warm -= 1;
            return [f64::NAN; 2];
        }
        [if hh > f64::MIN { hh } else { f64::MIN }, if -nl < f64::MAX { -nl } else { f64::MAX }]
    }
}

// ------------------------------------------------------------------------------------------------
// fisher
// ------------------------------------------------------------------------------------------------

/// Fisher Transform: the median price placed inside its `n`-bar range, smoothed, and run through an
/// inverse hyperbolic tangent; the trigger is the previous Fisher value. `NaN` until `n` bars are in
/// (the trigger one bar later). The arithmetic after the window extremes is the old kernel's, so the
/// answer is bit-for-bit the old one (the extremes are exact selections).
#[derive(Clone)]
pub(crate) struct FisherCore {
    win: LapMax2,
    n: usize,
    warm: usize,
    prev_value: f64,
    prev_fish: f64,
    started: bool,
}

impl FisherCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        FisherCore {
            win: LapMax2::new(n),
            n,
            warm: HiLoCore::warm_for(n),
            prev_value: 0.0,
            prev_fish: 0.0,
            started: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
        self.warm = HiLoCore::warm_for(self.n);
        self.prev_value = 0.0;
        self.prev_fish = 0.0;
        self.started = false;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> [f64; 2] {
        let median = (high + low) / 2.0;
        let [hi, nl] = self.win.push(median, -median);
        if self.warm != 0 {
            self.warm -= 1;
            return [f64::NAN; 2];
        }
        let hi = if hi > f64::MIN { hi } else { f64::MIN };
        let lo = if -nl < f64::MAX { -nl } else { f64::MAX };
        let rng = hi - lo;
        let norm = if rng == 0.0 { 0.0 } else { (median - lo) / rng };
        let mut value = 0.66 * (2.0 * norm - 1.0) + 0.67 * self.prev_value;
        value = value.clamp(-0.999, 0.999);
        let f = 0.5 * libm::log((1.0 + value) / (1.0 - value)) + 0.5 * self.prev_fish;
        let trigger = if self.started { self.prev_fish } else { f64::NAN };
        self.prev_value = value;
        self.prev_fish = f;
        self.started = true;
        [f, trigger]
    }
}

// ------------------------------------------------------------------------------------------------
// chande_kroll_stop
// ------------------------------------------------------------------------------------------------

/// Chande Kroll Stop: a Wilder ATR over `p` bars scaled by `x`, taken off the `p`-bar highest high and
/// put on the `p`-bar lowest low, then the highest of the first and the lowest of the second over the
/// last `q` bars. Both windows are lap maxima; the second ignores a `NaN` (the bars before the ATR
/// seeds) as the old kernel's "collect the defined values" did, and answers `NaN` when it holds none.
///
/// The ATR seeds with the mean of the true ranges of bars `1..=p` (bar 0's range is NOT in it), so it
/// lands at bar `p`; before that it is `NaN`, and so are the stops.
#[derive(Clone)]
pub(crate) struct ChandeKrollCore {
    hl: LapMax2,
    stops: LapMax2,
    p: usize,
    q: usize,
    xf: f64,
    pf: f64,
    inv: f64,
    keep: f64,
    prev_close: f64,
    seed: f64,
    atr: f64,
    /// True ranges fed to the seed so far.
    fed: usize,
    started: bool,
    /// Pushes still to go before the ATR is a pure recurrence (bar 0 and the seed bars).
    warm_p: usize,
    /// Pushes still to go before the stop window holds `q` values.
    warm_q: usize,
    /// A zero period (only reachable with a raw parameter) answers `NaN`, as the old kernel did.
    dead: bool,
}

impl ChandeKrollCore {
    #[inline]
    pub(crate) fn new(p: usize, x: f64, q: usize) -> Self {
        ChandeKrollCore {
            hl: LapMax2::new(p),
            stops: LapMax2::new(q),
            p,
            q,
            xf: x,
            pf: p as f64,
            inv: 1.0 / p as f64,
            keep: (p as f64 - 1.0) / p as f64,
            prev_close: f64::NAN,
            seed: 0.0,
            atr: f64::NAN,
            fed: 0,
            started: false,
            warm_p: p.saturating_add(1),
            warm_q: q.saturating_sub(1),
            dead: p == 0 || q == 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.hl.clear();
        self.stops.clear();
        self.prev_close = f64::NAN;
        self.seed = 0.0;
        self.atr = f64::NAN;
        self.fed = 0;
        self.started = false;
        self.warm_p = self.p.saturating_add(1);
        self.warm_q = self.q.saturating_sub(1);
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        if self.dead {
            return [f64::NAN; 2];
        }
        let [hh, nl] = self.hl.push(high, -low);
        if self.warm_p == 0 {
            let pc = self.prev_close;
            let tr = (high - low).max((high - pc).abs()).max((low - pc).abs());
            self.atr = self.atr * self.keep + tr * self.inv;
        } else {
            self.warm_atr(high, low);
        }
        self.prev_close = close;
        let xa = self.xf * self.atr;
        // `NaN` before the ATR exists: the stop window ignores it.
        let first_high = hh - xa;
        let first_low = (-nl) + xa;
        let [mh, nml] = self.stops.push(first_high, -first_low);
        if self.warm_q != 0 {
            self.warm_q -= 1;
            return [f64::NAN; 2];
        }
        let long = if mh > f64::NEG_INFINITY { mh } else { f64::NAN };
        let short = if nml > f64::NEG_INFINITY { -nml } else { f64::NAN };
        [long, short]
    }

    /// Bar 0 (no true range) and the seed bars.
    #[inline(never)]
    fn warm_atr(&mut self, high: f64, low: f64) {
        self.warm_p -= 1;
        if !self.started {
            self.started = true;
            return;
        }
        let pc = self.prev_close;
        let tr = (high - low).max((high - pc).abs()).max((low - pc).abs());
        self.fed += 1;
        self.seed += tr;
        if self.fed == self.p {
            self.atr = self.seed / self.pf;
        }
    }
}

// ------------------------------------------------------------------------------------------------
// connors_rsi
// ------------------------------------------------------------------------------------------------

/// Connors RSI: the mean of three parts, the Wilder RSI of the close over `rsi_p` moves, the Wilder RSI
/// over `streak_p` moves of the up/down streak length, and the percent rank of the bar's percentage
/// return among the last `rank_p` returns.
///
/// The two RSIs run as two lanes of one recurrence (`ag`, `al`), a lane being seeded by a few
/// warm-up pushes and then branch-free. The rank counts the values of the ring below the new return
/// with a plain compare-and-add loop the compiler turns into packed compares; a `NaN` return never
/// counts as below, and `nans` carries how many of the ring are not numbers.
#[derive(Clone)]
pub(crate) struct ConnorsCore {
    n: [usize; 2],
    nf: [f64; 2],
    inv: [f64; 2],
    keep: [f64; 2],
    ag: [f64; 2],
    al: [f64; 2],
    /// The previous close and the previous streak.
    prev: [f64; 2],
    /// Bar index while the RSIs are warming up.
    i: usize,
    warm: usize,
    ring: Vec<f64>,
    head: usize,
    nans: usize,
    rank_n: usize,
    rank_warm: usize,
}

impl ConnorsCore {
    #[inline]
    pub(crate) fn new(rsi_n: usize, streak_n: usize, rank_n: usize) -> Self {
        let n = [rsi_n, streak_n];
        let lane = |k: usize| if n[k] == 0 { f64::NAN } else { 0.0 };
        let len = rank_n.clamp(1, RING_CAP);
        ConnorsCore {
            n,
            nf: n.map(|v| v as f64),
            inv: n.map(|v| 1.0 / v as f64),
            keep: n.map(|v| (v as f64 - 1.0) / v as f64),
            ag: [lane(0), lane(1)],
            al: [lane(0), lane(1)],
            prev: [f64::NAN; 2],
            i: 0,
            warm: rsi_n.max(streak_n).saturating_add(1),
            ring: vec![f64::NAN; len],
            head: 0,
            nans: len,
            rank_n,
            rank_warm: if rank_n == 0 { usize::MAX } else { rank_n },
        }
    }

    pub(crate) fn clear(&mut self) {
        let lane = |k: usize| if self.n[k] == 0 { f64::NAN } else { 0.0 };
        self.ag = [lane(0), lane(1)];
        self.al = [lane(0), lane(1)];
        self.prev = [f64::NAN; 2];
        self.i = 0;
        self.warm = self.n[0].max(self.n[1]).saturating_add(1);
        self.ring.fill(f64::NAN);
        self.head = 0;
        self.nans = self.ring.len();
        self.rank_warm = if self.rank_n == 0 { usize::MAX } else { self.rank_n };
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        let pc = self.prev[0];
        let sp = self.prev[1];
        let d0 = c - pc;
        // The streak: a flat (or `NaN`) step resets it; bar 0 has no previous close, so `d0` is `NaN`.
        let streak = if d0 > 0.0 {
            sp.max(0.0) + 1.0
        } else if d0 < 0.0 {
            sp.min(0.0) - 1.0
        } else {
            0.0
        };
        let d1 = streak - sp;
        self.prev = [c, streak];
        // The 1-bar percentage return; undefined on bar 0 and after a zero close.
        let roc = if pc != 0.0 { (c / pc - 1.0) * 100.0 } else { f64::NAN };

        let [r1, r2] = if self.warm == 0 {
            // A `NaN` move counts as no move.
            let g0 = d0.max(0.0);
            let l0 = (-d0).max(0.0);
            let g1 = d1.max(0.0);
            let l1 = (-d1).max(0.0);
            self.ag = [
                self.ag[0] * self.keep[0] + g0 * self.inv[0],
                self.ag[1] * self.keep[1] + g1 * self.inv[1],
            ];
            self.al = [
                self.al[0] * self.keep[0] + l0 * self.inv[0],
                self.al[1] * self.keep[1] + l1 * self.inv[1],
            ];
            [RsiCore::rsi(self.ag[0], self.al[0]), RsiCore::rsi(self.ag[1], self.al[1])]
        } else {
            self.warm_rsi(d0, d1)
        };

        // The ring holds the last `rank_n` returns ending at this bar (the new one included).
        let old = std::mem::replace(&mut self.ring[self.head], roc);
        self.head += 1;
        if self.head == self.ring.len() {
            self.head = 0;
        }
        self.nans = self.nans + roc.is_nan() as usize - old.is_nan() as usize;
        let prank = if self.rank_warm != 0 {
            self.rank_warm = self.rank_warm.saturating_sub(1);
            f64::NAN
        } else if roc.is_nan() {
            f64::NAN
        } else {
            let mut below = 0u64;
            for &v in &self.ring {
                below += (v < roc) as u64;
            }
            100.0 * below as f64 / (self.ring.len() - self.nans) as f64
        };
        // A `NaN` in any part is a `NaN` in the sum.
        (r1 + r2 + prank) / 3.0
    }

    /// The first `max(rsi_p, streak_p) + 1` bars: the seeds of the two Wilder averages (each lane
    /// averages its first `n` moves with a division, then recurs).
    #[inline(never)]
    fn warm_rsi(&mut self, d0: f64, d1: f64) -> [f64; 2] {
        let i = self.i;
        self.i += 1;
        self.warm -= 1;
        let mut out = [f64::NAN; 2];
        if i == 0 {
            return out;
        }
        for (k, d) in [d0, d1].into_iter().enumerate() {
            let n = self.n[k];
            if n == 0 {
                continue;
            }
            let g = d.max(0.0);
            let l = (-d).max(0.0);
            if i < n {
                self.ag[k] += g;
                self.al[k] += l;
            } else if i == n {
                self.ag[k] = (self.ag[k] + g) / self.nf[k];
                self.al[k] = (self.al[k] + l) / self.nf[k];
                out[k] = RsiCore::rsi(self.ag[k], self.al[k]);
            } else {
                self.ag[k] = self.ag[k] * self.keep[k] + g * self.inv[k];
                self.al[k] = self.al[k] * self.keep[k] + l * self.inv[k];
                out[k] = RsiCore::rsi(self.ag[k], self.al[k]);
            }
        }
        out
    }
}

// ------------------------------------------------------------------------------------------------
// the population standard deviation of a window: hvol, relative_volatility
// ------------------------------------------------------------------------------------------------

/// A variance below this fraction of the mean square is below what the running sums can resolve; it is
/// decided from the window itself instead (see [`VarCore`]).
const VAR_FLOOR: f64 = 1.0e-3;

/// The population standard deviation of the last `n` values, from running sums of the window SHIFTED
/// by its oldest value at the last rebuild (so a price near a million with moves of a hundredth keeps
/// its digits), rebuilt from the ring every `ceil(512 / n)` laps and on the spot when the result is not
/// a number.
///
/// Warm-up is the `NaN` multiplier of [`crate::roll::SmaCore`]: `NaN` until the first lap of the ring
/// ends. A variance that the sums cannot resolve (negative, `NaN`, or under [`VAR_FLOOR`] of the mean
/// square: a flat window above all) is not trusted and not clamped; the cold path rebuilds the sums and,
/// if that does not resolve it, folds the window the long way, the old kernel's two passes, whose
/// constant-window law gives EXACTLY zero for a window of bit-identical values.
#[derive(Clone)]
pub(crate) struct VarCore {
    inv: f64,
    inv_n: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    s1: f64,
    s2: f64,
}

impl VarCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        VarCore {
            inv: f64::NAN,
            // A window of 0 never leaves warm-up: every output is NaN.
            inv_n: if n == 0 { f64::NAN } else { 1.0 / n as f64 },
            buf: vec![0.0; n.clamp(1, RING_CAP)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
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

    /// Push `x`; the population standard deviation of the last `n` values, `NaN` until `n` are in.
    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        self.s1 += d - o;
        self.s2 += d * d - o * o;
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0 on raw values.
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let (shift, s1, s2) = shifted_sums(&self.buf, 0);
                self.shift = shift;
                self.s1 = s1;
                self.s2 = s2;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let m = self.s1 * self.inv;
        let ms = self.s2 * self.inv;
        let var = ms - m * m;
        let resolved = var > VAR_FLOOR * ms;
        if !resolved {
            // Warming up, a `NaN` in the window, a stale sum, or a window flat to the rounding of the
            // sums. Cold, and over a shared slice: a `&mut self` method call here would keep the sums
            // in memory across every iteration of a caller's loop.
            if self.inv.is_nan() {
                return f64::NAN;
            }
            let (shift, s1, s2, sd) = var_cold(&self.buf, self.head, self.inv);
            self.shift = shift;
            self.s1 = s1;
            self.s2 = s2;
            return sd;
        }
        var.sqrt()
    }
}

/// Rebuild the shifted sums from the window (oldest value at `head`) and decide its standard
/// deviation from them, or from the window itself when they cannot resolve it.
#[cold]
#[inline(never)]
fn var_cold(buf: &[f64], head: usize, inv: f64) -> (f64, f64, f64, f64) {
    let (shift, s1, s2) = shifted_sums(buf, head);
    let m = s1 * inv;
    let ms = s2 * inv;
    let var = ms - m * m;
    if var.is_nan() {
        return (shift, s1, s2, f64::NAN);
    }
    if var > VAR_FLOOR * ms {
        return (shift, s1, s2, var.sqrt());
    }
    (shift, s1, s2, exact_sd(buf, head))
}

/// The population standard deviation of the window folded the long way, oldest value first: `0` for a
/// window of equal numbers, else the mean and then `sqrt(sum((x - mean)^2) / n)`.
fn exact_sd(buf: &[f64], head: usize) -> f64 {
    let first = buf[head];
    if !first.is_nan() && buf.iter().all(|&v| v == first) {
        return 0.0;
    }
    let n = buf.len() as f64;
    let mut sum = 0.0_f64;
    for &v in buf[head..].iter().chain(&buf[..head]) {
        sum += v;
    }
    let mean = sum / n;
    let mut ss = 0.0_f64;
    for &v in buf[head..].iter().chain(&buf[..head]) {
        let dev = v - mean;
        ss += dev * dev;
    }
    (ss / n).sqrt()
}

// ------------------------------------------------------------------------------------------------
// hvol
// ------------------------------------------------------------------------------------------------

/// Annualized historical volatility in percent: the population standard deviation of the last `n`
/// DEFINED log returns of the close, times the square root of `ann` and 100. A bar whose close or
/// previous close is not positive has no log return and is skipped, not counted.
#[derive(Clone)]
pub(crate) struct HvolCore {
    var: VarCore,
    scale: f64,
    prev: f64,
    dead: bool,
}

impl HvolCore {
    #[inline]
    pub(crate) fn new(n: usize, ann: usize) -> Self {
        HvolCore { var: VarCore::new(n), scale: (ann as f64).sqrt(), prev: f64::NAN, dead: n == 0 }
    }

    pub(crate) fn clear(&mut self) {
        self.var.clear();
        self.prev = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        let pc = self.prev;
        self.prev = close;
        let lr = if close > 0.0 && pc > 0.0 { libm::log(close / pc) } else { f64::NAN };
        if lr.is_nan() || self.dead {
            return f64::NAN;
        }
        self.var.push(lr) * self.scale * 100.0
    }
}

// ------------------------------------------------------------------------------------------------
// relative_volatility
// ------------------------------------------------------------------------------------------------

/// Two EMAs fed in lock step, each seeded with the mean of its first `n` values; `NaN` until then.
#[derive(Clone)]
struct Ema2 {
    n: usize,
    alpha: f64,
    keep: f64,
    fed: usize,
    seed: [f64; 2],
    prev: [f64; 2],
}

impl Ema2 {
    #[inline]
    fn new(n: usize) -> Self {
        let alpha = 2.0 / (n as f64 + 1.0);
        Ema2 { n, alpha, keep: 1.0 - alpha, fed: 0, seed: [0.0; 2], prev: [f64::NAN; 2] }
    }

    fn clear(&mut self) {
        self.fed = 0;
        self.seed = [0.0; 2];
        self.prev = [f64::NAN; 2];
    }

    #[inline(always)]
    fn push(&mut self, a: f64, b: f64) -> [f64; 2] {
        if self.fed >= self.n {
            self.prev = [
                self.alpha * a + self.keep * self.prev[0],
                self.alpha * b + self.keep * self.prev[1],
            ];
            return self.prev;
        }
        self.fed += 1;
        self.seed = [self.seed[0] + a, self.seed[1] + b];
        if self.fed == self.n {
            self.prev = [self.seed[0] / self.n as f64, self.seed[1] / self.n as f64];
            return self.prev;
        }
        [f64::NAN; 2]
    }
}

/// Relative Volatility Index: the rolling population standard deviation of the close, split by the
/// direction of the close (up bars feed the up side, down bars the down side, a flat or `NaN` bar
/// neither), each side EMA-smoothed over `n`, and the up side as a percentage of the two (`50` when
/// both are zero). The EMAs are fed only on bars whose window holds a number, so a `NaN` close leaves a
/// gap, not a poisoned average.
#[derive(Clone)]
pub(crate) struct RelVolCore {
    var: VarCore,
    ema: Ema2,
    prev: f64,
    started: bool,
}

impl RelVolCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        RelVolCore { var: VarCore::new(n), ema: Ema2::new(n), prev: f64::NAN, started: false }
    }

    pub(crate) fn clear(&mut self) {
        self.var.clear();
        self.ema.clear();
        self.prev = f64::NAN;
        self.started = false;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, close: f64) -> f64 {
        let sd = self.var.push(close);
        let pc = self.prev;
        self.prev = close;
        let had_prev = std::mem::replace(&mut self.started, true);
        if sd.is_nan() || !had_prev {
            return f64::NAN;
        }
        let up = if close > pc { sd } else { 0.0 };
        let down = if close < pc { sd } else { 0.0 };
        let [eu, ed] = self.ema.push(up, down);
        let denom = eu + ed;
        if denom != 0.0 { 100.0 * eu / denom } else { 50.0 }
    }
}

// ------------------------------------------------------------------------------------------------
// median_ma
// ------------------------------------------------------------------------------------------------

/// The rolling median of the last `n` values (the mean of the two middle ones when `n` is even);
/// `NaN` until `n` are in and while a `NaN` is among them.
///
/// The window is kept SORTED in a plain array and moves one element a bar: two binary searches and one
/// `copy_within` over the elements between the leaving value and the arriving one. A `NaN` never enters
/// the array; `nans` counts the ones in the window.
#[derive(Clone)]
pub(crate) struct MedianCore {
    ring: Vec<f64>,
    sorted: Vec<f64>,
    head: usize,
    n: usize,
    nans: usize,
    /// Values still to come before the window is full.
    warm: usize,
}

impl MedianCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        MedianCore {
            ring: vec![f64::NAN; n.clamp(1, RING_CAP)],
            sorted: Vec::with_capacity(n.clamp(1, RING_CAP)),
            head: 0,
            n,
            nans: 0,
            warm: if n == 0 { usize::MAX } else { n },
        }
    }

    pub(crate) fn clear(&mut self) {
        self.ring.fill(f64::NAN);
        self.sorted.clear();
        self.head = 0;
        self.nans = 0;
        self.warm = if self.n == 0 { usize::MAX } else { self.n };
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.ring[self.head], x);
        self.head += 1;
        if self.head == self.ring.len() {
            self.head = 0;
        }
        if self.warm != 0 {
            self.warm -= 1;
            self.fill(x);
        } else if !(old.is_nan() | x.is_nan()) {
            replace_sorted(&mut self.sorted, old, x);
        } else {
            self.swap_with_nan(old, x);
        }
        if self.warm != 0 || self.nans != 0 {
            return f64::NAN;
        }
        let s = &self.sorted;
        let mid = self.n / 2;
        if self.n % 2 == 1 { s[mid] } else { (s[mid - 1] + s[mid]) / 2.0 }
    }

    /// The first `n` values: sorted insertion.
    #[inline(never)]
    fn fill(&mut self, x: f64) {
        // A window of 0 never answers, and never stores.
        if self.n == 0 {
            return;
        }
        if x.is_nan() {
            self.nans += 1;
        } else {
            let at = self.sorted.partition_point(|&v| v < x);
            self.sorted.insert(at, x);
        }
    }

    /// A `NaN` is leaving or arriving.
    #[cold]
    #[inline(never)]
    fn swap_with_nan(&mut self, old: f64, x: f64) {
        if old.is_nan() {
            self.nans -= 1;
        } else {
            let at = self.sorted.partition_point(|&v| v < old);
            self.sorted.remove(at);
        }
        self.fill(x);
    }
}

/// Take `old` out of the sorted window and put `x` in, moving only the elements between them.
#[inline(always)]
fn replace_sorted(s: &mut [f64], old: f64, x: f64) {
    let i = s.partition_point(|&v| v < old);
    let j = s.partition_point(|&v| v < x);
    if j > i {
        s.copy_within(i + 1..j, i);
        s[j - 1] = x;
    } else {
        s.copy_within(j..i, j + 1);
        s[j] = x;
    }
}

// ------------------------------------------------------------------------------------------------
// linreg_channel
// ------------------------------------------------------------------------------------------------

/// Under this fraction of the sum of squares the residual of the fit is below what the running sums
/// resolve: the answer is decided from the window itself.
const FIT_FLOOR: f64 = 1.0e-5;

/// The least-squares line through the last `n` values against `0..n`, with bands `mult` residual
/// standard deviations either side: `[upper, mid, lower]`, `mid` the line at the newest bar.
///
/// Running sums of the window SHIFTED by its oldest value at the last rebuild (so a price near a
/// million with moves of a hundredth keeps its digits): `sum(y)`, `sum(y^2)` and `sum(i * y)` with `i`
/// the place in the window, the last updated in O(1) as the window slides
/// (`sxy' = sxy - (sy - y_old) + (n - 1) * y_new`). The slope and intercept are closed forms of
/// them, and the residual sum of squares is `syy - sy^2 / n - slope * (sxy - sx * sy / n)`. That is a
/// difference of nearly equal numbers for a window that is almost a straight line, so a residual under
/// [`FIT_FLOOR`] of `syy` (and a `NaN`, and a flat window) goes to the cold path, which rebuilds the
/// sums and, if that does not resolve it, folds the residuals of the window the long way; a window of
/// bit-identical values is its own line with a spread of EXACTLY zero.
///
/// Warm-up is a `NaN` `1 / denominator` until the first lap of the ring ends.
#[derive(Clone)]
pub(crate) struct LinregCore {
    nf: f64,
    n1: f64,
    sx: f64,
    inv_n: f64,
    inv_denom: f64,
    inv_denom_n: f64,
    mult: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    sy: f64,
    sxy: f64,
    syy: f64,
    /// A window under two values (only reachable with a raw parameter) never answers.
    dead: bool,
}

impl LinregCore {
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        let nf = n as f64;
        let n2 = nf * nf;
        LinregCore {
            nf,
            n1: nf - 1.0,
            sx: nf * (nf - 1.0) / 2.0,
            inv_n: 1.0 / nf,
            inv_denom: f64::NAN,
            inv_denom_n: 1.0 / (n2 * (n2 - 1.0) / 12.0),
            mult,
            buf: vec![0.0; n.clamp(1, RING_CAP)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shift: 0.0,
            sy: 0.0,
            sxy: 0.0,
            syy: 0.0,
            dead: n < 2,
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
        self.inv_denom = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> [f64; 3] {
        if self.dead {
            return [f64::NAN; 3];
        }
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        self.sxy += self.n1 * d - (self.sy - o);
        self.sy += d - o;
        self.syy += d * d - o * o;
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
                self.laps = 0;
            }
            self.inv_denom = self.inv_denom_n;
        }
        let slope = (self.nf * self.sxy - self.sx * self.sy) * self.inv_denom;
        let icpt = (self.sy - slope * self.sx) * self.inv_n;
        let cyy = self.syy - self.sy * self.sy * self.inv_n;
        let cxy = self.sxy - self.sx * self.sy * self.inv_n;
        let ssr = cyy - slope * cxy;
        let resolved = ssr > FIT_FLOOR * self.syy;
        if !resolved {
            // Warming up, a `NaN` in the window, a stale sum, or a window that is (nearly) a line.
            if self.inv_denom.is_nan() {
                return [f64::NAN; 3];
            }
            let (f, out) =
                fit_cold(&self.buf, self.head, self.nf, self.sx, self.inv_denom, self.mult);
            self.shift = f.shift;
            self.sy = f.sy;
            self.sxy = f.sxy;
            self.syy = f.syy;
            return out;
        }
        let sd = (ssr * self.inv_n).sqrt();
        let mid = self.shift + (icpt + slope * self.n1);
        [mid + self.mult * sd, mid, mid - self.mult * sd]
    }
}

/// The shifted sums of a window of `n` values (oldest at `head`).
struct Fit {
    shift: f64,
    sy: f64,
    sxy: f64,
    syy: f64,
}

impl Fit {
    /// The window as `(shift, sum(d), sum(i * d), sum(d^2))` with `d = y - shift`, `shift` being the
    /// oldest value when it is finite and `i` the place in the window from the oldest.
    #[cold]
    #[inline(never)]
    fn of(buf: &[f64], head: usize) -> Fit {
        let shift = if buf[head].is_finite() { buf[head] } else { 0.0 };
        let (mut sy, mut sxy, mut syy) = (0.0_f64, 0.0_f64, 0.0_f64);
        for (i, &v) in buf[head..].iter().chain(&buf[..head]).enumerate() {
            let d = v - shift;
            sy += d;
            sxy += i as f64 * d;
            syy += d * d;
        }
        Fit { shift, sy, sxy, syy }
    }
}

/// Rebuild the sums from the window (oldest at `head`) and decide the channel from them, or from the
/// residuals of the window itself when the sums cannot resolve it.
#[cold]
#[inline(never)]
fn fit_cold(
    buf: &[f64],
    head: usize,
    nf: f64,
    sx: f64,
    inv_denom: f64,
    mult: f64,
) -> (Fit, [f64; 3]) {
    let f = Fit::of(buf, head);
    let inv_n = 1.0 / nf;
    let slope = (nf * f.sxy - sx * f.sy) * inv_denom;
    let icpt = (f.sy - slope * sx) * inv_n;
    let ssr = (f.syy - f.sy * f.sy * inv_n) - slope * (f.sxy - sx * f.sy * inv_n);
    if slope.is_nan() || ssr.is_nan() {
        return (f, [f64::NAN; 3]);
    }
    let first = buf[head];
    let ordered = || buf[head..].iter().chain(&buf[..head]);
    let (mid, sd) = if ordered().all(|&v| v == first) {
        // A window of bit-identical values is its own line.
        (first, 0.0)
    } else {
        let sd = if ssr > FIT_FLOOR * f.syy {
            (ssr * inv_n).sqrt()
        } else {
            // The residuals of the window, folded the long way: the mean of them, then their spread.
            let (mut sum, mut same) = (0.0_f64, true);
            let mut r0 = 0.0_f64;
            for (i, &v) in ordered().enumerate() {
                let r = (v - f.shift) - (icpt + slope * i as f64);
                if i == 0 {
                    r0 = r;
                }
                same &= r == r0;
                sum += r;
            }
            if same {
                0.0
            } else {
                let mean = sum * inv_n;
                let mut ss = 0.0_f64;
                for (i, &v) in ordered().enumerate() {
                    let dev = ((v - f.shift) - (icpt + slope * i as f64)) - mean;
                    ss += dev * dev;
                }
                (ss * inv_n).sqrt()
            }
        };
        (f.shift + (icpt + slope * (nf - 1.0)), sd)
    };
    (f, [mid + mult * sd, mid, mid - mult * sd])
}

// ------------------------------------------------------------------------------------------------
// mcginley
// ------------------------------------------------------------------------------------------------

/// McGinley Dynamic: a moving average that speeds up and slows down with how far price has moved from
/// it, `prev += (c - prev) / (n * (c / prev)^4)`, starting at the first close.
///
/// The step is written `prev + (c - prev) * (1 / (n * c^4)) * prev^4`: the same number, but the one
/// division depends on the input alone, so it runs beside the recurrence instead of inside it, and the
/// chain through `prev` is two squarings, a multiply and an add. The old form (`c / prev` first) is the
/// cold path, taken when `c` or the step is outside the range where `c^4` and `prev^4` are safe numbers
/// (a price outside 1e-70..1e70, a zero, a `NaN`, an infinity).
#[derive(Clone)]
pub(crate) struct McginleyCore {
    pf: f64,
    prev: f64,
    fresh: bool,
}

impl McginleyCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        McginleyCore { pf: n as f64, prev: f64::NAN, fresh: true }
    }

    pub(crate) fn clear(&mut self) {
        self.prev = f64::NAN;
        self.fresh = true;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        let prev = self.prev;
        let a = c.abs();
        if !self.fresh && a > 1.0e-70 && a < 1.0e70 && prev != 0.0 {
            let c2 = c * c;
            let w = 1.0 / (self.pf * (c2 * c2));
            let p2 = prev * prev;
            let next = prev + ((c - prev) * w) * (p2 * p2);
            if next.is_finite() {
                self.prev = next;
                return next;
            }
        }
        let next = self.cold(c);
        self.prev = next;
        next
    }

    /// The first value, a restart after a zero, and every case the fast form does not cover: the old
    /// form, expression for expression.
    #[cold]
    #[inline(never)]
    fn cold(&mut self, c: f64) -> f64 {
        if self.fresh {
            self.fresh = false;
            return c;
        }
        let mut prev = self.prev;
        if prev == 0.0 {
            prev = c;
        } else {
            let ratio = c / prev;
            prev += (c - prev) / (self.pf * crate::math::quart(ratio));
        }
        prev
    }
}

// ------------------------------------------------------------------------------------------------
// asi
// ------------------------------------------------------------------------------------------------

/// Accumulative swing index: a running sum of Wilder's swing index, which reads the previous bar's open
/// and close. `NaN` on the first bar. A `NaN` price poisons the sum from then on, as it did before,
/// except that a `NaN` HIGH or LOW only drops out of the range's `max` (the old `f64::max`).
#[derive(Clone)]
pub(crate) struct AsiCore {
    /// `50 / limit`.
    scale: f64,
    /// A limit of 0 makes every swing index 0.
    flat: bool,
    prev_c: f64,
    prev_o: f64,
    cum: f64,
    fresh: bool,
}

impl AsiCore {
    #[inline]
    pub(crate) fn new(limit: f64) -> Self {
        AsiCore {
            scale: 50.0 / limit,
            flat: limit == 0.0,
            prev_c: f64::NAN,
            prev_o: f64::NAN,
            cum: 0.0,
            fresh: true,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.prev_c = f64::NAN;
        self.prev_o = f64::NAN;
        self.cum = 0.0;
        self.fresh = true;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
        let (cp, op) = (self.prev_c, self.prev_o);
        self.prev_c = close;
        self.prev_o = open;
        if self.fresh {
            self.fresh = false;
            return f64::NAN;
        }
        let a = (high - cp).abs();
        let b = (low - cp).abs();
        let range = high - low;
        let r = a.max(b).max(range);
        let k = a.max(b);
        let t = r + 0.25 * (cp - op).abs();
        let numerator = (close - cp) + 0.5 * (close - open) + 0.25 * (cp - op);
        // The division runs whatever `r` is (`0 / 0` is thrown away): no branch, and no part of the
        // running sum waits on it.
        let swing = (numerator / t) * (k * self.scale);
        let si = if r == 0.0 || self.flat { 0.0 } else { swing };
        self.cum += si;
        self.cum
    }
}
