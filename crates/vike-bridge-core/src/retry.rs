//! The rate-limited retry TIMING skeleton the venue kline/trade pagers share (the I/O capture half
//! is `http::get_raw`). Try up to `max_retries + 1` times; on each rate-limited (or 5xx) response
//! sleep `min(retry_after ∨ backoff, max)`, then double the backoff (capped). The exhaustion error
//! reads `"{label} rate-limited: {note} (after {max_retries} retries)"` when the last answer was a
//! rate limit.
//!
//! The CLASSIFICATION stays per venue (bybit adds a Cloudflare 403 plus retCode 10006/10018; okx
//! keys off a string `code` 50011/50061): each passes an `attempt` closure returning a [`RetryVerdict`].
//! A JSON pager's attempt judges its [`RawResponse`] through [`json_page_verdict`], handing in its own
//! rate-limit statuses, its business-code read and its parse.
//!
//! **A 5xx is retried like a rate limit** (owner ruling B-6, 2026-11-12: every venue retries a 5xx while
//! downloading history): an attempt that saw one answers [`RetryVerdict::Unavailable`], waited out on the same
//! cadence, and an exhaustion on it reads `"{label}: venue unavailable (HTTP {status}) after {n} tries"`,
//! never "rate-limited". A transport failure still fails at once. [`retry_scheduled`] below is untouched:
//! its venues keep their own 5xx rule.
//!
//! **One JSON history page** is [`json_page_fetch`]: the GET, the backoff and the verdict in that
//! order, with the venue's facts in one [`JsonPageSpec`] const (its words, extra headers, rate-limit
//! statuses and business code, and the policy, [`RetryBackoffPolicy::HISTORY_PAGE`] where it is the
//! shared one). It retries a rate limit and a 5xx; a transport failure or any other status fails the
//! page at once. The PAGE WALK over it is `vike_data::source`'s (`page_walk_forward`,
//! `page_walk_backward`), and the paced kline walk is [`crate::klines::kline_page_walk_backward`].
//!
//! [`retry_scheduled`] is the other shape: a GET of a FILE on a fixed schedule of waits (dukascopy's
//! hour files, fxcm's candle archive, CDNs that fail in bursts), over an injected attempt and sleep
//! so the policy runs with neither a network nor a clock. Each venue words its own error, and a
//! venue whose transient statuses are not a file CDN's five names its own ([`retry_scheduled_when`]:
//! oanda's candles retry a 429 or any 5xx).
//!
//! **Asking again after a pause** is `vike_utility::retry::retry_on_pauses` (it moved below this crate on
//! 2026-11-13, so the datahub, which may not link this one, shares it): ask, and while the answer is not
//! settled wait the next pause of a schedule and ask again, the LAST answer standing. The schedule, the
//! "settled" rule and the per-miss line are the venue's (oanda's submit lookup, ig's `/confirms` and
//! login, hyperliquid's `/info` and spot symbology, deribit's fresh-combo wait). **A redial's pause** after a
//! run of failures on a fixed table is [`retry_wait_after`] (fxcm's login, ibkr's gateway dials); a
//! doubling one is `vike_venues::venue_timings::VenueTimingsReconnect::wait_at_step`.

use std::time::Duration;

use serde_json::Value;

use crate::http::{RawResponse, body_head, get_raw, json_body};

/// One attempt of a [`retry_scheduled`] GET.
#[derive(Debug)]
pub enum RetryAttempt {
    /// The server answered: its status, the body (read only for a 2xx) and the wait its
    /// `Retry-After` header stated in seconds, if it stated one.
    Response { status: u16, body: Vec<u8>, retry_after: Option<Duration> },
    /// No usable answer at all — DNS, connect, TLS, a reset, a timeout, a body cut off mid-read.
    Failed(String),
}

/// A transient failure of one [`RetryAttempt`], as [`RetryGaveUp::Exhausted`] reports the last one.
#[derive(Debug, PartialEq, Eq)]
pub enum RetryFailure {
    /// A transient status: a 429, 500, 502, 503 or 504 for [`retry_scheduled`], the venue's own set
    /// for [`retry_scheduled_when`].
    Status(u16),
    /// No usable answer: the attempt's own words.
    Transport(String),
}

/// Why [`retry_scheduled`] gave up.
#[derive(Debug, PartialEq, Eq)]
pub enum RetryGaveUp {
    /// A status that is not 2xx, not 404 and not transient: failed at once, never retried.
    Refused(u16),
    /// Every attempt failed transiently; the LAST failure.
    Exhausted(RetryFailure),
}

/// Drive `attempt` on the `delays` schedule: `delays.len() + 1` attempts in all. A 404 is `Ok(None)`
/// (never retried: an absent file stays absent), a 2xx is its body, any other non-transient status
/// fails at once; a transient status or a failed attempt waits out the next of `delays` —
/// lengthened, never shortened, to a `Retry-After` up to `retry_after_cap` — and tries again. The
/// last failure is not slept after: there is nothing left to wait for. The transient statuses are a
/// 429, 500, 502, 503 and 504.
pub fn retry_scheduled(
    delays: &[Duration],
    retry_after_cap: Duration,
    attempt: impl FnMut() -> RetryAttempt,
    sleep: impl FnMut(Duration),
) -> Result<Option<Vec<u8>>, RetryGaveUp> {
    let file_cdn = |status: u16| matches!(status, 429 | 500 | 502 | 503 | 504);
    retry_scheduled_when(file_cdn, delays, retry_after_cap, attempt, sleep)
}

/// [`retry_scheduled`] with the venue's own `transient` statuses: oanda's candles retry a 429 or ANY
/// 5xx (its `request_with`), where a file CDN's schedule retries five.
pub fn retry_scheduled_when(
    transient: impl Fn(u16) -> bool,
    delays: &[Duration],
    retry_after_cap: Duration,
    mut attempt: impl FnMut() -> RetryAttempt,
    mut sleep: impl FnMut(Duration),
) -> Result<Option<Vec<u8>>, RetryGaveUp> {
    let mut waits = delays.iter();
    loop {
        let (failure, stated) = match attempt() {
            RetryAttempt::Response { status: 404, .. } => return Ok(None),
            RetryAttempt::Response { status, body, .. } if (200..300).contains(&status) => {
                return Ok(Some(body));
            }
            RetryAttempt::Response { status, retry_after, .. } => {
                if !transient(status) {
                    return Err(RetryGaveUp::Refused(status));
                }
                let stated = retry_after.unwrap_or(Duration::ZERO).min(retry_after_cap);
                (RetryFailure::Status(status), stated)
            }
            RetryAttempt::Failed(why) => (RetryFailure::Transport(why), Duration::ZERO),
        };
        let Some(delay) = waits.next() else { return Err(RetryGaveUp::Exhausted(failure)) };
        sleep((*delay).max(stated));
    }
}

/// The pause after `failures` failed attempts in a row on a fixed `schedule`: its first entry after the
/// first failure (and before any), the next after each further one, and its LAST for every failure past
/// its end. ⚠ An empty `schedule` panics: each venue's is a non-empty const.
pub fn retry_wait_after(schedule: &[Duration], failures: u32) -> Duration {
    let index = usize::try_from(failures.saturating_sub(1)).unwrap_or(usize::MAX);
    schedule[index.min(schedule.len() - 1)]
}

/// The backoff cadence for [`retry_rate_limited`]: up to `max_retries + 1` attempts; the sleep
/// starts at `initial` and doubles, capped at `max`. Each venue declares its own constants.
#[derive(Clone, Copy, Debug)]
pub struct RetryBackoffPolicy {
    /// Retries AFTER the first try (so the loop runs `max_retries + 1` attempts total).
    pub max_retries: u32,
    /// The first backoff sleep, before any doubling.
    pub initial: Duration,
    /// The ceiling for both the doubling backoff and a server-supplied `Retry-After`.
    pub max: Duration,
}

impl RetryBackoffPolicy {
    /// The rate-limit backoff the paged history GETs share: 6 retries, 1 s doubling to 60 s (bybit,
    /// okx and deribit klines and funding, binance aggTrades each declared these three numbers).
    /// binance's and aster's kline specs carry their own `KlineRateLimit` values on purpose.
    pub const HISTORY_PAGE: RetryBackoffPolicy = RetryBackoffPolicy {
        max_retries: 6,
        initial: Duration::from_secs(1),
        max: Duration::from_secs(60),
    };
}

/// One attempt's outcome, as classified by the venue's `attempt` closure.
///
/// A hard error (transport failure, non-retryable status, unparseable body) is NOT a variant: the
/// closure returns `Err(String)` and [`retry_rate_limited`] surfaces it at once, without retrying.
pub enum RetryVerdict<T> {
    /// The attempt succeeded; `T` is the caller's payload (e.g. the page's bars).
    Done(T),
    /// The venue signalled a rate limit; back off and retry. `retry_after` is the server's
    /// `Retry-After` (else the policy's doubling backoff); `note` is recorded for the exhaustion
    /// message (e.g. `"HTTP 429 (rate limited)"`, `"retCode 10006 (rate limited)"`).
    RateLimited { retry_after: Option<Duration>, note: String },
    /// The venue answered an HTTP 5xx (`status`): down or overloaded for now, so the SAME page is asked
    /// again on the rate limit's cadence (owner ruling B-6). `retry_after` as above.
    Unavailable { retry_after: Option<Duration>, status: u16 },
}

/// Drive `attempt` under `policy`'s backoff cadence. Returns the first [`RetryVerdict::Done`] payload;
/// propagates the first `Err` at once; a [`RetryVerdict::RateLimited`] or [`RetryVerdict::Unavailable`] waits
/// and asks again. On exhausting every attempt the error is the LAST one's: the module doc's
/// rate-limit words with its note, or `"{label}: venue unavailable (HTTP {status}) after {n} tries"`.
pub fn retry_rate_limited<T>(
    policy: RetryBackoffPolicy,
    label: &str,
    mut attempt: impl FnMut() -> Result<RetryVerdict<T>, String>,
) -> Result<T, String> {
    let mut backoff = policy.initial;
    let mut exhausted = String::new();
    for _ in 0..=policy.max_retries {
        let retry_after = match attempt()? {
            RetryVerdict::Done(value) => return Ok(value),
            RetryVerdict::RateLimited { retry_after, note } => {
                exhausted =
                    format!("{label} rate-limited: {note} (after {} retries)", policy.max_retries);
                retry_after
            }
            RetryVerdict::Unavailable { retry_after, status } => {
                exhausted = format!(
                    "{label}: venue unavailable (HTTP {status}) after {} tries",
                    policy.max_retries + 1
                );
                retry_after
            }
        };
        let wait = retry_after.unwrap_or(backoff).min(policy.max);
        std::thread::sleep(wait);
        backoff = (backoff * 2).min(policy.max);
    }
    Err(exhausted)
}

/// One JSON page's [`RetryVerdict`], judged in the order the bybit, deribit and okx pagers always judged
/// it: a status in `limited` is a rate limit (`"HTTP {status} (rate limited)"`); a 5xx is
/// [`RetryVerdict::Unavailable`] (owner ruling B-6); any other non-2xx is
/// the hard `"{label} HTTP {status}: {head}"`; a body that is not JSON is [`json_body`]'s error over
/// `json_what`; a body `business_limit` names a rate limit (`Some(note)`: those venues answer one with
/// HTTP 200) is one; anything else is `parse`d. Each venue's statuses, words and codes stay its own.
pub fn json_page_verdict<T>(
    raw: RawResponse,
    label: &str,
    json_what: &str,
    limited: &[u16],
    business_limit: impl FnOnce(&Value) -> Option<String>,
    parse: impl FnOnce(&Value) -> Result<T, String>,
) -> Result<RetryVerdict<T>, String> {
    if limited.contains(&raw.status) {
        let note = format!("HTTP {} (rate limited)", raw.status);
        return Ok(RetryVerdict::RateLimited { retry_after: raw.retry_after, note });
    }
    if (500..600).contains(&raw.status) {
        return Ok(RetryVerdict::Unavailable { retry_after: raw.retry_after, status: raw.status });
    }
    if !(200..300).contains(&raw.status) {
        return Err(format!("{label} HTTP {}: {}", raw.status, body_head(&raw.body)));
    }
    let v = json_body(&raw.body, json_what)?;
    if let Some(note) = business_limit(&v) {
        return Ok(RetryVerdict::RateLimited { retry_after: raw.retry_after, note });
    }
    parse(&v).map(RetryVerdict::Done)
}

/// **One venue's JSON history page, as facts** — what [`json_page_fetch`] needs to ask for a page and
/// judge the answer. A `const` per venue and endpoint (klines, funding).
#[derive(Clone, Copy, Debug)]
pub struct JsonPageSpec {
    /// Heads the GET error, the hard HTTP error and the exhaustion error: `"bybit klines"`.
    pub label: &'static str,
    /// Heads the error for a body that is not JSON: `"bybit kline"`.
    pub json_what: &'static str,
    /// Extra request headers: okx's Cloudflare browser `User-Agent` and its `Accept`.
    pub headers: &'static [(&'static str, &'static str)],
    /// The statuses that mean a rate limit: `[429, 418]`, and bybit adds its CDN's `403`.
    pub limited: &'static [u16],
    /// A readable body's rate-limit code (those venues answer one with HTTP 200), as the note the
    /// exhaustion error carries: `Some("retCode 10006 (rate limited)")`.
    pub business_limit: fn(&Value) -> Option<String>,
    /// The backoff on a rate limit.
    pub backoff: RetryBackoffPolicy,
}

/// GET one JSON page from `url` and judge it ([`json_page_verdict`]) under `spec`'s backoff: a rate
/// limit or a 5xx waits and asks the same page again ([`retry_rate_limited`]); any other failure
/// (transport, another status `spec` does not list, an unreadable body, `parse`'s own error) fails
/// the page at once.
pub fn json_page_fetch<T>(
    agent: &ureq::Agent,
    url: &str,
    spec: &JsonPageSpec,
    parse: impl Fn(&Value) -> Result<T, String>,
) -> Result<T, String> {
    retry_rate_limited(spec.backoff, spec.label, || {
        let raw = get_raw(agent, url, spec.label, spec.headers)?;
        json_page_verdict(
            raw,
            spec.label,
            spec.json_what,
            spec.limited,
            spec.business_limit,
            &parse,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    // A tiny policy with zero-duration sleeps so the tests don't actually wait.
    fn instant_policy(max_retries: u32) -> RetryBackoffPolicy {
        RetryBackoffPolicy { max_retries, initial: Duration::ZERO, max: Duration::ZERO }
    }

    #[test]
    fn returns_first_done_without_further_attempts() {
        let calls = Cell::new(0u32);
        let out: Result<i32, String> = retry_rate_limited(instant_policy(6), "test", || {
            calls.set(calls.get() + 1);
            Ok(RetryVerdict::Done(42))
        });
        assert_eq!(out.unwrap(), 42);
        assert_eq!(calls.get(), 1, "a Done must not retry");
    }

    #[test]
    fn propagates_hard_error_immediately() {
        let calls = Cell::new(0u32);
        let out: Result<i32, String> = retry_rate_limited(instant_policy(6), "test", || {
            calls.set(calls.get() + 1);
            Err("boom".to_string())
        });
        assert_eq!(out.unwrap_err(), "boom");
        assert_eq!(calls.get(), 1, "a hard error must not retry");
    }

    /// Rate-limited until attempt `done_at`, then `Done(done_at)`: what came back and how many calls.
    fn done_on_attempt(max_retries: u32, done_at: u32) -> (Result<u32, String>, u32) {
        let calls = Cell::new(0u32);
        let out = retry_rate_limited(instant_policy(max_retries), "test", || {
            let n = calls.get() + 1;
            calls.set(n);
            if n < done_at {
                Ok(RetryVerdict::RateLimited { retry_after: None, note: format!("throttle {n}") })
            } else {
                Ok(RetryVerdict::Done(n))
            }
        });
        (out, calls.get())
    }

    #[test]
    fn retries_then_succeeds() {
        assert_eq!(done_on_attempt(6, 3), (Ok(3), 3));
    }

    /// Boundary: a `Done` on the very LAST allowed attempt must succeed, not report exhaustion
    /// (`max_retries + 1` attempts are available).
    #[test]
    fn succeeds_on_the_final_allowed_attempt() {
        assert_eq!(done_on_attempt(3, 4), (Ok(4), 4));
    }

    /// The pin of dukascopy's `fetch_hour_with` and fxcm's `get_with_retries`, the two former copies
    /// of this schedule: every wait is the schedule's, lengthened to a capped `Retry-After`; a 404 is
    /// `None`; a non-transient status fails at once; the last failure is reported and not slept
    /// after. Red on a mutant that drops the cap, takes the delay over `max`, sleeps after the last
    /// attempt or retries a 403.
    #[test]
    fn a_scheduled_get_waits_the_schedule_and_reports_the_last_failure() {
        let secs = Duration::from_secs;
        let run = |script: Vec<RetryAttempt>| {
            let mut script = script.into_iter();
            let (mut attempts, mut waits) = (0usize, Vec::new());
            let delays = [secs(4), secs(10)];
            let out = retry_scheduled(
                &delays,
                secs(60),
                || {
                    attempts += 1;
                    script.next().expect("attempted more often than the script answers")
                },
                |wait| waits.push(wait),
            );
            (out, attempts, waits)
        };
        let status = |status: u16, retry_after: Option<u64>| RetryAttempt::Response {
            status,
            body: vec![7],
            retry_after: retry_after.map(secs),
        };
        assert_eq!(
            run(vec![
                status(503, Some(30)),
                RetryAttempt::Failed("reset".into()),
                status(200, None)
            ]),
            (Ok(Some(vec![7])), 3, vec![secs(30), secs(10)])
        );
        assert_eq!(
            run(vec![status(429, Some(3_600)), status(404, None)]),
            (Ok(None), 2, vec![secs(60)])
        );
        assert_eq!(run(vec![status(403, None)]), (Err(RetryGaveUp::Refused(403)), 1, vec![]));
        assert_eq!(
            run(vec![
                status(503, None),
                RetryAttempt::Failed("reset".into()),
                status(502, Some(1))
            ]),
            (Err(RetryGaveUp::Exhausted(RetryFailure::Status(502))), 3, vec![secs(4), secs(10)])
        );
        assert_eq!(
            run(vec![status(500, None), status(500, None), RetryAttempt::Failed("timeout".into())]),
            (
                Err(RetryGaveUp::Exhausted(RetryFailure::Transport("timeout".into()))),
                3,
                vec![secs(4), secs(10)]
            )
        );
    }

    /// The transient set is the schedule's one venue knob: a 501 is refused at once by the file
    /// CDNs' five, and waited out like a 503 under oanda's "a 429 or any 5xx"; a 404 is absent and a
    /// 4xx refused under both. Kill proof: `retry_scheduled_when` ignoring `transient`.
    #[test]
    fn a_venue_names_its_own_transient_statuses() {
        let secs = Duration::from_secs;
        let answer =
            |status: u16| RetryAttempt::Response { status, body: vec![], retry_after: None };
        let oanda = |status: u16| status == 429 || (500..=599).contains(&status);
        let mut waits = Vec::new();
        let mut script = vec![answer(501), answer(599), answer(200)].into_iter();
        let got = retry_scheduled_when(
            oanda,
            &[secs(1), secs(2)],
            secs(120),
            || script.next().expect("asked once too often"),
            |wait| waits.push(wait),
        );
        assert_eq!((got, waits), (Ok(Some(vec![])), vec![secs(1), secs(2)]));
        let once = |status: u16| {
            let mut script = vec![answer(status)].into_iter();
            retry_scheduled(&[secs(1)], secs(60), || script.next().expect("asked twice"), |_| {})
        };
        assert_eq!(once(501), Err(RetryGaveUp::Refused(501)));
        assert_eq!(once(404), Ok(None));
        let mut script = vec![answer(403)].into_iter();
        let refused =
            retry_scheduled_when(oanda, &[secs(1)], secs(60), || script.next().unwrap(), |_| {});
        assert_eq!(refused, Err(RetryGaveUp::Refused(403)));
    }

    #[test]
    fn exhaustion_message_carries_last_note_and_count() {
        // max_retries = 2 ⇒ 3 attempts total, all rate-limited ⇒ exhaustion.
        let calls = Cell::new(0u32);
        let out: Result<i32, String> = retry_rate_limited(instant_policy(2), "okx candles", || {
            calls.set(calls.get() + 1);
            Ok(RetryVerdict::RateLimited {
                retry_after: None,
                note: "code 50011 (rate limited)".to_string(),
            })
        });
        assert_eq!(calls.get(), 3, "max_retries + 1 attempts");
        assert_eq!(
            out.unwrap_err(),
            "okx candles rate-limited: code 50011 (rate limited) (after 2 retries)"
        );
    }

    fn raw(status: u16, body: &str) -> RawResponse {
        RawResponse {
            status,
            retry_after: Some(Duration::from_secs(3)),
            used_weight: None,
            body: body.to_string(),
        }
    }

    /// `json_page_verdict` as the three pagers judged a page (bybit's `[429, 418, 403]` +
    /// `retCode`, okx's and deribit's `[429, 418]` + their `code`), arm by arm and in that order.
    /// Kill proof: check the business code before the status (the 429 row with a non-JSON body
    /// errors), drop `limited` (the 429 rows hard-fail), or word the JSON error with `label`.
    #[test]
    fn a_json_page_is_judged_status_first_then_json_then_the_business_code() {
        fn judge(status: u16, body: &str, limited: &[u16]) -> Result<RetryVerdict<Value>, String> {
            let code = |v: &Value| {
                let code = v.get("retCode").and_then(Value::as_i64).unwrap_or(-1);
                [10006, 10018].contains(&code).then(|| format!("retCode {code} (rate limited)"))
            };
            let rows = |v: &Value| Ok(v["rows"].clone());
            json_page_verdict(raw(status, body), "bybit klines", "bybit kline", limited, code, rows)
        }
        fn as_limit(v: Result<RetryVerdict<Value>, String>) -> Option<(Option<Duration>, String)> {
            match v {
                Ok(RetryVerdict::RateLimited { retry_after, note }) => Some((retry_after, note)),
                _ => None,
            }
        }

        let three = Some(Duration::from_secs(3));
        assert_eq!(
            as_limit(judge(429, "<html>", &[429, 418, 403])),
            Some((three, "HTTP 429 (rate limited)".to_string())),
            "a limited status wins before the body is read"
        );
        assert_eq!(
            as_limit(judge(403, "", &[429, 418, 403])),
            Some((three, "HTTP 403 (rate limited)".to_string()))
        );
        assert_eq!(
            judge(403, "blocked", &[429, 418]).err().as_deref(),
            Some("bybit klines HTTP 403: blocked"),
            "a status the venue does not list is a hard error"
        );
        assert!(
            judge(200, "{oops", &[429]).err().is_some_and(|e| e.starts_with("bybit kline json: ")),
            "a body that is not JSON is the venue's json error"
        );
        assert_eq!(
            as_limit(judge(200, r#"{"retCode":10006}"#, &[429])),
            Some((three, "retCode 10006 (rate limited)".to_string()))
        );
        match judge(200, r#"{"retCode":0,"rows":[1]}"#, &[429]) {
            Ok(RetryVerdict::Done(v)) => assert_eq!(v, serde_json::json!([1])),
            _ => panic!("a readable page is parsed"),
        }
    }

    /// A spec as a venue declares one, with a backoff that does not sleep.
    fn spec(max_retries: u32) -> JsonPageSpec {
        fn ret_code(v: &Value) -> Option<String> {
            let code = v.get("retCode").and_then(Value::as_i64).unwrap_or(-1);
            (code == 10006).then(|| format!("retCode {code} (rate limited)"))
        }
        JsonPageSpec {
            label: "bybit klines",
            json_what: "bybit kline",
            headers: &[],
            limited: &[429, 418, 403],
            business_limit: ret_code,
            backoff: instant_policy(max_retries),
        }
    }

    /// `json_page_fetch` over a loopback venue: a limited status and a business-coded 200 each ask
    /// the SAME url again, the readable page is parsed, a 4xx the spec does not list fails at
    /// once, and exhaustion is the shared error over the spec's label. Kill proof: drop the retry
    /// (the first case errors), or retry a 400 (its one reply is spent, so the error is the retry's
    /// failed GET, not the HTTP 400).
    #[test]
    fn a_json_page_is_asked_again_on_a_rate_limit_and_failed_on_anything_else() {
        let rows = |v: &Value| Ok(v["rows"].clone());
        let replies = vec![
            (429, String::new()),
            (200, r#"{"retCode":10006}"#.to_string()),
            (200, r#"{"retCode":0,"rows":[7]}"#.to_string()),
        ];
        let (url, seen) = vike_log::loopback::serve_replies("/v5/market/kline?x=1", replies);
        let agent = crate::http::blocking_agent();
        assert_eq!(json_page_fetch(&agent, &url, &spec(6), rows), Ok(serde_json::json!([7])));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["GET /v5/market/kline?x=1 HTTP/1.1"; 3],
            "the same page, asked three times"
        );

        let (url, seen) = vike_log::loopback::serve_replies("/", vec![(400, "bad".into())]);
        assert_eq!(
            json_page_fetch(&agent, &url, &spec(6), rows),
            Err("bybit klines HTTP 400: bad".to_string())
        );
        assert_eq!(seen.lock().unwrap().len(), 1, "a 4xx is not retried");

        let limited = vec![(429, String::new()), (429, String::new())];
        let (url, _) = vike_log::loopback::serve_replies("/", limited);
        assert_eq!(
            json_page_fetch(&agent, &url, &spec(1), rows),
            Err("bybit klines rate-limited: HTTP 429 (rate limited) (after 1 retries)".to_string())
        );
    }

    /// Owner ruling B-6: a history page answered with a 5xx is asked AGAIN on the rate limit's cadence
    /// (any 5xx, a 500 as much as a 503), and the walk goes on once the venue answers; exhausted on a
    /// 5xx, the error says the venue is unavailable, never "rate-limited", while a rate-limit exhaustion
    /// keeps its own words. Red before the ruling: the first 5xx failed the page (`HTTP 500: down`).
    #[test]
    fn a_5xx_page_is_asked_again_and_exhausts_as_venue_unavailable() {
        let rows = |v: &Value| Ok(v["rows"].clone());
        let agent = crate::http::blocking_agent();
        let replies = vec![
            (500, "down".to_string()),
            (503, String::new()),
            (200, r#"{"retCode":0,"rows":[9]}"#.to_string()),
        ];
        let (url, seen) = vike_log::loopback::serve_replies("/v5/market/kline?y=2", replies);
        assert_eq!(json_page_fetch(&agent, &url, &spec(6), rows), Ok(serde_json::json!([9])));
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["GET /v5/market/kline?y=2 HTTP/1.1"; 3],
            "the same page, asked again after each 5xx"
        );

        let (url, _) = vike_log::loopback::serve_replies("/", vec![(503, String::new()); 2]);
        assert_eq!(
            json_page_fetch(&agent, &url, &spec(1), rows),
            Err("bybit klines: venue unavailable (HTTP 503) after 2 tries".to_string())
        );

        // the LAST answer words the exhaustion: a rate limit after a 5xx is still a rate limit
        let mixed = vec![(502, String::new()), (429, String::new())];
        let (url, _) = vike_log::loopback::serve_replies("/", mixed);
        assert_eq!(
            json_page_fetch(&agent, &url, &spec(1), rows),
            Err("bybit klines rate-limited: HTTP 429 (rate limited) (after 1 retries)".to_string())
        );
    }

    /// `retry_wait_after`: the first entry before any failure and after the first, one entry per
    /// further failure, and the last for every failure past the end, `u32::MAX` included.
    #[test]
    fn a_fixed_schedule_waits_its_entry_per_failure_and_repeats_its_last() {
        let secs = Duration::from_secs;
        let schedule = [secs(30), secs(60), secs(300)];
        let waits: Vec<Duration> =
            [0, 1, 2, 3, 4, u32::MAX].map(|n| retry_wait_after(&schedule, n)).to_vec();
        assert_eq!(waits, [secs(30), secs(30), secs(60), secs(300), secs(300), secs(300)]);
        assert_eq!(retry_wait_after(&[secs(7)], 0), secs(7));
    }

    #[test]
    fn the_history_page_policy_is_the_numbers_the_pagers_declared() {
        let p = RetryBackoffPolicy::HISTORY_PAGE;
        assert_eq!(
            (p.max_retries, p.initial, p.max),
            (6, Duration::from_secs(1), Duration::from_secs(60))
        );
    }
}
