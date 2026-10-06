use super::*;
use chrono::TimeZone;

fn ts(year: i32, month: u32, day: u32) -> i64 {
    Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap().timestamp_millis()
}

// --- period_key() ---

#[test]
fn period_key_daily() {
    assert_eq!(period_key(ts(2024, 3, 15), "daily"), "2024-03-15");
}

#[test]
fn period_key_daily_delegates_to_vike_model_time_equivalently() {
    // The "daily" arm now delegates to `vike_model::time::epoch_ms_to_utc_date` instead of
    // re-deriving Y-M-D via chrono. Prove the two ways of computing a UTC calendar day agree
    // over a range spanning pre-1970 (negative epoch-ms) through post-2024 dates — chrono's
    // `DateTime::from_timestamp_millis` and `vike_model`'s div_euclid day math must never
    // silently diverge on a boundary (midnight, leap day, negative-ms floor direction).
    let probes: &[i64] = &[
        ts(1960, 1, 1),   // pre-1970
        ts(1969, 12, 31), // day before epoch
        ts(1970, 1, 1),   // epoch day
        ts(1970, 1, 2),
        ts(2000, 2, 29), // leap day
        ts(2024, 3, 15),
        ts(2024, 12, 31),
        ts(2024, 3, 15) - 1,     // last ms of the prior UTC day
        ts(2024, 3, 15) + 1,     // first ms after midnight
        -1,                      // 1ms before the epoch
        86_400_000 * 10_000 + 1, // far future
    ];
    for &t in probes {
        let chrono_key = {
            let dt = epoch_ms_to_utc(t);
            format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
        };
        assert_eq!(
            period_key(t, "daily"),
            chrono_key,
            "vike_model::time day-floor must agree with chrono at ts={t}"
        );
    }
}

#[test]
fn period_key_weekly() {
    // 2024-03-15 is ISO week 11 of 2024
    assert_eq!(period_key(ts(2024, 3, 15), "weekly"), "2024-W11");
}

#[test]
fn period_key_monthly() {
    assert_eq!(period_key(ts(2024, 3, 15), "monthly"), "2024-03");
}

#[test]
fn period_key_quarterly_q1_through_q4() {
    assert_eq!(period_key(ts(2024, 1, 15), "quarterly"), "2024-Q1");
    assert_eq!(period_key(ts(2024, 4, 1), "quarterly"), "2024-Q2");
    assert_eq!(period_key(ts(2024, 7, 1), "quarterly"), "2024-Q3");
    assert_eq!(period_key(ts(2024, 10, 1), "quarterly"), "2024-Q4");
}

#[test]
fn period_key_yearly() {
    assert_eq!(period_key(ts(2024, 3, 15), "yearly"), "2024");
}

#[test]
#[should_panic(expected = "period must be")]
fn period_key_unknown_period_panics() {
    period_key(ts(2024, 1, 1), "decadely");
}

// --- periodic_returns() ---

fn three_month_equity() -> (Vec<f64>, Vec<i64>) {
    let eq = vec![10_000.0, 10_200.0, 10_100.0, 10_500.0, 10_300.0, 10_800.0];
    let t = vec![
        ts(2024, 1, 15),
        ts(2024, 1, 25),
        ts(2024, 2, 10),
        ts(2024, 2, 25),
        ts(2024, 3, 10),
        ts(2024, 3, 25),
    ];
    (eq, t)
}

#[test]
fn monthly_three_entries_with_labels() {
    let (eq, t) = three_month_equity();
    let result = periodic_returns(&eq, &t, "monthly");
    let labels: Vec<&str> = result.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["2024-01", "2024-02", "2024-03"]);
}

#[test]
fn monthly_jan_return_correct() {
    let (eq, t) = three_month_equity();
    let result = periodic_returns(&eq, &t, "monthly");
    assert!((result[0].1 - 0.02).abs() < 1e-9);
}

#[test]
fn monthly_feb_return_correct() {
    let (eq, t) = three_month_equity();
    let result = periodic_returns(&eq, &t, "monthly");
    assert!((result[1].1 - (10_500.0 / 10_200.0 - 1.0)).abs() < 1e-9);
}

#[test]
fn periodic_returns_empty() {
    assert_eq!(periodic_returns(&[], &[], "monthly"), vec![]);
}

#[test]
#[should_panic(expected = "equal length")]
fn periodic_returns_length_mismatch_panics() {
    periodic_returns(&[100.0, 110.0], &[ts(2024, 1, 1)], "monthly");
}

// --- monthly_return_matrix() ---

#[test]
fn matrix_years_and_months_present() {
    let (eq, t) = three_month_equity();
    let m = monthly_return_matrix(&eq, &t);
    assert_eq!(m.years, vec![2024]);
    let months_2024 = &m.matrix[&2024];
    assert!(months_2024.contains_key(&1));
    assert!(months_2024.contains_key(&2));
    assert!(months_2024.contains_key(&3));
    assert!(m.annual[&2024] > 0.0);
}

#[test]
fn matrix_multi_year() {
    let eq = vec![10_000.0, 10_500.0, 9_800.0, 10_200.0];
    let t = vec![ts(2023, 12, 15), ts(2023, 12, 29), ts(2024, 1, 10), ts(2024, 1, 25)];
    let m = monthly_return_matrix(&eq, &t);
    assert_eq!(m.years, vec![2023, 2024]);
}

// --- drawdown_table() ---

fn dd_curve() -> (Vec<f64>, Vec<i64>) {
    let eq = vec![100.0, 110.0, 120.0, 100.0, 90.0, 95.0];
    let t: Vec<i64> = (1..=6).map(|d| ts(2024, 1, d)).collect();
    (eq, t)
}

#[test]
fn dd_table_one_drawdown_found_with_depth_and_positions() {
    let (eq, t) = dd_curve();
    let table = drawdown_table(&eq, &t, 5);
    assert_eq!(table.len(), 1);
    assert!((table[0].depth - 0.25).abs() < 1e-9); // (120-90)/120
    assert_eq!(table[0].peak_ts, t[2]);
    assert_eq!(table[0].trough_ts, t[4]);
    assert_eq!(table[0].recovery_ts, None);
    assert_eq!(table[0].recovery, None);
    assert_eq!(table[0].length, 2); // trough idx 4 - peak idx 2
}

#[test]
fn dd_table_with_recovery() {
    let eq = vec![100.0, 120.0, 90.0, 130.0];
    let t: Vec<i64> = (1..=4).map(|d| ts(2024, 1, d)).collect();
    let table = drawdown_table(&eq, &t, 5);
    assert_eq!(table.len(), 1);
    assert_eq!(table[0].recovery_ts, Some(t[3]));
    assert_eq!(table[0].recovery, Some(1));
}

#[test]
fn dd_table_monotone_up_is_empty() {
    let eq = vec![100.0, 110.0, 120.0, 130.0];
    let t: Vec<i64> = (1..=4).map(|d| ts(2024, 1, d)).collect();
    assert_eq!(drawdown_table(&eq, &t, 5), vec![]);
}

#[test]
fn dd_table_empty_curve() {
    assert_eq!(drawdown_table(&[], &[], 5), vec![]);
}

#[test]
fn dd_table_top_n_limits_and_sorts_descending() {
    let eq = vec![
        100.0, 90.0, 100.0, // 10% dd, recovers
        110.0, 88.0, 110.0, // 20% dd, recovers
        120.0, 84.0, 120.0, // 30% dd, recovers
    ];
    let t: Vec<i64> = (1..=9).map(|d| ts(2024, 1, d)).collect();
    let table = drawdown_table(&eq, &t, 2);
    assert_eq!(table.len(), 2);
    assert!(table[0].depth >= table[1].depth);
}

#[test]
#[should_panic(expected = "equal length")]
fn dd_table_length_mismatch_panics() {
    drawdown_table(&[100.0, 90.0], &[ts(2024, 1, 1)], 5);
}

// --- PERIODS / parse_period() ---

/// The roster and [`period_key`]'s `match` are one set. A bucket in the array that the match
/// rejects would make [`parse_period`] hand a caller a period that then PANICS one frame
/// later — the exact hazard `parse_period` exists to remove, reintroduced from the other side.
#[test]
fn every_declared_period_is_one_period_key_accepts() {
    for p in PERIODS {
        let key = period_key(ts(2024, 3, 15), p);
        assert!(!key.is_empty(), "{p} produced no label");
    }
}

/// …and the other direction, which is the one the error message used to get wrong: the panic
/// text names the roster it was read from, so it can never advertise fewer periods than work.
#[test]
fn the_panic_message_names_every_declared_period() {
    let err = std::panic::catch_unwind(|| period_key(ts(2024, 1, 1), "decadely"))
        .expect_err("an unknown period panics");
    let msg =
        err.downcast_ref::<String>().cloned().unwrap_or_else(|| "<non-string panic>".to_string());
    for p in PERIODS {
        assert!(msg.contains(p), "the refusal omits {p}: {msg}");
    }
}

#[test]
fn parse_period_accepts_the_canonical_spellings() {
    for p in PERIODS {
        assert_eq!(parse_period(p), Ok(*p));
    }
}

/// Case is normalized to the canonical spelling, so an operator's casing reaches nothing
/// downstream and no second spelling of a period exists.
#[test]
fn parse_period_normalizes_case_to_the_canonical_spelling() {
    assert_eq!(parse_period("Monthly"), Ok("monthly"));
    assert_eq!(parse_period("YEARLY"), Ok("yearly"));
}

/// ⚠ The whole point: an operator's typo is a REFUSAL, not a panic — and the refusal names
/// the roster, because "unknown period" alone sends the reader to the source.
#[test]
fn parse_period_refuses_an_unknown_spelling_without_panicking() {
    let err = parse_period("fortnight").expect_err("not a bucket");
    assert!(err.contains("fortnight"), "it names what was typed: {err}");
    for p in PERIODS {
        assert!(err.contains(p), "…and every period that would have worked: {err}");
    }
}

/// No abbreviations, deliberately — `vike-cli`'s `report` verb publishes `day|month` and maps
/// them on its own side. An alias accepted here would make "the name of a period" a question
/// with two answers.
#[test]
fn parse_period_accepts_no_aliases() {
    for alias in ["day", "month", "mo", "1M", "d"] {
        assert!(parse_period(alias).is_err(), "{alias} must not resolve");
    }
}

// --- periodic_returns_text() ---

#[test]
fn periodic_returns_text_renders_a_row_per_bucket_with_percent_scaling() {
    let (eq, t) = three_month_equity();
    let rows = periodic_returns(&eq, &t, "monthly");
    let text = periodic_returns_text("monthly", &rows);
    assert!(text.starts_with("monthly returns — 3 periods\n"), "{text}");
    assert!(text.contains("period"), "the header is present: {text}");
    // ⚠ 2% renders as `2.0000%`, NOT as `0.0200` — the scaling is `MetricUnit::Percent`'s and
    // a renderer that dropped it would publish two basis points.
    assert!(text.contains("2.0000%"), "January's 2% return, scaled: {text}");
    for label in ["2024-01", "2024-02", "2024-03"] {
        assert!(text.contains(label), "missing {label}: {text}");
    }
}

/// An empty curve renders the FACT, never blank output: an operator who asked for a breakdown
/// and got nothing reads it as a broken command.
#[test]
fn periodic_returns_text_says_why_there_are_no_rows() {
    let text = periodic_returns_text("monthly", &[]);
    assert!(text.contains("no monthly periods"), "{text}");
    assert!(text.contains("no samples"), "…and the reason: {text}");
}

// --- drawdown_table_text() ---

#[test]
fn drawdown_table_text_renders_the_episode_with_utc_instants_and_a_scaled_depth() {
    let (eq, t) = dd_curve();
    let text = drawdown_table_text(&drawdown_table(&eq, &t, 5));
    assert!(text.starts_with("top 1 drawdown episodes, deepest first\n"), "{text}");
    assert!(text.contains("25.0000%"), "(120-90)/120 scaled to a percent: {text}");
    // The instants come from `vike_model::time`, the workspace's one UTC law — second
    // resolution, `Z`-suffixed.
    assert!(text.contains("2024-01-03T00:00:00Z"), "the peak instant: {text}");
    assert!(text.contains("2024-01-05T00:00:00Z"), "the trough instant: {text}");
}

/// ⚠ The row that matters most. `recovery_ts: None` means the curve ENDED under water — an
/// OPEN drawdown — and a blank cell would read as "no data". Both cells say so in words, and
/// `n/a` is a different claim from `0`.
#[test]
fn an_unrecovered_episode_is_spelled_out_rather_than_blanked() {
    let (eq, t) = dd_curve();
    let text = drawdown_table_text(&drawdown_table(&eq, &t, 5));
    assert!(text.contains("still open"), "{text}");
    assert!(text.contains("n/a"), "…and its bar count is not a zero: {text}");
}

#[test]
fn a_recovered_episode_prints_its_recovery_instant_and_bar_count() {
    let eq = vec![100.0, 120.0, 90.0, 130.0];
    let t: Vec<i64> = (1..=4).map(|d| ts(2024, 1, d)).collect();
    let text = drawdown_table_text(&drawdown_table(&eq, &t, 5));
    assert!(text.contains("2024-01-04T00:00:00Z"), "the recovery instant: {text}");
    assert!(!text.contains("still open"), "a recovered episode is not open: {text}");
}

/// A curve that never fell is a DIFFERENT fact from a curve that is not there, so it gets its
/// own sentence rather than the empty-curve one.
#[test]
fn drawdown_table_text_says_why_there_are_no_episodes() {
    let text = drawdown_table_text(&[]);
    assert!(text.contains("never closed below a prior peak"), "{text}");
}

/// The renderer re-sorts nothing: the rows it prints are the rows `drawdown_table` handed it,
/// in that order, so a table and a serialized document over one fold cannot disagree about
/// which drawdown was the worst.
#[test]
fn drawdown_table_text_preserves_the_folds_own_order() {
    let eq = vec![
        100.0, 90.0, 100.0, // 10% dd, recovers
        110.0, 88.0, 110.0, // 20% dd, recovers
        120.0, 84.0, 120.0, // 30% dd, recovers
    ];
    let t: Vec<i64> = (1..=9).map(|d| ts(2024, 1, d)).collect();
    let episodes = drawdown_table(&eq, &t, 3);
    let text = drawdown_table_text(&episodes);
    let body: Vec<&str> = text.lines().skip(2).take(episodes.len()).collect();
    for (row, ep) in body.iter().zip(&episodes) {
        assert!(
            row.contains(&MetricUnit::Percent.render(ep.depth)),
            "row `{row}` is not episode depth {}",
            ep.depth
        );
    }
}

/// `DrawdownEpisode` serializes under the PRODUCER's field names, so a machine consumer needs
/// no second document shape beside this one.
#[test]
fn a_drawdown_episode_serializes_under_its_own_field_names() {
    let (eq, t) = dd_curve();
    let episodes = drawdown_table(&eq, &t, 1);
    let json = serde_json::to_string(&episodes[0]).expect("serializable");
    for key in ["depth", "peak_ts", "trough_ts", "recovery_ts", "length", "recovery"] {
        assert!(json.contains(key), "missing {key}: {json}");
    }
}
