//! The Economic page's display model: the economic-event row the views draw (the datahub's RAW
//! event: an instant and a currency), the currency-to-country map behind the flag column, and the
//! event-importance word; with the page's table (`table`) and its two drawn glyphs and bundled flag
//! PNGs (`flag`).
//!
//! The rows come from the datahub (`vike_datahub_client::tool_feeds::poll`'s snapshot holds them
//! RAW); nothing here fetches.

pub(crate) mod flag;
pub(crate) mod table;

use vike_datahub_client::tool_feeds::CalendarEconomicEvent;

/// One economic event as the views draw it. It carries the INSTANT, never a date or a clock time:
/// those belong to the zone the window draws in (`CalendarWeek`).
#[derive(Clone, Default)]
pub(crate) struct CalendarEconomicEventView {
    pub currency: String,                       // currency code (USD/EUR/…)
    pub(crate) country_name: String,            // full name (United States/European Union/…)
    pub(crate) iso2: String,                    // flag code (us/eu/gb/jp/…)
    pub importance: CalendarEconomicImportance, // drives the 3-bar glyph and the "High only" filter
    pub title: String,
    pub actual: String,
    pub forecast: String,
    pub previous: String,
    pub timestamp_ms: i64, // event time (epoch ms, 0 = unknown): its day, clock time and countdown
}

impl CalendarEconomicEventView {
    /// The row the views draw for the datahub's raw `e`. The server sends an instant and no label,
    /// so the day and the clock time are derived where they are drawn, in the window's zone.
    pub fn from_wire(e: &CalendarEconomicEvent) -> Self {
        let (country_name, iso2) = currency_country(&e.currency);
        Self {
            importance: CalendarEconomicImportance::of(&e.impact),
            currency: e.currency.clone(),
            country_name: country_name.to_string(),
            iso2: iso2.to_string(),
            title: e.title.clone(),
            actual: e.actual.clone(),
            forecast: e.forecast.clone(),
            previous: e.previous.clone(),
            timestamp_ms: e.timestamp_ms,
        }
    }
}

/// currency code → (full country name, iso2 flag code). Ported from
/// data/calendar/taxonomy.py. Unknown → ("", "") so the UI falls back to the code chip.
pub(crate) fn currency_country(currency: &str) -> (&'static str, &'static str) {
    match currency {
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
pub enum CalendarEconomicImportance {
    High,
    Medium,
    #[default]
    Other,
}

impl CalendarEconomicImportance {
    /// The importance the feed's `impact` word names ("High", "Medium"; anything else is
    /// [`CalendarEconomicImportance::Other`]).
    pub fn of(impact: &str) -> CalendarEconomicImportance {
        match impact {
            "High" => CalendarEconomicImportance::High,
            "Medium" => CalendarEconomicImportance::Medium,
            _ => CalendarEconomicImportance::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CalendarEconomicImportance;

    /// The feed's impact word names the importance of that name; anything else is the last.
    #[test]
    fn the_feeds_impact_word_names_its_importance() {
        assert_eq!(CalendarEconomicImportance::of("High"), CalendarEconomicImportance::High);
        assert_eq!(CalendarEconomicImportance::of("Medium"), CalendarEconomicImportance::Medium);
        for other in ["Low", "Holiday", "", "high"] {
            assert_eq!(
                CalendarEconomicImportance::of(other),
                CalendarEconomicImportance::Other,
                "{other:?}"
            );
        }
        assert_eq!(CalendarEconomicImportance::default(), CalendarEconomicImportance::Other);
    }
}
