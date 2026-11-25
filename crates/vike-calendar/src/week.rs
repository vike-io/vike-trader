//! The week the Calendar window shows, with the ONE rule that puts an instant on a day of it
//! ([`CalendarWeek`]), and the nav bar's range label ([`week_range_label`]). The economic-event row
//! the views draw is `crate::economic`'s.
//!
//! The rows come from the datahub (`vike_datahub_client::tool_feeds::poll`'s snapshot holds them
//! RAW); nothing here fetches.

use chrono::{Datelike, NaiveDate, NaiveDateTime};
use vike_datahub_client::tool_feeds::poll::monday_of;
use vike_ui_theme::time_zone::ThemeDisplayTimeZone;

const MONTHS_SHORT: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// **The Monday-to-Sunday week the Calendar window shows, in the zone it shows it in**, and the
/// ONE rule that puts an event's instant on a day of it ([`CalendarWeek::day_of`]). The nav bar's
/// range, the day-strip cards' counts, the selected card's filter and the event list's day groups
/// all read it, so none of them can put an event on another zone's day or outside the week.
///
/// ⚠ The list used to show EVERY event the feed held while the cards and the range named the
/// viewer's Monday-to-Sunday week: the ForexFactory week starts on Sunday in New York, so its first
/// events (a Monday-morning New Zealand print is 23:30 on Sunday in eastern Europe) were listed
/// under the Sunday BEFORE the week the range named.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CalendarWeek {
    /// The week's Monday, in `zone`.
    pub(crate) monday: NaiveDate,
    zone: ThemeDisplayTimeZone,
}

impl CalendarWeek {
    /// The week holding the instant `now_ms` in `zone`.
    pub(crate) fn holding(now_ms: i64, zone: ThemeDisplayTimeZone) -> Self {
        let today = vike_ui_theme::time_zone::to_naive(now_ms, zone).unwrap_or_default().date();
        Self { monday: monday_of(today), zone }
    }

    /// The week `weeks` after this one (before it, for a negative count), in the same zone.
    pub(crate) fn weeks_later(self, weeks: i32) -> Self {
        Self { monday: self.monday + chrono::Duration::weeks(i64::from(weeks)), ..self }
    }

    /// The wall-clock time of `timestamp_ms` in the week's zone; `None` for an unknown instant (`0`).
    pub(crate) fn wall_clock(&self, timestamp_ms: i64) -> Option<NaiveDateTime> {
        (timestamp_ms > 0)
            .then(|| vike_ui_theme::time_zone::to_naive(timestamp_ms, self.zone))
            .flatten()
    }

    /// The day of the week `timestamp_ms` falls on in the week's zone, Monday `0` to Sunday `6`;
    /// `None` outside the week and for an unknown instant.
    pub(crate) fn day_of(&self, timestamp_ms: i64) -> Option<usize> {
        let days = (self.wall_clock(timestamp_ms)?.date() - self.monday).num_days();
        usize::try_from(days).ok().filter(|day| *day < 7)
    }

    /// The date of the week's `day` (Monday `0`).
    pub(crate) fn date(&self, day: usize) -> NaiveDate {
        self.monday + chrono::Days::new(day as u64)
    }
}

/// The nav bar's range label for the Monday-to-Sunday week starting `monday`
/// (`Jun 29 — Jul 5, 2026`): the week the day-strip cards show ([`CalendarWeek`]), not the span of
/// the dates the feed happens to carry.
pub(crate) fn week_range_label(monday: NaiveDate) -> String {
    let sunday = monday + chrono::Duration::days(6);
    format!(
        "{} {} — {} {}, {}",
        MONTHS_SHORT[monday.month0() as usize],
        monday.day(),
        MONTHS_SHORT[sunday.month0() as usize],
        sunday.day(),
        sunday.year()
    )
}

/// The display layer over the poll's raw rows: one event's display row, and the week that puts an
/// instant on a day in the window's zone.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::economic::{CalendarEconomicEventView, CalendarEconomicImportance};
    use vike_datahub_client::tool_feeds::CalendarEconomicEvent;

    const NOW_MS: i64 = 1_760_000_000_000;

    fn event(timestamp_ms: i64) -> CalendarEconomicEvent {
        CalendarEconomicEvent {
            timestamp_ms,
            currency: "USD".into(),
            title: "CPI".into(),
            impact: "High".into(),
            forecast: "0.3%".into(),
            previous: "0.2%".into(),
            actual: String::new(),
        }
    }

    #[test]
    fn a_raw_event_becomes_its_display_row() {
        let row = CalendarEconomicEventView::from_wire(&event(NOW_MS));
        assert_eq!(row.currency, "USD");
        assert_eq!(row.country_name, "United States");
        assert_eq!(row.iso2, "us");
        assert_eq!(row.importance, CalendarEconomicImportance::High);
        assert_eq!(row.timestamp_ms, NOW_MS);
    }

    #[test]
    fn an_event_instant_becomes_a_clock_time_and_a_day_of_the_week_in_the_zone() {
        // 2026-10-14 12:30:00 UTC, viewed from UTC+3 (Moscow) and from UTC-5 (Bogota): neither keeps
        // summer time.
        let timestamp = NaiveDate::from_ymd_opt(2026, 10, 14)
            .unwrap()
            .and_hms_opt(12, 30, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        assert_eq!(CalendarEconomicEventView::from_wire(&event(timestamp)).timestamp_ms, timestamp);
        let monday = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
        let east = CalendarWeek::holding(timestamp, ThemeDisplayTimeZone::parse("Europe/Moscow"));
        assert_eq!(east.monday, monday);
        assert_eq!(east.wall_clock(timestamp).unwrap().format("%H:%M").to_string(), "15:30");
        assert_eq!(east.day_of(timestamp), Some(2), "Wednesday");
        assert_eq!(east.date(2), NaiveDate::from_ymd_opt(2026, 10, 14).unwrap());

        // the same instant less 13 hours is the PREVIOUS day for a viewer far enough west
        let west = CalendarWeek::holding(timestamp, ThemeDisplayTimeZone::parse("America/Bogota"));
        let earlier = timestamp - 13 * 3600 * 1000;
        assert_eq!(west.monday, monday);
        assert_eq!(west.wall_clock(earlier).unwrap().format("%H:%M").to_string(), "18:30");
        assert_eq!(west.day_of(earlier), Some(1), "Tuesday");
    }

    #[test]
    fn an_event_with_no_instant_belongs_to_no_day() {
        let week = CalendarWeek::holding(NOW_MS, ThemeDisplayTimeZone::Utc);
        assert_eq!(week.wall_clock(0), None);
        assert_eq!(week.day_of(0), None);
    }

    #[test]
    fn the_range_label_names_the_monday_to_sunday_week() {
        let monday = NaiveDate::from_ymd_opt(2026, 6, 29).unwrap();
        assert_eq!(week_range_label(monday), "Jun 29 — Jul 5, 2026");
        assert_eq!(
            monday_of(NaiveDate::from_ymd_opt(2026, 7, 5).unwrap()),
            monday,
            "Sunday is the week's last day"
        );
        assert_eq!(monday_of(monday), monday);
    }
}
