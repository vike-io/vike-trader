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

use std::collections::VecDeque;

/// How many updates a running sum is trusted for before it is rebuilt from the window.
const REBUILD: usize = 512;

/// Laps of a ring of `n` values between two rebuilds of its sums.
#[inline]
fn laps_per(n: usize) -> usize {
    REBUILD.div_ceil(n.max(1)).max(1)
}

/// The weight total `1 + 2 + .. + n` of a linearly weighted average, inverted.
#[inline]
fn inverse_weights(n: usize) -> f64 {
    1.0 / (n * (n + 1) / 2) as f64
}

/// The sum of a ring, in any order.
#[inline(always)]
fn ring_total(buffer: &[f64]) -> f64 {
    buffer.iter().fold(0.0, |a, &v| a + v)
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
/// the second ring by the time its first output is read). `inverse` is `NaN` for the first
/// `p1 + p2 - 2` pushes, which makes those outputs `NaN` through the one `NaN` test every output
/// takes; the cold path behind it counts the warm-up down and then switches `inverse` on. Rings and
/// sums are checked and rebuilt at the START of a push, between two states that agree, each sum in
/// a block of its own (two rebuilds in one block are combined by the vectorizer into a packed
/// pair, and a packed pair carried round the loop costs a shuffle on each dependent chain).
#[derive(Clone)]
pub(crate) struct TrimaCore {
    buffer1: Box<[f64; RING]>,
    s1: f64,
    head1: usize,
    len1: usize,
    buffer2: Box<[f64; RING]>,
    head2: usize,
    len2: usize,
    inverse: f64,
    warm: usize,
    laps1: usize,
    laps_per1: usize,
    laps2: usize,
    laps_per2: usize,
    inverse0: f64,
    warm0: usize,
    s2: f64,
}

impl TrimaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        // The registry's longest period is 400; a longer one would not fit the rings.
        let n = n.clamp(1, 2 * (RING - 1));
        let (p1, p2) = (n.div_ceil(2), n / 2 + 1);
        let inverse0 = 1.0 / (p1 * p2) as f64;
        TrimaCore {
            buffer1: Box::new([0.0; RING]),
            s1: 0.0,
            head1: 0,
            len1: p1,
            buffer2: Box::new([0.0; RING]),
            head2: 0,
            len2: p2,
            inverse: if p1 + p2 == 2 { inverse0 } else { f64::NAN },
            warm: p1 + p2 - 2,
            laps1: 0,
            laps_per1: laps_per(p1),
            laps2: 0,
            laps_per2: laps_per(p2),
            inverse0,
            warm0: p1 + p2 - 2,
            s2: 0.0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buffer1.fill(0.0);
        self.buffer2.fill(0.0);
        self.head1 = 0;
        self.head2 = 0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.warm = self.warm0;
        self.inverse = if self.warm0 == 0 { self.inverse0 } else { f64::NAN };
        self.laps1 = 0;
        self.laps2 = 0;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        if self.head1 == self.len1 {
            self.head1 = 0;
            self.laps1 += 1;
            if self.laps1 >= self.laps_per1 {
                self.s1 = ring_total(&self.buffer1[..self.len1]);
                self.laps1 = 0;
            }
        }
        if self.head2 == self.len2 {
            self.head2 = 0;
            self.laps2 += 1;
            if self.laps2 >= self.laps_per2 {
                self.s2 = ring_total(&self.buffer2[..self.len2]);
                self.laps2 = 0;
            }
        }
        let old1 = std::mem::replace(&mut self.buffer1[self.head1 & RING_MASK], x);
        self.head1 += 1;
        let s1 = self.s1 + (x - old1);
        self.s1 = s1;
        let old2 = std::mem::replace(&mut self.buffer2[self.head2 & RING_MASK], s1);
        self.head2 += 1;
        self.s2 += s1 - old2;
        let out = self.s2 * self.inverse;
        if out.is_nan() {
            if self.warm != 0 {
                // Still warming up: count it down, and switch the average on at the end.
                self.warm -= 1;
                if self.warm == 0 {
                    self.inverse = self.inverse0;
                }
                return f64::NAN;
            }
            // A `NaN` in the window, or stale sums after one left. The first sum is rebuilt, the
            // slot of the second ring that just took the stale value is corrected, then the second.
            // Free functions over shared slices, not `&mut self` methods (see `SmaCore::push`).
            let s1 = ring_total(&self.buffer1[..self.len1]);
            self.buffer2[(self.head2 - 1) & RING_MASK] = s1;
            let s2 = ring_total(&self.buffer2[..self.len2]);
            self.s1 = s1;
            self.s2 = s2;
            let out = s2 * self.inverse;
            return if out.is_nan() { f64::NAN } else { out };
        }
        out
    }
}

// =================================== HMA ===================================

/// Hull moving average: `WMA(sqrt(n))` of `2 * WMA(n / 2) - WMA(n)`, `NaN` until
/// `n + root - 2` values are in (the registry row's `n / 2` and `sqrt(n)` floored, and at least 2
/// each, as the batch kernel always had them; [`hma_lengths`] gives the options' other lengths).
///
/// A linearly weighted average over a ring is two sums, `S = sum(y)` and `W = sum(k * y_k)` (weight
/// `k`, the newest heaviest); one push is `W += n * x - S`, `S += x - old`. The combined series
/// `2 * W_h / D_h - W_f / D_f` (`D` the weight totals) is a linear function of those four sums, so
/// it is carried directly: with `a = 2 / D_h`, `b = 1 / D_f`, `T = a W_h - b W_f` and
/// `U = a S_h - b S_f`, one push is `T += g x - U` and `U += d x - (a old_h - b old_f)` with
/// `g = a h - b n`, `d = a - b`. Two sums and two adds on the dependent chains instead of four.
/// The closes live in a ring written twice (at `head` and `head + n`), so the window is always the
/// contiguous `buffer[head..head + n]` and the leaving value of the half window is `buffer[head + n - h]`.
/// The third stage is a plain weighted average over the combined values, started when the first one
/// exists (the first `n - 1` pushes only feed the first stage).
#[derive(Clone)]
pub(crate) struct HmaCore {
    buffer: Vec<f64>,
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
    inverse3: f64,
    inverse30: f64,
}

/// The three lengths of a Hull over `n` values (at least 2): `(n, half, root)`, the half `n / 2`
/// (rounded up with `rounded_half`, else floored) and the root `sqrt(n)` (rounded with
/// `rounded_root`, else floored), each at least `least`. The registry row's are
/// `hma_lengths(n, false, false, 2)`.
#[inline]
pub(crate) fn hma_lengths(
    n: usize,
    rounded_half: bool,
    rounded_root: bool,
    least: usize,
) -> (usize, usize, usize) {
    let n = n.max(2);
    let half = if rounded_half { n.div_ceil(2) } else { n / 2 }.max(least);
    let s = (n as f64).sqrt();
    let root = (if rounded_root { s.round() } else { s } as usize).max(least);
    (n, half, root)
}

impl HmaCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        Self::with_lengths(hma_lengths(n, false, false, 2))
    }

    /// The core over the lengths `(n, half, root)` of [`hma_lengths`]; a half or a root of 1 is an
    /// average of one value, the value itself.
    #[inline]
    pub(crate) fn with_lengths((n, half, root): (usize, usize, usize)) -> Self {
        let ahalf = 2.0 * inverse_weights(half);
        let bfull = inverse_weights(n);
        HmaCore {
            buffer: vec![0.0; 2 * n],
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
            inverse3: f64::NAN,
            inverse30: inverse_weights(root),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.ring.fill(0.0);
        self.head = 0;
        self.t = 0.0;
        self.u = 0.0;
        self.warm = self.warm0;
        self.laps = 0;
        self.head3 = 0;
        self.s3 = 0.0;
        self.w3 = 0.0;
        self.inverse3 = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let n = self.n;
        let old_f = self.buffer[self.head];
        let old_h = self.buffer[self.head + n - self.half];
        self.buffer[self.head] = x;
        self.buffer[self.head + n] = x;
        self.head += 1;
        let leaving = self.ahalf * old_h - self.bfull * old_f;
        self.t += self.g * x - self.u;
        self.u += self.d * x - leaving;
        if self.head == n {
            self.head = 0;
            self.laps += 1;
            if self.laps >= self.laps_per {
                let (t, u) = combined_sums(&self.buffer[..n], self.half, self.ahalf, self.bfull);
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
            self.inverse3 = self.inverse30;
        }
        let out = self.w3 * self.inverse3;
        if out.is_nan() {
            // The third ring's own warm-up, a `NaN` in a window, or stale sums after one left. The
            // first stage is rebuilt, the slot of the ring that took the stale combined value is
            // corrected, then the third stage's sums.
            let (t, u) = combined_sums(
                &self.buffer[self.head..self.head + n],
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
            let out = w3 * self.inverse3;
            return if out.is_nan() { f64::NAN } else { out };
        }
        out
    }
}

/// The Hull with its lengths given on each value (`crate::indicators::Hma::next_value_with_period`):
/// the last values and the last combined values, each average a fresh weighted fold over this
/// value's lengths, so no running sum carries a length that changed.
#[derive(Clone, Default)]
pub(crate) struct HmaPerBar {
    values: VecDeque<f64>,
    combined: VecDeque<f64>,
}

impl HmaPerBar {
    /// One value with this value's lengths `(n, half, root)`, each at most `keep` (how many values
    /// and combined values are kept): the combined value `2 * WMA(half) - WMA(n)` is `NaN` while
    /// fewer than `n` values are in, and the result, the `WMA(root)` of the combined values, while
    /// fewer than `root` are.
    pub(crate) fn push(
        &mut self,
        x: f64,
        (n, half, root): (usize, usize, usize),
        keep: usize,
    ) -> f64 {
        let values = &mut self.values;
        values.push_back(x);
        if values.len() > keep {
            values.pop_front();
        }
        let c = if values.len() < n {
            f64::NAN
        } else {
            2.0 * wma_tail(values, half) - wma_tail(values, n)
        };
        self.combined.push_back(c);
        if self.combined.len() > keep {
            self.combined.pop_front();
        }
        if self.combined.len() < root { f64::NAN } else { wma_tail(&self.combined, root) }
    }
}

/// The linearly weighted average of the newest `m` of `v`: weight 1 for the oldest of them, `m` for
/// the newest.
fn wma_tail(v: &VecDeque<f64>, m: usize) -> f64 {
    let sum = v.range(v.len() - m..).zip(1usize..).fold(0.0, |s, (&x, k)| s + k as f64 * x);
    sum * inverse_weights(m)
}

/// `(T, U)` of a window given oldest first: `T = a W_h - b W_f`, `U = a S_h - b S_f` over the whole
/// window (`f`) and its newest `half` values (`h`).
#[inline(always)]
fn combined_sums(win: &[f64], half: usize, a: f64, b: f64) -> (f64, f64) {
    let first_half = win.len() - half;
    let (mut sf, mut weighted_full, mut sh, mut wh) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for (k, &v) in win.iter().enumerate() {
        sf += v;
        weighted_full += (k + 1) as f64 * v;
        if k >= first_half {
            sh += v;
            wh += (k - first_half + 1) as f64 * v;
        }
    }
    (a * wh - b * weighted_full, a * sh - b * sf)
}

/// `(S, W)` of the whole ring, oldest value at `head`.
#[inline(always)]
fn wma_sums(buffer: &[f64], head: usize) -> (f64, f64) {
    let (mut s, mut w) = (0.0_f64, 0.0_f64);
    for (k, &v) in buffer[head..].iter().chain(&buffer[..head]).enumerate() {
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
///
/// Three options, all off in [`KamaCore::new`]: `seed_first` seeds the average with the FIRST close
/// and answers from index 0, a window not yet full reading as a flat one; `flat_er` is the ratio a
/// flat window reads (1 unset); `textbook` folds the window's path afresh each bar
/// ([`ring_path`]), reads the ratio as `|net| / path` with no clamp, and moves the average as
/// `kama + sc * (close - kama)`.
#[derive(Clone)]
pub(crate) struct KamaCore {
    buffer: Vec<f64>,
    head: usize,
    n: usize,
    warm: usize,
    spread: f64,
    slow_sc: f64,
    previous_x: f64,
    path: f64,
    nz: usize,
    kama: f64,
    laps: usize,
    laps_per: usize,
    pub(crate) seed_first: bool,
    pub(crate) zero_seed: bool,
    pub(crate) flat_er: f64,
    pub(crate) textbook: bool,
}

impl KamaCore {
    /// `n` is the efficiency-ratio window, `fast` and `slow` the spans of the two limiting averages.
    #[inline]
    pub(crate) fn new(n: usize, fast: usize, slow: usize) -> Self {
        let n = n.max(1);
        let fast_sc = 2.0 / (fast as f64 + 1.0);
        let slow_sc = 2.0 / (slow as f64 + 1.0);
        KamaCore {
            buffer: vec![0.0; n + 1],
            head: 0,
            n,
            warm: n + 1,
            spread: fast_sc - slow_sc,
            slow_sc,
            previous_x: f64::NAN,
            path: 0.0,
            nz: 0,
            kama: f64::NAN,
            laps: 0,
            laps_per: laps_per(n + 1),
            seed_first: false,
            zero_seed: false,
            flat_er: 1.0,
            textbook: false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.head = 0;
        self.warm = self.n + 1;
        self.previous_x = f64::NAN;
        self.path = 0.0;
        self.nz = 0;
        self.kama = f64::NAN;
        self.laps = 0;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let net;
        // `seed_first` before the window is full: there is no ratio yet, which reads as flat.
        let mut filling = false;
        if self.warm != 0 {
            // Slow path, `n + 1` pushes: `t` is this push's index.
            let t = self.n + 1 - self.warm;
            self.buffer[t] = x;
            if t != 0 {
                let d = (x - self.previous_x).abs();
                self.path += d;
                self.nz += usize::from(d != 0.0);
            }
            self.previous_x = x;
            self.warm -= 1;
            if t < self.n {
                if self.seed_first {
                    if t == 0 {
                        // The average is seeded with the first close, and answers from here.
                        self.kama = x;
                        return x;
                    }
                    filling = true;
                } else if self.zero_seed {
                    if t == 0 {
                        // The average starts from 0 and moves on this first close.
                        self.kama = 0.0;
                    }
                    filling = true;
                } else {
                    if t + 1 == self.n {
                        // The average is seeded with the close at index `n - 1`.
                        self.kama = x;
                    }
                    return f64::NAN;
                }
            }
            net = x - self.buffer[0];
            self.head = 0;
        } else {
            let old = std::mem::replace(&mut self.buffer[self.head], x);
            let mut next = self.head + 1;
            let lap = next == self.buffer.len();
            if lap {
                next = 0;
            }
            self.head = next;
            let second = self.buffer[next];
            let moved_in = (x - self.previous_x).abs();
            let moved_out = (second - old).abs();
            self.previous_x = x;
            self.path = (self.path + moved_in) - moved_out;
            self.nz = (self.nz + usize::from(moved_in != 0.0)) - usize::from(moved_out != 0.0);
            if lap {
                self.laps += 1;
                if self.laps >= self.laps_per {
                    let (path, nz) = path_sum(&self.buffer);
                    self.path = path;
                    self.nz = nz;
                    self.laps = 0;
                }
            }
            net = x - second;
        }
        let er = if filling {
            self.flat_er
        } else if self.textbook {
            let path = ring_path(&self.buffer, self.head);
            if path > 0.0 { net.abs() / path } else { self.flat_er }
        } else if self.nz == 0 {
            self.path = 0.0;
            self.flat_er
        } else if self.path <= net {
            1.0
        } else {
            (net / self.path).abs()
        };
        let s = er * self.spread + self.slow_sc;
        let sc = s * s;
        self.kama = if self.textbook {
            self.kama + sc * (x - self.kama)
        } else {
            self.kama * (1.0 - sc) + x * sc
        };
        self.kama
    }
}

/// The path of a full ring read from `head` (the oldest), folded afresh: the window's own sum of
/// its absolute moves, oldest first, whatever came before it.
#[inline]
fn ring_path(buffer: &[f64], head: usize) -> f64 {
    let mut previous = buffer[head];
    let mut path = 0.0_f64;
    for &v in buffer[head + 1..].iter().chain(&buffer[..head]) {
        path += (v - previous).abs();
        previous = v;
    }
    path
}

/// The path of a ring read from slot 0 (the oldest) and the count of its non-zero moves.
#[inline(always)]
fn path_sum(buffer: &[f64]) -> (f64, usize) {
    let (mut path, mut nz) = (0.0_f64, 0usize);
    for pair in buffer.windows(2) {
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
    inverse: f64,
    inverse_n: f64,
    buffer: Vec<f64>,
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
            inverse: f64::NAN,
            inverse_n: if n == 0 { f64::NAN } else { 1.0 / n as f64 },
            buffer: vec![0.0; n.max(1)],
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
        self.buffer.fill(0.0);
        self.head = 0;
        self.laps = 0;
        self.shift = 0.0;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.smax = 0.0;
        self.inverse = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let old = std::mem::replace(&mut self.buffer[self.head], x);
        self.head += 1;
        let (d, o) = (x - self.shift, old - self.shift);
        self.s1 += d - o;
        self.s2 += d * d - o * o;
        self.smax = if self.s2 > self.smax { self.s2 } else { self.smax };
        if self.head == self.buffer.len() {
            self.head = 0;
            self.laps += 1;
            // The first lap always rebuilds: until then the sums ran with a shift of 0 on raw prices.
            if self.laps >= self.laps_per || self.inverse.is_nan() {
                let (shift, s1, s2) = shifted_sums(&self.buffer, 0);
                self.shift = shift;
                self.s1 = s1;
                self.s2 = s2;
                self.smax = s2;
                self.laps = 0;
            }
            self.inverse = self.inverse_n;
        }
        let m = self.s1 * self.inverse;
        let var = self.s2 * self.inverse - m * m;
        // A comparison with a `NaN` is false, so a `NaN` variance (warming up, or a `NaN` in the
        // window) falls through to the cold path with the too-small ones.
        if var >= self.smax * self.inverse * 1.0e-5 {
            return var.sqrt();
        }
        // Cold, and written inline over a shared slice (see `BollingerCore::push`).
        if self.inverse.is_nan() {
            return f64::NAN;
        }
        let (shift, s1, s2) = shifted_sums(&self.buffer, self.head);
        self.shift = shift;
        self.s1 = s1;
        self.s2 = s2;
        self.smax = s2;
        let m = s1 * self.inverse;
        // Not `.max(0.0)`: that returns 0 for a NaN variance, and a window holding a NaN is NaN.
        let mut var = s2 * self.inverse - m * m;
        if var < 0.0 {
            var = 0.0;
        }
        if var.is_nan() { f64::NAN } else { var.sqrt() }
    }
}

/// The window (oldest value at `head`) as `(shift, sum(x - shift), sum((x - shift)^2))`, `shift`
/// being the oldest value when it is finite.
#[inline(always)]
fn shifted_sums(buffer: &[f64], head: usize) -> (f64, f64, f64) {
    let shift = if buffer[head].is_finite() { buffer[head] } else { 0.0 };
    let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
    for &v in buffer[head..].iter().chain(&buffer[..head]) {
        let d = v - shift;
        s1 += d;
        s2 += d * d;
    }
    (shift, s1, s2)
}

// ================================ TRUE RANGE ===============================

/// The true range of a bar against the previous close, `max(high, previous) - min(low, previous)`: for a
/// bar with `low <= high` the same operands, subtracted, as `max(high - low, |high - prev|,
/// |low - prev|)` (rounding is monotonic, so the largest of the three rounded differences is the
/// rounded largest), with one subtraction instead of three.
///
/// The two selects are written as comparisons, which `maxsd` and `minsd` implement exactly: a
/// comparison with a `NaN` is false, so a `NaN` previous close (the first bar) leaves `high` and
/// `low` alone, giving `high - low`, and a `NaN` high or low gives `NaN`. `f64::max` and `f64::min`
/// answer the other operand for a `NaN`, which costs a compare and a blend each.
#[inline(always)]
pub(crate) fn true_range(high: f64, low: f64, previous_close: f64) -> f64 {
    let hi = if previous_close > high { previous_close } else { high };
    let lo = if previous_close < low { previous_close } else { low };
    hi - lo
}

/// [`true_range`] with the previous close kept between pushes.
#[derive(Clone)]
pub(crate) struct TrueRangeCore {
    previous_close: f64,
}

impl TrueRangeCore {
    #[inline]
    pub(crate) fn new() -> Self {
        TrueRangeCore { previous_close: f64::NAN }
    }

    pub(crate) fn clear(&mut self) {
        self.previous_close = f64::NAN;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let pc = self.previous_close;
        self.previous_close = close;
        true_range(high, low, pc)
    }

    /// [`TrueRangeCore::push`], `NaN` where the previous close is missing (the first bar's).
    #[inline(always)]
    pub(crate) fn push_after_close(&mut self, high: f64, low: f64, close: f64) -> f64 {
        let pc = self.previous_close;
        self.previous_close = close;
        if pc.is_nan() { f64::NAN } else { true_range(high, low, pc) }
    }
}
