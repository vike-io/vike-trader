//! UTC calendar reads strategy files share. Copied from the strategy gallery (plan 2b, C3).

use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_utility::calendar::{civil_from_days, days_from_civil};

/// `(hour, minute)` of the bar's open in UTC.
pub fn clock(bar: &Bar) -> (i64, i64) {
    let minute = bar.timestamp.rem_euclid(MS_PER_DAY) / 60_000;
    (minute / 60, minute % 60)
}

/// The UTC day of the month `ts` lies in.
pub fn day_of_month(ts: i64) -> i64 {
    i64::from(civil_from_days(ts.div_euclid(MS_PER_DAY)).2)
}

/// `timestamp(year, month, day, hour, minute)` on a UTC exchange, in ms.
pub fn timestamp(year: i64, month: u32, day: u32, hour: i64, minute: i64) -> i64 {
    days_from_civil(year, month, day) * MS_PER_DAY + (hour * 60 + minute) * 60_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_of_the_month_and_a_timestamp() {
        let jan_2025 = 1_735_689_600_000;
        let feb_2024 = 1_706_745_600_000;
        assert_eq!((day_of_month(jan_2025), day_of_month(feb_2024 + 28 * MS_PER_DAY)), (1, 29));
        assert_eq!(timestamp(2025, 1, 1, 0, 0), jan_2025);
        assert_eq!(timestamp(2024, 2, 29, 12, 30), feb_2024 + 28 * MS_PER_DAY + 45_000_000);
    }
}
