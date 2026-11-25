use super::*;
use serde_json::json;

/// The non-finite mapping, as a unit — the three tags are the values a reader matches on.
#[test]
fn the_non_finite_tags_are_the_three_ieee_values_and_nothing_else() {
    assert_eq!(nonfinite_tag(f64::NAN), Some("NaN"));
    assert_eq!(nonfinite_tag(f64::INFINITY), Some("inf"));
    assert_eq!(nonfinite_tag(f64::NEG_INFINITY), Some("-inf"));
    assert_eq!(nonfinite_tag(0.0), None);
    assert_eq!(nonfinite_tag(-0.0), None);
    assert_eq!(nonfinite_tag(f64::MAX), None);
    assert_eq!(
        serde_json::to_value(StudyMetric::of("x", 1.5)).unwrap(),
        json!({"name":"x","value":1.5,"nonfinite":null})
    );
    // The ONE printed form: six decimals, or the tag — never a number a non-finite was rounded into.
    assert_eq!(StudyMetric::of("x", 1.5).text(), "1.500000");
    assert_eq!(StudyMetric::of("x", f64::NAN).text(), "NaN");
    assert_eq!(StudyMetric::of("x", f64::NEG_INFINITY).text(), "-inf");
}
