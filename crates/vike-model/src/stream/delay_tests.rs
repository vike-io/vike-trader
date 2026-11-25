//! The serde bytes (what any process boundary ships) and the one word per label.
use super::*;

/// EXHAUSTIVE: the no-`_` match makes a new variant fail to COMPILE until it is given its bytes.
#[test]
fn the_serde_bytes_are_pinned() {
    let cases = [
        (StreamDelay::RealTime, r#""RealTime""#),
        (StreamDelay::Frozen, r#""Frozen""#),
        (StreamDelay::DELAYED, r#"{"Delayed":{"lag_ms":null}}"#),
        (StreamDelay::Delayed { lag_ms: Some(900_000) }, r#"{"Delayed":{"lag_ms":900000}}"#),
    ];
    for (delay, json) in cases {
        match delay {
            StreamDelay::RealTime | StreamDelay::Delayed { .. } | StreamDelay::Frozen => {}
        }
        assert_eq!(serde_json::to_string(&delay).unwrap(), json);
        assert_eq!(serde_json::from_str::<StreamDelay>(json).unwrap(), delay);
    }
}

#[test]
fn only_real_time_is_real_time_and_each_label_has_one_word() {
    assert!(StreamDelay::RealTime.is_real_time());
    assert!(!StreamDelay::DELAYED.is_real_time());
    assert!(!StreamDelay::Delayed { lag_ms: Some(1) }.is_real_time());
    assert!(!StreamDelay::Frozen.is_real_time());
    assert_eq!(StreamDelay::RealTime.word(), "real-time");
    assert_eq!(StreamDelay::Delayed { lag_ms: Some(1) }.word(), "delayed");
    assert_eq!(StreamDelay::Frozen.word(), "frozen");
}

/// The word round-trips every label except a stated lag (which the word does not carry), and an
/// unknown word is no label.
#[test]
fn the_word_round_trips_and_an_unknown_word_is_no_label() {
    for d in [StreamDelay::RealTime, StreamDelay::DELAYED, StreamDelay::Frozen] {
        assert_eq!(StreamDelay::from_word(d.word()), Some(d));
    }
    assert_eq!(StreamDelay::from_word("delayed"), Some(StreamDelay::Delayed { lag_ms: None }));
    assert_eq!(StreamDelay::from_word("Delayed"), None);
    assert_eq!(StreamDelay::from_word(""), None);
}
