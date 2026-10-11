//! The Calendar tool's data: the event and equity-row models and the ForexFactory weekly fetch.

use serde::Deserialize;
use std::time::Duration;

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

#[derive(Clone, Default)]
pub struct EarningsRow {
    pub date: String, // ISO day (group)
    pub symbol: String,
    pub hour: String, // Pre-mkt / After-hrs / Mid-day
    pub eps_est: String,
    pub eps_act: String,
    pub surprise: String, // signed %
}
#[derive(Clone, Default)]
pub struct DividendRow {
    pub date: String, // ex-date (group)
    pub symbol: String,
    pub pay_date: String,
    pub amount: String,
    pub yield_pct: String,
    pub freq: String,
}
#[derive(Clone, Default)]
pub struct IpoRow {
    pub date: String, // priced/expected day (group)
    pub symbol: String,
    pub company: String,
    pub exchange: String,
    pub price: String,
    pub shares: String,
    pub status: String,
}

// --- Calendar (ForexFactory weekly JSON) ---
#[derive(Deserialize)]
struct FfEvent {
    title: String,
    country: String,
    date: String,
    impact: String,
    #[serde(default)]
    forecast: String,
    #[serde(default)]
    previous: String,
    #[serde(default)]
    actual: String,
}
pub(super) fn fetch_calendar() -> Result<(String, Vec<CalEvent>), Box<dyn std::error::Error>> {
    use chrono::Datelike;
    // browser UA + 10s timeout — the bare request stalls on ForexFactory's CDN (Python uses a UA)
    let agent: ureq::Agent =
        ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
    let mut resp = agent
        .get("https://nfs.faireconomy.media/ff_calendar_thisweek.json")
        .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0 Safari/537.36")
        .header("Accept", "application/json, text/plain, */*")
        .call()?;
    let body = resp.body_mut().read_to_string()?;
    let evs: Vec<FfEvent> = serde_json::from_str(&body)?;
    const WD: [&str; 7] =
        ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    const WD3: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MO: [&str; 12] = [
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
    const MO3: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut bounds: Option<(chrono::NaiveDate, chrono::NaiveDate)> = None;
    let out: Vec<CalEvent> = evs
        .into_iter()
        .take(400) // the full week can run ~150+ events; 90 truncated Thu/Fri (count mismatch)
        .map(|e| {
            // date "2026-06-29T08:15:00-04:00" → convert to LOCAL tz (matches vike's _local
            // display-tz bucketing) so times + per-day grouping line up with the Python app.
            let dt = chrono::DateTime::parse_from_rfc3339(&e.date)
                .ok()
                .map(|d| d.with_timezone(&chrono::Local));
            if let Some(d) = dt {
                let nd = d.date_naive();
                bounds = Some(match bounds {
                    Some((lo, hi)) => (lo.min(nd), hi.max(nd)),
                    None => (nd, nd),
                });
            }
            let time = dt.map(|d| d.format("%H:%M").to_string()).unwrap_or_else(|| {
                e.date
                    .split('T')
                    .nth(1)
                    .map(|t| t.chars().take(5).collect())
                    .unwrap_or_else(|| "—".into())
            });
            let (day, day_short) = dt
                .map(|d| {
                    let wd = d.weekday().num_days_from_monday() as usize;
                    (
                        format!("{}, {} {}", WD[wd], MO[d.month0() as usize], d.day()),
                        format!("{} {}", WD3[wd], d.day()),
                    )
                })
                .unwrap_or_default();
            let (country_name, iso2) = currency_country(&e.country);
            CalEvent {
                time,
                importance: Importance::of(&e.impact),
                country: e.country,
                country_name: country_name.to_string(),
                iso2: iso2.to_string(),
                impact: e.impact,
                title: e.title,
                actual: e.actual,
                forecast: e.forecast,
                previous: e.previous,
                day,
                day_short,
                date_iso: dt.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(),
                ts_ms: dt.map(|d| d.timestamp_millis()).unwrap_or(0),
            }
        })
        .collect();
    // range label = the Mon→Sun week containing today (matches the day-strip cards AND Python's
    // CalendarSpace), NOT the min/max event-date span the feed happens to carry.
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let sunday = monday + chrono::Duration::days(6);
    let range = format!(
        "{} {} — {} {}, {}",
        MO3[monday.month0() as usize],
        monday.day(),
        MO3[sunday.month0() as usize],
        sunday.day(),
        sunday.year()
    );
    let _ = bounds; // (previously the data-span range; superseded by the Mon→Sun week)
    Ok((range, out))
}
