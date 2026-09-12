//! Batch compute primitives + the column view — the shared f64 kernels every
//! indicator's `vectorize` folds through. Faithful extraction of the
//! moving-average / true-range family from vike-trader-app
//! `core/indicators/base.py`.
//!
//! PARITY: these are the reference math. Warm-up positions emit `f64::NAN`
//! exactly as the Python oracle; fold order is load-bearing (naive left folds,
//! `.iter().sum()` == `fold(0.0, +)`). Never widen a tolerance to make a test
//! pass — investigate the divergence.

use vike_model::Bar;

/// `x * x`, and the ONE spelling of it this crate uses — never `x.powi(2)`.
///
/// ⚠ **MEASURED 2026-08-25: `f64::powi` is not bit-stable across OPTIMISATION LEVELS on
/// Windows.** In a `dev` build MSVC lowers `powi` to the CRT's `pow()`, which IEEE 754 does not
/// require to be correctly rounded; in a `release` build LLVM expands the same call to a plain
/// multiply. Identical source therefore produced two different numbers on one box — and `dev` is
/// the profile `just t vike-indicators` and CI's roster lane actually run.
///
/// The measurement lives in `crates/vike-indicators/tests/libm_platform_probe.rs`'s module doc:
/// the four-cell table, plus the row that identifies the libcall by showing `powi(2)` hashing
/// EQUAL to `powf(2.0)` in exactly the one anomalous cell.
///
/// A plain multiply is what every other configuration already computes, so routing through here
/// moves NOTHING except that cell — and it is what lets `libm_cross_platform_pin` hold on the dev
/// box in the profile that box actually uses.
///
/// See `cube` / `quart` for the higher powers, which close the residual this doc used to declare.
#[inline]
pub(crate) fn sq(x: f64) -> f64 {
    x * x
}

/// `x³`, spelled as LLVM's own expansion spells it — never `x.powi(3)`.
///
/// ⚠ **The association order is the whole point, and a left-to-right chain is the WRONG one.**
/// `llvm.powi` with a constant exponent lowers by BINARY EXPONENTIATION, not by a running product:
/// it squares once and multiplies the square by the base. `x * x * x` parses as `(x * x) * x`,
/// which happens to agree here — but writing it as a chain invites the four-term case below to be
/// written the same way, where it does NOT agree. Both helpers are therefore spelled from the
/// square outward, so a reader sees one rule rather than two coincidences.
///
/// This is why converting these sites moves NOTHING on a platform whose LLVM already expanded
/// them, while fixing the one that emitted a libcall — the same argument `sq` makes, extended to
/// the exponents `sq` deliberately left behind. `libm_cross_platform_pin`'s committed table is
/// the evidence: it did not move when these landed.
#[inline]
pub(crate) fn cube(x: f64) -> f64 {
    let s = x * x;
    s * x
}

/// `x⁴` as `(x²)²` — never `x.powi(4)`, and never `x * x * x * x`.
///
/// ⚠ **This is the one where the naive chain genuinely disagrees.** LLVM squares the square;
/// a left-to-right product rounds after each of three multiplies instead of two, so
/// `((x * x) * x) * x` is a DIFFERENT f64 from `(x * x) * (x * x)` for inputs whose intermediate
/// products round. Converting to the chain would have moved values on every platform — which is
/// exactly what the residual note here used to predict, and why it read as a reason not to
/// convert at all. The prediction was right about the chain and wrong about the conversion: the
/// correct spelling is the square-of-the-square, and it moves nothing.
#[inline]
pub(crate) fn quart(x: f64) -> f64 {
    let s = x * x;
    s * s
}

/// OHLCV columns resolved from a window's bars, plus UTC-day session boundaries
/// (VWAP resets on each new session). `o` (open) is materialised for the price /
/// pattern / balance-of-power ports that read it.
pub(crate) struct Columns {
    pub o: Vec<f64>,
    pub h: Vec<f64>,
    pub l: Vec<f64>,
    pub c: Vec<f64>,
    pub v: Vec<f64>,
    pub new_session: Vec<bool>,
}

impl Columns {
    pub(crate) fn from_bars(bars: &[Bar]) -> Self {
        let mut new_session = vec![false; bars.len()];
        let mut last_day = i64::MIN;
        for (i, b) in bars.iter().enumerate() {
            let day = b.ts / 86_400_000; // UTC day (ts is epoch-ms)
            if day != last_day {
                new_session[i] = true;
                last_day = day;
            }
        }
        Columns {
            o: bars.iter().map(|b| b.open).collect(),
            h: bars.iter().map(|b| b.high).collect(),
            l: bars.iter().map(|b| b.low).collect(),
            c: bars.iter().map(|b| b.close).collect(),
            v: bars.iter().map(|b| b.volume).collect(),
            new_session,
        }
    }
}

/// Smooth the non-NaN tail of `src` with `ma(tail, period)` and scatter the
/// results back into a full-length list aligned to `src` — the port of
/// vike-trader-app `base.smooth_defined`, the shared "smooth the defined tail,
/// map back to aligned positions" form behind the multi-EMA/SMA indicators
/// (dema/tema/trima/trix/tsi/coppock/ac/kst/smi/t3/macd-signal…). Positions that
/// were NaN in `src` (warm-up / undefined) stay NaN, as do positions inside
/// `ma`'s own warm-up. Returns all-NaN if fewer than `period` defined values
/// exist. Fold order is load-bearing (matches the Python).
pub(crate) fn smooth_defined(
    src: &[f64],
    ma: impl Fn(&[f64], usize) -> Vec<f64>,
    period: usize,
) -> Vec<f64> {
    let defined: Vec<(usize, f64)> =
        src.iter().enumerate().filter(|(_, v)| !v.is_nan()).map(|(i, v)| (i, *v)).collect();
    let mut out = vec![f64::NAN; src.len()];
    if defined.len() >= period {
        let values: Vec<f64> = defined.iter().map(|(_, v)| *v).collect();
        let smoothed = ma(&values, period);
        for ((i, _), sv) in defined.iter().zip(smoothed) {
            out[*i] = sv;
        }
    }
    out
}

/// True-range array — faithful to vike-trader-app `volatility.py:true_range`:
/// `tr[0] = h[0] - l[0]`, later bars gap-aware `max(h-l, |h-cprev|, |l-cprev|)`.
/// Shared by the volatility (chop/atr) and momentum (vortex) ports.
pub(crate) fn true_range(h: &[f64], l: &[f64], c: &[f64]) -> Vec<f64> {
    let n = c.len();
    let mut out = vec![0.0; n];
    if n == 0 {
        return out;
    }
    out[0] = h[0] - l[0];
    for i in 1..n {
        out[i] = (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs());
    }
    out
}

/// Wilder ATR faithful to vike-trader-app `volatility.py:atr`: all-NaN when
/// `len <= period`; else seed = `mean(tr[1..=period])` placed at index `period`,
/// then the Wilder recurrence. NOTE this deliberately differs from [`atr`]/[`rma`]
/// (which seed with `tr[0..period]` at index `period-1`, matching the legacy
/// `base.py` behind the unchanged existing `Atr` indicator). Used by the new
/// natr/chop-adjacent and momentum `chande_kroll_stop` ports so they match the
/// current Python.
pub(crate) fn atr_v(h: &[f64], l: &[f64], c: &[f64], period: usize) -> Vec<f64> {
    let n = c.len();
    let mut out = vec![f64::NAN; n];
    if n <= period || period == 0 {
        return out;
    }
    let trs = true_range(h, l, c);
    let mut prev = trs[1..=period].iter().sum::<f64>() / period as f64;
    out[period] = prev;
    for i in (period + 1)..n {
        prev = (prev * (period as f64 - 1.0) + trs[i]) / period as f64;
        out[i] = prev;
    }
    out
}

/// Wilder's smoothing (RMA): seed = SMA of the first `n`, then recursive smoothing.
pub(crate) fn rma(x: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    if x.len() < n || n == 0 {
        return out;
    }
    let seed: f64 = x[..n].iter().sum::<f64>() / n as f64;
    out[n - 1] = seed;
    for i in n..x.len() {
        out[i] = (out[i - 1] * (n as f64 - 1.0) + x[i]) / n as f64;
    }
    out
}

/// Simple moving average — each window RECOMPUTED as a naive left fold, carrying nothing between
/// bars. The same fold [`sma_nan`] below already uses, and the same one
/// `base.rs`'s `batch_bollinger` uses: this is the crate's majority shape, not a new one.
///
/// ⚠ **This was a sliding `sum += c[i] - c[i - n]`, and that is what made the indicators built on
/// it wrong past `hist_indicator!`'s history drain.** A carried sum's value at bar `i` holds error
/// from bar 0, so the window is mathematically finite while the ARITHMETIC is not: re-run the
/// kernel over a truncated tail and the identical window returns different bits. No `KEEP_FACTOR`
/// repairs that — the error is a random walk, not a decaying tail — which is why `ac`, `dpo`,
/// `envelopes`, `eom`, `kst`, `stochf`, `stochrsi`, `trima` and both `bbands_*` rows sat in
/// `parity.rs`'s `NOT_TRUNCATION_INVARIANT`. Folding the window makes the value a pure function of
/// the window, which is exactly the precondition the drain depends on: MEASURED 0 mismatching
/// retained lengths out of 64 (`period = 20`) and 232 (`period = 200`), against 64/232 before.
///
/// ⚠ **Two hand-written mirrors must move with this and are ONE commit with it** —
/// `crates/vike-indicators/src/indicators/state.rs`'s `SmaAcc` and
/// `crates/vike-indicators/src/indicators/base.rs`'s `Sma`, both documented as bit-identical to
/// this function. If either lags, `parity.rs`'s `on_bar_equals_vectorize_for_every_indicator` goes
/// red at once, which is the cheap failure — but it cannot be landed kernel-by-kernel.
///
/// COST: O(len * n) instead of O(len). That is the trade the retention split pays for — see
/// `crates/vike-indicators/src/lib.rs`'s `WindowReach`, without which this change alone measures
/// 1.1x-36x SLOWER per streamed bar.
///
/// VALUES CHANGE in the last bits. Nothing pins them: this crate ships no golden fixtures and no
/// downstream crate asserts an indicator value, so the only contracts are `on_bar == vectorize`
/// and the truncation gate — both of which this strengthens.
///
/// ⚠ **The blast radius reaches the candlestick patterns, and it was MEASURED at zero.**
/// `crates/vike-indicators/src/indicators/patterns.rs`'s `avg_body` calls this over the bar bodies
/// and feeds the threshold comparisons behind all 63 pattern series, so this function's last bits
/// decide whether a pattern fires. This comment used to say the change "WILL flip some +/-100
/// signals on bars that sat exactly on a threshold" — reasonable, and **wrong**:
/// `patterns.rs`'s `measure_the_threshold_flip_rate_from_deaccumulating_sma` reconstructs the old
/// accumulator and counts the disagreements directly, and finds **none** — 0 differing `avg_body`
/// values in 200,000 bars, 0 flips across all nine threshold multipliers and ~40 comparison sites,
/// 0 `is_doji` disagreements.
///
/// ⚠ **Do not generalize that zero.** It is a fact about a TEN-bar window, not about this function.
/// A sliding sum diverges from a fresh one only when its terms span enough magnitude for
/// cancellation to bite, and ten candle bodies never do — the same test's detector check shows the
/// reconstruction diverging on 9 of 21 bars once a `1e16` term enters the window, which is what
/// makes the zero evidence rather than a broken measurement. The deep-period accumulators this
/// change also removed genuinely did drift; the body average never did.
pub(crate) fn sma(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if c.len() < n || n == 0 {
        return out;
    }
    for i in (n - 1)..c.len() {
        out[i] = c[i + 1 - n..=i].iter().sum::<f64>() / n as f64;
    }
    out
}

/// SMA that skips leading NaNs and only emits when the whole window is finite
/// (used to smooth the stochastic %K/%D, whose raw line is NaN during warm-up).
pub(crate) fn sma_nan(c: &[f64], n: usize) -> Vec<f64> {
    let len = c.len();
    let mut out = vec![f64::NAN; len];
    let start = c.iter().position(|x| !x.is_nan()).unwrap_or(len);
    if start + n > len || n == 0 {
        return out;
    }
    for i in (start + n - 1)..len {
        let w = &c[i + 1 - n..=i];
        if w.iter().all(|x| !x.is_nan()) {
            out[i] = w.iter().sum::<f64>() / n as f64;
        }
    }
    out
}

/// Exponential moving average: SMA seed at index `n-1`, then the alpha recurrence.
pub(crate) fn ema(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if c.len() < n || n == 0 {
        return out;
    }
    let alpha = 2.0 / (n as f64 + 1.0);
    out[n - 1] = c[..n].iter().sum::<f64>() / n as f64;
    for i in n..c.len() {
        out[i] = alpha * c[i] + (1.0 - alpha) * out[i - 1];
    }
    out
}

/// Weighted moving average (linear weights 1..=n), recomputed per window.
pub(crate) fn wma(c: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    if c.len() < n || n == 0 {
        return out;
    }
    let denom = (n * (n + 1) / 2) as f64;
    for i in (n - 1)..c.len() {
        let mut acc = 0.0;
        for k in 1..=n {
            // `i + k - n` == the oracle's `i - n + k` index, but the usize
            // intermediate never goes negative (i >= n-1, k >= 1 => i + k >= n).
            acc += k as f64 * c[i + k - n];
        }
        out[i] = acc / denom;
    }
    out
}

/// Average true range: true range then RMA-smoothed over `n`.
pub(crate) fn atr(h: &[f64], l: &[f64], c: &[f64], n: usize) -> Vec<f64> {
    let len = c.len();
    let mut tr = vec![f64::NAN; len];
    if len == 0 {
        return tr;
    }
    tr[0] = h[0] - l[0];
    for i in 1..len {
        tr[i] = (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs());
    }
    rma(&tr, n)
}
