use super::*;

fn close(got: f64, want: f64, relative: f64) -> bool {
    (got - want).abs() <= relative * want.abs()
}

/// At the money with `r = 0`: S = K = 50,000, σ = 0.4, t = 0.25, so σ√t = 0.2, d1 = 0.1, d2 = −0.1,
/// N(0.1) = 0.539827837277029 and φ(0.1) = e^{−0.005}/√(2π) = 0.3969525474770118. By hand:
/// - coin price N(d1) − N(d2) = 0.539827837277029 − 0.460172162722971 = 0.079655674554058 (call = put);
/// - call delta Δ − C = N(d2) = 0.460172162722971; put delta (N(d1) − 1) − C = −N(d1);
/// - BS gamma φ(d1)/(S·0.2) = 3.9695254747701184e-5, call coin gamma minus 0.460172162722971/S
///   = 3.049181149324176e-5;
/// - vega S·φ·√t/100 over S = 0.001984762737385059; theta −S·φ·σ/(2√t)/365 over S
///   = −4.350164903857664e-4.
#[test]
fn an_at_the_money_coin_option_by_hand() {
    let call = coin_settled_greeks(50_000.0, 50_000.0, 0.25, 0.4, OptionKind::Call, 0.0).unwrap();
    let put = coin_settled_greeks(50_000.0, 50_000.0, 0.25, 0.4, OptionKind::Put, 0.0).unwrap();
    let tight = 1e-12;
    assert!(close(call.price, 0.079_655_674_554_058, tight), "{call:?}");
    assert!(close(put.price, 0.079_655_674_554_058, tight), "{put:?}");
    assert!(close(call.delta, 0.460_172_162_722_971, tight), "{call:?}");
    assert!(close(put.delta, -0.539_827_837_277_029, tight), "{put:?}");
    assert!(close(call.gamma, 3.049_181_149_324_176e-5, tight), "{call:?}");
    assert!(close(call.vega, 0.001_984_762_737_385_059, tight), "{call:?}");
    assert!(close(call.theta, -4.350_164_903_857_664e-4, tight), "{call:?}");
    // The coin price is the USD one over the price, the USD price the chain fold shows.
    let usd = black_scholes_price(50_000.0, 50_000.0, 0.25, 0.4, OptionKind::Call, 0.0).unwrap();
    assert_eq!(call.price.to_bits(), (usd / 50_000.0).to_bits());
}

/// The closed forms with a rate, off the money: a call's coin delta is (K/S)·e^{−rt}·N(d2), a put's
/// −(K/S)·e^{−rt}·N(−d2); checked against `N(d2)` from the crate's own normal CDF.
#[test]
fn the_premium_adjusted_delta_is_the_discounted_strike_weighted_n_of_d2() {
    let (s, k, t, sigma, r) = (52_000.0, 55_000.0, 0.1, 0.65, 0.03);
    let (_, d2, _) = crate::pricing::greeks::d1_d2(s, k, t, sigma, r);
    let weight = k / s * libm::exp(-r * t);
    let norm = vike_math::distributions::normal_cdf;
    let call = coin_settled_greeks(s, k, t, sigma, OptionKind::Call, r).unwrap();
    let put = coin_settled_greeks(s, k, t, sigma, OptionKind::Put, r).unwrap();
    assert!(close(call.delta, weight * norm(d2), 1e-12), "{call:?}");
    assert!(close(put.delta, -weight * norm(-d2), 1e-12), "{put:?}");
}

/// Finite differences of the coin price `C(S) = V(S)/S` and of its premium-adjusted delta, a central
/// step of 1 USD on 50,000: the delta is `S·dC/dS`, the gamma `d(delta)/dS`.
#[test]
fn delta_and_gamma_are_the_derivatives_of_the_coin_price() {
    let (k, t, sigma, r, h) = (48_000.0, 0.2, 0.55, 0.0, 1.0);
    for kind in [OptionKind::Call, OptionKind::Put] {
        let at = |s: f64| coin_settled_greeks(s, k, t, sigma, kind, r).unwrap();
        let s = 50_000.0;
        let slope = (at(s + h).price - at(s - h).price) / (2.0 * h);
        assert!(close(at(s).delta, s * slope, 1e-6), "{kind:?}: {} vs {}", at(s).delta, s * slope);
        let curvature = (at(s + h).delta - at(s - h).delta) / (2.0 * h);
        assert!(close(at(s).gamma, curvature, 1e-5), "{kind:?}: {} vs {curvature}", at(s).gamma);
    }
    assert_eq!(coin_settled_greeks(50_000.0, k, 0.0, sigma, OptionKind::Call, r), None);
}
