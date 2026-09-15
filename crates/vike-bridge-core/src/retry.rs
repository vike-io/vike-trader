//! The rate-limited retry *timing skeleton* the venue kline-backfill pagers shared.
//!
//! Companion to [`crate::http::get_raw`]: `get_raw` hoisted the I/O capture (status, `Retry-After`
//! and the weight header, read before draining the body); this hoists the ONE other thing that was
//! byte-identical across the venues' `fetch_page_rate_limited` loops — the backoff cadence itself.
//! Try up to `max_retries + 1` times; on each rate-limited response sleep for
//! `min(retry_after ∨ backoff, max)`, then double the backoff (capped). The exhaustion error reads
//! `"{label} rate-limited: {note} (after {max_retries} retries)"`.
//!
//! What stays PER-VENUE is the *classification* — which HTTP statuses/business codes mean "rate
//! limited", "hard error", or "done" genuinely differ (bybit adds a Cloudflare 403 plus retCode
//! 10006/10018; okx keys off a string `code` 50011/50061). Each venue passes that as an `attempt`
//! closure returning a [`Verdict`]; the closure keeps its exact codes, labels, and body parsing.
//!
//! Deliberately NOT hoisted: the *paging* loops. They are three different shapes — bybit pages
//! FORWARD off each page's newest bar, okx pages BACKWARD off an exclusive `after` cursor, and
//! binance/aster (already shared in their own `family::klines` rung) pages forward while threading a
//! used-weight cooldown (that rung's RETRY does ride this driver, returning `(bars, used_weight)`
//! through the `Verdict::Done` payload). A driver spanning all three pagers would need a direction,
//! a cursor-derive, two done-predicates and an optional weight arm — more parameters than the
//! ~25-line loops it replaces.

use std::time::Duration;

/// The backoff cadence for [`retry_rate_limited`]: attempt up to `max_retries + 1` times; the sleep
/// on a rate-limited response starts at `initial` and doubles each time, capped at `max`. Kept as an
/// explicit struct (not hard-coded) so each venue passes its own constants — they happen to match
/// today (6 / 1s / 60s) but stay independently declared per venue.
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
/// A hard error (transport failure, non-retryable HTTP status, unparseable body) is NOT a variant
/// here: the closure returns `Err(String)` for those and [`retry_rate_limited`] surfaces it
/// immediately without retrying — mirroring the original loops' early `return Err(..)`.
pub enum Verdict<T> {
    /// The attempt succeeded; `T` is the caller's payload (e.g. the page's bars).
    Done(T),
    /// The venue signalled a rate limit; back off and retry. `retry_after` is the server's honored
    /// `Retry-After` (else the policy's doubling backoff is used); `note` is the diagnostic recorded
    /// for the exhaustion message (e.g. `"HTTP 429 (rate limited)"` / `"retCode 10006 (rate limited)"`).
    RateLimited { retry_after: Option<Duration>, note: String },
}

/// Drive `attempt` under `policy`'s backoff cadence. Returns the first [`Verdict::Done`] payload;
/// propagates the first `Err` immediately (no retry); on exhausting all attempts while still
/// [`Verdict::RateLimited`], returns `Err("{label} rate-limited: {last note} (after {max_retries}
/// retries)")` — byte-identical to the per-venue loops it replaces.
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

    #[test]
    fn retries_then_succeeds() {
        let calls = Cell::new(0u32);
        let out: Result<i32, String> = retry_rate_limited(instant_policy(6), "test", || {
            let n = calls.get() + 1;
            calls.set(n);
            if n < 3 {
                Ok(Verdict::RateLimited { retry_after: None, note: format!("throttle {n}") })
            } else {
                Ok(Verdict::Done(7))
            }
        });
        assert_eq!(out.unwrap(), 7);
        assert_eq!(calls.get(), 3);
    }

    /// Boundary: a `Done` on the very LAST allowed attempt must succeed, not report exhaustion.
    #[test]
    fn succeeds_on_the_final_allowed_attempt() {
        let calls = Cell::new(0u32);
        let out: Result<i32, String> = retry_rate_limited(instant_policy(3), "test", || {
            let n = calls.get() + 1;
            calls.set(n);
            if n < 4 {
                Ok(Verdict::RateLimited { retry_after: None, note: "throttled".to_string() })
            } else {
                Ok(Verdict::Done(1))
            }
        });
        assert_eq!(out.unwrap(), 1);
        assert_eq!(calls.get(), 4, "max_retries + 1 attempts are available");
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
}
