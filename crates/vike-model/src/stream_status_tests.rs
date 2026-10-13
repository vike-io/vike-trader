//! The two facts other crates lean on: the serde bytes (the datahub market-data wire) and the one
//! projection onto [`FeedStatus`].
use super::*;

/// The WIRE pin. These are the exact bytes the datahub's `MdFrame::Status` has carried since the
/// retired `WireStreamStatus` mirror; a renamed variant or field changes what a deployed datahub
/// sends a desktop, so it must fail here first. EXHAUSTIVE: the no-`_` match makes a new variant
/// fail to COMPILE until it is given its bytes.
#[test]
fn the_serde_bytes_are_the_market_data_wire() {
    let cases = [
        (StreamStatus::GapStart { at_ts_ms: 7 }, r#"{"GapStart":{"at_ts_ms":7}}"#),
        (StreamStatus::Live { gap_started_ts_ms: None }, r#"{"Live":{"gap_started_ts_ms":null}}"#),
        (StreamStatus::Live { gap_started_ts_ms: Some(7) }, r#"{"Live":{"gap_started_ts_ms":7}}"#),
        (
            StreamStatus::Stale { newest_data_ts_ms: 5, now_ms: 9 },
            r#"{"Stale":{"newest_data_ts_ms":5,"now_ms":9}}"#,
        ),
    ];
    for (status, json) in cases {
        match status {
            StreamStatus::GapStart { .. }
            | StreamStatus::Live { .. }
            | StreamStatus::Stale { .. } => {}
        }
        assert_eq!(serde_json::to_string(&status).unwrap(), json);
        assert_eq!(serde_json::from_str::<StreamStatus>(json).unwrap(), status);
    }
}

/// The one map onto the strategy-facing state: a gap is `Disconnected`, staleness is `Stale`, a
/// recovery is `Live`, whatever the stamps.
#[test]
fn feed_status_is_the_state_without_the_stamps() {
    assert_eq!(StreamStatus::GapStart { at_ts_ms: 7 }.feed_status(), FeedStatus::Disconnected);
    assert_eq!(StreamStatus::Live { gap_started_ts_ms: Some(7) }.feed_status(), FeedStatus::Live);
    assert_eq!(StreamStatus::Live { gap_started_ts_ms: None }.feed_status(), FeedStatus::Live);
    assert_eq!(
        StreamStatus::Stale { newest_data_ts_ms: 5, now_ms: 9 }.feed_status(),
        FeedStatus::Stale
    );
}
