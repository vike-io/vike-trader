use super::*;

/// 2026-01-01T00:00:00Z — an exact UTC midnight, so every date in these tests reads as the day
/// its offset names.
const JAN1: i64 = 1_767_225_600_000;

fn span(kind: &str, name: &str, first_day: i64, last_day: i64) -> SeriesSpan {
    SeriesSpan {
        kind: kind.to_string(),
        venue: "binance".to_string(),
        name: name.to_string(),
        grouped: false,
        first_ts: JAN1 + first_day * MS_PER_DAY,
        last_ts: JAN1 + last_day * MS_PER_DAY,
        rows: 100,
    }
}

fn day(n: i64) -> i64 {
    JAN1 + n * MS_PER_DAY
}

#[test]
fn the_fold_joins_kinds_and_takes_the_widest_span() {
    let members = fold_members(&[
        span("trade", "BTCUSDT", 0, 10),
        span("bar", "BTCUSDT", 3, 20),
        span("quote", "BTCUSDT", 5, 8),
    ]);
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].kinds, vec!["bar", "quote", "trade"], "kind-sorted, deterministic");
    assert_eq!(members[0].first_ts, Some(day(0)), "the earliest of any kind");
    assert_eq!(members[0].last_ts, Some(day(20)), "the latest of any kind");
    assert_eq!(members[0].rows, 300);
}

/// ⚠ A zero-row series must not pull an instrument's start back to the epoch. The store folds
/// an empty series to an all-zero coverage, so admitting one would make every affected
/// instrument read as having begun on 1970-01-01.
#[test]
fn an_empty_series_does_not_drag_the_span_to_the_epoch() {
    let mut empty = span("quote", "BTCUSDT", 0, 0);
    empty.first_ts = 0;
    empty.last_ts = 0;
    empty.rows = 0;
    let members = fold_members(&[span("bar", "BTCUSDT", 5, 9), empty]);
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].first_ts, Some(day(5)));
    assert_eq!(members[0].kinds, vec!["bar", "quote"], "the empty kind is still NAMED");
}

/// An instrument whose every series is empty keeps no endpoints and is `absent` — not a member
/// with a 1970 span, and not silently dropped either.
#[test]
fn an_instrument_with_only_empty_series_is_absent() {
    let mut empty = span("bar", "GHOSTUSDT", 0, 0);
    empty.first_ts = 0;
    empty.last_ts = 0;
    empty.rows = 0;
    let members = fold_members(&[empty]);
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].first_ts, None);
    let status = classify(&members[0], day(0), day(30), Some(day(30)));
    assert!(!status.present);
    assert_eq!(status.as_str(), "absent");
}

#[test]
fn grouped_and_per_symbol_instruments_do_not_merge() {
    let mut grouped = span("trade", "fam", 0, 5);
    grouped.grouped = true;
    let members = fold_members(&[span("trade", "fam", 0, 5), grouped]);
    assert_eq!(members.len(), 2, "{members:#?}");
}

/// An instrument recorded across the whole window is the one a naive listing also finds — the
/// baseline this verb measures the other answers against.
#[test]
fn a_tape_spanning_the_window_is_whole() {
    let members = fold_members(&[span("bar", "BTCUSDT", 0, 100)]);
    let status = classify(&members[0], day(10), day(20), Some(day(100)));
    assert!(status.covers_window);
    assert_eq!(status.as_str(), "whole");
    assert_eq!(status.gone_days, None);
}

/// **The survivorship case this verb exists for.** The tape stops on day 20 while the store
/// records to day 100: a universe taken from the store today omits this instrument entirely,
/// and a backtest over days 0..60 then samples only what survived.
#[test]
fn a_tape_that_stops_while_the_store_goes_on_is_the_survivorship_signal() {
    let members = fold_members(&[span("bar", "DEADUSDT", 0, 20)]);
    let status = classify(&members[0], day(0), day(60), Some(day(100)));
    assert!(status.present, "it WAS in the universe for part of the window");
    assert!(status.left_inside);
    assert!(!status.covers_window);
    assert_eq!(status.as_str(), "left");
    assert_eq!(status.gone_days, Some(80), "the size of the signal, in days behind the edge");
}

/// ⚠ **The tautology this check has to avoid.** Every tape stops somewhere, so "stopped before
/// the window's end" alone would flag an instrument that simply runs to the store's own edge —
/// i.e. every instrument in a store whose window reaches past its newest row.
#[test]
fn a_tape_running_to_the_stores_own_edge_never_reads_as_having_left() {
    let members = fold_members(&[span("bar", "BTCUSDT", 0, 100)]);
    // The window extends beyond anything the store holds; the tape still reaches the edge.
    let status = classify(&members[0], day(0), day(365), Some(day(100)));
    assert!(!status.left_inside, "{status:?}");
    assert_eq!(status.gone_days, None);
    // …and it lands on the fifth verdict rather than being squeezed into one of the four:
    // the WINDOW outran the store, which is not the same fact as an instrument stopping.
    assert_eq!(status.as_str(), "partial", "{status:?}");
}

#[test]
fn a_tape_beginning_inside_the_window_is_reported_as_such() {
    let members = fold_members(&[span("bar", "NEWUSDT", 30, 100)]);
    let status = classify(&members[0], day(0), day(60), Some(day(100)));
    assert!(status.listed_inside);
    assert!(!status.left_inside);
    assert_eq!(status.as_str(), "began");
}

/// Both at once — the row that an enum would have had to pick one half of.
#[test]
fn a_tape_that_began_and_stopped_inside_the_window_says_both() {
    let members = fold_members(&[span("bar", "BRIEFUSDT", 10, 20)]);
    let status = classify(&members[0], day(0), day(60), Some(day(100)));
    assert!(status.listed_inside && status.left_inside);
    assert_eq!(status.as_str(), "began+left");
}

/// A tape entirely outside the window is absent from it, even though the store holds it — which
/// is the whole point of asking the question as of a date.
#[test]
fn a_tape_outside_the_window_is_absent_from_it() {
    let members = fold_members(&[span("bar", "OLDUSDT", 0, 5)]);
    let status = classify(&members[0], day(50), day(60), Some(day(100)));
    assert!(!status.present);
    assert_eq!(status.as_str(), "absent");
}

/// The bounds are INCLUSIVE at both ends: a tape covering exactly the window's two endpoints
/// spans it, and an exclusive end would have dropped the last day of every window typed.
#[test]
fn the_window_bounds_are_inclusive() {
    let members = fold_members(&[span("bar", "BTCUSDT", 10, 20)]);
    let status = classify(&members[0], day(10), day(20), Some(day(20)));
    assert!(status.covers_window, "{status:?}");
    assert_eq!(status.as_str(), "whole");
}

// ── the frame and the window resolution ─────────────────────────────────────────────────────

#[test]
fn the_frame_is_the_stores_own_span_over_every_reported_series() {
    let store = store_span(&[span("bar", "A", 5, 40), span("trade", "B", 0, 100)]);
    assert_eq!(store.first_ts, Some(day(0)));
    assert_eq!(store.last_ts, Some(day(100)));
}

#[test]
fn an_unbounded_window_takes_the_stores_endpoints() {
    let store = StoreSpan { first_ts: Some(day(0)), last_ts: Some(day(100)) };
    assert_eq!(
        resolve_window(MembershipWindow::default(), store),
        Some((day(0), day(100))),
        "a bare `data universe` asks about everything the store spans"
    );
    assert_eq!(
        resolve_window(MembershipWindow { from: Some(day(7)), to: None }, store),
        Some((day(7), day(100))),
        "one bound alone is a well-formed question; the other takes the store's"
    );
}

/// A store that reported nothing gives no frame, so there is no verdict to render — answered
/// as `None` rather than as a window over zero.
#[test]
fn an_empty_store_yields_no_window_at_all() {
    assert_eq!(resolve_window(MembershipWindow::default(), StoreSpan::default()), None);
    // …and an operator's own bounds do not manufacture one either: the frame's far side is
    // still missing, and inventing `now()` for it is exactly what the module doc refuses.
    let asked = MembershipWindow { from: Some(day(0)), to: None };
    assert_eq!(resolve_window(asked, StoreSpan::default()), None);
}

// ── the renderings ──────────────────────────────────────────────────────────────────────────

fn classified(spans: &[SeriesSpan], from: i64, to: i64) -> Vec<(Member, Membership)> {
    let store = store_span(spans);
    fold_members(spans)
        .into_iter()
        .map(|m| {
            let status = classify(&m, from, to, store.last_ts);
            (m, status)
        })
        .collect()
}

/// The table states the WINDOW above itself: half these verdicts are relative to a bound the
/// operator may never have typed, so a reader who cannot see it cannot check a STATUS.
#[test]
fn the_rendering_states_the_window_it_judged_against() {
    let rows = classified(&[span("bar", "BTCUSDT", 0, 100)], day(0), day(100));
    let out = lines(&rows, 1, false, Some((day(0), day(100))));
    assert!(out[0].contains("membership over"), "{}", out[0]);
    assert!(out[0].contains("2026-01-01"), "{}", out[0]);
    assert!(out[0].contains("inclusive"), "{}", out[0]);
}

/// The summary counts the three answers apart, and the survivorship warning fires ONLY when
/// something actually stopped — a paragraph printed every run is a paragraph nobody reads.
#[test]
fn the_survivorship_warning_fires_only_when_something_stopped() {
    let healthy = classified(&[span("bar", "BTCUSDT", 0, 100)], day(0), day(100));
    let out = lines(&healthy, 1, false, Some((day(0), day(100))));
    let text = out.join("\n");
    assert!(text.contains("1 spanned the whole window"), "{text}");
    assert!(text.contains("0 stopped inside it"), "{text}");
    assert!(!text.contains("survived"), "no warning when nothing stopped: {text}");

    let mixed = classified(
        &[span("bar", "BTCUSDT", 0, 100), span("bar", "DEADUSDT", 0, 20)],
        day(0),
        day(100),
    );
    let out = lines(&mixed, 2, false, Some((day(0), day(100))));
    let text = out.join("\n");
    assert!(text.contains("1 stopped inside it"), "{text}");
    assert!(text.contains("survived"), "the warning names the defence: {text}");
    assert!(text.contains("data list"), "…and what NOT to select on: {text}");
}

/// An instrument that recorded nothing renders `-` for both endpoints rather than a
/// synthesized 1970 date.
#[test]
fn an_absent_instrument_renders_dashes_not_the_epoch() {
    let mut empty = span("bar", "GHOSTUSDT", 0, 0);
    empty.first_ts = 0;
    empty.last_ts = 0;
    empty.rows = 0;
    let rows = classified(&[empty], day(0), day(100));
    let out = lines(&rows, 1, false, None);
    let text = out.join("\n");
    assert!(!text.contains("1970"), "{text}");
    assert!(text.contains("absent"), "{text}");
}

#[test]
fn nothing_matched_is_reported_as_a_filter_outcome() {
    let out = lines(&[], 9, true, None);
    assert_eq!(out.len(), 1);
    assert!(out[0].contains("9 reported"), "{}", out[0]);
}

/// The document carries the measured timestamps AND the four booleans beside the status string,
/// so a consumer filtering for "everything that stopped" never has to substring-match a
/// composite verdict.
///
/// ⚠ The long-running BTCUSDT span is not decoration — it is what makes the FRAME extend past
/// BRIEFUSDT's last row. With the short tape alone the store's newest row IS that tape's last
/// row, [`classify`]'s second condition holds, and the verdict is `began` rather than
/// `began+left`. That is the tautology guard doing its job, and a fixture that omitted the
/// second span would have been asserting the wrong thing about the right code.
#[test]
fn the_document_carries_the_booleans_beside_the_status() {
    let spans = [span("bar", "BRIEFUSDT", 10, 20), span("bar", "BTCUSDT", 0, 100)];
    let rows = classified(&spans, day(0), day(60));
    let doc = json_members(&rows);
    assert_eq!(doc.len(), 2, "ordered by (venue, name): BRIEFUSDT sorts before BTCUSDT");
    assert_eq!(doc[0]["name"], serde_json::json!("BRIEFUSDT"));
    assert_eq!(doc[0]["status"], serde_json::json!("began+left"));
    assert_eq!(doc[0]["stopped_inside"], serde_json::json!(true));
    assert_eq!(doc[0]["began_inside"], serde_json::json!(true));
    assert_eq!(doc[0]["last_recorded_date"], serde_json::json!("2026-01-21"));
    assert_eq!(doc[0]["gone_days"], serde_json::json!(80));
}
