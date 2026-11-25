//! Fast cores for group `k` of the fourth wave of indicators (see `_followups/perf-conv4-addendum.md`):
//! the volume family `adosc`, `anchored_vwap`, `efi`, `eom`, `kvo`, `net_volume`, `nvi`, `pvi`,
//! `pvt`, `volume_osc`, `williams_ad` and `vwap`.
//!
//! Each core is a small `Clone` struct of plain numbers whose `push` is `#[inline(always)]`, holds no
//! `&mut self` call in its hot part, and is used by BOTH the streaming stepper and the whole-series
//! functions, so a stream and a batch agree bit for bit.
//!
//! The recurrences are the old steppers' expressions, term for term, so the values do not move
//! (`eom` runs the ring-sum mean of [`crate::roll::SmaCore`] where the kernel re-folded each window,
//! so it differs from the kernel by rounding noise). What changed is the shape:
//!
//! * "No previous bar" is a `NaN` previous value wherever the comparisons it feeds are false for a
//!   `NaN` (`net_volume`, `nvi`, `pvi`, `williams_ad`), so the first bar needs no branch; where it
//!   would not give the old answer (`pvt`, `eom`, `kvo`) a flag is tested instead (a branch the
//!   predictor never misses).
//! * A choice that is a coin toss in the data (an up bar or a down bar) is a select, not a branch:
//!   `net_volume` flips the sign bit of the volume, `nvi` and `pvi` multiply by `1.0` when the
//!   volume did not move the right way (an exact no-op, `NaN` included).
//! * An exponential average ([`EmaK`]) counts its seed down with one `warm` counter shared by the
//!   averages of a core, so the steady state is two multiplies and an add per average and nothing
//!   else; the seed is the sum of the first `n` values divided by `n`, as the old steppers had it.

use crate::roll::PeakSmaCore;

/// An exponential average seeded with the mean of its first `n` inputs, then
/// `alpha * x + (1 - alpha) * previous`; `NaN` until `n` inputs are in. The old steppers' recurrence bit
/// for bit: the seed divides by `n` (once) and the keep factor is `1 - alpha`.
///
/// `seed0` is where the seed sum starts: `+0.0` or `-0.0`, which differ only when every seeded
/// input is `-0.0` (the old `EmaState` started at `+0.0`, the `Iterator::sum` folds at `-0.0`).
/// `n == 0` never seeds: the average stays `NaN`, as it did.
#[derive(Clone)]
struct EmaK {
    nf: f64,
    alpha: f64,
    keep: f64,
    seed0: f64,
    /// Inputs still to go before the seed (0 once seeded; 0 from the start for `n == 0`).
    warm: usize,
    seed: f64,
    previous: f64,
}

impl EmaK {
    #[inline]
    fn new(n: usize, seed0: f64) -> Self {
        let alpha = vike_math::smoothing::exponential_moving_average_alpha(n);
        EmaK {
            nf: n as f64,
            alpha,
            keep: 1.0 - alpha,
            seed0,
            warm: n,
            seed: seed0,
            previous: f64::NAN,
        }
    }

    fn clear(&mut self) {
        self.warm = self.nf as usize;
        self.seed = self.seed0;
        self.previous = f64::NAN;
    }

    /// The value the next input would give, committed to nothing.
    #[inline(always)]
    fn next(&self, x: f64) -> f64 {
        self.alpha * x + self.keep * self.previous
    }

    /// The steady-state step. Only valid once seeded (or for `n == 0`, where it keeps `NaN`).
    #[inline(always)]
    fn step(&mut self, x: f64) -> f64 {
        self.previous = self.alpha * x + self.keep * self.previous;
        self.previous
    }

    /// Any step: the seed ladder while warming up, else [`EmaK::step`].
    #[inline(always)]
    fn push(&mut self, x: f64) -> f64 {
        if self.warm == 0 {
            return self.step(x);
        }
        self.warm -= 1;
        self.seed += x;
        if self.warm == 0 {
            self.previous = self.seed / self.nf;
            self.previous
        } else {
            f64::NAN
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Chaikin A/D oscillator.
// ------------------------------------------------------------------------------------------------

/// Chaikin A/D oscillator: the fast average of the accumulation/distribution line minus the slow
/// one, `NaN` until both have seeded. The line is `AdCore`'s sum of `clv * volume` (a bar with no
/// range adds nothing), kept from `-0.0` so the first flow is added exactly.
#[derive(Clone)]
pub(crate) struct AdoscCore {
    accumulator: f64,
    fast: EmaK,
    slow: EmaK,
    /// Pushes still to go before both averages are seeded; 0 is the branch-free steady state.
    warm: usize,
}

impl AdoscCore {
    #[inline]
    pub(crate) fn new(fast: usize, slow: usize) -> Self {
        AdoscCore {
            accumulator: -0.0,
            fast: EmaK::new(fast, 0.0),
            slow: EmaK::new(slow, 0.0),
            warm: fast.max(slow),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.accumulator = -0.0;
        self.fast.clear();
        self.slow.clear();
        self.warm = self.fast.nf.max(self.slow.nf) as usize;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64, v: f64) -> f64 {
        let rng = h - l;
        let clv = if rng != 0.0 { ((c - l) - (h - c)) / rng } else { 0.0 };
        self.accumulator += clv * v;
        if self.warm == 0 {
            let f = self.fast.step(self.accumulator);
            let s = self.slow.step(self.accumulator);
            return f - s;
        }
        self.warm -= 1;
        let f = self.fast.push(self.accumulator);
        let s = self.slow.push(self.accumulator);
        f - s
    }
}

// ------------------------------------------------------------------------------------------------
// Volume-weighted average prices.
// ------------------------------------------------------------------------------------------------

/// `1 / 3`, so a typical price is a multiply where the textbook divides (a divide costs about four
/// multiplies of throughput and the VWAPs already pay one for their output); the result differs from
/// `(h + l + c) / 3.0` in the last bit at most.
const THIRD: f64 = 1.0 / 3.0;

/// Milliseconds in a UTC day (`Bar::ts` is epoch milliseconds).
const DAY_MS: i64 = 86_400_000;

/// Volume-weighted average price restarted each UTC day: the running sum of typical price times
/// volume over the running sum of volume, both zeroed when the day (`ts / 86_400_000`, truncating)
/// changes. `NaN` while the day's volume is not positive. Beside them the volume-weighted sum of the
/// squared price, for [`VwapCore::deviation`]; it moves no bit of the average.
#[derive(Clone)]
pub(crate) struct VwapCore {
    pv: f64,
    vol: f64,
    ppv: f64,
    last_day: i64,
    /// A day's first bar answers the price it weighted, exactly.
    first_price: bool,
}

impl VwapCore {
    #[inline]
    pub(crate) fn new() -> Self {
        VwapCore { pv: 0.0, vol: 0.0, ppv: 0.0, last_day: i64::MIN, first_price: false }
    }

    pub(crate) fn clear(&mut self) {
        *self = VwapCore { first_price: self.first_price, ..Self::new() };
    }

    /// A day's first bar answers the price itself (see `Vwap::with_first_bar_price`).
    pub(crate) fn set_first_price(&mut self, first_price: bool) {
        self.first_price = first_price;
    }

    /// The volume-weighted standard deviation of the weighted price about `vwap` over the day so
    /// far, `sqrt(max(0, sum(v p^2) / sum(v) - vwap^2))`; `NaN` while the day's volume is not
    /// positive or `vwap` is `NaN`.
    pub(crate) fn deviation(&self, vwap: f64) -> f64 {
        let var = self.ppv / self.vol - vwap * vwap;
        if self.vol > 0.0 && !var.is_nan() { var.max(0.0).sqrt() } else { f64::NAN }
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, ts: i64, h: f64, l: f64, c: f64, v: f64) -> f64 {
        self.push_price(ts, (h + l + c) * THIRD, v)
    }

    /// [`VwapCore::push`] weighting `price` instead of the typical price.
    #[inline(always)]
    pub(crate) fn push_price(&mut self, ts: i64, price: f64, v: f64) -> f64 {
        let day = ts / DAY_MS;
        let fresh = day != self.last_day;
        if fresh {
            self.pv = 0.0;
            self.vol = 0.0;
            self.ppv = 0.0;
            self.last_day = day;
        }
        self.pv += price * v;
        self.vol += v;
        self.ppv += v * (price * price);
        if fresh && self.first_price {
            return price;
        }
        let out = self.pv / self.vol;
        if self.vol > 0.0 { out } else { f64::NAN }
    }
}

/// Volume-weighted average price accumulated from the first bar at or after `anchor_timestamp` (epoch
/// milliseconds), nothing restarting. `NaN` before the anchor and while no volume has accumulated.
#[derive(Clone)]
pub(crate) struct AnchoredVwapCore {
    anchor: f64,
    started: bool,
    pv: f64,
    vol: f64,
}

impl AnchoredVwapCore {
    #[inline]
    pub(crate) fn new(anchor_timestamp: f64) -> Self {
        AnchoredVwapCore { anchor: anchor_timestamp, started: false, pv: 0.0, vol: 0.0 }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new(self.anchor);
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, ts: i64, h: f64, l: f64, c: f64, v: f64) -> f64 {
        // Once started it stays started, whatever a later timestamp says.
        self.started |= (ts as f64) >= self.anchor;
        if !self.started {
            return f64::NAN;
        }
        let typical_price = (h + l + c) * THIRD;
        self.pv += typical_price * v;
        self.vol += v;
        let out = self.pv / self.vol;
        if self.vol > 0.0 { out } else { f64::NAN }
    }
}

// ------------------------------------------------------------------------------------------------
// Elder force index, ease of movement.
// ------------------------------------------------------------------------------------------------

/// Elder force index: the average of `(close - previous close) * volume`. A bar whose force is
/// `NaN` (the first bar, or a `NaN` price or volume) is neither pushed nor emitted, so the average
/// carries on from the last defined force. The seed sum starts at `-0.0`, as the kernel's did.
#[derive(Clone)]
pub(crate) struct EfiCore {
    previous: f64,
    ema: EmaK,
}

impl EfiCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        EfiCore { previous: f64::NAN, ema: EmaK::new(n, -0.0) }
    }

    pub(crate) fn clear(&mut self) {
        self.previous = f64::NAN;
        self.ema.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64, v: f64) -> f64 {
        let raw = (c - self.previous) * v;
        self.previous = c;
        // `alpha * raw` is made here, before the branch, and used on both sides of it, so the compiler
        // cannot sink it next to `keep * previous` and fuse the two products into one packed multiply with
        // a shuffle on each side of it, which sits on the loop-carried chain (measured: 2.8 ns a value
        // fused, 1.4 ns apart). It is `NaN` exactly when `raw` is.
        let scaled = self.ema.alpha * raw;
        if self.ema.warm == 0 {
            // Steady state. A `NaN` force makes `next` `NaN` too, so ONE test on the output (the
            // value the recurrence produces anyway) finds it; testing `raw` first made the loop twice
            // as slow (measured), because the compiler then orders the average behind that branch.
            // `next` is also `NaN` for a poisoned average, which does advance, so ask `raw` there.
            let next = scaled + self.ema.keep * self.ema.previous;
            if next.is_nan() && raw.is_nan() {
                return f64::NAN;
            }
            self.ema.previous = next;
            return next;
        }
        if scaled.is_nan() {
            return f64::NAN;
        }
        self.ema.push(raw)
    }
}

/// Ease of movement: `(midpoint move) / (volume / (high - low))` per bar (`0` when the bar has no
/// range or no volume, `NaN` on the first bar), averaged over the last `n` DEFINED values. A bar
/// whose value is `NaN` is neither pushed nor emitted.
#[derive(Clone)]
pub(crate) struct EomCore {
    started: bool,
    previous_mid: f64,
    sma: PeakSmaCore,
}

impl EomCore {
    #[inline]
    pub(crate) fn new(n: usize) -> Self {
        EomCore { started: false, previous_mid: f64::NAN, sma: PeakSmaCore::new(n) }
    }

    pub(crate) fn clear(&mut self) {
        self.started = false;
        self.previous_mid = f64::NAN;
        self.sma.clear();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, v: f64) -> f64 {
        // `* 0.5` is `/ 2.0` exactly.
        let mid = (h + l) * 0.5;
        let mid_move = mid - self.previous_mid;
        self.previous_mid = mid;
        if !self.started {
            // No previous bar: no value, and nothing for the average.
            self.started = true;
            return f64::NAN;
        }
        let hl = h - l;
        let raw = if hl != 0.0 && v != 0.0 { mid_move / (v / hl) } else { 0.0 };
        if raw.is_nan() {
            return f64::NAN;
        }
        self.sma.push(raw)
    }
}

// ------------------------------------------------------------------------------------------------
// Klinger volume oscillator, volume oscillator.
// ------------------------------------------------------------------------------------------------

/// Klinger volume oscillator: the volume signed by whether the typical price `(h + l + c) / 3` rose
/// strictly (`-` otherwise, a `NaN` previous price included), averaged fast and slow; `kvo` is
/// their difference and `signal` an average of the defined `kvo`. The first bar has no force. A bar
/// whose force is `NaN` (a `NaN` volume) or whose `kvo` is `NaN` advances nothing downstream.
#[derive(Clone)]
pub(crate) struct KvoCore {
    started: bool,
    previous: f64,
    fast: EmaK,
    slow: EmaK,
    sig: EmaK,
    /// Defined forces still to go before both averages are seeded; 0 is the steady state.
    warm: usize,
}

impl KvoCore {
    #[inline]
    pub(crate) fn new(fast: usize, slow: usize, signal: usize) -> Self {
        KvoCore {
            started: false,
            previous: f64::NAN,
            fast: EmaK::new(fast, 0.0),
            slow: EmaK::new(slow, 0.0),
            sig: EmaK::new(signal, 0.0),
            warm: fast.max(slow),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.started = false;
        self.previous = f64::NAN;
        self.fast.clear();
        self.slow.clear();
        self.sig.clear();
        self.warm = self.fast.nf.max(self.slow.nf) as usize;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64, v: f64) -> [f64; 2] {
        let hlc3 = vike_math::price_transforms::typical_price(h, l, c);
        let before = self.previous;
        self.previous = hlc3;
        if !self.started {
            self.started = true;
            return [f64::NAN; 2];
        }
        let vf = v * if hlc3 > before { 1.0 } else { -1.0 };
        if self.warm == 0 {
            // Steady state: the `NaN` tests sit on the output of the averages (see `EfiCore::push`).
            // A `NaN` force gives a `NaN` `kvo` and advances nothing; a `NaN` `kvo` from a poisoned
            // average advances the averages and not the signal.
            let f = self.fast.next(vf);
            let s = self.slow.next(vf);
            let kvo = f - s;
            if kvo.is_nan() {
                if vf.is_nan() {
                    return [f64::NAN; 2];
                }
                self.fast.previous = f;
                self.slow.previous = s;
                return [f64::NAN; 2];
            }
            self.fast.previous = f;
            self.slow.previous = s;
            return [kvo, self.sig.push(kvo)];
        }
        if vf.is_nan() {
            return [f64::NAN; 2];
        }
        self.warm -= 1;
        let kvo = self.fast.push(vf) - self.slow.push(vf);
        if kvo.is_nan() {
            return [f64::NAN; 2];
        }
        [kvo, self.sig.push(kvo)]
    }
}

/// Volume oscillator: the percentage gap of a short average of the volume above a long one,
/// `(short - long) / long * 100`, `NaN` until both have seeded and wherever the long average is `0`.
/// The seed sums start at `-0.0`, as the kernel's did.
#[derive(Clone)]
pub(crate) struct VolumeOscCore {
    short: EmaK,
    long: EmaK,
    warm: usize,
}

impl VolumeOscCore {
    #[inline]
    pub(crate) fn new(short: usize, long: usize) -> Self {
        VolumeOscCore {
            short: EmaK::new(short, -0.0),
            long: EmaK::new(long, -0.0),
            warm: short.max(long),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.short.clear();
        self.long.clear();
        self.warm = self.short.nf.max(self.long.nf) as usize;
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, v: f64) -> f64 {
        let (s, l) = if self.warm == 0 {
            (self.short.step(v), self.long.step(v))
        } else {
            self.warm -= 1;
            (self.short.push(v), self.long.push(v))
        };
        let out = (s - l) / l * 100.0;
        // A `NaN` average already makes `out` `NaN`; a zero long average must too.
        if l != 0.0 { out } else { f64::NAN }
    }
}

// ------------------------------------------------------------------------------------------------
// Cumulative series driven by the close and the volume.
// ------------------------------------------------------------------------------------------------

/// Signed volume: the bar's volume when the close rose, its negation when it fell, `0` otherwise
/// (and on the first bar, and beside a `NaN` close). Branch-free: the sign bit of the volume is
/// flipped by the comparison, and a bar that did not move takes the select's other arm.
#[derive(Clone)]
pub(crate) struct NetVolumeCore {
    previous: f64,
}

impl NetVolumeCore {
    #[inline]
    pub(crate) fn new() -> Self {
        NetVolumeCore { previous: f64::NAN }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64, v: f64) -> f64 {
        let pc = self.previous;
        self.previous = c;
        let down = c < pc;
        let signed = f64::from_bits(v.to_bits() ^ (u64::from(down) << 63));
        if (c > pc) | down { signed } else { 0.0 }
    }
}

/// The percentage change of the close from the previous one, `0` when that close is zero.
#[inline(always)]
pub(crate) fn rocp(c: f64, pc: f64) -> f64 {
    let r = (c - pc) / pc;
    if pc != 0.0 { r } else { 0.0 }
}

/// Negative (`POSITIVE == false`) or positive (`true`) volume index: starts at 1000 and is
/// multiplied by `1 + rocp(close)` on the bars whose volume fell (rose) from the previous bar, and by
/// `1.0` (an exact no-op) on the others. No previous bar is a `NaN` volume, which neither
/// comparison is true for.
#[derive(Clone)]
pub(crate) struct VolumeIndexCore<const POSITIVE: bool> {
    pc: f64,
    pv: f64,
    index: f64,
}

impl<const POSITIVE: bool> VolumeIndexCore<POSITIVE> {
    #[inline]
    pub(crate) fn new() -> Self {
        VolumeIndexCore { pc: f64::NAN, pv: f64::NAN, index: 1000.0 }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64, v: f64) -> f64 {
        let hit = if POSITIVE { v > self.pv } else { v < self.pv };
        let r = rocp(c, self.pc);
        // Whether the volume moved the right way is a coin toss in the data, so the factor is picked by
        // an integer mask, not by an `if`: the code generator turns a select over a value that comes
        // out of a division into a branch, and a branch mispredicted every other bar costs three times
        // what the division does (measured: 3.6 ns with the select, 1.3 ns with the mask).
        let moved = (1.0 + r).to_bits();
        let mask = u64::from(hit).wrapping_neg();
        let factor = f64::from_bits((moved & mask) | (1.0f64.to_bits() & !mask));
        self.index *= factor;
        self.pc = c;
        self.pv = v;
        self.index
    }
}

/// Price volume trend: a running sum of each bar's volume times the close's percentage change; zero
/// on the first bar.
#[derive(Clone)]
pub(crate) struct PvtCore {
    started: bool,
    pc: f64,
    accumulator: f64,
}

impl PvtCore {
    #[inline]
    pub(crate) fn new() -> Self {
        PvtCore { started: false, pc: 0.0, accumulator: 0.0 }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, c: f64, v: f64) -> f64 {
        if self.started {
            self.accumulator += v * rocp(c, self.pc);
        }
        self.started = true;
        self.pc = c;
        self.accumulator
    }
}

/// Williams' accumulation/distribution: a running sum of the close's distance above the true low on
/// an up close, below the true high on a down close (true high and low include the previous close),
/// nothing on an unchanged close. Ignores volume; zero on the first bar.
///
/// The true low is `if l < pc { l } else { pc }` where the kernel used `f64::min`: the two differ only
/// for a `NaN` previous close, and that arm is only reached when the close rose past a previous close
/// that is a number.
#[derive(Clone)]
pub(crate) struct WilliamsAdCore {
    previous: f64,
    accumulator: f64,
}

impl WilliamsAdCore {
    #[inline]
    pub(crate) fn new() -> Self {
        WilliamsAdCore { previous: f64::NAN, accumulator: 0.0 }
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, h: f64, l: f64, c: f64) -> f64 {
        let pc = self.previous;
        self.previous = c;
        let true_low = if l < pc { l } else { pc };
        let true_high = if h > pc { h } else { pc };
        // Up or down is a coin toss in the data: both arms are computed from registers and the choice is
        // a select over them (a branch here, mispredicted every other bar, measured 4.8 ns against 1.0).
        let up = c - true_low;
        let down = c - true_high;
        let flat_or_down = if c < pc { down } else { 0.0 };
        let step = if c > pc { up } else { flat_or_down };
        self.accumulator += step;
        self.accumulator
    }
}
