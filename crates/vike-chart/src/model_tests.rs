use super::testutil::{bar, wire_bar_at};
use super::*;

/// A [`Bar`] at bar-index `i` with an explicit open-time `ot` (ms) — for tz-mark tests that
/// need specific wall-clock instants rather than [`bar`]'s fixed one-minute-epoch formula.
fn bar_at(i: usize, ot: i64) -> Bar {
    Bar { t: i as f64, ot, o: 1.0, h: 2.0, l: 1.0, c: 1.5, v: 1.0 }
}

#[test]
fn hour_marks_one_per_hour_for_minute_bars() {
    // 180 one-minute bars = 3 full hours → exactly 3 hourly marks, 60 apart,
    // for ANY fixed-offset local timezone (minute-of-hour repeats mod 60).
    let bars: Vec<Bar> = (0..180).map(|i| bar(i, 1.0, 2.0)).collect();
    let marks = hour_mark_indices(&bars, DisplayTz::Local);
    assert_eq!(marks.len(), 3, "3 hours of minute bars → 3 marks");
    assert_eq!(marks[1] - marks[0], 60.0);
    assert_eq!(marks[2] - marks[1], 60.0);
    for m in marks {
        assert!((0.0..180.0).contains(&m));
    }
}

/// `hour_mark_indices` re-evaluates minute==0 in the REQUESTED tz, not always local/UTC —
/// the same instants land on different bar indices depending on `tz`.
#[test]
fn hour_marks_follow_display_tz() {
    // bars every 15 min from 11:30 UTC: ots at 11:30, 11:45, 12:00, 12:15 UTC.
    let base =
        chrono::DateTime::parse_from_rfc3339("2026-07-11T11:30:00Z").unwrap().timestamp_millis();
    let bars: Vec<Bar> = (0..4).map(|i| bar_at(i, base + i as i64 * 900_000)).collect();
    // UTC: only 12:00 (index 2) has minute==0.
    assert_eq!(hour_mark_indices(&bars, DisplayTz::Utc), vec![2.0]);
    // Kolkata (+5:30): 11:30 UTC == 17:00 local → index 0 (and 12:30 would be next; not
    // present).
    assert_eq!(hour_mark_indices(&bars, DisplayTz::Named(chrono_tz::Asia::Kolkata)), vec![0.0]);
}

#[test]
fn day_marks_at_local_midnight_and_move_with_tz() {
    // Hourly bars 21:00 UTC Jul-10 .. 03:00 UTC Jul-11 (7 bars).
    let base =
        chrono::DateTime::parse_from_rfc3339("2026-07-10T21:00:00Z").unwrap().timestamp_millis();
    let bars: Vec<Bar> = (0..7).map(|i| bar_at(i, base + i as i64 * 3_600_000)).collect();
    // UTC: date flips at 00:00 UTC == index 3. Never index 0.
    assert_eq!(day_mark_indices(&bars, DisplayTz::Utc), vec![3.0]);
    // Tokyo (+9): 21:00 UTC Jul-10 == 06:00 Jul-11 local; prior local midnight is before bar 0
    // → no flip until 15:00 UTC — not in range. So NO day mark in Tokyo for this window.
    assert_eq!(
        day_mark_indices(&bars, DisplayTz::Named(chrono_tz::Asia::Tokyo)),
        Vec::<f64>::new()
    );
    // New York (-4, DST): midnight local == 04:00 UTC — not in range either (all 7 bars land
    // on NY-local Jul-10).
    assert_eq!(
        day_mark_indices(&bars, DisplayTz::Named(chrono_tz::America::New_York)),
        Vec::<f64>::new()
    );
}

#[test]
fn day_marks_append_fastpath_equals_full_recompute() {
    // 3 days of 4h bars, synced in two chunks — cache after incremental == full recompute.
    let base =
        chrono::DateTime::parse_from_rfc3339("2026-07-09T02:00:00Z").unwrap().timestamp_millis();
    let wire: Vec<vike_marketdata::Bar> =
        (0..18).map(|i| wire_bar_at(base + i as i64 * 4 * 3_600_000)).collect();
    let mut incremental = ChartState::default();
    incremental.set_tz(DisplayTz::Utc);
    incremental.sync(&std::sync::Arc::new(wire[..10].to_vec()), None);
    incremental.sync(&std::sync::Arc::new(wire.clone()), None); // append path
    let mut full = ChartState::default();
    full.set_tz(DisplayTz::Utc);
    full.sync(&std::sync::Arc::new(wire.clone()), None);
    assert_eq!(incremental.day_marks, full.day_marks);
    assert!(!full.day_marks.is_empty());
}

/// Carried Minor from A2's review: a `set_tz` immediately followed, in the SAME `sync` call,
/// by newly-appended closed bars must NOT take the append fast-path under the OLD tz —
/// `refresh_caches`'s `key.3 == prev_key.3` guard must force the full recompute so both
/// `hour_marks` and `day_marks` end up computed entirely under the NEW tz, matching a fresh
/// `ChartState` synced directly to the same final series under that tz. Uses 15-min bars
/// spanning a Kolkata (+5:30) local-midnight crossing: Utc and Kolkata disagree on which of
/// the first 5 bars land on the hour (index 2 vs. index 4), so a stale Utc-computed prefix
/// surviving into a Kolkata-keyed result is directly observable via `hour_marks`.
#[test]
fn tz_change_plus_append_forces_full_recompute() {
    let base =
        chrono::DateTime::parse_from_rfc3339("2026-07-11T11:30:00Z").unwrap().timestamp_millis();
    let wire: Vec<vike_marketdata::Bar> =
        (0..41).map(|i| wire_bar_at(base + i as i64 * 900_000)).collect();

    let mut cs = ChartState::default();
    cs.set_tz(DisplayTz::Utc);
    cs.sync(&std::sync::Arc::new(wire[..5].to_vec()), None);

    // ONE step: change tz AND append the rest of the series.
    cs.set_tz(DisplayTz::Named(chrono_tz::Asia::Kolkata));
    cs.sync(&std::sync::Arc::new(wire.clone()), None);

    let mut fresh = ChartState::default();
    fresh.set_tz(DisplayTz::Named(chrono_tz::Asia::Kolkata));
    fresh.sync(&std::sync::Arc::new(wire.clone()), None);

    assert_eq!(cs.hour_marks, fresh.hour_marks, "hour_marks must match a fresh Kolkata recompute");
    assert_eq!(cs.day_marks, fresh.day_marks, "day_marks must match a fresh Kolkata recompute");
    assert!(!fresh.day_marks.is_empty(), "sanity: window must cross a real Kolkata midnight");
}

#[test]
fn grain_tiers_and_median() {
    let mk = |secs: i64, n: i64| -> Vec<Bar> {
        (0..n).map(|i| bar_at(i as usize, i * secs * 1000)).collect()
    };
    assert_eq!(median_bar_secs(&mk(1, 100)), Some(1.0));
    assert_eq!(median_bar_secs(&mk(60, 100)), Some(60.0));
    assert_eq!(median_bar_secs(&mk(3600, 50)), Some(3600.0));
    assert_eq!(median_bar_secs(&[]), None);
    assert_eq!(median_bar_secs(&mk(60, 1)), None); // one bar → no delta
    assert_eq!(grain(Some(1.0)), TimeGrain::Sub60);
    assert_eq!(grain(Some(60.0)), TimeGrain::Minute);
    assert_eq!(grain(Some(3600.0)), TimeGrain::HourPlus);
    assert_eq!(grain(None), TimeGrain::Minute);
}

/// Carried Minor from A4's review: `median_bar_secs`'s `filter(|d| *d > 0)` branch — a
/// zero delta (two bars sharing one `ot`) leaves no positive deltas at all, and a negative
/// (out-of-order) delta mixed among positive ones is dropped rather than counted.
#[test]
fn median_bar_secs_filters_non_positive_deltas() {
    // two bars sharing the same ot → delta 0 → filtered out → no deltas left → None.
    let same_ot = vec![bar_at(0, 1_000), bar_at(1, 1_000)];
    assert_eq!(median_bar_secs(&same_ot), None);

    // deltas across the tail: +60_000, -30_000 (out-of-order), +60_000 — only the two
    // positive deltas are counted, so the median is 60_000ms == 60.0s, not skewed by the
    // negative one.
    let mixed = vec![bar_at(0, 0), bar_at(1, 60_000), bar_at(2, 30_000), bar_at(3, 90_000)];
    assert_eq!(median_bar_secs(&mixed), Some(60.0));
}

#[test]
fn mark_step_is_median_gap() {
    assert_eq!(mark_step(&[10.0, 70.0, 130.0, 190.0], 60.0), 60.0);
    assert_eq!(mark_step(&[5.0], 60.0), 60.0); // <2 marks → fallback
    assert_eq!(mark_step(&[], 1440.0), 1440.0);
    // irregular (tick bars): 3,5,100 gaps → median 5
    assert_eq!(mark_step(&[0.0, 3.0, 8.0, 108.0], 60.0), 5.0);
}

#[test]
fn refresh_caches_folds_closed_only_and_invalidates_on_close() {
    let mut cs = ChartState {
        bars: (0..10).map(|i| bar(i, 10.0 - i as f64 * 0.1, 20.0 + i as f64 * 0.1)).collect(),
        closed_len: 9, // last bar is forming
        ..Default::default()
    };
    cs.refresh_caches();
    let (lo, hi) = cs.y_ext.expect("closed bars present");
    assert_eq!(lo, 10.0 - 8.0 * 0.1); // min low over bars 0..=8 (forming excluded)
    assert_eq!(hi, 20.0 + 8.0 * 0.1);

    // forming-bar mutation → same key → no recompute (y_ext unchanged even
    // though the forming bar now has a wilder range)
    cs.bars[9].l = 0.0;
    cs.bars[9].h = 99.0;
    cs.refresh_caches();
    assert_eq!(cs.y_ext, Some((lo, hi)));

    // bar close (closed_len grows) → cache invalidates and picks up bar 9
    cs.closed_len = 10;
    cs.refresh_caches();
    assert_eq!(cs.y_ext, Some((0.0, 99.0)));
}

#[test]
fn refresh_caches_empty_series() {
    let mut cs = ChartState::default();
    cs.refresh_caches();
    assert_eq!(cs.y_ext, None);
    assert!(cs.hour_marks.is_empty());
}

/// Oracle: a fresh full min/max fold + `hour_mark_indices`/`day_mark_indices` (in `tz`) +
/// `median_bar_secs`/`mark_step` (task A4) over the entire closed prefix — exactly what
/// today's `refresh_caches` computes on every key change. `refresh_caches`'s incremental
/// output must equal this byte-for-byte after every call, no matter which internal branch
/// (append fast-path vs. full recompute) it took.
#[expect(clippy::type_complexity)]
fn full_cache_recompute(
    closed: &[Bar],
    tz: DisplayTz,
) -> (Option<(f64, f64)>, Vec<f64>, Vec<f64>, Option<f64>, f64, f64) {
    let y_ext = if closed.is_empty() {
        None
    } else {
        let lo = closed.iter().map(|b| b.l).fold(f64::INFINITY, f64::min);
        let hi = closed.iter().map(|b| b.h).fold(f64::NEG_INFINITY, f64::max);
        Some((lo, hi))
    };
    let hour_marks = hour_mark_indices(closed, tz);
    let day_marks = day_mark_indices(closed, tz);
    let median_secs = median_bar_secs(closed);
    let hour_step = mark_step(&hour_marks, 60.0);
    let day_step = mark_step(&day_marks, 1440.0);
    (y_ext, hour_marks, day_marks, median_secs, hour_step, day_step)
}

/// Assert `cs.y_ext`/`cs.hour_marks`/`cs.day_marks`/`cs.median_secs()`/`cs.hour_step()`/
/// `cs.day_step()` (as left by the last `refresh_caches` call) equal a fresh full recompute
/// over `closed` at `cs`'s current tz — the equivalence gate for every step of a tick
/// sequence, now locking the whole A4 cache family (not just `y_ext`/`hour_marks`) to the
/// same oracle.
fn assert_refresh_matches_full(cs: &ChartState, closed: &[Bar], label: &str) {
    let (y_ext, hour_marks, day_marks, median_secs, hour_step, day_step) =
        full_cache_recompute(closed, cs.tz());
    assert_eq!(cs.y_ext, y_ext, "{label}: y_ext diverged from full recompute");
    assert_eq!(cs.hour_marks, hour_marks, "{label}: hour_marks diverged from full recompute");
    assert_eq!(cs.day_marks, day_marks, "{label}: day_marks diverged from full recompute");
    assert_eq!(cs.median_secs(), median_secs, "{label}: median_secs diverged from full recompute");
    assert_eq!(cs.hour_step(), hour_step, "{label}: hour_step diverged from full recompute");
    assert_eq!(cs.day_step(), day_step, "{label}: day_step diverged from full recompute");
}

/// `refresh_caches` must be byte-identical to a full recompute at every step of a
/// realistic tick sequence — seed 120 closed + forming, three forming-only ticks, a
/// single bar close, a forming tick after that close, five bars closing at once, then
/// enough further closes to push the closed prefix from 120 to 190 (a ≥60-bar append
/// span, which — since hour marks recur every 60 one-minute bars for ANY fixed-offset
/// local timezone — is guaranteed to cross a real hour boundary regardless of the
/// test machine's timezone), and finally a reload onto a shorter, differently-epoched
/// series (symbol/timeframe swap). This exercises the append fast-path's `+ prev_len`
/// hour-mark offset against a genuine mark in the appended region, not just an empty
/// slice — a wrong offset would silently pass if no real mark ever landed there.
#[test]
fn refresh_caches_incremental_matches_full() {
    let mut cs = ChartState::default();

    // seed: 120 closed bars (indices 0..119) + forming (index 120)
    let mut series: Vec<Bar> =
        (0..=120).map(|i| bar(i, 10.0 - i as f64 * 0.01, 20.0 + i as f64 * 0.01)).collect();
    cs.bars = series.clone();
    cs.closed_len = 120;
    cs.refresh_caches();
    assert_refresh_matches_full(&cs, &series[..120], "seed");

    // forming-tick x3: mutate the forming bar (index 120) only — closed prefix stable
    for k in 0..3 {
        series[120].h = 30.0 + k as f64;
        series[120].l = 5.0 - k as f64;
        cs.bars = series.clone();
        cs.refresh_caches();
        assert_refresh_matches_full(&cs, &series[..120], "forming-tick");
    }

    // close 1: bar 120 closes, new forming bar 121
    series.push(bar(121, 5.0, 25.0));
    cs.bars = series.clone();
    cs.closed_len = 121;
    cs.refresh_caches();
    assert_refresh_matches_full(&cs, &series[..121], "close 1");

    // forming-tick after close 1
    series[121].h = 40.0;
    cs.bars = series.clone();
    cs.refresh_caches();
    assert_refresh_matches_full(&cs, &series[..121], "forming-tick after close 1");

    // close 5 at once: bars 121..=125 close, new forming bar 126
    for i in 122..=126 {
        series.push(bar(i, 1.0 + i as f64 * 0.02, 50.0 - i as f64 * 0.02));
    }
    cs.bars = series.clone();
    cs.closed_len = 126;
    cs.refresh_caches();
    assert_refresh_matches_full(&cs, &series[..126], "close 5");

    // further closes pushing the closed prefix 126 -> 190: combined with the closes
    // above, the appended span from the seed boundary (120) to 190 is 70 bars — >60,
    // so it's guaranteed to contain a genuine hour-mark boundary.
    for i in 127..=190 {
        series.push(bar(i, 2.0, 60.0));
    }
    cs.bars = series.clone();
    cs.closed_len = 190;
    cs.refresh_caches();
    assert_refresh_matches_full(&cs, &series[..190], "close to 190");
    assert!(
        cs.hour_marks.iter().any(|&m| m >= 120.0),
        "expected a genuine hour-mark boundary within the appended region (index >= \
             120) so the append fast-path's + prev_len offset is exercised for real, not \
             just against an empty slice"
    );

    // reload onto a shorter, differently-epoched series (symbol/timeframe swap) —
    // forces the full-recompute fallback (boundary mismatch)
    let reload: Vec<Bar> = (0..40)
        .map(|i| {
            let mut b = bar(i, 3.0, 33.0);
            b.ot += 3_600_000; // different epoch boundary -> forces full recompute
            b
        })
        .collect();
    cs.bars = reload.clone();
    cs.closed_len = 40;
    cs.refresh_caches();
    assert_refresh_matches_full(&cs, &reload[..40], "reload");
}

/// `refresh_caches` does O(delta) work, not O(history): `last_refresh_work.recomputed`
/// must read 0 on an unchanged-key call (forming tick), the exact append delta on a
/// bar close (1, then 5 at once), and the full closed length again on a structural
/// reload — never more than the bars that actually changed.
#[test]
fn refresh_caches_does_delta_work_only() {
    let mut cs = ChartState::default();

    let mut series: Vec<Bar> = (0..=100).map(|i| bar(i, 10.0, 20.0)).collect();
    cs.bars = series.clone();
    cs.closed_len = 100;
    cs.refresh_caches();
    assert_eq!(cs.last_refresh_work.recomputed, 100, "seed is a full recompute");

    // forming-tick: mutate the forming bar (index 100), closed prefix unchanged
    series[100].h = 999.0;
    cs.bars = series.clone();
    cs.refresh_caches();
    assert_eq!(cs.last_refresh_work.recomputed, 0, "forming tick touches no closed bars");

    // close 1
    series.push(bar(101, 5.0, 6.0));
    cs.bars = series.clone();
    cs.closed_len = 101;
    cs.refresh_caches();
    assert_eq!(cs.last_refresh_work.recomputed, 1, "one bar closed");

    // close 5 at once
    for i in 102..=106 {
        series.push(bar(i, 1.0, 2.0));
    }
    cs.bars = series.clone();
    cs.closed_len = 106;
    cs.refresh_caches();
    assert_eq!(cs.last_refresh_work.recomputed, 5, "five bars closed at once");

    // reload onto a shorter, differently-epoched series
    let reload: Vec<Bar> = (0..40)
        .map(|i| {
            let mut b = bar(i, 3.0, 33.0);
            b.ot += 3_600_000;
            b
        })
        .collect();
    cs.bars = reload.clone();
    cs.closed_len = 40;
    cs.refresh_caches();
    assert_eq!(cs.last_refresh_work.recomputed, 40, "reload is a full recompute");
}
