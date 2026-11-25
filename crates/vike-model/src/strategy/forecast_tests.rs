use super::*;

#[test]
fn the_rule_accepts_a_finite_value_over_a_positive_horizon() {
    let f = Forecast::new(-0.002, 3_600_000, Some(0.75)).unwrap();
    assert_eq!((f.value(), f.horizon_ms(), f.confidence()), (-0.002, 3_600_000, Some(0.75)));
    let bare = Forecast::new(1.5, 1, None).unwrap();
    assert_eq!(bare.confidence(), None);
    // The bounds of the confidence are inside.
    assert!(Forecast::new(0.0, 60_000, Some(0.0)).is_ok());
    assert!(Forecast::new(0.0, 60_000, Some(1.0)).is_ok());
}

#[test]
fn the_rule_refuses_each_bad_term_in_its_own_words() {
    let refused = |v, h, c| Forecast::new(v, h, c).unwrap_err();
    assert!(refused(f64::NAN, 60_000, None).contains("value"));
    assert!(refused(f64::INFINITY, 60_000, None).contains("value"));
    assert!(refused(0.1, 0, None).contains("horizon"));
    assert!(refused(0.1, -5, None).contains("horizon"));
    assert!(refused(0.1, 60_000, Some(1.01)).contains("confidence"));
    assert!(refused(0.1, 60_000, Some(-0.01)).contains("confidence"));
    assert!(refused(0.1, 60_000, Some(f64::NAN)).contains("confidence"));
}

/// A row's document: the confidence is absent when not said, and a document is read back through the
/// same rule, so a horizon of zero written by hand does not read.
#[test]
fn a_row_reads_back_through_the_rule() {
    let row = ForecastRow {
        timestamp: 1_700_000_000_000,
        symbol: "BTCUSDT".into(),
        forecast: Forecast::new(0.01, 86_400_000, None).unwrap(),
    };
    let json = serde_json::to_string(&row).unwrap();
    assert_eq!(
        json,
        r#"{"timestamp":1700000000000,"symbol":"BTCUSDT","forecast":{"value":0.01,"horizon_ms":86400000}}"#
    );
    assert_eq!(serde_json::from_str::<ForecastRow>(&json).unwrap(), row);
    let zero = r#"{"timestamp":1,"symbol":"X","forecast":{"value":0.01,"horizon_ms":0}}"#;
    let err = serde_json::from_str::<ForecastRow>(zero).unwrap_err().to_string();
    assert!(err.contains("horizon"), "{err}");
}
