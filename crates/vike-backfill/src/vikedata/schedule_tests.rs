use super::*;
// ⚠ Imported HERE rather than at module scope: nothing outside the tests calls it, and an
// `--all-targets` clippy compiles this lib without `cfg(test)`, where a module-level import
// would be an unused one and the merge gate is `-D warnings`.
use crate::vikedata::client::check_grading_applies;

/// 2026-08-24T00:00:00Z — a UTC midnight, so every offset below reads as a clock time.
const MIDNIGHT: i64 = 1_787_529_600;

fn windows(now: i64, catch_up: u32) -> Vec<CohortWindow> {
    scheduled_windows(now, catch_up).unwrap()
}

/// Do two INCLUSIVE windows share an hour?
fn overlap(a: &CohortWindow, b: &CohortWindow) -> bool {
    a.start_secs <= b.anchor_secs && b.start_secs <= a.anchor_secs
}

#[test]
fn a_scheduled_window_is_one_whole_utc_day_that_has_already_ended() {
    let w = windows(MIDNIGHT + 3 * SECS_PER_HOUR, 1);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].start_secs, MIDNIGHT - SECS_PER_DAY, "yesterday 00:00");
    assert_eq!(w[0].anchor_secs, MIDNIGHT - SECS_PER_HOUR, "…through yesterday 23:00");
    assert_eq!(w[0].days, 1);
    assert!(w[0].pinned, "a scheduled window is an EXACT range — the endpoint serves past it");
}

/// ⚠ THE property this module exists for. The still-filling day is never fetched, because a
/// partial day stored under a key naming the whole day spends that key forever.
#[test]
fn the_day_the_clock_is_in_is_never_fetched_however_late_in_it_the_run_happens() {
    for offset in [0, SECS_PER_HOUR, 12 * SECS_PER_HOUR, SECS_PER_DAY - 1] {
        let w = windows(MIDNIGHT + offset, 7);
        let newest = w.last().unwrap().anchor_secs;
        assert!(
            newest < MIDNIGHT,
            "a run at +{offset}s reached {newest}, inside the day it is running in"
        );
        assert_eq!(newest, MIDNIGHT - SECS_PER_HOUR, "…and stops at the last complete hour");
    }
}

/// Property 1: the cadence cannot double a row. Every firing inside one UTC day asks for the
/// SAME windows, so every firing after the first is the store's own no-op.
#[test]
fn every_firing_inside_one_utc_day_asks_for_the_identical_windows() {
    let first = windows(MIDNIGHT, 3);
    for offset in [1, 3_599, SECS_PER_HOUR, 13 * SECS_PER_HOUR + 42, SECS_PER_DAY - 1] {
        assert_eq!(
            windows(MIDNIGHT + offset, 3),
            first,
            "a firing at +{offset}s inside the same UTC day asked for a DIFFERENT window set — \
                 that is a second commit key over shared hours, i.e. doubled rows"
        );
    }
}

/// Property 2: consecutive days are adjacent — no shared hour (no doubling) and no missing hour
/// (no gap). Both halves in one assertion, because either alone is satisfiable by a bug.
#[test]
fn consecutive_days_are_adjacent_sharing_no_hour_and_skipping_none() {
    let today = windows(MIDNIGHT + 6 * SECS_PER_HOUR, 1);
    let tomorrow = windows(MIDNIGHT + SECS_PER_DAY + 6 * SECS_PER_HOUR, 1);
    assert!(!overlap(&today[0], &tomorrow[0]), "consecutive firings must share NO hour");
    assert_eq!(
        today[0].anchor_secs + SECS_PER_HOUR,
        tomorrow[0].start_secs,
        "…and must skip none either: the next window starts the hour after this one ends"
    );
}

/// ⚠ The naive shape this replaces, PINNED rather than described. `--days N` on a timer is two
/// keys over one tape, and the same clocks through the grid are two keys over two tapes.
#[test]
fn the_trailing_shape_a_timer_would_reach_for_overlaps_where_the_grid_does_not() {
    let (early, late) = (MIDNIGHT + 4 * SECS_PER_HOUR, MIDNIGHT + 5 * SECS_PER_HOUR);
    let (a, b) = (CohortWindow::trailing(30, early), CohortWindow::trailing(30, late));
    assert_ne!(a, b, "a trailing window SLIDES with the clock — two firings, two commit keys");
    assert!(overlap(&a, &b), "…and those two keys share all but one hour of their tape");

    let (ga, gb) = (windows(early, 1), windows(late, 1));
    assert_eq!(ga, gb, "the grid answers the same thing at both clocks");
}

#[test]
fn no_two_windows_of_one_firing_overlap_at_any_look_back() {
    for catch_up in 1..=MAX_CATCH_UP {
        let w = windows(MIDNIGHT + 7 * SECS_PER_HOUR, catch_up);
        assert_eq!(w.len(), catch_up as usize);
        for i in 0..w.len() {
            for j in (i + 1)..w.len() {
                assert!(
                    !overlap(&w[i], &w[j]),
                    "catch_up={catch_up}: windows {i} and {j} share an hour ({w:?})"
                );
            }
        }
    }
}

#[test]
fn the_look_back_comes_back_oldest_first_and_ends_on_the_newest_complete_day() {
    let w = windows(MIDNIGHT + 9 * SECS_PER_HOUR, 3);
    assert_eq!(w[0].start_secs, MIDNIGHT - 3 * SECS_PER_DAY);
    assert_eq!(w[1].start_secs, MIDNIGHT - 2 * SECS_PER_DAY);
    assert_eq!(w[2].start_secs, MIDNIGHT - SECS_PER_DAY);
    assert!(w.windows(2).all(|p| p[0].start_secs < p[1].start_secs), "oldest first");
}

#[test]
fn a_look_back_of_zero_or_past_the_ceiling_is_refused_rather_than_quietly_collecting_nothing() {
    let zero = scheduled_windows(MIDNIGHT, 0).unwrap_err().to_string();
    assert!(zero.contains("at least 1"), "{zero}");
    let over = scheduled_windows(MIDNIGHT, MAX_CATCH_UP + 1).unwrap_err().to_string();
    assert!(over.contains(&MAX_CATCH_UP.to_string()), "{over}");
    assert!(scheduled_windows(MIDNIGHT, MAX_CATCH_UP).is_ok(), "the ceiling itself is allowed");
}

/// `div_euclid`, not `/`: a pre-epoch clock must floor DOWN to a day boundary like any other,
/// or the grid stops being a grid on one side of 1970 and windows from the two sides can share
/// an hour.
#[test]
fn the_grid_is_still_day_aligned_before_the_epoch() {
    let w = windows(-1, 1);
    assert_eq!(w[0].start_secs, -2 * SECS_PER_DAY, "the day before the one containing -1s");
    assert_eq!(w[0].anchor_secs, -SECS_PER_DAY - SECS_PER_HOUR);
    assert_eq!(w[0].start_secs.rem_euclid(SECS_PER_DAY), 0, "still on a day boundary");
}

// ---- the polled set ----------------------------------------------------------------------

/// Every `(axis, grading)` the endpoint's vocabulary admits is classified. A new axis or a new
/// grading reddens this until somebody decides whether a timer should pay for it — the roster
/// discipline `crates/vike-model/src/venues/mod.rs`'s `VENUES` applies to venues, applied to the
/// only other axis this collector has.
#[test]
fn every_ladder_the_endpoint_serves_is_polled_or_deferred_with_a_reason() {
    for axis in [Axis::Size, Axis::Pnl, Axis::Tier] {
        for grading in [Grading::Realized, Grading::RealizedPit, Grading::Unrealized] {
            if check_grading_applies(axis, grading).is_err() {
                // Not a ladder at all: the client refuses it before the wire. Nothing to
                // classify, and a row for it would claim a request shape that cannot exist.
                continue;
            }
            let polled = POLLED.iter().filter(|l| l.axis == axis && l.grading == grading).count();
            let deferred = DEFERRED.iter().filter(|(a, g, _)| *a == axis && *g == grading).count();
            assert_eq!(
                polled + deferred,
                1,
                "{}/{} is in POLLED {polled} time(s) and DEFERRED {deferred} time(s) — every \
                     ladder the endpoint serves must be classified EXACTLY once. A supply-driven \
                     collector pays for what it polls on every firing forever; an unclassified \
                     ladder is that decision going unmade.",
                axis.as_str(),
                grading.echoed()
            );
        }
    }
}

/// A deferral is a claim about the world, so it must carry the measurement. Prose, checked
/// loosely — the point is that the row cannot be a bare "not yet".
#[test]
fn each_deferred_ladder_names_the_measurement_that_deferred_it() {
    for (axis, grading, why) in DEFERRED {
        assert!(
            why.len() > 80,
            "{}/{} defers with {why:?} — a deferral needs the reason, not a shrug",
            axis.as_str(),
            grading.echoed()
        );
    }
    let tier = DEFERRED.iter().find(|(a, _, _)| *a == Axis::Tier).expect("tier is deferred");
    assert!(tier.2.contains("NOT DEPLOYED"), "{}", tier.2);
    let pit = DEFERRED
        .iter()
        .find(|(_, g, _)| *g == Grading::RealizedPit)
        .expect("realized-pit is deferred");
    assert!(pit.2.contains("2026-08-16"), "the freeze date is the measurement: {}", pit.2);
}

/// …and the schedule actually honours its own table: nothing deferred is polled, and every
/// polled ladder is one the client will put on the wire.
#[test]
fn nothing_deferred_is_polled_and_every_polled_ladder_reaches_the_wire() {
    for l in POLLED {
        assert!(
            !DEFERRED.iter().any(|(a, g, _)| *a == l.axis && *g == l.grading),
            "{} is polled AND deferred",
            l.label()
        );
        check_grading_applies(l.axis, l.grading)
            .unwrap_or_else(|e| panic!("{} would be refused before the wire: {e}", l.label()));
    }
    assert!(!POLLED.is_empty(), "a schedule that polls nothing is a timer that does nothing");
}

#[test]
fn a_ladder_labels_itself_as_the_axis_and_the_grading_a_stored_row_carries() {
    assert_eq!(Ladder { axis: Axis::Pnl, grading: Grading::Unrealized }.label(), "pnl/unrealized");
    assert_eq!(Ladder { axis: Axis::Size, grading: Grading::Realized }.label(), "size/realized");
}
