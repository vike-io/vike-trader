//! Finite-difference verification gate for the second-order greeks (`src/second_order.rs`).
//!
//! Every analytic second-order greek is checked against a CENTRAL finite difference of the
//! crate's own first-order greeks (`black_scholes_greeks` — delta/gamma/per-point-vega),
//! at ≤1e-6 relative, across four (S, K, t, σ, r) probe points (ATM zero-r, ITM/nonzero-r,
//! OTM short-dated, ITM long-dated) and — for the delta-based greeks — BOTH kinds:
//!
//! - vanna  vs dΔ/dσ AND (mixed-partial symmetry) vs dν_raw/dS
//! - vomma  vs dν_raw/dσ
//! - charm  vs dΔ/dt   (passage of time: bump t DOWN = the option aging)
//! - veta   vs dν_raw/dt
//! - color  vs dΓ/dt
//!
//! `ν_raw = vega_per_point · 100` (the crate's vega is per vol-point); charm/veta/color are
//! per calendar day, so the analytic value is ×365 before comparing with the annualized
//! difference quotient. Central differences with h = 1e-5 (σ, t) / 1e-4 (S) put the
//! truncation + cancellation error orders of magnitude below the 1e-6 gate.

use vike_options::{
    OptionKind, black_scholes_greeks, charm, color, second_order_greeks, vanna, veta, vomma,
};

const C: OptionKind = OptionKind::Call;
const P: OptionKind = OptionKind::Put;

/// (S, K, t, sigma, r) probe points — all away from any greek's zero crossing.
const POINTS: [(f64, f64, f64, f64, f64); 4] = [
    (100.0, 100.0, 1.0, 0.20, 0.0),  // ATM, zero rate
    (105.0, 98.0, 0.5, 0.35, 0.04),  // ITM call, nonzero rate (the oracle_parity probe)
    (80.0, 100.0, 0.25, 0.50, 0.01), // OTM call, short-dated, high vol
    (120.0, 100.0, 2.0, 0.15, 0.03), // ITM call, long-dated, low vol
];

const H_SIGMA: f64 = 1e-5;
const H_T: f64 = 1e-5;
const H_S: f64 = 1e-4;
const DAYS: f64 = 365.0;
const TOL: f64 = 1e-6;

fn delta(s: f64, k: f64, t: f64, sigma: f64, kind: OptionKind, r: f64) -> f64 {
    black_scholes_greeks(s, k, t, sigma, kind, r).unwrap().0
}

fn gamma(s: f64, k: f64, t: f64, sigma: f64, r: f64) -> f64 {
    black_scholes_greeks(s, k, t, sigma, C, r).unwrap().1
}

/// Raw (per 1.0 vol) vega — the crate returns per-point, so ×100.
fn vega_raw(s: f64, k: f64, t: f64, sigma: f64, r: f64) -> f64 {
    black_scholes_greeks(s, k, t, sigma, C, r).unwrap().3 * 100.0
}

fn assert_rel(what: &str, point: (f64, f64, f64, f64, f64), got: f64, fd: f64) {
    let rel = (got - fd).abs() / fd.abs().max(1e-9);
    assert!(
        rel <= TOL,
        "{what} at {point:?}: analytic {got:e} vs finite-difference {fd:e} (rel {rel:e})"
    );
}

#[test]
fn vanna_matches_d_delta_d_sigma_both_kinds() {
    for &(s, k, t, sigma, r) in &POINTS {
        let got = vanna(s, k, t, sigma, r).unwrap();
        for kind in [C, P] {
            let fd = (delta(s, k, t, sigma + H_SIGMA, kind, r)
                - delta(s, k, t, sigma - H_SIGMA, kind, r))
                / (2.0 * H_SIGMA);
            assert_rel("vanna(dΔ/dσ)", (s, k, t, sigma, r), got, fd);
        }
    }
}

#[test]
fn vanna_matches_d_vega_d_spot_mixed_partial() {
    for &(s, k, t, sigma, r) in &POINTS {
        let got = vanna(s, k, t, sigma, r).unwrap();
        let fd =
            (vega_raw(s + H_S, k, t, sigma, r) - vega_raw(s - H_S, k, t, sigma, r)) / (2.0 * H_S);
        assert_rel("vanna(dν/dS)", (s, k, t, sigma, r), got, fd);
    }
}

#[test]
fn vomma_matches_d_vega_d_sigma() {
    for &(s, k, t, sigma, r) in &POINTS {
        let got = vomma(s, k, t, sigma, r).unwrap();
        let fd = (vega_raw(s, k, t, sigma + H_SIGMA, r) - vega_raw(s, k, t, sigma - H_SIGMA, r))
            / (2.0 * H_SIGMA);
        assert_rel("vomma", (s, k, t, sigma, r), got, fd);
    }
}

#[test]
fn charm_matches_d_delta_dt_both_kinds() {
    for &(s, k, t, sigma, r) in &POINTS {
        for kind in [C, P] {
            let got = charm(s, k, t, sigma, kind, r).unwrap() * DAYS; // per-day → annual
            // Passage of time: one h of calendar time passing means t (time to expiry)
            // SHRINKS, so dΔ/dt = (Δ(t-h) - Δ(t+h)) / (2h).
            let fd = (delta(s, k, t - H_T, sigma, kind, r) - delta(s, k, t + H_T, sigma, kind, r))
                / (2.0 * H_T);
            assert_rel("charm", (s, k, t, sigma, r), got, fd);
        }
    }
}

#[test]
fn veta_matches_d_vega_dt() {
    for &(s, k, t, sigma, r) in &POINTS {
        let got = veta(s, k, t, sigma, r).unwrap() * DAYS;
        let fd =
            (vega_raw(s, k, t - H_T, sigma, r) - vega_raw(s, k, t + H_T, sigma, r)) / (2.0 * H_T);
        assert_rel("veta", (s, k, t, sigma, r), got, fd);
    }
}

#[test]
fn color_matches_d_gamma_dt() {
    for &(s, k, t, sigma, r) in &POINTS {
        let got = color(s, k, t, sigma, r).unwrap() * DAYS;
        let fd = (gamma(s, k, t - H_T, sigma, r) - gamma(s, k, t + H_T, sigma, r)) / (2.0 * H_T);
        assert_rel("color", (s, k, t, sigma, r), got, fd);
    }
}

#[test]
fn combined_helper_agrees_with_the_fd_gated_individuals() {
    // The FD gates above pin the individual functions; the combined helper must be
    // bit-identical to them (also asserted in-module — re-checked here through the public API).
    for &(s, k, t, sigma, r) in &POINTS {
        let g = second_order_greeks(s, k, t, sigma, C, r).unwrap();
        assert_eq!(g.vanna.to_bits(), vanna(s, k, t, sigma, r).unwrap().to_bits());
        assert_eq!(g.vomma.to_bits(), vomma(s, k, t, sigma, r).unwrap().to_bits());
        assert_eq!(g.charm.to_bits(), charm(s, k, t, sigma, C, r).unwrap().to_bits());
        assert_eq!(g.veta.to_bits(), veta(s, k, t, sigma, r).unwrap().to_bits());
        assert_eq!(g.color.to_bits(), color(s, k, t, sigma, r).unwrap().to_bits());
    }
}
