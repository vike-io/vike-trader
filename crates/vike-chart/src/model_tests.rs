use super::*;

fn bar(i: usize, l: f64, h: f64) -> Bar {
    Bar {
        t: i as f64,
        // one-minute bars starting at an arbitrary epoch minute
        ot: 1_700_000_040_000 + i as i64 * 60_000,
        o: (l + h) / 2.0,
        h,
        l,
        c: (l + h) / 2.0,
        v: 1.0,
    }
}

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
#[allow(clippy::type_complexity)]
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

/// One `vike_marketdata::Bar` (the wire type `sync` reads FROM) at index `i`, on the
/// same one-minute-bar epoch as [`bar`] above. `jitter` perturbs `close`/`high` so
/// a "forming tick" mutation is distinguishable from the bar's prior values.
fn raw_bar(i: usize, jitter: f64) -> vike_marketdata::Bar {
    let ts = 1_700_000_040_000 + i as i64 * 60_000;
    let base = 100.0 + (i as f64 * 0.31).sin() * 3.0;
    vike_marketdata::Bar {
        ts,
        open: base,
        high: base + 1.0 + jitter.abs(),
        low: base - 1.0,
        close: base + jitter,
        volume: 500.0 + i as f64,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A `vike_marketdata::Bar` (wire type) at an explicit open-time `ts` (ms) — for tz-mark tests
/// that need specific wall-clock instants rather than [`raw_bar`]'s index-derived
/// one-minute epoch.
fn wire_bar_at(ts: i64) -> vike_marketdata::Bar {
    vike_marketdata::Bar {
        ts,
        open: 1.0,
        high: 2.0,
        low: 1.0,
        close: 1.5,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Oracle: a fresh full clear+rebuild of `(closed, forming)` via `render_bar` —
/// exactly what today's `sync_from_core` loop (and `sync`'s own full-rebuild
/// branch) produces. `sync`'s incremental output must equal this byte-for-byte
/// after every call, no matter which internal branch it took.
fn full_rebuild(
    closed: &[vike_marketdata::Bar],
    forming: Option<&vike_marketdata::Bar>,
) -> Vec<Bar> {
    let mut bars: Vec<Bar> = closed.iter().enumerate().map(|(i, b)| render_bar(i, b)).collect();
    if let Some(f) = forming {
        let idx = bars.len();
        bars.push(render_bar(idx, f));
    }
    bars
}

/// `sync` then assert `cs.bars` equals a fresh full rebuild of the same
/// `(closed, forming)` — the equivalence gate for every step of a tick sequence.
fn sync_and_check_equivalence(
    cs: &mut ChartState,
    closed: &[vike_marketdata::Bar],
    forming: Option<&vike_marketdata::Bar>,
    label: &str,
) {
    cs.sync(&std::sync::Arc::new(closed.to_vec()), forming);
    assert_eq!(cs.bars, full_rebuild(closed, forming), "{label}: bars diverged from full rebuild");
}

/// `sync` must be byte-identical to a full clear+rebuild at every step of a
/// realistic tick sequence: seed, five forming-only ticks, a single bar close, a
/// forming tick after that close, three bars closing at once (GUI polled less
/// often than bars closed), then a reload onto a shorter, differently-shaped
/// series (symbol/timeframe swap). Exercises all three `sync` branches
/// (unchanged-prefix, append, full-rebuild) against the same oracle.
#[test]
fn sync_matches_full_rebuild_over_tick_sequence() {
    let mut cs = ChartState::default();
    let mut closed: Vec<vike_marketdata::Bar> = (0..100).map(|i| raw_bar(i, 0.3)).collect();
    let mut forming = raw_bar(100, 0.0);

    sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "seed");

    for k in 0..5 {
        forming.close += (k + 1) as f64 * 0.02;
        forming.high = forming.high.max(forming.close);
        sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "forming-tick");
    }

    closed.push(forming); // bar 100 closes
    forming = raw_bar(101, -0.1);
    sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "close 1");

    forming.close -= 0.03;
    sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "forming-tick after close 1");

    closed.push(forming); // bar 101 closes
    closed.push(raw_bar(102, 0.15)); // + two more that closed while unpolled
    closed.push(raw_bar(103, -0.25));
    forming = raw_bar(104, 0.0);
    sync_and_check_equivalence(&mut cs, &closed, Some(&forming), "close 3");

    let reload_closed: Vec<vike_marketdata::Bar> = (0..40).map(|i| raw_bar(i, -0.5)).collect();
    let reload_forming = raw_bar(40, 0.2);
    sync_and_check_equivalence(&mut cs, &reload_closed, Some(&reload_forming), "reload");
}

/// `sync` does O(delta) rendering work, not O(history): the work counter must
/// read 0 on an unchanged-prefix forming tick, the exact delta on an append (1
/// bar, then 3 at once), and the full length again on a structural reload — never
/// more than the bars that actually changed.
#[test]
fn sync_does_delta_work_only() {
    let mut cs = ChartState::default();
    let mut closed: Vec<vike_marketdata::Bar> = (0..100).map(|i| raw_bar(i, 0.3)).collect();
    let mut forming = raw_bar(100, 0.0);

    cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
    assert_eq!(cs.last_sync_work.closed_rerendered, 100, "seed renders every closed bar");
    assert!(cs.last_sync_work.forming_rerendered);

    forming.close += 0.05;
    cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
    assert_eq!(cs.last_sync_work.closed_rerendered, 0, "forming tick touches no closed bars");

    closed.push(forming); // bar 100 closes
    forming = raw_bar(101, -0.1);
    cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
    assert_eq!(cs.last_sync_work.closed_rerendered, 1, "one bar closed");

    closed.push(forming); // bar 101 closes
    closed.push(raw_bar(102, 0.15));
    closed.push(raw_bar(103, -0.25));
    forming = raw_bar(104, 0.0);
    cs.sync(&std::sync::Arc::new(closed.clone()), Some(&forming));
    assert_eq!(cs.last_sync_work.closed_rerendered, 3, "three bars closed at once");

    let reload_closed: Vec<vike_marketdata::Bar> = (0..40).map(|i| raw_bar(i, -0.5)).collect();
    cs.sync(&std::sync::Arc::new(reload_closed.clone()), None);
    assert_eq!(cs.last_sync_work.closed_rerendered, 40, "reload is a full rebuild");
    assert!(!cs.last_sync_work.forming_rerendered);
}

/// `set_tz` must invalidate `hour_marks` on the very next `sync`, even though the closed
/// bars themselves are byte-identical (so `sync`'s own bars-content dedup takes the
/// "unchanged prefix" fast path) — `refresh_caches`'s `cache_key` carries the tz identity
/// specifically so this doesn't go stale.
#[test]
fn set_tz_invalidates_mark_cache() {
    let base =
        chrono::DateTime::parse_from_rfc3339("2026-07-11T11:30:00Z").unwrap().timestamp_millis();
    let closed: Vec<vike_marketdata::Bar> =
        (0..4).map(|i| wire_bar_at(base + i as i64 * 900_000)).collect();
    let mut cs = ChartState::default();
    cs.set_tz(DisplayTz::Utc);
    cs.sync(&std::sync::Arc::new(closed.clone()), None);
    assert_eq!(cs.hour_marks, vec![2.0]);
    cs.set_tz(DisplayTz::Named(chrono_tz::Asia::Kolkata));
    cs.sync(&std::sync::Arc::new(closed.clone()), None); // same bars, new tz → cache must refresh
    assert_eq!(cs.hour_marks, vec![0.0]);
}

/// [`ChartState::visible_vol_max`] must cache by `(lo, hi, cache_key)`: a repeat call
/// with the same window is an O(1) cache hit (no work-counter bump); a different
/// window, or a bar CLOSE (which bumps `cache_key`), forces a genuine recompute. Every
/// returned value must also equal a plain fold over the CLOSED window
/// `bars[lo..hi.min(closed_len)]` — the oracle this cache must never diverge from.
#[test]
fn visible_vol_max_caches_by_key() {
    // 100 closed bars, v = 1.0..=100.0 (bar i has v = i + 1).
    let mut cs = ChartState {
        bars: (0..100)
            .map(|i| {
                let mut b = bar(i, 10.0, 20.0);
                b.v = (i + 1) as f64;
                b
            })
            .collect(),
        closed_len: 100,
        ..Default::default()
    };
    cs.refresh_caches(); // seeds cache_key

    // oracle: a plain fold over the CLOSED window, matching visible_vol_max's contract.
    let oracle = |cs: &ChartState, lo: usize, hi: usize| -> f64 {
        let hi = hi.min(cs.closed_len);
        let lo = lo.min(hi);
        cs.bars[lo..hi].iter().map(|b| b.v).fold(0.0_f64, f64::max)
    };

    assert_eq!(cs.visible_vol_max(0, 50), 50.0, "max of v=1..=50 is 50.0");
    assert_eq!(cs.visible_vol_max(0, 50), oracle(&cs, 0, 50));
    assert_eq!(cs.vol_recompute_count.get(), 1, "first call is a genuine miss");

    // same (lo, hi), same cache_key -> cache hit, no recompute.
    assert_eq!(cs.visible_vol_max(0, 50), 50.0);
    assert_eq!(cs.vol_recompute_count.get(), 1, "repeat call must be a cache hit");

    // different window -> recompute (count bumps).
    assert_eq!(cs.visible_vol_max(10, 60), oracle(&cs, 10, 60));
    assert_eq!(cs.vol_recompute_count.get(), 2, "a different (lo, hi) must miss");

    // repeat the NEW window -> cache hit again.
    assert_eq!(cs.visible_vol_max(10, 60), oracle(&cs, 10, 60));
    assert_eq!(cs.vol_recompute_count.get(), 2, "repeat of the new window must hit");

    // close a bar (bumps cache_key via refresh_caches) -> same (lo, hi) now misses.
    let mut closing = bar(100, 10.0, 20.0);
    closing.v = 999.0; // outside [10, 60), so the oracle value is unchanged...
    cs.bars.push(closing);
    cs.closed_len = 101;
    cs.refresh_caches(); // bumps cache_key (closed_len 100 -> 101)
    assert_eq!(
        cs.visible_vol_max(10, 60),
        oracle(&cs, 10, 60),
        "value is unchanged (bar 100 is outside [10,60)), but the key changed"
    );
    assert_eq!(
        cs.vol_recompute_count.get(),
        3,
        "a bar close must force a recompute even for the same window"
    );

    // empty window -> 0.0 (the fold's zero identity), and does not panic.
    assert_eq!(cs.visible_vol_max(60, 10), 0.0, "lo > hi collapses to an empty window");
    assert_eq!(cs.visible_vol_max(200, 300), 0.0, "window entirely past closed_len");
}

/// [`ChartState::visible_vol_max`] only ever folds the CLOSED prefix — a forming bar
/// (`bars[closed_len..]`) sitting inside `[lo, hi)` must NOT be picked up, even though
/// today's inline `visible_slice(bars, ..)` fold (which callers replace with this cache
/// PLUS their own `.max(forming.v)`) would see it as just the last element. This is the
/// forming-visible/forming-hidden equivalence the two call sites depend on.
#[test]
fn visible_vol_max_excludes_the_forming_bar() {
    let mut cs = ChartState {
        bars: (0..10)
            .map(|i| {
                let mut b = bar(i, 10.0, 20.0);
                b.v = 5.0; // closed bars: uniform v=5.0
                b
            })
            .collect(),
        closed_len: 9, // bar index 9 is forming
        ..Default::default()
    };
    cs.bars[9].v = 1_000.0; // forming bar: wildly larger volume
    cs.refresh_caches();

    // forming bar (index 9) IS inside [0, 10) -> must still be excluded from the CLOSED-only max.
    assert_eq!(
        cs.visible_vol_max(0, 10),
        5.0,
        "forming bar's huge v must not leak into the closed-only max"
    );

    // sanity: the SAME window over the full `bars` slice (closed + forming), i.e. what
    // today's inline fold computes, DOES pick up the forming bar — proving the two are
    // deliberately different, and that callers must .max() the forming value in themselves.
    let full_fold = cs.bars[0..10].iter().map(|b| b.v).fold(0.0_f64, f64::max);
    assert_eq!(full_fold, 1_000.0);
}

/// `visible_vol_max` returns [`ChartState::visible_vol_max_shared`]'s value exactly —
/// the `&mut self` entry point is a thin forwarder, not a second cache/implementation.
#[test]
fn visible_vol_max_matches_shared_twin() {
    let mut cs = ChartState {
        bars: (0..20)
            .map(|i| {
                let mut b = bar(i, 10.0, 20.0);
                b.v = (i * 3) as f64;
                b
            })
            .collect(),
        closed_len: 20,
        ..Default::default()
    };
    let oracle = cs.bars[2..15].iter().map(|b| b.v).fold(0.0_f64, f64::max);
    assert_eq!(cs.visible_vol_max_shared(2, 15), oracle);
    assert_eq!(
        cs.visible_vol_max(2, 15),
        oracle,
        "&mut self entry point must agree with the &self twin"
    );
}

// === SP2 T4: visible-range volume-profile overlay cache ================================

fn fp(idx: u64, cells: &[(f64, f64, f64)]) -> FootprintBar {
    FootprintBar {
        bar_index: idx,
        cells: cells
            .iter()
            .map(|&(p, b, s)| vike_orderflow::PriceBin { price: p, buy_vol: b, sell_vol: s })
            .collect(),
    }
}

/// [`ChartState::visible_profile`] must cache by [`ProfileCacheKey`]: a repeat call with
/// the same `(i0, i1, footprints.len(), tick_size)` is an O(1) cache hit (no recompute-
/// counter bump); a different range, a different footprint length, or a different tick
/// size each force a genuine recompute. Every returned value must also equal a direct
/// [`VolumeProfile::from_footprints`] call — the oracle this cache must never diverge from.
#[test]
fn visible_profile_caches_by_key() {
    let cs = ChartState::default();
    let fps = vec![
        fp(0, &[(100.0, 5.0, 2.0)]),
        fp(1, &[(100.0, 1.0, 0.0), (101.0, 0.0, 4.0)]),
        fp(2, &[(101.0, 3.0, 0.0)]),
    ];
    let oracle = |ts: f64, i0: usize, i1: usize| VolumeProfile::from_footprints(&fps, ts, i0, i1);

    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 0, 2), oracle(1.0, 0, 2));
    assert_eq!(cs.profile_recompute_count.get(), 1, "first call is a genuine miss");

    // same (i0, i1, len, tick_size) -> cache hit, no recompute.
    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 0, 2), oracle(1.0, 0, 2));
    assert_eq!(cs.profile_recompute_count.get(), 1, "repeat call must be a cache hit");

    // different range -> miss.
    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 1, 2), oracle(1.0, 1, 2));
    assert_eq!(cs.profile_recompute_count.get(), 2, "a different (i0, i1) must miss");

    // repeat the new range -> hit.
    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 1, 2), oracle(1.0, 1, 2));
    assert_eq!(cs.profile_recompute_count.get(), 2, "repeat of the new range must hit");

    // same range, different tick_size -> miss (bucket width changed, len unchanged).
    assert_eq!(*cs.visible_profile_shared(&fps, 2.0, 1, 2), oracle(2.0, 1, 2));
    assert_eq!(cs.profile_recompute_count.get(), 3, "a different tick_size must miss");

    // shorter footprint slice (len changes, same range/tick_size) -> miss. Oracle is over
    // the SAME shortened slice — `from_footprints` clamps `hi` to the slice's own length,
    // so comparing against the full-`fps` oracle would silently check a different
    // computation (a different `hi` clamp) rather than the cache-vs-direct equivalence.
    let short = &fps[..2];
    assert_eq!(
        *cs.visible_profile_shared(short, 2.0, 1, 2),
        VolumeProfile::from_footprints(short, 2.0, 1, 2)
    );
    assert_eq!(cs.profile_recompute_count.get(), 4, "a different footprint length must miss");
}

/// `visible_profile` returns [`ChartState::visible_profile_shared`]'s value exactly — the
/// `&mut self` entry point is a thin forwarder, not a second cache/implementation.
#[test]
fn visible_profile_matches_shared_twin() {
    let mut cs = ChartState::default();
    let fps = vec![fp(0, &[(50.0, 2.0, 1.0)]), fp(1, &[(51.0, 0.0, 3.0)])];
    let oracle = VolumeProfile::from_footprints(&fps, 1.0, 0, 1);
    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 0, 1), oracle);
    assert_eq!(
        *cs.visible_profile(&fps, 1.0, 0, 1),
        oracle,
        "&mut self entry point must agree with the &self twin"
    );
}

/// Empty/out-of-range windows must not panic and must match
/// [`VolumeProfile::from_footprints`]'s own documented empty behavior (poc 0.0, va (0,0),
/// no bins) — the cache is a pure memoization layer, never a second source of truth for
/// edge-case handling.
#[test]
fn visible_profile_empty_range_is_the_from_footprints_default() {
    let cs = ChartState::default();
    let fps = vec![fp(0, &[(100.0, 5.0, 2.0)])];
    let empty = cs.visible_profile_shared(&fps, 1.0, 5, 9);
    assert!(empty.bins.is_empty());
    assert_eq!(empty.poc, 0.0);
    assert_eq!(empty.value_area, (0.0, 0.0));
    // lo > hi also collapses to the same empty result, matching from_footprints directly.
    assert_eq!(*cs.visible_profile_shared(&fps, 1.0, 2, 1), *empty);
}

// === SP3 Task B #2 (SP2 final-review finding B) + SP3 TB-fix (post-Task-B review finding,
// MEDIUM: generation-hardened against ABA staleness) CVD pane recompute cache ============

/// [`ChartState::cvd_shared`] must cache by [`CvdCacheKey`] (`(generation, len)`): a repeat
/// call at the SAME generation+len is an O(1) cache hit — no recompute-counter bump, and the
/// exact same `Rc` handed back (proving it wasn't just a coincidental content match from an
/// independent recompute) — while a generation bump is ALWAYS a genuine miss, even over the
/// byte-identical slice at the SAME address. That last property is the ABA-hardening fix
/// itself under direct test: the old `(ptr, len)` key would have wrongly served the stale
/// cached `Rc` here (same address, same length); keying on the caller-supplied generation
/// instead means a bumped generation is trusted as "content changed" without re-deriving that
/// from the slice's own (recyclable) address. Every returned value must equal a direct
/// `orderflow::cvd_from_footprints` call — the oracle this cache must never diverge from.
#[test]
fn cvd_shared_caches_by_generation_and_len_not_pointer_identity() {
    let cs = ChartState::default();
    let fps = vec![
        fp(0, &[(100.0, 5.0, 2.0)]),                    // delta +3
        fp(1, &[(101.0, 1.0, 4.0)]),                    // delta -3
        fp(2, &[(100.0, 2.0, 0.0), (101.0, 0.0, 1.0)]), // delta +1
    ];
    let oracle = crate::orderflow::cvd_from_footprints(&fps);

    let first = cs.cvd_shared(&fps, 1);
    assert_eq!(*first, oracle);
    assert_eq!(cs.cvd_recompute_count.get(), 1, "first call is a genuine miss");

    // same slice (same address AND length), same generation -> cache hit: same Rc, no
    // recompute.
    let second = cs.cvd_shared(&fps, 1);
    assert!(
        Rc::ptr_eq(&first, &second),
        "a repeat call at the SAME generation must hand back the SAME Rc"
    );
    assert_eq!(cs.cvd_recompute_count.get(), 1, "repeat call must be a cache hit");

    // the IDENTICAL slice (same address, same length, same content) but a bumped generation
    // -> must MISS. This is the ABA scenario the fix targets: a toggle-off/on cycle can hand
    // `cvd_shared` a slice whose address+length coincidentally match the stale cached key even
    // though the content is logically new; the generation is the caller's authoritative signal
    // that it changed, and must win over the slice's own identity.
    let third = cs.cvd_shared(&fps, 2);
    assert_eq!(*third, oracle);
    assert_eq!(
        cs.cvd_recompute_count.get(),
        2,
        "a generation bump must miss even over the identical slice"
    );

    // a shorter slice of the SAME underlying allocation, SAME generation as `third` -> still
    // a miss (len is a belt-and-suspenders second key component).
    let shorter = &fps[..2];
    let fourth = cs.cvd_shared(shorter, 2);
    assert_eq!(*fourth, crate::orderflow::cvd_from_footprints(shorter));
    assert_eq!(cs.cvd_recompute_count.get(), 3, "a different length must miss");

    // repeat the full slice at generation 1 again -> miss (the slot now holds generation 2's
    // key from the calls above).
    let fifth = cs.cvd_shared(&fps, 1);
    assert_eq!(*fifth, oracle);
    assert_eq!(
        cs.cvd_recompute_count.get(),
        4,
        "the slot was evicted by the later-generation calls"
    );
}

/// Empty footprints must not panic and must match `cvd_from_footprints(&[])`'s own
/// documented empty behavior (an empty `Vec`) — the cache is a pure memoization layer, never
/// a second source of truth for edge-case handling.
#[test]
fn cvd_shared_empty_footprints_is_empty() {
    let cs = ChartState::default();
    assert!(cs.cvd_shared(&[], 0).is_empty());
}

// === chart-perf T6: transform-style recompute cache ===================================

const T6_PARAMS: TransformParams = TransformParams { line_break_n: 3, pnf_reversal: 3 };

/// A render `Bar` with a ±1 range around `close` — one-minute epoch like [`bar`] above.
fn tbar(i: usize, close: f64) -> Bar {
    Bar {
        t: i as f64,
        ot: 1_700_000_040_000 + i as i64 * 60_000,
        o: close,
        h: close + 1.0,
        l: close - 1.0,
        c: close,
        v: 100.0 + i as f64,
    }
}

/// Trend + oscillation so every transform (Renko/Range/LineBreak/Kagi/PnF) sees real
/// breakouts against its `auto_box`(~2.0, since h-l == 2) — otherwise the transforms return
/// empty vecs and the equivalence gate would pass vacuously.
fn close_at(i: usize) -> f64 {
    100.0 + (i as f64 * 0.05).sin() * 8.0 + i as f64 * 0.03
}

/// The oracle: exactly how `chart.rs` builds the `owned` proxy for each transform style over a
/// full (closed + forming) series — independent of [`ChartState::transformed`]'s internals.
fn oracle_transform(
    style: ChartStyle,
    params: TransformParams,
    bars: &[Bar],
    first_ot: i64,
) -> Vec<Bar> {
    use ChartStyle::*;
    match style {
        HeikinAshi => heikin_ashi(bars),
        Renko => transforms::reindex(transforms::renko(bars)),
        Range => transforms::reindex(transforms::range_bars(bars)),
        LineBreak => transforms::reindex(transforms::line_break(bars, params.line_break_n)),
        Kagi => transforms::reindex(
            transforms::kagi(bars)
                .prices
                .iter()
                .map(|&p| Bar { t: 0.0, ot: first_ot, o: p, h: p, l: p, c: p, v: 0.0 })
                .collect(),
        ),
        PointFigure => {
            let (cols, _) = transforms::point_and_figure(bars, params.pnf_reversal);
            transforms::reindex(
                cols.iter()
                    .map(|c| Bar {
                        t: 0.0,
                        ot: first_ot,
                        o: c.bottom,
                        h: c.top,
                        l: c.bottom,
                        c: c.top,
                        v: 0.0,
                    })
                    .collect(),
            )
        }
        _ => vec![],
    }
}

/// Byte/value-identical assertion — every f64 field compared by `to_bits`, not `==`.
fn assert_bars_bit_identical(got: &[Bar], want: &[Bar], label: &str) {
    assert_eq!(got.len(), want.len(), "{label}: length {} != oracle {}", got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert_eq!(g.t.to_bits(), w.t.to_bits(), "{label}[{i}].t");
        assert_eq!(g.ot, w.ot, "{label}[{i}].ot");
        assert_eq!(g.o.to_bits(), w.o.to_bits(), "{label}[{i}].o");
        assert_eq!(g.h.to_bits(), w.h.to_bits(), "{label}[{i}].h");
        assert_eq!(g.l.to_bits(), w.l.to_bits(), "{label}[{i}].l");
        assert_eq!(g.c.to_bits(), w.c.to_bits(), "{label}[{i}].c");
        assert_eq!(g.v.to_bits(), w.v.to_bits(), "{label}[{i}].v");
    }
}

fn set_series(cs: &mut ChartState, bars: &[Bar], closed_len: usize) {
    cs.bars = bars.to_vec();
    cs.closed_len = closed_len;
    cs.refresh_caches();
}

/// Install `(bars, closed_len)`, run `transformed`, and assert it is byte-identical to the
/// oracle full-transform of (closed + forming).
fn drive_and_check(
    cs: &mut ChartState,
    bars: &[Bar],
    closed_len: usize,
    style: ChartStyle,
    label: &str,
) {
    set_series(cs, bars, closed_len);
    let got = cs.transformed(style, T6_PARAMS);
    let first_ot = cs.bars.first().map_or(0, |b| b.ot);
    let want = oracle_transform(style, T6_PARAMS, &cs.bars, first_ot);
    assert_bars_bit_identical(&got, &want, label);
}

/// THE gate: for EACH of the six transform styles, `transformed` is byte-identical to a full
/// recompute of (closed + forming) at every step of a realistic tick sequence — seed 300
/// closed + forming, five forming-only ticks, a bar close, another forming tick, then five
/// bars closing at once.
#[test]
fn transformed_matches_full_recompute_per_style() {
    use ChartStyle::*;
    for style in [HeikinAshi, Renko, Range, LineBreak, Kagi, PointFigure] {
        let mut cs = ChartState::default();

        // seed: 300 closed bars (0..299) + forming (300)
        let mut series: Vec<Bar> = (0..=300).map(|i| tbar(i, close_at(i))).collect();
        drive_and_check(&mut cs, &series, 300, style, &format!("{style:?} seed"));
        assert!(
            !cs.transformed(style, T6_PARAMS).is_empty(),
            "{style:?}: expected a non-empty transform (the gate must not pass vacuously)"
        );

        // forming-tick x5: mutate the forming bar (index 300) only
        for k in 0..5 {
            series[300] = tbar(300, close_at(300) + (k as f64 + 1.0) * 0.7);
            drive_and_check(&mut cs, &series, 300, style, &format!("{style:?} forming-tick {k}"));
        }

        // close 1: bar 300 closes, new forming 301
        series.push(tbar(301, close_at(301)));
        drive_and_check(&mut cs, &series, 301, style, &format!("{style:?} close 1"));

        // forming-tick after close 1
        series[301] = tbar(301, close_at(301) + 1.3);
        drive_and_check(
            &mut cs,
            &series,
            301,
            style,
            &format!("{style:?} forming-tick after close 1"),
        );

        // close 5 at once: bars 302..=306 append, closed prefix 301 -> 306, forming 306
        for i in 302..=306 {
            series.push(tbar(i, close_at(i)));
        }
        drive_and_check(&mut cs, &series, 306, style, &format!("{style:?} close 5"));
    }
}

/// The work counter: closed-prefix recomputes bump only on bar-close / style / param change
/// for the TWO-TIER styles (HeikinAshi/LineBreak) and never on a forming tick or a static
/// frame; the FALLBACK styles (Renko/Range/Kagi/PnF) serve a static frame from cache (no bump)
/// but reprocess on a forming tick (their `auto_box` reads the forming bar) — the documented
/// residual cost.
#[test]
fn transformed_recomputes_only_tail() {
    use ChartStyle::*;

    // two-tier: forming ticks reuse the cached closed prefix.
    for style in [HeikinAshi, LineBreak] {
        let mut cs = ChartState::default();
        let mut series: Vec<Bar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
        set_series(&mut cs, &series, 200);
        cs.transformed(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: seed builds the closed prefix once"
        );

        // static repeat (identical state) → O(1) cache hit, no bump
        cs.transformed(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: static repeat is a cache hit"
        );

        // forming ticks → closed prefix unchanged → reuse, no bump
        for k in 0..4 {
            series[200] = tbar(200, close_at(200) + (k as f64 + 1.0) * 0.6);
            set_series(&mut cs, &series, 200);
            cs.transformed(style, T6_PARAMS);
        }
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: forming ticks reuse the closed prefix"
        );

        // bar close → closed prefix changes → bump
        series.push(tbar(201, close_at(201)));
        set_series(&mut cs, &series, 201);
        cs.transformed(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            2,
            "{style:?}: a bar close rebuilds the closed prefix"
        );

        // param change → different key → bump
        cs.transformed(style, TransformParams { line_break_n: 2, pnf_reversal: 3 });
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            3,
            "{style:?}: a param change rebuilds the closed prefix"
        );

        // style change → different key → bump
        cs.transformed(Candles, T6_PARAMS); // non-transform style: still a key change
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            4,
            "{style:?}: a style change rebuilds"
        );
    }

    // fallback: static frame cached, forming tick reprocesses.
    for style in [Renko, Range, Kagi, PointFigure] {
        let mut cs = ChartState::default();
        let mut series: Vec<Bar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
        set_series(&mut cs, &series, 200);
        cs.transformed(style, T6_PARAMS);
        assert_eq!(cs.transform_closed_recompute_count.get(), 1, "{style:?}: seed recompute");

        // static repeat → cache hit, no bump (the win the fallback still delivers)
        cs.transformed(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            1,
            "{style:?}: static repeat is a cache hit"
        );

        // forming tick → global auto_box shifts → must reprocess → bump
        series[200] = tbar(200, close_at(200) + 3.0);
        set_series(&mut cs, &series, 200);
        cs.transformed(style, T6_PARAMS);
        assert_eq!(
            cs.transform_closed_recompute_count.get(),
            2,
            "{style:?}: a forming tick reprocesses (global auto_box)"
        );
    }
}

/// The Kagi/PnF structured results the custom drawing reads (`cached_{kagi,pnf}_shared`) are
/// populated by `transformed` and byte-identical to a fresh `transforms::{kagi,point_and_figure}`
/// over (closed + forming) — the data-flow the `chart.rs` `draw_kagi`/`draw_pnf` rewire relies
/// on. Each style leaves the OTHER structured slot empty.
#[test]
fn cached_structured_matches_fresh_compute() {
    let mut cs = ChartState::default();
    let series: Vec<Bar> = (0..=200).map(|i| tbar(i, close_at(i))).collect();
    set_series(&mut cs, &series, 200);

    cs.transformed(ChartStyle::Kagi, T6_PARAMS);
    let k = cs.cached_kagi_shared().expect("Kagi style populates the structured Kagi cache");
    let fresh = transforms::kagi(&cs.bars);
    assert_eq!(k.prices.len(), fresh.prices.len(), "kagi price count");
    for (a, b) in k.prices.iter().zip(&fresh.prices) {
        assert_eq!(a.to_bits(), b.to_bits(), "kagi price bits");
    }
    assert_eq!(k.thick, fresh.thick, "kagi thick flags");
    assert!(cs.cached_pnf_shared().is_none(), "Kagi style leaves the PnF slot empty");

    cs.transformed(ChartStyle::PointFigure, T6_PARAMS);
    let pnf = cs.cached_pnf_shared().expect("PnF style populates the structured PnF cache");
    let (cols, box_) = transforms::point_and_figure(&cs.bars, T6_PARAMS.pnf_reversal);
    assert_eq!(pnf.1.to_bits(), box_.to_bits(), "pnf box bits");
    assert_eq!(pnf.0.len(), cols.len(), "pnf column count");
    for (a, b) in pnf.0.iter().zip(&cols) {
        assert_eq!(a.up, b.up, "pnf column up flag");
        assert_eq!(a.top.to_bits(), b.top.to_bits(), "pnf column top bits");
        assert_eq!(a.bottom.to_bits(), b.bottom.to_bits(), "pnf column bottom bits");
    }
    assert!(cs.cached_kagi_shared().is_none(), "PnF style leaves the Kagi slot empty");
}
