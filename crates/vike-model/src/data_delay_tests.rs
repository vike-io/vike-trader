//! The serde bytes (what any process boundary ships) and the one word per label.
use super::*;

/// EXHAUSTIVE: the no-`_` match makes a new variant fail to COMPILE until it is given its bytes.
#[test]
fn the_serde_bytes_are_pinned() {
    let cases = [
        (DataDelay::RealTime, r#""RealTime""#),
        (DataDelay::Frozen, r#""Frozen""#),
        (DataDelay::DELAYED, r#"{"Delayed":{"lag_ms":null}}"#),
        (DataDelay::Delayed { lag_ms: Some(900_000) }, r#"{"Delayed":{"lag_ms":900000}}"#),
    ];
    for (delay, json) in cases {
        match delay {
            DataDelay::RealTime | DataDelay::Delayed { .. } | DataDelay::Frozen => {}
        }
        assert_eq!(serde_json::to_string(&delay).unwrap(), json);
        assert_eq!(serde_json::from_str::<DataDelay>(json).unwrap(), delay);
    }
}

#[test]
fn only_real_time_is_real_time_and_each_label_has_one_word() {
    assert!(DataDelay::RealTime.is_real_time());
    assert!(!DataDelay::DELAYED.is_real_time());
    assert!(!DataDelay::Delayed { lag_ms: Some(1) }.is_real_time());
    assert!(!DataDelay::Frozen.is_real_time());
    assert_eq!(DataDelay::RealTime.word(), "real-time");
    assert_eq!(DataDelay::Delayed { lag_ms: Some(1) }.word(), "delayed");
    assert_eq!(DataDelay::Frozen.word(), "frozen");
}

/// The word round-trips every label except a stated lag (which the word does not carry), and an
/// unknown word is no label.
#[test]
fn the_word_round_trips_and_an_unknown_word_is_no_label() {
    for d in [DataDelay::RealTime, DataDelay::DELAYED, DataDelay::Frozen] {
        assert_eq!(DataDelay::from_word(d.word()), Some(d));
    }
    assert_eq!(DataDelay::from_word("delayed"), Some(DataDelay::Delayed { lag_ms: None }));
    assert_eq!(DataDelay::from_word("Delayed"), None);
    assert_eq!(DataDelay::from_word(""), None);
}
