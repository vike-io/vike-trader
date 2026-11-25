use super::*;

/// A skewed slice free of butterfly arbitrage: its density's least on k in [−3, 3] (step 0.01) is
/// 0.1886, at k = −0.59 (measured by hand).
const SMILE: SviParameters = SviParameters { a: 0.04, b: 0.4, rho: -0.4, m: 0.1, sigma: 0.1 };

/// `k` from `from` to `to` in `steps` equal steps, both ends included.
fn grid(from: f64, to: f64, steps: usize) -> Vec<f64> {
    (0..=steps).map(|i| from + (to - from) * i as f64 / steps as f64).collect()
}

#[test]
fn the_slice_is_gatheral_s_formula() {
    // At the vertex k = m: a + b·σ (ρ·0 + √σ²) = 0.04 + 0.4 × 0.1 = 0.08.
    assert!((SMILE.total_variance(0.1) - 0.08).abs() < 1e-16);
    // At k = 0.5: x = 0.4, √(0.16 + 0.01) = 0.412310…, so 0.04 + 0.4 × (−0.16 + 0.412310…).
    let w = 0.04 + 0.4 * (-0.16 + 0.17f64.sqrt());
    assert!((SMILE.total_variance(0.5) - w).abs() < 1e-16);
    let iv = SMILE.implied_vol(0.1, 0.25).expect("a vol");
    assert!((iv - (0.08f64 / 0.25).sqrt()).abs() < 1e-15, "{iv}");
    assert_eq!(SMILE.implied_vol(0.1, 0.0), None);
    assert!(SMILE.is_admissible());
    for bad in [
        SviParameters { b: -0.1, ..SMILE },
        SviParameters { rho: 1.0, ..SMILE },
        SviParameters { rho: f64::NAN, ..SMILE },
        SviParameters { sigma: 0.0, ..SMILE },
        SviParameters { a: -0.1, ..SMILE }, // −0.1 + 0.4 × 0.1 × √0.84 < 0
    ] {
        assert!(!bad.is_admissible(), "{bad:?}");
    }
}

/// The fit recovers a known slice from its own total variances on 21 strikes, k in [−0.5, 0.5].
#[test]
fn the_fit_recovers_a_synthetic_smile_s_parameters() {
    let points: Vec<(f64, f64)> =
        grid(-0.5, 0.5, 20).into_iter().map(|k| (k, SMILE.total_variance(k))).collect();
    let fit = fit_svi(&points).expect("a fit");
    println!("svi fit of the synthetic smile: {fit:?}");
    let p = fit.parameters;
    for (name, got, want) in [
        ("a", p.a, SMILE.a),
        ("b", p.b, SMILE.b),
        ("rho", p.rho, SMILE.rho),
        ("m", p.m, SMILE.m),
        ("sigma", p.sigma, SMILE.sigma),
    ] {
        assert!((got - want).abs() < 1e-6, "{name}: {got} against {want} ({fit:?})");
    }
    assert!(fit.rmse < 1e-9, "{fit:?}");
    assert!(p.is_admissible());
    // The same smile handed over as implied vols of a quarter-year expiry, in any order.
    let years = 0.25;
    let mut from_vols: Vec<(f64, f64)> = points
        .iter()
        .rev()
        .map(|&(k, w)| {
            let iv = (w / years).sqrt();
            (k, iv * iv * years)
        })
        .collect();
    from_vols.swap(3, 17);
    let again = fit_svi(&from_vols).expect("a fit");
    assert!((again.parameters.rho - SMILE.rho).abs() < 1e-6, "{again:?}");
}

#[test]
fn the_fit_refuses_what_is_not_a_smile() {
    let five = [(-0.2, 0.1), (-0.1, 0.09), (0.0, 0.08), (0.1, 0.085), (0.2, 0.095)];
    assert!(fit_svi(&five[..4]).is_none(), "four points for five parameters");
    assert!(fit_svi(&[(0.0, 0.08); 5]).is_none(), "one strike");
    assert!(
        fit_svi(&[(-0.2, 0.1), (-0.1, f64::NAN), (0.0, 0.08), (0.1, 0.085), (0.2, 0.09)]).is_none()
    );
    assert!(
        fit_svi(&[(-0.2, 0.1), (-0.1, -0.01), (0.0, 0.08), (0.1, 0.085), (0.2, 0.09)]).is_none()
    );
    assert!(fit_svi(&five).is_some());
}

/// Wings steeper than Lee's bound (`b·(1 + ρ) = 3 > 2`) price a negative density: by hand at k = 3,
/// x = 3, √9.01 = 3.0016662, w = 9.0441656, w′ = 2.9986121, w″ = 0.0009245, so
/// g = (1 − 3·w′/(2w))² − (w′²/4)(1/w + 1/4) + w″/2 = −0.5573876379329004.
#[test]
fn butterfly_arbitrage_is_a_negative_density() {
    let steep = SviParameters { a: 0.04, b: 2.5, rho: 0.2, m: 0.0, sigma: 0.1 };
    assert!((steep.butterfly_density(3.0) + 0.557_387_637_932_900_4).abs() < 1e-12);
    let found = butterfly_arbitrage(&steep, &grid(-3.0, 3.0, 600));
    assert!(found.len() > 500 && found.contains(&3.0), "{} points", found.len());
    assert!(butterfly_arbitrage(&SMILE, &grid(-3.0, 3.0, 600)).is_empty(), "the smile is clean");
}

/// Total variance must not fall with maturity. Doubling `a` and `b` doubles `w` (clean); a later
/// slice with the skew turned the other way, `w₂ − w₁ = 0.01 + 0.24·(k − 0.1)`, falls below the
/// earlier one wherever `k < 0.0583`: on k = −1, −0.9, …, 1 the eleven points from −1 to 0.
#[test]
fn calendar_arbitrage_is_a_later_slice_below_an_earlier_one() {
    let early = SviSlice { years: 0.25, parameters: SMILE };
    let doubled = SviParameters { a: 0.08, b: 0.8, ..SMILE };
    let k = grid(-1.0, 1.0, 20);
    assert!(
        calendar_arbitrage(&[early, SviSlice { years: 0.5, parameters: doubled }], &k).is_empty()
    );
    let turned = SviParameters { a: 0.05, rho: 0.2, ..SMILE };
    // Handed over later first: the indices are the caller's.
    let found = calendar_arbitrage(&[SviSlice { years: 0.5, parameters: turned }, early], &k);
    assert_eq!(found.len(), 11, "{found:?}");
    assert!(found.iter().all(|f| f.earlier == 1 && f.later == 0 && f.k <= 0.0), "{found:?}");
    let same_maturity = SviSlice { years: 0.25, parameters: turned };
    assert!(calendar_arbitrage(&[early, same_maturity], &k).is_empty(), "one maturity");
}
