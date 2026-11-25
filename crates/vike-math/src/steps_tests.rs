use super::*;

/// The decimals of a decimal step, a step under a billionth (an absolute tolerance answered 0 for
/// every one of them), a quarter (two decimals, never one) and a negative step (its size).
#[test]
fn a_step_needs_the_decimals_its_multiples_print_with() {
    for (step, decimals) in [
        (1.0, 0),
        (20.0, 0),
        (0.5, 1),
        (0.1, 1),
        (0.25, 2),
        (0.01, 2),
        (0.001, 3),
        (0.0001, 4),
        (1e-8, 8),
        (1e-10, 10),
        (2.5e-11, 12),
        (1e-12, 12),
        (1e-13, 13),
        (-0.01, 2),
    ] {
        assert_eq!(step_decimals(step), Some(decimals), "{step}");
    }
}

/// The 1/2/5·10^k marks a chart draws: the decimals that tell two neighbours apart.
#[test]
fn a_one_two_five_mark_step_needs_the_decimals_of_its_power_of_ten() {
    for k in -15..=6 {
        let decade = libm::pow(10.0, f64::from(k));
        let want = usize::try_from(-k).unwrap_or(0);
        for mantissa in [1.0, 2.0, 5.0] {
            let step = mantissa * decade;
            assert_eq!(step_decimals(step), Some(want), "{step}");
        }
    }
}

/// No decimals belong to a step that is zero or not a number, and a step that is not a decimal
/// fraction still ends.
#[test]
fn a_step_that_is_not_one_has_no_decimals_and_a_third_ends() {
    for step in [0.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(step_decimals(step), None, "{step}");
    }
    let third = step_decimals(1.0 / 3.0).expect("a third is a finite step");
    assert!((1..=12).contains(&third), "a third ended at {third} decimals");
}

/// The chart's axis-mark decimals, pinned as vike-chart's `chart/fmt.rs` answered them before the
/// move: the decade of a grid step, a step off the grid at its decade's, and an inverted axis's step
/// (a difference of two mapped prices) by its decade, not by its noise.
#[test]
fn an_axis_mark_step_prints_the_decimals_of_its_decade() {
    for (step, decimals) in [
        (100.0, 0),
        (1.0, 0),
        (0.5, 1),
        (0.2, 1),
        (0.1, 1),
        (0.05, 2),
        (0.001, 3),
        (1e-6, 6),
        (1e-13, 13),
        (0.25, 1),
        (65_432.1, 0),
    ] {
        assert_eq!(axis_mark_decimals(step), Some(decimals), "{step}");
    }
    // 0.001 marks on an inverted axis near 65,432: the step is the difference of two mapped prices.
    let above = 65_432.002_f64 - 65_432.001;
    assert_eq!(above, 0.0010000000038417056);
    assert_eq!(axis_mark_decimals(above), Some(3));
    assert_eq!(step_decimals(above), Some(12), "the exact reading counts the noise as digits");
    let below = 65_432.001_f64 - 65_432.0;
    assert_eq!(below, 0.000999999996565748);
    assert_eq!(axis_mark_decimals(below), Some(4), "further under 0.001 than the 1e-9 forgives");
    for step in [0.0, -0.01, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(axis_mark_decimals(step), None, "{step}");
    }
}

/// `Up` is the ceiling (`<=` at 1, 2 and 5), `Nearest` cuts at the midpoints 1.5, 3.5 and 7.5.
#[test]
fn the_two_roundings_pick_their_nice_step() {
    use NiceStepRounding::{Nearest, Up};
    for (raw, want) in [(1.0, 1.0), (1.5, 2.0), (2.0, 2.0), (3.0, 5.0), (5.0, 5.0), (7.0, 10.0)] {
        assert_eq!(nice_step(raw, Up), want, "up {raw}");
    }
    assert_eq!(nice_step(150.0, Up), 200.0);
    for (raw, want) in [
        (1.4, 1.0),
        (1.5, 2.0),
        (3.4, 2.0),
        (3.5, 5.0),
        (7.4, 5.0),
        (7.5, 10.0),
        (12.8, 10.0),
        (0.6, 0.5),
    ] {
        assert_eq!(nice_step(raw, Nearest), want, "nearest {raw}");
    }
}

/// A raw step that is not a positive finite number has no nice step, in either rounding.
#[test]
fn a_raw_step_that_is_not_positive_has_no_nice_step() {
    for rounding in [NiceStepRounding::Up, NiceStepRounding::Nearest] {
        for raw in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(nice_step(raw, rounding).is_nan(), "{raw} {rounding:?}");
        }
    }
}
