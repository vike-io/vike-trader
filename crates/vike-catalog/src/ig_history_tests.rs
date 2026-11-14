//! Tests of IG's history vocabulary: the epic grammar and the pinned interval table.

use super::*;

/// **The width the rule gives for a step**: `round(TARGET_ROWS_A_CHUNK x step / day)` clamped to 1 to
/// [`RULE_MAX_CHUNK_DAYS`] days. Not what the lane runs — [`IG_HISTORY_STEPS`] is — but what the pin
/// test recomputes each row from, so a hand-edited width that no longer follows the rule is a failure
/// to explain and not a silent drift.
fn rule_chunk_days(step_ms: i64) -> u32 {
    let rows_per_day = DAY / step_ms.max(1);
    // round(500 / rows_per_day) in integers; a step wider than a day has fewer than one row a day.
    let days = if rows_per_day == 0 {
        i64::from(RULE_MAX_CHUNK_DAYS)
    } else {
        (2 * TARGET_ROWS_A_CHUNK + rows_per_day) / (2 * rows_per_day)
    };
    days.clamp(1, i64::from(RULE_MAX_CHUNK_DAYS)) as u32
}

/// **The chunk width of every interval IG's history serves, pinned.** A chunk's commit key is its grid
/// bounds, so the width is part of the identity of every IG series ever stored: this table may change
/// only with each stored series deleted (`vike-cli data hist rm`) and fetched again, because the same
/// bars at another width are different batches to a store that dedups by commit key and never by row.
/// If this test fails, the answer is almost never to edit the literal.
#[test]
fn the_chunk_width_table_is_pinned() {
    let pinned: [(&str, u32, u32); 12] = [
        ("1m", 40, 1),
        ("2m", 40, 1),
        ("3m", 40, 1),
        ("5m", 360, 2),
        ("10m", 360, 3),
        ("15m", 360, 5),
        ("30m", 360, 10),
        ("1h", 360, 21),
        ("2h", 360, 28),
        ("3h", 360, 28),
        ("4h", 360, 28),
        ("1d", 5_475, 28),
    ];
    let table: Vec<(&str, u32, u32)> =
        IG_HISTORY_STEPS.iter().map(|s| (s.interval, s.depth_days, s.chunk_days)).collect();
    assert_eq!(table, pinned);
}

/// Every row follows the rule that made it, so a width nobody can explain is a visible failure: about
/// 500 calendar-day bars a chunk, at most 28 days, at least one.
#[test]
fn every_width_follows_the_rule_that_made_it() {
    for step in IG_HISTORY_STEPS {
        assert_eq!(step.chunk_days, rule_chunk_days(step.step_ms), "{}", step.interval);
        assert!((1..=RULE_MAX_CHUNK_DAYS).contains(&step.chunk_days), "{}", step.interval);
    }
    // The rule on its own terms: round(500 / bars a day), clamped.
    assert_eq!(rule_chunk_days(5 * 60_000), 2, "1.7 rounds up");
    assert_eq!(rule_chunk_days(10 * 60_000), 3, "3.5 rounds down: 500 / 144 is 3.47");
    assert_eq!(rule_chunk_days(60_000), 1, "never below a day");
    assert_eq!(rule_chunk_days(86_400_000), 28, "never above the cap");
    assert_eq!(rule_chunk_days(7 * 86_400_000), 28, "a step wider than a day has no rows a day");
}

/// The table's step is the label's own arithmetic, and the rows are in ascending width, so a wrong
/// `step_ms` (which the estimator multiplies by) cannot hide behind a right label.
#[test]
fn the_step_is_the_labels_width_and_the_rows_ascend() {
    let mut previous = 0;
    for step in IG_HISTORY_STEPS {
        let (count, unit) = step.interval.split_at(step.interval.len() - 1);
        let count: i64 = count.parse().expect("a count");
        let unit_ms = match unit {
            "m" => 60_000,
            "h" => 3_600_000,
            "d" => 86_400_000,
            other => panic!("{}: unit {other}", step.interval),
        };
        assert_eq!(step.step_ms, count * unit_ms, "{}", step.interval);
        assert!(step.step_ms > previous, "{} is not wider than the row before it", step.interval);
        previous = step.step_ms;
    }
}

/// An interval IG's history does not serve here has no row: the second (the live feed's), the week and
/// the month (no fixed step), and anything not a label.
#[test]
fn an_unserved_interval_has_no_row() {
    for interval in ["1s", "1w", "1mo", "7m", "0m", "", "x", "1H", "60m"] {
        assert_eq!(ig_history_step(interval), None, "{interval:?}");
    }
    assert_eq!(ig_history_step("1h").map(|s| s.chunk_days), Some(21));
}

/// **The epic grammar**: the three epics the 2026-10-23 probe fetched history for are epics; a bare
/// pair, another case, a wrong number of segments, an empty segment, a space and a path are not.
#[test]
fn an_epic_is_five_upper_case_dotted_segments() {
    for epic in ["CS.D.EURUSD.MINI.IP", "IX.D.DAX.IFD.IP", "CC.D.CL.BMU.IP", "KA.D.1COV.DAILY.IP"] {
        assert!(is_ig_epic(epic), "{epic}");
    }
    for not in [
        "",
        "EURUSD",
        "EUR_USD",
        "cs.d.eurusd.mini.ip",
        "CS.D.EURUSD.MINI",
        "CS.D.EURUSD.MINI.IP.X",
        "CS..EURUSD.MINI.IP",
        "CS.D.EURUSD.MINI.IP.",
        "CS.D.EUR USD.MINI.IP",
        "CS.D./.MINI.IP",
        "CS.D.EURUSD.MINI.IP\n",
    ] {
        assert!(!is_ig_epic(not), "{not:?}");
    }
}

/// **Only the two measured intervals are admitted.** The 2026-10-23 probe read HOUR and DAY only, and a
/// chunk width becomes a series' key identity at its first fetch, so the other ten stay in the pinned
/// table (their widths are decided) but are refused until a measured acceptance run admits them. The
/// refusal says why and what to do, and names the interval it refuses.
#[test]
fn only_one_hour_and_one_day_are_admitted_and_the_rest_are_unmeasured() {
    assert_eq!(IG_ADMITTED_INTERVALS, ["1h", "1d"]);
    for step in IG_HISTORY_STEPS {
        let admitted = matches!(step.interval, "1h" | "1d");
        assert_eq!(ig_interval_admitted(step.interval), admitted, "{}", step.interval);
        if !admitted {
            let words = ig_unmeasured_words(step.interval);
            for part in
                [step.interval, "unmeasured", "lean acceptance first", "Nothing was fetched"]
            {
                assert!(words.contains(part), "{part:?} in {words}");
            }
        }
    }
    for not in ["1s", "1w", "1mo", "x", "", "1H"] {
        assert!(!ig_interval_admitted(not), "{not:?}");
    }
}

/// **A window is costed by the whole chunks it touches**, the calendar bound: one row per bar step, a
/// chunk is fetched whole, so a one-second window inside a chunk costs the chunk. Hours are 24 a day, so
/// a 21-day chunk is 504; days are 28 a 28-day chunk; and the week and the share are the measured 10000
/// and a quarter.
#[test]
fn a_window_is_costed_by_the_whole_chunks_it_touches() {
    let hour = ig_history_step("1h").expect("1h");
    let day = ig_history_step("1d").expect("1d");
    let line = 21 * DAY * 900; // a grid line of the 21-day width
    assert_eq!(ig_window_points(hour, line, line), 504, "an instant costs its chunk");
    assert_eq!(ig_window_points(hour, line, line + 21 * DAY - 1), 504);
    assert_eq!(
        ig_window_points(hour, line, line + 21 * DAY),
        1_008,
        "one millisecond into the next"
    );
    assert_eq!(ig_window_points(hour, line + 1, line + 4 * 21 * DAY), 5 * 504, "rounded outward");
    let line_d = 28 * DAY * 700;
    assert_eq!(ig_window_points(day, line_d, line_d + 89 * 28 * DAY - 1), 89 * 28);
    assert_eq!(ig_window_points(day, 5, 4), 0, "an inverted window costs nothing");
    assert_eq!((IG_WEEKLY_POINTS, IG_WINDOW_SHARE_PERCENT), (10_000, 25));
}
