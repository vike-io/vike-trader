//! The A-S fill-intensity ESTIMATORS — the `κ` fits and the base-intensity `A` profile, split out
//! of `avellaneda.rs` (audit F11); every item moved VERBATIM, arithmetic byte-identical.
//!
//! Four pure functions over a tape, no state and no venue types, each the maximiser of one
//! likelihood the A-S/GLFT closed forms consume:
//!
//! - [`fit_kappa`] — the size-weighted exponential MLE over the PUBLIC trade tape's running sums
//!   (`κ̂ = Σw / Σ(w·δ)`), with the `n_min` sample floor and the `[κ_min, κ_max]` clamp.
//! - [`fit_kappa_censored`] — the CENSORED exponential-hazard MLE over the maker's OWN order
//!   outcomes (fills AND right-censored pulls), solved by deterministic bisection.
//! - [`shrink_kappa`] — the own→public linear blend that keeps a thin own-fill tape honest.
//! - [`fit_base_intensity`] — the closed-form `Â = D / S0(κ)` the GLFT skew coefficient prices
//!   with.
//!
//! Naïve folds, no `mul_add`, `<= 0.0` off-guards — the expression shapes are bit-anchored by the
//! tests below and must not be "simplified".

/// Size-weighted exponential MLE for the fill-intensity decay `κ` (spec §4): `κ̂ = Σw / Σ(w·δ)`, with
/// the `n_min` sample floor and the degenerate-window guard (`Σ(w·δ) <= 0`) both falling back to
/// `κ_default`, and a trusted fit clamped to `[κ_min, κ_max]`. Pure over the two running sums + the
/// sample count.
pub(crate) fn fit_kappa(
    sum_w: f64,
    sum_w_delta: f64,
    n: usize,
    n_min: usize,
    kappa_default: f64,
    kappa_min: f64,
    kappa_max: f64,
) -> f64 {
    if n < n_min || sum_w_delta <= 0.0 {
        return kappa_default;
    }
    (sum_w / sum_w_delta).clamp(kappa_min, kappa_max)
}

/// CENSORED exponential-hazard MLE for the fill-intensity decay `κ` from the maker's OWN order
/// outcomes ([`KappaMode::OwnFillFit`](vike_model::KappaMode)). Models each order's fill
/// intensity as `λ(δ) = A·exp(−κ·δ)` (δ = distance-to-mid at placement) and treats the order as an
/// exponential survival with that distance-dependent rate: a FILLED order contributes its fill, a
/// PULLED/unfilled order is a RIGHT-CENSORED exposure. Profiling the base rate `A` out of the
/// log-likelihood leaves `κ` as the root of `S1(κ)/S0(κ) = mean-fill-δ`, where
/// `S0(κ) = Σ tᵢ·exp(−κ·δᵢ)` and `S1(κ) = Σ δᵢ·tᵢ·exp(−κ·δᵢ)` sum over ALL orders (fills AND
/// censored — the censored exposures carry the survival information) and the RHS is the mean δ over
/// the FILLED orders only. `S1/S0` is a `t·exp(−κ·δ)`-weighted mean of δ, monotone DECREASING in
/// `κ`, so the root is found by BISECTION on `[κ_min, κ_max]` (deterministic, no RNG); a target
/// outside the bracket clamps to the near bound. Returns `None` when there are NO fills (the
/// likelihood carries nothing to locate `κ`), so the caller can fall back to (or shrink toward) the
/// public-print κ. Pure over the `(δ, exposure_ms, filled)` tape; naïve folds, no `mul_add`.
pub(crate) fn fit_kappa_censored(
    obs: &[(f64, f64, bool)],
    kappa_min: f64,
    kappa_max: f64,
) -> Option<f64> {
    // fill count D and Σδ over fills → the RHS target, the mean distance of the FILLED orders.
    let mut n_fill = 0usize;
    let mut sum_fill_delta = 0.0_f64;
    for &(delta, _t, filled) in obs {
        if filled {
            n_fill += 1;
            sum_fill_delta += delta;
        }
    }
    if n_fill == 0 {
        return None;
    }
    let mbar = sum_fill_delta / n_fill as f64;
    // g(κ) = S1(κ)/S0(κ) − mbar — the score-equation residual, monotone DECREASING in κ.
    let g = |k: f64| -> f64 {
        let mut s0 = 0.0_f64;
        let mut s1 = 0.0_f64;
        for &(delta, t, _filled) in obs {
            if t <= 0.0 {
                continue; // a zero/negative exposure carries no survival weight
            }
            let w = t * libm::exp(-k * delta);
            s0 += w;
            s1 += delta * w;
        }
        if s0 <= 0.0 {
            // every weight underflowed (κ too high for this data) — push the bracket down.
            return -mbar - 1.0;
        }
        s1 / s0 - mbar
    };
    // monotone-decreasing bisection with boundary clamps: g(lo) ≥ 0 ≥ g(hi) brackets the root.
    let (lo0, hi0) =
        if kappa_min <= kappa_max { (kappa_min, kappa_max) } else { (kappa_max, kappa_min) };
    if g(lo0) <= 0.0 {
        return Some(lo0); // even the smallest κ over-decays → clamp low
    }
    if g(hi0) >= 0.0 {
        return Some(hi0); // even the largest κ under-decays → clamp high
    }
    let mut lo = lo0;
    let mut hi = hi0;
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        if g(mid) > 0.0 {
            lo = mid; // still left of the root (κ too small)
        } else {
            hi = mid;
        }
    }
    Some(0.5 * (lo + hi))
}

/// Shrink an own-fill `κ̂` toward the public-print `κ` below the `n_min` own-fill floor — a LINEAR
/// blend `w·own + (1−w)·public` with `w = min(n_fill / n_min, 1)`. At `n_fill = 0` it is all public,
/// at `n_fill ≥ n_min` all own, ramping in between — so a maker prices with the robust public
/// estimate until it has accumulated enough of its OWN fills to trust their intensity. `n_min == 0`
/// (no floor) ⇒ the own fit is used as-is. Pure; naïve folds.
pub(crate) fn shrink_kappa(own_kappa: f64, public_kappa: f64, n_fill: usize, n_min: usize) -> f64 {
    if n_min == 0 {
        return own_kappa;
    }
    let w = (n_fill as f64 / n_min as f64).min(1.0);
    w * own_kappa + (1.0 - w) * public_kappa
}

/// Profile the BASE fill intensity `A` (the `λ(δ)=A·e^(−κδ)` rate at the touch) from the SAME
/// censored own-order tape [`fit_kappa_censored`] fits `κ` on, given a known/fitted `kappa`. Once `κ`
/// is fixed, the exponential-hazard log-likelihood
/// `Σ_i [dᵢ·(ln A − κδᵢ) − A·tᵢ·e^(−κδᵢ)]` has a CLOSED-FORM maximizer in `A`:
/// `∂/∂A = D/A − S0(κ) = 0 ⇒ Â = D / S0(κ)`, where `D` = number of FILLED orders and
/// `S0(κ) = Σ_i tᵢ·e^(−κδᵢ)` sums exposure over ALL orders (fills AND censored — the survival term is
/// what carries the base rate). This is the same `S0` the `κ` fit profiles `A` out of, run in reverse
/// to recover it. Units: `tᵢ` in ms ⇒ `Â` in orders/MILLISECOND, matching the per-ms `σ̂²` the GLFT `S`
/// consumes (the units the fixed [`AsParams::base_intensity_a`] placeholder must otherwise be
/// hand-set in). `None` with no fills (`D = 0` ⇒ the likelihood can't locate `A`) or a degenerate
/// `S0 ≤ 0`; the caller then keeps the fixed `base_intensity_a`. Naïve folds, no `mul_add`.
///
/// [`AsParams::base_intensity_a`]: vike_model::AsParams::base_intensity_a
pub(crate) fn fit_base_intensity(obs: &[(f64, f64, bool)], kappa: f64) -> Option<f64> {
    let mut d: usize = 0;
    let mut s0 = 0.0;
    for &(delta, t, filled) in obs {
        if filled {
            d += 1;
        }
        s0 += t * libm::exp(-kappa * delta);
    }
    if d == 0 || s0 <= 0.0 {
        return None;
    }
    Some(d as f64 / s0)
}

/// The base-intensity profile `Â = D / S0(κ)` in isolation — pinned against the closed form
/// recomputed independently over a tiny `(δ, exposure, filled)` tape.
#[cfg(test)]
mod base_intensity_tests {
    use super::*;

    const EPS: f64 = 1e-12;

    /// The online base-intensity estimator `Â = D / Σ tᵢ·e^(−κδᵢ)` — pinned against the closed form
    /// recomputed independently over a tiny (δ, exposure, filled) tape (D = 2 fills of 3 orders).
    #[test]
    fn fit_base_intensity_matches_closed_form() {
        let kappa = 50.0_f64;
        let obs: [(f64, f64, bool); 3] =
            [(0.01, 100.0, true), (0.02, 200.0, false), (0.005, 150.0, true)];
        let s0: f64 = obs.iter().map(|&(d, t, _)| t * (-kappa * d).exp()).sum();
        assert!((fit_base_intensity(&obs, kappa).unwrap() - 2.0 / s0).abs() < EPS);
    }

    /// `None` when the likelihood can't locate `A`: no fills (`D = 0`) or an empty/degenerate tape
    /// (`S0 ≤ 0`). The caller then keeps the fixed `base_intensity_a`.
    #[test]
    fn fit_base_intensity_none_without_fills_or_data() {
        assert_eq!(fit_base_intensity(&[], 50.0), None, "empty tape ⇒ S0 = 0 ⇒ None");
        let censored = [(0.01, 100.0, false), (0.02, 50.0, false)];
        assert_eq!(fit_base_intensity(&censored, 50.0), None, "no fills ⇒ D = 0 ⇒ None");
    }

    /// Recovery: at the touch (δ = 0 ⇒ e^(−κ·0) = 1) `S0 = Σ tᵢ`, so `Â = D / Σ tᵢ`. Five fills over
    /// 1000 ms of exposure recover exactly 0.005 orders/ms — the per-ms units the GLFT `S` expects.
    #[test]
    fn fit_base_intensity_recovers_known_rate_at_the_touch() {
        let obs: Vec<(f64, f64, bool)> = (0..5).map(|_| (0.0, 200.0, true)).collect();
        assert!((fit_base_intensity(&obs, 50.0).unwrap() - 0.005).abs() < EPS);
    }
}

#[cfg(test)]
mod own_fill_kappa_tests {
    // `super::*` pulls in the four estimator fns defined here; `AsState` (the estimator state
    // that drives them) and the two vike-model knobs it is constructed from come from their own
    // modules, since this module needs no imports outside its tests.
    use super::*;
    use crate::avellaneda::AsState;
    use vike_model::{AsParams, KappaMode};

    /// The OwnFillFit `AsParams` these tests price with: an interior κ the synthetic set can recover,
    /// with a small own-fill floor so a modest dataset clears it.
    fn as_params(kappa_mode: KappaMode, n_min: usize) -> AsParams {
        AsParams { kappa_mode, n_min, ..AsParams::default() }
    }

    /// Build the EXACT-recovery dataset: `J` levels at δⱼ = (j+1)·Δ, each with the SAME total order
    /// count `M` and unit exposure, and fill counts fⱼ ∝ 0.5ʲ. Because every level carries the same
    /// `M` orders, the exposure-weighted score `S1/S0` becomes the pure `exp(−κδ)`-weighted mean of
    /// δ (M cancels), whose root sits at `x = e^{−κΔ} = 0.5` ⇒ `κ̂ = ln 2 / Δ` — independent of `M`.
    /// The censored orders that equalise each level to `M` are ESSENTIAL: drop them and the weighting
    /// (hence the recovered κ) changes. Fills sit at the SMALL δ (8 nearest), censored dominate the
    /// LARGE δ (1 fill farthest).
    fn recoverable_obs(delta_step: f64) -> Vec<(f64, f64, bool)> {
        let deltas = [delta_step, 2.0 * delta_step, 3.0 * delta_step, 4.0 * delta_step];
        let fills = [8usize, 4, 2, 1]; // ∝ 0.5^j
        let m = 8usize; // equal total orders per level (fills + censored)
        let mut obs = Vec::new();
        for (j, &delta) in deltas.iter().enumerate() {
            for i in 0..m {
                obs.push((delta, 1.0_f64, i < fills[j]));
            }
        }
        obs
    }

    // The censored MLE recovers a KNOWN κ (= ln 2 / Δ) essentially exactly from the synthetic set,
    // and the returned κ̂ genuinely solves the score equation `S1/S0 = mean-fill-δ` (the MLE's
    // first-order condition), to bisection precision.
    #[test]
    fn censored_mle_recovers_known_kappa() {
        let delta_step = 0.02_f64;
        let obs = recoverable_obs(delta_step);
        let kappa = fit_kappa_censored(&obs, 1.0, 1000.0).expect("has fills");
        let kappa_true = std::f64::consts::LN_2 / delta_step; // ≈ 34.657
        assert!((kappa - kappa_true).abs() < 1e-3, "recovered κ {kappa} ≉ known κ {kappa_true}");

        // κ̂ solves S1(κ̂)/S0(κ̂) = mean distance over the FILLED orders.
        let (mut s0, mut s1, mut n_fill, mut sum_fd) = (0.0_f64, 0.0_f64, 0usize, 0.0_f64);
        for &(d, t, f) in &obs {
            let w = t * (-kappa * d).exp();
            s0 += w;
            s1 += d * w;
            if f {
                n_fill += 1;
                sum_fd += d;
            }
        }
        let mbar = sum_fd / n_fill as f64;
        assert!((s1 / s0 - mbar).abs() < 1e-6, "score not solved: {} vs {mbar}", s1 / s0);
    }

    // No fills ⇒ the likelihood cannot locate κ ⇒ `None` (the caller then leans on the public κ).
    #[test]
    fn censored_mle_needs_a_fill() {
        let all_censored = [(0.02_f64, 1.0_f64, false), (0.05, 1.0, false)];
        assert_eq!(fit_kappa_censored(&all_censored, 1.0, 1000.0), None, "all-censored");
        assert_eq!(fit_kappa_censored(&[], 1.0, 1000.0), None, "empty ⇒ unfittable");
    }

    // Fills concentrated at the NEAR distance (vs spread out to the far distance) imply a FASTER
    // intensity decay ⇒ a LARGER κ — a monotonic sanity direction on recovery.
    #[test]
    fn tighter_fill_profile_gives_larger_kappa() {
        let build = |near_fills: usize, far_fills: usize| {
            let mut obs = Vec::new();
            for i in 0..10usize {
                obs.push((0.02_f64, 1.0_f64, i < near_fills));
            }
            for i in 0..10usize {
                obs.push((0.08_f64, 1.0_f64, i < far_fills));
            }
            fit_kappa_censored(&obs, 1.0, 1000.0).expect("has fills")
        };
        let concentrated = build(8, 1); // most fills near ⇒ fast decay
        let spread = build(5, 4); // fills reach the far level ⇒ slower decay
        assert!(concentrated > spread, "near-concentrated κ {concentrated} ≤ spread κ {spread}");
    }

    // shrink_kappa: a linear own→public blend weighted by n_fill/n_min, all-public at 0 fills,
    // full-own at (and beyond) n_min; n_min == 0 disables the floor.
    #[test]
    fn shrink_blends_toward_public_below_n_min() {
        assert_eq!(shrink_kappa(30.0, 50.0, 0, 20).to_bits(), 50.0_f64.to_bits(), "0 ⇒ public");
        assert!((shrink_kappa(30.0, 50.0, 10, 20) - 40.0).abs() < 1e-12, "half weight ⇒ midpoint");
        assert_eq!(shrink_kappa(30.0, 50.0, 20, 20).to_bits(), 30.0_f64.to_bits(), "n_min ⇒ own");
        assert_eq!(shrink_kappa(30.0, 50.0, 99, 20).to_bits(), 30.0_f64.to_bits(), "beyond ⇒ own");
        assert_eq!(shrink_kappa(30.0, 50.0, 5, 0).to_bits(), 30.0_f64.to_bits(), "no floor ⇒ own");
    }

    // effective_kappa under OwnFillFit: cold it is the public-print κ (which itself falls back to
    // kappa_default with no public trades); warm (≥ n_min own fills) it prices the censored own-fit.
    // Fixed NEVER reads the own tape even when populated — a maker not selecting OwnFillFit is
    // byte-identical.
    #[test]
    fn effective_kappa_owns_fit_and_off_modes_ignore_the_tape() {
        // OwnFillFit, own-fill floor 10 so the 15-fill synthetic set fully engages.
        let mut own = AsState::new(as_params(KappaMode::OwnFillFit, 10));
        assert_eq!(
            own.effective_kappa().to_bits(),
            own.params.kappa_default.to_bits(),
            "cold OwnFillFit ⇒ kappa_default via the empty public tape"
        );
        for &(delta, _t, filled) in &recoverable_obs(0.02) {
            own.record_own_outcome(delta, 1.0, filled, 1_000);
        }
        let kappa_true = std::f64::consts::LN_2 / 0.02;
        assert!(
            (own.effective_kappa() - kappa_true).abs() < 1e-3,
            "warm OwnFillFit ⇒ censored own-fit ≈ {kappa_true}, got {}",
            own.effective_kappa()
        );

        // Fixed ignores the own tape entirely: populate it, and effective_kappa stays kappa_default.
        let mut fixed = AsState::new(as_params(KappaMode::Fixed, 10));
        fixed.record_own_outcome(0.02, 1.0, true, 1_000);
        fixed.record_own_outcome(0.06, 1.0, false, 1_000);
        assert_eq!(
            fixed.effective_kappa().to_bits(),
            fixed.params.kappa_default.to_bits(),
            "Fixed ignores the own tape ⇒ kappa_default, unchanged"
        );
    }

    // Window eviction keeps own_n_fill in step: an outcome older than trade_window_ms falls out of
    // the tape and decrements the fill count, so a stale fill stops informing the fit.
    #[test]
    fn own_tape_evicts_outside_the_window() {
        let mut st = AsState::new(as_params(KappaMode::OwnFillFit, 1));
        // window is the default 60_000 ms; the first fill lands far in the past.
        st.record_own_outcome(0.02, 1.0, true, 0);
        assert_eq!((st.own_obs.len(), st.own_n_fill), (1, 1), "first fill tracked");
        // a much later outcome slides the window past the first, evicting it.
        st.record_own_outcome(0.04, 1.0, false, 1_000_000);
        assert_eq!((st.own_obs.len(), st.own_n_fill), (1, 0), "stale fill evicted");
    }

    /// `effective_a`: the base intensity `A` the GLFT `S` prices with. With no own fills it is the
    /// fixed `base_intensity_a` (byte-identical fallback); once the OwnFillFit tape carries fills it
    /// switches to the online `Â = D/S0(κ)`, matching `fit_base_intensity` over the same tape at
    /// `effective_kappa`.
    #[test]
    fn effective_a_uses_online_fit_once_fills_arrive_else_the_fixed_default() {
        let p = AsParams { base_intensity_a: 7.0, ..as_params(KappaMode::OwnFillFit, 1) };
        let mut st = AsState::new(p);
        assert_eq!(st.effective_a(), 7.0, "no fills ⇒ the fixed base_intensity_a");

        st.record_own_outcome(0.01, 100.0, true, 0);
        st.record_own_outcome(0.02, 100.0, false, 0);
        st.record_own_outcome(0.005, 100.0, true, 0);
        let a = st.effective_a();
        assert!(
            a > 0.0 && (a - 7.0).abs() > 1e-9,
            "fills ⇒ the online Â replaces the placeholder (got {a})"
        );
        let obs: Vec<(f64, f64, bool)> = st.own_obs.iter().map(|&(d, t, f, _)| (d, t, f)).collect();
        assert_eq!(
            a,
            fit_base_intensity(&obs, st.effective_kappa()).unwrap(),
            "Â = D/S0 over the tape"
        );
    }
}
