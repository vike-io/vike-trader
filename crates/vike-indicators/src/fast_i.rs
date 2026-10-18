//! Fast cores for group `i` of the third wave of indicators (see `_followups/perf-conv3-brief.md`):
//! `zlema`, `coppock`, `tsi`, `smi_ergodic`, `mass`, `kst`, `qstick`, `rwi`, `acceleration_bands` and
//! `ttm_squeeze`.
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds no
//! `&mut self` call in its hot part, and is used by BOTH the streaming stepper and the whole-series
//! functions, so a stream and a batch agree bit for bit.
//!
//! # The shapes
//!
//! * **EMA chains** (`zlema`, `tsi`, `smi_ergodic`, `mass`) follow `crate::fast_b`: a `warm` counter,
//!   a ladder over [`Stage`] (the mean seed, then the textbook recurrence) for the first few dozen
//!   values, and a branch-free steady state of one recurrence per stage. `zlema` is one long
//!   dependent chain, so it takes the two-step recurrence of `fast_b` (see its module doc);
//!   `tsi`, `smi_ergodic` and `mass` run two or four independent chains and keep the one-step form.
//! * **Rings with running sums** (`coppock`, `kst`, `qstick`, `rwi`, `acceleration_bands`,
//!   `ttm_squeeze`) follow `crate::roll::SmaCore`: the ring starts as zeros so the carried sums are
//!   right from the first value, a multiplier that is `NaN` until the first lap of the ring ends
//!   gates the warm-up at no cost per value, and a `NaN` RESULT takes a cold rebuild from the ring,
//!   which makes the output `NaN` while a `NaN` is inside the window and exact again the update after
//!   it left.
//! * **Closes `k` back** (`coppock`, `kst`) are read from a ring that starts as `NaN`: a rate of
//!   change whose reference bar does not exist yet is `NaN` by arithmetic, with no warm-up branch.
//!
//! # NaN
//!
//! The stages of a chain that read the stage before them skip its `NaN` (the stage does not advance),
//! as `math::smooth_defined` always did; the first stage of `zlema`, `tsi`, `smi_ergodic` and `mass`
//! reads the raw input, so a `NaN` close is pushed into it and poisons the chain for good (every
//! later output is `NaN`), exactly as before. `coppock` and `kst` DROP a rate of change that is
//! `NaN` (a `NaN` close, or a zero reference close): the output at that bar is `NaN` and the
//! averages carry on over the values that exist.

use crate::fast_a::HiLo;
use crate::roll::{PEAK_SHARE, PeakSmaCore, RING_CAP, SmaCore};

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const REBUILD: usize = 512;

/// The sums of the TTM momentum's regression are rebuilt at once when the largest `|delta|` they have carried
/// since their last rebuild is more than this many times the high-low range of the window now: a delta is
/// never wider than that range, so a bigger one is a bad tick that has left, and its rounding error stays
/// in the sums (about `eps * n * peak`) until the scheduled rebuild.
const TTM_PEAK: f64 = 1.0e3;

/// Laps of a ring of `n` values between two rebuilds of its sums.
#[inline]
fn laps_per(n: usize) -> usize {
    REBUILD.div_ceil(n.max(1)).max(1)
}

// ================================ EMA machinery ================================

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
/// have seeded, plus `extra` pushes (the one that leaves the two-step state behind). A zero period
/// never seeds, so its chain stays `NaN`.
#[inline(always)]
fn chain_warm(k: usize, n: usize, extra: usize) -> usize {
    k.saturating_mul(n.saturating_sub(1)).saturating_add(1).saturating_add(extra)
}

// =================================== ZLEMA =====================================

/// Zero-lag exponential moving average: an EMA of `close + (close - close[lag])` with
/// `lag = (n - 1) / 2` (the plain close until `lag + 1` closes exist). `NaN` until the average has
/// seeded (push `n`). The recurrence is the two-step form of `fast_b` (one chain, so the latency of
/// `alpha * x + keep * prev` is what it costs).
///
/// The closes live in a ring of `lag + 1`: after a push its oldest slot, the one the NEXT push
/// overwrites, is `close[i - lag]`.
#[derive(Clone)]
pub(crate) struct ZlemaCore {
    n: usize,
    lag: usize,
    k: K,
    /// Pushes still to go on the seed ladder (the average seeds at push `n`, and one more push
    /// leaves the two-step state behind); 0 is the steady state.
    warm: usize,
    /// Closes seen on the ladder (the de-lagging term exists once this exceeds `lag`).
    seen: usize,
    st: Stage,
    buf: Vec<f64>,
    head: usize,
}

impl ZlemaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let lag = n.saturating_sub(1) / 2;
        ZlemaCore {
            n,
            lag,
            k: K::new(n),
            warm: chain_warm(1, n, 1),
            seen: 0,
            st: Stage::NEW,
            buf: vec![0.0; lag + 1],
            head: 0,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        self.buf[self.head] = c;
        self.head += 1;
        if self.head == self.buf.len() {
            self.head = 0;
        }
        // The slot just ahead of the write is the oldest: `close[i - lag]` once the ring is full.
        let old = self.buf[self.head];
        let k = self.k;
        if self.warm == 0 {
            return self.st.step(&k, c + (c - old));
        }
        self.warm -= 1;
        self.seen += 1;
        let x = if self.seen > self.lag { c + (c - old) } else { c };
        self.st.push(self.n, &k, x)
    }

    /// `out[i] = push(get(&input[i]))` for the shorter of the two, two values at a time once seeded,
    /// the de-lagged values read straight off the input. Starts from a FRESH core: it leaves the
    /// ring behind (the series functions drop the core after it).
    #[inline(always)]
    pub(crate) fn run<X>(&mut self, input: &[X], get: impl Fn(&X) -> f64, out: &mut [f64]) {
        let len = input.len().min(out.len());
        let (input, out) = (&input[..len], &mut out[..len]);
        let head = self.warm.min(len);
        for (o, x) in out[..head].iter_mut().zip(&input[..head]) {
            *o = self.push(get(x));
        }
        let (k, lag) = (self.k, self.lag);
        let mut st = self.st;
        // Past the ladder `i >= n + 1 > lag`, so the closes `lag` back are inside the input.
        let delagged = |i: usize| {
            let c = get(&input[i]);
            c + (c - get(&input[i - lag]))
        };
        let mut i = head;
        while i + 1 < len {
            let (e0, e1) = st.step2(&k, delagged(i), delagged(i + 1));
            out[i] = e0;
            out[i + 1] = e1;
            i += 2;
        }
        if i < len {
            out[i] = st.step(&k, delagged(i));
        }
        self.st = st;
    }
}

// ============================== TSI and SMI ergodic ============================

/// `100 * d / a`, `NaN` where the smoothed absolute change is exactly zero.
#[inline(always)]
fn ratio100(d: f64, a: f64) -> f64 {
    if a != 0.0 { 100.0 * d / a } else { f64::NAN }
}

/// True strength index: `100 * EMA_short(EMA_long(delta)) / EMA_short(EMA_long(|delta|))` of the
/// close-to-close change (0 for the first bar). Four averages, two independent chains of two; `NaN`
/// until both second averages have seeded (push `long + short - 1`), and wherever the smoothed
/// absolute change is exactly zero. A `NaN` close poisons it for good.
#[derive(Clone)]
pub(crate) struct TsiCore {
    nl: usize,
    ns: usize,
    kl: K,
    ks: K,
    /// Pushes still to go on the seed ladder; 0 is the steady state. Never 0 on the first push, which
    /// is the one whose change is 0.
    warm: usize,
    started: bool,
    prev: f64,
    /// `d1`, `a1` (long averages of the change and its absolute value), then `d2`, `a2`.
    s: [Stage; 4],
}

impl TsiCore {
    #[inline]
    pub(crate) fn new(nl: usize, ns: usize) -> Self {
        TsiCore {
            nl,
            ns,
            kl: K::new(nl),
            ks: K::new(ns),
            warm: nl.saturating_add(ns).saturating_sub(1).max(1),
            started: false,
            prev: f64::NAN,
            s: [Stage::NEW; 4],
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        if self.warm == 0 {
            let d = c - self.prev;
            self.prev = c;
            let (la, lk, sa, sk) = (self.kl.a, self.kl.keep, self.ks.a, self.ks.keep);
            let e1 = la * d + lk * self.s[0].p1;
            let e2 = la * d.abs() + lk * self.s[1].p1;
            let e3 = sa * e1 + sk * self.s[2].p1;
            let e4 = sa * e2 + sk * self.s[3].p1;
            self.s[0].p1 = e1;
            self.s[1].p1 = e2;
            self.s[2].p1 = e3;
            self.s[3].p1 = e4;
            return ratio100(e3, e4);
        }
        self.warm -= 1;
        let d = if self.started { c - self.prev } else { 0.0 };
        self.started = true;
        self.prev = c;
        let (nl, ns, kl, ks) = (self.nl, self.ns, self.kl, self.ks);
        let e1 = self.s[0].push(nl, &kl, d);
        let e2 = self.s[1].push(nl, &kl, d.abs());
        let e3 = self.s[2].push_defined(ns, &ks, e1);
        let e4 = self.s[3].push_defined(ns, &ks, e2);
        ratio100(e3, e4)
    }
}

/// SMI ergodic oscillator: the [`TsiCore`] ratio (`smi`), and an EMA of its DEFINED values over
/// `signal` (`signal`, seeded with the mean of the first `signal` of them). `[smi, signal]`.
#[derive(Clone)]
pub(crate) struct SmiCore {
    t: TsiCore,
    nsig: usize,
    ksig: K,
    /// Defined `smi` values still to go on the signal's seed ladder; 0 is its steady state.
    sw: usize,
    sig: Stage,
}

impl SmiCore {
    #[inline]
    pub(crate) fn new(nl: usize, ns: usize, nsig: usize) -> Self {
        SmiCore { t: TsiCore::new(nl, ns), nsig, ksig: K::new(nsig), sw: nsig, sig: Stage::NEW }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> [f64; 2] {
        let smi = self.t.push(c);
        let sig = if smi.is_nan() {
            f64::NAN
        } else if self.sw == 0 {
            let s = self.ksig.a * smi + self.ksig.keep * self.sig.p1;
            self.sig.p1 = s;
            s
        } else {
            self.sw -= 1;
            self.sig.push(self.nsig, &self.ksig, smi)
        };
        [smi, sig]
    }
}

// =================================== MASS ======================================

/// Mass index: the sum over `np` values of `EMA(range) / EMA(EMA(range))` (the range is
/// `high - low`, both averages over `ne`). The ratio is `NaN` until the second average has seeded
/// (push `2 * ne - 1`) and where it is exactly zero; the sum is `NaN` until `np` ratios exist and
/// while a `NaN` ratio is in its window.
///
/// The ratios go into a [`SmaCore`] (whose mean times `np` is the sum); a ratio that is `NaN` on the
/// seed ladder is not fed, a later one is, so the window shows it.
#[derive(Clone)]
pub(crate) struct MassCore {
    ne: usize,
    k: K,
    warm: usize,
    s: [Stage; 2],
    win: SmaCore,
    npf: f64,
}

impl MassCore {
    #[inline]
    pub(crate) fn new(np: usize, ne: usize) -> Self {
        MassCore {
            ne,
            k: K::new(ne),
            warm: chain_warm(2, ne, 0),
            s: [Stage::NEW; 2],
            win: SmaCore::new(np),
            npf: np as f64,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> f64 {
        let hl = high - low;
        let ratio;
        if self.warm == 0 {
            let (a, keep) = (self.k.a, self.k.keep);
            let e1 = a * hl + keep * self.s[0].p1;
            let e2 = a * e1 + keep * self.s[1].p1;
            self.s[0].p1 = e1;
            self.s[1].p1 = e2;
            ratio = if e2 != 0.0 { e1 / e2 } else { f64::NAN };
        } else {
            self.warm -= 1;
            let (ne, k) = (self.ne, self.k);
            let e1 = self.s[0].push(ne, &k, hl);
            let e2 = self.s[1].push_defined(ne, &k, e1);
            ratio = if e2 != 0.0 { e1 / e2 } else { f64::NAN };
            if ratio.is_nan() {
                return f64::NAN;
            }
        }
        self.win.push(ratio) * self.npf
    }
}

// =================================== COPPOCK ===================================

/// A ring slot `k` pushes back from the newest, given the ring's write position AFTER the push
/// (`k = 0` is the value just written). The ring is `r = max reach + 1` long.
#[inline(always)]
fn back(pos: usize, k: usize, r: usize) -> usize {
    if pos > k { pos - 1 - k } else { pos + r - 1 - k }
}

/// Coppock curve: a linearly weighted average over `nw` values of `ROC(long) + ROC(short)`, where
/// `ROC(k) = (close / close[k back] - 1) * 100`. A rate is `NaN` while its reference close does not
/// exist or is exactly zero; a bar where either is `NaN` gives `NaN` and is DROPPED from the average
/// (it keeps its last `nw` defined sums). `NaN` until `nw` sums exist.
///
/// The weighted sum is carried (`W' = W - S + nw * x`, `S' = S - old + x`) and rebuilt from the ring
/// every `ceil(512 / nw)` laps, like `fast_a`'s weighted average.
#[derive(Clone)]
pub(crate) struct CoppockCore {
    nl: usize,
    ns: usize,
    /// The closes, `NaN` until written.
    buf: Vec<f64>,
    pos: usize,
    wbuf: Vec<f64>,
    whead: usize,
    laps: usize,
    laps_per: usize,
    s: f64,
    w: f64,
    nf: f64,
    /// `NaN` until the first lap of the weighted ring ends, then `inv0`.
    inv: f64,
    inv0: f64,
}

impl CoppockCore {
    #[inline]
    pub(crate) fn new(nw: usize, nl: usize, ns: usize) -> Self {
        CoppockCore {
            nl,
            ns,
            buf: vec![f64::NAN; nl.max(ns).saturating_add(1)],
            pos: 0,
            wbuf: vec![0.0; nw.clamp(1, RING_CAP)],
            whead: 0,
            laps: 0,
            laps_per: laps_per(nw),
            s: 0.0,
            w: 0.0,
            nf: nw as f64,
            inv: f64::NAN,
            // A window of 0 never leaves warm-up: every output is NaN.
            inv0: if nw == 0 { f64::NAN } else { 1.0 / (nw as f64 * (nw as f64 + 1.0) / 2.0) },
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        let r = self.buf.len();
        self.buf[self.pos] = c;
        self.pos += 1;
        if self.pos == r {
            self.pos = 0;
        }
        let pl = self.buf[back(self.pos, self.nl, r)];
        let ps = self.buf[back(self.pos, self.ns, r)];
        let rl = if pl != 0.0 { (c / pl - 1.0) * 100.0 } else { f64::NAN };
        let rs = if ps != 0.0 { (c / ps - 1.0) * 100.0 } else { f64::NAN };
        let comb = rl + rs;
        if comb.is_nan() {
            return f64::NAN;
        }
        let old = std::mem::replace(&mut self.wbuf[self.whead], comb);
        self.whead += 1;
        self.w = (self.w - self.s) + self.nf * comb;
        self.s = self.s - old + comb;
        if self.whead == self.wbuf.len() {
            self.whead = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                // The head is 0, so the ring is already oldest-to-newest.
                let (s, w) = weighted_sums(&self.wbuf, 0);
                self.s = s;
                self.w = w;
                self.laps = 0;
            }
            self.inv = self.inv0;
        }
        let mut out = self.w * self.inv;
        if out.is_nan() {
            if self.inv.is_nan() {
                return f64::NAN;
            }
            let (s, w) = weighted_sums(&self.wbuf, self.whead);
            self.s = s;
            self.w = w;
            out = w * self.inv;
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

// ===================================== KST =====================================

/// Know Sure Thing: four rates of change `(close - close[k back]) / close[k back] * 100`, each
/// averaged over its own simple window of DEFINED rates, then `1*r1 + 2*r2 + 3*r3 + 4*r4`, and the
/// simple average of that over `signal` defined values. `[kst, signal]`.
///
/// The ring keeps each close beside its reciprocal (`NaN` for a zero close), so a rate costs a
/// subtract and two multiplies and a bar costs ONE division however many rates there are. A rate
/// that is `NaN` (no reference close yet, a zero reference, a `NaN` close) is dropped: its average
/// does not advance and its output is `NaN`.
#[derive(Clone)]
pub(crate) struct KstCore {
    roc: [usize; 4],
    /// `(close, 1 / close)`, `NaN` until written.
    buf: Vec<[f64; 2]>,
    pos: usize,
    sma: [SmaCore; 4],
    sig: SmaCore,
}

impl KstCore {
    #[inline]
    pub(crate) fn new(roc: [usize; 4], sma: [usize; 4], sig: usize) -> Self {
        let reach = roc[0].max(roc[1]).max(roc[2]).max(roc[3]);
        KstCore {
            roc,
            buf: vec![[f64::NAN; 2]; reach.saturating_add(1)],
            pos: 0,
            sma: [
                SmaCore::new(sma[0]),
                SmaCore::new(sma[1]),
                SmaCore::new(sma[2]),
                SmaCore::new(sma[3]),
            ],
            sig: SmaCore::new(sig),
        }
    }

    /// The average of one component, advanced only by a defined rate.
    #[inline(always)]
    fn stage(sma: &mut SmaCore, rate: f64) -> f64 {
        if rate.is_nan() { f64::NAN } else { sma.push(rate) }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> [f64; 2] {
        let r = self.buf.len();
        self.buf[self.pos] = [c, if c != 0.0 { 1.0 / c } else { f64::NAN }];
        self.pos += 1;
        if self.pos == r {
            self.pos = 0;
        }
        let pos = self.pos;
        let [a0, a1, a2, a3] = self.roc;
        let [p0, i0] = self.buf[back(pos, a0, r)];
        let [p1, i1] = self.buf[back(pos, a1, r)];
        let [p2, i2] = self.buf[back(pos, a2, r)];
        let [p3, i3] = self.buf[back(pos, a3, r)];
        let r0 = Self::stage(&mut self.sma[0], (c - p0) * i0 * 100.0);
        let r1 = Self::stage(&mut self.sma[1], (c - p1) * i1 * 100.0);
        let r2 = Self::stage(&mut self.sma[2], (c - p2) * i2 * 100.0);
        let r3 = Self::stage(&mut self.sma[3], (c - p3) * i3 * 100.0);
        let kst = 1.0 * r0 + 2.0 * r1 + 3.0 * r2 + 4.0 * r3;
        let sig = if kst.is_nan() { f64::NAN } else { self.sig.push(kst) };
        [kst, sig]
    }
}

// =================================== QSTICK ====================================

/// Qstick: the simple average of `close - open` over `n` bars. `NaN` for the first `n - 1`, while a
/// `NaN` is in the window, and for a zero window.
#[derive(Clone)]
pub(crate) struct QstickCore {
    sma: PeakSmaCore,
}

impl QstickCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        QstickCore { sma: PeakSmaCore::new(n) }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, open: f64, close: f64) -> f64 {
        self.sma.push(close - open)
    }
}

// =============================== ACCELERATION BANDS ============================

/// Headley's acceleration bands: with `w = (high - low) / (high + low)` (0 when the sum is 0), the
/// simple averages over `n` of `high * (1 + factor * w)`, of the close and of `low * (1 - factor * w)`.
/// `[upper, mid, lower]`, each `NaN` for the first `n - 1` and while a `NaN` is in ITS window.
#[derive(Clone)]
pub(crate) struct AccelCore {
    factor: f64,
    upper: SmaCore,
    mid: SmaCore,
    lower: SmaCore,
}

impl AccelCore {
    #[inline]
    pub(crate) fn new(n: usize, factor: f64) -> Self {
        AccelCore { factor, upper: SmaCore::new(n), mid: SmaCore::new(n), lower: SmaCore::new(n) }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> [f64; 3] {
        let total = high + low;
        let spread = if total == 0.0 { 0.0 } else { (high - low) / total };
        [
            self.upper.push(high * (1.0 + self.factor * spread)),
            self.mid.push(close),
            self.lower.push(low * (1.0 - self.factor * spread)),
        ]
    }
}

// ===================================== RWI =====================================

/// The gap-aware true range of a bar against the previous close, the three-way maximum with
/// `f64::max` (which answers the other operand for a `NaN`). A `NaN` previous close (the first bar)
/// leaves `high - low`.
#[inline(always)]
fn true_range3(high: f64, low: f64, prev_close: f64) -> f64 {
    (high - low).max((high - prev_close).abs()).max((low - prev_close).abs())
}

/// Random walk index: `[(high - low[n back]) / D, (high[n back] - low) / D]` with
/// `D = ATR * sqrt(n)`, `ATR` the plain average of the last `n` true ranges. `NaN` for the first `n`
/// bars (it needs `n + 1`), and where `D` is not positive.
///
/// The true ranges go through a ring with a running sum (as [`SmaCore`]); the highs and lows `n`
/// back sit in a ring of `n + 1` pairs written once per bar, whose oldest slot is the bar `n` back.
#[derive(Clone)]
pub(crate) struct RwiCore {
    inv_n: f64,
    /// `NaN` until the true-range ring's first lap ends.
    inv: f64,
    root0: f64,
    /// `NaN` until the pair ring's first lap ends (one bar after `inv` is set).
    root: f64,
    prev_close: f64,
    tr: Vec<f64>,
    th: usize,
    laps: usize,
    laps_per: usize,
    sum: f64,
    /// The largest `sum` since the last rebuild.
    peak: f64,
    hl: Vec<[f64; 2]>,
    hh: usize,
}

impl RwiCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let m = n.clamp(1, RING_CAP);
        RwiCore {
            inv_n: if n == 0 { f64::NAN } else { 1.0 / n as f64 },
            inv: f64::NAN,
            root0: if n == 0 { f64::NAN } else { (n as f64).sqrt() },
            root: f64::NAN,
            prev_close: f64::NAN,
            tr: vec![0.0; m],
            th: 0,
            laps: 0,
            laps_per: laps_per(n),
            sum: 0.0,
            peak: 0.0,
            hl: vec![[0.0; 2]; m + 1],
            hh: 0,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        let tr = true_range3(high, low, self.prev_close);
        self.prev_close = close;
        let old = std::mem::replace(&mut self.tr[self.th], tr);
        self.th += 1;
        self.sum += tr - old;
        self.peak = if self.sum > self.peak { self.sum } else { self.peak };
        if self.th == self.tr.len() {
            self.th = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                // The head is 0, so the ring is already oldest-to-newest.
                self.sum = self.tr.iter().fold(0.0, |a, &v| a + v);
                self.peak = self.sum;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        self.hl[self.hh] = [high, low];
        self.hh += 1;
        if self.hh == self.hl.len() {
            self.hh = 0;
            self.root = self.root0;
        }
        // The slot ahead of the write is the oldest: the bar `n` back once the ring is full.
        let [high_back, low_back] = self.hl[self.hh];
        let mut atr = self.sum * self.inv;
        // Joined with `|`: a sum far under its peak carries the rounding error of a bad tick that has left.
        if atr.is_nan() | (self.sum < self.peak * PEAK_SHARE) {
            // Warming up (or a window of 0), a `NaN` in the window, or a stale carried sum. A free
            // function over a shared slice, not a `&mut self` method (see `SmaCore::push`).
            if self.inv.is_nan() {
                return [f64::NAN; 2];
            }
            self.sum = ring_sum(&self.tr, self.th);
            self.peak = self.sum;
            atr = self.sum * self.inv;
            if atr.is_nan() {
                return [f64::NAN; 2];
            }
        }
        let denom = atr * self.root;
        if denom > 0.0 {
            let r = 1.0 / denom;
            [(high - low_back) * r, (high_back - low) * r]
        } else {
            [f64::NAN; 2]
        }
    }
}

/// The ring summed oldest to newest (the oldest value is at `head`).
#[cold]
#[inline(never)]
fn ring_sum(buf: &[f64], head: usize) -> f64 {
    buf[head..].iter().chain(&buf[..head]).fold(0.0, |a, &v| a + v)
}

// ================================== TTM SQUEEZE ================================

/// TTM Squeeze over `n` bars: `[momentum, squeeze]`.
///
/// With `mid` the simple mean of the closes, `dev` their population standard deviation (exactly 0
/// for a window of bit-equal closes), and `range` the simple mean of the true ranges:
/// `squeeze = 1` when `mid -/+ bb_mult * dev` lie strictly inside `mid -/+ kc_mult * range` (and `0`
/// otherwise, a `NaN` window included, since a comparison with `NaN` is false). The momentum is the
/// value at the newest point of the least-squares line through the last `n` values of
/// `close - ((highest high + lowest low) / 2 + mid) / 2`.
///
/// The closes and true ranges share one ring of pairs and one lap counter; the mean and variance are
/// running sums on `close - shift` (see [`crate::roll::BollingerCore`]) with the re-centring rule of
/// `fast_d2`'s standard deviation, so a flat window is exactly flat; the extremes are
/// [`HiLo`]; the regression carries `sum y` and `sum i * y` of the delta ring. `squeeze` exists from
/// bar `n`, `momentum` from bar `2n - 1`.
#[derive(Clone)]
pub(crate) struct TtmCore {
    bbm: f64,
    kcm: f64,
    inv_n: f64,
    /// `NaN` until the first lap of the ring ends (always `NaN` for `n < 2`).
    inv: f64,
    prev_close: f64,
    /// `(close, true range)`.
    buf: Vec<[f64; 2]>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    s1: f64,
    s2: f64,
    smax: f64,
    t: f64,
    win: HiLo,
    dbuf: Vec<f64>,
    dhead: usize,
    dlaps: usize,
    /// `NaN` until the first lap of the delta ring ends, then `1 / n`.
    dinv: f64,
    sy: f64,
    sxy: f64,
    /// The largest `|delta|` since the delta sums were last rebuilt.
    dpeak: f64,
    nf: f64,
    nm1: f64,
    sx: f64,
    inv_den: f64,
    half: f64,
}

impl TtmCore {
    #[inline]
    pub(crate) fn new(n: usize, bb_mult: f64, kc_mult: f64) -> Self {
        let m = n.clamp(1, RING_CAP);
        let nf = n as f64;
        let n2 = (n as u128) * (n as u128);
        TtmCore {
            bbm: bb_mult,
            kcm: kc_mult,
            // The kernel answers all-NaN for a window under 2.
            inv_n: if n < 2 { f64::NAN } else { 1.0 / nf },
            inv: f64::NAN,
            prev_close: f64::NAN,
            buf: vec![[0.0; 2]; m],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shift: 0.0,
            s1: 0.0,
            s2: 0.0,
            smax: 0.0,
            t: 0.0,
            win: HiLo::new(m),
            dbuf: vec![0.0; m],
            dhead: 0,
            dlaps: 0,
            dinv: f64::NAN,
            sy: 0.0,
            sxy: 0.0,
            dpeak: 0.0,
            nf,
            nm1: nf - 1.0,
            sx: nf * (nf - 1.0) / 2.0,
            inv_den: 1.0 / (n2 * n2.saturating_sub(1) / 12).max(1) as f64,
            half: (nf - 1.0) / 2.0,
        }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        let tr = true_range3(high, low, self.prev_close);
        self.prev_close = close;
        let old = std::mem::replace(&mut self.buf[self.head], [close, tr]);
        self.head += 1;
        let (d, o) = (close - self.shift, old[0] - self.shift);
        self.s1 += d - o;
        self.s2 += d * d - o * o;
        self.smax = if self.s2 > self.smax { self.s2 } else { self.smax };
        self.t += tr - old[1];
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0 on raw prices.
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let (shift, s1, s2, t) = shifted_sums(&self.buf, 0);
                self.shift = shift;
                self.s1 = s1;
                self.s2 = s2;
                self.smax = s2;
                self.t = t;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let [hh, ll] = self.win.push(high, low);
        // A window with no high or no low has no extreme: `HiLo` answers its empty identities, which are
        // not a price, and an infinite delta would poison the running regression. `NaN` is what comes out.
        let hh = if hh == f64::NEG_INFINITY { f64::NAN } else { hh };
        let ll = if ll == f64::INFINITY { f64::NAN } else { ll };
        let mut m = self.s1 * self.inv;
        let mut var = self.s2 * self.inv - m * m;
        let mut range = self.t * self.inv;
        // A comparison with a `NaN` is false, so a `NaN` variance (warming up, or a `NaN` close in
        // the window) falls through to the cold path with the too-small ones (which re-centre the
        // shift, and make a flat window exactly flat) and a `NaN` range with them.
        let steady = var >= self.smax * self.inv * 1.0e-5 && !range.is_nan();
        if !steady {
            if self.inv.is_nan() {
                return [f64::NAN; 2];
            }
            let (shift, s1, s2, t) = shifted_sums(&self.buf, self.head);
            self.shift = shift;
            self.s1 = s1;
            self.s2 = s2;
            self.smax = s2;
            self.t = t;
            m = s1 * self.inv;
            var = s2 * self.inv - m * m;
            // Not `.max(0.0)`: that returns 0 for a NaN variance, and a window holding a NaN is NaN.
            if var < 0.0 {
                var = 0.0;
            }
            range = t * self.inv;
        }
        let mid = self.shift + m;
        let dev = var.sqrt();
        let (bb, kc) = (self.bbm * dev, self.kcm * range);
        let squeeze = if mid - bb > mid - kc && mid + bb < mid + kc { 1.0 } else { 0.0 };
        let delta = close - ((hh + ll) / 2.0 + mid) / 2.0;
        // The regression over the last `n` deltas: `sy = sum y`, `sxy = sum i * y`, `i = 0` oldest.
        let od = std::mem::replace(&mut self.dbuf[self.dhead], delta);
        let ad = delta.abs();
        self.dpeak = if ad > self.dpeak { ad } else { self.dpeak };
        self.dhead += 1;
        self.sxy = (self.sxy - (self.sy - od)) + self.nm1 * delta;
        self.sy = (self.sy - od) + delta;
        if self.dhead == self.dbuf.len() {
            self.dhead = 0;
            self.dlaps += 1;
            if self.dlaps >= self.laps_per {
                let (sy, sxy, peak) = line_sums(&self.dbuf, 0);
                self.sy = sy;
                self.sxy = sxy;
                self.dpeak = peak;
                self.dlaps = 0;
            }
            self.dinv = self.inv_n;
        }
        let slope = (self.nf * self.sxy - self.sx * self.sy) * self.inv_den;
        let mut momentum = self.sy * self.dinv + slope * self.half;
        // A `NaN` momentum (a `NaN` delta is in the ring, or has just left), or a delta that was once
        // `TTM_PEAK` ranges of the window: the sums of the ring carry a rounding error of about `eps * n *
        // dpeak`, which a bad tick that has since left would keep until the scheduled rebuild.
        let stale = momentum.is_nan() | (self.dpeak > TTM_PEAK * (hh - ll));
        if stale & !self.dinv.is_nan() {
            let (sy, sxy, peak) = line_sums(&self.dbuf, self.dhead);
            self.sy = sy;
            self.sxy = sxy;
            self.dpeak = peak;
            let slope = (self.nf * sxy - self.sx * sy) * self.inv_den;
            momentum = sy * self.dinv + slope * self.half;
            if momentum.is_nan() {
                momentum = f64::NAN;
            }
        }
        [momentum, squeeze]
    }
}

/// The window of `(close, true range)` pairs (oldest at `head`) as `(shift, sum(x - shift),
/// sum((x - shift)^2), sum of true ranges)`, `shift` being the oldest close when it is finite.
#[cold]
#[inline(never)]
fn shifted_sums(buf: &[[f64; 2]], head: usize) -> (f64, f64, f64, f64) {
    let shift = if buf[head][0].is_finite() { buf[head][0] } else { 0.0 };
    let (mut s1, mut s2, mut t) = (0.0_f64, 0.0_f64, 0.0_f64);
    for v in buf[head..].iter().chain(&buf[..head]) {
        let d = v[0] - shift;
        s1 += d;
        s2 += d * d;
        t += v[1];
    }
    (shift, s1, s2, t)
}

/// `(sum y, sum i * y, largest |y|)` of a ring with `i = 0` for the oldest value (at `head`).
#[cold]
#[inline(never)]
fn line_sums(buf: &[f64], head: usize) -> (f64, f64, f64) {
    let (mut sy, mut sxy, mut peak) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (i, &v) in buf[head..].iter().chain(&buf[..head]).enumerate() {
        sy += v;
        sxy += i as f64 * v;
        peak = if v.abs() > peak { v.abs() } else { peak };
    }
    (sy, sxy, peak)
}
