//! `crate::runtime::bar_grid`: the bar an input should get next, the instant it is stale from, and the bar a
//! restart's tail fill falls in, under the FX week's calendar (both weeks the clocks change) and without one.

use super::bar_grid::{bar_at, next_bar, stale_from};
use vike_venues::session::FX_WEEK;

const H: i64 = 3_600_000;
const DAY: i64 = 24 * H;

/// Epoch-ms for a UTC instant.
fn ms(y: i64, mo: u32, d: u32, h: i64) -> i64 {
    vike_utility::calendar::days_from_civil(y, mo, d) * DAY + h * H
}

/// **A daily FX bar rolled at 17:00 New York is due at 17:00 New York in both weeks the clocks change**: the
/// Thursday bar's next is the week's first, Sunday 17:00 to Monday 17:00 New York, 21:00 UTC in March after the
/// change (the plain step would wait until 22:00) and 22:00 UTC in November after it (the plain step would want it
/// at 21:00). A venue that rolls its day at UTC midnight keeps the plain step.
#[test]
fn a_daily_fx_bar_is_due_at_17_new_york_across_both_changes_of_the_clocks() {
    let fx = Some(&FX_WEEK);
    // Thursday 2024-03-07 17:00 EST = 22:00 UTC; the clocks go forward on Sunday 2024-03-10.
    assert_eq!(next_bar(ms(2024, 3, 7, 22), DAY, fx), (ms(2024, 3, 10, 21), ms(2024, 3, 11, 21)));
    // Thursday 2024-10-31 17:00 EDT = 21:00 UTC; the clocks go back on Sunday 2024-11-03.
    assert_eq!(next_bar(ms(2024, 10, 31, 21), DAY, fx), (ms(2024, 11, 3, 22), ms(2024, 11, 4, 22)));
    // A day rolled at UTC midnight: Thursday's next is Friday's, the market open on it.
    assert_eq!(next_bar(ms(2024, 3, 7, 0), DAY, fx), (ms(2024, 3, 8, 0), ms(2024, 3, 9, 0)));
    // An hour bar over the weekend: Friday's last hour's next is the week's first, Sunday 17:00 New York.
    assert_eq!(next_bar(ms(2024, 3, 8, 21), H, fx), (ms(2024, 3, 10, 21), ms(2024, 3, 10, 22)));
    assert_eq!(next_bar(ms(2024, 11, 1, 20), H, fx), (ms(2024, 11, 3, 22), ms(2024, 11, 3, 23)));
    // No calendar: the plain step.
    assert_eq!(next_bar(ms(2024, 3, 8, 21), H, None), (ms(2024, 3, 8, 22), ms(2024, 3, 8, 23)));
}

/// **A daily FX input is stale only after two days of OPEN market** since its newest closed, in both change
/// weeks: the Thursday bar closing at Friday's 17:00 New York is stale from Tuesday 17:00 New York (the weekend
/// counts nothing, and the change moves nothing an hour early). Without a calendar: the plain two bars.
#[test]
fn a_daily_fx_input_is_stale_from_two_open_days_after_its_close_in_both_change_weeks() {
    let fx = Some(&FX_WEEK);
    assert_eq!(stale_from(ms(2024, 3, 8, 22), DAY, fx), ms(2024, 3, 12, 21));
    assert_eq!(stale_from(ms(2024, 11, 1, 21), DAY, fx), ms(2024, 11, 5, 22));
    // An hour input closing at the week's close: two open hours after Sunday's open.
    assert_eq!(stale_from(ms(2024, 1, 5, 22), H, fx), ms(2024, 1, 8, 0));
    // In the week, two hours.
    assert_eq!(stale_from(ms(2024, 1, 4, 11), H, fx), ms(2024, 1, 4, 13));
    assert_eq!(stale_from(ms(2024, 1, 5, 22), H, None), ms(2024, 1, 6, 0));
}

/// **A tail fill's bar counts only the periods its market traded in**: from an hour series' clock at Friday's
/// last hour (index 100), a fill at Monday 00:30 UTC falls in the third hour of the week (Sunday 22:00, 23:00,
/// Monday 00:00), index 103, where the plain count says 151. A fill inside the clock's own bar is the clock's
/// index; one before it too. Without a calendar the plain count stands.
#[test]
fn a_tail_fills_bar_counts_only_the_periods_its_market_traded_in() {
    let fx = Some(&FX_WEEK);
    let clock = (ms(2024, 1, 5, 21), 100);
    assert_eq!(bar_at(clock, H, fx, ms(2024, 1, 8, 0) + 30 * 60_000), 103);
    assert_eq!(bar_at(clock, H, None, ms(2024, 1, 8, 0) + 30 * 60_000), 151);
    assert_eq!(bar_at(clock, H, fx, ms(2024, 1, 5, 21) + 59 * 60_000), 100);
    assert_eq!(bar_at(clock, H, fx, ms(2024, 1, 5, 20)), 100);
    // In the week: one bar a period.
    assert_eq!(bar_at((ms(2024, 1, 4, 10), 7), H, fx, ms(2024, 1, 4, 13)), 10);
    // A daily series across the November change: Thursday's bar (index 5) to a fill on Monday 2024-11-04 at
    // 23:00 UTC, in the bar opened Monday 17:00 EST (22:00 UTC): Sunday's and Monday's bars, index 7.
    assert_eq!(bar_at((ms(2024, 10, 31, 21), 5), DAY, fx, ms(2024, 11, 4, 23)), 7);
}
