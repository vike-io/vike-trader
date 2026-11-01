//! Fast cores for group `o` of the fourth wave of indicators (see `_followups/perf-conv4-addendum.md`):
//! the price transforms `avgprice`, `medprice`, `typprice`, `wclprice` and `heikin_ashi`, and the
//! structure indicators `pivot_points`, `volume_profile_poc`, `zigzag` and `williams_fractal`.
//!
//! The price transforms and the pivot lines are the old expressions written once, so every way in
//! (`next`, `next_*`, `vectorize`, `series::*`) runs the same code and the same bits come out. The
//! Heikin-Ashi core and the pivot core are small `Clone` structs of plain numbers whose `push` is
//! `#[inline(always)]` and holds no `&mut self` call.
//!
//! `volume_profile_poc` keeps the exact arithmetic of the old kernel (the histogram is folded oldest
//! bar first and the divisions stay divisions: a bin index is a discrete answer, and a multiplication
//! by a reciprocal could move a close across a bin edge), on struct-of-arrays rings that also carry
//! the bin of every close, so a bar that leaves the window's extremes alone costs one division.
//!
//! `zigzag` and `williams_fractal` read FUTURE bars: they are batch-authoritative, so what lives here
//! is the batch kernel over plain slices, which the `Bar` kernel and `series::*` both call.

use crate::roll::RING_CAP;
use vike_marketdata::Bar;

// ------------------------------------------------------------------------------------------------
// Price transforms
// ------------------------------------------------------------------------------------------------

/// `(O + H + L + C) / 4`.
#[inline(always)]
pub(crate) fn avgprice(o: f64, h: f64, l: f64, c: f64) -> f64 {
    (o + h + l + c) / 4.0
}

/// `(H + L) / 2`.
#[inline(always)]
pub(crate) fn medprice(h: f64, l: f64) -> f64 {
    (h + l) / 2.0
}

/// `(H + L + C) / 3`. A division, not a multiplication by `1/3`: the old bits stay.
#[inline(always)]
pub(crate) fn typprice(h: f64, l: f64, c: f64) -> f64 {
    (h + l + c) / 3.0
}

/// `(H + L + 2C) / 4`.
#[inline(always)]
pub(crate) fn wclprice(h: f64, l: f64, c: f64) -> f64 {
    (h + l + 2.0 * c) / 4.0
}

/// One output value per bar. Stateless, so built by `collect`: each element is written once, with no
/// prefill pass in front of it.
#[inline]
pub(crate) fn price_bars(bars: &[Bar], f: impl Fn(&Bar) -> f64) -> Vec<Vec<f64>> {
    vec![bars.iter().map(f).collect()]
}

// ------------------------------------------------------------------------------------------------
// Heikin-Ashi
// ------------------------------------------------------------------------------------------------

/// Heikin-Ashi candles: the close is the mean of the bar's four prices; the open is the mean of the
/// previous candle's open and close (the first candle opens at the mean of the bar's open and close);
/// the high and low are the bar's extremes widened to the candle's open and close.
///
/// An infinite-memory recurrence (every open depends on the previous candle), so a `NaN` price
/// poisons the open for good, as it always did.
#[derive(Clone)]
pub(crate) struct HeikinAshiCore {
    prev_open: f64,
    prev_close: f64,
    started: bool,
}

impl HeikinAshiCore {
    #[inline]
    pub(crate) fn new() -> Self {
        HeikinAshiCore { prev_open: f64::NAN, prev_close: f64::NAN, started: false }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    /// `[ha_open, ha_high, ha_low, ha_close]`.
    #[inline(always)]
    pub(crate) fn push(&mut self, o: f64, h: f64, l: f64, c: f64) -> [f64; 4] {
        let ha_close = (o + h + l + c) / 4.0;
        let ha_open =
            if self.started { (self.prev_open + self.prev_close) / 2.0 } else { (o + c) / 2.0 };
        self.started = true;
        self.prev_open = ha_open;
        self.prev_close = ha_close;
        let ha_high = h.max(ha_open).max(ha_close);
        let ha_low = l.min(ha_open).min(ha_close);
        [ha_open, ha_high, ha_low, ha_close]
    }
}

/// The four Heikin-Ashi columns of `bars`, the stepper's `vectorize`.
#[inline]
pub(crate) fn heikin_ashi_bars(bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut core = HeikinAshiCore::new();
    let n = bars.len();
    let mut cols = [vec![f64::NAN; n], vec![f64::NAN; n], vec![f64::NAN; n], vec![f64::NAN; n]];
    let [a, b, c, d] = &mut cols;
    for ((((x, y), z), w), bar) in
        a.iter_mut().zip(b.iter_mut()).zip(c.iter_mut()).zip(d.iter_mut()).zip(bars)
    {
        let v = core.push(bar.open, bar.high, bar.low, bar.close);
        *x = v[0];
        *y = v[1];
        *z = v[2];
        *w = v[3];
    }
    cols.into()
}

// ------------------------------------------------------------------------------------------------
// Pivot points
// ------------------------------------------------------------------------------------------------

/// Classic floor pivots from the PRIOR bar: `[p, r1, r2, r3, s1, s2, s3]`.
///
/// The previous bar starts as `NaN`, so the first bar is `NaN` on every line by arithmetic and the
/// hot path has no first-bar branch; a `NaN` price gives `NaN` lines on the bar after it, and nothing
/// is carried further.
#[derive(Clone)]
pub(crate) struct PivotCore {
    ph: f64,
    pl: f64,
    pc: f64,
}

impl PivotCore {
    #[inline]
    pub(crate) fn new() -> Self {
        PivotCore { ph: f64::NAN, pl: f64::NAN, pc: f64::NAN }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> [f64; 7] {
        let (ph, pl, pc) = (self.ph, self.pl, self.pc);
        self.ph = h;
        self.pl = l;
        self.pc = c;
        let p = (ph + pl + pc) / 3.0;
        let rng = ph - pl;
        [p, 2.0 * p - pl, p + rng, ph + 2.0 * (p - pl), 2.0 * p - ph, p - rng, pl - 2.0 * (ph - p)]
    }
}

/// The seven pivot columns of `bars`, the stepper's `vectorize`.
#[inline]
pub(crate) fn pivot_bars(bars: &[Bar]) -> Vec<Vec<f64>> {
    let mut core = PivotCore::new();
    let n = bars.len();
    let mut cols: Vec<Vec<f64>> = (0..7).map(|_| vec![f64::NAN; n]).collect();
    for (i, b) in bars.iter().enumerate() {
        let v = core.push(b.high, b.low, b.close);
        for (col, x) in cols.iter_mut().zip(v) {
            col[i] = x;
        }
    }
    cols
}

// ------------------------------------------------------------------------------------------------
// Volume profile point of control
// ------------------------------------------------------------------------------------------------

/// The lowest of `x` by the old kernel's fold (`NaN` skipped, `f64::MAX` when none is a number).
#[cold]
#[inline(never)]
fn lowest(x: &[f64]) -> f64 {
    let mut m = f64::MAX;
    for &v in x {
        if v < m {
            m = v;
        }
    }
    m
}

/// The highest of `x` by the old kernel's fold (`NaN` skipped, `f64::MIN` when none is a number).
#[cold]
#[inline(never)]
fn highest(x: &[f64]) -> f64 {
    let mut m = f64::MIN;
    for &v in x {
        if v > m {
            m = v;
        }
    }
    m
}

/// The bin of a close: `(c - price_min) / bin_width` truncated (`NaN` and anything below the range go
/// to bin 0) and clamped to the last bin. A division, never a multiplication by a reciprocal: a bin
/// index is a discrete answer and a close sitting on an edge must not move.
#[inline(always)]
fn bin_of(c: f64, price_min: f64, bin_width: f64, top: usize) -> u32 {
    (((c - price_min) / bin_width) as usize).min(top) as u32
}

/// Rolling point of control over the last `w` bars: the centre price of the highest-volume bin when
/// the volume of those bars is bucketed by close across their price range (the first bin wins a tie;
/// the window's price when it has no range). `NaN` until `w` bars are in.
///
/// Four rings (high, low, close, volume) plus the bin of each close. The window's lowest low and
/// highest high are carried: a new bar can only lower the low or raise the high, and a rescan is
/// needed only when the bar that LEAVES held the extreme. While both are unchanged the bin width is
/// unchanged and so is the bin of every close in the window, so only the new close is binned; the
/// histogram is then folded again, oldest bar first, which is the old kernel's order (the sums of
/// floats must not be updated by subtraction: the argmax of a rounded sum is a discrete answer).
///
/// A window whose every bin is `NaN` (a `NaN` volume reaching each of them) has no highest bin: the
/// answer is `NaN`, where the old kernel panicked on an `unwrap`.
#[derive(Clone)]
pub(crate) struct PocCore {
    w: usize,
    bins: usize,
    hi: Vec<f64>,
    lo: Vec<f64>,
    cl: Vec<f64>,
    vo: Vec<f64>,
    ix: Vec<u32>,
    bucket: Vec<f64>,
    pos: usize,
    seen: usize,
    pmin: f64,
    pmax: f64,
    /// The range the bins in `ix` were computed for; `valid` is false until they were.
    cmin: f64,
    cmax: f64,
    bin_width: f64,
    valid: bool,
}

impl PocCore {
    /// `w` and `bins` are capped at [`RING_CAP`]; a window of 0 never answers.
    #[inline]
    pub(crate) fn new(w: usize, bins: usize) -> Self {
        let w = w.min(RING_CAP);
        let bins = bins.min(RING_CAP);
        PocCore {
            w,
            bins,
            hi: vec![0.0; w],
            lo: vec![0.0; w],
            cl: vec![0.0; w],
            vo: vec![0.0; w],
            ix: vec![0; w],
            bucket: vec![0.0; bins],
            pos: 0,
            seen: 0,
            pmin: f64::MAX,
            pmax: f64::MIN,
            cmin: f64::NAN,
            cmax: f64::NAN,
            bin_width: f64::NAN,
            valid: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.hi.fill(0.0);
        self.lo.fill(0.0);
        self.cl.fill(0.0);
        self.vo.fill(0.0);
        self.pos = 0;
        self.seen = 0;
        self.valid = false;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64, v: f64) -> f64 {
        let w = self.w;
        if w == 0 {
            return f64::NAN;
        }
        let p = self.pos;
        let (old_h, old_l) = (self.hi[p], self.lo[p]);
        self.hi[p] = h;
        self.lo[p] = l;
        self.cl[p] = c;
        self.vo[p] = v;
        let next = if p + 1 == w { 0 } else { p + 1 };
        self.pos = next;
        if self.seen < w {
            self.seen += 1;
            if self.seen < w {
                return f64::NAN;
            }
            // The window just filled: both extremes from scratch, no bins yet.
            self.pmin = lowest(&self.lo);
            self.pmax = highest(&self.hi);
            self.valid = false;
        } else {
            if old_l == self.pmin {
                self.pmin = lowest(&self.lo);
            } else if l < self.pmin {
                self.pmin = l;
            }
            if old_h == self.pmax {
                self.pmax = highest(&self.hi);
            } else if h > self.pmax {
                self.pmax = h;
            }
        }
        let (price_min, price_max) = (self.pmin, self.pmax);
        if price_max == price_min {
            // Degenerate range: all prices the same; the POC is that price.
            self.valid = false;
            return price_min;
        }
        if self.bins == 0 {
            self.valid = false;
            return f64::NAN;
        }
        let top = self.bins - 1;
        if self.valid && price_min == self.cmin && price_max == self.cmax {
            self.ix[p] = bin_of(c, price_min, self.bin_width, top);
        } else {
            let bin_width = (price_max - price_min) / self.bins as f64;
            for (b, &cc) in self.ix.iter_mut().zip(&self.cl) {
                *b = bin_of(cc, price_min, bin_width, top);
            }
            self.bin_width = bin_width;
            self.cmin = price_min;
            self.cmax = price_max;
            self.valid = true;
        }
        // Oldest bar first: from the slot after the newest one round to the newest. A run of closes in
        // one bin (the usual case: consecutive closes are near each other) keeps that bin's sum in a
        // register instead of storing and reloading it, each add being the same add on the same
        // number, in the same order.
        let bucket = &mut self.bucket[..self.bins];
        bucket.fill(0.0);
        let mut cur = self.ix[next] as usize;
        let mut sum = 0.0;
        for (&b, &vol) in self.ix[next..]
            .iter()
            .zip(&self.vo[next..])
            .chain(self.ix[..next].iter().zip(&self.vo[..next]))
        {
            let b = b as usize;
            if b != cur {
                bucket[cur] = sum;
                cur = b;
                sum = bucket[b];
            }
            sum += vol;
        }
        bucket[cur] = sum;
        let mut max_vol = f64::MIN;
        let mut at = usize::MAX;
        for (i, &b) in bucket.iter().enumerate() {
            if b > max_vol {
                max_vol = b;
                at = i;
            }
        }
        if at == usize::MAX {
            return f64::NAN;
        }
        price_min + (at as f64 + 0.5) * self.bin_width
    }
}

// ------------------------------------------------------------------------------------------------
// ZigZag and Williams fractals: batch kernels over plain slices (they read future bars)
// ------------------------------------------------------------------------------------------------

/// The ZigZag pivots of parallel `highs` and `lows` (the shorter decides the length): the price of
/// each CONFIRMED swing extreme at its own bar, `NaN` everywhere else. A swing is confirmed once price
/// has reversed by `deviation` percent from the running extreme.
pub(crate) fn zigzag_slices(highs: &[f64], lows: &[f64], deviation: f64) -> Vec<f64> {
    let n = highs.len().min(lows.len());
    let (highs, lows) = (&highs[..n], &lows[..n]);
    let mut out = vec![f64::NAN; n];
    if n < 2 {
        return out;
    }
    // Looking for a new high (`up`) or a new low.
    let mut up = true;
    let mut extreme_price = highs[0];
    let mut extreme_idx = 0usize;
    for (i, (&h, &l)) in highs.iter().zip(lows).enumerate().skip(1) {
        if up {
            if h >= extreme_price {
                extreme_price = h;
                extreme_idx = i;
            } else if extreme_price > 0.0
                && (extreme_price - l) / extreme_price * 100.0 >= deviation
            {
                // Confirm the extreme as an up-pivot and start tracking a downswing from here.
                out[extreme_idx] = extreme_price;
                extreme_price = l;
                extreme_idx = i;
                up = false;
            }
        } else if l <= extreme_price {
            extreme_price = l;
            extreme_idx = i;
        } else if extreme_price > 0.0 && (h - extreme_price) / extreme_price * 100.0 >= deviation {
            // Confirm the extreme as a down-pivot and start tracking an upswing from here.
            out[extreme_idx] = extreme_price;
            extreme_price = h;
            extreme_idx = i;
            up = true;
        }
    }
    out
}

/// The fractals of bars `n..len - n`, for a window of `N` bars on each side known at compile time, so
/// that the comparisons unroll and the loop over bars has no dependency to stop it vectorizing.
#[inline(always)]
fn fractals_fixed<const N: usize>(highs: &[f64], lows: &[f64], up: &mut [f64], down: &mut [f64]) {
    let len = highs.len();
    for i in N..len - N {
        let (hc, lc) = (highs[i], lows[i]);
        // Every neighbour is compared, with no early exit: `|` keeps the loop branch-free, and a
        // `NaN` neighbour compares false, so it does not block.
        let mut blocked_up = false;
        let mut blocked_down = false;
        for k in 1..=N {
            blocked_up |= (highs[i - k] >= hc) | (highs[i + k] >= hc);
            blocked_down |= (lows[i - k] <= lc) | (lows[i + k] <= lc);
        }
        // Written either way (a select, not a branch), so the loop over bars vectorizes.
        up[i] = if blocked_up { f64::NAN } else { hc };
        down[i] = if blocked_down { f64::NAN } else { lc };
    }
}

/// Williams fractals of parallel `highs` and `lows` (the shorter decides the length), `[fractal_up,
/// fractal_down]`: a bar whose high is strictly above the `n` highs on each side (its low strictly
/// below the `n` lows on each side), the bar's own price at that bar and `NaN` elsewhere. The first
/// and last `n` bars cannot be fractals. A `NaN` neighbour does not block a fractal.
pub(crate) fn williams_fractal_slices(highs: &[f64], lows: &[f64], n: usize) -> [Vec<f64>; 2] {
    let len = highs.len().min(lows.len());
    let (highs, lows) = (&highs[..len], &lows[..len]);
    let mut up = vec![f64::NAN; len];
    let mut down = vec![f64::NAN; len];
    // Oracle `range(n, length - n)`, written so that no length underflows.
    if n >= 1 && len > n && len - n > n {
        let (u, d) = (&mut up[..], &mut down[..]);
        match n {
            1 => fractals_fixed::<1>(highs, lows, u, d),
            2 => fractals_fixed::<2>(highs, lows, u, d),
            3 => fractals_fixed::<3>(highs, lows, u, d),
            4 => fractals_fixed::<4>(highs, lows, u, d),
            5 => fractals_fixed::<5>(highs, lows, u, d),
            6 => fractals_fixed::<6>(highs, lows, u, d),
            7 => fractals_fixed::<7>(highs, lows, u, d),
            8 => fractals_fixed::<8>(highs, lows, u, d),
            9 => fractals_fixed::<9>(highs, lows, u, d),
            10 => fractals_fixed::<10>(highs, lows, u, d),
            _ => {
                for i in n..(len - n) {
                    let (hc, lc) = (highs[i], lows[i]);
                    let mut blocked_up = false;
                    let mut blocked_down = false;
                    for k in 1..=n {
                        blocked_up |= (highs[i - k] >= hc) | (highs[i + k] >= hc);
                        blocked_down |= (lows[i - k] <= lc) | (lows[i + k] <= lc);
                    }
                    if !blocked_up {
                        u[i] = hc;
                    }
                    if !blocked_down {
                        d[i] = lc;
                    }
                }
            }
        }
    } else if n == 0 && len > 0 {
        // A zero window has no neighbour, so every bar is both fractals (the old kernel's answer).
        up.copy_from_slice(highs);
        down.copy_from_slice(lows);
    }
    [up, down]
}

/// Heikin-Ashi over parallel slices into four prefilled columns of `len` (the shortest input) values:
/// the same expressions as [`HeikinAshiCore::push`], in two passes. The first carries the only
/// dependency (each open reads the previous open and close) and writes the opens and closes; the
/// second widens the highs and lows, which depend on nothing but their own bar and so vectorize.
#[inline]
pub(crate) fn heikin_ashi_fill(
    o: &[f64],
    h: &[f64],
    l: &[f64],
    c: &[f64],
    out: &mut [Vec<f64>; 4],
) {
    let len = out[0].len();
    let [ha_o, ha_h, ha_l, ha_c] = out;
    if len == 0 {
        return;
    }
    let (mut po, mut pc) = ((o[0] + c[0]) / 2.0, (o[0] + h[0] + l[0] + c[0]) / 4.0);
    ha_o[0] = po;
    ha_c[0] = pc;
    for i in 1..len {
        let ho = (po + pc) / 2.0;
        let hc = (o[i] + h[i] + l[i] + c[i]) / 4.0;
        ha_o[i] = ho;
        ha_c[i] = hc;
        po = ho;
        pc = hc;
    }
    for ((((hh, hl), &hi), &lo), (&ho, &hc)) in
        ha_h.iter_mut().zip(ha_l.iter_mut()).zip(h).zip(l).zip(ha_o.iter().zip(ha_c.iter()))
    {
        *hh = hi.max(ho).max(hc);
        *hl = lo.min(ho).min(hc);
    }
}
