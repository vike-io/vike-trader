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
//! * `lag` — both values are genuinely used. Feature z-scores want `lag = 1`; `vol_pct_168` wants
//!   `lag = 0` over a 169-bar window, deliberately, so that `vol_pct_168 > 0.5` agrees with
//!   `vol_regime` (at `lag = 1` they disagree on ~4% of rows).
//!
//! # The fold is naive, on purpose
//!
//! `sum()` here is a plain left fold, NOT `vike_model::py_sum`. `batch_var` used a naive fold and
//! `tests/window_pin.rs` pins its exact bits; a compensated sum would change registered indicator
//! output. The variance is a TWO-PASS fold (mean, then `Σ(x - mean)²`) rather than the O(1)
//! `E[x²] - E[x]²` form — see `indicators/statistics.rs`'s `batch_var` doc for the two measured
//! bugs that form caused, the second of which flipped a `sd != 0.0` guard between a number and a
//! NaN on flat data. Cohort bias is bounded and pins flat for hours, so that is the first failure
//! this kernel would hit.
//!
//! # ⚠ A CONSTANT window is zero by DECISION, not by cancellation
//!
//! The two-pass fold is not enough on its own, and the claim that it is — "a window of identical
//! values has `x - mean == 0` exactly" — was written in this file and in `batch_var`'s doc, was
//! FALSE, and was gated by a test that could not see its own violation. `mean` is `Σx / n`, and
//! `n` copies of `v` summed then divided by `n` is only `v` again when the rounding happens to
//! cancel. It does for `100.0` (the fixture both tests used) and for a funding rate of `1.25e-5`;
//! it does NOT for `0.1`, where 24 copies fold to `3fb999999999999b` against the value's
//! `3fb999999999999a`. Each deviation is then ~1.4e-17 instead of `0`, the variance is
//! rounding-sized instead of zero, and [`zscore`] divides a tiny numerator by a tiny denominator —
//! MEASURED against the oracle on real `funding_rate` data as `0.9789450103725608` on one export
//! window and ±1e14 on another, where pandas returns NaN. A ±1e14 cell in a column that feeds ML
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
//! (pandas 3.0.1 / numpy 2.4.3, 2026-08-10) over a real `funding_rate` series, one identical
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
//! `crates/vike-indicators/tests/parity.rs`'s
//! `a_constant_window_decides_the_zero_variance_guards_the_same_way_with_and_without_history`).
//! **Reproducing pandas here would mean reintroducing it by hand**, so the port is deliberately
//! not bit-identical to the oracle on a constant window. Measured over the seven export windows of
//! 2026-08-10, which contain 708 constant 24-hour `funding_rate` windows between them: the oracle
//! answers NaN on **699** of them — where this kernel now agrees and previously did not — and
//! answers a frozen-residue FINITE value on **9**, whose magnitudes run to `2.6e8` and `1.19e5`.
//! Trading 699 wrong cells for 9 that disagree with an artifact is the whole of the argument.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One group, as a slice `per_group` can take.
    ///
    /// Spelled through a function rather than as a `[a..b]` literal because
    /// `clippy::single_range_in_vec_init` reads that literal as a possible typo for `[a; b]` — an
    /// array of `b` copies of `a` — and refuses it under `-D warnings`. Naming the intent once
    /// beats an `#[allow]` at four call sites.
    ///
    /// ⚠ It does NOT hide `clippy::reversed_empty_ranges`, which fires on the literal at the CALL
    /// site regardless of what it is passed to. `an_inverted_group_is_refused` therefore still
    /// carries its own allow, and must.
    fn one(g: std::ops::Range<usize>) -> [std::ops::Range<usize>; 1] {
        [g]
    }

    /// THE bug this function exists to make unrepresentable, driven directly.
    ///
    /// Two assets stacked in one column. Applied to the WHOLE slice, the kernel's window walks
    /// straight across the boundary and the second asset's first rows average in the first
    /// asset's tail — silently, with a plausible number where a warm-up NaN belongs.
    #[test]
    fn a_kernel_applied_per_group_cannot_reach_across_a_group_boundary() {
        // asset A: four 100s. asset B: four 1s. A 3-period mean.
        let x = [100.0, 100.0, 100.0, 100.0, 1.0, 1.0, 1.0, 1.0];
        let spec = WindowSpec::indicator(3);

        // WITHOUT grouping: index 4 is B's first row and index 5 its second, and both average in
        // A's 100s. This is the defect, asserted so the fix is measured against it rather than
        // assumed.
        let flat = rolling_mean(&x, spec);
        assert!(flat[4] > 60.0, "the ungrouped kernel should be contaminated, got {}", flat[4]);
        assert!(flat[5] > 30.0, "the ungrouped kernel should be contaminated, got {}", flat[5]);

        // WITH grouping: B's first two rows are warm-up NaN, and its third is a clean 1.0.
        let out = per_group(&x, &[0..4, 4..8], "close", |s| rolling_mean(s, spec)).unwrap();
        assert!(out[0].is_nan() && out[1].is_nan(), "A's own warm-up must survive");
        assert_eq!(out[2], 100.0);
        assert_eq!(out[3], 100.0);
        assert!(out[4].is_nan(), "B's first row must be warm-up NaN, got {}", out[4]);
        assert!(out[5].is_nan(), "B's second row must be warm-up NaN, got {}", out[5]);
        assert_eq!(out[6], 1.0, "B's first complete window is its own data alone");
        assert_eq!(out[7], 1.0);
    }

    /// A row no group covers stays NaN rather than silently taking a neighbour's value.
    #[test]
    fn rows_outside_every_group_stay_nan() {
        let x = [1.0, 2.0, 3.0, 4.0];
        let groups = one(0..2);
        let out = per_group(&x, &groups, "v", |s| s.to_vec()).unwrap();
        assert_eq!(out[0], 1.0);
        assert_eq!(out[1], 2.0);
        assert!(out[2].is_nan() && out[3].is_nan(), "uncovered rows must not be filled");
    }

    /// A kernel that returns the wrong length is a REFUSAL, never a silent truncation — the same
    /// failure mode the grouping exists to prevent, one level up.
    #[test]
    fn a_kernel_that_returns_the_wrong_length_is_refused() {
        let x = [1.0, 2.0, 3.0, 4.0];
        let groups = one(0..4);
        let e = per_group(&x, &groups, "close", |_| vec![0.0; 2]).unwrap_err();
        match e {
            WindowError::KernelLength { src, expected, got } => {
                assert_eq!(src, "close");
                assert_eq!(expected, 4);
                assert_eq!(got, 2);
            }
            other => panic!("expected KernelLength, got {other:?}"),
        }
    }

    /// A group reaching past the end of the column is refused rather than panicking on the slice.
    #[test]
    fn a_group_out_of_bounds_is_refused_rather_than_panicking() {
        let x = [1.0, 2.0];
        let groups = one(0..5);
        let e = per_group(&x, &groups, "close", |s| s.to_vec()).unwrap_err();
        assert!(matches!(e, WindowError::GroupOutOfBounds { .. }), "got {e:?}");
    }

    /// An inverted range is refused too — `start > end` would otherwise panic inside the slice.
    #[test]
    fn an_inverted_group_is_refused() {
        let x = [1.0, 2.0, 3.0];

        // An inverted range IS what this test asserts against, so clippy's refusal of the literal
        // is exactly backwards here.
        #[allow(clippy::reversed_empty_ranges)]
        let groups = one(2..1);
        let e = per_group(&x, &groups, "close", |s| s.to_vec()).unwrap_err();
        assert!(matches!(e, WindowError::GroupOutOfBounds { .. }), "got {e:?}");
    }

    #[test]
    fn indicator_preset_is_population_variance_over_the_inclusive_window() {
        // [1,2,3]: mean 2, population var = ((1)+(0)+(1))/3
        let v = rolling_var(&[1.0, 2.0, 3.0], WindowSpec::indicator(3));
        assert!(v[0].is_nan() && v[1].is_nan());
        assert_eq!(v[2], 2.0 / 3.0);
    }

    #[test]
    fn point_in_time_preset_is_sample_variance_and_excludes_the_current_row() {
        // At i=3 the window is [1,2,3] (row 3 excluded); sample var = 2/2 = 1.
        let v = rolling_var(&[1.0, 2.0, 3.0, 99.0], WindowSpec::point_in_time(3));
        assert!(v[0].is_nan() && v[1].is_nan() && v[2].is_nan());
        assert_eq!(v[3], 1.0);
    }

    #[test]
    fn std_is_the_sqrt_of_var_against_a_known_value() {
        // Same fixture as the population-variance test above: var = 2/3, so std = sqrt(2/3).
        // `rolling_std` is a named Task 2 deliverable with no caller yet — a dropped `sqrt` would
        // otherwise ship green.
        let s = rolling_std(&[1.0, 2.0, 3.0], WindowSpec::indicator(3));
        assert!(s[0].is_nan() && s[1].is_nan());
        assert!((s[2] - (2.0f64 / 3.0).sqrt()).abs() < 1e-15);
    }

    #[test]
    fn a_flat_window_has_exactly_zero_variance_not_a_rounding_artifact() {
        // ⚠ This fixture passes with OR without the constant-window check, and for years it was
        // the only one: `100.0` is exact in binary, `100*4/4` is `100.0`, so the two-pass fold
        // cancels to zero by luck of the value. The sibling below is the one that can fail.
        let v = rolling_var(&[100.0; 8], WindowSpec::indicator(4));
        assert_eq!(v[7], 0.0, "two-pass fold must return exact zero on a flat window");
    }

    #[test]
    fn a_flat_window_is_exactly_zero_even_when_its_naive_mean_is_not_the_value() {
        // The defect, minimised. 24 copies of 0.1 fold to a sum whose /24 is ONE ULP above 0.1,
        // so every deviation is ~1.4e-17 rather than 0 and the fold alone returns ~2e-34.
        // pandas (3.0.1, measured on the oracle's interpreter 2026-08-10):
        // `pd.Series([0.1]*24).rolling(24).std()` is bits 0000000000000000.
        let v = 0.1_f64;
        let x = [v; 24];
        assert_ne!(
            x.iter().sum::<f64>() / 24.0,
            v,
            "fixture is pointless unless the naive mean MISSES the repeated value"
        );
        let var = rolling_var(&x, WindowSpec::indicator(24));
        assert_eq!(
            var[23].to_bits(),
            0u64,
            "got {} — a window of identical values has variance exactly zero",
            var[23]
        );
    }

    #[test]
    fn zscore_is_nan_when_the_window_is_flat() {
        let z = zscore(&[100.0; 8], WindowSpec::indicator(4), None);
        assert!(z[7].is_nan(), "zero sd must yield NaN, never an infinity");
    }

    #[test]
    fn zscore_is_nan_on_a_flat_window_whose_naive_mean_misses_the_value() {
        // The live shape: `funding_rate` holding one value for 24 hours. Before the
        // constant-window check this returned a FINITE number — measured against the oracle as
        // 0.9789450103725608 on one export window and ±1e14 on another, both against a pandas
        // NaN. A ±1e14 cell in a feature column that feeds ML training dominates every split.
        let z = zscore(&[0.1_f64; 24], WindowSpec::indicator(24), None);
        assert!(z[23].is_nan(), "got {} — a zero-variance window has no z-score", z[23]);
    }

    #[test]
    fn a_flat_point_in_time_history_is_nan_even_when_the_current_row_differs() {
        // `funding_z_24h`'s own spec: the window EXCLUDES the current row, so the numerator here
        // is a real (0.5 - 0.1) sitting over a zero sd. The oracle divides by
        // `fstd.replace(0, np.nan)`, so the answer is NaN and NOT ±inf, and NOT the clip floor —
        // measured end-to-end through `features/price_context.py`'s expression, pandas 3.0.1.
        let mut x = vec![0.1_f64; 24];
        x.push(0.5);
        let z = zscore(&x, WindowSpec::point_in_time(24), Some(6.0));
        assert!(
            z[24].is_nan(),
            "got {} — a zero-sd history has no z-score at any numerator",
            z[24]
        );
    }

    #[test]
    fn a_variance_that_is_small_but_real_still_yields_a_finite_z() {
        // ⚠ The anti-blindness gate. The constant-window check must be EXACT equality over the
        // inputs; a magnitude threshold on the output would swallow this window, whose values
        // differ by a single ULP and whose variance is therefore genuinely non-zero.
        //
        // ⚠ NEITHER the sd NOR the z is compared against pandas' number here, and that is a
        // MEASURED residual rather than a shrug. On this window, both measured 2026-08-10:
        //
        //   sd — this kernel 2.790601e-17, pandas 3.0.1 (ddof = 1) 2.893719e-18: a factor of 9.6.
        //   z  — pandas -4.795831523312719; this kernel's differs in the same way and worse.
        //
        // It is arithmetic that predates the constant-window check and is untouched by it: the
        // mean here is the NAIVE fold `Σx / n`, which on 24 copies of 0.1 lands 2 ULP above the
        // true mean, while pandas' lands on it (measured: bits 3fb999999999999a against this
        // kernel's 3fb999999999999b). When the window's ENTIRE spread is
        // one ULP the mean's own rounding error is the LARGER quantity and it dominates the
        // deviations — 23 of them become 2 ULP instead of 1/24 of one. The arithmetic checks out
        // both ways: `sqrt((23·2² + 1²)/23)` is 2.01 ULP of 0.1 = 2.79e-17, and the exact-mean
        // `sqrt((23·(1/24)² + (23/24)²)/23)` is 0.204 ULP = 2.83e-18. The z is worse again because
        // its numerator is then a catastrophic cancellation. This is the regime immediately
        // OUTSIDE the constant case, it is UNCALIBRATED, and a caller who needs accuracy there
        // wants a compensated mean or Welford — not an epsilon, which would swallow the window
        // whole rather than merely rounding it.
        //
        // What this test gates is the property the fix must not break: the window is NOT
        // constant, so a number comes out — and it is a tiny one, which is what proves the
        // fixture sits in the near-flat regime rather than being a fat window in disguise.
        let v = 0.1_f64;
        let mut x = [v; 24];
        x[5] = f64::from_bits(v.to_bits() + 1); // nextafter(0.1, +inf) — one ULP
        let spec = WindowSpec { period: 24, lag: 0, min_periods: 24, ddof: 1 };
        let sd = rolling_std(&x, spec)[23];
        assert!(sd > 0.0 && sd.is_finite(), "got {sd} — a one-ULP spread is a REAL variance");
        assert!(
            (1e-20..1e-15).contains(&sd),
            "sd {sd} left the near-flat regime — this fixture no longer tests the boundary"
        );
        assert!(
            zscore(&x, spec, None)[23].is_finite(),
            "the constant-window check swallowed a real variance"
        );
    }

    #[test]
    fn clip_bounds_a_positive_excursion() {
        // `point_in_time`'s window at i=19 is x[0..=18] — it EXCLUDES x[19] itself (lag=1). A
        // perfectly flat history there would hit the zero-sd -> NaN guard (see the test above),
        // which is not what this test means to exercise, so x[18] is the one nonzero row in that
        // history: enough for a real (small) sd, so the outlier at x[19] produces a huge but
        // FINITE z.
        let mut x = vec![0.0; 20];
        x[18] = 1.0; // the one nonzero row in an otherwise flat history
        x[19] = 1_000.0; // an enormous excursion against that near-flat history
        let unclipped = zscore(&x, WindowSpec::point_in_time(19), None);
        let clipped = zscore(&x, WindowSpec::point_in_time(19), Some(6.0));
        assert!(unclipped[19] > 6.0);
        assert_eq!(clipped[19], 6.0);
    }

    #[test]
    fn clip_bounds_a_negative_excursion() {
        // Mirror of the positive case above: `Some(c)` clips to `[-c, c]`, not just `<= c`, and
        // nothing in the file exercised the lower bound — `z.max(-c).min(c)` would pass every
        // other test here even with no lower bound at all (e.g. a stray `z.min(c)`).
        let mut x = vec![0.0; 20];
        x[18] = 1.0; // the one nonzero row in an otherwise flat history
        x[19] = -1_000.0; // an enormous excursion the other way
        let unclipped = zscore(&x, WindowSpec::point_in_time(19), None);
        let clipped = zscore(&x, WindowSpec::point_in_time(19), Some(6.0));
        assert!(unclipped[19] < -6.0);
        assert_eq!(clipped[19], -6.0);
    }

    #[test]
    fn clipping_in_range_values_leaves_their_relative_order_unchanged() {
        // A short oscillating series gives two different, well-inside-the-clip z-scores at two
        // complete windows; clip(6.0) must not touch either, so both their values AND their
        // relative order survive untouched.
        let x = [1.0, 2.0, 1.0, 3.0, 1.0, 4.0, 1.0, 5.0];
        let unclipped = zscore(&x, WindowSpec::indicator(4), None);
        let clipped = zscore(&x, WindowSpec::indicator(4), Some(6.0));
        assert!(unclipped[6] < unclipped[7], "fixture must produce two distinct in-range z-scores");
        assert_eq!(clipped[6], unclipped[6]);
        assert_eq!(clipped[7], unclipped[7]);
        assert!(clipped[6] < clipped[7]);
    }

    #[test]
    fn a_nan_inside_the_window_propagates_to_nan() {
        let v = rolling_var(&[1.0, f64::NAN, 3.0], WindowSpec::indicator(3));
        assert!(v[2].is_nan());
    }

    #[test]
    fn a_zero_period_is_not_a_window() {
        let v = rolling_var(&[1.0, 2.0], WindowSpec { period: 0, lag: 0, min_periods: 0, ddof: 0 });
        assert!(v.iter().all(|x| x.is_nan()));
    }

    #[test]
    fn rank_pct_of_the_largest_value_in_its_window_is_one() {
        let r = rolling_rank_pct(&[1.0, 2.0, 3.0, 4.0], WindowSpec::indicator(4));
        assert_eq!(r[3], 1.0);
    }

    #[test]
    fn rank_pct_of_the_smallest_value_is_one_over_n() {
        let r = rolling_rank_pct(&[4.0, 3.0, 2.0, 1.0], WindowSpec::indicator(4));
        assert_eq!(r[3], 0.25);
    }

    #[test]
    fn ties_take_the_average_rank_like_pandas() {
        // window [1,2,2,4], current value 4 -> rank 4 -> 1.0
        let r = rolling_rank_pct(&[1.0, 2.0, 2.0, 4.0], WindowSpec::indicator(4));
        assert_eq!(r[3], 1.0);
        // window [2,2,4,2], current value 2: two below-or-equal ties + itself
        // less = 0, eq = 3 -> rank = 0 + (3+1)/2 = 2 -> 0.5
        let r2 = rolling_rank_pct(&[2.0, 2.0, 4.0, 2.0], WindowSpec::indicator(4));
        assert_eq!(r2[3], 0.5);
    }

    #[test]
    fn rank_pct_with_lag_one_ranks_the_previous_row_not_the_current_one() {
        // At i=3, lag=1, period=3: the window is [1,2,3] and the ranked value is x[2] = 3.
        let r = rolling_rank_pct(&[1.0, 2.0, 3.0, 99.0], WindowSpec::point_in_time(3));
        assert_eq!(r[3], 1.0);
    }

    #[test]
    fn median_of_an_odd_window_is_the_middle_value() {
        let m = rolling_median(&[3.0, 1.0, 2.0], WindowSpec::indicator(3));
        assert_eq!(m[2], 2.0);
    }

    #[test]
    fn median_of_an_even_window_averages_the_two_middle_values() {
        let m = rolling_median(&[4.0, 1.0, 3.0, 2.0], WindowSpec::indicator(4));
        assert_eq!(m[3], 2.5);
    }

    #[test]
    fn median_is_nan_before_the_window_is_complete() {
        let m = rolling_median(&[1.0, 2.0, 3.0], WindowSpec::indicator(3));
        assert!(m[0].is_nan() && m[1].is_nan());
    }

    #[test]
    fn a_missing_current_value_stays_nan_under_a_clip() {
        // The window [1..=8] is complete and non-degenerate; row 8 is the missing observation.
        // `window_at` inspects rows `i - lag - period + 1 ..= i - lag`, so at `lag = 1` the
        // CURRENT row is never checked — which is the sparse-cohort shape exactly: `3x smart`
        // absent while the 24/72/168h history behind it is full.
        let mut x: Vec<f64> = (1..=8).map(|v| v as f64).collect();
        x.push(f64::NAN);
        let z = zscore(&x, WindowSpec::point_in_time(8), Some(6.0));
        assert!(z[8].is_nan(), "got {} — f64::max ignores NaN, so the clip invented a value", z[8]);
    }

    #[test]
    fn an_unclipped_missing_value_was_already_nan_and_stays_nan() {
        let mut x: Vec<f64> = (1..=8).map(|v| v as f64).collect();
        x.push(f64::NAN);
        assert!(zscore(&x, WindowSpec::point_in_time(8), None)[8].is_nan());
    }
}
