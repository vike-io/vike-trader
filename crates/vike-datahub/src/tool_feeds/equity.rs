//! **The equity calendars: Finnhub earnings, FMP dividends and Nasdaq IPOs, for ONE week.** The
//! parsing and the formatting of every cell are the desktop's (`vike-app-core`'s former
//! `tools/fetchers.rs`), moved rather than reinvented; what differs is WHO fetches (this process, on
//! its own schedule, for every desktop) and what a failure or a missing key does.
//!
//! # The week
//!
//! A week is its Monday, an ISO date the server's door has already checked
//! (`vike_datahub_client::tool_feeds::validate_equity_week`): a Monday within a week of the server's
//! current week. Earnings and dividends are ONE range call each, `from=<Monday>&to=<Sunday>`. The IPO
//! feed is monthly, so a week costs one Nasdaq call, or two when it straddles a month; its rows are
//! the whole month(s), as the desktop's were (the IPO window groups them by day itself).
//!
//! # The two keys
//!
//! `FINNHUB_API_KEY` and `FMP_API_KEY` are read from THIS box's credential store on every poll cycle
//! through [`super::KeyReader`] (a one-name scoped read: decision 0097's pattern), spent on the
//! server's schedule and never on a request, and put on the wire in exactly one place each — Finnhub's
//! in its documented `X-Finnhub-Token` header, FMP's in the `apikey` query parameter its API takes.
//! The value reaches no log line, no error string, no reply and no `Debug` ([`super::Secret`]). An
//! ABSENT key makes the feed `NotConfigured` (it is a feed state, not an error); an UNREADABLE store
//! is a failed poll that keeps the last good rows.
//!
//! # ⚠ A body that is not the provider's shape is a FAILED poll
//!
//! The desktop turned a 200 whose body lacked `earningsCalendar` into an empty list, and replaced the
//! rows with it. A body without the expected array is an `Err` here, so the poll keeps what it has.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use chrono::{Duration as ChronoDuration, NaiveDate};
use serde_json::Value;
use vike_datahub_client::tool_feeds::{DividendRow, EarningsRow, IpoRow};

use super::health::ProviderHealth;
use super::worker::PollResult;
use super::{FINNHUB_API_KEY, FMP_API_KEY, FeedHttp, HttpGet, KeyRead, KeyReader};

/// The Finnhub and FMP request timeout (the desktop's shared agent's).
pub const KEYED_TIMEOUT: Duration = Duration::from_secs(12);

/// The Nasdaq request timeout (the desktop gave it a tighter agent so a stuck month fails fast).
pub const NASDAQ_TIMEOUT: Duration = Duration::from_secs(8);

/// How many months of IPO rows the poller remembers across polls (this week's two months, the
/// neighbouring weeks' two more).
const IPO_MONTHS_KEPT: usize = 6;

fn parse_day(iso: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(iso, "%Y-%m-%d").ok()
}

/// The Sunday of the week whose Monday is `week_start`, ISO.
pub fn week_end(week_start: &str) -> Option<String> {
    let monday = parse_day(week_start)?;
    Some((monday + ChronoDuration::days(6)).format("%Y-%m-%d").to_string())
}

/// The `YYYY-MM` months a week touches: one, or two when it straddles a month end; ascending.
pub fn ipo_months(week_start: &str) -> Vec<String> {
    let Some(monday) = parse_day(week_start) else { return Vec::new() };
    let mut months = vec![monday.format("%Y-%m").to_string()];
    let last = (monday + ChronoDuration::days(6)).format("%Y-%m").to_string();
    if months[0] != last {
        months.push(last);
    }
    months
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// **Parse Finnhub's earnings-calendar body.** `Err` when the body is not JSON or has no
/// `earningsCalendar` array; a row with no date is skipped.
pub fn parse_finnhub(body: &str) -> Result<Vec<EarningsRow>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let rows = v["earningsCalendar"].as_array().ok_or("no earningsCalendar array in the body")?;
    let mut out = Vec::new();
    for r in rows {
        let date = text(&r["date"]);
        if date.is_empty() {
            continue;
        }
        let hour = match r["hour"].as_str().unwrap_or("") {
            "bmo" => "Pre-mkt",
            "amc" => "After-hrs",
            "dmh" => "Mid-day",
            _ => "",
        };
        let est = r["epsEstimate"].as_f64();
        let act = r["epsActual"].as_f64();
        let surprise = match (act, est) {
            (Some(a), Some(e)) if e.abs() > 1e-9 => format!("{:+.1}%", (a - e) / e.abs() * 100.0),
            _ => String::new(),
        };
        out.push(EarningsRow {
            date,
            symbol: text(&r["symbol"]),
            hour: hour.to_string(),
            eps_est: est.map(|x| format!("{x:.2}")).unwrap_or_default(),
            eps_act: act.map(|x| format!("{x:.2}")).unwrap_or_default(),
            surprise,
        });
    }
    Ok(out)
}

/// **Parse FMP's dividends-calendar body.** `Err` when the body is not a JSON array; a row with no
/// date is skipped.
pub fn parse_fmp(body: &str) -> Result<Vec<DividendRow>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    let rows = v.as_array().ok_or("the body is not a JSON array")?;
    let mut out = Vec::new();
    for r in rows {
        let date = text(&r["date"]);
        if date.is_empty() {
            continue;
        }
        out.push(DividendRow {
            date,
            symbol: text(&r["symbol"]),
            pay_date: text(&r["paymentDate"]),
            amount: r["dividend"].as_f64().map(|x| format!("{x:.2} $")).unwrap_or_default(),
            yield_pct: r["yield"].as_f64().map(|x| format!("{x:.2}%")).unwrap_or_default(),
            freq: text(&r["frequency"]),
        });
    }
    Ok(out)
}

/// **Parse one month of Nasdaq's IPO calendar**: the priced rows, then the upcoming ones. `Err` when
/// the body is not JSON or carries no `data` object (Nasdaq's refusal shape); a month with no IPOs
/// is `Ok(empty)`.
pub fn parse_nasdaq_ipo(body: &str) -> Result<Vec<IpoRow>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("not JSON: {e}"))?;
    if !v["data"].is_object() {
        return Err("no data object in the body".to_string());
    }
    let mut out = Vec::new();
    for (rows, status) in [
        (&v["data"]["priced"]["rows"], "Priced"),
        (&v["data"]["upcoming"]["upcomingTable"]["rows"], "Upcoming"),
    ] {
        let Some(rows) = rows.as_array() else { continue };
        for r in rows {
            let raw =
                r["pricedDate"].as_str().or_else(|| r["expectedPriceDate"].as_str()).unwrap_or("");
            let parts: Vec<&str> = raw.split('/').collect(); // "MM/DD/YYYY"
            let date = if parts.len() == 3 {
                format!("{}-{:0>2}-{:0>2}", parts[2], parts[0], parts[1])
            } else {
                String::new()
            };
            out.push(IpoRow {
                date,
                symbol: text(&r["proposedTickerSymbol"]),
                company: text(&r["companyName"]),
                exchange: text(&r["proposedExchange"]),
                price: text(&r["proposedSharePrice"]),
                shares: text(&r["sharesOffered"]),
                status: status.to_string(),
            });
        }
    }
    Ok(out)
}

/// Read `name`'s key for a keyed poll, or the poll result that says why there is none.
fn key_or_result<T>(
    keys: &dyn KeyReader,
    name: &'static str,
) -> Result<super::Secret, PollResult<T>> {
    match keys.read(name) {
        KeyRead::Present(secret) => Ok(secret),
        KeyRead::Absent => Err(PollResult::NotConfigured { key_name: name }),
        // The reader already logged why; the rows stay.
        KeyRead::Unreadable => Err(PollResult::Failed { total: 1 }),
    }
}

/// **One poll of Finnhub's earnings for `week_start`'s week.**
pub fn poll_earnings(
    http: &dyn FeedHttp,
    keys: &dyn KeyReader,
    health: &ProviderHealth,
    base: &str,
    week_start: &str,
) -> PollResult<EarningsRow> {
    let key = match key_or_result(keys, FINNHUB_API_KEY) {
        Ok(key) => key,
        Err(result) => return result,
    };
    let Some(to) = week_end(week_start) else { return PollResult::Failed { total: 1 } };
    let url = format!("{base}/api/v1/calendar/earnings?from={week_start}&to={to}");
    let request = HttpGet::new(url, KEYED_TIMEOUT)
        .accepting_json()
        .with_header("X-Finnhub-Token", key.expose());
    match http.get(&request).and_then(|body| parse_finnhub(&body)) {
        Ok(rows) => {
            health.record("earnings", "Finnhub", week_start, Ok(()));
            PollResult::Rows { rows, ok: 1, total: 1 }
        }
        Err(why) => {
            health.record("earnings", "Finnhub", week_start, Err(why.as_str()));
            PollResult::Failed { total: 1 }
        }
    }
}

/// **One poll of FMP's dividends for `week_start`'s week.**
pub fn poll_dividends(
    http: &dyn FeedHttp,
    keys: &dyn KeyReader,
    health: &ProviderHealth,
    base: &str,
    week_start: &str,
) -> PollResult<DividendRow> {
    let key = match key_or_result(keys, FMP_API_KEY) {
        Ok(key) => key,
        Err(result) => return result,
    };
    let Some(to) = week_end(week_start) else { return PollResult::Failed { total: 1 } };
    let url = format!(
        "{base}/stable/dividends-calendar?from={week_start}&to={to}&apikey={}",
        key.expose()
    );
    let request = HttpGet::new(url, KEYED_TIMEOUT).accepting_json();
    match http.get(&request).and_then(|body| parse_fmp(&body)) {
        Ok(rows) => {
            health.record("dividends", "FMP", week_start, Ok(()));
            PollResult::Rows { rows, ok: 1, total: 1 }
        }
        Err(why) => {
            health.record("dividends", "FMP", week_start, Err(why.as_str()));
            PollResult::Failed { total: 1 }
        }
    }
}

/// The IPO poll: the transport, the base URL, and each month's last good rows — a week that
/// straddles a month end whose second call fails still serves the first month's fresh rows AND the
/// second month's previous ones.
pub struct IpoPoller {
    base: String,
    months: Mutex<BTreeMap<String, Vec<IpoRow>>>,
    health: ProviderHealth,
}

impl IpoPoller {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            months: Mutex::new(BTreeMap::new()),
            health: ProviderHealth::new(),
        }
    }

    /// **One poll of Nasdaq's IPO calendar for `week_start`'s week**: one call per month the week
    /// touches, the rows of those months in month order.
    pub fn poll(&self, http: &dyn FeedHttp, week_start: &str) -> PollResult<IpoRow> {
        let wanted = ipo_months(week_start);
        let total = u32::try_from(wanted.len()).unwrap_or(u32::MAX);
        let mut ok = 0u32;
        let mut months = self.months.lock().unwrap_or_else(PoisonError::into_inner);
        for ym in &wanted {
            let request =
                HttpGet::new(format!("{}/api/ipo/calendar?date={ym}", self.base), NASDAQ_TIMEOUT)
                    .with_browser_headers();
            match http.get(&request).and_then(|body| parse_nasdaq_ipo(&body)) {
                Ok(rows) => {
                    ok += 1;
                    months.insert(ym.clone(), rows);
                    self.health.record("ipos", "Nasdaq", ym, Ok(()));
                }
                Err(why) => self.health.record("ipos", "Nasdaq", ym, Err(why.as_str())),
            }
        }
        while months.len() > IPO_MONTHS_KEPT {
            months.pop_first();
        }
        if ok == 0 {
            return PollResult::Failed { total };
        }
        let rows = wanted.iter().filter_map(|ym| months.get(ym)).flatten().cloned().collect();
        PollResult::Rows { rows, ok, total }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Datelike;

    use super::*;
    use crate::tool_feeds::Secret;

    const FINNHUB: &str = r#"{"earningsCalendar":[
      {"date":"2026-10-13","symbol":"JPM","hour":"bmo","epsEstimate":4.5,"epsActual":4.8},
      {"date":"2026-10-14","symbol":"NFLX","hour":"amc","epsEstimate":5.0},
      {"date":"2026-10-15","symbol":"XYZ","hour":"dmh","epsEstimate":0.0,"epsActual":0.1},
      {"date":"","symbol":"NODATE"},
      {"date":"2026-10-16","symbol":"ODD","hour":"zzz"}
    ],"from":"2026-10-12","to":"2026-10-18"}"#;

    #[test]
    fn finnhub_rows_carry_the_strings_the_window_renders() {
        let rows = parse_finnhub(FINNHUB).unwrap();
        assert_eq!(rows.len(), 4, "the dateless row is skipped");
        assert_eq!(
            rows[0],
            EarningsRow {
                date: "2026-10-13".into(),
                symbol: "JPM".into(),
                hour: "Pre-mkt".into(),
                eps_est: "4.50".into(),
                eps_act: "4.80".into(),
                surprise: "+6.7%".into(),
            }
        );
        assert_eq!(
            (rows[1].hour.as_str(), rows[1].eps_act.as_str(), rows[1].surprise.as_str()),
            ("After-hrs", "", "")
        );
        assert_eq!(rows[2].hour, "Mid-day");
        assert_eq!(rows[2].surprise, "", "a zero estimate has no percentage surprise");
        assert_eq!(rows[3].hour, "", "an unknown hour code is blank");
    }

    #[test]
    fn a_finnhub_body_without_the_array_is_an_error_not_an_empty_list() {
        for body in ["", "{\"error\":\"API limit reached\"}", "[]", "<html/>"] {
            assert!(parse_finnhub(body).is_err(), "{body}");
        }
        assert_eq!(parse_finnhub("{\"earningsCalendar\":[]}").unwrap().len(), 0);
    }

    #[test]
    fn fmp_rows_carry_the_strings_the_window_renders() {
        let body = r#"[
          {"symbol":"KO","date":"2026-10-14","paymentDate":"2026-11-03","dividend":0.51,"yield":3.123,"frequency":"Quarterly"},
          {"symbol":"BARE","date":"2026-10-15"},
          {"symbol":"NODATE"}
        ]"#;
        let rows = parse_fmp(body).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            DividendRow {
                date: "2026-10-14".into(),
                symbol: "KO".into(),
                pay_date: "2026-11-03".into(),
                amount: "0.51 $".into(),
                yield_pct: "3.12%".into(),
                freq: "Quarterly".into(),
            }
        );
        assert_eq!(
            (rows[1].amount.as_str(), rows[1].yield_pct.as_str(), rows[1].freq.as_str()),
            ("", "", "")
        );
        for body in ["{\"Error Message\":\"Limit Reach\"}", "", "nope"] {
            assert!(parse_fmp(body).is_err(), "{body}");
        }
    }

    const NASDAQ: &str = r#"{"data":{
      "priced":{"rows":[{"pricedDate":"10/09/2026","proposedTickerSymbol":"AAA","companyName":"Aaa Inc",
                "proposedExchange":"NASDAQ","proposedSharePrice":"15.00","sharesOffered":"5,000,000"}]},
      "upcoming":{"upcomingTable":{"rows":[{"expectedPriceDate":"10/14/2026","proposedTickerSymbol":"BBB",
                "companyName":"Bbb Corp","proposedExchange":"NYSE","proposedSharePrice":"10.00-12.00","sharesOffered":"1,000,000"},
                {"expectedPriceDate":"TBD","proposedTickerSymbol":"CCC"}]}}
    },"message":null,"status":{"rCode":200}}"#;

    #[test]
    fn nasdaq_rows_are_priced_then_upcoming_with_iso_days() {
        let rows = parse_nasdaq_ipo(NASDAQ).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            (rows[0].status.as_str(), rows[0].date.as_str(), rows[0].symbol.as_str()),
            ("Priced", "2026-10-09", "AAA")
        );
        assert_eq!(rows[0].shares, "5,000,000");
        assert_eq!((rows[1].status.as_str(), rows[1].date.as_str()), ("Upcoming", "2026-10-14"));
        assert_eq!(rows[1].price, "10.00-12.00");
        assert_eq!(rows[2].date, "", "an unparsable day is blank, as the desktop had it");
    }

    #[test]
    fn nasdaq_with_no_ipos_is_ok_and_its_refusal_shape_is_an_error() {
        let empty =
            r#"{"data":{"priced":{"rows":null},"upcoming":{"upcomingTable":{"rows":null}}}}"#;
        assert_eq!(parse_nasdaq_ipo(empty).unwrap().len(), 0);
        for body in [r#"{"data":null,"status":{"rCode":400}}"#, "", "<html>"] {
            assert!(parse_nasdaq_ipo(body).is_err(), "{body}");
        }
    }

    #[test]
    fn a_week_is_a_monday_to_sunday_range_and_names_one_or_two_months() {
        assert_eq!(parse_day("2026-10-12").unwrap().weekday(), chrono::Weekday::Mon);
        assert_eq!(week_end("2026-10-12").as_deref(), Some("2026-10-18"));
        assert_eq!(week_end("2026-12-28").as_deref(), Some("2027-01-03"), "across a year");
        assert_eq!(week_end("2028-02-28").as_deref(), Some("2028-03-05"), "across a leap day");
        assert_eq!(week_end("garbage"), None);
        assert_eq!(ipo_months("2026-10-12"), ["2026-10"]);
        assert_eq!(ipo_months("2026-09-28"), ["2026-09", "2026-10"]);
        assert_eq!(ipo_months("2026-12-28"), ["2026-12", "2027-01"]);
        assert!(ipo_months("garbage").is_empty());
    }

    /// One recorded request: its URL and its headers.
    type Seen = (String, Vec<(String, String)>);

    /// A transport that records every request and answers by URL prefix.
    #[derive(Default)]
    struct Recording {
        seen: Mutex<Vec<Seen>>,
        answers: Mutex<Vec<(String, Result<String, String>)>>,
    }

    impl Recording {
        fn answering(answers: Vec<(&str, Result<String, String>)>) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::default(),
                answers: Mutex::new(answers.into_iter().map(|(p, r)| (p.to_string(), r)).collect()),
            })
        }
    }

    impl FeedHttp for Recording {
        fn get(&self, request: &HttpGet) -> Result<String, String> {
            let headers =
                request.headers().iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
            self.seen.lock().unwrap().push((request.url().to_string(), headers));
            self.answers
                .lock()
                .unwrap()
                .iter()
                .find(|(p, _)| request.url().contains(p.as_str()))
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| Err("unscripted".into()))
        }
    }

    struct Keys(Vec<(&'static str, &'static str)>, bool);
    impl KeyReader for Keys {
        fn read(&self, name: &str) -> KeyRead {
            if self.1 {
                return KeyRead::Unreadable;
            }
            match self.0.iter().find(|(n, _)| *n == name) {
                Some((_, v)) => KeyRead::Present(Secret::new(*v)),
                None => KeyRead::Absent,
            }
        }
    }

    const SECRET: &str = "sekrit-token-8f3a";

    #[test]
    fn the_keyed_polls_put_each_key_in_exactly_one_place() {
        let http = Recording::answering(vec![
            ("finnhub.test", Ok(FINNHUB.to_string())),
            ("fmp.test", Ok("[]".to_string())),
        ]);
        let keys = Keys(vec![(FINNHUB_API_KEY, SECRET), (FMP_API_KEY, "fmp-secret-1d2c")], false);
        assert!(matches!(
            poll_earnings(
                http.as_ref(),
                &keys,
                &ProviderHealth::new(),
                "http://finnhub.test",
                "2026-10-12"
            ),
            PollResult::Rows { ok: 1, total: 1, .. }
        ));
        assert!(matches!(
            poll_dividends(
                http.as_ref(),
                &keys,
                &ProviderHealth::new(),
                "http://fmp.test",
                "2026-10-12"
            ),
            PollResult::Rows { .. }
        ));
        let seen = http.seen.lock().unwrap();
        let (url, headers) = &seen[0];
        assert_eq!(
            url,
            "http://finnhub.test/api/v1/calendar/earnings?from=2026-10-12&to=2026-10-18"
        );
        assert!(!url.contains(SECRET), "Finnhub's key rides a header, not the URL");
        assert!(headers.iter().any(|(k, v)| k == "X-Finnhub-Token" && v == SECRET));
        let (url, headers) = &seen[1];
        assert_eq!(
            url,
            "http://fmp.test/stable/dividends-calendar?from=2026-10-12&to=2026-10-18&apikey=fmp-secret-1d2c"
        );
        assert!(
            !headers.iter().any(|(_, v)| v.contains("fmp-secret")),
            "FMP's key is in the query only"
        );
    }

    #[test]
    fn an_absent_key_is_not_configured_and_calls_nobody() {
        let http = Recording::answering(vec![]);
        let none = Keys(vec![], false);
        assert!(matches!(
            poll_earnings(http.as_ref(), &none, &ProviderHealth::new(), "http://x", "2026-10-12"),
            PollResult::NotConfigured { key_name } if key_name == FINNHUB_API_KEY
        ));
        assert!(matches!(
            poll_dividends(http.as_ref(), &none, &ProviderHealth::new(), "http://x", "2026-10-12"),
            PollResult::NotConfigured { key_name } if key_name == FMP_API_KEY
        ));
        assert!(http.seen.lock().unwrap().is_empty(), "no key, no request");
        // an unreadable store is a failed poll (rows kept), never NotConfigured
        let broken = Keys(vec![], true);
        assert!(matches!(
            poll_earnings(http.as_ref(), &broken, &ProviderHealth::new(), "http://x", "2026-10-12"),
            PollResult::Failed { total: 1 }
        ));
        assert!(http.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_provider_failure_is_logged_once_and_names_no_key_or_query() {
        let http = Recording::answering(vec![("fmp.test", Err("HTTP 403".into()))]);
        let keys = Keys(vec![(FMP_API_KEY, SECRET)], false);
        let health = ProviderHealth::new();
        let (results, lines) = vike_log::capture::captured(|| {
            (0..3)
                .map(|_| {
                    poll_dividends(http.as_ref(), &keys, &health, "http://fmp.test", "2026-10-12")
                })
                .collect::<Vec<_>>()
        });
        assert!(results.iter().all(|r| matches!(r, PollResult::Failed { total: 1 })));
        assert_eq!(lines.len(), 1, "three failed polls, ONE line (the transition): {lines:?}");
        let line = &lines[0];
        assert_eq!(line.level, tracing::Level::WARN);
        assert_eq!(
            (line.field("provider"), line.field("why"), line.field("scope")),
            (Some("FMP"), Some("HTTP 403"), Some("2026-10-12"))
        );
        let text = format!("{line:?}");
        for forbidden in [SECRET, "apikey", "from=", "to=", "?", "http://"] {
            assert!(!text.contains(forbidden), "{forbidden:?} in {text}");
        }

        // The provider answers again: exactly one recovery line, still no key or query.
        *http.answers.lock().unwrap() = vec![("fmp.test".to_string(), Ok("[]".to_string()))];
        let (_, lines) = vike_log::capture::captured(|| {
            poll_dividends(http.as_ref(), &keys, &health, "http://fmp.test", "2026-10-12");
            poll_dividends(http.as_ref(), &keys, &health, "http://fmp.test", "2026-10-12");
        });
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].level, tracing::Level::INFO);
        let text = format!("{:?}", lines[0]);
        assert!(!text.contains(SECRET) && !text.contains("apikey"), "{text}");
    }

    #[test]
    fn a_straddling_week_keeps_the_failed_months_last_good_rows() {
        let sept = r#"{"data":{"priced":{"rows":[{"pricedDate":"09/30/2026","proposedTickerSymbol":"SEP"}]}}}"#;
        let oct = r#"{"data":{"priced":{"rows":[{"pricedDate":"10/01/2026","proposedTickerSymbol":"OCT"}]}}}"#;
        let http = Recording::answering(vec![
            ("date=2026-09", Ok(sept.to_string())),
            ("date=2026-10", Ok(oct.to_string())),
        ]);
        let poller = IpoPoller::new("http://nasdaq.test");
        let PollResult::Rows { rows, ok, total } = poller.poll(http.as_ref(), "2026-09-28") else {
            panic!("rows")
        };
        assert_eq!((ok, total), (2, 2));
        assert_eq!(rows.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), ["SEP", "OCT"]);
        // October now fails: September is fresh, October's rows are the previous ones
        *http.answers.lock().unwrap() = vec![
            ("date=2026-09".to_string(), Ok(sept.replace("SEP", "SEP2"))),
            ("date=2026-10".to_string(), Err("HTTP 503".into())),
        ];
        let PollResult::Rows { rows, ok, total } = poller.poll(http.as_ref(), "2026-09-28") else {
            panic!("rows")
        };
        assert_eq!((ok, total), (1, 2));
        assert_eq!(rows.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), ["SEP2", "OCT"]);
        // both fail: a failed poll
        *http.answers.lock().unwrap() = vec![];
        assert!(matches!(
            poller.poll(http.as_ref(), "2026-09-28"),
            PollResult::Failed { total: 2 }
        ));
        // and the browser headers Nasdaq needs went out
        let seen = http.seen.lock().unwrap();
        assert!(seen[0].1.iter().any(|(k, v)| k == "User-Agent" && v.starts_with("Mozilla/5.0")));
    }
}
