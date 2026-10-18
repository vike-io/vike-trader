//! Fast cores for group `b` of the second wave of indicators (see `_followups/perf-conv2-brief.md`):
//! `dema`, `tema`, `t3`, `trix`, `ppo` and `apo`, the six that are chains (or pairs) of exponential
//! moving averages.
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds
//! no `&mut self` call in its hot part, and is used by BOTH the streaming stepper and the
//! whole-series functions, so a stream and a batch agree bit for bit.
//!
//! # The shape every core here has
//!
//! An EMA chain is seeded one stage at a time: stage 1 emits its mean seed at push `n`, stage 2
//! (fed stage 1's emissions) `n - 1` pushes later, and so on, so a chain of `k` stages of period `n`
//! is fully seeded after `k * (n - 1) + 1` pushes. `warm` counts the pushes to go:
//!
//! - `warm == 0` is the **steady state**: one recurrence per stage and nothing else, no seed
//!   ladder, no `Option`, no per-stage count;
//! - `warm > 0` is the **ladder**, inline code over [`Stage`] (count, seed sum, the last values)
//!   that runs for the first few dozen values only: the seed is a naive left-fold sum divided by
//!   `n`, then the textbook recurrence `alpha * x + (1 - alpha) * prev`.
//!
//! # The two-step recurrence (`dema`, `ppo`, `apo`)
//!
//! `alpha * x + keep * prev` is a dependent multiply-then-add per value: about six cycles a value
//! however little else the loop does, and that, not the arithmetic count, was what these indicators
//! cost. The steady state therefore steps by TWO values: with `e[i] = alpha * x[i] + keep * e[i-1]`
//! substituted once,
//!
//! ```text
//! e[i] = (alpha * x[i] + alpha*keep * x[i-1]) + keep^2 * e[i-2]
//! ```
//!
//! whose chain runs through `e[i-2]` only (the even and the odd values are two independent chains),
//! so a value costs half the latency for two more multiplies. It is the same average in exact
//! arithmetic and differs from the textbook form by rounding (a few ulps), which is why the ladder
//! needs ONE push past the last seed: it must leave `e[i-1]`, `e[i-2]` and `x[i-1]` behind.
//! `push` takes one such step and `run` takes two at a time over a slice with the state in
//! registers; both evaluate the same expressions, so they agree bit for bit.
//!
//! `tema`, `trix` and `t3` keep the one-step form (the textbook recurrence, bit for bit what the
//! steppers always computed): they chain three or six stages, so they are limited by the multiplies
//! and by the instructions, not by the latency, and the two-step form needs more of both (measured:
//! `series::tema` 105 us two-step against 95 us one-step, `series::trix` 124 against 95).
//! Their `run` is still one tight loop with the averages in registers once the ladder is done.
//!
//! # NaN
//!
//! A `NaN` close is pushed into the first stage of `dema`, `tema`, `trix`, `ppo` and `apo`, and
//! poisons it for good (in the two-step form both the even and the odd chain), so every later output is `NaN`: the steady
//! state needs no check because `NaN` propagates through the recurrences by itself. `t3`'s first
//! stage DROPS a `NaN` close (the stage does not advance, the output is `NaN`, the next close carries
//! on), so `t3` alone tests its input, once, before touching any state.

/// One exponential-average stage: how many values it has seen, the running sum of the first `n`, the
/// last two averages and the last input (the two-step steady state reads and writes the last three).
#[derive(Clone, Copy)]
struct Stage {
    count: usize,
    seed: f64,
    /// `e[i-1]`.
    p1: f64,
    /// `e[i-2]`.
    p2: f64,
    /// `x[i-1]`.
    xp: f64,
}

impl Stage {
    const NEW: Stage = Stage { count: 0, seed: 0.0, p1: f64::NAN, p2: f64::NAN, xp: f64::NAN };

    /// The ladder: `NaN` for the first `n - 1` inputs, the mean of the first `n` at the `n`-th, then
    /// `alpha * x + keep * prev`; keeps the last values the two-step steady state starts from.
    #[inline(always)]
    fn push(&mut self, n: usize, k: &K, x: f64) -> f64 {
        self.count += 1;
        let e = if self.count < n {
            self.seed += x;
            f64::NAN
        } else if self.count == n {
            self.seed += x;
            self.seed / n as f64
        } else {
            k.a * x + k.keep * self.p1
        };
        self.p2 = self.p1;
        self.p1 = e;
        self.xp = x;
        e
    }

    /// [`Stage::push`] over the DEFINED values of the stage before it: a `NaN` input is dropped (the
    /// stage does not advance and the output is `NaN`), as `math::smooth_defined` does.
    #[inline(always)]
    fn push_defined(&mut self, n: usize, k: &K, x: f64) -> f64 {
        if x.is_nan() { f64::NAN } else { self.push(n, k, x) }
    }

    /// One steady step of the two-step recurrence.
    #[inline(always)]
    fn step(&mut self, k: &K, x: f64) -> f64 {
        let e = (k.a * x + k.ak * self.xp) + k.k2 * self.p2;
        self.p2 = self.p1;
        self.p1 = e;
        self.xp = x;
        e
    }

    /// Two steady steps at once, the same two expressions [`Stage::step`] evaluates.
    #[inline(always)]
    fn step2(&mut self, k: &K, x0: f64, x1: f64) -> (f64, f64) {
        let e0 = (k.a * x0 + k.ak * self.xp) + k.k2 * self.p2;
        let e1 = (k.a * x1 + k.ak * x0) + k.k2 * self.p1;
        self.xp = x1;
        self.p2 = e0;
        self.p1 = e1;
        (e0, e1)
    }
}

/// The constants of a period `n`: `a = 2 / (n + 1)`, `keep = 1 - a`, `ak = a * keep`, `k2 = keep^2`.
#[derive(Clone, Copy)]
struct K {
    a: f64,
    keep: f64,
    ak: f64,
    k2: f64,
}

impl K {
    #[inline(always)]
    fn new(n: usize) -> K {
        let a = 2.0 / (n as f64 + 1.0);
        let keep = 1.0 - a;
        K { a, keep, ak: a * keep, k2: keep * keep }
    }
}

/// Pushes on the ladder of a chain of `k` stages of period `n`: `k * (n - 1) + 1` for every stage to
/// have seeded, plus the one push that leaves the two-step state behind (`extra`). A zero period
/// never seeds, so its chain stays `NaN`.
#[inline(always)]
fn chain_warm(k: usize, n: usize, extra: usize) -> usize {
    k.saturating_mul(n.saturating_sub(1)).saturating_add(1).saturating_add(extra)
}

/// Double exponential moving average: `2 * EMA(close) - EMA(EMA(close))`, both over `n`; `NaN` until
/// both averages have seeded (push `2n - 1`).
#[derive(Clone)]
pub(crate) struct DemaCore {
    n: usize,
    k: K,
    /// Pushes still to go on the seed ladder; 0 is the steady state.
    warm: usize,
    s: [Stage; 2],
}

impl DemaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        DemaCore { n, k: K::new(n), warm: chain_warm(2, n, 1), s: [Stage::NEW; 2] }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        let k = self.k;
        if self.warm == 0 {
            let e1 = self.s[0].step(&k, c);
            let e2 = self.s[1].step(&k, e1);
            return 2.0 * e1 - e2;
        }
        self.warm -= 1;
        let n = self.n;
        let e1 = self.s[0].push(n, &k, c);
        let e2 = self.s[1].push_defined(n, &k, e1);
        2.0 * e1 - e2
    }

    /// `out[i] = push(get(&input[i]))` for the shorter of the two, two values at a time once seeded.
    #[inline(always)]
    pub(crate) fn run<X>(&mut self, input: &[X], get: impl Fn(&X) -> f64, out: &mut [f64]) {
        let len = input.len().min(out.len());
        let (input, out) = (&input[..len], &mut out[..len]);
        let head = self.warm.min(len);
        for (o, x) in out[..head].iter_mut().zip(&input[..head]) {
            *o = self.push(get(x));
        }
        let k = self.k;
        let (mut s0, mut s1) = (self.s[0], self.s[1]);
        let (mut cx, mut co) = (input[head..].chunks_exact(2), out[head..].chunks_exact_mut(2));
        for (x, o) in (&mut cx).zip(&mut co) {
            let (a0, a1) = s0.step2(&k, get(&x[0]), get(&x[1]));
            let (b0, b1) = s1.step2(&k, a0, a1);
            o[0] = 2.0 * a0 - b0;
            o[1] = 2.0 * a1 - b1;
        }
        self.s = [s0, s1];
        for (x, o) in cx.remainder().iter().zip(co.into_remainder()) {
            *o = self.push(get(x));
        }
    }
}

/// Triple exponential moving average: `3 * e1 - 3 * e2 + e3` of three chained EMAs over `n`; `NaN`
/// until all three have seeded (push `3n - 2`). One-step steady state (see the module doc).
#[derive(Clone)]
pub(crate) struct TemaCore {
    n: usize,
    k: K,
    warm: usize,
    s: [Stage; 3],
}

impl TemaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        TemaCore { n, k: K::new(n), warm: chain_warm(3, n, 0), s: [Stage::NEW; 3] }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        let (a, keep) = (self.k.a, self.k.keep);
        if self.warm == 0 {
            let e1 = a * c + keep * self.s[0].p1;
            let e2 = a * e1 + keep * self.s[1].p1;
            let e3 = a * e2 + keep * self.s[2].p1;
            self.s[0].p1 = e1;
            self.s[1].p1 = e2;
            self.s[2].p1 = e3;
            return 3.0 * e1 - 3.0 * e2 + e3;
        }
        self.warm -= 1;
        let (n, k) = (self.n, self.k);
        let e1 = self.s[0].push(n, &k, c);
        let e2 = self.s[1].push_defined(n, &k, e1);
        let e3 = self.s[2].push_defined(n, &k, e2);
        3.0 * e1 - 3.0 * e2 + e3
    }

    /// See [`DemaCore::run`]: the seeded part is one loop with the three averages in registers.
    #[inline(always)]
    pub(crate) fn run<X>(&mut self, input: &[X], get: impl Fn(&X) -> f64, out: &mut [f64]) {
        let len = input.len().min(out.len());
        let (input, out) = (&input[..len], &mut out[..len]);
        let head = self.warm.min(len);
        for (o, x) in out[..head].iter_mut().zip(&input[..head]) {
            *o = self.push(get(x));
        }
        let (a, keep) = (self.k.a, self.k.keep);
        let (mut p0, mut p1, mut p2) = (self.s[0].p1, self.s[1].p1, self.s[2].p1);
        for (o, x) in out[head..].iter_mut().zip(&input[head..]) {
            let e1 = a * get(x) + keep * p0;
            let e2 = a * e1 + keep * p1;
            let e3 = a * e2 + keep * p2;
            p0 = e1;
            p1 = e2;
            p2 = e3;
            *o = 3.0 * e1 - 3.0 * e2 + e3;
        }
        (self.s[0].p1, self.s[1].p1, self.s[2].p1) = (p0, p1, p2);
    }
}

/// TRIX: the one-bar percentage change of a triple-smoothed EMA, `(e3 - prev) / prev * 100`, `NaN`
/// where the previous `e3` is exactly zero. The first `e3` has no previous one, so the first value is
/// push `3n - 1`. One-step steady state (see the module doc).
#[derive(Clone)]
pub(crate) struct TrixCore {
    n: usize,
    k: K,
    warm: usize,
    s: [Stage; 3],
}

/// `(e3 - p) / p * 100`, `NaN` for a zero (or `NaN`) previous value.
#[inline(always)]
fn pct_change(e3: f64, p: f64) -> f64 {
    if p != 0.0 { (e3 - p) / p * 100.0 } else { f64::NAN }
}

impl TrixCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        TrixCore { n, k: K::new(n), warm: chain_warm(3, n, 0), s: [Stage::NEW; 3] }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        let (a, keep) = (self.k.a, self.k.keep);
        if self.warm == 0 {
            let e1 = a * c + keep * self.s[0].p1;
            let e2 = a * e1 + keep * self.s[1].p1;
            let p = self.s[2].p1;
            let e3 = a * e2 + keep * p;
            self.s[0].p1 = e1;
            self.s[1].p1 = e2;
            self.s[2].p1 = e3;
            return pct_change(e3, p);
        }
        self.warm -= 1;
        let (n, k) = (self.n, self.k);
        let e1 = self.s[0].push(n, &k, c);
        let e2 = self.s[1].push_defined(n, &k, e1);
        // The previous `e3` is `NaN` until it has seeded, so the push that seeds it gives `NaN`.
        let p = self.s[2].p1;
        let e3 = self.s[2].push_defined(n, &k, e2);
        pct_change(e3, p)
    }

    /// See [`TemaCore::run`].
    #[inline(always)]
    pub(crate) fn run<X>(&mut self, input: &[X], get: impl Fn(&X) -> f64, out: &mut [f64]) {
        let len = input.len().min(out.len());
        let (input, out) = (&input[..len], &mut out[..len]);
        let head = self.warm.min(len);
        for (o, x) in out[..head].iter_mut().zip(&input[..head]) {
            *o = self.push(get(x));
        }
        let (a, keep) = (self.k.a, self.k.keep);
        let (mut p0, mut p1, mut p2) = (self.s[0].p1, self.s[1].p1, self.s[2].p1);
        for (o, x) in out[head..].iter_mut().zip(&input[head..]) {
            let e1 = a * get(x) + keep * p0;
            let e2 = a * e1 + keep * p1;
            let e3 = a * e2 + keep * p2;
            *o = pct_change(e3, p2);
            p0 = e1;
            p1 = e2;
            p2 = e3;
        }
        (self.s[0].p1, self.s[1].p1, self.s[2].p1) = (p0, p1, p2);
    }
}

/// Tillson T3: three chained "generalized DEMA" passes `(1 + v) * e1 - v * e2` (each `e1`, `e2` an EMA
/// over `n` of the defined values before it); `NaN` until the last pass has seeded (push `6n - 5`).
/// A `NaN` close is dropped, not pushed: the output is `NaN` and no stage advances. One-step steady
/// state (see the module doc).
#[derive(Clone)]
pub(crate) struct T3Core {
    n: usize,
    v: f64,
    /// `1 + v`, computed once (the same bits the stepper computed per bar).
    vp1: f64,
    k: K,
    warm: usize,
    s: [Stage; 6],
}

impl T3Core {
    #[inline]
    pub(crate) fn new(n: usize, v: f64) -> Self {
        T3Core { n, v, vp1: 1.0 + v, k: K::new(n), warm: chain_warm(6, n, 0), s: [Stage::NEW; 6] }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        if c.is_nan() {
            return f64::NAN;
        }
        let (a, keep, v, vp1) = (self.k.a, self.k.keep, self.v, self.vp1);
        if self.warm == 0 {
            let e1 = a * c + keep * self.s[0].p1;
            let e2 = a * e1 + keep * self.s[1].p1;
            let g1 = vp1 * e1 - v * e2;
            let e3 = a * g1 + keep * self.s[2].p1;
            let e4 = a * e3 + keep * self.s[3].p1;
            let g2 = vp1 * e3 - v * e4;
            let e5 = a * g2 + keep * self.s[4].p1;
            let e6 = a * e5 + keep * self.s[5].p1;
            self.s[0].p1 = e1;
            self.s[1].p1 = e2;
            self.s[2].p1 = e3;
            self.s[3].p1 = e4;
            self.s[4].p1 = e5;
            self.s[5].p1 = e6;
            return vp1 * e5 - v * e6;
        }
        self.warm -= 1;
        let (n, k) = (self.n, self.k);
        let e1 = self.s[0].push(n, &k, c);
        let e2 = self.s[1].push_defined(n, &k, e1);
        let g1 = vp1 * e1 - v * e2;
        let e3 = self.s[2].push_defined(n, &k, g1);
        let e4 = self.s[3].push_defined(n, &k, e3);
        let g2 = vp1 * e3 - v * e4;
        let e5 = self.s[4].push_defined(n, &k, g2);
        let e6 = self.s[5].push_defined(n, &k, e5);
        vp1 * e5 - v * e6
    }

    /// See [`TemaCore::run`]; a `NaN` input gives a `NaN` output and moves no average.
    #[inline(always)]
    pub(crate) fn run<X>(&mut self, input: &[X], get: impl Fn(&X) -> f64, out: &mut [f64]) {
        let len = input.len().min(out.len());
        let (input, out) = (&input[..len], &mut out[..len]);
        let mut i = 0;
        // The ladder, whose length counts only the values that are not `NaN`.
        while self.warm != 0 && i < len {
            out[i] = self.push(get(&input[i]));
            i += 1;
        }
        let (a, keep, v, vp1) = (self.k.a, self.k.keep, self.v, self.vp1);
        let mut p =
            [self.s[0].p1, self.s[1].p1, self.s[2].p1, self.s[3].p1, self.s[4].p1, self.s[5].p1];
        for (o, x) in out[i..].iter_mut().zip(&input[i..]) {
            let c = get(x);
            if c.is_nan() {
                *o = f64::NAN;
                continue;
            }
            let e1 = a * c + keep * p[0];
            let e2 = a * e1 + keep * p[1];
            let g1 = vp1 * e1 - v * e2;
            let e3 = a * g1 + keep * p[2];
            let e4 = a * e3 + keep * p[3];
            let g2 = vp1 * e3 - v * e4;
            let e5 = a * g2 + keep * p[4];
            let e6 = a * e5 + keep * p[5];
            p = [e1, e2, e3, e4, e5, e6];
            *o = vp1 * e5 - v * e6;
        }
        for (st, p) in self.s.iter_mut().zip(p) {
            st.p1 = p;
        }
    }
}

/// The two EMAs of the close that `ppo` and `apo` subtract, `EMA(fast) - EMA(slow)`; with `PCT` the
/// difference is taken as a percentage of the slow one, `(ef - es) / es * 100`, `NaN` where `es` is
/// exactly zero. `NaN` until both have seeded (push `max(fast, slow)`); both advance on every push.
#[derive(Clone)]
pub(crate) struct OscCore<const PCT: bool> {
    nf: usize,
    ns: usize,
    kf: K,
    ks: K,
    warm: usize,
    f: Stage,
    s: Stage,
}

/// Percentage price oscillator.
pub(crate) type PpoCore = OscCore<true>;
/// Absolute price oscillator.
pub(crate) type ApoCore = OscCore<false>;

impl<const PCT: bool> OscCore<PCT> {
    #[inline]
    pub(crate) fn new(nf: usize, ns: usize) -> Self {
        OscCore {
            nf,
            ns,
            kf: K::new(nf),
            ks: K::new(ns),
            warm: nf.max(ns) + 1,
            f: Stage::NEW,
            s: Stage::NEW,
        }
    }

    #[inline(always)]
    fn out(ef: f64, es: f64) -> f64 {
        if PCT { if es != 0.0 { (ef - es) / es * 100.0 } else { f64::NAN } } else { ef - es }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        let (kf, ks) = (self.kf, self.ks);
        if self.warm == 0 {
            let ef = self.f.step(&kf, c);
            let es = self.s.step(&ks, c);
            return Self::out(ef, es);
        }
        self.warm -= 1;
        let ef = self.f.push(self.nf, &kf, c);
        let es = self.s.push(self.ns, &ks, c);
        Self::out(ef, es)
    }

    /// See [`DemaCore::run`].
    #[inline(always)]
    pub(crate) fn run<X>(&mut self, input: &[X], get: impl Fn(&X) -> f64, out: &mut [f64]) {
        let len = input.len().min(out.len());
        let (input, out) = (&input[..len], &mut out[..len]);
        let head = self.warm.min(len);
        for (o, x) in out[..head].iter_mut().zip(&input[..head]) {
            *o = self.push(get(x));
        }
        let (kf, ks) = (self.kf, self.ks);
        let (mut f, mut s) = (self.f, self.s);
        let (mut cx, mut co) = (input[head..].chunks_exact(2), out[head..].chunks_exact_mut(2));
        for (x, o) in (&mut cx).zip(&mut co) {
            let (x0, x1) = (get(&x[0]), get(&x[1]));
            let (f0, f1) = f.step2(&kf, x0, x1);
            let (s0, s1) = s.step2(&ks, x0, x1);
            o[0] = Self::out(f0, s0);
            o[1] = Self::out(f1, s1);
        }
        self.f = f;
        self.s = s;
        for (x, o) in cx.remainder().iter().zip(co.into_remainder()) {
            *o = self.push(get(x));
        }
    }
}
