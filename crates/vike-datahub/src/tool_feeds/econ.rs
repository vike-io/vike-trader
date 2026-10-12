//! **The economic calendar: ForexFactory's weekly JSON.** The parsing is the desktop's
//! (`vike-app-core`'s former `tools/calendar.rs`), minus everything that depended on the viewer: the
//! desktop turned each event's stamp into a LOCAL clock time and day label and looked its currency
//! up in a country table; here an event is the raw instant (`ts_ms`, UTC) and the feed's currency
//! code, and the display layer derives the rest from the viewer's timezone
//! (`vike_datahub_client::tool_feeds::CalEvent`'s doc).
//!
//! The request is the desktop's too, browser `User-Agent` and all: the bare request stalls on
//! ForexFactory's CDN. `ff_calendar_thisweek.json` is the SERVER's current week; the feed carries no
//! week parameter.

use std::time::Duration;

use serde::Deserialize;
use vike_datahub_client::tool_feeds::CalEvent;

use super::worker::PollResult;
use super::{FeedHttp, HttpGet};

/// The request timeout (the desktop's).
pub const CALENDAR_TIMEOUT: Duration = Duration::from_secs(10);

/// How many events of the week are kept. The desktop's own comment: "the full week can run ~150+
/// events; 90 truncated Thu/Fri".
pub const MAX_EVENTS: usize = 400;

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

/// **Parse the weekly JSON** into at most [`MAX_EVENTS`] raw events, in the feed's order. A body that
/// is not a JSON array of events is an `Err` (the poll then keeps the last good week). An event whose
/// `date` is not RFC 3339 keeps `ts_ms == 0`.
pub fn parse_calendar(body: &str) -> Result<Vec<CalEvent>, String> {
    let events: Vec<FfEvent> =
        serde_json::from_str(body).map_err(|e| format!("not the weekly calendar JSON: {e}"))?;
    Ok(events
        .into_iter()
        .take(MAX_EVENTS)
        .map(|e| CalEvent {
            ts_ms: chrono::DateTime::parse_from_rfc3339(&e.date)
                .map(|d| d.timestamp_millis())
                .unwrap_or(0),
            currency: e.country,
            title: e.title,
            impact: e.impact,
            forecast: e.forecast,
            previous: e.previous,
            actual: e.actual,
        })
        .collect())
}

/// **One poll of the weekly calendar** at `url`.
pub fn poll(http: &dyn FeedHttp, url: &str) -> PollResult<CalEvent> {
    let request = HttpGet::new(url, CALENDAR_TIMEOUT).with_browser_headers();
    let parsed = http.get(&request).and_then(|body| parse_calendar(&body));
    match parsed {
        Ok(rows) => PollResult::Rows { rows, ok: 1, total: 1 },
        Err(why) => {
            tracing::debug!(why = why.as_str(), "vike-datahub calendar: the weekly fetch failed");
            PollResult::Failed { total: 1 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEEK: &str = r#"[
  {"title":"Non-Farm Employment Change","country":"USD","date":"2026-10-09T08:30:00-04:00",
   "impact":"High","forecast":"150K","previous":"142K"},
  {"title":"Bank Holiday","country":"JPY","date":"2026-10-12T00:00:00-04:00","impact":"Holiday",
   "forecast":"","previous":""},
  {"title":"BOE Gov Bailey Speaks","country":"GBP","date":"garbled","impact":"Medium",
   "forecast":"","previous":"","actual":"n/a"}
]"#;

    #[test]
    fn the_weekly_json_parses_to_raw_events() {
        let events = parse_calendar(WEEK).unwrap();
        assert_eq!(events.len(), 3);
        let nfp = &events[0];
        // 08:30 at UTC-4 is 12:30 UTC
        assert_eq!(nfp.ts_ms, 1_791_549_000_000);
        assert_eq!(
            (nfp.currency.as_str(), nfp.impact.as_str(), nfp.forecast.as_str()),
            ("USD", "High", "150K")
        );
        assert_eq!(
            (nfp.previous.as_str(), nfp.actual.as_str()),
            ("142K", ""),
            "absent actual is empty"
        );
        assert_eq!(events[1].impact, "Holiday");
        assert_eq!(events[2].ts_ms, 0, "an unparsable date is ts_ms 0, as the contract says");
        assert_eq!(events[2].actual, "n/a");
    }

    #[test]
    fn a_body_that_is_not_the_calendar_is_an_error_not_an_empty_week() {
        for body in
            ["", "<html>Access denied</html>", "{\"error\":\"rate limited\"}", "[{\"title\":1}]"]
        {
            assert!(parse_calendar(body).is_err(), "{body}");
        }
        assert_eq!(parse_calendar("[]").unwrap().len(), 0, "a genuinely empty week is Ok(empty)");
    }

    #[test]
    fn a_week_is_cut_at_the_event_cap() {
        let one =
            r#"{"title":"t","country":"USD","date":"2026-10-09T08:30:00-04:00","impact":"Low"}"#;
        let body = format!("[{}]", vec![one; MAX_EVENTS + 25].join(","));
        assert_eq!(parse_calendar(&body).unwrap().len(), MAX_EVENTS);
    }

    struct Fixed(Result<String, String>);
    impl FeedHttp for Fixed {
        fn get(&self, request: &HttpGet) -> Result<String, String> {
            assert!(
                request
                    .headers()
                    .iter()
                    .any(|(k, v)| *k == "User-Agent" && v.starts_with("Mozilla/5.0")),
                "ForexFactory's CDN stalls a bare request"
            );
            self.0.clone()
        }
    }

    #[test]
    fn a_failed_fetch_is_a_failed_poll() {
        let ok = poll(&Fixed(Ok(WEEK.to_string())), "http://t/ff");
        assert!(matches!(ok, PollResult::Rows { ok: 1, total: 1, .. }));
        assert!(matches!(
            poll(&Fixed(Err("HTTP 429".into())), "http://t/ff"),
            PollResult::Failed { total: 1 }
        ));
        assert!(matches!(
            poll(&Fixed(Ok("garbage".into())), "http://t/ff"),
            PollResult::Failed { total: 1 }
        ));
    }
}
