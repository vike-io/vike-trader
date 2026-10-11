//! The per-trial return vectors a search retains, and the anti-overfitting statistics over them.

use std::collections::HashMap;

use super::Candidate;
use crate::harness::sweep::ParamscanRow;
use crate::search::trials::candidate_key;

#[cfg(doc)]
use super::{SearchOutcome, StoreEvaluator};
#[cfg(doc)]
use crate::harness::sweep::{ParamscanReport, ReturnBuckets};

/// **The per-trial return vectors one search retained, keyed by candidate** — the side channel
/// [`StoreEvaluator`] fills and [`overfit_stats`] joins against the ranked rows.
///
/// ⚠ **A SIDE CHANNEL rather than a field on [`ParamscanRow`], and the arithmetic behind that is
/// already written down one module over.** `crate::search::trials`'s module doc measured the
/// alternatives when the trial LEDGER needed per-evaluation information: "a field on
/// [`SearchOutcome`] would be 6 struct-literal edits, a field on [`ParamscanRow`] 19". Both counts
/// still hold, and a row field would also cost something a decorator cannot: [`ParamscanReport`]
/// CROSSES THE DATAHUB WIRE
/// (`vike_datahub_client::proto::Response::ParamscanReport`), so a `#[serde(skip)]` 4 KB field
/// would sit on the wire type and
/// `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
/// `profile_sweep_is_byte_identical_local_and_remote` would then rest on an ATTRIBUTE being right
/// rather than on the type carrying nothing new at all.
///
/// ⚠ **Keyed by `crate::search::trials::candidate_key`, which is exact and total** — a
/// [`Candidate`] is `Vec<(String, toml::Value)>` and cannot be a map key on its own (no `Eq`, no
/// `Hash`); that function keys floats by BIT PATTERN and escapes its own separators, so two
/// distinct candidates cannot collide. A candidate evaluated TWICE inside one search (a resume
/// whose warm cache missed a ledger line it holds) overwrites its own entry with an identical
/// vector, because the same candidate over the same data produces the same curve — a repeat is not
/// a conflict.
#[derive(Debug, Clone, Default)]
pub struct CapturedReturns {
    pub(super) buckets: usize,
    pub(super) by_candidate: HashMap<String, Vec<f64>>,
}

impl CapturedReturns {
    /// The bucket count that was REQUESTED, which is not the same as a column's LENGTH:
    /// `ReturnBuckets::capture` shortens a column over a range holding fewer returns than
    /// buckets. `crate::trial_ledger::OverfitStats`'s `buckets` reports the length actually used.
    pub fn buckets(&self) -> usize {
        self.buckets
    }

    /// How many trials retained a vector.
    pub fn len(&self) -> usize {
        self.by_candidate.len()
    }

    /// Whether nothing was retained — the state of every search that did not opt in.
    pub fn is_empty(&self) -> bool {
        self.by_candidate.is_empty()
    }

    /// This candidate's retained vector, if it has one. A row whose point FAILED, was answered
    /// from a warm CACHE, or produced a non-finite block return has none.
    pub fn get(&self, overrides: &Candidate) -> Option<&[f64]> {
        self.by_candidate.get(&candidate_key(overrides)).map(Vec::as_slice)
    }
}

/// **The CSCV split count `overfit_stats` uses**, and the reason a `T` of 512 is sized the way it
/// is.
///
/// Sixteen is López de Prado's own setting for CSCV and the one `pbo_cscv`'s doc is written
/// against. It is fixed rather than a knob because the number is not free: `pbo_cscv` iterates
/// `C(S, S/2)` combinations — `C(16, 8)` = 12 870 — and each one takes two column-mean passes over
/// half the matrix, so the whole statistic is `~C(S, S/2) * T * N` multiply-adds. At `S = 16`,
/// `T = 512`, `N = 500` that is a few billion, i.e. seconds, paid ONCE at the end of a search that
/// already took hours. `S = 20` would be four times that for no extra resolution, and `S = 8`
/// (`C(8,4)` = 70) gives a PBO estimated from seventy logits.
///
/// ⚠ `pbo_cscv` PANICS on an odd count — the CSCV construction has no meaning without an even
/// split — so this constant being even is load-bearing, not cosmetic.
pub const DEFAULT_CSCV_SPLITS: usize = 16;

/// **The anti-overfitting statistics of a whole search, computed once over the ranked rows** —
/// PBO via CSCV, the correlation-corrected effective trial count, and the deflated Sharpe
/// benchmarked against it.
///
/// `None` when there is nothing to compute — no search opted in, every trial failed, or the range
/// was too short to give `T >= splits`. A `None` here is why
/// `crate::trial_ledger::TrialsDocument::overfit` is an `Option`: a document that carried a zeroed
/// block would read as "measured, and clean".
///
/// # What the numbers are computed over, stated because it is easy to assume otherwise
///
/// * **The matrix is `T` observations x `N` trials, TRANSPOSED here.** Each trial's retained vector
///   is `T` long and `pbo_cscv` indexes `matrix[t][j]`, so the columns are built by transposition;
///   that costs one more `T * N` allocation (2 MB at the defaults) and is the whole reason
///   `effective_n_trials`, which wants the PER-TRIAL series, is fed `series` instead.
/// * **Column ORDER is the ranked row order**, so the statistic is deterministic even though the
///   evaluator filled its map from rayon workers in completion order. That matters: `pbo_cscv`'s
///   first-argmax tie-break reads column index, so a map-iteration order would make the PBO of a
///   search with tied points depend on scheduling.
/// * **`observed_sharpe` is the WINNER's**, i.e. ranked row 0 — the configuration a search actually
///   hands onward (`--export-params`, a walk-forward's chosen parameters). Deflating the mean or
///   the median would answer a question nobody asks of a search.
/// * **`n_obs` is `T`, not the bar count**, and that is not a loss of power:
///   [`ReturnBuckets`]'s own doc carries the invariance argument (`sr_per_obs * sqrt(n - 1)` is
///   preserved by compounding).
/// * **A row with no retained vector is EXCLUDED and counted**, never zero-filled. The reachable
///   causes are a failed point, a resume's reused trial (whose answer is a score, not a report —
///   `crate::search::trials::WarmTrial` says why), and a curve that touched exactly zero.
///   `excluded` is what makes a thin matrix visible instead of merely small.
///
/// ⚠ **Every statistic is reported as `None` rather than as a number when it is uncomputable.**
/// `pbo_cscv` answers `NaN` for a degenerate matrix, which `overfit::overfit_verdict` reads as
/// "not assessed" — a `0.0` would read as "no overfit", the exact opposite. The verdict is carried
/// alongside so a reader does not have to re-derive the thresholds.
pub fn overfit_stats(
    rows: &[ParamscanRow],
    captured: &CapturedReturns,
    splits: usize,
) -> Option<crate::trial_ledger::OverfitStats> {
    use vike_analytics::overfit;

    if captured.is_empty() || splits < 2 {
        return None;
    }
    // Per-trial series in RANKED row order — see the ordering note above.
    let mut series: Vec<Vec<f64>> = Vec::new();
    let mut excluded = 0usize;
    let mut t = 0usize;
    for row in rows {
        match captured.get(&row.overrides) {
            // The first admitted column fixes `T`. A column of another length cannot be
            // time-aligned with it (bucket `b` would cover a different fraction of the range), so
            // it is excluded rather than truncated. Unreachable while every trial of a search runs
            // one `[data]` range, which is why this is a belt and not a documented mode.
            Some(col) if t == 0 || col.len() == t => {
                t = col.len();
                series.push(col.to_vec());
            }
            _ => excluded += 1,
        }
    }
    if series.is_empty() || t < splits {
        return None;
    }

    // T x N, the shape `pbo_cscv` indexes.
    let matrix: Vec<Vec<f64>> = (0..t).map(|i| series.iter().map(|col| col[i]).collect()).collect();
    let pbo = overfit::pbo_cscv(&matrix, splits);

    // Each trial's PER-OBSERVATION moments, derived through the ONE home for the two unit
    // conventions (`overfit::sharpe_moments`) rather than re-spelled here: a pseudo-curve whose
    // successive ratios ARE the bucket returns is what that function wants, and
    // `vike_analytics::metrics::returns` recovers them from it.
    let moments: Vec<overfit::SharpeMoments> =
        series.iter().map(|col| overfit::sharpe_moments(&curve_from_returns(col))).collect();
    let trial_sharpes: Vec<f64> = moments.iter().map(|m| m.sr_per_obs).collect();
    let best = moments[0];
    let effective_n = overfit::effective_n_trials(&series);
    let deflated = overfit::deflated_sharpe_with_effective_n(
        best.sr_per_obs,
        &trial_sharpes,
        &series,
        best.n_obs,
        best.skew,
        best.kurt,
    );
    // ⚠ `pbo` is handed on as-is, NaN included: `overfit_verdict`'s first branch is the
    // "not assessed" one and reads that NaN deliberately. Collapsing it to a number first would
    // delete the only signal that the matrix was degenerate.
    let verdict = overfit::overfit_verdict(pbo, deflated, None);

    Some(crate::trial_ledger::OverfitStats {
        buckets: t,
        requested_buckets: captured.buckets(),
        trials: series.len(),
        excluded,
        splits,
        pbo: finite(pbo),
        effective_n: finite(effective_n),
        deflated_sharpe: finite(deflated),
        observed_sharpe: finite(best.sr_per_obs),
        observations: best.n_obs,
        verdict: match verdict.level {
            overfit::OverfitLevel::Low => "low",
            overfit::OverfitLevel::Medium => "medium",
            overfit::OverfitLevel::High => "high",
        }
        .to_string(),
    })
}

/// A non-finite statistic becomes `None`, so the document says "uncomputable" in one spelling.
///
/// ⚠ Not cosmetic: `crates/vike-cli/src/cmd/runs/jsondoc.rs`'s `number_at` already filters a
/// non-finite leaf to `None`, and `serde_json::Number` cannot hold one — so a `NaN` written into a
/// document either serializes as `null` or, on a format that refuses it, fails the whole write.
/// Making the ABSENCE explicit in the type means `backtest gate --fail-if` reports "carries no
/// finite number under `overfit.pbo`" rather than a missing key, which names the right problem.
fn finite(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}

/// A pseudo equity curve whose successive RATIOS are `returns` — the shape
/// `overfit::sharpe_moments` takes.
///
/// ⚠ **This exists so the two unit conventions have ONE home.** `sharpe_moments`'s doc is emphatic
/// that `sr_per_obs` must be per-period (an annualized Sharpe saturates PSR to ~1.0 and makes the
/// significance test vacuous) and that `kurt` must be rebased to non-excess, and it derives both
/// from an equity CURVE. Computing mean/std/skew/kurtosis over the bucket returns directly would
/// be a second spelling of `vike_analytics::metrics`' conventions, which is exactly the
/// duplication `crates/vike-ops/tests/docs/one_authority_gate.rs` exists to refuse.
///
/// Starts at `1.0`, so the curve is a growth index and no cash scale is invented. The roundtrip is
/// not bit-exact — `metrics::returns` recomputes `c[i]/c[i-1] - 1` from a product — but the
/// statistic is over hundreds of samples and the error is last-bit; a bit-exact path would mean
/// duplicating the moment formulas, which is the thing being avoided.
pub(super) fn curve_from_returns(returns: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(returns.len() + 1);
    let mut level = 1.0f64;
    out.push(level);
    for r in returns {
        level *= 1.0 + r;
        out.push(level);
    }
    out
}
