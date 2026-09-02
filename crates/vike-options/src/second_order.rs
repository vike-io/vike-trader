//! Second-order Black–Scholes greeks — vanna, vomma, charm, veta, color — built on the SAME
//! `d1`/`d2`/pdf internals ([`crate::greeks::d1_d2`], [`crate::greeks::norm_pdf`]) and the
//! same conventions as [`crate::greeks`].
//!
//! NOT an oracle port: the Python twin (`data/options/greeks.py`) stops at first-order
//! greeks, so this module is an independent ADDITIVE extension from the standard published
//! formulas (Haug, *The Complete Guide to Option Pricing Formulas*; the Wikipedia Greeks
//! table), verified by finite differences against the crate's own first-order greeks in
//! `tests/second_order_fd.rs` (≤1e-6 relative, every greek, calls and puts). No parity
//! fixture governs it and none was touched.
//!
//! Conventions (matching `greeks.rs` exactly):
//! - European exercise, continuous compounding, **no dividends** (`q = 0`); `r` is a plain
//!   parameter (env reads stay in the consuming binary — pass `0.0` for the crate default).
//! - Year basis is **365 calendar days** (the per-day theta denominator in `greeks.rs`):
//!   [`charm`], [`veta`], [`color`] are reported **per calendar day** (annual value / 365).
//! - Sign convention for the time derivatives: charm/veta/color are **passage-of-time**
//!   derivatives `dX/dt` (time to expiry SHRINKING) — negative when the quantity decays as
//!   the clock runs, e.g. veta < 0 for an ATM option at `r = 0` (vega bleeds off). Beware:
//!   published tables mix `∂/∂τ` (time-to-expiry) and `∂/∂t` conventions per row; the
//!   finite-difference gate pins ours.
//! - [`vanna`]/[`vomma`] are RAW annualized sensitivities **per 1.0 change in sigma** (not
//!   per vol-point); divide by 100 to put them on the per-point scale of
//!   `black_scholes_greeks`' vega. Likewise [`veta`] moves the RAW vega (`vega_per_point *
//!   100`) per day.
//! - Invalid inputs (`s/k/t/sigma <= 0`) return `None`, same guard as the first-order twin.
//!
//! With `q = 0` the only kind-dependent terms in the published charm formulas
//! (`±q e^{-qτ} Φ(±d1)`) vanish, so every greek here — charm included — is identical for
//! calls and puts (put delta = call delta − 1, so their time drift matches). [`charm`] and
//! [`second_order_greeks`] still take an [`OptionKind`] to mirror `black_scholes_greeks`'
//! signature and keep call sites explicit; the equality is asserted in tests.

use crate::greeks::{d1_d2, norm_pdf};
use crate::model::OptionKind;

const DAYS_PER_YEAR: f64 = 365.0;

/// The five second-order greeks at one (S, K, t, sigma, r) point — see the module doc for
/// units and sign conventions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SecondOrderGreeks {
    /// dDelta/dSigma (== dVega/dS), per 1.0 vol.
    pub vanna: f64,
    /// dVega/dSigma (raw vega, per 1.0 vol).
    pub vomma: f64,
    /// dDelta/dt, per calendar day (call == put at q = 0).
    pub charm: f64,
    /// dVega/dt (raw vega), per calendar day.
    pub veta: f64,
    /// dGamma/dt, per calendar day.
    pub color: f64,
}

/// Shared per-point context: `(d1, d2, sqrt_t, pdf(d1))`, or `None` on the invalid-input
/// guard — the one place the guard + intermediates live for this module.
#[allow(clippy::many_single_char_names)] // s/k/t/σ/r are the domain vocabulary
fn ctx(s: f64, k: f64, t: f64, sigma: f64, r: f64) -> Option<(f64, f64, f64, f64)> {
    if s <= 0.0 || k <= 0.0 || t <= 0.0 || sigma <= 0.0 {
        return None;
    }
    let (d1, d2, sqrt_t) = d1_d2(s, k, t, sigma, r);
    Some((d1, d2, sqrt_t, norm_pdf(d1)))
}

/// Vanna `= ∂²V/∂S∂σ = ∂Δ/∂σ = ∂ν/∂S = -φ(d1)·d2/σ` (per 1.0 vol), or `None` on invalid
/// inputs. Kind-independent.
#[allow(clippy::many_single_char_names)]
pub fn vanna(s: f64, k: f64, t: f64, sigma: f64, r: f64) -> Option<f64> {
    let (_d1, d2, _sqrt_t, pdf_d1) = ctx(s, k, t, sigma, r)?;
    Some(-pdf_d1 * d2 / sigma)
}

/// Vomma (volga) `= ∂²V/∂σ² = ∂ν/∂σ = ν·d1·d2/σ` with raw vega `ν = S·φ(d1)·√t` (per 1.0
/// vol), or `None` on invalid inputs. Kind-independent.
#[allow(clippy::many_single_char_names)]
pub fn vomma(s: f64, k: f64, t: f64, sigma: f64, r: f64) -> Option<f64> {
    let (d1, d2, sqrt_t, pdf_d1) = ctx(s, k, t, sigma, r)?;
    Some(s * pdf_d1 * sqrt_t * d1 * d2 / sigma)
}

/// Charm (delta decay) `= dΔ/dt` **per calendar day**, or `None` on invalid inputs.
///
/// Standard no-dividend formula (annualized, passage of time):
/// `charm = -φ(d1)·(2rt - d2·σ·√t) / (2t·σ·√t)`; the `±q e^{-qτ} Φ(±d1)` dividend terms of
/// the full call/put formulas vanish at `q = 0`, so both kinds coincide (tested).
#[allow(clippy::many_single_char_names)]
pub fn charm(s: f64, k: f64, t: f64, sigma: f64, kind: OptionKind, r: f64) -> Option<f64> {
    let (_d1, d2, sqrt_t, pdf_d1) = ctx(s, k, t, sigma, r)?;
    let drift = -pdf_d1 * (2.0 * r * t - d2 * sigma * sqrt_t) / (2.0 * t * sigma * sqrt_t);
    let annual = match kind {
        OptionKind::Call => drift, // full formula: q·e^{-qt}·Φ(d1) + drift, q = 0
        OptionKind::Put => drift,  // full formula: -q·e^{-qt}·Φ(-d1) + drift, q = 0
    };
    Some(annual / DAYS_PER_YEAR)
}

/// Veta (DvegaDtime) `= dν/dt` for the RAW vega `ν = S·φ(d1)·√t`, **per calendar day**, or
/// `None` on invalid inputs. Kind-independent.
///
/// Annualized passage-of-time form (q = 0):
/// `veta = S·φ(d1)·√t·[ r·d1/(σ·√t) - (1 + d1·d2)/(2t) ]` — the negative of the
/// time-to-expiry derivative `∂ν/∂τ`, so an ATM option at `r = 0` reads negative (vega
/// bleeds as the clock runs).
#[allow(clippy::many_single_char_names)]
pub fn veta(s: f64, k: f64, t: f64, sigma: f64, r: f64) -> Option<f64> {
    let (d1, d2, sqrt_t, pdf_d1) = ctx(s, k, t, sigma, r)?;
    let annual = s * pdf_d1 * sqrt_t * (r * d1 / (sigma * sqrt_t) - (1.0 + d1 * d2) / (2.0 * t));
    Some(annual / DAYS_PER_YEAR)
}

/// Color (gamma decay) `= dΓ/dt` **per calendar day**, or `None` on invalid inputs.
/// Kind-independent.
///
/// Annualized passage-of-time form (q = 0):
/// `color = φ(d1)/(2S·t·σ·√t) · [ 1 + d1·(2rt - d2·σ·√t)/(σ·√t) ]` — positive for an ATM
/// option (gamma grows as expiry approaches).
#[allow(clippy::many_single_char_names)]
pub fn color(s: f64, k: f64, t: f64, sigma: f64, r: f64) -> Option<f64> {
    let (d1, d2, sqrt_t, pdf_d1) = ctx(s, k, t, sigma, r)?;
    let annual = pdf_d1 / (2.0 * s * t * sigma * sqrt_t)
        * (1.0 + d1 * (2.0 * r * t - d2 * sigma * sqrt_t) / (sigma * sqrt_t));
    Some(annual / DAYS_PER_YEAR)
}

/// All five second-order greeks at once (one `d1`/`d2`/pdf computation), or `None` on invalid
/// inputs — bit-identical to calling the five individual functions (tested).
#[allow(clippy::many_single_char_names)]
pub fn second_order_greeks(
    s: f64,
    k: f64,
    t: f64,
    sigma: f64,
    kind: OptionKind,
    r: f64,
) -> Option<SecondOrderGreeks> {
    let (d1, d2, sqrt_t, pdf_d1) = ctx(s, k, t, sigma, r)?;
    let vanna = -pdf_d1 * d2 / sigma;
    let vomma = s * pdf_d1 * sqrt_t * d1 * d2 / sigma;
    let charm_annual = -pdf_d1 * (2.0 * r * t - d2 * sigma * sqrt_t) / (2.0 * t * sigma * sqrt_t);
    let charm = match kind {
        OptionKind::Call | OptionKind::Put => charm_annual / DAYS_PER_YEAR, // q = 0: kinds coincide
    };
    let veta_annual =
        s * pdf_d1 * sqrt_t * (r * d1 / (sigma * sqrt_t) - (1.0 + d1 * d2) / (2.0 * t));
    let color_annual = pdf_d1 / (2.0 * s * t * sigma * sqrt_t)
        * (1.0 + d1 * (2.0 * r * t - d2 * sigma * sqrt_t) / (sigma * sqrt_t));
    Some(SecondOrderGreeks {
        vanna,
        vomma,
        charm,
        veta: veta_annual / DAYS_PER_YEAR,
        color: color_annual / DAYS_PER_YEAR,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: OptionKind = OptionKind::Call;
    const P: OptionKind = OptionKind::Put;

    #[test]
    fn invalid_inputs_are_none() {
        for (s, k, t, sigma) in [
            (0.0, 100.0, 1.0, 0.2),
            (100.0, 0.0, 1.0, 0.2),
            (100.0, 100.0, 0.0, 0.2),
            (100.0, 100.0, 1.0, 0.0),
            (-1.0, 100.0, 1.0, 0.2),
        ] {
            assert!(vanna(s, k, t, sigma, 0.0).is_none(), "vanna({s},{k},{t},{sigma})");
            assert!(vomma(s, k, t, sigma, 0.0).is_none(), "vomma({s},{k},{t},{sigma})");
            assert!(charm(s, k, t, sigma, C, 0.0).is_none(), "charm({s},{k},{t},{sigma})");
            assert!(veta(s, k, t, sigma, 0.0).is_none(), "veta({s},{k},{t},{sigma})");
            assert!(color(s, k, t, sigma, 0.0).is_none(), "color({s},{k},{t},{sigma})");
            assert!(second_order_greeks(s, k, t, sigma, C, 0.0).is_none());
        }
    }

    #[test]
    fn charm_call_put_coincide_at_q0() {
        let c = charm(105.0, 98.0, 0.5, 0.35, C, 0.04).unwrap();
        let p = charm(105.0, 98.0, 0.5, 0.35, P, 0.04).unwrap();
        assert_eq!(c.to_bits(), p.to_bits());
    }

    #[test]
    fn combined_matches_individuals_bit_for_bit() {
        for &(s, k, t, sigma, r) in &[
            (100.0, 100.0, 1.0, 0.20, 0.0),
            (105.0, 98.0, 0.5, 0.35, 0.04),
            (80.0, 100.0, 0.25, 0.50, 0.01),
        ] {
            for kind in [C, P] {
                let g = second_order_greeks(s, k, t, sigma, kind, r).unwrap();
                assert_eq!(g.vanna.to_bits(), vanna(s, k, t, sigma, r).unwrap().to_bits());
                assert_eq!(g.vomma.to_bits(), vomma(s, k, t, sigma, r).unwrap().to_bits());
                assert_eq!(g.charm.to_bits(), charm(s, k, t, sigma, kind, r).unwrap().to_bits());
                assert_eq!(g.veta.to_bits(), veta(s, k, t, sigma, r).unwrap().to_bits());
                assert_eq!(g.color.to_bits(), color(s, k, t, sigma, r).unwrap().to_bits());
            }
        }
    }

    #[test]
    fn atm_signs_match_intuition() {
        // ATM, r = 0, 1y: vega bleeds (veta < 0), gamma sharpens (color > 0), vomma < 0
        // (d1·d2 = -σ²t/4 < 0 at ATM), vanna > 0 (d2 < 0).
        let g = second_order_greeks(100.0, 100.0, 1.0, 0.20, C, 0.0).unwrap();
        assert!(g.veta < 0.0, "veta {}", g.veta);
        assert!(g.color > 0.0, "color {}", g.color);
        assert!(g.vomma < 0.0, "vomma {}", g.vomma);
        assert!(g.vanna > 0.0, "vanna {}", g.vanna);
    }
}
