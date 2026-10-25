//! The Calendar tool's display model: the economic-event row the views draw, derived from the
//! datahub's RAW event (an instant and a currency) in the viewer's timezone, the currency-to-country
//! map behind the flag column, and the event-importance word.
//!
//! The rows come from the datahub (`super::tool_feeds`); nothing here fetches.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone};
use vike_datahub_client::tool_feeds::CalEvent as WireCalEvent;

#[derive(Clone, Default)]
pub struct CalEvent {
    pub time: String,
    pub country: String,      // currency code (USD/EUR/…)
    pub country_name: String, // full name (United States/European Union/…)
    pub iso2: String,         // flag code (us/eu/gb/jp/…)
    pub impact: String,
    pub importance: Importance, // drives the 3-bar glyph and the "High only" filter
    pub title: String,
    pub actual: String,
    pub forecast: String,
    pub previous: String,
    pub day: String,       // "Monday, June 29" — day-group header
    pub day_short: String, // "Mon 29" — day-strip card title
    pub date_iso: String,  // "2026-06-29" — day matching for the card strip
    pub ts_ms: i64,        // event time (epoch ms) — countdown + now-marker
}

const WEEKDAYS: [&str; 7] =
    ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const WEEKDAYS_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_SHORT: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

impl CalEvent {
    /// The row the views draw for the datahub's raw `e`, with every date label in `tz` (the
    /// viewer's LOCAL timezone: the clock time and the per-day grouping line up with the viewer's
    /// day, which is why the server sends an instant and no label). An event whose instant is
    /// unknown (`ts_ms` of 0) reads `—` and belongs to no day.
    pub(super) fn from_wire<Tz: TimeZone>(e: &WireCalEvent, tz: &Tz) -> Self
    where
        Tz::Offset: std::fmt::Display,
    {
        let dt: Option<DateTime<Tz>> = (e.ts_ms > 0)
            .then(|| DateTime::<chrono::Utc>::from_timestamp_millis(e.ts_ms))
            .flatten()
            .map(|d| d.with_timezone(tz));
        let (day, day_short) = dt
            .as_ref()
            .map(|d| {
                let wd = d.weekday().num_days_from_monday() as usize;
                (
                    format!("{}, {} {}", WEEKDAYS[wd], MONTHS[d.month0() as usize], d.day()),
                    format!("{} {}", WEEKDAYS_SHORT[wd], d.day()),
                )
            })
            .unwrap_or_default();
        let (country_name, iso2) = currency_country(&e.currency);
        Self {
            time: dt.as_ref().map_or_else(|| "—".into(), |d| d.format("%H:%M").to_string()),
            importance: Importance::of(&e.impact),
            country: e.currency.clone(),
            country_name: country_name.to_string(),
            iso2: iso2.to_string(),
            impact: e.impact.clone(),
            title: e.title.clone(),
            actual: e.actual.clone(),
            forecast: e.forecast.clone(),
            previous: e.previous.clone(),
            day,
            day_short,
            date_iso: dt.as_ref().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
            ts_ms: e.ts_ms,
        }
    }
}

/// The nav bar's range label for the Monday-to-Sunday week starting `monday`
/// (`Jun 29 — Jul 5, 2026`): the week containing today, which is what the day-strip cards show,
/// not the span of the dates the feed happens to carry.
pub(super) fn week_range_label(monday: NaiveDate) -> String {
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

/// currency code → (full country name, iso2 flag code). Ported from
/// data/calendar/taxonomy.py. Unknown → ("", "") so the UI falls back to the code chip.
pub fn currency_country(ccy: &str) -> (&'static str, &'static str) {
    match ccy {
        "USD" => ("United States", "us"),
        "EUR" => ("European Union", "eu"),
        "GBP" => ("United Kingdom", "gb"),
        "JPY" => ("Japan", "jp"),
        "AUD" => ("Australia", "au"),
        "NZD" => ("New Zealand", "nz"),
        "CAD" => ("Canada", "ca"),
        "CHF" => ("Switzerland", "ch"),
        "CNY" => ("Mainland China", "cn"),
        "INR" => ("India", "in"),
        "BRL" => ("Brazil", "br"),
        "ZAR" => ("South Africa", "za"),
        "KRW" => ("South Korea", "kr"),
        "MXN" => ("Mexico", "mx"),
        "RUB" => ("Russia", "ru"),
        "TRY" => ("Turkey", "tr"),
        "IDR" => ("Indonesia", "id"),
        "SAR" => ("Saudi Arabia", "sa"),
        "SGD" => ("Singapore", "sg"),
        "HKD" => ("Hong Kong", "hk"),
        "SEK" => ("Sweden", "se"),
        "NOK" => ("Norway", "no"),
        _ => ("", ""),
    }
}

/// How much a calendar event matters, from the feed's `impact` word: it drives the 3-bar glyph
/// (how many bars are lit and in what colour are the `importance` map's rows) and the "High only"
/// filter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Importance {
    High,
    Medium,
    #[default]
    Other,
}

impl Importance {
    /// The importance the feed's `impact` word names ("High", "Medium"; anything else is
    /// [`Importance::Other`]).
    pub fn of(impact: &str) -> Importance {
        match impact {
            "High" => Importance::High,
            "Medium" => Importance::Medium,
            _ => Importance::Other,
        }
    }
}
