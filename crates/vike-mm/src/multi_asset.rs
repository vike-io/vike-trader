//! Multi-asset (correlation-aware) Guéant market-making pure core — the covariance-matrix
//! generalization of the single-asset A-S/GLFT inventory penalty in [`crate::avellaneda`].
//!
//! Ports the multi-asset optimal-market-making result of Bergault–Guéant (Guéant, "Optimal market
//! making", *Mathematical Finance* / Bergault & Guéant, "Size matters for OTC market makers",
//! arXiv 1606.01862 and arXiv 1810.04383): for a maker holding an inventory VECTOR `q` across N
//! correlated assets with a returns covariance matrix `Σ` (per-ms σ², symmetric PSD) and risk
//! aversion `γ`, the reservation-price skew for asset `i` is the i-th component of the
//! matrix-vector product
//!
//!   skew = γ · (Σ · q)          →     skewᵢ = γ · Σⱼ Σ[i][j]·q[j]
//!   rᵢ   = mᵢ − skewᵢ
//!
//! This is the exact generalization of the single-asset reservation term `r = s − q·γ·V`
//! ([`crate::avellaneda::as_reservation_price`]): the SCALAR variance penalty `q·γ·V` becomes the
//! VECTOR penalty `γ·(Σ·q)`. On a DIAGONAL `Σ` (uncorrelated assets) it collapses back to N
//! independent single-asset makers — `skewᵢ = γ·Σ[i][i]·q[i]` — so the value of the matrix form is
//! entirely in the OFF-DIAGONAL terms: a position in a CORRELATED asset `j` leans asset `i`'s
//! reservation too. Holding a long BTC-perp inventory therefore already skews the ETH-perp quote
//! down, hedging the correlated exposure a fleet of independent makers would each ignore — that
//! cross-asset lean is the whole reason to run one correlated maker over N separate ones.
//!
//! House rule: pure `pub(crate)` free fns, NAIVE folds (no `mul_add`), in-file tests. The dense
//! matrix math is HAND-ROLLED (no linear-algebra crate) — the matrices are tiny (N assets a maker
//! quotes, single digits), so a plain nested naive fold is both simplest and keeps the workspace
//! dependency/`cargo deny` surface flat. `Σ` symmetry/PSD is the CALLER's contract (a covariance
//! estimator's job), NOT enforced here.

/// The correlated-inventory reservation SKEW vector `γ · (Σ · q)` — element `i` is
/// `gamma · Σⱼ cov[i][j]·inventory[j]` (naive left-to-right fold, no `mul_add`). This is the
/// multi-asset generalization of the single-asset penalty `q_norm·γ·V`: the scalar per-asset
/// variance `V` becomes the covariance matrix `Σ`, so a position in asset `j` skews asset `i`
/// through the off-diagonal `cov[i][j]`.
///
/// Guard: a DIM MISMATCH — `cov` not square-of-side `inventory.len()`, i.e. any row whose length ≠
/// `inventory.len()`, or a row count ≠ `inventory.len()` — returns a ZERO vector of length
/// `inventory.len()` (the inert, no-skew answer), rather than panicking or silently reading a ragged
/// matrix. A well-formed but empty problem (`inventory` empty) returns an empty vector.
pub(crate) fn multi_asset_skew(inventory: &[f64], cov: &[Vec<f64>], gamma: f64) -> Vec<f64> {
    let n = inventory.len();
    // Dim guard: `cov` must be exactly n×n. A ragged/mismatched matrix ⇒ the inert zero vector.
    if cov.len() != n || cov.iter().any(|row| row.len() != n) {
        return vec![0.0; n];
    }
    let mut skew = Vec::with_capacity(n);
    for row in cov.iter() {
        // (Σ·q)ᵢ = Σⱼ cov[i][j]·inventory[j], naive fold, then scale by γ.
        let mut acc = 0.0;
        for (c_ij, q_j) in row.iter().zip(inventory.iter()) {
            acc += c_ij * q_j;
        }
        skew.push(gamma * acc);
    }
    skew
}

/// The per-asset reservation-price vector `mids[i] − skew[i]`, where `skew = γ·(Σ·q)` from
/// [`multi_asset_skew`] — the multi-asset twin of [`crate::avellaneda::as_reservation_price`]'s
/// `s − q_norm·γ·V`. The shared quote assembly would post each side around `mids[i] − skewᵢ`.
///
/// Guard: on a dim mismatch (`mids.len() != inventory.len()`, or a non-`n×n` `cov`) the skew term is
/// the zero vector, so each reservation is the raw mid unchanged. Naive fold; `mids.len() == 0` ⇒
/// empty vector.
pub(crate) fn multi_asset_reservation(
    mids: &[f64],
    inventory: &[f64],
    cov: &[Vec<f64>],
    gamma: f64,
) -> Vec<f64> {
    let skew = multi_asset_skew(inventory, cov, gamma);
    // If mids and the skew disagree in length (a mids/inventory mismatch), fall back to the raw mids
    // — the inert, no-skew reservation, mirroring the zero-vector guard in `multi_asset_skew`.
    if skew.len() != mids.len() {
        return mids.to_vec();
    }
    mids.iter().zip(skew.iter()).map(|(m, sk)| m - sk).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::avellaneda::as_reservation_price;

    const EPS: f64 = 1e-12;

    /// DIAGONAL covariance ⇒ the multi-asset skew reduces to N INDEPENDENT single-asset makers:
    /// `skewᵢ == γ·cov[i][i]·q[i]`, exactly the inventory term of `as_reservation_price` per asset.
    #[test]
    fn diagonal_cov_reduces_to_independent_single_asset() {
        let gamma = 0.1;
        let inv = [3.0, -2.0, 1.5];
        // purely diagonal Σ (uncorrelated per-ms variances)
        let cov = vec![vec![4.0e-6, 0.0, 0.0], vec![0.0, 9.0e-6, 0.0], vec![0.0, 0.0, 1.0e-6]];
        let skew = multi_asset_skew(&inv, &cov, gamma);
        for i in 0..3 {
            let expected = gamma * cov[i][i] * inv[i];
            assert!((skew[i] - expected).abs() < EPS, "asset {i}: γ·cov_ii·q_i");
            // and it matches the single-asset reservation's inventory term bit-for-parity: a mid of
            // `m` with V = cov_ii and q_norm = q gives r = m − skewᵢ.
            let m = 100.0 + i as f64;
            let r_single = as_reservation_price(m, inv[i], gamma, cov[i][i]);
            let r_multi = multi_asset_reservation(&[m], &[inv[i]], &[vec![cov[i][i]]], gamma)[0];
            assert!((r_single - r_multi).abs() < EPS, "asset {i}: multi == single on diagonal");
        }
    }

    /// A POSITIVE off-diagonal (assets A,B positively correlated) makes a LONG position in A also
    /// skew B's reservation DOWN — the cross-asset lean that is the whole point of the matrix form.
    #[test]
    fn positive_off_diagonal_leans_correlated_asset() {
        let gamma = 0.2;
        // long A only (q = [+5, 0]); B is flat.
        let inv = [5.0, 0.0];
        // positive correlation: off-diagonal cov[0][1] = cov[1][0] > 0.
        let cov = vec![vec![1.0e-5, 4.0e-6], vec![4.0e-6, 1.0e-5]];
        let skew = multi_asset_skew(&inv, &cov, gamma);
        // A's own skew is positive (long ⇒ reservation pulled DOWN vs its mid).
        assert!(skew[0] > 0.0, "long A skews A down");
        // B is FLAT, yet the positive correlation to the long A gives it a POSITIVE skew too — so
        // B's reservation `mid_B − skew_B` is pulled DOWN even with zero B inventory: the cross lean.
        assert!(skew[1] > 0.0, "long A leans B down through the positive off-diagonal");
        // exact value: skew_B = γ·(cov[1][0]·q_A + cov[1][1]·q_B) = γ·cov[1][0]·5.
        assert!((skew[1] - gamma * cov[1][0] * inv[0]).abs() < EPS, "B skew = γ·cov_BA·q_A");
        // reservation: mid_B pulled below its mid.
        let mids = [0.40, 0.60];
        let r = multi_asset_reservation(&mids, &inv, &cov, gamma);
        assert!(r[1] < mids[1], "B reservation < mid_B (leaned down)");
    }

    /// A NEGATIVE off-diagonal flips the lean: a long A leans a (flat) negatively-correlated B UP.
    #[test]
    fn negative_off_diagonal_flips_the_lean() {
        let gamma = 0.2;
        let inv = [5.0, 0.0];
        let cov = vec![vec![1.0e-5, -4.0e-6], vec![-4.0e-6, 1.0e-5]];
        let skew = multi_asset_skew(&inv, &cov, gamma);
        assert!(skew[1] < 0.0, "long A leans a negatively-correlated B UP (negative skew)");
        let mids = [0.40, 0.60];
        let r = multi_asset_reservation(&mids, &inv, &cov, gamma);
        assert!(r[1] > mids[1], "B reservation > mid_B (leaned up)");
    }

    /// Zero inventory ⇒ zero skew (every component), and the reservation is the raw mids unchanged.
    #[test]
    fn zero_inventory_gives_zero_skew() {
        let cov = vec![vec![1.0e-5, 4.0e-6], vec![4.0e-6, 1.0e-5]];
        let inv = [0.0, 0.0];
        let skew = multi_asset_skew(&inv, &cov, 0.3);
        assert!(skew.iter().all(|&s| s == 0.0), "flat ⇒ no skew");
        let mids = [0.5, 0.5];
        let r = multi_asset_reservation(&mids, &inv, &cov, 0.3);
        assert_eq!(r, mids.to_vec(), "flat ⇒ reservation is the raw mids");
    }

    /// Dim-mismatch guard: a non-square / ragged `cov`, or a `cov` whose side ≠ `inventory.len()`,
    /// yields a zero vector of length `inventory.len()` — and the reservation falls back to raw mids.
    #[test]
    fn dim_mismatch_returns_zeros() {
        let inv = [1.0, 2.0, 3.0];
        // wrong row count (2 rows for 3 assets)
        let bad_rows = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]];
        assert_eq!(multi_asset_skew(&inv, &bad_rows, 0.1), vec![0.0, 0.0, 0.0]);
        // ragged row (middle row too short)
        let ragged = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0], vec![0.0, 0.0, 1.0]];
        assert_eq!(multi_asset_skew(&inv, &ragged, 0.1), vec![0.0, 0.0, 0.0]);
        // reservation with a mids/inventory length disagreement falls back to raw mids
        let mids = [10.0, 20.0, 30.0];
        assert_eq!(
            multi_asset_reservation(&mids, &inv, &ragged, 0.1),
            mids.to_vec(),
            "mismatch ⇒ raw mids"
        );
        // empty problem ⇒ empty vectors
        assert!(multi_asset_skew(&[], &[], 0.1).is_empty(), "empty inventory ⇒ empty skew");
    }

    /// A hand-computed 2×2 correlated example — the full `γ·(Σ·q)` product pinned against arithmetic
    /// worked by hand (the caller owns Σ symmetry/PSD; here Σ is a valid symmetric PSD 2×2).
    #[test]
    fn hand_computed_2x2_correlated_example() {
        let gamma = 0.5;
        let q = [2.0, -3.0];
        // symmetric PSD Σ = [[2, 1], [1, 2]] (eigenvalues 1 and 3, both > 0)
        let cov = vec![vec![2.0, 1.0], vec![1.0, 2.0]];
        // (Σ·q)_0 = 2·2 + 1·(−3) = 1  ⇒ skew_0 = 0.5·1  = 0.5
        // (Σ·q)_1 = 1·2 + 2·(−3) = −4 ⇒ skew_1 = 0.5·−4 = −2.0
        let skew = multi_asset_skew(&q, &cov, gamma);
        assert!((skew[0] - 0.5).abs() < EPS, "skew_0 = 0.5");
        assert!((skew[1] - (-2.0)).abs() < EPS, "skew_1 = -2.0");
        // reservation: mids − skew
        let mids = [100.0, 50.0];
        let r = multi_asset_reservation(&mids, &q, &cov, gamma);
        assert!((r[0] - 99.5).abs() < EPS, "r_0 = 100 − 0.5");
        assert!((r[1] - 52.0).abs() < EPS, "r_1 = 50 − (−2.0)");
    }
}
