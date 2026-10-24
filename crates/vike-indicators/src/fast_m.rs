//! Fast cores for group `m` of the fourth wave of indicators (see `_followups/perf-conv4-addendum.md`):
//! the 18 one-bar candlestick patterns (`src/indicators/patterns/one_bar.rs`).
//!
//! A pattern is a pure decision over the current bar and the rolling average body ([`AvgBody`]).
//! Each decision is a zero-sized RULE type (`Rule1`) holding the one expression of that pattern, and
//! the core here ([`Core1`]) carries the state: the average body. The streaming stepper,
//! `vectorize` and `series::*` all run the same rule, so they agree bit for bit.
//!
//! The decisions are the old batch kernels' conditions, comparison for comparison (same constants, same
//! `NaN` behaviour: a comparison with a `NaN` is false, so a bar with a `NaN` field answers whatever the
//! old kernel answered). The output is `+100.0`, `-100.0` or `0.0`, never anything else, so there is no
//! tolerance anywhere: the new output is bit-identical to the old (`tests/fast_m.rs` keeps a copy of the
//! old kernels and compares bit for bit).
//!
//! ## Why the rules have no branches
//!
//! Every condition is evaluated and combined with `&` / `|` instead of `&&` / `||` and early returns:
//! the conditions are pure comparisons, so evaluating all of them changes nothing but the number of
//! mispredicted branches (the outcome of a threshold test on a price series is close to a coin flip).
//! A skip such as `r <= 0.0 || b <= 0.0` stays a SKIP (`!skip & ..`), never `r > 0.0`: for a `NaN` the
//! first is true and the second false, and the old kernel took the first reading.
//!
//! ## The average body
//!
//! `patterns.rs`'s `avg_body` is the mean of the last ten bodies, every window folded oldest first from
//! `-0.0`; a running sum rounds differently and would flip a signal now and then, so the window is
//! folded again on every bar. It is a straight line of nine additions over a shift register
//! ([`AvgBody`]): fixed positions, no ring index, no data-dependent branch. The
//! whole-series call runs the same fold over windows of a body array, ten additions per window and
//! independent from window to window, which the compiler vectorizes.

use std::marker::PhantomData;

/// Bars of context for the rolling average body (`patterns.rs`'s `CTX`).
const CTX: usize = 10;

// ------------------------------------------------------------------------------------------------
// The candle helpers (patterns.rs's private ones, restated: that file is shared with group n)
// ------------------------------------------------------------------------------------------------

/// `|close - open|`.
#[inline(always)]
fn body(o: f64, c: f64) -> f64 {
    (c - o).abs()
}

/// `high - low`.
#[inline(always)]
fn rng(h: f64, l: f64) -> f64 {
    h - l
}

/// `high - max(open, close)`.
#[inline(always)]
fn upper(o: f64, h: f64, c: f64) -> f64 {
    h - o.max(c)
}

/// `min(open, close) - low`.
#[inline(always)]
fn lower(o: f64, l: f64, c: f64) -> f64 {
    o.min(c) - l
}

/// `+100.0` when `up`, `-100.0` when `down`, else `0.0`, as arithmetic on the two flags (no branch, no
/// three-way select). Every caller passes flags that cannot both be true (`close > open` against
/// `close < open`), so `up` and `down` together never arise; the integer `0` becomes `+0.0`, never
/// `-0.0`.
#[inline(always)]
fn signed(up: bool, down: bool) -> f64 {
    (i32::from(up) - i32::from(down)) as f64 * 100.0
}

/// `+100.0` when `hit`, else `0.0`.
#[inline(always)]
fn flag(hit: bool) -> f64 {
    if hit { 100.0 } else { 0.0 }
}

/// `-100.0` when `hit`, else `0.0`.
#[inline(always)]
fn flag_down(hit: bool) -> f64 {
    if hit { -100.0 } else { 0.0 }
}

/// The old `avg is None / not > 0` skip: no usable average body (`NaN`, zero or negative).
#[inline(always)]
fn no_avg(a: f64) -> bool {
    a.is_nan() | (a <= 0.0)
}

// ------------------------------------------------------------------------------------------------
// The average body
// ------------------------------------------------------------------------------------------------

/// One window of ten bodies folded oldest first from `-0.0` and divided by ten: exactly
/// `iter().sum::<f64>() / 10.0` of the old `math::sma`.
#[inline(always)]
fn fold_window(w: &[f64; CTX]) -> f64 {
    let mut sum = -0.0_f64;
    for &v in w {
        sum += v;
    }
    sum / CTX as f64
}

/// The mean of the last ten bodies `|close - open|`, `NaN` until ten are in (and while a `NaN` body is
/// inside), one bar at a time.
///
/// The window is a shift register, oldest first: each bar moves nine bodies down one place and puts the
/// new one last, so the fold reads fixed positions (no ring index, no wrap-around, no data-dependent
/// address) and, with the stepper inlined into a loop, the ten numbers stay in registers. It starts as
/// `NaN`, so the first nine answers are `NaN` by arithmetic and there is no warm-up branch.
#[derive(Clone)]
pub(crate) struct AvgBody {
    w: [f64; CTX],
}

impl AvgBody {
    #[inline]
    pub(crate) fn new() -> Self {
        AvgBody { w: [f64::NAN; CTX] }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    /// Push a bar's open and close; the average body including it.
    #[inline(always)]
    pub(crate) fn push(&mut self, open: f64, close: f64) -> f64 {
        let w = &mut self.w;
        w[0] = w[1];
        w[1] = w[2];
        w[2] = w[3];
        w[3] = w[4];
        w[4] = w[5];
        w[5] = w[6];
        w[6] = w[7];
        w[7] = w[8];
        w[8] = w[9];
        w[9] = (close - open).abs();
        fold_window(w)
    }
}

// ------------------------------------------------------------------------------------------------
// The core
// ------------------------------------------------------------------------------------------------

/// A pattern decided by the current bar alone (with the average body when `AVG`).
pub(crate) trait Rule1 {
    /// Whether [`Rule1::eval`] reads `avg`; when it does not the core skips the average.
    const AVG: bool;
    /// `+100.0`, `-100.0` or `0.0` for a bar `(o, h, l, c)` whose average body is `avg`.
    fn eval(o: f64, h: f64, l: f64, c: f64, avg: f64) -> f64;
}

/// The state of a one-bar pattern: the rolling average body.
pub(crate) struct Core1<R> {
    avg: AvgBody,
    rule: PhantomData<fn() -> R>,
}

impl<R> Clone for Core1<R> {
    fn clone(&self) -> Self {
        Core1 { avg: self.avg.clone(), rule: PhantomData }
    }
}

impl<R: Rule1> Core1<R> {
    #[inline]
    pub(crate) fn new() -> Self {
        Core1 { avg: AvgBody::new(), rule: PhantomData }
    }

    pub(crate) fn clear(&mut self) {
        self.avg.clear();
    }

    /// The signal of the next bar: `+100.0`, `-100.0` or `0.0`.
    #[inline(always)]
    pub(crate) fn push(&mut self, o: f64, h: f64, l: f64, c: f64) -> f64 {
        let avg = if R::AVG { self.avg.push(o, c) } else { f64::NAN };
        R::eval(o, h, l, c, avg)
    }
}

// ------------------------------------------------------------------------------------------------
// The rule generator
// ------------------------------------------------------------------------------------------------

/// A one-bar rule: `|o, h, l, c, avg| { body }` (unused names start with `_`).
macro_rules! rule1 {
    ($(#[$m:meta])* $Name:ident, $avg:expr, |$o:ident, $h:ident, $l:ident, $c:ident, $a:ident| $body:block) => {
        $(#[$m])*
        #[derive(Clone, Copy)]
        pub(crate) struct $Name;
        impl Rule1 for $Name {
            const AVG: bool = $avg;
            #[inline(always)]
            fn eval($o: f64, $h: f64, $l: f64, $c: f64, $a: f64) -> f64 {
                $body
            }
        }
    };
}

// ------------------------------------------------------------------------------------------------
// One-bar rules
// ------------------------------------------------------------------------------------------------

rule1! {
    /// Tiny body against the average body, with a real range.
    DojiRule, true, |o, h, l, c, a| {
        flag(!a.is_nan() & (a > 0.0) & (body(o, c) <= 0.1 * a) & (rng(h, l) > 0.0))
    }
}

rule1! {
    /// Small body, long lower shadow, short upper one (bullish).
    HammerRule, false, |o, h, l, c, _a| {
        let b = body(o, c);
        let r = rng(h, l);
        let skip = (r <= 0.0) | (b <= 0.0);
        let lo = lower(o, l, c);
        let up = upper(o, h, c);
        flag(!skip & (b <= 0.3 * r) & (lo >= 2.0 * b) & (up <= b))
    }
}

rule1! {
    /// Small body, long upper shadow, short lower one (bullish).
    InvertedHammerRule, false, |o, h, l, c, _a| {
        let b = body(o, c);
        let r = rng(h, l);
        let skip = (r <= 0.0) | (b <= 0.0);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        flag(!skip & (b <= 0.3 * r) & (up >= 2.0 * b) & (lo <= b))
    }
}

rule1! {
    /// The hammer geometry as a bearish signal.
    HangingManRule, false, |o, h, l, c, _a| {
        let b = body(o, c);
        let r = rng(h, l);
        let skip = (r <= 0.0) | (b <= 0.0);
        let lo = lower(o, l, c);
        let up = upper(o, h, c);
        flag_down(!skip & (b <= 0.3 * r) & (lo >= 2.0 * b) & (up <= b))
    }
}

rule1! {
    /// The inverted-hammer geometry as a bearish signal.
    ShootingStarRule, false, |o, h, l, c, _a| {
        let b = body(o, c);
        let r = rng(h, l);
        let skip = (r <= 0.0) | (b <= 0.0);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        flag_down(!skip & (b <= 0.3 * r) & (up >= 2.0 * b) & (lo <= b))
    }
}

rule1! {
    /// Doji with a long lower shadow and almost no upper one (bullish).
    DragonflyDojiRule, true, |o, h, l, c, a| {
        let b = body(o, c);
        let r = rng(h, l);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        let skip = no_avg(a) | (r <= 0.0);
        flag(!skip & (b <= 0.1 * a) & (up <= 0.1 * r) & (lo >= 0.5 * r))
    }
}

rule1! {
    /// Doji with a long upper shadow and almost no lower one (bearish).
    GravestoneDojiRule, true, |o, h, l, c, a| {
        let b = body(o, c);
        let r = rng(h, l);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        let skip = no_avg(a) | (r <= 0.0);
        flag_down(!skip & (b <= 0.1 * a) & (lo <= 0.1 * r) & (up >= 0.5 * r))
    }
}

rule1! {
    /// Doji with both shadows long.
    LongleggedDojiRule, true, |o, h, l, c, a| {
        let b = body(o, c);
        let r = rng(h, l);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        let skip = no_avg(a) | (r <= 0.0);
        flag(!skip & (b <= 0.1 * a) & (up >= 0.3 * r) & (lo >= 0.3 * r))
    }
}

rule1! {
    /// Long-legged doji with the body near the middle of the range.
    RickshawManRule, true, |o, h, l, c, a| {
        let b = body(o, c);
        let r = rng(h, l);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        let body_mid = (o.max(c) + o.min(c)) / 2.0;
        let range_mid = (h + l) / 2.0;
        let skip = no_avg(a) | (r <= 0.0);
        flag(
            !skip
                & (b <= 0.1 * a)
                & (up >= 0.3 * r)
                & (lo >= 0.3 * r)
                & ((body_mid - range_mid).abs() <= 0.25 * r),
        )
    }
}

rule1! {
    /// Dragonfly doji with an exceptionally long lower shadow.
    TakuriRule, true, |o, h, l, c, a| {
        let b = body(o, c);
        let r = rng(h, l);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        let skip = no_avg(a) | (r <= 0.0);
        flag(!skip & (b <= 0.1 * a) & (up <= 0.1 * r) & (lo >= 0.7 * r))
    }
}

rule1! {
    /// Body is nearly the full range: white +100, black -100.
    MarubozuRule, false, |o, h, l, c, _a| {
        let b = body(o, c);
        let r = rng(h, l);
        let skip = (r <= 0.0) | (b <= 0.0);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        let both = !skip & (up <= 0.05 * r) & (lo <= 0.05 * r);
        signed(both & (c > o), both & (c < o))
    }
}

rule1! {
    /// No shadow on the close side: white +100, black -100.
    ClosingMarubozuRule, false, |o, h, l, c, _a| {
        let b = body(o, c);
        let r = rng(h, l);
        let skip = (r <= 0.0) | (b <= 0.0);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        signed(!skip & (c > o) & (up <= 0.05 * r), !skip & (c < o) & (lo <= 0.05 * r))
    }
}

rule1! {
    /// Small body with both shadows larger than the body.
    SpinningTopRule, true, |o, h, l, c, a| {
        let b = body(o, c);
        let r = rng(h, l);
        let skip = no_avg(a) | (r <= 0.0) | (b <= 0.0);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        flag(!skip & (b <= 0.3 * r) & (up > b) & (lo > b))
    }
}

rule1! {
    /// Very small body with very long shadows on both sides.
    HighWaveRule, false, |o, h, l, c, _a| {
        let b = body(o, c);
        let r = rng(h, l);
        let skip = (r <= 0.0) | (b <= 0.0);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        flag(!skip & (b <= 0.15 * r) & (up >= 3.0 * b) & (lo >= 3.0 * b))
    }
}

rule1! {
    /// A body of at least 1.3 average bodies: white +100, black -100.
    LongLineRule, true, |o, _h, _l, c, a| {
        let b = body(o, c);
        let long = !no_avg(a) & (b >= 1.3 * a);
        signed(long & (c > o), long & (c < o))
    }
}

rule1! {
    /// A short, compact candle (presence).
    ShortLineRule, true, |o, h, l, c, a| {
        let b = body(o, c);
        let r = rng(h, l);
        flag(!no_avg(a) & (b <= 0.5 * a) & (r <= a))
    }
}

rule1! {
    /// A long body opening at its extreme: white +100, black -100.
    BeltHoldRule, true, |o, h, l, c, a| {
        let b = body(o, c);
        let r = rng(h, l);
        let skip = no_avg(a) | (r <= 0.0) | (b <= 0.0);
        let lo = lower(o, l, c);
        let up = upper(o, h, c);
        signed(
            !skip & (c > o) & (lo <= 0.05 * r) & (b >= a),
            !skip & (c < o) & (up <= 0.05 * r) & (b >= a),
        )
    }
}

rule1! {
    /// No shadow on the open side: white +100, black -100.
    OpeningMarubozuRule, false, |o, h, l, c, _a| {
        let b = body(o, c);
        let r = rng(h, l);
        let skip = (r <= 0.0) | (b <= 0.0);
        let up = upper(o, h, c);
        let lo = lower(o, l, c);
        signed(!skip & (c > o) & (lo <= 0.05 * r), !skip & (c < o) & (up <= 0.05 * r))
    }
}

// ------------------------------------------------------------------------------------------------
// The stepper type of one pattern
// ------------------------------------------------------------------------------------------------

/// The public type of one pattern: a stepper over a core, `next(&Bar)` and `next_ohlc(..)` (the same
/// answer, one with a `Bar` and one with plain numbers), `vectorize` as the core folded over the
/// bars, and the registry plumbing. A pattern has no parameter, so `Default` is its constructor.
macro_rules! pattern_stepper {
    ($(#[$m:meta])* $Name:ident, $key:literal, $Core:ty) => {
        $(#[$m])*
        ///
        /// Hand-written bounded state (the rolling average body and the bar(s) it compares,
        /// `trims_history() == false`), over a core in `crate::fast_m`.
        #[derive(Clone)]
        pub struct $Name {
            core: $Core,
            last: f64,
        }
        impl $Name {
            /// The registry defaults. Private: `Default` is the public spelling.
            #[inline]
            fn defaults() -> Self {
                Self { core: <$Core>::new(), last: f64::NAN }
            }
            #[inline]
            pub(crate) fn with_params(_p: &[f64]) -> Self {
                Self::defaults()
            }
            /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec. The
            /// answer is `100.0`, `-100.0` or `0.0`.
            #[inline]
            pub fn next(&mut self, bar: &vike_marketdata::Bar) -> f64 {
                self.next_ohlc(bar.open, bar.high, bar.low, bar.close)
            }
            /// Feed the next bar's open, high, low and close as plain numbers, no [`vike_marketdata::Bar`] needed;
            /// the same answer as [`Self::next`] for a bar with those values.
            #[inline]
            pub fn next_ohlc(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
                self.last = self.core.push(open, high, low, close);
                self.last
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
                vec![self.next(bar)]
            }
            fn vectorize(&self, bars: &[vike_marketdata::Bar]) -> Vec<Vec<f64>> {
                let mut core = <$Core>::new();
                let mut out = vec![0.0_f64; bars.len()];
                for (o, b) in out.iter_mut().zip(bars) {
                    *o = core.push(b.open, b.high, b.low, b.close);
                }
                vec![out]
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

pub(crate) use pattern_stepper;

/// Bars per chunk of the whole-series call (the stack arrays below hold one chunk).
const CHUNK: usize = 256;

/// Run a one-bar rule over four parallel slices (the result is as long as the shortest).
///
/// A rule that does not read the average body is one loop over the slices. One that does is chunked:
/// the bodies of a chunk go into an array behind the nine bodies before it, every window of ten is
/// folded (independent from window to window, so the compiler vectorizes it), and the rule runs over
/// the chunk with its averages. The fold is the one [`AvgBody::push`] runs, so a series call and a
/// stream agree bit for bit.
#[inline(always)]
pub(crate) fn run1<R: Rule1>(open: &[f64], high: &[f64], low: &[f64], close: &[f64]) -> Vec<f64> {
    let len = open.len().min(high.len()).min(low.len()).min(close.len());
    let (open, high, low, close) = (&open[..len], &high[..len], &low[..len], &close[..len]);
    let mut out = vec![0.0_f64; len];
    if !R::AVG {
        for ((((o, &op), &h), &l), &c) in out.iter_mut().zip(open).zip(high).zip(low).zip(close) {
            *o = R::eval(op, h, l, c, f64::NAN);
        }
        return out;
    }
    // `bod[j]` is the body of the bar `j - 9` of the chunk; the nine before the first chunk are `NaN`.
    let mut bod = [f64::NAN; CHUNK + CTX - 1];
    let mut avg = [0.0_f64; CHUNK];
    let mut start = 0;
    while start < len {
        let end = (start + CHUNK).min(len);
        let m = end - start;
        let (op, hi, lo, cl) =
            (&open[start..end], &high[start..end], &low[start..end], &close[start..end]);
        for ((b, &x), &y) in bod[CTX - 1..CTX - 1 + m].iter_mut().zip(op).zip(cl) {
            *b = (y - x).abs();
        }
        for (a, w) in avg[..m].iter_mut().zip(bod[..m + CTX - 1].windows(CTX)) {
            *a = fold_window(w.try_into().unwrap());
        }
        for (((((o, &x), &h), &l), &c), &a) in
            out[start..end].iter_mut().zip(op).zip(hi).zip(lo).zip(cl).zip(&avg[..m])
        {
            *o = R::eval(x, h, l, c, a);
        }
        bod.copy_within(m..m + CTX - 1, 0);
        start = end;
    }
    out
}
