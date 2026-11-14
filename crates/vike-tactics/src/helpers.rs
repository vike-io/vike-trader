//! The arithmetic on missing values, and the counts, crosses and clocks two or more strategy files share, each written once
//! here. Copied from the strategy gallery (plan 2b, C3); the two that read an indicator (`FlexiMedian`, `parabolic_sar`) are
//! `vike_indicators::strategy_kit`'s, as this crate names no indicator (decision 0269), and so are the NaN-propagating
//! `max` and `min`, whose one copy the indicators' own `Qqe` folds with.

use std::collections::VecDeque;

use vike_model::MS_PER_DAY;
use vike_model::time::utc_weekday;
use vike_utility::calendar::{civil_from_days, days_from_civil};

/// The reference's `nz`: a missing value read as 0.
pub fn nan_to_zero(x: f64) -> f64 {
    if x.is_nan() { 0.0 } else { x }
}

/// A number the reference reads as true: there, and not zero.
pub fn truthy(x: f64) -> bool {
    !x.is_nan() && x != 0.0
}

/// A level the rule leaves missing as `None`, so it places no order.
pub fn known(x: f64) -> Option<f64> {
    (!x.is_nan()).then_some(x)
}

/// `x[i]` of a ring kept newest first, NaN past what it holds.
pub fn back(ring: &VecDeque<f64>, i: usize) -> f64 {
    ring.get(i).copied().unwrap_or(f64::NAN)
}

/// Puts `x` at the front of a ring kept newest first, `keep` long.
pub fn push(ring: &mut VecDeque<f64>, x: f64, keep: usize) {
    ring.push_front(x);
    ring.truncate(keep);
}

/// The reference's bars-since: 0 on a bar the condition holds, one more on each bar after, `None` before it first held.
#[derive(Default)]
pub struct BarsSince(pub Option<usize>);

impl BarsSince {
    pub fn step(&mut self, now: bool) -> Option<usize> {
        self.0 = if now { Some(0) } else { self.0.map(|n| n + 1) };
        self.0
    }
}

/// `a < b` of two bars-since counts, false while either is missing.
pub fn fewer(a: Option<usize>, b: Option<usize>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a < b)
}

/// The reference's cross of `a` over (or under) `b`: strictly past it now, and at or short of it on the last bar both were
/// there. A bar with either NaN fires nothing and leaves that memory alone, so a line shown only while its trend lasts can be
/// crossed on the first bar it comes back, against the bar it was last seen.
#[derive(Default)]
pub struct CrossDefined {
    was_short: bool,
}

impl CrossDefined {
    /// `up`: a cross over; otherwise a cross under.
    pub fn step(&mut self, a: f64, b: f64, up: bool) -> bool {
        if a.is_nan() || b.is_nan() {
            return false;
        }
        let (past, short) = if up { (a > b, a <= b) } else { (a < b, a >= b) };
        let crossed = past && self.was_short;
        self.was_short = short;
        crossed
    }
}

/// The day of the week of a UTC instant, Monday 0 to Sunday 6: `vike_model::time::utc_weekday`, as the rules' `i64`.
pub fn weekday(ts: i64) -> i64 {
    i64::from(utc_weekday(ts))
}

/// The `n`-th Sunday (1-based) of a month, as a day count.
fn sunday(y: i64, m: u32, n: i64) -> i64 {
    let first = days_from_civil(y, m, 1);
    let weekday = (first + 4).rem_euclid(7); // 0 is Sunday
    first + (7 - weekday) % 7 + 7 * (n - 1)
}

/// Minutes into the local day of a US time zone `standard` hours from UTC (-5 New York, -6 Chicago), with the US daylight
/// rule: from 02:00 on March's second Sunday to 02:00 on November's first.
pub fn us_minute(ts: i64, standard: i64) -> i64 {
    let y = civil_from_days(ts.div_euclid(MS_PER_DAY)).0;
    let hour = 3_600_000;
    let start = sunday(y, 3, 2) * MS_PER_DAY + (2 - standard) * hour;
    let end = sunday(y, 11, 1) * MS_PER_DAY + (2 - standard - 1) * hour;
    let offset = standard + i64::from(ts >= start && ts < end);
    (ts + offset * hour).rem_euclid(MS_PER_DAY) / 60_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_arithmetic_reads_as_its_rules_do() {
        assert_eq!(nan_to_zero(f64::NAN), 0.0);
        let mut since = BarsSince::default();
        let got: Vec<Option<usize>> =
            [false, true, false, false, true].map(|c| since.step(c)).into();
        assert_eq!(got, [None, Some(0), Some(1), Some(2), Some(0)]);
        assert!(fewer(Some(0), Some(1)) && !fewer(None, Some(1)) && !fewer(Some(1), Some(1)));
        // 2025-01-02 15:00 UTC is 10:00 EST, 2025-07-01 14:00 UTC 10:00 EDT, 2025-11-02 05:30 UTC still 01:30 EDT.
        for ts in [1_735_830_000_000, 1_751_378_400_000] {
            assert_eq!(us_minute(ts, -5), 600, "{ts}");
        }
        assert_eq!(us_minute(1_762_061_400_000, -5), 90);
        assert_eq!(weekday(0), 3, "1970-01-01 was a Thursday");
    }
}
