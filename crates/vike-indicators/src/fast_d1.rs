//! Fast cores for group `d1` of the second wave of indicators (see `_followups/perf-conv2-brief.md`).
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds
//! no `&mut self` call in its hot part, and is used by BOTH the streaming stepper and the
//! whole-series functions, so a stream and a batch agree bit for bit.
//!
//! Six cores: momentum (`MomCore`), Chande momentum (`CmoCore`), Aroon (`AroonCore`), the fast
//! stochastic (`StochfCore`), the money flow index (`MfiCore`) and Chaikin money flow (`CmfCore`).
//! Their sums and extremes are EXACT window functions, never carried: the sums are rebuilt every
//! lap from the window itself (`PairSums`), the extremes are lap maxima (`LapMax2`, `LapArgMax2`), so a
//! flat stretch gives exactly zero and a `NaN` leaves no trace once it is out of the window.

use crate::roll::SmaCore;

/// The window length an indicator is built with: the parameter rounded, at least 1 and at most a
/// bound far above any registry range (`with_params` is also reachable with a raw, unclamped slice,
/// and a ring of `usize::MAX` values must not be asked for).
#[inline]
pub(crate) fn window(p: f64) -> usize {
    crate::indicators::pbars(p).clamp(1, 4096)
}

// ------------------------------------------------------------------------------------------------
// Window sums that are functions of their window.
// ------------------------------------------------------------------------------------------------

/// The sums of two per-value quantities over the last `n` values, EXACT at every step.
///
/// A carried sum (`sum += new - old`) drifts, and drift is garbage exactly where these indicators
/// are most sensitive: a window of equal prices must give a total of zero, not `1e-16` that a
/// ratio then blows up. Here the ring holds the values of the lap being filled; at the end of a lap
/// it is turned, in place, into suffix sums (`ring[k]` = the sum of the lap from `k` on). The window
/// at position `p` of the next lap is then `ring[p + 1]` (the old lap's tail, read before it is
/// overwritten) plus `back` (the new lap's head): one addition per lane, and no subtraction ever, so
/// a window sum is never wrong by more than `n` roundings of its own terms. A `NaN` or an infinity is
/// in the sum while it is in the window and gone the step after it leaves.
///
/// Warm-up costs nothing in the hot path: `back` starts as `NaN` (for a window that is first valid
/// one value into the second lap) or the unwritten ring starts as `NaN` (valid at the end of the
/// first lap).
#[derive(Clone)]
pub(crate) struct PairSums {
    n: usize,
    head: usize,
    /// `n + 1` entries; `ring[n]` is the zero the last suffix sum is read against.
    ring: Vec<[f64; 2]>,
    back: [f64; 2],
}

impl PairSums {
    /// `skip_first`: the first value pushed is not part of any window that is reported (the
    /// quantity is a difference with the previous value), so the first window is valid one value
    /// later than the end of the first lap.
    #[inline]
    pub(crate) fn new(n: usize, skip_first: bool) -> Self {
        let n = n.max(1);
        let mut s = PairSums { n, head: 0, ring: vec![[0.0; 2]; n + 1], back: [0.0; 2] };
        s.clear(skip_first);
        s
    }

    pub(crate) fn clear(&mut self, skip_first: bool) {
        self.head = 0;
        let n = self.n;
        if skip_first {
            self.ring.fill([0.0; 2]);
            self.back = [f64::NAN; 2];
        } else {
            self.ring[..n].fill([f64::NAN; 2]);
            self.ring[n] = [0.0; 2];
            self.back = [0.0; 2];
        }
    }

    /// Push one value of each lane; the sums of the last `n` values of each (`NaN` while the
    /// window is not complete).
    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> [f64; 2] {
        let p = self.head;
        let tail = self.ring[p + 1];
        self.ring[p] = [a, b];
        let back = [self.back[0] + a, self.back[1] + b];
        self.back = back;
        self.head = p + 1;
        if self.head == self.n {
            self.head = 0;
            self.back = [0.0; 2];
            suffix_sums(&mut self.ring[..self.n]);
        }
        [tail[0] + back[0], tail[1] + back[1]]
    }
}

/// `lap[k]` becomes the sum of `lap[k..]`, per lane. Once per lap; a free function over a slice
/// the caller's state does not alias.
#[inline(never)]
fn suffix_sums(lap: &mut [[f64; 2]]) {
    let (mut a, mut b) = (0.0_f64, 0.0_f64);
    for v in lap.iter_mut().rev() {
        a += v[0];
        b += v[1];
        *v = [a, b];
    }
}

// ------------------------------------------------------------------------------------------------
// Window maxima, by the same lap as the sums.
// ------------------------------------------------------------------------------------------------

/// `x` if it is above `acc`, else `acc`: a `NaN` `x` is ignored (it is not above anything), which
/// is what `f64::max` does, in the one `maxsd` it compiles to.
#[inline(always)]
fn above(acc: f64, x: f64) -> f64 {
    if x > acc { x } else { acc }
}

/// The maximum of the last `n` values of two lanes, O(1) in the worst case with no branch that
/// depends on the data: the lap scheme of [`PairSums`] with `max` for `+` (the van Herk / Gil-Werman
/// window). The ring holds the lap being filled; at the end of the lap it becomes the running
/// maxima from each place to the end of the lap, and the window at place `p` of the next lap is the
/// larger of `ring[p + 1]` and the maximum of that lap so far. A `NaN` is ignored, as `f64::max`
/// ignores it; a window with no number in it is `-inf`.
#[derive(Clone)]
pub(crate) struct LapMax2 {
    n: usize,
    head: usize,
    ring: Vec<[f64; 2]>,
    back: [f64; 2],
}

impl LapMax2 {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.max(1);
        LapMax2 {
            n,
            head: 0,
            ring: vec![[f64::NEG_INFINITY; 2]; n + 1],
            back: [f64::NEG_INFINITY; 2],
        }
    }

    pub(crate) fn clear(&mut self) {
        self.head = 0;
        self.ring.fill([f64::NEG_INFINITY; 2]);
        self.back = [f64::NEG_INFINITY; 2];
    }

    /// Push one value of each lane; the maxima of the last `n` values of each (over fewer while the
    /// window is not full).
    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> [f64; 2] {
        let p = self.head;
        let tail = self.ring[p + 1];
        self.ring[p] = [a, b];
        let back = [above(self.back[0], a), above(self.back[1], b)];
        self.back = back;
        self.head = p + 1;
        if self.head == self.n {
            self.head = 0;
            self.back = [f64::NEG_INFINITY; 2];
            suffix_max(&mut self.ring[..self.n]);
        }
        [above(tail[0], back[0]), above(tail[1], back[1])]
    }
}

/// `lap[k]` becomes the maximum of `lap[k..]`, per lane.
#[inline(never)]
fn suffix_max(lap: &mut [[f64; 2]]) {
    let (mut a, mut b) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for v in lap.iter_mut().rev() {
        a = above(a, v[0]);
        b = above(b, v[1]);
        *v = [a, b];
    }
}

/// [`LapMax2`] that also says WHEN: the maxima of the last `n` values of two lanes and how many
/// values ago each was made, the NEWEST of equal maxima winning (so the age is the smallest one).
/// The times are exact whole numbers carried as `f64`, so the selects stay in the float registers.
#[derive(Clone)]
pub(crate) struct LapArgMax2 {
    n: usize,
    head: usize,
    /// The time the next value gets.
    now: f64,
    /// The values of the lap being filled, and the suffix maxima of the lap before.
    val: Vec<[f64; 2]>,
    /// The times of those suffix maxima (written once a lap); `NaN` where there is no maximum.
    at: Vec<[f64; 2]>,
    /// The lap so far: `[max a, its time, max b, its time]`.
    back: [f64; 4],
}

impl LapArgMax2 {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.max(1);
        LapArgMax2 {
            n,
            head: 0,
            now: 0.0,
            val: vec![[f64::NEG_INFINITY; 2]; n + 1],
            at: vec![[f64::NAN; 2]; n + 1],
            back: [f64::NEG_INFINITY, f64::NAN, f64::NEG_INFINITY, f64::NAN],
        }
    }

    pub(crate) fn clear(&mut self) {
        self.head = 0;
        self.now = 0.0;
        self.val.fill([f64::NEG_INFINITY; 2]);
        self.at.fill([f64::NAN; 2]);
        self.back = [f64::NEG_INFINITY, f64::NAN, f64::NEG_INFINITY, f64::NAN];
    }

    /// Push one value of each lane; `[max a, age of it, max b, age of it]` over the last `n` values.
    /// A lane with no number in its window answers `-inf` and an age of `NaN`.
    #[inline(always)]
    pub(crate) fn push(&mut self, a: f64, b: f64) -> [f64; 4] {
        let p = self.head;
        let now = self.now;
        self.now = now + 1.0;
        let tv = self.val[p + 1];
        let tt = self.at[p + 1];
        self.val[p] = [a, b];
        let mut bk = self.back;
        if a >= bk[0] {
            bk[0] = a;
            bk[1] = now;
        }
        if b >= bk[2] {
            bk[2] = b;
            bk[3] = now;
        }
        self.back = bk;
        // The lap so far is the newer: it wins a tie with the tail of the lap before.
        let (va, ta) = if bk[0] >= tv[0] { (bk[0], bk[1]) } else { (tv[0], tt[0]) };
        let (vb, tb) = if bk[2] >= tv[1] { (bk[2], bk[3]) } else { (tv[1], tt[1]) };
        self.head = p + 1;
        if self.head == self.n {
            self.head = 0;
            self.back = [f64::NEG_INFINITY, f64::NAN, f64::NEG_INFINITY, f64::NAN];
            suffix_argmax(
                &mut self.val[..self.n],
                &mut self.at[..self.n],
                now - (self.n - 1) as f64,
            );
        }
        [va, now - ta, vb, now - tb]
    }
}

/// `lap[k]` becomes the maximum of `lap[k..]` per lane and `at[k]` the time it was made, the lap's
/// first value having time `first`; going back, an older value replaces the maximum only when it is
/// strictly above it.
#[inline(never)]
fn suffix_argmax(lap: &mut [[f64; 2]], at: &mut [[f64; 2]], first: f64) {
    let (mut a, mut b) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let (mut ta, mut tb) = (f64::NAN, f64::NAN);
    let mut time = first + lap.len() as f64;
    for (v, t) in lap.iter_mut().zip(at.iter_mut()).rev() {
        time -= 1.0;
        if v[0] > a {
            a = v[0];
            ta = time;
        }
        if v[1] > b {
            b = v[1];
            tb = time;
        }
        *v = [a, b];
        *t = [ta, tb];
    }
}

// ------------------------------------------------------------------------------------------------
// The cores.
// ------------------------------------------------------------------------------------------------

/// Momentum: `x[i] - x[i - n]`. The ring starts as `NaN`, so the first `n` outputs are `NaN` with
/// no warm-up branch.
#[derive(Clone)]
pub(crate) struct MomCore {
    buf: Vec<f64>,
    head: usize,
}

impl MomCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        MomCore { buf: vec![f64::NAN; n.max(1)], head: 0 }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(f64::NAN);
        self.head = 0;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        if self.head == self.buf.len() {
            self.head = 0;
        }
        x - old
    }
}

/// Chande momentum oscillator: `100 * (up - down) / (up + down)` over the last `n` moves of the
/// close, `0` when the window has no move at all. `NaN` for the first `n` values.
#[derive(Clone)]
pub(crate) struct CmoCore {
    prev: f64,
    sums: PairSums,
}

impl CmoCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        CmoCore { prev: f64::NAN, sums: PairSums::new(n, true) }
    }

    pub(crate) fn clear(&mut self) {
        self.prev = f64::NAN;
        self.sums.clear(true);
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64) -> f64 {
        let d = c - self.prev;
        self.prev = c;
        // A `NaN` move lands on the down side, so it poisons the window's total.
        let (g, l) = if d > 0.0 { (d, 0.0) } else { (0.0, -d) };
        let [up, dn] = self.sums.push(g, l);
        let denom = up + dn;
        if denom != 0.0 { 100.0 * (up - dn) / denom } else { 0.0 }
    }
}

/// Aroon up and down: how recently, within the last `n + 1` bars, the highest high and the lowest
/// low were made, as `100 * (n - bars_ago) / n`. `NaN` for the first `n` values.
#[derive(Clone)]
pub(crate) struct AroonCore {
    win: LapArgMax2,
    n: usize,
    nf: f64,
    warm: usize,
}

impl AroonCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        let n = n.max(1);
        AroonCore { win: LapArgMax2::new(n + 1), n, nf: n as f64, warm: n }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
        self.warm = self.n;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64) -> [f64; 2] {
        // The lowest low is the highest of the negated lows.
        let [_, hs, _, ls] = self.win.push(high, -low);
        if self.warm != 0 {
            self.warm -= 1;
            return [f64::NAN; 2];
        }
        // The kernel's own expression. A window with no number in it has an age of `NaN`.
        let up = 100.0 * (self.nf - hs) / self.nf;
        let dn = 100.0 * (self.nf - ls) / self.nf;
        [up, dn]
    }
}

/// Fast stochastic: `%K = 100 * (close - lowest low) / (highest high - lowest low)` over `k` bars
/// (`0` on a window with no range) and `%D`, the mean of the last `d` values of `%K`. `NaN` until
/// `k` bars (`%K`) and `k + d - 1` bars (`%D`) are in.
#[derive(Clone)]
pub(crate) struct StochfCore {
    win: LapMax2,
    d: SmaCore,
    k: usize,
    warm: usize,
}

impl StochfCore {
    #[inline]
    pub(crate) fn new(k: usize, d: usize) -> Self {
        let k = k.max(1);
        StochfCore { win: LapMax2::new(k), d: SmaCore::new(d.max(1)), k, warm: k - 1 }
    }

    pub(crate) fn clear(&mut self) {
        self.win.clear();
        self.d.clear();
        self.warm = self.k - 1;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> [f64; 2] {
        // The lowest low is the highest of the negated lows.
        let [hh, nl] = self.win.push(high, -low);
        if self.warm != 0 {
            self.warm -= 1;
            return [f64::NAN; 2];
        }
        let ll = -nl;
        let rng = hh - ll;
        let k = if rng != 0.0 { 100.0 * (close - ll) / rng } else { 0.0 };
        // `-inf`: no number in the window.
        let k = if hh > f64::NEG_INFINITY && nl > f64::NEG_INFINITY { k } else { f64::NAN };
        [k, self.d.push(k)]
    }
}

/// Money flow index: the volume-weighted RSI, `100 * up / (up + down)` over the last `n` bars'
/// raw money flow (typical price times volume), split by the direction of the typical price; `100`
/// when nothing flowed down. `NaN` for the first `n` values.
#[derive(Clone)]
pub(crate) struct MfiCore {
    prev: f64,
    sums: PairSums,
}

impl MfiCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        MfiCore { prev: f64::NAN, sums: PairSums::new(n, true) }
    }

    pub(crate) fn clear(&mut self) {
        self.prev = f64::NAN;
        self.sums.clear(true);
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64, volume: f64) -> f64 {
        let tp = (high + low + close) / 3.0;
        let rmf = tp * volume;
        // A `NaN` typical price is neither up nor down: it adds nothing to either side.
        let g = if tp > self.prev { rmf } else { 0.0 };
        let l = if tp < self.prev { rmf } else { 0.0 };
        self.prev = tp;
        let [pos, neg] = self.sums.push(g, l);
        if neg == 0.0 { 100.0 } else { 100.0 * pos / (pos + neg) }
    }
}

/// Chaikin money flow: the volume-weighted mean of the close location value over the last `n`
/// bars, `sum(clv * volume) / sum(volume)`; `NaN` where the window has no volume. `NaN` for the
/// first `n - 1` values.
#[derive(Clone)]
pub(crate) struct CmfCore {
    sums: PairSums,
}

impl CmfCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        CmfCore { sums: PairSums::new(n, false) }
    }

    pub(crate) fn clear(&mut self) {
        self.sums.clear(false);
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64, volume: f64) -> f64 {
        let rng = high - low;
        let clv = if rng != 0.0 { ((close - low) - (high - close)) / rng } else { 0.0 };
        let [flow, vol] = self.sums.push(clv * volume, volume);
        if vol != 0.0 { flow / vol } else { f64::NAN }
    }
}
