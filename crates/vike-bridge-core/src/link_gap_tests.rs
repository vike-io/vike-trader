//! The minute's summary over a scripted sequence of event times: the count, the longest gap (one that spans two windows
//! is the later window's) and the age of the last event, and the one line it says.

use super::*;
use vike_log::capture::captured;

fn at(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

#[test]
fn each_window_counts_its_events_and_keeps_the_longest_gap_that_ended_in_it() {
    let t0 = Instant::now();
    let mut gaps = GapWindow::new(t0);
    assert_eq!(gaps.take_due(t0), None, "a window that has not run a minute says nothing");

    // Gaps of 0.5 s, 3 s and 6 s.
    for ms in [500, 1_000, 4_000, 10_000] {
        gaps.note(at(t0, ms));
    }
    assert_eq!(gaps.take_due(at(t0, 59_999)), None);
    assert_eq!(
        gaps.take_due(at(t0, 60_000)),
        Some(GapSummary {
            window_ms: 60_000,
            events: 4,
            longest_gap_ms: 6_000,
            since_last_ms: Some(50_000),
        })
    );

    // One event 65 s after the last: the gap began in the first window and ended in this one.
    gaps.note(at(t0, 75_000));
    assert_eq!(gaps.take_due(at(t0, 110_000)), None, "the window restarted at the summary");
    assert_eq!(
        gaps.take_due(at(t0, 120_100)),
        Some(GapSummary {
            window_ms: 60_100,
            events: 1,
            longest_gap_ms: 65_000,
            since_last_ms: Some(45_100),
        })
    );

    // A silent minute: no event, no gap that ended, and the silence so far.
    assert_eq!(
        gaps.take_due(at(t0, 180_100)),
        Some(GapSummary {
            window_ms: 60_000,
            events: 0,
            longest_gap_ms: 0,
            since_last_ms: Some(105_100),
        })
    );
}

#[test]
fn a_session_with_no_event_yet_says_no_age_and_the_line_names_its_fields() {
    let t0 = Instant::now();
    let mut gaps = GapWindow::new(t0);
    let summary = gaps.take_due(at(t0, 60_000)).expect("a minute has run");
    assert_eq!(summary.since_last_ms, None);
    assert_eq!(summary.events, 0);

    let ((), events) = captured(|| summary.say("fxcm"));
    let [line] = events.as_slice() else { panic!("one line: {events:?}") };
    assert_eq!(line.level, tracing::Level::INFO);
    assert_eq!(line.message, "link gap summary");
    assert_eq!(line.field("venue"), Some("fxcm"));
    assert_eq!(line.field("events"), Some("0"));
    assert_eq!(line.field("longest_gap_ms"), Some("0"));
    assert_eq!(line.field("window_ms"), Some("60000"));
    assert_eq!(line.field("since_last_ms"), None, "no event yet: the field is absent");
}
