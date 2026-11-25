use super::*;

/// `normal_cdf`'s output BITS at fixed inputs, recorded from the two copies it replaced
/// (`vike-options`' `greeks::norm_cdf` and the inline tail of `vike_model::fair::p_up`), whose
/// expression it keeps verbatim.
const GOLDEN: &[(f64, u64)] = &[
    (0.0, 0x3fe0_0000_0000_0000),  // ½ exactly: erf(0) = 0
    (1.0, 0x3fea_ec4b_d120_d37d),  // 0.8413447460685429
    (-1.0, 0x3fc4_4ed0_bb7c_b20c), // 0.15865525393145707
    (1.96, 0x3fef_3337_9d3b_d368), // 0.9750021048517796
    (-2.5, 0x3f79_6f4e_57e4_9d00), // 0.006209665325776159
    (0.1, 0x3fe1_4645_0752_6871),  // 0.539827837277029
    (8.0, 0x3fef_ffff_ffff_fffa),  // 0.9999999999999993
    // 6.106226635438361e-16: `1 + erf` cancels in the lower tail, the `erf` form's known cost
    (-8.0, 0x3cc6_0000_0000_0000),
    (-37.5, 0x0000_0000_0000_0000), // erf saturates to -1 already: +0.0
    (1e-300, 0x3fe0_0000_0000_0000), // the argument vanishes below erf's resolution: ½
    (40.0, 0x3ff0_0000_0000_0000),  // saturates to 1.0
    (-40.0, 0x0000_0000_0000_0000), // saturates to +0.0
];

#[test]
fn normal_cdf_bits_are_pinned() {
    let mut wrong = Vec::new();
    for &(x, bits) in GOLDEN {
        let got = normal_cdf(x).to_bits();
        if got != bits {
            wrong.push(format!("    ({x:?}, {got:#020x}), // got {}", f64::from_bits(got)));
        }
    }
    assert!(wrong.is_empty(), "normal_cdf moved off its pinned bits:\n{}", wrong.join("\n"));
}

#[test]
fn normal_cdf_is_the_expression_it_replaced() {
    // The deleted copies' expression, verbatim. Same operands, same association, so the same bits
    // on every input, the tails and the non-finite ones included.
    fn copy(x: f64) -> f64 {
        0.5 * (1.0 + libm::erf(x / std::f64::consts::SQRT_2))
    }
    let mut x = -45.0_f64;
    while x <= 45.0 {
        assert_eq!(normal_cdf(x).to_bits(), copy(x).to_bits(), "x = {x:?}");
        x += 0.0137;
    }
    for x in [f64::INFINITY, f64::NEG_INFINITY, f64::MIN_POSITIVE, -0.0, f64::MAX, f64::MIN] {
        assert_eq!(normal_cdf(x).to_bits(), copy(x).to_bits(), "x = {x:?}");
    }
    assert!(normal_cdf(f64::NAN).is_nan());
}

#[test]
fn normal_cdf_is_symmetric_about_one_half() {
    for x in [0.25, 1.0, 2.0, 3.5] {
        assert!((normal_cdf(x) + normal_cdf(-x) - 1.0).abs() < 1e-15, "x = {x}");
    }
}

/// The roundtrip that moved here with the two functions from `vike-analytics`' `overfit` tests,
/// unchanged, plus the textbook critical values a confidence interval reads.
#[test]
fn normal_quantile_inverts_the_complementary_form() {
    assert!((normal_cdf_complementary_form(0.0) - 0.5).abs() < 1e-12);
    for &x in &[-2.5, -1.0, -0.3, 0.7, 1.96, 3.0] {
        assert!(
            (normal_quantile(normal_cdf_complementary_form(x)) - x).abs() < 1e-9,
            "roundtrip {x}"
        );
    }
    // Φ(1.96) ≈ 0.975
    assert!((normal_cdf_complementary_form(1.96) - 0.9750021048517795).abs() < 1e-9);
    // Φ⁻¹(0.975), Φ⁻¹(0.95), Φ⁻¹(0.995): the two-sided 95%, 90% and 99% critical values.
    for (p, z) in [(0.975, 1.959963984540054), (0.95, 1.6448536269514722), (0.995, 2.5758293035489)]
    {
        assert!((normal_quantile(p) - z).abs() < 1e-12, "Φ⁻¹({p}) = {} vs {z}", normal_quantile(p));
        assert!((normal_quantile(1.0 - p) + z).abs() < 1e-12, "the lower tail mirrors at {p}");
    }
    assert_eq!(normal_quantile(0.5), 0.0);
    assert_eq!(normal_quantile(0.0), f64::NEG_INFINITY);
    assert_eq!(normal_quantile(1.0), f64::INFINITY);
}

/// The two CDF forms agree where neither cancels and differ in the lower tail, which is why both
/// are kept.
#[test]
fn the_two_cdf_forms_agree_away_from_the_lower_tail() {
    for x in [-3.0, -1.0, 0.0, 0.5, 2.0, 6.0] {
        assert!((normal_cdf(x) - normal_cdf_complementary_form(x)).abs() < 1e-15, "x = {x}");
    }
    // At -8 the `erf` form has cancelled to a few ULP of 1; the `erfc` form keeps the digits.
    let tail = normal_cdf_complementary_form(-8.0);
    assert!((tail - 6.22096057427178e-16).abs() < 1e-28, "{tail:e}");
    assert_ne!(normal_cdf(-8.0).to_bits(), tail.to_bits());
}

/// The density at the mean is `1/√τ` and it is symmetric (the TPE sampler's closed-form test, moved
/// here with its `gaussian_pdf`), and the standard normal is the options greeks' `norm_pdf`
/// expression to the bit, the proof that `vike-options` could drop its copy.
#[test]
fn the_normal_density_is_the_expressions_it_replaced() {
    let peak = 1.0 / std::f64::consts::TAU.sqrt();
    assert!((normal_density(0.0, 0.0, 1.0) - peak).abs() < 1e-12, "N(0; 0, 1) = 1/√τ");
    assert!((normal_density(1.3, 0.0, 1.0) - normal_density(-1.3, 0.0, 1.0)).abs() < 1e-15);
    assert!(
        (normal_density(3.0, 1.0, 2.0) - normal_density(0.0, 0.0, 1.0) * libm::exp(-0.5) / 2.0)
            .abs()
            < 1e-15
    );
    for i in -4000..=4000 {
        let x = f64::from(i) * 0.003_7;
        let options_form = libm::exp(-0.5 * x * x) / (2.0 * std::f64::consts::PI).sqrt();
        assert_eq!(normal_density(x, 0.0, 1.0).to_bits(), options_form.to_bits(), "x = {x}");
    }
    assert_eq!(normal_density(-0.0, 0.0, 1.0).to_bits(), normal_density(0.0, 0.0, 1.0).to_bits());
}

/// The two properties `vike_model::fair::p_up`'s own tests pinned, carried with it: a flat spot is a
/// coin flip EXACTLY whatever the other inputs (`ln 1 = 0`, `Φ(0) = ½`), and the probability rises with
/// the spot and falls back toward the flip as sigma grows. The bits are pinned by the cheap_np parity
/// gate and by the model probe's `distributions::up_probability` row.
#[test]
fn a_flat_spot_is_a_coin_flip_and_the_probability_rises_with_the_spot() {
    assert_eq!(up_probability(0.83, 100.0, 100.0, 1e-4, 100.0, 300.0), 0.5);
    assert_eq!(up_probability(2.0, 5.0, 5.0, 0.01, 0.0, 300.0), 0.5);
    let up = up_probability(0.83, 100.5, 100.0, 1e-4, 100.0, 300.0);
    let up_more = up_probability(0.83, 101.0, 100.0, 1e-4, 100.0, 300.0);
    assert!(up > 0.5 && up_more > up, "{up} {up_more}");
    let noisier = up_probability(0.83, 100.5, 100.0, 1e-3, 100.0, 300.0);
    assert!(noisier < up && noisier > 0.5, "{noisier} {up}");
}

/// The window's last instant (and past it) divides by the `1e-9` floor, never by zero: a finite
/// probability, the same as at the floor exactly.
#[test]
fn the_last_instant_of_the_window_divides_by_the_floor() {
    let at_close = up_probability(0.83, 100.5, 100.0, 1e-4, 300.0, 300.0);
    assert!(at_close.is_finite() && at_close > 0.5, "{at_close}");
    let past_close = up_probability(0.83, 100.5, 100.0, 1e-4, 301.0, 300.0);
    assert_eq!(at_close.to_bits(), past_close.to_bits());
}
