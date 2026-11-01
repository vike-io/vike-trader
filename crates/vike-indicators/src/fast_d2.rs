//! Fast cores for group `d2` of the second wave of indicators (see `_followups/perf-conv2-brief.md`):
//! `trima`, `hma`, `kama`, `stddev` and `true_range`.
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds
//! no `&mut self` call in its hot part, and is used by BOTH the streaming stepper and the
//! whole-series functions, so a stream and a batch agree bit for bit.
//!
//! The shape is the first wave's (`crate::roll::SmaCore`, `BollingerCore`): rings that start as
//! zeros so the carried sums are right from the first value; running sums rebuilt from the rings
//! every `ceil(512 / n)` laps; and a `NaN` RESULT (a `NaN` in the window, or a stale carried sum
//! after one left) takes a cold rebuild over a shared slice, so the result is `NaN` while a `NaN`
//! is inside the window and exact again the update after it left. Every carried sum is written as
//! `sum += change`, with the change computed off the recurrence's dependent chain, so a chain is one
//! add long. `kama` is the exception to the `NaN` rule, as it always was: it is an infinite-memory
//! recurrence, so a `NaN` close poisons it for good.

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const REBUILD: usize = 512;

/// Laps of a ring of `n` values between two rebuilds of its sums.
#[inline]
fn laps_per(n: usize) -> usize {
    REBUILD.div_ceil(n.max(1)).max(1)
}

/// The weight total `1 + 2 + .. + n` of a linearly weighted average, inverted.
#[inline]
fn inv_weights(n: usize) -> f64 {
    1.0 / (n * (n + 1) / 2) as f64
}

/// The sum of a ring, in any order.
#[inline(always)]
fn ring_total(buf: &[f64]) -> f64 {
    buf.iter().fold(0.0, |a, &v| a + v)
}

// ================================== TRIMA ==================================

/// Slots in each of the two rings of [`TrimaCore`]: a power of two, so that `index & RING_MASK` is in
/// range for the compiler without a check, and at least the 201 the registry's longest period needs.
const RING: usize = 256;
const RING_MASK: usize = RING - 1;

/// Triangular moving average: the mean over `ceil(n / 2)` values of the mean over `n / 2 + 1`
/// values, `NaN` until `n` values are in.
///
/// Two rings and two raw sums: `s1` is the sum of the last `p1` closes, `s2` the sum of the last
/// `p2` values `s1` took, and the average is `s2 / (p1 * p2)`. Both rings start as zeros, so the
/// sums are right from the first value (the early `s1` values are partial sums, which have left
/// the second ring by the time its first output is read). `inv` is `NaN` for the first
/// `p1 + p2 - 2` pushes, which makes those outputs `NaN` through the one `NaN` test every output
/// takes; the cold path behind it counts the warm-up down and then switches `inv` on. Rings and
/// sums are checked and rebuilt at the START of a push, between two states that agree, each sum in
/// a block of its own (two rebuilds in one block are combined by the vectorizer into a packed
/// pair, and a packed pair carried round the loop costs a shuffle on each dependent chain).
#[derive(Clone)]
pub(crate) struct TrimaCore {
    buf1: Box<[f64; RING]>,
    s1: f64,
    head1: usize,
    len1: usize,
    buf2: Box<[f64; RING]>,
    head2: usize,
    len2: usize,
    inv: f64,
    warm: usize,
    laps1: usize,
    laps_per1: usize,
    laps2: usize,
    laps_per2: usize,
    inv0: f64,
    warm0: usize,
    s2: f64,
}

impl TrimaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        // The registry's longest period is 400; a longer one would not fit the rings.
        let n = n.clamp(1, 2 * (RING - 1));
        let (p1, p2) = (n.div_ceil(2), n / 2 + 1);
        let inv0 = 1.0 / (p1 * p2) as f64;
        TrimaCore {
            buf1: Box::new([0.0; RING]),
            s1: 0.0,
            head1: 0,
            len1: p1,
            buf2: Box::new([0.0; RING]),
            head2: 0,
            len2: p2,
            inv: if p1 + p2 == 2 { inv0 } else { f64::NAN },
            warm: p1 + p2 - 2,
            laps1: 0,
            laps_per1: laps_per(p1),
            laps2: 0,
            laps_per2: laps_per(p2),
            inv0,
            warm0: p1 + p2 - 2,
            s2: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf1.fill(0.0);
        self.buf2.fill(0.0);
        self.head1 = 0;
        self.head2 = 0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.warm = self.warm0;
        self.inv = if self.warm0 == 0 { self.inv0 } else { f64::NAN };
        self.laps1 = 0;
        self.laps2 = 0;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        if self.head1 == self.len1 {
            self.head1 = 0;
            self.laps1 += 1;
            if self.laps1 >= self.laps_per1 {
                self.s1 = ring_total(&self.buf1[..self.len1]);
                self.laps1 = 0;
            }
        }
        if self.head2 == self.len2 {
            self.head2 = 0;
            self.laps2 += 1;
            if self.laps2 >= self.laps_per2 {
                self.s2 = ring_total(&self.buf2[..self.len2]);
                self.laps2 = 0;
            }
        }
        let old1 = std::mem::replace(&mut self.buf1[self.head1 & RING_MASK], x);
        self.head1 += 1;
        let s1 = self.s1 + (x - old1);
        self.s1 = s1;
        let old2 = std::mem::replace(&mut self.buf2[self.head2 & RING_MASK], s1);
        self.head2 += 1;
        self.s2 += s1 - old2;
        let out = self.s2 * self.inv;
        if out.is_nan() {
            if self.warm != 0 {
                // Still warming up: count it down, and switch the average on at the end.
                self.warm -= 1;
                if self.warm == 0 {
                    self.inv = self.inv0;
                }
                return f64::NAN;
            }
            // A `NaN` in the window, or stale sums after one left. The first sum is rebuilt, the
            // slot of the second ring that just took the stale value is corrected, then the second.
            // Free functions over shared slices, not `&mut self` methods (see `SmaCore::push`).
            let s1 = ring_total(&self.buf1[..self.len1]);
            self.buf2[(self.head2 - 1) & RING_MASK] = s1;
            let s2 = ring_total(&self.buf2[..self.len2]);
            self.s1 = s1;
            self.s2 = s2;
            let out = s2 * self.inv;
            return if out.is_nan() { f64::NAN } else { out };
        }
        out
    }
}

// =================================== HMA ===================================

/// Hull moving average: `WMA(sqrt(n))` of `2 * WMA(n / 2) - WMA(n)`, `NaN` until
/// `n + sqrt(n) - 2` values are in (`n / 2` and `sqrt(n)` floored, and at least 2 each, as the
/// batch kernel always had them).
///
/// A linearly weighted average over a ring is two sums, `S = sum(y)` and `W = sum(k * y_k)` (weight
/// `k`, the newest heaviest); one push is `W += n * x - S`, `S += x - old`. The combined series
/// `2 * W_h / D_h - W_f / D_f` (`D` the weight totals) is a linear function of those four sums, so
/// it is carried directly: with `a = 2 / D_h`, `b = 1 / D_f`, `T = a W_h - b W_f` and
/// `U = a S_h - b S_f`, one push is `T += g x - U` and `U += d x - (a old_h - b old_f)` with
/// `g = a h - b n`, `d = a - b`. Two sums and two adds on the dependent chains instead of four.
/// The closes live in a ring written twice (at `head` and `head + n`), so the window is always the
/// contiguous `buf[head..head + n]` and the leaving value of the half window is `buf[head + n - h]`.
/// The third stage is a plain weighted average over the combined values, started when the first one
/// exists (the first `n - 1` pushes only feed the first stage).
#[derive(Clone)]
pub(crate) struct HmaCore {
    buf: Vec<f64>,
    head: usize,
    n: usize,
    half: usize,
    ahalf: f64,
    bfull: f64,
    g: f64,
    d: f64,
    t: f64,
    u: f64,
    warm: usize,
    warm0: usize,
    laps: usize,
    laps_per: usize,
    ring: Vec<f64>,
    head3: usize,
    mf: f64,
    s3: f64,
    w3: f64,
    /// `NaN` until the third ring's first lap ends.
    inv3: f64,
    inv30: f64,
}

impl HmaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        // The registry's smallest period is 2; the stages below need a window of at least 2 each.
        let n = n.max(2);
        let half = (n / 2).max(2);
        let root = ((n as f64).sqrt() as usize).max(2);
        let ahalf = 2.0 * inv_weights(half);
        let bfull = inv_weights(n);
        HmaCore {
            buf: vec![0.0; 2 * n],
            head: 0,
            n,
            half,
            ahalf,
            bfull,
            g: ahalf * half as f64 - bfull * n as f64,
            d: ahalf - bfull,
            t: 0.0,
            u: 0.0,
            warm: n - 1,
            warm0: n - 1,
            laps: 0,
            laps_per: laps_per(n),
            ring: vec![0.0; root],
            head3: 0,
            mf: root as f64,
            s3: 0.0,
            w3: 0.0,
            inv3: f64::NAN,
            inv30: inv_weights(root),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.ring.fill(0.0);
        self.head = 0;
        self.t = 0.0;
        self.u = 0.0;
        self.warm = self.warm0;
        self.laps = 0;
        self.head3 = 0;
        self.s3 = 0.0;
        self.w3 = 0.0;
        self.inv3 = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let n = self.n;
        let old_f = self.buf[self.head];
        let old_h = self.buf[self.head + n - self.half];
        self.buf[self.head] = x;
        self.buf[self.head + n] = x;
        self.head += 1;
        let leaving = self.ahalf * old_h - self.bfull * old_f;
        self.t += self.g * x - self.u;
        self.u += self.d * x - leaving;
        if self.head == n {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                let (t, u) = combined_sums(&self.buf[..n], self.half, self.ahalf, self.bfull);
                self.t = t;
                self.u = u;
                let (s3, w3) = wma_sums(&self.ring, self.head3);
                self.s3 = s3;
                self.w3 = w3;
                self.laps = 0;
            }
        }
        if self.warm != 0 {
            self.warm -= 1;
            return f64::NAN;
        }
        let c = self.t;
        let old3 = std::mem::replace(&mut self.ring[self.head3], c);
        self.head3 += 1;
        let dw3 = self.mf * c - self.s3;
        self.w3 += dw3;
        self.s3 += c - old3;
        if self.head3 == self.ring.len() {
            self.head3 = 0;
            self.inv3 = self.inv30;
        }
        let out = self.w3 * self.inv3;
        if out.is_nan() {
            // The third ring's own warm-up, a `NaN` in a window, or stale sums after one left. The
            // first stage is rebuilt, the slot of the ring that took the stale combined value is
            // corrected, then the third stage's sums.
            let (t, u) = combined_sums(
                &self.buf[self.head..self.head + n],
                self.half,
                self.ahalf,
                self.bfull,
            );
            self.t = t;
            self.u = u;
            let last = if self.head3 == 0 { self.ring.len() - 1 } else { self.head3 - 1 };
            self.ring[last] = t;
            let (s3, w3) = wma_sums(&self.ring, self.head3);
            self.s3 = s3;
            self.w3 = w3;
            let out = w3 * self.inv3;
            return if out.is_nan() { f64::NAN } else { out };
        }
        out
    }
}

/// `(T, U)` of a window given oldest first: `T = a W_h - b W_f`, `U = a S_h - b S_f` over the whole
/// window (`f`) and its newest `half` values (`h`).
#[inline(always)]
fn combined_sums(win: &[f64], half: usize, a: f64, b: f64) -> (f64, f64) {
    let first_half = win.len() - half;
    let (mut sf, mut wf, mut sh, mut wh) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for (k, &v) in win.iter().enumerate() {
        sf += v;
        wf += (k + 1) as f64 * v;
        if k >= first_half {
            sh += v;
            wh += (k - first_half + 1) as f64 * v;
        }
    }
    (a * wh - b * wf, a * sh - b * sf)
}

/// `(S, W)` of the whole ring, oldest value at `head`.
#[inline(always)]
fn wma_sums(buf: &[f64], head: usize) -> (f64, f64) {
    let (mut s, mut w) = (0.0_f64, 0.0_f64);
    for (k, &v) in buf[head..].iter().chain(&buf[..head]).enumerate() {
        s += v;
        w += (k + 1) as f64 * v;
    }
    (s, w)
}

// =================================== KAMA ==================================

/// Kaufman's adaptive moving average over `n` moves with a `fast` and a `slow` span.
///
/// The efficiency ratio is `|net move| / path` over the last `n + 1` closes, where the path is the
/// sum of the `n` absolute moves between them. The path is a running sum: the move that enters is
/// added and the move that leaves is subtracted (both read off one ring of `n + 1` closes), and it
/// is rebuilt from the ring every `ceil(512 / (n + 1))` laps. A count of the non-zero moves in the
/// window keeps a FLAT window exactly flat (efficiency ratio 1, as the kernel always had it)
/// whatever rounding residue the running path carries after a volatile stretch.
///
/// The first `n + 1` pushes take a short slow path (fill the ring, sum the first moves, seed the
/// average with the close at index `n - 1`); every later push is branch-free. The average is
/// started at index `n`, so the first `n` outputs are `NaN`. A `NaN` close poisons the average for
/// good, as it always did.
#[derive(Clone)]
pub(crate) struct KamaCore {
    buf: Vec<f64>,
    head: usize,
    n: usize,
    warm: usize,
    spread: f64,
    slow_sc: f64,
    prev_x: f64,
    path: f64,
    nz: usize,
    kama: f64,
    laps: usize,
    laps_per: usize,
}

impl KamaCore {
    /// `n` is the efficiency-ratio window, `fast` and `slow` the spans of the two limiting averages.
    #[inline]
    pub(crate) fn new(n: usize, fast: usize, slow: usize) -> Self {
        let n = n.max(1);
        let fast_sc = 2.0 / (fast as f64 + 1.0);
        let slow_sc = 2.0 / (slow as f64 + 1.0);
        KamaCore {
            buf: vec![0.0; n + 1],
            head: 0,
            n,
            warm: n + 1,
            spread: fast_sc - slow_sc,
            slow_sc,
            prev_x: f64::NAN,
            path: 0.0,
            nz: 0,
            kama: f64::NAN,
            laps: 0,
            laps_per: laps_per(n + 1),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.warm = self.n + 1;
        self.prev_x = f64::NAN;
        self.path = 0.0;
        self.nz = 0;
        self.kama = f64::NAN;
        self.laps = 0;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let net;
        if self.warm != 0 {
            // Slow path, `n + 1` pushes: `t` is this push's index.
            let t = self.n + 1 - self.warm;
            self.buf[t] = x;
            if t != 0 {
                let d = (x - self.prev_x).abs();
                self.path += d;
                self.nz += usize::from(d != 0.0);
            }
            self.prev_x = x;
            self.warm -= 1;
            if t < self.n {
                if t + 1 == self.n {
                    // The average is seeded with the close at index `n - 1`.
                    self.kama = x;
                }
                return f64::NAN;
            }
            net = x - self.buf[0];
            self.head = 0;
        } else {
            let old = std::mem::replace(&mut self.buf[self.head], x);
            let mut next = self.head + 1;
            let lap = next == self.buf.len();
            if lap {
                next = 0;
            }
            self.head = next;
            let second = self.buf[next];
            let moved_in = (x - self.prev_x).abs();
            let moved_out = (second - old).abs();
            self.prev_x = x;
            self.path = (self.path + moved_in) - moved_out;
            self.nz = (self.nz + usize::from(moved_in != 0.0)) - usize::from(moved_out != 0.0);
            if lap {
                self.laps += 1;
                if self.laps >= self.laps_per {
                    let (path, nz) = path_sum(&self.buf);
                    self.path = path;
                    self.nz = nz;
                    self.laps = 0;
                }
            }
            net = x - second;
        }
        let er = if self.nz == 0 {
            self.path = 0.0;
            1.0
        } else if self.path <= net {
            1.0
        } else {
            (net / self.path).abs()
        };
        let s = er * self.spread + self.slow_sc;
        let sc = s * s;
        self.kama = self.kama * (1.0 - sc) + x * sc;
        self.kama
    }
}

/// The path of a ring read from slot 0 (the oldest) and the count of its non-zero moves.
#[inline(always)]
fn path_sum(buf: &[f64]) -> (f64, usize) {
    let (mut path, mut nz) = (0.0_f64, 0usize);
    for pair in buf.windows(2) {
        let d = (pair[1] - pair[0]).abs();
        path += d;
        nz += usize::from(d != 0.0);
    }
    (path, nz)
}

// ================================== STDDEV =================================

/// Population standard deviation over `n` values, from running sums of the window shifted by its
/// oldest value at the last rebuild (so a price near a million with moves of a hundredth keeps its
/// digits), rebuilt every `ceil(512 / n)` laps. This is [`crate::roll::BollingerCore`]'s variance.
///
/// The sums' rounding error scales with the largest second moment `smax` since the last rebuild
/// (the shift is up to a rebuild old, so a window that has become tight beside it keeps few
/// digits; as a deviation the error reaches 1e-8 times the shift distance). One compare catches
/// every case that matters: a variance that is `NaN` (warming up, or a `NaN` in the window), or
/// under a hundred thousandth of `smax / n`, takes the cold path, which re-centres the shift on the
/// window's oldest value and sums again. That also makes a window of bit-equal values EXACTLY
/// `0.0` (the decision `window::is_constant_window` records): re-centred on the repeated value
/// every deviation is `0.0`, and the compare is then `0 >= 0`, so it stays exact without a branch.
/// A window holding a `NaN` is `NaN`, and exact again the update after the `NaN` left.
#[derive(Clone)]
pub(crate) struct StddevCore {
    inv: f64,
    inv_n: f64,
    buf: Vec<f64>,
    head: usize,
    laps: usize,
    laps_per: usize,
    shift: f64,
    s1: f64,
    s2: f64,
    /// The largest `s2` since the last rebuild.
    smax: f64,
}

impl StddevCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        StddevCore {
            inv: f64::NAN,
            inv_n: if n == 0 { f64::NAN } else { 1.0 / n as f64 },
            buf: vec![0.0; n.max(1)],
            head: 0,
            laps: 0,
            laps_per: laps_per(n),
            shift: 0.0,
            s1: 0.0,
            s2: 0.0,
            smax: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buf.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.shift = 0.0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.smax = 0.0;
        self.inv = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.buf[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        self.s1 += d - o;
        self.s2 += d * d - o * o;
        self.smax = if self.s2 > self.smax { self.s2 } else { self.smax };
        if self.head == self.buf.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0 on raw prices.
            if self.laps >= self.laps_per || self.inv.is_nan() {
                let (shift, s1, s2) = shifted_sums(&self.buf, 0);
                self.shift = shift;
                self.s1 = s1;
                self.s2 = s2;
                self.smax = s2;
                self.laps = 0;
            }
            self.inv = self.inv_n;
        }
        let m = self.s1 * self.inv;
        let var = self.s2 * self.inv - m * m;
        // A comparison with a `NaN` is false, so a `NaN` variance (warming up, or a `NaN` in the
        // window) falls through to the cold path with the too-small ones.
        if var >= self.smax * self.inv * 1.0e-5 {
            return var.sqrt();
        }
        // Cold, and written inline over a shared slice (see `BollingerCore::push`).
        if self.inv.is_nan() {
            return f64::NAN;
        }
        let (shift, s1, s2) = shifted_sums(&self.buf, self.head);
        self.shift = shift;
        self.s1 = s1;
        self.s2 = s2;
        self.smax = s2;
        let m = s1 * self.inv;
        // Not `.max(0.0)`: that returns 0 for a NaN variance, and a window holding a NaN is NaN.
        let mut var = s2 * self.inv - m * m;
        if var < 0.0 {
            var = 0.0;
        }
        if var.is_nan() { f64::NAN } else { var.sqrt() }
    }
}

/// The window (oldest value at `head`) as `(shift, sum(x - shift), sum((x - shift)^2))`, `shift`
/// being the oldest value when it is finite.
#[inline(always)]
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

// ================================ TRUE RANGE ===============================

/// The true range of a bar against the previous close, `max(high, prev) - min(low, prev)`: for a
/// bar with `low <= high` the same operands, subtracted, as `max(high - low, |high - prev|,
/// |low - prev|)` (rounding is monotonic, so the largest of the three rounded differences is the
/// rounded largest), with one subtraction instead of three.
///
/// The two selects are written as comparisons, which `maxsd` and `minsd` implement exactly: a
/// comparison with a `NaN` is false, so a `NaN` previous close (the first bar) leaves `high` and
/// `low` alone, giving `high - low`, and a `NaN` high or low gives `NaN`. `f64::max` and `f64::min`
/// answer the other operand for a `NaN`, which costs a compare and a blend each.
#[inline(always)]
pub(crate) fn true_range(high: f64, low: f64, prev_close: f64) -> f64 {
    let hi = if prev_close > high { prev_close } else { high };
    let lo = if prev_close < low { prev_close } else { low };
    hi - lo
}

/// [`true_range`] with the previous close kept between pushes.
#[derive(Clone)]
pub(crate) struct TrueRangeCore {
    prev_close: f64,
}

impl TrueRangeCore {
    #[inline]
    pub(crate) fn new() -> Self {
        TrueRangeCore { prev_close: f64::NAN }
    }

    pub(crate) fn clear(&mut self) {
        self.prev_close = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let pc = self.prev_close;
        self.prev_close = close;
        true_range(high, low, pc)
    }
}
