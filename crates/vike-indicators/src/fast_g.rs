//! Fast cores for group `g` of the third wave of indicators (see `_followups/perf-conv3-brief.md`):
//! `dx`, `plus_di`, `minus_di`, `plus_dm`, `minus_dm`, `chaikin_volatility`, `chandelier`, `starc`,
//! `smma`, `supertrend`, `adxr` and `natr`.
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds no
//! `&mut self` call that could write the struct in its hot part, and is used by BOTH the streaming
//! stepper and the whole-series functions, so a stream and a batch agree bit for bit.
//!
//! The recurrences multiply by a precomputed `1/n` or `(n-1)/n` instead of dividing, so the last bits
//! differ from the textbook division form by the rounding noise any such rewrite has
//! (`tests/fast_g.rs` holds them to a relative 1e-9). Warm-up costs nothing in the steady state: a
//! counter compared with a precomputed end is the only test, and the seed ladders sit behind it.

use crate::fast_a::HiLo;
use crate::fast_c::AdxCore;
use crate::roll::{RING_CAP, SmaCore};

/// A period read from a raw parameter: rounded, and capped so a degenerate value (an infinity reads
/// as `usize::MAX`) cannot ask the allocator for the address space. A `NaN` or negative reads as 0.
#[inline]
pub(crate) fn period(v: f64) -> usize {
    v.round().min(RING_CAP as f64) as usize
}

// ------------------------------------------------------------------------------------------------
// Wilder / EMA smoothing
// ------------------------------------------------------------------------------------------------

/// A recurrence `prev = prev * keep + x * gain` seeded with the plain mean of its first `n` values:
/// Wilder's smoothing (`gain = 1/n`, `keep = (n-1)/n`) or an EMA (`gain = 2/(n+1)`, `keep = 1-gain`).
/// `NaN` until the seed is complete.
///
/// `SKIP` ignores the very first value (a true range needs a previous close, so ATRs that start at
/// bar 1 seed on values `1..=n`); the seed is then complete at value `n`, otherwise at value `n - 1`.
/// A seed starts at `-0.0`, as a left fold of `Iterator::sum` does. A period of 0 never emits.
#[derive(Clone)]
pub(crate) struct SmoothCore<const SKIP: bool> {
    gain: f64,
    keep: f64,
    inv: f64,
    /// Values seen on the slow path; the steady state starts at `end` and leaves it alone.
    i: usize,
    end: usize,
    /// The index of the value that completes the seed.
    last: usize,
    seed: f64,
    prev: f64,
}

impl<const SKIP: bool> SmoothCore<SKIP> {
    #[inline]
    fn build(n: usize, gain: f64, keep: f64) -> Self {
        let skip = usize::from(SKIP);
        SmoothCore {
            gain,
            keep,
            // A period of 0 seeds with `NaN`, so it can never emit a number.
            inv: if n == 0 { f64::NAN } else { 1.0 / n as f64 },
            i: 0,
            end: if n == 0 { usize::MAX } else { n + skip },
            last: (n + skip).wrapping_sub(1),
            seed: -0.0,
            prev: f64::NAN,
        }
    }

    /// Wilder's smoothing over `n` values.
    #[inline]
    pub(crate) fn wilder(n: usize) -> Self {
        Self::build(n, 1.0 / n as f64, (n as f64 - 1.0) / n as f64)
    }

    /// An exponential moving average over `n` values.
    #[inline]
    pub(crate) fn ema(n: usize) -> Self {
        let alpha = 2.0 / (n as f64 + 1.0);
        Self::build(n, alpha, 1.0 - alpha)
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        if self.i >= self.end {
            self.prev = self.prev * self.keep + x * self.gain;
            return self.prev;
        }
        let i = self.i;
        self.i = i + 1;
        if SKIP && i == 0 {
            return f64::NAN;
        }
        self.seed += x;
        if i < self.last {
            return f64::NAN;
        }
        self.prev = self.seed * self.inv;
        self.prev
    }
}

/// The true range of a bar against the previous close; a `NaN` previous close (the first bar) leaves
/// `high - low`, because `f64::max` hands back the number.
#[inline(always)]
fn true_range(h: f64, l: f64, pc: f64) -> f64 {
    (h - l).max((h - pc).abs()).max((l - pc).abs())
}

/// Wilder's average true range. With `SKIP` the first bar has no range (the ATR starts at bar `n`,
/// seeded on the ranges of bars `1..=n`); without it the first range is `high - low` (the ATR starts
/// at bar `n - 1`).
#[derive(Clone)]
pub(crate) struct AtrG<const SKIP: bool> {
    rma: SmoothCore<SKIP>,
    /// The previous close, `NaN` before the first bar.
    pc: f64,
}

impl<const SKIP: bool> AtrG<SKIP> {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        AtrG { rma: SmoothCore::wilder(n), pc: f64::NAN }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let tr = true_range(h, l, self.pc);
        self.pc = c;
        self.rma.push(tr)
    }
}

// ------------------------------------------------------------------------------------------------
// SMMA
// ------------------------------------------------------------------------------------------------

/// Wilder's smoothed moving average of a series: `NaN` for the first `n - 1` values.
#[derive(Clone)]
pub(crate) struct SmmaCore {
    rma: SmoothCore<false>,
}

impl SmmaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        SmmaCore { rma: SmoothCore::wilder(n) }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        self.rma.push(x)
    }
}

// ------------------------------------------------------------------------------------------------
// The directional movement family
// ------------------------------------------------------------------------------------------------

/// Which line of the directional movement system a [`DmCore`] answers.
pub(crate) const DX: u8 = 0;
pub(crate) const PLUS_DI: u8 = 1;
pub(crate) const MINUS_DI: u8 = 2;
pub(crate) const PLUS_DM: u8 = 3;
pub(crate) const MINUS_DM: u8 = 4;

/// Wilder's directional movement, one line of it: the smoothed sums of true range, +DM and -DM
/// (bars `1..=n` seed them as plain sums, later bars decay them, `s = s * (n-1)/n + x`), and the line
/// `K` picked from them.
///
/// Only the sums the line needs are carried, so `plus_dm` runs one recurrence and `dx` three.
/// `NaN` for the first `n` bars. `dx` is `100 * |+DM - -DM| / (+DM + -DM)`, the same number as
/// `100 * |+DI - -DI| / (+DI + -DI)` with the shared true-range factor cancelled, and `0` while the
/// range is not positive or both sums are zero.
///
/// The first `n + 1` bars take the slow path; after that `push` is the steady state.
#[derive(Clone)]
pub(crate) struct DmCore<const K: u8> {
    n: usize,
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
}

impl<const K: u8> DmCore<K> {
    /// The true-range sum is needed by `dx` and by both DI lines.
    const TR: bool = K == DX || K == PLUS_DI || K == MINUS_DI;
    /// The +DM sum is needed by `dx`, `plus_di` and `plus_dm`.
    const P: bool = K == DX || K == PLUS_DI || K == PLUS_DM;
    /// The -DM sum is needed by `dx`, `minus_di` and `minus_dm`.
    const M: bool = K == DX || K == MINUS_DI || K == MINUS_DM;

    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        DmCore {
            n,
            keep: (n as f64 - 1.0) / n as f64,
            i: 0,
            // A period of 0 never leaves the slow path, which answers NaN.
            end: if n == 0 { usize::MAX } else { n + 1 },
            ph: f64::NAN,
            pl: f64::NAN,
            pc: f64::NAN,
            atr_s: -0.0,
            pdm_s: -0.0,
            mdm_s: -0.0,
        }
    }

    /// The line from the three sums.
    #[inline(always)]
    fn line(atr_s: f64, p: f64, m: f64) -> f64 {
        match K {
            DX => {
                let s = p + m;
                let d = 100.0 * (p - m).abs() / s;
                if atr_s > 0.0 && s > 0.0 { d } else { 0.0 }
            }
            PLUS_DI => {
                let v = p * (100.0 / atr_s);
                if atr_s > 0.0 { v } else { 0.0 }
            }
            MINUS_DI => {
                let v = m * (100.0 / atr_s);
                if atr_s > 0.0 { v } else { 0.0 }
            }
            PLUS_DM => p,
            _ => m,
        }
    }

    /// One bar. The close is read only by the lines that use the true range.
    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let up = h - self.ph;
        let dn = self.pl - l;
        // `up > dn && up > 0` as masked selects: which of the two moves is larger is a coin toss, and
        // a short-circuit `&&` would be a branch.
        let upp = if up > 0.0 { up } else { 0.0 };
        let dnp = if dn > 0.0 { dn } else { 0.0 };
        let pdm = if up > dn { upp } else { 0.0 };
        let mdm = if dn > up { dnp } else { 0.0 };
        let tr = if Self::TR { true_range(h, l, self.pc) } else { 0.0 };
        self.ph = h;
        self.pl = l;
        self.pc = c;
        if self.i >= self.end {
            if Self::TR {
                self.atr_s = self.atr_s * self.keep + tr;
            }
            if Self::P {
                self.pdm_s = self.pdm_s * self.keep + pdm;
            }
            if Self::M {
                self.mdm_s = self.mdm_s * self.keep + mdm;
            }
            return Self::line(self.atr_s, self.pdm_s, self.mdm_s);
        }
        let i = self.i;
        self.i = i + 1;
        // Bar 0 has no previous bar to difference; a period of 0 has no line.
        if self.n == 0 || i == 0 {
            return f64::NAN;
        }
        // Bars 1..=n are summed plainly: the seed is the sum of bars 1..=n, this bar included.
        if Self::TR {
            self.atr_s += tr;
        }
        if Self::P {
            self.pdm_s += pdm;
        }
        if Self::M {
            self.mdm_s += mdm;
        }
        if i < self.n {
            return f64::NAN;
        }
        Self::line(self.atr_s, self.pdm_s, self.mdm_s)
    }
}

// ------------------------------------------------------------------------------------------------
// Chaikin volatility
// ------------------------------------------------------------------------------------------------

/// Chaikin volatility: the percentage change over `roc` bars of the EMA (over `ema` bars) of the
/// high-low range. `NaN` until the EMA and the value it is compared with exist, and where that
/// earlier EMA is exactly zero.
///
/// The ring of smoothed ranges starts as `NaN`, so the first `roc` values come out `NaN` by arithmetic.
/// A lag of 0 is read as 1 (the registry range starts at 1).
#[derive(Clone)]
pub(crate) struct ChaikinCore {
    ema: SmoothCore<false>,
    buf: Vec<f64>,
    head: usize,
}

impl ChaikinCore {
    #[inline]
    pub(crate) fn new(ema: usize, roc: usize) -> Self {
        ChaikinCore {
            ema: SmoothCore::ema(ema),
            buf: vec![f64::NAN; roc.clamp(1, RING_CAP)],
            head: 0,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64) -> f64 {
        let smooth = self.ema.push(h - l);
        let old = std::mem::replace(&mut self.buf[self.head], smooth);
        self.head += 1;
        if self.head == self.buf.len() {
            self.head = 0;
        }
        // The division runs for every value and the zero test only selects (a `NaN` on either side
        // is a `NaN` result by itself).
        let r = (smooth - old) / old * 100.0;
        if old != 0.0 { r } else { f64::NAN }
    }
}

// ------------------------------------------------------------------------------------------------
// Chandelier exit, STARC bands
// ------------------------------------------------------------------------------------------------

/// Chandelier exit: `[highest high - mult * ATR, lowest low + mult * ATR]` over the last `n` bars,
/// the ATR Wilder's with the first range `high - low`. `NaN` for the first `n - 1` bars.
#[derive(Clone)]
pub(crate) struct ChandelierCore {
    win: HiLo,
    atr: AtrG<false>,
    mult: f64,
}

impl ChandelierCore {
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        ChandelierCore { win: HiLo::new(n), atr: AtrG::new(n), mult }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 2] {
        let [hh, ll] = self.win.push(h, l);
        // A window with no number in it has no extreme: `HiLo` answers its empty identities, which are
        // not a price. `NaN`, the crate's "no value", is what comes out.
        let hh = if hh == f64::NEG_INFINITY { f64::NAN } else { hh };
        let ll = if ll == f64::INFINITY { f64::NAN } else { ll };
        let m = self.mult * self.atr.push(h, l, c);
        [hh - m, ll + m]
    }
}

/// STARC bands: the mean of the last `sma` closes and `mult` ATRs either side, `[upper, mid, lower]`;
/// all `NaN` until both the mean and the ATR exist.
#[derive(Clone)]
pub(crate) struct StarcCore {
    mean: SmaCore,
    atr: AtrG<false>,
    mult: f64,
}

impl StarcCore {
    #[inline]
    pub(crate) fn new(sma: usize, atr: usize, mult: f64) -> Self {
        StarcCore { mean: SmaCore::new(sma), atr: AtrG::new(atr), mult }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 3] {
        let mid = self.mean.push(c);
        let m = self.mult * self.atr.push(h, l, c);
        let upper = mid + m;
        // A `NaN` in either the mean or the ATR takes the whole triple with it.
        if upper.is_nan() { [f64::NAN; 3] } else { [upper, mid, mid - m] }
    }
}

// ------------------------------------------------------------------------------------------------
// Supertrend
// ------------------------------------------------------------------------------------------------

/// Supertrend: ATR bands that only tighten while the trend holds. `[supertrend, direction]` with the
/// direction `1.0` up and `-1.0` down, both `NaN` until the ATR exists (bar `n`).
///
/// The first bar with an ATR seeds both bands from its own midpoint and the direction from the close
/// against it; after that the upper band is carried down (and the lower one up) while the previous
/// close stayed on its side of it, and the direction flips when the close crosses the opposite band.
/// The carry rules are selects, so the loop-carried part is a compare and a select. A bar whose ATR is
/// `NaN` leaves the state untouched.
#[derive(Clone)]
pub(crate) struct SupertrendCore {
    atr: AtrG<true>,
    mult: f64,
    started: bool,
    up: bool,
    fu: f64,
    fl: f64,
}

impl SupertrendCore {
    #[inline]
    pub(crate) fn new(n: usize, mult: f64) -> Self {
        SupertrendCore {
            atr: AtrG::new(n),
            mult,
            started: false,
            up: false,
            fu: f64::NAN,
            fl: f64::NAN,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 2] {
        let pc = self.atr.pc;
        let atr = self.atr.push(h, l, c);
        if atr.is_nan() {
            return [f64::NAN; 2];
        }
        let hl2 = (h + l) * 0.5;
        let band = self.mult * atr;
        let bu = hl2 + band;
        let bl = hl2 - band;
        if !self.started {
            // The first bar with an ATR seeds the bands from its own midpoint.
            self.started = true;
            self.fu = bu;
            self.fl = bl;
            self.up = c >= hl2;
        } else {
            // Upper band: tighten only while the previous close stayed at or under it. Both compares
            // are selects; a `NaN` band leaves the carried one (as `f64::min` does).
            let tight = if bu < self.fu { bu } else { self.fu };
            self.fu = if pc <= self.fu { tight } else { bu };
            // Lower band: ratchet only up while the previous close stayed at or over it.
            let tight = if bl > self.fl { bl } else { self.fl };
            self.fl = if pc >= self.fl { tight } else { bl };
            let crossed_down = c < self.fl;
            let crossed_up = c > self.fu;
            self.up = if self.up { !crossed_down } else { crossed_up };
        }
        if self.up { [self.fl, 1.0] } else { [self.fu, -1.0] }
    }
}

// ------------------------------------------------------------------------------------------------
// ADXR, NATR
// ------------------------------------------------------------------------------------------------

/// ADX rating: the mean of the ADX line and its value `n` bars earlier. `NaN` until both exist
/// (bar `3n - 1`).
///
/// The ring of ADX values starts as `NaN`, so the warm-up is arithmetic.
#[derive(Clone)]
pub(crate) struct AdxrCore {
    adx: AdxCore,
    buf: Vec<f64>,
    head: usize,
}

impl AdxrCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        AdxrCore { adx: AdxCore::new(n), buf: vec![f64::NAN; n.clamp(1, RING_CAP)], head: 0 }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let adx = self.adx.push(h, l, c)[0];
        let old = std::mem::replace(&mut self.buf[self.head], adx);
        self.head += 1;
        if self.head == self.buf.len() {
            self.head = 0;
        }
        (adx + old) * 0.5
    }
}

/// Normalized ATR: `100 * ATR / close`, the ATR Wilder's over the ranges of bars `1..=n` (the first
/// bar has no previous close). `NaN` for the first `n` bars and on a bar that closes at exactly zero.
#[derive(Clone)]
pub(crate) struct NatrCore {
    atr: AtrG<true>,
}

impl NatrCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        NatrCore { atr: AtrG::new(n) }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let a = self.atr.push(h, l, c);
        let v = 100.0 * a / c;
        if c != 0.0 { v } else { f64::NAN }
    }
}

/// `typed_new!` with an `#[inline]` constructor, for the types of this group.
///
/// A constructor that is not `#[inline]` is an out-of-line call across the crate boundary: the struct
/// it returns is built through a pointer the caller can no longer see through, so a caller's loop
/// keeps the state in memory (and a literal period is not a constant to the loop). Inlined, the
/// state lives in registers (measured on `smma`: 3.0 against 1.5 ns a value).
macro_rules! typed_new_inline {
    ($Name:ident, $key:literal, [ $( $pf:ident = $pdef:expr ),+ ]) => {
        impl $Name {
            /// Build with these parameters, in the order the registry row lists them. A value the
            /// registry would not accept (out of range, or `NaN`) is a [`crate::ParamError`] naming
            /// it; [`crate::make_with`] clamps instead. `Default` gives the registry defaults.
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
    };
}
pub(crate) use typed_new_inline;
