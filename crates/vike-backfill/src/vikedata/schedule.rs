//! The SCHEDULED shape: what a timer is allowed to ask for, and which ladders it is allowed to ask
//! for it on. Two tables and one piece of arithmetic — everything a periodic run of
//! `crates/vike-backfill/src/bin/vikedata_backfill.rs` decides that an operator must not decide per
//! firing.
//!
//! # ⚠ Why a THIRD window shape exists at all
//!
//! [`crate::vikedata::ingest::CohortWindow::trailing`] — the `--days N` shape — is the obvious thing
//! to put on a timer and it is the one shape that must never go on one. This store's idempotency is
//! BATCH-level on the commit key (`crates/vike-data/src/rec/cohort_rec.rs`'s `CohortFetch`), so:
//!
//!   * re-running the SAME command writes 0 rows (the key is spent), but
//!   * two OVERLAPPING windows are two DIFFERENT keys, and every hour they share lands TWICE.
//!
//! A trailing window slides with the clock. Fire `--days 30` at 04:00 and again at 05:00 and you
//! have two windows sharing 719 hours, two commit keys, and 719 hours of doubled notional in a
//! series whose whole purpose is a ratio. `crates/vike-backfill/src/vikedata/ingest_tests.rs`'s
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
/// commit key (`crates/vike-data/src/rec/cohort_rec.rs`'s `CohortFetch`).
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

#[path = "schedule_tests.rs"]
#[cfg(test)]
mod schedule_tests;
