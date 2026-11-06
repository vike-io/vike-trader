//! The rate-limited retry TIMING skeleton the venue kline/trade pagers share (the I/O capture half
//! is `http::get_raw`). Try up to `max_retries + 1` times; on each rate-limited response
//! sleep `min(retry_after ∨ backoff, max)`, then double the backoff (capped). The exhaustion error
//! reads `"{label} rate-limited: {note} (after {max_retries} retries)"`.
//!
//! The CLASSIFICATION stays per venue (bybit adds a Cloudflare 403 plus retCode 10006/10018; okx
//! keys off a string `code` 50011/50061): each passes an `attempt` closure returning a [`Verdict`].
//! A JSON pager's attempt judges its [`RawResp`] through [`json_page_verdict`], handing in its own
//! rate-limit statuses, its business-code read and its parse.
//! The PAGING loops stay per venue too (forward, backward off a cursor, or forward with a weight
//! cooldown): one driver over all of them would need more parameters than the loops it replaces.
//!
//! [`retry_scheduled`] is the other shape: a GET of a FILE on a fixed schedule of waits (dukascopy's
//! hour files, fxcm's candle archive, CDNs that fail in bursts), over an injected attempt and sleep
//! so the policy runs with neither a network nor a clock. Each venue words its own error.

use std::time::Duration;

use serde_json::Value;

use crate::http::{RawResp, body_head, json_body};

/// One attempt of a [`retry_scheduled`] GET.
#[derive(Debug)]
pub enum Attempt {
    /// The server answered: its status, the body (read only for a 2xx) and the wait its
    /// `Retry-After` header stated in seconds, if it stated one.
    Response { status: u16, body: Vec<u8>, retry_after: Option<Duration> },
    /// No usable answer at all — DNS, connect, TLS, a reset, a timeout, a body cut off mid-read.
    Failed(String),
}

/// A transient failure of one [`Attempt`], as [`GaveUp::Exhausted`] reports the last one.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// A 429, 500, 502, 503 or 504.
    Status(u16),
    /// No usable answer: the attempt's own words.
    Transport(String),
}

/// Why [`retry_scheduled`] gave up.
#[derive(Debug, PartialEq, Eq)]
pub enum GaveUp {
    /// A status that is not 2xx, not 404 and not transient: failed at once, never retried.
    Refused(u16),
    /// Every attempt failed transiently; the LAST failure.
    Exhausted(Failure),
}

/// Drive `attempt` on the `delays` schedule: `delays.len() + 1` attempts in all. A 404 is `Ok(None)`
/// (never retried: an absent file stays absent), a 2xx is its body, any other non-transient status
/// fails at once; a transient status or a failed attempt waits out the next of `delays` —
/// lengthened, never shortened, to a `Retry-After` up to `retry_after_cap` — and tries again. The
/// last failure is not slept after: there is nothing left to wait for.
pub fn retry_scheduled(
    delays: &[Duration],
    retry_after_cap: Duration,
    mut attempt: impl FnMut() -> Attempt,
    mut sleep: impl FnMut(Duration),
) -> Result<Option<Vec<u8>>, GaveUp> {
    let mut waits = delays.iter();
    loop {
        let (failure, stated) = match attempt() {
            Attempt::Response { status: 404, .. } => return Ok(None),
            Attempt::Response { status, body, .. } if (200..300).contains(&status) => {
                return Ok(Some(body));
            }
            Attempt::Response { status, retry_after, .. } => {
                if !matches!(status, 429 | 500 | 502 | 503 | 504) {
                    return Err(GaveUp::Refused(status));
                }
                let stated = retry_after.unwrap_or(Duration::ZERO).min(retry_after_cap);
                (Failure::Status(status), stated)
            }
            Attempt::Failed(why) => (Failure::Transport(why), Duration::ZERO),
        };
        let Some(delay) = waits.next() else { return Err(GaveUp::Exhausted(failure)) };
        sleep((*delay).max(stated));
    }
}

/// The backoff cadence for [`retry_rate_limited`]: up to `max_retries + 1` attempts; the sleep
/// starts at `initial` and doubles, capped at `max`. Each venue declares its own constants.
#[derive(Clone, Copy, Debug)]
pub struct BackoffPolicy {
    /// Retries AFTER the first try (so the loop runs `max_retries + 1` attempts total).
    pub max_retries: u32,
    /// The first backoff sleep, before any doubling.
    pub initial: Duration,
    /// The ceiling for both the doubling backoff and a server-supplied `Retry-After`.
    pub max: Duration,
}

/// One attempt's outcome, as classified by the venue's `attempt` closure.
///
/// A hard error (transport failure, non-retryable status, unparseable body) is NOT a variant: the
/// closure returns `Err(String)` and [`retry_rate_limited`] surfaces it at once, without retrying.
pub enum Verdict<T> {
    /// The attempt succeeded; `T` is the caller's payload (e.g. the page's bars).
    Done(T),
    /// The venue signalled a rate limit; back off and retry. `retry_after` is the server's
    /// `Retry-After` (else the policy's doubling backoff); `note` is recorded for the exhaustion
    /// message (e.g. `"HTTP 429 (rate limited)"`, `"retCode 10006 (rate limited)"`).
    RateLimited { retry_after: Option<Duration>, note: String },
}

/// Drive `attempt` under `policy`'s backoff cadence. Returns the first [`Verdict::Done`] payload;
/// propagates the first `Err` at once; on exhausting every attempt while still
/// [`Verdict::RateLimited`], returns the module doc's exhaustion error with the LAST note.
pub fn retry_rate_limited<T>(
    policy: BackoffPolicy,
    label: &str,
    mut attempt: impl FnMut() -> Result<Verdict<T>, String>,
) -> Result<T, String> {
    let mut backoff = policy.initial;
    let mut last = String::new();
    for _ in 0..=policy.max_retries {
        match attempt()? {
            Verdict::Done(value) => return Ok(value),
            Verdict::RateLimited { retry_after, note } => {
                last = note;
                let wait = retry_after.unwrap_or(backoff).min(policy.max);
                std::thread::sleep(wait);
                backoff = (backoff * 2).min(policy.max);
            }
        }
    }
    Err(format!("{label} rate-limited: {last} (after {} retries)", policy.max_retries))
}

/// One JSON page's [`Verdict`], judged in the order the bybit, deribit and okx pagers always judged
/// it: a status in `limited` is a rate limit (`"HTTP {status} (rate limited)"`); any other non-2xx is
/// the hard `"{label} HTTP {status}: {head}"`; a body that is not JSON is [`json_body`]'s error over
/// `json_what`; a body `business_limit` names a rate limit (`Some(note)`: those venues answer one with
/// HTTP 200) is one; anything else is `parse`d. Each venue's statuses, words and codes stay its own.
pub fn json_page_verdict<T>(
    raw: RawResp,
    label: &str,
    json_what: &str,
    limited: &[u16],
    business_limit: impl FnOnce(&Value) -> Option<String>,
    parse: impl FnOnce(&Value) -> Result<T, String>,
) -> Result<Verdict<T>, String> {
    if limited.contains(&raw.status) {
        let note = format!("HTTP {} (rate limited)", raw.status);
        return Ok(Verdict::RateLimited { retry_after: raw.retry_after, note });
    }
    if !(200..300).contains(&raw.status) {
        return Err(format!("{label} HTTP {}: {}", raw.status, body_head(&raw.body)));
    }
    let v = json_body(&raw.body, json_what)?;
    if let Some(note) = business_limit(&v) {
        return Ok(Verdict::RateLimited { retry_after: raw.retry_after, note });
    }
    parse(&v).map(Verdict::Done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    // A tiny policy with zero-duration sleeps so the tests don't actually wait.
    fn instant_policy(max_retries: u32) -> BackoffPolicy {
        BackoffPolicy { max_retries, initial: Duration::ZERO, max: Duration::ZERO }
    }

    #[test]
    fn returns_first_done_without_further_attempts() {
        let calls = Cell::new(0u32);
        let out: Result<i32, String> = retry_rate_limited(instant_policy(6), "test", || {
            calls.set(calls.get() + 1);
            Ok(Verdict::Done(42))
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
                Ok(Verdict::RateLimited { retry_after: None, note: format!("throttle {n}") })
            } else {
                Ok(Verdict::Done(n))
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
        let run = |script: Vec<Attempt>| {
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
        let status = |status: u16, retry_after: Option<u64>| Attempt::Response {
            status,
            body: vec![7],
            retry_after: retry_after.map(secs),
        };
        assert_eq!(
            run(vec![status(503, Some(30)), Attempt::Failed("reset".into()), status(200, None)]),
            (Ok(Some(vec![7])), 3, vec![secs(30), secs(10)])
        );
        assert_eq!(
            run(vec![status(429, Some(3_600)), status(404, None)]),
            (Ok(None), 2, vec![secs(60)])
        );
        assert_eq!(run(vec![status(403, None)]), (Err(GaveUp::Refused(403)), 1, vec![]));
        assert_eq!(
            run(vec![status(503, None), Attempt::Failed("reset".into()), status(502, Some(1))]),
            (Err(GaveUp::Exhausted(Failure::Status(502))), 3, vec![secs(4), secs(10)])
        );
        assert_eq!(
            run(vec![status(500, None), status(500, None), Attempt::Failed("timeout".into())]),
            (
                Err(GaveUp::Exhausted(Failure::Transport("timeout".into()))),
                3,
                vec![secs(4), secs(10)]
            )
        );
    }

    #[test]
    fn exhaustion_message_carries_last_note_and_count() {
        // max_retries = 2 ⇒ 3 attempts total, all rate-limited ⇒ exhaustion.
        let calls = Cell::new(0u32);
        let out: Result<i32, String> = retry_rate_limited(instant_policy(2), "okx candles", || {
            calls.set(calls.get() + 1);
            Ok(Verdict::RateLimited {
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

    fn raw(status: u16, body: &str) -> RawResp {
        RawResp {
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
        fn judge(status: u16, body: &str, limited: &[u16]) -> Result<Verdict<Value>, String> {
            let code = |v: &Value| {
                let code = v.get("retCode").and_then(Value::as_i64).unwrap_or(-1);
                [10006, 10018].contains(&code).then(|| format!("retCode {code} (rate limited)"))
            };
            let rows = |v: &Value| Ok(v["rows"].clone());
            json_page_verdict(raw(status, body), "bybit klines", "bybit kline", limited, code, rows)
        }
        fn as_limit(v: Result<Verdict<Value>, String>) -> Option<(Option<Duration>, String)> {
            match v {
                Ok(Verdict::RateLimited { retry_after, note }) => Some((retry_after, note)),
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
            Ok(Verdict::Done(v)) => assert_eq!(v, serde_json::json!([1])),
            _ => panic!("a readable page is parsed"),
        }
    }
}
