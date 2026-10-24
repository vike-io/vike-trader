//! GLFT (Guéant–Lehalle–Fernandez-Tapia) closed-form pricing: the skew coefficient `S`, its
//! `V_eff = S/γ` routing through the SHARED `as_quotes`, and the "reduces to A-S" identity.
use super::*;

const EPS: f64 = 1e-12;

// Representative `$`-scale-crypto GLFT inputs (per-ms σ̂², interior κ, unit base intensity).
const SIG2: f64 = 1e-6;
const GAMMA: f64 = 0.1;
const KAPPA: f64 = 50.0;
const A: f64 = 1.0;

/// `effective_variance` routes each model correctly: A-S passes the diffusion `v` through
/// untouched (byte-identical), Gueant returns exactly `S/γ`, and a degenerate `γ ≤ 0` under
/// Gueant falls back to the A-S `v` (defensive).
#[test]
fn effective_variance_routes_per_model() {
    let v_as = 4.2e-7;
    // A-S: identity, bit-for-bit.
    assert_eq!(
        effective_variance(SpreadModel::AvellanedaStoikov, v_as, SIG2, GAMMA, KAPPA, A).to_bits(),
        v_as.to_bits(),
        "A-S must pass the diffusion variance through unchanged"
    );
    // Gueant: exactly S/γ, regardless of the A-S v_as it discards.
    let s = gueant_skew_coeff(SIG2, GAMMA, KAPPA, A);
    assert_eq!(
        effective_variance(SpreadModel::Gueant, v_as, SIG2, GAMMA, KAPPA, A).to_bits(),
        (s / GAMMA).to_bits(),
        "Gueant must return S/γ, ignoring the A-S variance"
    );
    // Gueant with γ ≤ 0: defensive fallback to v_as.
    assert_eq!(
        effective_variance(SpreadModel::Gueant, v_as, SIG2, 0.0, KAPPA, A).to_bits(),
        v_as.to_bits(),
        "γ ≤ 0 under Gueant falls back to the A-S v"
    );
}

/// Non-positive `γ`, `κ`, `A`, or `σ̂²` ⇒ `S == 0` (the caller then floors the half-spread at the
/// intensity term, so the maker still quotes rather than dividing by zero).
#[test]
fn gueant_skew_coeff_guards_nonpositive_inputs() {
    assert_eq!(gueant_skew_coeff(SIG2, 0.0, KAPPA, A), 0.0);
    assert_eq!(gueant_skew_coeff(SIG2, GAMMA, 0.0, A), 0.0);
    assert_eq!(gueant_skew_coeff(SIG2, GAMMA, KAPPA, 0.0), 0.0);
    assert_eq!(gueant_skew_coeff(0.0, GAMMA, KAPPA, A), 0.0);
    assert!(gueant_skew_coeff(SIG2, GAMMA, KAPPA, A) > 0.0, "positive inputs ⇒ positive S");
}

/// `S = √( σ̂²·γ / (2κA) · (1+γ/κ)^(1+κ/γ) )` — pinned against the formula recomputed
/// independently (a regression against an accidental rearrangement of the closed form).
#[test]
fn gueant_skew_coeff_matches_closed_form() {
    let ratio = GAMMA / KAPPA;
    let expected =
        (SIG2 * GAMMA / (2.0 * KAPPA * A) * (1.0 + ratio).powf(1.0 + KAPPA / GAMMA)).sqrt();
    assert!((gueant_skew_coeff(SIG2, GAMMA, KAPPA, A) - expected).abs() < EPS);
}

/// THE AFFINE IDENTITY (the headline): feeding `as_quotes` the GLFT `V_eff = S/γ` on an UNBOUNDED
/// domain (no wall clamp, no standoff, no floor/cap) yields the GLFT depths
/// `bid_depth = c1 + (½+q)·S`, `ask_depth = c1 + (½−q)·S` — the SAME affine form A-S emits with
/// `γ·V`. This proves GLFT prices through the shared assembly, and that A-S IS the `V=S/γ` limit.
#[test]
fn glft_depths_are_the_affine_form_through_as_quotes() {
    let s = 100.0;
    let q = 0.7;
    let big = gueant_skew_coeff(SIG2, GAMMA, KAPPA, A);
    let v_eff = big / GAMMA;
    let c1 = as_intensity_halfspread(GAMMA, KAPPA);
    // Unbounded domain, no standoff/floor/cap, tick 0 (no snap) — isolate the pure affine math.
    let (bid, ask) = as_quotes(
        s,
        q,
        GAMMA,
        v_eff,
        KAPPA,
        0.0,
        f64::NEG_INFINITY,
        f64::INFINITY,
        0.0,
        0.0,
        0.0,
        0.0,
    )
    .expect("interior GLFT quote straddles the mid");
    let bid_depth = s - bid;
    let ask_depth = ask - s;
    assert!((bid_depth - (c1 + (0.5 + q) * big)).abs() < EPS, "bid depth = c1 + (½+q)·S");
    assert!((ask_depth - (c1 + (0.5 - q) * big)).abs() < EPS, "ask depth = c1 + (½−q)·S");
}

/// The GLFT skew is LINEAR in inventory and the base (q=0) half-spread is INDEPENDENT of it:
/// `bid_depth − ask_depth = 2q·S`, and at `q=0` the two depths are equal (`c1 + ½·S`).
#[test]
fn glft_skew_is_linear_and_base_spread_is_inventory_independent() {
    let s = 100.0;
    let big = gueant_skew_coeff(SIG2, GAMMA, KAPPA, A);
    let v_eff = big / GAMMA;
    let quote = |q: f64| {
        as_quotes(
            s,
            q,
            GAMMA,
            v_eff,
            KAPPA,
            0.0,
            f64::NEG_INFINITY,
            f64::INFINITY,
            0.0,
            0.0,
            0.0,
            0.0,
        )
        .unwrap()
    };
    // q = 0 ⇒ symmetric.
    let (b0, a0) = quote(0.0);
    assert!(((s - b0) - (a0 - s)).abs() < EPS, "at q=0 the depths are equal");
    // skew(q) = bid_depth − ask_depth = 2q·S.
    for q in [0.3, 0.7, 1.5] {
        let (b, a) = quote(q);
        let skew = (s - b) - (a - s);
        assert!((skew - 2.0 * q * big).abs() < EPS, "skew must be 2q·S (linear in q)");
    }
}

/// "Reduces to A-S": the GLFT-model effective variance fed to `as_quotes` is bit-identical to
/// running the A-S assembly directly with `v = S/γ`. So a GLFT mount and an A-S mount tuned to the
/// same `V` post the SAME quotes — the reduction is a code-level identity, not an approximation.
#[test]
fn glft_equals_avellaneda_stoikov_at_matching_variance() {
    let s = 100.0;
    let q = 0.4;
    let v_glft = effective_variance(SpreadModel::Gueant, 999.0, SIG2, GAMMA, KAPPA, A);
    let v_as = gueant_skew_coeff(SIG2, GAMMA, KAPPA, A) / GAMMA; // the A-S maker's matching V
    let args = |v: f64| {
        as_quotes(s, q, GAMMA, v, KAPPA, 0.01, f64::NEG_INFINITY, f64::INFINITY, 0.0, 0.0, 0.0, 1.0)
    };
    assert_eq!(args(v_glft), args(v_as), "GLFT ≡ A-S at V = S/γ (bit-identical quotes)");
}

/// `γ → 0` limit: the adverse-selection floor `c1 = (1/γ)ln(1+γ/κ)` (shared by BOTH models)
/// converges to `1/κ`, resolving the literature ambiguity (some sources print the `1/k` limit as
/// the constant itself). And `S ∝ √γ → 0`, so a risk-neutral GLFT maker quotes the floor.
#[test]
fn gamma_to_zero_floor_is_one_over_kappa_and_skew_vanishes() {
    let tiny = 1e-9;
    assert!((as_intensity_halfspread(tiny, KAPPA) - 1.0 / KAPPA).abs() < 1e-6, "c1 → 1/κ as γ → 0");
    assert!(
        gueant_skew_coeff(SIG2, tiny, KAPPA, A) < 1e-6,
        "S ∝ √γ → 0 as γ → 0 (risk-neutral ⇒ no inventory skew)"
    );
}
