//! The option-market lanes' shared rules: the number grammar and the quote that is absent at zero.

use super::*;

#[test]
fn a_number_is_finite_and_an_empty_field_is_absent() {
    assert_eq!(number("0E-8", "f"), Ok(Some(0.0)), "the vendor's zero");
    assert_eq!(number("-39.08507011", "f"), Ok(Some(-39.085_070_11)));
    assert_eq!(number("", "f"), Ok(None));
    assert_eq!(number("\"\"", "f"), Ok(None), "the vendor's empty field");
    for bad in ["NaN", "inf", "-inf", "1,5", "x"] {
        assert!(number(bad, "f").is_err(), "{bad}");
    }
    assert!(not_negative("-1E-8", "f").is_err());
    assert_eq!(not_negative("0E-8", "f"), Ok(Some(0.0)));
}

#[test]
fn a_zero_quote_is_absent_and_a_positive_one_kept() {
    assert_eq!(quote("0E-8", "bid"), Ok(None));
    assert_eq!(quote("", "bid"), Ok(None));
    assert_eq!(quote("80.00000000", "bid"), Ok(Some(80.0)));
    assert!(quote("-80", "bid").is_err());
}
