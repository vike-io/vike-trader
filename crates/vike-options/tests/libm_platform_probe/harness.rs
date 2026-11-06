//! The hashing/corpus machinery every row and control arm folds its samples through.

use vike_model::test_support::corpus::Fnv;

// =================================================================================================
// The corpus: this crate's rows, the inputs they draw and their PINNED hashes. The hasher and the
// generators are `vike_model::test_support::corpus`'s, one copy for every probe, so a row moved
// between crates hashes identically by construction; the TEXT parser of the source half is
// `vike_model::libm_walk`'s (decision 0074).
// =================================================================================================

/// Samples per row for the cheap rows.
pub(super) const SAMPLES: usize = 4096;

/// Samples for the QUADRATURE row.
///
/// ⚠ Deliberately far smaller, and the reason is cost rather than taste: `Basket::expected_value`
/// evaluates the payoff `(kinks + 1) * (steps + 1)` times per call. This test is NOT `#[ignore]`d —
/// it runs on every PR and on the Windows dev box — so a 4096-sample row at the default
/// `EvGrid { steps: 4096 }` would be tens of millions of payoff evaluations for one hash. The grid
/// is shrunk too (see the row), which is safe here because what is being pinned is whether the
/// SAME arithmetic happens on both boxes, not whether the quadrature has converged.
pub(super) const EV_SAMPLES: usize = 384;

/// One row's verdict over its own corpus.
pub(super) struct Row {
    pub(super) label: &'static str,
    pub(super) hash: String,
    pub(super) n: usize,
    /// ⚠ NON-VACUITY GUARD — how many DISTINCT FINITE values the samples took. A row that collapses
    /// to one value hashes identically on every platform and reads as a PASS while pinning nothing.
    /// The analytics twin caught a real instance of exactly that (`overfit::pbo_cscv`). In THIS
    /// crate the shape to watch is different and just as easy to hit: every public function here
    /// returns `None` on an out-of-domain input, and a corpus that mostly missed the domain would
    /// hash a wall of canonical NaNs — perfectly stable on every platform.
    pub(super) distinct: usize,
}

pub(super) fn finish(label: &'static str, vals: &[f64]) -> Row {
    let mut h = Fnv::new();
    for &v in vals {
        h.push(v);
    }
    let mut bits: Vec<u64> = vals.iter().filter(|v| v.is_finite()).map(|v| v.to_bits()).collect();
    bits.sort_unstable();
    bits.dedup();
    Row { label, hash: h.hex(), n: vals.len(), distinct: bits.len() }
}

/// Smallest number of DISTINCT finite values a row may carry before its pin is treated as vacuous.
pub(super) const MIN_DISTINCT: usize = 64;

/// A `None` is hashed as a canonical NaN rather than skipped: WHETHER a function answers is part of
/// the behaviour being pinned, and skipping the misses would hide a platform that stopped solving.
pub(super) fn or_nan(v: Option<f64>) -> f64 {
    v.unwrap_or(f64::NAN)
}
