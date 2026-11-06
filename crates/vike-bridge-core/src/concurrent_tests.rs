use super::*;
use crate::rate_discovery::WeightBudget;

/// binance SPOT: 6000 weight/min, weight-2 pages, 0.40 ⇒ a 50 ms target gap against ~280 ms.
fn spot_pacer() -> Pacer {
    let mut p = Pacer::discovered(WeightBudget { limit: 6000, interval_secs: 60 }, 0.4);
    p.observe_request(Some(100), Duration::from_millis(280));
    p.observe_request(Some(102), Duration::from_millis(280)); // weight 2
    p
}

// ----- split_range --------------------------------------------------------------------------

#[test]
fn spans_are_disjoint_ascending_and_cover_the_window_exactly() {
    for lanes in 1..=8usize {
        for (start, end) in [(0i64, 999i64), (1_700_000_000_000, 1_700_086_400_000), (-50, 50)] {
            let spans = split_range(start, end, lanes);
            assert!(!spans.is_empty(), "lanes={lanes} {start}..{end}");
            assert_eq!(spans.first().unwrap().0, start, "the cover starts at start_ms");
            assert_eq!(spans.last().unwrap().1, end, "...and ends at end_ms");
            for w in spans.windows(2) {
                assert_eq!(w[1].0, w[0].1 + 1, "disjoint AND contiguous: {spans:?}");
            }
            for (s, e) in &spans {
                assert!(s <= e, "no inverted span: {spans:?}");
            }
            assert!(spans.len() <= lanes);
        }
    }
}

/// The one-lane case IS the caller's own window.
#[test]
fn one_lane_is_the_whole_window_unchanged() {
    assert_eq!(split_range(10, 20, 1), vec![(10, 20)]);
    assert_eq!(split_range(10, 20, 0), vec![(10, 20)], "zero lanes is one lane, never none");
    // A window too narrow to split does not produce empty or inverted spans.
    assert_eq!(split_range(10, 10, 8), vec![(10, 10)]);
    assert_eq!(split_range(10, 13, 8), vec![(10, 13)]);
}

#[test]
fn an_empty_window_yields_no_spans_so_the_venue_is_never_touched() {
    assert!(split_range(20, 10, 4).is_empty());
    assert!(split_range(i64::MAX, i64::MIN, 8).is_empty());
}

/// An absurd lane count is CLAMPED, not honoured: no allocation that aborts the process.
#[test]
fn an_absurd_lane_count_clamps_instead_of_allocating() {
    let spans = split_range(i64::MIN, i64::MAX, usize::MAX);
    assert_eq!(spans.len(), MAX_LANES);
    assert_eq!(spans.first().unwrap().0, i64::MIN);
    assert_eq!(spans.last().unwrap().1, i64::MAX);
    assert_eq!(split_range(0, 999, 1_000_000).len(), MAX_LANES);
}

/// Extreme bounds must not wrap into an overlapping or inverted span (the arithmetic is i128).
#[test]
fn a_full_width_window_does_not_wrap() {
    let spans = split_range(i64::MIN, i64::MAX, 4);
    assert_eq!(spans.len(), 4);
    assert_eq!(spans.first().unwrap().0, i64::MIN);
    assert_eq!(spans.last().unwrap().1, i64::MAX);
    for w in spans.windows(2) {
        assert_eq!(w[1].0, w[0].1 + 1);
        assert!(w[0].0 < w[0].1);
    }
}

/// The remainder lands in the LAST span, and the cover is exact rather than approximately right.
#[test]
fn the_division_remainder_lands_in_the_final_span() {
    // 10 units over 3 lanes: 3 + 3 + 4.
    assert_eq!(split_range(0, 9, 3), vec![(0, 2), (3, 5), (6, 9)]);
}

// ----- LaneGate: the aggregate rate ---------------------------------------------------------

/// The gate's pacer answers the target the aggregate must not exceed: 40 % of binance spot's 6000
/// weight/min at weight-2 pages is a 50 ms gap ⇒ 2400 weight/min.
#[test]
fn the_gate_paces_on_the_target_gap_not_on_the_floored_sleep() {
    let p = spot_pacer();
    let gap = p.target_gap().as_secs_f64();
    assert!((gap - 0.050).abs() < 1e-6, "{gap}");
    // The sequential SLEEP is floored: it cannot express 50 ms when a request costs 280 ms.
    assert!(p.next_delay() <= Duration::from_millis(1), "the sequential sleep is floored");
    let per_min = (60.0 / gap) * 2.0;
    assert!((per_min - 2400.0).abs() < 1.0, "{per_min} weight/min is 40% of 6000");
    assert!(per_min <= 6000.0, "and stays under the venue's published ceiling");
}

/// THE budget property: K admissions occupy `K * target_gap` of wall clock **however many lanes
/// take them**. Asserted as a LOWER bound on elapsed time, the direction that cannot flake (a
/// loaded box is slower, never faster).
#[test]
fn admissions_occupy_one_target_gap_each_whatever_the_lane_count() {
    // 20 ms: quick, yet far above scheduler noise (24 admissions = ~460 ms mandated).
    let gap = Duration::from_millis(20);
    const ADMITS_PER_LANE: usize = 6;

    for lanes in [1usize, 2, 4] {
        let gate = LaneGate::new(Pacer::fallback(gap)); // `Fixed`'s target gap IS `gap`
        let spans = split_range(0, 10_000, lanes);
        assert_eq!(spans.len(), lanes);
        let started = Instant::now();
        run_lanes(&spans, |_, s, _| {
            for _ in 0..ADMITS_PER_LANE {
                gate.admit();
            }
            Ok(vec![s])
        })
        .unwrap();
        let elapsed = started.elapsed();
        let admissions = lanes * ADMITS_PER_LANE;
        // The first admission is free (no schedule yet), so N admissions mandate N-1 gaps.
        let floor = gap * (admissions as u32 - 1);
        assert!(
            elapsed >= floor,
            "lanes={lanes}: {admissions} admissions took {elapsed:?}, under the {floor:?} the \
                 aggregate budget mandates — concurrency multiplied through the gate"
        );
    }
}

/// A gate that IDLED must not bank the unused slots into a burst: it resumes from now.
#[test]
fn an_idle_gate_resumes_from_now_instead_of_bursting() {
    let gate = LaneGate::new(spot_pacer());
    {
        // Put the schedule far in the past. `checked_sub`, not `-`: the MONOTONIC clock starts at
        // boot on Linux, so a bare subtraction panics on a machine up for under a minute.
        let mut g = gate.lock();
        g.next_slot = Some(Instant::now().checked_sub(Duration::from_secs(60)).unwrap_or_else(
            // No such instant on a very young clock: `now` exercises the same branch.
            Instant::now,
        ));
    }
    let before = Instant::now();
    gate.admit(); // must return immediately, and re-anchor the schedule
    let after = gate.lock().next_slot.unwrap();
    assert!(before.elapsed() < Duration::from_millis(50), "a past slot must not sleep");
    assert!(
        after >= before,
        "the schedule must re-anchor to now, or the next 1200 slots are all in the past"
    );
    assert!(after <= Instant::now() + Duration::from_millis(60));
}

/// The cooldown applies ONCE to the shared schedule, however many lanes report it.
#[test]
fn a_cooldown_pauses_every_lane_once_and_does_not_stack() {
    let cooldown = Duration::from_secs(10);
    let gate = LaneGate::new(spot_pacer());
    let before = Instant::now();
    // Six lanes all complete a page while the counter sits above the soft limit.
    for _ in 0..6 {
        gate.complete(Some(5_000), Duration::from_millis(280), 2_000, cooldown);
    }
    let slot = gate.lock().next_slot.expect("a cooldown sets the schedule");
    let pushed = slot - before;
    assert!(pushed >= cooldown, "the pause must be at least one cooldown: {pushed:?}");
    assert!(
        pushed < cooldown * 2,
        "six lanes must not stack six cooldowns into a minute: {pushed:?}"
    );
}

/// THE inflation check: under concurrency a counter delta is not one request's cost
/// (`Pacer::observe_absolute`'s doc). FAILS if the gate calls `Pacer::observe`: the first reading
/// would infer a 12-weight request (six lanes x weight 2) and stretch the gap 6x, cancelling six
/// lanes.
#[test]
fn interleaved_completions_never_move_the_gates_gap() {
    let gate = LaneGate::new(spot_pacer());
    let sequential_gap = gate.pacer().target_gap();
    assert!((sequential_gap.as_secs_f64() - 0.050).abs() < 1e-6, "{sequential_gap:?}");

    // A server-sampled counter under six lanes: each reading advances ~six requests' worth, and
    // one arrival in four is out of order (a backward step `Pacer::observe` reads as a roll).
    let mut counter = 200u64;
    for i in 0..24 {
        counter += 12; // 6 lanes x the real weight-2 page cost
        let read = if i % 4 == 3 { counter - 7 } else { counter };
        gate.complete(Some(read), Duration::from_millis(280), 4_800, Duration::from_secs(10));
    }
    // Below BOTH guards (soft limit 4800, window share 2400): this test is about the gap only.
    assert!(counter < 2_400);

    assert_eq!(
        gate.pacer().target_gap(),
        sequential_gap,
        "an unattributable delta must not re-pace the shared gate"
    );
    assert_eq!(
        gate.pacer().per_request_weight(),
        2.0,
        "the SEQUENTIAL probe's measurement is what the gate paces on"
    );
}

/// ...and the freeze must not disarm the brake: the ABSOLUTE value still fires the guard that
/// prevents a 429. FAILS on the last assertion if the gate calls `observe` (a 2398-weight
/// request inferred from the same reading).
#[test]
fn the_frozen_weight_leaves_the_absolute_guards_armed() {
    let gate = LaneGate::new(spot_pacer());
    gate.complete(Some(2_500), Duration::from_millis(280), 4_800, Duration::from_secs(10));
    assert!(gate.pacer().should_cool_down(), "2500 is past the 2400 window share");
    assert!(gate.lock().next_slot.is_some(), "...and every lane is paused for it");
    assert_eq!(gate.pacer().per_request_weight(), 2.0, "without that reading re-pacing the gate");
}

/// Below both guards a completion moves no schedule: the gate must not invent a pause.
#[test]
fn a_completion_under_both_guards_does_not_push_the_schedule() {
    let gate = LaneGate::new(spot_pacer());
    gate.complete(Some(10), Duration::from_millis(280), 2_000, Duration::from_secs(10));
    assert!(gate.lock().next_slot.is_none(), "no guard fired ⇒ no slot pushed");
    // ...and the measurement DID land, so the run can still persist its pace.
    assert!(gate.pacer().measured().is_some());
}

// ----- run_lanes: ordering, dedup, failure ---------------------------------------------------

/// One span runs INLINE, no thread, so a one-lane run IS the sequential path.
#[test]
fn a_single_span_runs_on_the_callers_own_thread() {
    let caller = std::thread::current().id();
    let seen = Mutex::new(None);
    let out = run_lanes(&[(0i64, 100i64)], |_, s, e| {
        *seen.lock().unwrap() = Some(std::thread::current().id());
        Ok(vec![(s, e)])
    })
    .unwrap();
    assert_eq!(out, vec![(0, 100)]);
    assert_eq!(seen.into_inner().unwrap(), Some(caller));
}

#[test]
fn rows_come_back_in_span_order_across_lanes() {
    let spans = split_range(0, 999, 4);
    // Each lane emits its span's start; concatenation must be ascending by span, not by
    // completion order (lane 3 sleeps longest and still lands last).
    let out = run_lanes(&spans, |_, s, _| {
        std::thread::sleep(Duration::from_millis((999 - s) as u64 / 100));
        Ok(vec![s])
    })
    .unwrap();
    assert_eq!(out, spans.iter().map(|s| s.0).collect::<Vec<_>>());
    assert!(out.windows(2).all(|w| w[0] < w[1]), "ascending: {out:?}");
}

#[test]
fn an_empty_span_list_never_calls_the_body() {
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let out: Vec<i64> = run_lanes(&[], |_, _, _| {
        calls.fetch_add(1, Ordering::Relaxed);
        Ok(vec![1i64])
    })
    .unwrap();
    assert!(out.is_empty());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

/// THE failure policy: one lane failing fails the WHOLE call (module doc).
#[test]
fn one_failed_lane_fails_the_whole_fetch_and_returns_no_rows() {
    let spans = split_range(0, 999, 4);
    let err = run_lanes(&spans, |_, s, _| {
        if s == spans[2].0 { Err("lane 2 blew up".to_string()) } else { Ok(vec![s]) }
    })
    .unwrap_err();
    assert_eq!(err, "lane 2 blew up", "the failure is reported, never silently shortened");
}

/// ...and the reported error is DETERMINISTIC: the lowest failing span, not the fastest thread.
#[test]
fn the_reported_error_is_the_lowest_failing_span_not_the_fastest() {
    let spans = split_range(0, 999, 4);
    for _ in 0..8 {
        let err = run_lanes(&spans, |_, s, _| {
            if s == spans[1].0 {
                std::thread::sleep(Duration::from_millis(20)); // finish LAST
                Err("span 1".to_string())
            } else if s == spans[3].0 {
                Err("span 3".to_string()) // finish FIRST
            } else {
                Ok(vec![s])
            }
        })
        .unwrap_err();
        assert_eq!(err, "span 1");
    }
}

/// A failure STOPS the siblings within one page per lane instead of running the window out.
#[test]
fn a_failure_aborts_the_remaining_lanes() {
    let spans = split_range(0, 9_999, 4);
    let pages = std::sync::atomic::AtomicUsize::new(0);
    let _ = run_lanes(&spans, |lanes, s, _| {
        if s == spans[0].0 {
            return Err("fail immediately".to_string());
        }
        // A long "pager": without the abort check this would run 1000 pages per lane.
        for _ in 0..1_000 {
            if lanes.aborted() {
                return Ok(vec![s]);
            }
            pages.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_micros(200));
        }
        Ok(vec![s])
    });
    assert!(
        pages.load(Ordering::Relaxed) < 3_000,
        "the siblings must stop early, not run all 3000 pages"
    );
}
