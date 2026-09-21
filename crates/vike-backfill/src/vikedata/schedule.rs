//! The SCHEDULED shape: what a timer is allowed to ask for, and which ladders it is allowed to ask
//! for it on. Two tables and one piece of arithmetic — everything a periodic run of
//! `crates/vike-backfill/src/bin/vikedata_backfill.rs` decides that an operator must not decide per
//! firing.
//!
//! # ⚠ Why a THIRD window shape exists at all
//!
//! [`crate::vikedata::ingest::CohortWindow::trailing`] — the `--days N` shape — is the obvious thing
//! to put on a timer and it is the one shape that must never go on one. This store's idempotency is
//! BATCH-level on the commit key (`crates/vike-data/src/cohort_rec.rs`'s `CohortFetch`), so:
//!
//!   * re-running the SAME command writes 0 rows (the key is spent), but
//!   * two OVERLAPPING windows are two DIFFERENT keys, and every hour they share lands TWICE.
//!
//! A trailing window slides with the clock. Fire `--days 30` at 04:00 and again at 05:00 and you
//! have two windows sharing 719 hours, two commit keys, and 719 hours of doubled notional in a
//! series whose whole purpose is a ratio. `crates/vike-backfill/src/vikedata/ingest.rs`'s
//! `two_overlapping_windows_are_two_batches_and_the_shared_hours_land_twice` pins that consequence;
//! this module is the shape that cannot express it.
//!
//! # The grid, and why it makes the CADENCE irrelevant
//!
//! A scheduled window is a whole UTC DAY, aligned to the day grid: `[D 00:00, D 23:00]` inclusive,
//! and only for days that have already ENDED. [`scheduled_windows`] derives them from the clock, so
//! the operator supplies no boundary at all — and the two properties that follow are what make the
//! schedule safe rather than merely documented:
//!
//!   1. **Two firings inside the same UTC day emit the IDENTICAL window list**, so a re-run is the
//!      store's own no-op. The timer may fire hourly, daily, on boot, twice by accident, or be
//!      re-run by hand during an incident: none of it can double a row.
//!   2. **Two different days emit ADJACENT windows** — day D ends at `D 23:00` and day D+1 starts at
//!      `D+1 00:00`, one hour later. No shared hour (so no doubling), and no skipped hour (so no
//!      gap).
//!
//! Together: the CADENCE of the timer cannot cause a double-write, which is the property a comment
//! saying "please use non-overlapping windows" does not have.
//!
//! ⚠ **The still-filling day is never fetched.** A window ending inside the current day would be
//! stored under a key naming that whole day, and that key is then SPENT — the honest fetch of the
//! complete day afterwards is a silent no-op forever. Same hazard
//! `crates/vike-backfill/src/vikedata/ingest.rs`'s `walk_pages` refuses at its page ceiling, reached
//! by the clock instead of by the cursor.
//!
//! ⚠ **The day is also the store's own partition unit**, so one scheduled batch writes exactly one
//! `date=` partition rather than straddling two (`crates/vike-data/src/store_kind.rs`'s
//! `STORE_KINDS` is the layout authority). That is a consequence of the grid, not the reason for it.
//!
//! # The look-back, and what it costs
//!
//! [`DEFAULT_CATCH_UP`] is 1: the run fetches the day that just ended. A larger `catch_up` re-emits
//! the preceding days as SEPARATE grid cells, so a firing missed to an outage self-heals — the days
//! already stored cost one request each and write 0 rows, and the missing one lands. It is bounded
//! by [`MAX_CATCH_UP`] because every re-attempt is a real request against a METERED endpoint: the
//! spend of one firing is `catch_up × ladders`, and nothing here can tell in advance which of those
//! requests will write anything.
//!
//! # ⚠ The POLLED SET IS NARROW, and each exclusion is a measured way to burn the meter for nothing
//!
//! The study client this collector was ported from was DEMAND-gated — an axis nobody selected sent
//! no request at all (`crates/vike-research/src/sources/api.rs`'s `CohortClient`, deleted with that
//! crate; `crates/vike-backfill/src/vikedata/mod.rs` states once why the citation stays). A
//! collector is
//! SUPPLY-driven: it pays for every ladder it is configured with, on every firing, forever, whether
//! or not anything reads the rows. So the polled set is an ENUMERATION with a reason on every row
//! that is not in it ([`POLLED`], [`DEFERRED`]), not "every ladder the API serves", and
//! [`every_ladder_the_endpoint_serves_is_polled_or_deferred_with_a_reason`] fails when a new
//! `(axis, grading)` pair joins the vocabulary without being classified either way.

use crate::error::CollectError;
use crate::vikedata::client::{Axis, CTX, Grading};
use crate::vikedata::ingest::CohortWindow;
use crate::vikedata::{SECS_PER_DAY, SECS_PER_HOUR};

/// How many complete UTC days one scheduled firing fetches when the operator names no number.
///
/// ONE: the day that just ended. Every extra day is a metered request that will usually write
/// nothing, so the default buys no self-healing and spends nothing — [`MAX_CATCH_UP`]'s doc carries
/// the trade for raising it.
pub const DEFAULT_CATCH_UP: u32 = 1;

/// The ceiling on the look-back, so a typo cannot spend a month of requests on every firing.
///
/// A scheduled firing issues `catch_up × POLLED.len()` requests whatever it finds, because a spent
/// commit key is only discoverable AFTER the fetch — the store answers "0 rows written", not "do
/// not bother". 31 is a month of daily cells: enough to ride out any outage an operator would still
/// be gap-filling by schedule rather than with one `--start`/`--end` command, and small enough that
/// the worst firing is bounded and boring.
pub const MAX_CATCH_UP: u32 = 31;

/// Whole UTC days since the epoch. `div_euclid` so a pre-epoch instant floors DOWN, the same reason
/// `crates/vike-backfill/src/vikedata/mod.rs`'s `floor_to_hour` uses `rem_euclid`.
fn utc_day(secs: i64) -> i64 {
    secs.div_euclid(SECS_PER_DAY)
}

/// The `catch_up` most recently COMPLETED UTC days, as day-aligned windows, OLDEST FIRST.
///
/// Oldest first because the store is append-only per batch and a reader following the run's log
/// should see the series being filled forward; nothing depends on the order for correctness, since
/// every window is its own commit key.
///
/// ⚠ This is the whole of the scheduled window policy. A caller cannot pass a boundary, so it cannot
/// pass an overlapping one — see the module doc for why that is the point rather than a convenience.
pub fn scheduled_windows(now_secs: i64, catch_up: u32) -> Result<Vec<CohortWindow>, CollectError> {
    if catch_up == 0 {
        return Err(CollectError::Fetch(format!(
            "{CTX}: --catch-up must be at least 1 — a scheduled run that fetches no window at all \
             exits 0 having collected nothing, which is the one failure a timer cannot show you."
        )));
    }
    if catch_up > MAX_CATCH_UP {
        return Err(CollectError::Fetch(format!(
            "{CTX}: --catch-up {catch_up} is past the {MAX_CATCH_UP}-day ceiling. Every day in the \
             look-back is a real request against a METERED endpoint on EVERY firing, spent whether \
             or not it writes a row. Fill a longer gap once with --start/--end instead."
        )));
    }
    // The day the clock is IN is still filling and is never fetched; the newest complete day is the
    // one before it.
    let newest_complete = utc_day(now_secs) - 1;
    (0..catch_up)
        .rev()
        .map(|back| {
            let start = (newest_complete - i64::from(back)) * SECS_PER_DAY;
            // INCLUSIVE at both ends, so the last hour of the cell is 23:00 and the next cell's
            // first hour is the following 00:00 — adjacent, sharing nothing.
            CohortWindow::range(start, start + SECS_PER_DAY - SECS_PER_HOUR)
        })
        .collect()
}

/// One `(axis, grading)` ladder — the pair that decides both the REQUEST and two segments of the
/// commit key (`crates/vike-data/src/cohort_rec.rs`'s `CohortFetch`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ladder {
    pub axis: Axis,
    pub grading: Grading,
}

impl Ladder {
    /// `axis/grading` — what the run's per-ladder report line is keyed on.
    pub fn label(self) -> String {
        format!("{}/{}", self.axis.as_str(), self.grading.echoed())
    }
}

/// The ladders a scheduled firing collects. ⚠ Adding a row here adds `catch_up` metered requests to
/// EVERY firing, forever — the module doc's supply-vs-demand note is the standing argument for
/// keeping this list at what is actually being read.
pub const POLLED: &[Ladder] = &[
    // The account-value ladder. The server's default grading, live to the current hour.
    Ladder { axis: Axis::Size, grading: Grading::Realized },
    // The realized-PnL ladder, always requested `labelBasis=point_in_time` and hard-failed if the
    // server does not echo it (`crates/vike-backfill/src/vikedata/parse.rs`'s `guard_label_basis`).
    Ladder { axis: Axis::Pnl, grading: Grading::Realized },
    // The unrealized rollup. ⚠ A non-default grading, so
    // `crates/vike-backfill/src/vikedata/ingest.rs`'s `guard_window_reaches_the_anchor` applies: if
    // that rollup stalls, this ladder FAILS the run rather than storing a short day under a key
    // naming the whole one. That is the intended behaviour — the day retries on the next firing
    // while `catch_up` still covers it — and it is the reason a stalled rollup is worth a
    // notification rather than a silent thin tape.
    Ladder { axis: Axis::Pnl, grading: Grading::Unrealized },
];

/// Every ladder the endpoint's vocabulary admits and this schedule deliberately does NOT poll, with
/// the measurement that put it here. A row is a claim about the WORLD, not a convenience exemption:
/// each one is a way a timer would burn a metered endpoint on every firing and collect nothing.
pub const DEFERRED: &[(Axis, Grading, &str)] = &[
    (
        Axis::Tier,
        Grading::Realized,
        "the TIER axis is NOT DEPLOYED — a tier-selecting fetch fails against the live endpoint \
         today, so scheduling it is one guaranteed failure per firing per asset, yielding nothing. \
         The axis stays in the client's vocabulary (`crates/vike-backfill/src/vikedata/client.rs`'s \
         `Axis`) because an ad-hoc `--axis tier` run is how anyone will find out it has shipped; it \
         joins POLLED once one does.",
    ),
    (
        Axis::Pnl,
        Grading::RealizedPit,
        "FROZEN upstream: nothing has written this grading since 2026-08-16 19:00 UTC, so every \
         firing buys zero NEW rows forever — and it fails LOUDLY as well as uselessly, because a \
         non-default grading whose newest hour falls short of the window end is refused by \
         `crates/vike-backfill/src/vikedata/ingest.rs`'s `guard_window_reaches_the_anchor`. \
         Scheduling it would page an operator every day about an upstream artefact that is behaving \
         exactly as expected. Gap-fill the frozen range ONCE with --start/--end if it is wanted.",
    ),
];

#[cfg(test)]
mod tests {
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
    /// discipline `crates/vike-model/src/venues.rs`'s `VENUES` applies to venues, applied to the
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
                let polled =
                    POLLED.iter().filter(|l| l.axis == axis && l.grading == grading).count();
                let deferred =
                    DEFERRED.iter().filter(|(a, g, _)| *a == axis && *g == grading).count();
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
        assert_eq!(
            Ladder { axis: Axis::Pnl, grading: Grading::Unrealized }.label(),
            "pnl/unrealized"
        );
        assert_eq!(
            Ladder { axis: Axis::Size, grading: Grading::Realized }.label(),
            "size/realized"
        );
    }
}
