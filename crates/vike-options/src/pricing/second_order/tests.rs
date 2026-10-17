//! Unit tests for [`super`] — reach, signs and edge cases of the five second-order greeks.

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
