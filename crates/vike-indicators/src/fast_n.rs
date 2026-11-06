//! Fast cores for group `n` of the fourth wave of indicators (see `_followups/perf-conv4-addendum.md`):
//! the 27 candlestick patterns that read three, four or five bars (`patterns/three_bar.rs`,
//! `patterns/four_five_bar.rs`).
//!
//! A pattern is a pure function of the last `K` bars `(open, high, low, close)` and, for most of
//! them, of the average body `avg_body` (the SMA of the last [`CTX`] bodies `|close - open|`). The
//! signal is `+100.0`, `-100.0` or `0.0`, and it must be **bit-identical** to what the history
//! recompute gave: a discrete signal does not get a tolerance. So every kernel below is the old
//! `batch_<name>` body, test for test, with the tests written in their original form (a `NaN` makes
//! the same tests true and false as before). Two things differ from the old code, and neither can
//! change a signal:
//!
//! * the tests that do not read the average body run first and the average is fetched only when they
//!   pass (every test is a pure comparison that ends in `0.0`, so the order cannot change the
//!   answer, only how often the ten-term fold below runs);
//! * the first `K - 1` values are `0.0` (the old kernels started their loop at index `K - 1` and left
//!   the output at `0.0` before it).
//!
//! The ten-term fold is the old `avg_body`'s, oldest body first from `-0.0` (what
//! `iter().sum::<f64>()` does, and what the old `avg_body` was), but run on demand: [`Bodies`] only
//! writes the ring each bar and folds when a kernel asks, which most bars never do.
//!
//! [`Core3`], [`Core4`] and [`Core5`] (by the number of bars) are the one implementation: `next(&Bar)`, `next_ohlc`, `vectorize` and the slice call
//! `series::<pattern>(open, high, low, close)` all run its `push`.

use crate::fast_pat::CTX;
use std::marker::PhantomData;

/// One bar's four prices.
#[derive(Clone, Copy)]
pub(crate) struct Ohlc {
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
}

/// Slots of the bodies ring: a power of two at least [`CTX`], so an index wraps with a mask.
const RING: usize = 16;
const _: () = assert!(RING.is_power_of_two() && RING >= CTX);

/// The last [`CTX`] bodies `|close - open|`, folded on demand, and the count of bars seen.
///
/// `get` is `avg_body` of the bar just pushed: `NaN` until `CTX` bodies are in, then the mean of the
/// last `CTX` folded oldest first from `-0.0`, exactly the old `avg_body`. ONE
/// counter (`n`, bars pushed) is all the position state: the slot of a body is `n & (RING - 1)`
/// (no wrap compare, no bounds check), and the window being full, the average being defined and the
/// pattern having its bars are all comparisons of `n`.
// ⚠ The ring is on the heap (`Box`) on purpose. With the array inside the struct the compiler kept the
// WHOLE state (window, counter, ring) in memory inside a caller's loop, because the ring is indexed by
// a runtime value; with only scalars in the struct it keeps the window and the counter in registers.
// Measured on `morning_star` over 50,000 bars: the slice call 260 us -> 110 us, one stream step
// 5.2 ns -> 2.4 ns. A clone costs one 128-byte allocation.
#[derive(Clone)]
pub(crate) struct Bodies {
    buf: Box<[f64; RING]>,
    n: usize,
}

impl Bodies {
    #[inline]
    pub(crate) fn new() -> Self {
        Bodies { buf: Box::new([0.0; RING]), n: 0 }
    }

    #[inline(always)]
    fn push(&mut self, open: f64, close: f64) {
        self.buf[self.n & (RING - 1)] = (close - open).abs();
        self.n = self.n.wrapping_add(1);
    }

    /// Count a bar whose body no kernel of this pattern reads.
    #[inline(always)]
    fn skip(&mut self) {
        self.n = self.n.wrapping_add(1);
    }

    /// The average body including the bar just pushed.
    #[inline(always)]
    fn get(&self) -> f64 {
        if self.n < CTX {
            return f64::NAN;
        }
        // Oldest first.
        let first = self.n - CTX;
        let mut sum = -0.0_f64;
        let mut j = 0;
        while j < CTX {
            sum += self.buf[(first + j) & (RING - 1)];
            j += 1;
        }
        sum / CTX as f64
    }

    /// The average body, or `None` when it is `NaN` or not positive (the old `a.is_nan() || a <= 0.0`
    /// gate that opened every kernel which reads it).
    #[inline(always)]
    fn usable(&self) -> Option<f64> {
        let a = self.get();
        if a.is_nan() || a <= 0.0 { None } else { Some(a) }
    }
}

/// A pattern over the last `K` bars (`w[K - 1]` is the current one, `w[0]` the oldest).
pub(crate) trait Pattern<const K: usize>: Clone + Send + Sync + 'static {
    /// Whether `eval` reads the average body; when it does not, the ring is not even written.
    const AVG: bool;
    /// `+100.0`, `-100.0` or `0.0`.
    fn eval(w: &[Ohlc; K], avg: &Bodies) -> f64;
}

/// What a stepper needs from the state of a pattern, whatever number of bars it keeps.
pub(crate) trait PatPush: Clone + Send + Sync + 'static {
    fn new() -> Self;
    /// Push a bar; the signal for it. `0.0` for the first bars, until the window is full.
    fn push(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64;
}

/// The state of a pattern over `$K` bars: the `$K - 1` before the current one as NAMED fields (so a
/// caller's loop can keep them in registers), and the bodies ring with its bar count, which also says
/// whether the window is full yet. `$shift` moves each previous bar one place towards the oldest and
/// stores the current one last.
macro_rules! pat_core {
    ($Core:ident, $K:literal, [$($p:ident),+], |$s:ident, $cur:ident| $shift:block) => {
        #[derive(Clone)]
        pub(crate) struct $Core<P> {
            $($p: Ohlc,)+
            bodies: Bodies,
            kernel: PhantomData<fn() -> P>,
        }

        impl<P: Pattern<$K>> $Core<P> {
            #[inline]
            pub(crate) fn clear(&mut self) {
                *self = <Self as PatPush>::new();
            }
        }

        impl<P: Pattern<$K>> PatPush for $Core<P> {
            #[inline]
            fn new() -> Self {
                let z = Ohlc { o: 0.0, h: 0.0, l: 0.0, c: 0.0 };
                $Core { $($p: z,)+ bodies: Bodies::new(), kernel: PhantomData }
            }

            #[inline(always)]
            fn push(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
                if P::AVG {
                    self.bodies.push(open, close);
                } else {
                    self.bodies.skip();
                }
                let $cur = Ohlc { o: open, h: high, l: low, c: close };
                let w = [$(self.$p,)+ $cur];
                {
                    let $s = &mut *self;
                    $shift
                }
                if self.bodies.n < $K {
                    return 0.0;
                }
                P::eval(&w, &self.bodies)
            }
        }
    };
}

pat_core!(Core3, 3, [p1, p2], |s, cur| {
    s.p1 = s.p2;
    s.p2 = cur;
});
pat_core!(Core4, 4, [p1, p2, p3], |s, cur| {
    s.p1 = s.p2;
    s.p2 = s.p3;
    s.p3 = cur;
});
pat_core!(Core5, 5, [p1, p2, p3, p4], |s, cur| {
    s.p1 = s.p2;
    s.p2 = s.p3;
    s.p3 = s.p4;
    s.p4 = cur;
});

/// `a || b || ..` over pure `bool` terms, counted instead of branched: see the note above the
/// kernels.
macro_rules! any_of {
    ($($x:expr),+ $(,)?) => {
        (0u32 $(+ u32::from($x))+) != 0
    };
}

/// `a && b && ..` over pure `bool` terms, counted instead of branched.
macro_rules! all_of {
    ($($x:expr),+ $(,)?) => {
        (0u32 $(+ u32::from($x))+) == [$(stringify!($x)),+].len() as u32
    };
}

// ------------------------------------------------------------------------------ candle helpers
// The helpers at the top of `patterns.rs`, copied (they are private to that file).

#[inline(always)]
fn body(o: f64, c: f64) -> f64 {
    (c - o).abs()
}

#[inline(always)]
fn rng(h: f64, l: f64) -> f64 {
    h - l
}

#[inline(always)]
fn upper(o: f64, h: f64, c: f64) -> f64 {
    h - o.max(c)
}

#[inline(always)]
fn lower(o: f64, l: f64, c: f64) -> f64 {
    o.min(c) - l
}

#[inline(always)]
fn is_white(o: f64, c: f64) -> bool {
    c > o
}

#[inline(always)]
fn is_black(o: f64, c: f64) -> bool {
    c < o
}

/// `(o.max(c) + o.min(c)) / 2.0`, the middle of a body. Without a `NaN` the maximum and the minimum are
/// the two operands in some order, and addition does not care about order, so `o + c` is the same
/// bits and is cheaper than `max` and `min`; a `NaN` operand (which
/// `max` and `min` skip) takes the old expression.
#[inline(always)]
fn body_mid(o: f64, c: f64) -> f64 {
    let s = o + c;
    if s.is_nan() { (o.max(c) + o.min(c)) / 2.0 } else { s / 2.0 }
}

// `!(r <= 0.0)` is the old `if r <= 0.0 { return false }`: a `NaN` range passes it, `r > 0.0` would not.
#[allow(clippy::neg_cmp_op_on_partial_ord, clippy::allow_attributes)]
#[inline(always)]
fn is_marubozu(o: f64, h: f64, l: f64, c: f64) -> bool {
    let r = rng(h, l);
    all_of!(!(r <= 0.0), upper(o, h, c) <= 0.05 * r, lower(o, l, c) <= 0.05 * r)
}

/// Declares a pattern kernel: a unit struct and its `Pattern<K>` impl. The body sees the window as
/// `$w` and the lazily folded average body as `$avg`.
macro_rules! kernel {
    ($(#[$m:meta])* $Name:ident, $K:literal, avg = $avg:literal, |$w:ident, $a:ident| $body:block) => {
        $(#[$m])*
        #[derive(Clone)]
        pub(crate) struct $Name;
        impl Pattern<$K> for $Name {
            const AVG: bool = $avg;
            #[inline(always)]
            // The negated comparisons are the old kernels' own tests, kept in their form: `!(a >= b)` and
            // `a < b` differ on `NaN`, and the signal must not.
            #[allow(
                unused_variables,
                clippy::neg_cmp_op_on_partial_ord,
                clippy::allow_attributes
            )]
            fn eval($w: &[Ohlc; $K], $a: &Bodies) -> f64 $body
        }
    };
}

// ============================================================================= three bars
//
// ⚠ How every kernel below is written. A pattern is a conjunction of pure comparisons, and on a
// real series each comparison is about as likely true as false, so `a && b && c` compiled to one
// branch per term mispredicts every other bar (and `a & b` / `a | b` on `bool` is split back into
// branches by the code generator). The terms are therefore COUNTED: `any_of!` / `all_of!` add the
// terms as integers, so the whole test is one value and the only branch left is "does the pattern
// hold", which is almost never. The terms are pure (comparisons and copies, nothing that can panic
// or has an effect), so the truth value is the one of `&&` / `||`. Each term keeps the FORM it had
// in the old kernel (a rejection `h2 >= c1` stays `h2 >= c1`, not `h2 < c1`): they differ on
// `NaN`, and the signal must not.
//
// Six kernels (`three_inside`, `three_stars_in_south`, `three_line_strike`, `concealing_baby_swallow`,
// `rise_fall_three_methods`, `breakaway`) join their terms with `&` and `|` instead, and measured
// faster that way on both test series: their conjunctions are long (eight or more terms) and the
// first terms fail on most bars, so the code generator's short-circuit skips the rest. Same truth
// value either way.

kernel! {
    /// Long black, star, long white (bullish).
    MorningStarK, 3, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, h2, c2) = (w[1].o, w[1].h, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let mid1 = (o1 + c1) / 2.0;
        if any_of!(!is_black(o1, c1), h2 >= c1, !is_white(o3, c3), !(c3 > mid1)) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(body(o1, c1) < a, body(o2, c2) >= 0.3 * a, body(o3, c3) < a) {
            return 0.0;
        }
        100.0
    }
}

kernel! {
    /// Long white, star, long black (bearish).
    EveningStarK, 3, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, l2, c2) = (w[1].o, w[1].l, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let mid1 = (o1 + c1) / 2.0;
        if any_of!(!is_white(o1, c1), l2 <= c1, !is_black(o3, c3), !(c3 < mid1)) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(body(o1, c1) < a, body(o2, c2) >= 0.3 * a, body(o3, c3) < a) {
            return 0.0;
        }
        -100.0
    }
}

kernel! {
    /// Morning star whose star is a doji.
    MorningDojiStarK, 3, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, h2, c2) = (w[1].o, w[1].h, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let mid1 = (o1 + c1) / 2.0;
        if any_of!(!is_black(o1, c1), h2 >= c1, !is_white(o3, c3), !(c3 > mid1)) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(body(o1, c1) < a, body(o2, c2) > 0.1 * a, body(o3, c3) < a) {
            return 0.0;
        }
        100.0
    }
}

kernel! {
    /// Evening star whose star is a doji.
    EveningDojiStarK, 3, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, l2, c2) = (w[1].o, w[1].l, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let mid1 = (o1 + c1) / 2.0;
        if any_of!(!is_white(o1, c1), l2 <= c1, !is_black(o3, c3), !(c3 < mid1)) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(body(o1, c1) < a, body(o2, c2) > 0.1 * a, body(o3, c3) < a) {
            return 0.0;
        }
        -100.0
    }
}

kernel! {
    /// Three progressing long white candles.
    ThreeWhiteSoldiersK, 3, avg = true, |w, avg| {
        let (o1, h1, c1) = (w[0].o, w[0].h, w[0].c);
        let (o2, h2, c2) = (w[1].o, w[1].h, w[1].c);
        let (o3, h3, c3) = (w[2].o, w[2].h, w[2].c);
        let white = all_of!(is_white(o1, c1), is_white(o2, c2), is_white(o3, c3));
        let ordered = all_of!(o1 < o2, o2 < c1, o2 < o3, o3 < c2);
        let rising = all_of!(c1 < c2, c2 < c3);
        if any_of!(
            !white,
            !ordered,
            upper(o1, h1, c1) > 0.3 * body(o1, c1),
            upper(o2, h2, c2) > 0.3 * body(o2, c2),
            upper(o3, h3, c3) > 0.3 * body(o3, c3),
            !rising
        ) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(body(o1, c1) < 0.7 * a, body(o2, c2) < 0.7 * a, body(o3, c3) < 0.7 * a) {
            return 0.0;
        }
        100.0
    }
}

kernel! {
    /// Three progressing long black candles.
    ThreeBlackCrowsK, 3, avg = true, |w, avg| {
        let (o1, l1, c1) = (w[0].o, w[0].l, w[0].c);
        let (o2, l2, c2) = (w[1].o, w[1].l, w[1].c);
        let (o3, l3, c3) = (w[2].o, w[2].l, w[2].c);
        let black = all_of!(is_black(o1, c1), is_black(o2, c2), is_black(o3, c3));
        let ordered = all_of!(c1 < o2, o2 < o1, c2 < o3, o3 < o2);
        let falling = all_of!(c1 > c2, c2 > c3);
        if any_of!(
            !black,
            !ordered,
            lower(o1, l1, c1) > 0.3 * body(o1, c1),
            lower(o2, l2, c2) > 0.3 * body(o2, c2),
            lower(o3, l3, c3) > 0.3 * body(o3, c3),
            !falling
        ) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(body(o1, c1) < 0.7 * a, body(o2, c2) < 0.7 * a, body(o3, c3) < 0.7 * a) {
            return 0.0;
        }
        -100.0
    }
}

kernel! {
    /// Three crows each opening at about the prior close.
    IdenticalThreeCrowsK, 3, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let black = all_of!(is_black(o1, c1), is_black(o2, c2), is_black(o3, c3));
        let falling = all_of!(c1 > c2, c2 > c3);
        if any_of!(!black, !falling) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(body(o1, c1) < 0.7 * a, body(o2, c2) < 0.7 * a, body(o3, c3) < 0.7 * a) {
            return 0.0;
        }
        let tol = 0.05 * a;
        if any_of!((o2 - c1).abs() > tol, (o3 - c2).abs() > tol) {
            return 0.0;
        }
        -100.0
    }
}

kernel! {
    /// Harami then a confirming third bar.
    ThreeInsideK, 3, avg = false, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let inside = !(o2.max(c2) >= o1.max(c1)) & !(o2.min(c2) <= o1.min(c1));
        if inside & is_black(o1, c1) & is_white(o2, c2) & is_white(o3, c3) & (c3 > o1) {
            100.0
        } else if inside & is_white(o1, c1) & is_black(o2, c2) & is_black(o3, c3) & (c3 < o1) {
            -100.0
        } else {
            0.0
        }
    }
}

kernel! {
    /// Engulfing then a confirming third bar.
    ThreeOutsideK, 3, avg = false, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        // The two first-bar colours exclude each other, so the old `if .. { if .. } else if ..`
        // is one test per direction.
        let bull = all_of!(
            is_black(o1, c1),
            is_white(o2, c2),
            c2 >= o1,
            o2 <= c1,
            is_white(o3, c3),
            c3 > c2
        );
        let bear = all_of!(
            is_white(o1, c1),
            is_black(o2, c2),
            o2 >= c1,
            c2 <= o1,
            is_black(o3, c3),
            c3 < c2
        );
        if bull {
            100.0
        } else if bear {
            -100.0
        } else {
            0.0
        }
    }
}

kernel! {
    /// Three black candles of diminishing range.
    ThreeStarsInSouthK, 3, avg = false, |w, avg| {
        let (o1, h1, l1, c1) = (w[0].o, w[0].h, w[0].l, w[0].c);
        let (o2, h2, l2, c2) = (w[1].o, w[1].h, w[1].l, w[1].c);
        let (o3, h3, l3, c3) = (w[2].o, w[2].h, w[2].l, w[2].c);
        let rng1 = rng(h1, l1);
        let rng2 = rng(h2, l2);
        let rng3 = rng(h3, l3);
        if !(is_black(o1, c1) & is_black(o2, c2) & is_black(o3, c3))
            | (rng1 <= 0.0)
            | (rng2 <= 0.0)
            | (rng3 <= 0.0)
            | !((rng1 > rng2) & (rng2 > rng3))
            | (l2 <= l1)
            | (h3 >= h2)
            | (l3 <= l2)
        {
            return 0.0;
        }
        100.0
    }
}

kernel! {
    /// Doji island reversal with gaps on both sides.
    AbandonedBabyK, 3, avg = true, |w, avg| {
        let (o1, h1, l1, c1) = (w[0].o, w[0].h, w[0].l, w[0].c);
        let (o2, h2, l2, c2) = (w[1].o, w[1].h, w[1].l, w[1].c);
        let (o3, h3, l3, c3) = (w[2].o, w[2].h, w[2].l, w[2].c);
        let bull = all_of!(is_black(o1, c1), h2 < l1, is_white(o3, c3), l3 >= h2);
        let bear = all_of!(is_white(o1, c1), l2 > h1, is_black(o3, c3), h3 <= l2);
        if any_of!(rng(h2, l2) <= 0.0, !any_of!(bull, bear)) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(body(o2, c2) > 0.1 * a, !(body(o1, c1) >= a)) {
            return 0.0;
        }
        if bull { 100.0 } else { -100.0 }
    }
}

kernel! {
    /// Three whites weakening (bearish warning).
    AdvanceBlockK, 3, avg = true, |w, avg| {
        let (o1, h1, c1) = (w[0].o, w[0].h, w[0].c);
        let (o2, h2, c2) = (w[1].o, w[1].h, w[1].c);
        let (o3, h3, c3) = (w[2].o, w[2].h, w[2].c);
        let b1 = body(o1, c1);
        let b2 = body(o2, c2);
        let b3 = body(o3, c3);
        let u1 = upper(o1, h1, c1);
        let u2 = upper(o2, h2, c2);
        let u3 = upper(o3, h3, c3);
        let white = all_of!(is_white(o1, c1), is_white(o2, c2), is_white(o3, c3));
        let rising = all_of!(c1 < c2, c2 < c3);
        let weakening_bodies = all_of!(b2 < b1, b3 < b2);
        let growing_shadows = all_of!(u2 > u1, u3 > u2);
        if any_of!(
            !white,
            b1 <= 0.0,
            b2 <= 0.0,
            b3 <= 0.0,
            !rising,
            !any_of!(weakening_bodies, growing_shadows)
        ) {
            return 0.0;
        }
        if avg.usable().is_none() {
            return 0.0;
        }
        -100.0
    }
}

kernel! {
    /// Two long whites then a small stalling white.
    StalledPatternK, 3, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let white = all_of!(is_white(o1, c1), is_white(o2, c2), is_white(o3, c3));
        let stall = all_of!(c1 < c2, o3 >= c2 * 0.98);
        if any_of!(!white, !stall) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(body(o1, c1) < a, body(o2, c2) < a, body(o3, c3) >= 0.5 * a) {
            return 0.0;
        }
        -100.0
    }
}

kernel! {
    /// Long white, gap-up black, black closing into the first body.
    TwoCrowsK, 3, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let closes_in = all_of!(o1 < c3, c3 < c1);
        if any_of!(!is_white(o1, c1), !is_black(o2, c2), o2 <= c1, !is_black(o3, c3), !closes_in) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if body(o1, c1) < a {
            return 0.0;
        }
        -100.0
    }
}

kernel! {
    /// Gap-up black then an engulfing black in the gap.
    UpsideGapTwoCrowsK, 3, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let engulfs = all_of!(o3 >= o2, c3 <= c2, c3 > c1);
        if any_of!(!is_white(o1, c1), !is_black(o2, c2), o2 <= c1, !is_black(o3, c3), !engulfs) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if body(o1, c1) < a {
            return 0.0;
        }
        -100.0
    }
}

kernel! {
    /// Three dojis with a gapping middle (reversal by direction).
    TristarK, 3, avg = true, |w, avg| {
        let (o1, h1, l1, c1) = (w[0].o, w[0].h, w[0].l, w[0].c);
        let (o2, h2, l2, c2) = (w[1].o, w[1].h, w[1].l, w[1].c);
        let (o3, h3, l3, c3) = (w[2].o, w[2].h, w[2].l, w[2].c);
        let mid1 = body_mid(o1, c1);
        let mid2 = body_mid(o2, c2);
        let mid3 = body_mid(o3, c3);
        let bull = all_of!(mid2 < mid1, mid3 > mid2);
        let bear = all_of!(mid2 > mid1, mid3 < mid2);
        if any_of!(rng(h1, l1) <= 0.0, rng(h2, l2) <= 0.0, rng(h3, l3) <= 0.0, !any_of!(bull, bear)) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(body(o1, c1) > 0.1 * a, body(o2, c2) > 0.1 * a, body(o3, c3) > 0.1 * a) {
            return 0.0;
        }
        if bull { 100.0 } else { -100.0 }
    }
}

kernel! {
    /// Long black, lower-low black, small white (bullish).
    UniqueThreeRiverK, 3, avg = true, |w, avg| {
        let (o1, l1, c1) = (w[0].o, w[0].l, w[0].c);
        let (o2, l2, c2) = (w[1].o, w[1].l, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let inside = all_of!(o2.min(c2) > o1.min(c1), o2.max(c2) < o1.max(c1));
        if any_of!(
            !is_black(o1, c1),
            !is_black(o2, c2),
            !inside,
            l2 >= l1,
            !is_white(o3, c3),
            body(o3, c3) >= body(o2, c2)
        ) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if body(o1, c1) < a {
            return 0.0;
        }
        100.0
    }
}

kernel! {
    /// Black/white/black with equal outer closes (bullish).
    StickSandwichK, 3, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        if !all_of!(is_black(o1, c1), is_white(o2, c2), is_black(o3, c3)) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if (c3 - c1).abs() <= 0.03 * a + 1e-9 {
            return 100.0;
        }
        0.0
    }
}

kernel! {
    /// A gap then a partial-fill continuation candle.
    XsideGapThreeMethodsK, 3, avg = false, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let bull = all_of!(is_white(o1, c1), is_white(o2, c2), o2 > c1, is_black(o3, c3), c3 > c1);
        let bear = all_of!(is_black(o1, c1), is_black(o2, c2), o2 < c1, is_white(o3, c3), c3 < c1);
        if bull {
            100.0
        } else if bear {
            -100.0
        } else {
            0.0
        }
    }
}

// ============================================================================ four, five bars

kernel! {
    /// Three trend candles then an engulfing fourth.
    ThreeLineStrikeK, 4, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let (o4, c4) = (w[3].o, w[3].c);
        let bear = is_white(o1, c1)
            & is_white(o2, c2)
            & is_white(o3, c3)
            & (c1 < c2)
            & (c2 < c3)
            & is_black(o4, c4)
            & (o4 >= c3)
            & (c4 <= o1);
        let bull = is_black(o1, c1)
            & is_black(o2, c2)
            & is_black(o3, c3)
            & (c1 > c2)
            & (c2 > c3)
            & is_white(o4, c4)
            & (o4 <= c3)
            & (c4 >= o1);
        if !(bear | bull) {
            return 0.0;
        }
        if avg.usable().is_none() {
            return 0.0;
        }
        if bear { -100.0 } else { 100.0 }
    }
}

kernel! {
    /// Four stepping blacks then a white reversal (bullish).
    LadderBottomK, 5, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let (o4, h4, c4) = (w[3].o, w[3].h, w[3].c);
        let (o5, c5) = (w[4].o, w[4].c);
        let black = all_of!(is_black(o1, c1), is_black(o2, c2), is_black(o3, c3), is_black(o4, c4));
        let stepping = all_of!(c1 > c2, c2 > c3, c3 > c4);
        let reversal = all_of!(is_white(o5, c5), c5 > o4);
        if any_of!(!black, !stepping, upper(o4, h4, c4) <= 0.0, !reversal) {
            return 0.0;
        }
        if avg.usable().is_none() {
            return 0.0;
        }
        100.0
    }
}

kernel! {
    /// Four blacks, a marubozu pair, an engulfing fourth.
    ConcealingBabySwallowK, 4, avg = false, |w, avg| {
        let (o1, h1, l1, c1) = (w[0].o, w[0].h, w[0].l, w[0].c);
        let (o2, h2, l2, c2) = (w[1].o, w[1].h, w[1].l, w[1].c);
        let (o3, h3, l3, c3) = (w[2].o, w[2].h, w[2].l, w[2].c);
        let (o4, h4, l4, c4) = (w[3].o, w[3].h, w[3].l, w[3].c);
        if is_black(o1, c1)
            & is_black(o2, c2)
            & is_black(o3, c3)
            & is_black(o4, c4)
            & is_marubozu(o1, h1, l1, c1)
            & is_marubozu(o2, h2, l2, c2)
            & !(upper(o3, h3, c3) <= 0.0)
            & (h4 >= h3)
            & (l4 <= l3)
        {
            100.0
        } else {
            0.0
        }
    }
}

kernel! {
    /// Five-bar continuation (rising or falling).
    RiseFallThreeMethodsK, 5, avg = true, |w, avg| {
        let (o1, h1, l1, c1) = (w[0].o, w[0].h, w[0].l, w[0].c);
        let (o2, h2, l2, c2) = (w[1].o, w[1].h, w[1].l, w[1].c);
        let (o3, h3, l3, c3) = (w[2].o, w[2].h, w[2].l, w[2].c);
        let (o4, h4, l4, c4) = (w[3].o, w[3].h, w[3].l, w[3].c);
        let (o5, c5) = (w[4].o, w[4].c);
        let inside = (l2 > l1) & (h2 < h1) & (l3 > l1) & (h3 < h1) & (l4 > l1) & (h4 < h1);
        let bull = is_white(o1, c1)
            & is_black(o2, c2)
            & is_black(o3, c3)
            & is_black(o4, c4)
            & is_white(o5, c5)
            & inside
            & (c5 > c1);
        let bear = is_black(o1, c1)
            & is_white(o2, c2)
            & is_white(o3, c3)
            & is_white(o4, c4)
            & is_black(o5, c5)
            & inside
            & (c5 < c1);
        if !(bull | bear) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if (body(o1, c1) < a)
            | (body(o2, c2) >= 0.5 * a)
            | (body(o3, c3) >= 0.5 * a)
            | (body(o4, c4) >= 0.5 * a)
        {
            return 0.0;
        }
        // The fifth bar is white (or black) here, so its body is a number and this is `body >= a`.
        if body(o5, c5) < a {
            return 0.0;
        }
        if bull { 100.0 } else { -100.0 }
    }
}

kernel! {
    /// Bullish gap variant of rising three methods.
    MatHoldK, 5, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, l3, c3) = (w[2].o, w[2].l, w[2].c);
        let (o4, l4, c4) = (w[3].o, w[3].l, w[3].c);
        let (o5, c5) = (w[4].o, w[4].c);
        let breakout = all_of!(is_white(o5, c5), c5 > c1);
        if any_of!(!is_white(o1, c1), !is_black(o2, c2), o2 <= c1, l3 <= o1, l4 <= o1, !breakout) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if any_of!(
            body(o1, c1) < a,
            body(o2, c2) >= 0.5 * a,
            body(o3, c3) >= 0.5 * a,
            body(o4, c4) >= 0.5 * a,
            // The fifth bar is white, so its body is a number and this is `body >= a`.
            body(o5, c5) < a
        ) {
            return 0.0;
        }
        100.0
    }
}

kernel! {
    /// Inside bar then a false breakout that reverses.
    HikkakeK, 4, avg = false, |w, avg| {
        let (h1, l1) = (w[0].h, w[0].l);
        let (h2, l2) = (w[1].h, w[1].l);
        let (h3, l3) = (w[2].h, w[2].l);
        let c4 = w[3].c;
        let inside = !any_of!(h2 >= h1, l2 <= l1);
        let bull = all_of!(inside, l3 < l2, c4 > h2);
        let bear = all_of!(inside, h3 > h2, c4 < l2);
        if bull {
            100.0
        } else if bear {
            -100.0
        } else {
            0.0
        }
    }
}

kernel! {
    /// Modified five-bar hikkake with a confirming bar.
    HikkakeModK, 5, avg = false, |w, avg| {
        let (h1, l1) = (w[0].h, w[0].l);
        let (h2, l2) = (w[1].h, w[1].l);
        let (h3, l3) = (w[2].h, w[2].l);
        let c4 = w[3].c;
        let c5 = w[4].c;
        let inside = !any_of!(h2 >= h1, l2 <= l1);
        let bull = all_of!(inside, l3 < l2, c4 > h2, c5 > c4);
        let bear = all_of!(inside, h3 > h2, c4 < l2, c5 < c4);
        if bull {
            100.0
        } else if bear {
            -100.0
        } else {
            0.0
        }
    }
}

kernel! {
    /// Five-bar gap run then a reversal closing into the gap.
    BreakawayK, 5, avg = true, |w, avg| {
        let (o1, c1) = (w[0].o, w[0].c);
        let (o2, c2) = (w[1].o, w[1].c);
        let (o3, c3) = (w[2].o, w[2].c);
        let (o4, c4) = (w[3].o, w[3].c);
        let (o5, c5) = (w[4].o, w[4].c);
        let bull = is_black(o1, c1)
            & is_black(o2, c2)
            & is_black(o3, c3)
            & is_black(o4, c4)
            & (c1 > c2)
            & (c2 > c3)
            & (c3 > c4)
            & is_white(o5, c5)
            & (c5 >= o2);
        let bear = is_white(o1, c1)
            & is_white(o2, c2)
            & is_white(o3, c3)
            & is_white(o4, c4)
            & (c1 < c2)
            & (c2 < c3)
            & (c3 < c4)
            & is_black(o5, c5)
            & (c5 <= o2);
        if !(bull | bear) {
            return 0.0;
        }
        let Some(a) = avg.usable() else { return 0.0 };
        if body(o1, c1) >= 0.7 * a {
            if bull { 100.0 } else { -100.0 }
        } else {
            0.0
        }
    }
}

/// Generates the struct over a core (`Core3`, `Core4` or `Core5`), the registry-shaped constructors, `next(&Bar)` and
/// `next_ohlc`, and the `Indicator` impl (`on_bar` and `vectorize` both run the one `push`). Warm-up
/// (`lookback() == 0`, not exact), the `0.0` of the first `K - 1` bars, `reset` and the empty
/// `value()` before the first bar (`NaN`) are what the history-recompute type had.
macro_rules! pattern_stepper {
    ($(#[$m:meta])* $Name:ident, $key:literal, $Core:ident, $Kernel:ty) => {
        $(#[$m])*
        ///
        /// A bounded-state stepper (the last few bars and the ten-bar average body), no history is
        /// kept. Its answer is `+100.0`, `-100.0` or `0.0`, and `0.0` for the first bars.
        #[derive(Clone)]
        pub struct $Name {
            core: $crate::fast_n::$Core<$Kernel>,
            last: f64,
        }
        impl $Name {
            /// The registry defaults (it has no parameters). Private: `Default` is the public spelling.
            #[inline]
            fn defaults() -> Self {
                Self { core: <$crate::fast_n::$Core<$Kernel> as $crate::fast_n::PatPush>::new(), last: f64::NAN }
            }
            /// The registry's constructor: it has no parameters, so `p` is ignored.
            #[inline]
            pub(crate) fn with_params(_p: &[f64]) -> Self {
                Self::defaults()
            }
            /// Feed the next bar; this IS `Indicator::on_bar`, bit for bit, without the Vec.
            #[inline(always)]
            pub fn next(&mut self, bar: &vike_marketdata::Bar) -> f64 {
                self.step(bar)
            }
            /// Feed the next bar's prices as plain numbers, no `Bar` needed; the same answer as
            /// `next` for a bar with those values.
            #[inline(always)]
            pub fn next_ohlc(&mut self, open: f64, high: f64, low: f64, close: f64) -> f64 {
                self.last = $crate::fast_n::PatPush::push(&mut self.core, open, high, low, close);
                self.last
            }
            /// One bar; `next` and `Indicator::on_bar` both run it, so there is one body.
            #[inline(always)]
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
pub(crate) use pattern_stepper;
