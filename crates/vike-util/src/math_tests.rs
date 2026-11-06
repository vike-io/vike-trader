use super::*;

/// `norm_cdf`'s output BITS at fixed inputs, recorded from the two copies it replaced
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
fn norm_cdf_bits_are_pinned() {
    let mut wrong = Vec::new();
    for &(x, bits) in GOLDEN {
        let got = norm_cdf(x).to_bits();
        if got != bits {
            wrong.push(format!("    ({x:?}, {got:#020x}), // got {}", f64::from_bits(got)));
        }
    }
    assert!(wrong.is_empty(), "norm_cdf moved off its pinned bits:\n{}", wrong.join("\n"));
}

#[test]
fn norm_cdf_is_the_expression_it_replaced() {
    // The deleted copies' expression, verbatim. Same operands, same association, so the same bits
    // on every input, the tails and the non-finite ones included.
    fn copy(x: f64) -> f64 {
        0.5 * (1.0 + libm::erf(x / std::f64::consts::SQRT_2))
    }
    let mut x = -45.0_f64;
    while x <= 45.0 {
        assert_eq!(norm_cdf(x).to_bits(), copy(x).to_bits(), "x = {x:?}");
        x += 0.0137;
    }
    for x in [f64::INFINITY, f64::NEG_INFINITY, f64::MIN_POSITIVE, -0.0, f64::MAX, f64::MIN] {
        assert_eq!(norm_cdf(x).to_bits(), copy(x).to_bits(), "x = {x:?}");
    }
    assert!(norm_cdf(f64::NAN).is_nan());
}

#[test]
fn norm_cdf_is_symmetric_about_one_half() {
    for x in [0.25, 1.0, 2.0, 3.5] {
        assert!((norm_cdf(x) + norm_cdf(-x) - 1.0).abs() < 1e-15, "x = {x}");
    }
}
