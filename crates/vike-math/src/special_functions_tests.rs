use super::*;

/// The logistic pair at its fixed points and edges; `logit` is unclamped by contract.
#[test]
fn logistic_and_logit_are_inverse_and_keep_their_edges() {
    assert_eq!(logistic(0.0), 0.5);
    assert_eq!((logistic(f64::NEG_INFINITY), logistic(f64::INFINITY)), (0.0, 1.0));
    assert_eq!(logit(0.5), 0.0);
    assert_eq!((logit(0.0), logit(1.0)), (f64::NEG_INFINITY, f64::INFINITY));
    assert!(logit(1.5).is_nan());
    for p in [0.05, 0.25, 0.7, 0.95] {
        assert!((logistic(logit(p)) - p).abs() < 1e-15, "{p}");
    }
}

#[test]
fn softmax_is_a_probability_vector_in_the_inputs_order() {
    // Hand-computed: exp(0), exp(ln 2), exp(ln 3) over 1 + 2 + 3.
    let mut scores = [0.0, libm::log(2.0), libm::log(3.0)];
    softmax(&mut scores);
    for (got, want) in scores.iter().zip([1.0 / 6.0, 2.0 / 6.0, 3.0 / 6.0]) {
        assert!((got - want).abs() < 1e-15, "{got} vs {want}");
    }
    // Shifting every score by a constant changes nothing, and huge scores do not overflow.
    let mut shifted = [1000.0, 1000.0 + libm::log(2.0), 1000.0 + libm::log(3.0)];
    softmax(&mut shifted);
    for (a, b) in shifted.iter().zip(scores) {
        assert!((a - b).abs() < 1e-12, "{a} vs {b}");
    }
    let mut one = [-7.5];
    softmax(&mut one);
    assert_eq!(one, [1.0]);
    let mut none: [f64; 0] = [];
    softmax(&mut none);
}
