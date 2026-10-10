//! ONE parameterised window-statistics kernel over `&[f64]` — the single home for rolling
//! mean / variance / std / percentile-rank / median / z-score in this workspace.
//!
//! # Why one function and not two
//!
//! A chart indicator and a point-in-time model feature want different z-scores: the indicator's
//! window ENDS at the current row and divides by `n`; a feature's window EXCLUDES the current row
//! (pandas' `shift(1)`), divides by `n - 1`, and clips. Those are three arguments, not three
//! implementations — so [`WindowSpec`] carries them and the registered `zscore`/`var` indicators
//! are presets of the same code the research features use.
//!
//! # Two parameters that are load-bearing
//!
//! * `ddof` — the indicator divides by `period` (population); pandas' `rolling().std()` defaults
//!   to `ddof = 1` (sample). Sharing a kernel WITHOUT this argument silently changes one caller
//!   or the other, and nothing fails.
//! * `lag` — both values are genuinely used. A point-in-time feature z-score wants `lag = 1`, so
//!   the window cannot contain the row it scores. A rolling percentile whose value is COMPARED
//!   against a categorical label derived from the same window wants `lag = 0` over a window one
//!   bar longer, deliberately, so the two agree row for row instead of on most rows — at `lag = 1`
//!   they disagree on a small but non-trivial fraction of them, which is a silent disagreement
//!   between a threshold and its own label rather than a visible failure. (The specific columns
//!   and the threshold that pairing uses are a caller's, and are deliberately not named here.)
//!
//! # The fold is naive, on purpose
//!
//! `sum()` here is a plain left fold, NOT `vike_model::py_sum`. `batch_var` used a naive fold and
//! `tests/window_pin.rs` pins its exact bits; a compensated sum would change registered indicator
//! output. The variance is a TWO-PASS fold (mean, then `Σ(x - mean)²`) rather than the O(1)
//! `E[x²] - E[x]²` form — see `indicators/statistics.rs`'s `batch_var` doc for the two measured
//! bugs that form caused, the second of which flipped a `sd != 0.0` guard between a number and a
//! NaN on flat data. A bounded input — a sentiment-style series in `[-1, 1]` — pins flat for
//! hours, so that is the first failure this kernel would hit.
//!
//! # ⚠ A CONSTANT window is zero by DECISION, not by cancellation
//!
//! The two-pass fold is not enough on its own, and the claim that it is — "a window of identical
//! values has `x - mean == 0` exactly" — was written in this file and in `batch_var`'s doc, was
//! FALSE, and was gated by a test that could not see its own violation. `mean` is `Σx / n`, and
//! `n` copies of `v` summed then divided by `n` is only `v` again when the rounding happens to
//! cancel. It does for `100.0` (the fixture both tests used) and for a rate of `1.25e-5`;
//! it does NOT for `0.1`, where 24 copies fold to `3fb999999999999b` against the value's
//! `3fb999999999999a`. Each deviation is then ~1.4e-17 instead of `0`, the variance is
//! rounding-sized instead of zero, and [`zscore`] divides a tiny numerator by a tiny denominator —
//! MEASURED against the oracle on a real hourly rate series as an order-1 value on one stretch and
//! ±1e14 on another, where pandas returns NaN. A ±1e14 cell in a column that feeds ML
//! training is not a rounding artifact; it dominates every split the learner makes.
//!
//! So `is_constant_window` decides it, and the answer is the MATHEMATICS plus numpy's two-pass —
//! `np.std([v] * 24, ddof=1)` is `0.0` — deliberately NOT pandas' rolling accumulator.
//!
//! # ⚠ pandas' `rolling().std()` is NOT the authority here, and this was MEASURED, not assumed
//!
//! The obvious justification for the paragraph above is "pandas returns exactly 0.0 on a constant
//! window, so we must too". **That is false, and believing it is how this defect was found in the
//! first place.** `roll_var` is an ONLINE add/remove accumulator, so its value on a given window
//! is a function of every row that preceded it. Measured on the oracle's own interpreter
//! (pandas 3.0.1 / numpy 2.4.3) over a real hourly rate series, one identical
//! 24-hour constant window with different amounts of history in front of it:
//!
//! | rows of history before the window | `rolling(24).std()` |
//! |---|---|
//! | 0, 1, 4, 24, 54 | `0.0` |
//! | 154 | `5.177070960435764e-14` |
//! | 254 | `2.379637956941763e-13` |
//!
//! ...and it is not a run-length question either: appending **1000** further copies of the same
//! value leaves the answer at `2.379637956941763e-13`, frozen, because once the window is constant
//! every subsequent add and remove moves the accumulator by exactly zero and the residue it
//! already carried stays forever. `np.std` on the same 24 values is `0.0`.
//!
//! That is precisely the failure this workspace already removed from its own kernels — a variance
//! that is a function of all history rather than of the window, deciding NaN-vs-number
//! (`indicators/statistics.rs`'s `batch_var` doc; the gate is
//! `crates/vike-indicators/tests/parity/constant_window.rs`'s
//! `a_constant_window_decides_the_zero_variance_guards_the_same_way_with_and_without_history`).
//! **Reproducing pandas here would mean reintroducing it by hand**, so the port is deliberately
//! not bit-identical to the oracle on a constant window. Measured over a real hourly rate series
//! and its constant 24-hour windows: on the overwhelming majority of them the oracle answers NaN —
//! where this kernel now agrees and previously did not — and on a residual few percent it answers
//! a frozen-residue FINITE value, whose magnitude runs as high as ~`1e8`. Trading the many wrong
//! cells for the few that disagree with an artifact is the whole of the argument. (The counts and
//! the export they were taken over are a caller's private measurement and are deliberately not
//! written here; the RATIO and the magnitude are what carry the argument.)
//!
//! ⚠ The predicate is an EXACT equality over the INPUTS, never a magnitude threshold on the
//! OUTPUT, and the difference is the whole point. A window whose values are all bit-equal has a
//! true variance of exactly `0` — there is no rounding to be near. A window holding two distinct
//! values, however close, has a true variance above `0` and keeps whatever the fold returns: one
//! value moved by a single ULP off 24 copies of `0.1` gives pandas `std = 2.8937187931548476e-18`
//! and a finite `z` of `-4.795831523312719`, and it must stay finite here too. An epsilon would be
//! a second law with a number in it, and it would swallow that window.

use crate::math::sq;

/// How a window is taken: its length, how far back it ends, how many observations it needs, and
/// which variance convention it uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowSpec {
    /// Number of values in the window.
    pub period: usize,
    /// How many rows back the window ENDS. `0` includes the current row; `1` is pandas `shift(1)`.
    pub lag: usize,
    /// Fewer non-NaN observations than this yields `NaN` rather than a partial estimate.
    ///
    /// ⚠ Only meaningful today when equal to `period` — both shipped presets set it that way, and
    /// the six functions below do not agree on what a SMALLER `min_periods` would mean: the
    /// mean/variance/std/zscore family folds over the whole window regardless, so a NaN inside it
    /// still propagates to NaN (the knob buys nothing there, it just changes nothing before the
    /// fold does); `rolling_rank_pct` divides by `period`, not by the count of survivors, so a
    /// smaller `min_periods` would NOT reproduce pandas' rank-of-a-partial-window semantics its own
    /// doc cites; `rolling_median` silently takes the median of whatever non-NaN values survive. A
    /// caller wanting a real "partial window" policy must pick one of those three behaviors and
    /// make the others match it — this field does not do that for you.
    pub min_periods: usize,
    /// `0` = population (divide by `n`), `1` = sample (divide by `n - 1`, pandas' default).
    pub ddof: u8,
}

impl WindowSpec {
    /// The registered-indicator preset: window ends at the current row, population variance.
    pub fn indicator(period: usize) -> Self {
        Self { period, lag: 0, min_periods: period, ddof: 0 }
    }

    /// The point-in-time feature preset: window EXCLUDES the current row, sample variance.
    pub fn point_in_time(period: usize) -> Self {
        Self { period, lag: 1, min_periods: period, ddof: 1 }
    }
}

/// What [`per_group`] refuses.
///
/// This crate's kernels deliberately never fail — they answer `NaN` and, as [`rolling_median`]'s
/// doc puts it, have "no business panicking on a caller's bad float". Both variants here are the
/// other thing: a broken CONTRACT rather than awkward data, where continuing would write a wrong
/// number into a column nobody would think to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowError {
    /// A kernel returned a different number of values than the group it was handed.
    ///
    /// Filling what arrived and leaving the rest would shift every later value in that group by
    /// the shortfall — the same silent-misalignment failure [`per_group`] exists to prevent,
    /// one level up.
    KernelLength { src: String, expected: usize, got: usize },
    /// A group range is inverted or reaches past the end of the column.
    ///
    /// Slicing would panic; answering is worse. Groups are normally DERIVED from the same index
    /// as the column, so this fires when they have come from somewhere else and drifted.
    GroupOutOfBounds { src: String, start: usize, end: usize, len: usize },
}

impl std::fmt::Display for WindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowError::KernelLength { src, expected, got } => write!(
                f,
                "{src}: the kernel returned {got} values for a group of {expected} — a partial \
                 fill would misalign every later row in that group"
            ),
            WindowError::GroupOutOfBounds { src, start, end, len } => write!(
                f,
                "{src}: group {start}..{end} does not fit a column of {len} — the groups did not \
                 come from this column's index"
            ),
        }
    }
}

impl std::error::Error for WindowError {}

/// Apply a window kernel to each GROUP of a stacked column, independently, and stitch the results
/// back into one column.
///
/// # The bug this makes unrepresentable
///
/// A panel holds several instruments in ONE column, one after another. Run any kernel above over
/// the whole slice and its window walks straight across the boundary: the second instrument's
/// first rows average in the first instrument's tail. There is no error and no `NaN` — just a
/// plausible number where a warm-up `NaN` belongs, in a feature that then correlates with
/// something. Stack four values of `100.0` ahead of four of `1.0`, take a 3-period mean, and the
/// fifth output is `67.0` for an instrument that never traded above `1.0`.
///
/// `groups` are half-open ranges into `x`, normally DERIVED from the same index the column was
/// built from so the two cannot disagree. Rows no group covers stay `NaN`: a panel with a gap
/// must not silently inherit a neighbour's value.
///
/// `src` names the column and appears in both errors — a length mismatch reported without saying
/// WHICH column is a message that sends the reader back to the call site to guess.
///
/// ```
/// use vike_indicators::window::{per_group, rolling_mean, WindowSpec};
/// let x = [100.0, 100.0, 100.0, 100.0, 1.0, 1.0, 1.0, 1.0];
/// let spec = WindowSpec::indicator(3);
/// let out = per_group(&x, &[0..4, 4..8], "close", |s| rolling_mean(s, spec)).unwrap();
/// assert!(out[4].is_nan()); // the second group's own warm-up, not the first group's tail
/// assert_eq!(out[6], 1.0);
/// ```
pub fn per_group<K>(
    x: &[f64],
    groups: &[std::ops::Range<usize>],
    src: &str,
    mut kernel: K,
) -> Result<Vec<f64>, WindowError>
where
    K: FnMut(&[f64]) -> Vec<f64>,
{
    let mut out = vec![f64::NAN; x.len()];
    for g in groups {
        if g.start > g.end || g.end > x.len() {
            return Err(WindowError::GroupOutOfBounds {
                src: src.to_string(),
                start: g.start,
                end: g.end,
                len: x.len(),
            });
        }
        let got = kernel(&x[g.start..g.end]);
        if got.len() != g.end - g.start {
            return Err(WindowError::KernelLength {
                src: src.to_string(),
                expected: g.end - g.start,
                got: got.len(),
            });
        }
        out[g.start..g.end].copy_from_slice(&got);
    }
    Ok(out)
}

/// The slice of `x` this spec's window covers for output index `i`, or `None` if there is no
/// complete window there yet.
fn window_at(x: &[f64], i: usize, s: WindowSpec) -> Option<&[f64]> {
    if s.period == 0 {
        return None;
    }
    let end = i.checked_sub(s.lag)?;
    if end + 1 < s.period {
        return None;
    }
    let w = &x[end + 1 - s.period..=end];
    if w.iter().filter(|v| !v.is_nan()).count() < s.min_periods {
        return None;
    }
    Some(w)
}

/// Rolling arithmetic mean. `NaN` until the window is complete.
pub fn rolling_mean(x: &[f64], s: WindowSpec) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    for (i, o) in out.iter_mut().enumerate() {
        if let Some(w) = window_at(x, i, s) {
            *o = w.iter().sum::<f64>() / s.period as f64;
        }
    }
    out
}

/// Whether every value in `w` is the same float, so its variance is exactly `0` with no
/// arithmetic performed at all.
///
/// ⚠ **This is the ONE home for that law, and it is `pub(crate)` for exactly that reason.** It was
/// private while [`rolling_var`] was its only caller, and the two-pass folds in
/// `crates/vike-indicators/src/indicators/volatility.rs` — which do NOT route through this module —
/// carried the defect this predicate removes for as long as that stayed true. A second spelling of
/// the same condition is the failure mode, not the private-ness: the callers are named in
/// `is_constant_window_is_the_only_spelling_of_the_constant_window_law` in `tests/parity.rs`, which
/// fails when a hand-rolled copy appears.
///
/// ⚠ This is NOT "what pandas does" — pandas' rolling accumulator carries history and does not
/// reliably answer zero here at all. See the module doc for the measurement and for why matching
/// it would be a regression. Three properties are load-bearing:
///
/// * **NaN is never constant.** `NaN != NaN`, so a window of two-or-more NaNs falls through to the
///   fold and stays NaN. The explicit `is_nan` guard covers the one-element case, where `rest` is
///   empty and `all` would vacuously answer `true` — that window is `NaN`, not `0.0`.
/// * **`-0.0 == 0.0`.** A window mixing the two signed zeros is constant, because it holds one
///   real number, and the variance of one real number repeated is zero.
/// * **It reads only the window.** The answer is a pure function of the `period` values in front
///   of it, which is the property `tests/parity.rs`'s truncation-invariance gate exists to keep.
pub(crate) fn is_constant_window(w: &[f64]) -> bool {
    match w.split_first() {
        Some((first, rest)) => !first.is_nan() && rest.iter().all(|v| v == first),
        None => false,
    }
}

/// Rolling variance, two-pass. `ddof` selects population (`0`) or sample (`1`).
///
/// A window whose values are all equal returns exactly `0.0` without folding — see
/// `is_constant_window` and the module doc's "A CONSTANT window is zero by DECISION" section for
/// why the fold alone does not get there and why an epsilon would be the wrong cure. Every other
/// window is byte-identical to what this function returned before that check existed.
pub fn rolling_var(x: &[f64], s: WindowSpec) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    let denom = s.period as f64 - s.ddof as f64;
    if denom <= 0.0 {
        return out;
    }
    for (i, o) in out.iter_mut().enumerate() {
        let Some(w) = window_at(x, i, s) else { continue };
        if is_constant_window(w) {
            *o = 0.0;
            continue;
        }
        let mean = w.iter().sum::<f64>() / s.period as f64;
        *o = w.iter().map(|v| sq(v - mean)).sum::<f64>() / denom;
    }
    out
}

/// Rolling standard deviation — `rolling_var().sqrt()`.
pub fn rolling_std(x: &[f64], s: WindowSpec) -> Vec<f64> {
    rolling_var(x, s).into_iter().map(f64::sqrt).collect()
}

/// Rolling percentile rank in `[0, 1]` of the window's LAST value against the whole window,
/// with pandas' `method="average"` tie handling: a tied group takes the mean of the ranks it
/// spans, so `less + (equal + 1) / 2`.
///
/// Note this ranks the value at the window's end — which is the current row at `lag = 0` and the
/// previous one at `lag = 1`.
pub fn rolling_rank_pct(x: &[f64], s: WindowSpec) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    for (i, o) in out.iter_mut().enumerate() {
        let Some(w) = window_at(x, i, s) else { continue };
        let v = w[w.len() - 1];
        if v.is_nan() {
            continue;
        }
        let less = w.iter().filter(|a| **a < v).count() as f64;
        let equal = w.iter().filter(|a| **a == v).count() as f64;
        *o = (less + (equal + 1.0) / 2.0) / s.period as f64;
    }
    out
}

/// Rolling median. An even-length window averages the two middle values, as pandas does.
///
/// Sorts a copy of each window rather than maintaining a skip list: the windows here are at most
/// 169 wide over a few thousand rows, so the simple form is fast enough and has no incremental
/// state to get wrong — the failure mode `batch_var`'s doc records.
pub fn rolling_median(x: &[f64], s: WindowSpec) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    let mut buf: Vec<f64> = Vec::with_capacity(s.period);
    for (i, o) in out.iter_mut().enumerate() {
        let Some(w) = window_at(x, i, s) else { continue };
        buf.clear();
        buf.extend(w.iter().copied().filter(|v| !v.is_nan()));
        if buf.is_empty() {
            continue;
        }
        buf.sort_by(|a, b| a.partial_cmp(b).expect("NaN filtered above"));
        let n = buf.len();
        *o = if n % 2 == 1 { buf[n / 2] } else { (buf[n / 2 - 1] + buf[n / 2]) / 2.0 };
    }
    out
}

/// Rolling z-score of the CURRENT value against its window, optionally clipped to `±clip`.
///
/// A zero standard deviation yields `NaN` (an undefined z-score), never an infinity.
///
/// `clip`, when given, must be finite and non-negative — it is used as `[-clip, clip]`. Bounded
/// with `max`/`min` rather than `f64::clamp` deliberately: `clamp` PANICS if the resolved bounds
/// are inverted or NaN, which a negative or NaN `clip` would do, and this is a pure-math kernel
/// with no business panicking on a caller's bad float.
pub fn zscore(x: &[f64], s: WindowSpec, clip: Option<f64>) -> Vec<f64> {
    let mean = rolling_mean(x, s);
    let var = rolling_var(x, s);
    let mut out = vec![f64::NAN; x.len()];
    for i in 0..x.len() {
        let sd = var[i].sqrt();
        if sd.is_nan() || sd == 0.0 {
            continue;
        }
        let z = (x[i] - mean[i]) / sd;
        if z.is_nan() {
            // `x[i]` is itself NaN while its TRAILING window is complete. Without this guard the
            // clip INVENTS a value: `f64::max` returns the non-NaN operand by definition, so
            // `f64::NAN.max(-6.0).min(6.0)` is `-6.0` — a missing observation handed to a model as
            // the most extreme bearish reading the band allows, on ~70% of rows of an entire
            // feature family. `f64::clamp` would PANIC instead, which is why the code uses
            // max/min at all; the fix is to reach neither.
            continue;
        }
        out[i] = match clip {
            Some(c) => z.max(-c).min(c),
            None => z,
        };
    }
    out
}

#[path = "window_tests.rs"]
#[cfg(test)]
mod window_tests;
