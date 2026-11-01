//! Fast cores for group `q` of the fourth wave of indicators (see `_followups/perf-conv4-addendum.md`):
//! the eighteen TWO-BAR candlestick patterns of `indicators/patterns/two_bar.rs`.
//!
//! A two-bar pattern reads the current bar, the one before it and (for seven of them) the rolling
//! average body. Today they stream by re-running a batch kernel over a retained tail on every bar;
//! here each is a small kernel `signal(previous, current, average body)` and ONE core,
//! [`TwoBarCore`], that keeps the previous bar (its four prices and the numbers every kernel derives
//! from them, computed ONCE per bar and reused when that bar is the previous one) and, for the
//! kernels that read it, an [`AvgBody`]. The stream, `vectorize` and the slice calls all run that
//! one `push`.
//!
//! The output is `+100.0`, `-100.0` or `0.0` and is BIT-IDENTICAL to the old kernels' on every
//! input. Each kernel is the old one's conditions, with every early `continue` turned into one
//! boolean `skip` and every `&&` / `||` over pure comparisons turned into `&` / `|` (no operand has
//! a side effect, so no operand can tell): the whole signal is then a handful of comparisons and one
//! select, with no branch for a random price series to mispredict. A comparison with a `NaN` price
//! is false, exactly as before (`skip` is spelled with the POSITIVE `<` / `>` / `<=` of the old
//! code, never as a negated comparison, so a `NaN` skips or passes exactly where it did), and a
//! `NaN` poisons the average body for ten bars, exactly as before. The first bar answers `0.0`
//! (nothing before it), and so does every bar of the average-body kernels' first nine (no average
//! yet).
//!
//! The one-line candle helpers are written here, `#[inline(always)]`, from `patterns.rs` (which this
//! group may not edit): a call into another file's plain function does not inline across code
//! generation units.

use crate::fast_pat::CTX;
use std::marker::PhantomData;

/// One bar: its four prices and what the kernels derive from them. A derived number is computed only
/// for a kernel that reads it (`TwoBar::HILO`, `TwoBar::MARU`), and each is the old expression
/// verbatim: `hi = o.max(c)`, `lo = o.min(c)`, `body = (c - o).abs()`, `maru` = `is_marubozu`.
#[derive(Clone, Copy)]
pub(crate) struct Ohlc {
    pub(crate) o: f64,
    pub(crate) h: f64,
    pub(crate) l: f64,
    pub(crate) c: f64,
    /// `max(open, close)`: the top of the body.
    pub(crate) hi: f64,
    /// `min(open, close)`: the bottom of the body.
    pub(crate) lo: f64,
    /// `|close - open|`.
    pub(crate) body: f64,
    /// Both shadows at most 5% of a real range (`patterns.rs`'s `is_marubozu`). The old code
    /// returned `false` early for `r <= 0`; `r > 0` is false for that and for a `NaN` range alike,
    /// and the old code's `NaN` range failed its shadow comparisons, so the answer is the same.
    pub(crate) maru: bool,
}

impl Ohlc {
    #[inline(always)]
    fn of<K: TwoBar>(o: f64, h: f64, l: f64, c: f64) -> Self {
        let body = (c - o).abs();
        let (hi, lo) = if K::HILO { (o.max(c), o.min(c)) } else { (f64::NAN, f64::NAN) };
        let r = h - l;
        let maru = K::MARU && (r > 0.0) & (h - hi <= 0.05 * r) & (lo - l <= 0.05 * r);
        Ohlc { o, h, l, c, hi, lo, body, maru }
    }

    const BLANK: Ohlc = Ohlc {
        o: f64::NAN,
        h: f64::NAN,
        l: f64::NAN,
        c: f64::NAN,
        hi: f64::NAN,
        lo: f64::NAN,
        body: f64::NAN,
        maru: false,
    };
}

/// `patterns.rs`'s `is_white`: a bullish candle, `close > open`.
#[inline(always)]
fn white(o: f64, c: f64) -> bool {
    c > o
}

/// `patterns.rs`'s `is_black`: a bearish candle, `close < open`.
#[inline(always)]
fn black(o: f64, c: f64) -> bool {
    c < o
}

/// The old guard `a.is_nan() || a <= 0.0` on the average body: true when the pattern must stay quiet.
#[inline(always)]
fn no_avg(a: f64) -> bool {
    a.is_nan() | (a <= 0.0)
}

/// `+100` when `up`, `-100` when `down`, else `0`: the old `if up {100.0} else if down {-100.0}`.
/// Every use has `up` and `down` mutually exclusive (they test opposite colours of the previous
/// bar), so the order of the old `else if` cannot matter; a difference of two flags needs no branch.
#[inline(always)]
fn pick(up: bool, down: bool) -> f64 {
    f64::from(i32::from(up) - i32::from(down)) * 100.0
}

/// `+100` when `hit`, else `0`.
#[inline(always)]
fn bull(hit: bool) -> f64 {
    if hit { 100.0 } else { 0.0 }
}

/// `-100` when `hit`, else `0`.
#[inline(always)]
fn bear(hit: bool) -> f64 {
    if hit { -100.0 } else { 0.0 }
}

/// The average body over the last `CTX` bars, `NaN` until `CTX` bodies are in: the old
/// `avg_body` computed as a SHIFT REGISTER OF PARTIAL FOLDS instead of folding a ring each bar.
///
/// The old average is the left fold `(((b[t-9] + b[t-8]) + b[t-7]) ... ) + b[t]` (from `-0.0`, which
/// changes nothing: `-0.0 + b` is `b` for every body, an absolute value being never `-0.0`). Each
/// window starts one bar after the last, so the nine folds still under way at bar `t` are the
/// prefixes of the next nine windows: `acc[i]` holds the fold of the `CTX - 1 - i` newest bodies
/// before this bar. A new body `b` extends every one by one addition (`acc[i + 1] + b` is the next
/// shorter window's prefix, `acc[0] + b` is the finished window) and starts a one-element prefix.
/// Every addition is the old fold's, in the old order, so the bits are the same, but a bar's value
/// is ONE addition after its body rather than nine in a row: the chain that kept a bar's result
/// waiting nine additions (and the iterations behind it out of the processor's window) is gone, and
/// nothing is loaded from memory.
#[derive(Clone)]
struct AvgBody {
    acc: [f64; CTX - 1],
    seen: usize,
}

impl AvgBody {
    #[inline]
    fn new() -> Self {
        AvgBody { acc: [0.0; CTX - 1], seen: 0 }
    }

    fn clear(&mut self) {
        *self = Self::new();
    }

    /// Push a bar's body (`|close - open|`); the average body including it.
    #[inline(always)]
    fn push(&mut self, b: f64) -> f64 {
        let full = self.acc[0] + b;
        let mut next = [b; CTX - 1];
        for (n, a) in next.iter_mut().zip(&self.acc[1..]) {
            *n = *a + b;
        }
        self.acc = next;
        if self.seen < CTX - 1 {
            self.seen += 1;
            return f64::NAN;
        }
        full / CTX as f64
    }
}

/// A two-bar pattern: its signal from the previous bar, the current bar and the average body
/// (`NaN` while fewer than ten bodies are in, and for a kernel with `AVG == false`, which never
/// reads it and whose average is not even computed).
pub(crate) trait TwoBar: Clone {
    /// Whether the kernel reads the average body.
    const AVG: bool;
    /// Whether the kernel reads `Ohlc::hi` and `Ohlc::lo`.
    const HILO: bool = false;
    /// Whether the kernel reads `Ohlc::maru` (it needs `HILO` too).
    const MARU: bool = false;
    /// The signal on `cur` given the bar before it (the first bar never gets here).
    fn signal(prev: Ohlc, cur: Ohlc, avg: f64) -> f64;
}

/// The state of one two-bar pattern: the previous bar and, for a kernel that reads it, the average
/// body.
#[derive(Clone)]
pub(crate) struct TwoBarCore<K: TwoBar> {
    prev: Ohlc,
    seen: bool,
    avg: AvgBody,
    kernel: PhantomData<K>,
}

impl<K: TwoBar> TwoBarCore<K> {
    #[inline]
    pub(crate) fn new() -> Self {
        TwoBarCore { prev: Ohlc::BLANK, seen: false, avg: AvgBody::new(), kernel: PhantomData }
    }

    pub(crate) fn clear(&mut self) {
        self.prev = Ohlc::BLANK;
        self.seen = false;
        self.avg.clear();
    }

    /// Feed one bar; the signal on it: `+100.0`, `-100.0` or `0.0`.
    #[inline(always)]
    pub(crate) fn push(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
        let cur = Ohlc::of::<K>(open, high, low, close);
        let avg = if K::AVG { self.avg.push(cur.body) } else { f64::NAN };
        let out = if self.seen { K::signal(self.prev, cur, avg) } else { 0.0 };
        self.prev = cur;
        self.seen = true;
        out
    }
}

/// The whole-series loop of every two-bar pattern: the signal on each bar of parallel `open`,
/// `high`, `low` and `close` slices (the result is as long as the shortest).
#[inline(always)]
pub(crate) fn run<K: TwoBar>(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    let len = open.len().min(high.len()).min(low.len()).min(close.len());
    let mut core = TwoBarCore::<K>::new();
    // Filled first, then written by index (see `series::ema`).
    let mut out = vec![f64::NAN; len];
    for ((((x, &o), &h), &l), &c) in out.iter_mut().zip(open).zip(high).zip(low).zip(close) {
        *x = core.push(o, h, l, c);
    }
    out
}

/// Declares a kernel: a zero-sized type and its `signal`. `$p` and `$k` are the previous and the
/// current bar, `$a` the average body; `hilo` / `maru` say which derived numbers it reads.
macro_rules! kernel {
    (
        $(#[$m:meta])* $K:ident, avg = $avg:literal, hilo = $hilo:literal, maru = $maru:literal,
        |$p:ident, $k:ident, $a:ident| $body:block
    ) => {
        $(#[$m])*
        #[derive(Clone)]
        pub(crate) struct $K;
        impl TwoBar for $K {
            const AVG: bool = $avg;
            const HILO: bool = $hilo;
            const MARU: bool = $maru;
            #[inline(always)]
            fn signal($p: Ohlc, $k: Ohlc, $a: f64) -> f64 $body
        }
    };
}

kernel! {
    /// `engulfing`: the current body engulfs the prior opposite-colour body.
    EngulfingKernel, avg = false, hilo = false, maru = false, |p, k, _a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        pick(
            black(po, pc) & white(oo, cc) & (cc >= po) & (oo <= pc),
            white(po, pc) & black(oo, cc) & (oo >= pc) & (cc <= po),
        )
    }
}

kernel! {
    /// `doji_star`: a doji gapping away from a prior long body.
    DojiStarKernel, avg = true, hilo = false, maru = false, |p, k, a| {
        let (po, pc) = (p.o, p.c);
        let skip = no_avg(a) | (k.h - k.l <= 0.0) | (k.body > 0.1 * a) | (p.body < a);
        pick(!skip & black(po, pc) & (k.h < pc), !skip & white(po, pc) & (k.l > pc))
    }
}

kernel! {
    /// `harami`: the current body inside the prior opposite-colour body.
    HaramiKernel, avg = false, hilo = true, maru = false, |p, k, _a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        let skip = (k.hi >= p.hi) | (k.lo <= p.lo);
        pick(!skip & black(po, pc) & white(oo, cc), !skip & white(po, pc) & black(oo, cc))
    }
}

kernel! {
    /// `harami_cross`: a harami whose current bar is a doji.
    HaramiCrossKernel, avg = true, hilo = true, maru = false, |p, k, a| {
        let (po, pc) = (p.o, p.c);
        let doji_mid = (k.o + k.c) / 2.0;
        let skip = no_avg(a) | (k.body > 0.1 * a) | (doji_mid >= p.hi) | (doji_mid <= p.lo);
        pick(!skip & black(po, pc), !skip & white(po, pc))
    }
}

kernel! {
    /// `piercing`: a bullish bar opening below the prior low and closing above the prior midpoint.
    PiercingKernel, avg = false, hilo = false, maru = false, |p, k, _a| {
        let (po, pl, pc, oo, cc) = (p.o, p.l, p.c, k.o, k.c);
        let midpoint = (po + pc) / 2.0;
        bull(black(po, pc) & white(oo, cc) & (oo < pl) & (cc > midpoint) & (cc < po))
    }
}

kernel! {
    /// `dark_cloud_cover`: a bearish bar opening above the prior high and closing below the prior midpoint.
    DarkCloudCoverKernel, avg = false, hilo = false, maru = false, |p, k, _a| {
        let (po, ph, pc, oo, cc) = (p.o, p.h, p.c, k.o, k.c);
        let midpoint = (po + pc) / 2.0;
        bear(white(po, pc) & black(oo, cc) & (oo > ph) & (cc < midpoint) & (cc > po))
    }
}

kernel! {
    /// `counterattack`: opposite-colour bodies closing at about the same price.
    CounterattackKernel, avg = true, hilo = false, maru = false, |p, k, a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        let skip = no_avg(a)
            | ((cc - pc).abs() > 0.03 * a + 1e-9)
            | (p.body < 0.3 * a)
            | (k.body < 0.3 * a);
        pick(!skip & black(po, pc) & white(oo, cc), !skip & white(po, pc) & black(oo, cc))
    }
}

kernel! {
    /// `meeting_lines`: a counterattack with a tighter close tolerance.
    MeetingLinesKernel, avg = true, hilo = false, maru = false, |p, k, a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        let skip = no_avg(a)
            | ((cc - pc).abs() > 0.02 * a + 1e-9)
            | (p.body < 0.3 * a)
            | (k.body < 0.3 * a);
        pick(!skip & black(po, pc) & white(oo, cc), !skip & white(po, pc) & black(oo, cc))
    }
}

kernel! {
    /// `separating_lines`: a same-colour continuation opening at the prior open.
    SeparatingLinesKernel, avg = true, hilo = false, maru = false, |p, k, a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        let skip = no_avg(a) | ((oo - po).abs() > 0.01 * a + 1e-9);
        pick(!skip & white(po, pc) & white(oo, cc), !skip & black(po, pc) & black(oo, cc))
    }
}

kernel! {
    /// `matching_low`: two black candles with equal closes (bullish).
    MatchingLowKernel, avg = true, hilo = false, maru = false, |p, k, a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        let skip = no_avg(a);
        bull(!skip & black(po, pc) & black(oo, cc) & ((cc - pc).abs() <= 0.01 * a + 1e-9))
    }
}

kernel! {
    /// `on_neck`: a bearish continuation closing at about the prior low.
    OnNeckKernel, avg = false, hilo = false, maru = false, |p, k, _a| {
        let (po, pl, pc, oo, cc) = (p.o, p.l, p.c, k.o, k.c);
        let no_body = p.body <= 0.0;
        bear(
            black(po, pc)
                & white(oo, cc)
                & !no_body
                & (oo < pl)
                & ((cc - pl).abs() <= 0.03 * p.body),
        )
    }
}

kernel! {
    /// `in_neck`: a bearish continuation closing slightly into the prior body.
    InNeckKernel, avg = false, hilo = false, maru = false, |p, k, _a| {
        let (po, pl, pc, oo, cc) = (p.o, p.l, p.c, k.o, k.c);
        let no_body = p.body <= 0.0;
        bear(
            black(po, pc)
                & white(oo, cc)
                & !no_body
                & (oo < pl)
                & (cc > pc)
                & ((cc - pc) <= 0.15 * p.body),
        )
    }
}

kernel! {
    /// `thrusting`: a bearish continuation closing below the prior midpoint.
    ThrustingKernel, avg = false, hilo = false, maru = false, |p, k, _a| {
        let (po, pl, pc, oo, cc) = (p.o, p.l, p.c, k.o, k.c);
        let no_body = p.body <= 0.0;
        let midpoint = (po + pc) / 2.0;
        bear(black(po, pc) & white(oo, cc) & !no_body & (oo < pl) & (cc > pc) & (cc < midpoint))
    }
}

kernel! {
    /// `kicking`: opposite-colour marubozu with a gap.
    KickingKernel, avg = false, hilo = true, maru = true, |p, k, _a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        let both = p.maru & k.maru;
        pick(
            both & black(po, pc) & white(oo, cc) & (oo > pc),
            both & white(po, pc) & black(oo, cc) & (oo < pc),
        )
    }
}

kernel! {
    /// `kicking_by_length`: a kicking whose signal is the longer marubozu's colour.
    KickingByLengthKernel, avg = false, hilo = true, maru = true, |p, k, _a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        let is_gap_up = black(po, pc) & white(oo, cc) & (oo > pc);
        let is_gap_down = white(po, pc) & black(oo, cc) & (oo < pc);
        let hit = p.maru & k.maru & (is_gap_up | is_gap_down);
        // The longer body's colour; the current bar on a tie (and on a `NaN`, as before). Written as
        // flag arithmetic so a random series has no branch to mispredict: +1 for white, -1 for
        // black, times 1 for a hit and 0 for none.
        let longer_is_current = k.body >= p.body;
        let white_wins =
            (longer_is_current & white(oo, cc)) | (!longer_is_current & white(po, pc));
        f64::from((i32::from(white_wins) * 2 - 1) * i32::from(hit)) * 100.0
    }
}

kernel! {
    /// `homing_pigeon`: two blacks, the second inside the first (bullish).
    HomingPigeonKernel, avg = false, hilo = true, maru = false, |p, k, _a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        bull(black(po, pc) & black(oo, cc) & (k.hi < p.hi) & (k.lo > p.lo))
    }
}

kernel! {
    /// `gap_side_side_white`: two similar-size white candles, the second gapping up.
    GapSideSideWhiteKernel, avg = true, hilo = false, maru = false, |p, k, a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        let skip = no_avg(a) | (p.body <= 0.0) | (k.body <= 0.0);
        bull(
            !skip
                & white(po, pc)
                & white(oo, cc)
                & (oo > pc)
                & ((k.body - p.body).abs() <= 0.5 * p.body.max(k.body)),
        )
    }
}

kernel! {
    /// `tasuki_gap`: a gap, then an opposite-colour candle staying inside the prior body.
    TasukiGapKernel, avg = false, hilo = true, maru = false, |p, k, _a| {
        let (po, pc, oo, cc) = (p.o, p.c, k.o, k.c);
        let inside = (p.lo < oo) & (oo < p.hi);
        pick(
            white(po, pc) & black(oo, cc) & inside & (cc > po),
            black(po, pc) & white(oo, cc) & inside & (cc < po),
        )
    }
}

/// Generates a two-bar pattern's stepper around its kernel: the struct, `Default`, `next` (a
/// [`vike_marketdata::Bar`]), `next_ohlc` (plain numbers), the registry-equality test and the
/// `Indicator` impl, whose `vectorize` is the stream folded over the bars. A pattern has no
/// parameter, so it has no `new` (as with `Ad`); `Default` is its constructor.
macro_rules! two_bar_indicator {
    ($(#[$m:meta])* $Name:ident, $key:literal, $K:ty) => {
        $(#[$m])*
        ///
        /// Hand-written bounded state (the previous bar, and for some patterns the ten-body average;
        /// `trims_history() == false`). The signal is `+100.0`, `-100.0` or `0.0`; the first bar
        /// answers `0.0`.
        #[derive(Clone)]
        pub struct $Name {
            core: $crate::fast_q::TwoBarCore<$K>,
            last: f64,
        }
        impl $Name {
            /// The registry defaults. Private: `Default` is the public spelling.
            #[inline]
            fn defaults() -> Self {
                Self { core: $crate::fast_q::TwoBarCore::new(), last: f64::NAN }
            }
            /// The registry's constructor: a pattern has no parameter, so whatever is given is ignored.
            #[inline]
            pub(crate) fn with_params(_p: &[f64]) -> Self {
                Self::defaults()
            }
            /// Feed the next bar and get this bar's signal. This IS [`crate::Indicator::on_bar`], bit for
            /// bit, without the `Vec`.
            #[inline]
            pub fn next(&mut self, bar: &vike_marketdata::Bar) -> f64 {
                self.step(bar)
            }
            /// Feed the next bar's `open`, `high`, `low` and `close` as plain numbers, no
            /// [`vike_marketdata::Bar`] needed; the same answer as `next` for a bar with those.
            #[inline]
            pub fn next_ohlc(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
                self.last = self.core.push(open, high, low, close);
                self.last
            }
            /// One bar of the recurrence; `next` and `Indicator::on_bar` both run it.
            #[inline]
            fn step(&mut self, bar: &vike_marketdata::Bar) -> f64 {
                self.next_ohlc(bar.open, bar.high, bar.low, bar.close)
            }
        }
        impl Default for $Name {
            fn default() -> Self {
                Self::defaults()
            }
        }
        $crate::indicators::macros::typed_new!($Name, $key, []);
        impl $crate::Indicator for $Name {
            fn on_bar(&mut self, bar: &vike_marketdata::Bar) -> Vec<f64> {
                vec![self.step(bar)]
            }
            fn vectorize(&self, bars: &[vike_marketdata::Bar]) -> Vec<Vec<f64>> {
                $crate::roll::batch_scalar(self, bars, Self::step)
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
        }
    };
}

pub(crate) use two_bar_indicator;
