//! Batch compute primitives + the column view — the shared f64 kernels every
//! indicator's `vectorize` folds through. Faithful extraction of the
//! moving-average / true-range family from vike-trader-app
//! `core/indicators/base.py`.
//!
//! PARITY: these are the reference math. Warm-up positions emit `f64::NAN`
//! exactly as the Python oracle; fold order is load-bearing (naive left folds,
//! `.iter().sum()` == `fold(0.0, +)`). Never widen a tolerance to make a test
//! pass — investigate the divergence.

use vike_marketdata::Bar;

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

/// High, low and close columns resolved from a window's bars.
///
/// ⚠ **Ask for the columns a kernel reads, with [`Columns::needing`].** A [`Bar`] is about 120
/// bytes (it carries optional funding, bid, ask and a symbol), so every column built is a full pass
/// over the bars: building all six for a kernel that reads the close tripled-to-sextupled its memory
/// traffic (measured: a 50,000-bar EMA batch cost 48 ns a bar, almost all of it this). A column that
/// was not requested is an EMPTY `Vec`, so a kernel that indexes one it did not ask for panics in
/// the first test that reaches it rather than reading zeros.
pub(crate) struct Columns {
    pub h: Vec<f64>,
    pub l: Vec<f64>,
    pub c: Vec<f64>,
}

/// Which columns of [`Columns`] a kernel reads; combine with `|`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Need(u8);

impl Need {
    pub(crate) const HIGH: Need = Need(2);
    pub(crate) const LOW: Need = Need(4);
    pub(crate) const CLOSE: Need = Need(8);

    fn has(self, other: Need) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for Need {
    type Output = Need;
    fn bitor(self, rhs: Need) -> Need {
        Need(self.0 | rhs.0)
    }
}

impl Columns {
    /// Only the requested columns, in ONE pass over `bars`. The values are the bars' own fields, so
    /// a column built here is bit-identical to the one the earlier all-columns constructor built.
    pub(crate) fn needing(bars: &[Bar], need: Need) -> Self {
        let n = bars.len();
        let col = |want: Need| if need.has(want) { Vec::with_capacity(n) } else { Vec::new() };
        let (mut h, mut l) = (col(Need::HIGH), col(Need::LOW));
        let mut c = col(Need::CLOSE);
        for b in bars {
            if need.has(Need::HIGH) {
                h.push(b.high);
            }
            if need.has(Need::LOW) {
                l.push(b.low);
            }
            if need.has(Need::CLOSE) {
                c.push(b.close);
            }
        }
        Columns { h, l, c }
    }
}

/// Simple moving average — each window RECOMPUTED as a naive left fold, carrying nothing between
/// bars. The same fold [`sma_nan`] below already uses, and the same one
/// `base/windowed.rs`'s `batch_bollinger` uses: this is the crate's majority shape, not a new one.
///
/// ⚠ **This was a sliding `sum += c[i] - c[i - n]`, and that is what made the indicators built on
/// it wrong past `hist_indicator!`'s history drain.** A carried sum's value at bar `i` holds error
/// from bar 0, so the window is mathematically finite while the ARITHMETIC is not: re-run the
/// kernel over a truncated tail and the identical window returns different bits. No `KEEP_FACTOR`
/// repairs that — the error is a random walk, not a decaying tail — which is why `ac`, `dpo`,
/// `envelopes`, `eom`, `kst`, `stochf`, `stochrsi`, `trima` and both `bbands_*` rows sat in
/// `parity/past_the_trim.rs`'s `NOT_TRUNCATION_INVARIANT`. Folding the window makes the value a pure function of
/// the window, which is exactly the precondition the drain depends on: MEASURED 0 mismatching
/// retained lengths out of 64 (`period = 20`) and 232 (`period = 200`), against 64/232 before.
///
/// ⚠ **A hand-written mirror must move with this and is ONE commit with it** —
/// `crates/vike-indicators/src/indicators/base/incremental.rs`'s `Sma`, documented as bit-identical to
/// this function. If it lags, `parity/stream_vs_batch.rs`'s `on_bar_equals_vectorize_for_every_indicator` goes
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
/// The patterns' old `avg_body` called this over the bar bodies and fed the threshold comparisons
/// behind all 63 pattern series, so this function's last bits decided whether a pattern fired (the
/// pattern cores now fold the same ten bodies the same way themselves; this function remains for
/// the flip-rate measurement below). This comment used to say the change "WILL flip some +/-100
/// signals on bars that sat exactly on a threshold" — reasonable, and **wrong**:
/// `patterns/flip_rate.rs`'s `measure_the_threshold_flip_rate_from_deaccumulating_sma` reconstructs the old
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
#[cfg(test)]
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
