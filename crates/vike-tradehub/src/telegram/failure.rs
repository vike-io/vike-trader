//! What a FAILED `getUpdates` means, and what the poller loop does about it — the one place
//! "retrying could help" is separated from "retrying can never help".
//!
//! # The defect this file exists for
//!
//! [`crate::telegram::poll_once`] used to answer every `getUpdates` failure the same way: one
//! `tracing::warn!` and return, whereupon the loop napped [`POLL_GAP`](super::POLL_GAP) and asked
//! again. That is correct for a network blip and catastrophic for a wrong bot token, because a wrong
//! token fails INSTANTLY — there is no 20 s long-poll to wait through — so the loop settles at
//! ~2 requests/second against `api.telegram.org`, forever. Measured on a clean install with a
//! mistyped `VIKE_TELEGRAM_BOT_TOKEN`: **19 warnings in 10 s**, i.e. ~164,000 outbound third-party
//! requests and ~164,000 WARN lines per day (~37 MB), at `warn` — the exact level
//! `docs/ops/tradehub-the CI box.md` tells operators to set `VIKE_LOG_FILE_LEVEL` to, so the recommended
//! configuration could not turn it down.
//!
//! # The policy, in one table
//!
//! | verdict | statuses | what happens |
//! |---|---|---|
//! | **permanent** | 401, 403, 404 | ONE `error!`, the poller STOPS. 1 request, 1 line, ever. |
//! | **transient** | everything else (5xx, 429, 409, transport, unreadable body, bad JSON) | exponential backoff to a [`BACKOFF_MAX`] ceiling, announcements throttled to consecutive failures 1, 2, 4, 8, … |
//!
//! **Permanent means the CREDENTIAL was rejected, and stopping is `maybe_spawn`'s own refusal moved
//! to where the evidence actually arrives.** `crates/vike-tradehub/src/telegram/ledger.rs` already
//! establishes the shape: a control channel whose precondition cannot be met does not exist —
//! [`crate::telegram::maybe_spawn`] logs one `error!` and returns `None` while the daemon keeps
//! trading headless. An unusable ledger is provable at mount; a rejected token is not, because
//! proving it would mean a network call inside `maybe_spawn` — and a mount-time probe still could
//! not catch the token being REVOKED under a running daemon, which is the same outage. So the
//! refusal fires on the first pass that carries the evidence, and it is the identical end state: no
//! channel, one loud line, a daemon that keeps trading.
//!
//! The verdict is trustworthy because it is AUTHENTICATED: `getUpdates` is an HTTPS GET to
//! `api.telegram.org`, so a 401/403/404 reaching [`PollError::from_status`] was signed by Telegram's
//! certificate and cannot be a captive portal or an intercepting proxy inventing one. What it CAN
//! be — an operator fixing the token and expecting the running daemon to pick it up — is answered by
//! the error line naming the restart, because the token is read once at mount and nothing re-reads
//! it.
//!
//! ⚠ **409 is deliberately TRANSIENT.** Telegram answers `409 Conflict` for "terminated by other
//! getUpdates request" (two daemons polling one token — which resolves the moment the other stops)
//! and for "can't use getUpdates while webhook is active" (which resolves when an operator deletes
//! the webhook, no restart of this daemon required). Neither is a rejected credential, and the
//! backoff below already bounds the cost of being wrong to one request/minute.
//!
//! ⚠ **A transient failure is never SILENT.** An operator whose channel has stopped working needs
//! to know, so the throttle is logarithmic rather than a mute: failures 1, 2, 4, 8, … announce,
//! which is immediate at the start of an outage and ~11 lines across a full day of one.

use std::time::Duration;

use super::POLL_GAP;

/// The ceiling the retry backoff walks up to and stays at. One minute: long enough that a
/// multi-hour outage costs ~60 requests/hour instead of ~7,200, short enough that a channel an
/// operator has just repaired is live again within a minute without a restart.
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Why one `getUpdates` failed — and the only thing the poller branches on: whether asking again
/// could ever produce a different answer.
///
/// The classification is made where the HTTP status is KNOWN
/// (`crates/vike-tradehub/src/telegram/deps.rs`'s `ProdTelegramDeps`), never re-derived by sniffing
/// a message string: a `contains("404")` test would silently start passing the day a message is
/// reworded, and this decision arms or disarms a remote order surface.
///
/// `message` is operator-facing and, like everything else in this module tree, **never carries the
/// URL or the bot token** — see `crates/vike-tradehub/src/telegram/deps.rs`'s module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollError {
    /// Operator-facing text. Never the URL, never the token, never a raw transport error (whose
    /// `Display` can echo the token-bearing request target).
    pub message: String,
    /// `true` ⇒ retrying is pointless; the bot token itself was rejected.
    pub permanent: bool,
}

impl PollError {
    /// A failure that MIGHT succeed next time: a 5xx, a rate limit, a dead socket, an unreadable
    /// body, malformed JSON.
    pub fn transient(message: impl Into<String>) -> Self {
        PollError { message: message.into(), permanent: false }
    }

    /// A failure that can NEVER succeed by trying again — reserved for a rejected credential.
    pub fn permanent(message: impl Into<String>) -> Self {
        PollError { message: message.into(), permanent: true }
    }

    /// Classify a non-2xx `getUpdates` response by its HTTP status.
    pub fn from_status(status: u16) -> Self {
        PollError {
            message: format!("getUpdates: http {status}"),
            permanent: status_is_permanent(status),
        }
    }
}

/// The statuses on which `getUpdates` will never succeed by trying again.
///
/// * **401 Unauthorized** — Telegram's answer to a token it does not recognise.
/// * **404 Not Found** — a MALFORMED token: `/bot<garbage>/getUpdates` is not a route, so there is
///   no bot to be unauthorized as. This is the shape a typo takes, and the one measured on the
///   clean install that prompted this module.
/// * **403 Forbidden** — the bot exists and is refused (deleted, or banned).
///
/// Everything else — including `409 Conflict` and `429 Too Many Requests` — is transient; see the
/// module doc for why 409 is on that side of the line.
pub fn status_is_permanent(status: u16) -> bool {
    matches!(status, 401 | 403 | 404)
}

/// What the poller loop should do after one pass.
///
/// Returned by [`PollBackoff::observe`], which is PURE — it decides, and the caller
/// (`crates/vike-tradehub/src/telegram/mod.rs`'s `spawn`) both sleeps and logs. Keeping the
/// `tracing` calls out of here is what makes the whole policy — the schedule, the ceiling, the
/// announcement throttle and the stop — assertable in a unit test with no clock and no subscriber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollStep {
    /// The pass did not fail. Nap `wait` (always [`POLL_GAP`](super::POLL_GAP) — the healthy
    /// cadence is untouched by this module) and poll again.
    ///
    /// `recovered_after` is `Some(n)` on the pass that ENDED a run of `n` consecutive failures, so
    /// an outage that was announced can be announced closed exactly once.
    Ok { wait: Duration, recovered_after: Option<u32> },
    /// A transient failure. Nap `wait`, then poll again.
    ///
    /// `announce` is the log throttle: `true` on consecutive failures 1, 2, 4, 8, … and `false`
    /// otherwise, so an outage is reported immediately and a LONG outage still cannot flood.
    Retry { wait: Duration, consecutive: u32, announce: bool },
    /// A permanent failure: STOP the poller. The caller logs once, loudly, and leaves the loop.
    Stop,
}

/// The consecutive-failure counter behind the retry schedule and the announcement throttle.
///
/// One `u32` of state, owned by the poller loop and reset by any successful pass.
#[derive(Debug, Default, Clone)]
pub struct PollBackoff {
    /// Consecutive failed passes; 0 whenever the last pass succeeded.
    consecutive: u32,
}

impl PollBackoff {
    /// Fold one pass's outcome in and say what to do next. `None` is a pass that reached Telegram.
    pub fn observe(&mut self, failure: Option<&PollError>) -> PollStep {
        let Some(f) = failure else {
            let recovered_after = (self.consecutive > 0).then_some(self.consecutive);
            self.consecutive = 0;
            return PollStep::Ok { wait: POLL_GAP, recovered_after };
        };
        if f.permanent {
            // No counter bump: there is no next attempt to schedule.
            return PollStep::Stop;
        }
        self.consecutive = self.consecutive.saturating_add(1);
        PollStep::Retry {
            wait: backoff_delay(self.consecutive),
            consecutive: self.consecutive,
            announce: self.consecutive.is_power_of_two(),
        }
    }

    /// Consecutive failures so far (0 when the last pass succeeded) — for tests and for the
    /// caller's log fields.
    pub fn consecutive(&self) -> u32 {
        self.consecutive
    }
}

/// The retry schedule: [`POLL_GAP`](super::POLL_GAP) doubled per consecutive failure, capped at
/// [`BACKOFF_MAX`]. `0.5s, 1s, 2s, 4s, 8s, 16s, 32s, 60s, 60s, …` — the ceiling is reached 63.5 s
/// into an outage, after which the cost of a dead endpoint is one request per minute.
///
/// `consecutive` is 1-based (the first failure is 1). The shift is saturating, so a counter that
/// has run for days cannot overflow it into a short delay.
fn backoff_delay(consecutive: u32) -> Duration {
    let doublings = consecutive.saturating_sub(1).min(u32::BITS - 1);
    POLL_GAP.checked_mul(1u32 << doublings).unwrap_or(BACKOFF_MAX).min(BACKOFF_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠ **The classification, status by status.** These three are what stop a channel, so they are
    /// pinned individually rather than as a range: widening the permanent set silently disarms a
    /// working channel on a transient server error, and narrowing it restores the hot loop.
    #[test]
    fn only_a_rejected_credential_is_permanent() {
        for status in [401, 403, 404] {
            assert!(status_is_permanent(status), "{status} is a rejected bot token");
            assert!(PollError::from_status(status).permanent);
        }
        // Everything a working token can still hit must stay retryable.
        for status in [408, 409, 429, 500, 502, 503, 504, 200, 301] {
            assert!(!status_is_permanent(status), "{status} can succeed on a retry");
            assert!(!PollError::from_status(status).permanent);
        }
    }

    /// The message carries the status and NOTHING that could be a URL or a token.
    #[test]
    fn a_status_error_says_only_the_status() {
        let e = PollError::from_status(404);
        assert_eq!(e.message, "getUpdates: http 404");
        assert!(!e.message.contains("api.telegram.org") && !e.message.contains("bot"));
    }

    /// The healthy path is byte-identical to before this module existed: [`POLL_GAP`] after every
    /// successful pass, no announcement, no state.
    #[test]
    fn a_successful_pass_keeps_the_untouched_cadence() {
        let mut b = PollBackoff::default();
        for _ in 0..5 {
            assert_eq!(b.observe(None), PollStep::Ok { wait: POLL_GAP, recovered_after: None });
        }
        assert_eq!(b.consecutive(), 0);
    }

    /// The schedule, exactly: doubling from [`POLL_GAP`] and PINNED at [`BACKOFF_MAX`] — a ceiling
    /// that is missing turns "back off" into "stop polling forever".
    #[test]
    fn the_backoff_doubles_then_holds_at_the_ceiling() {
        let expected = [500, 1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000, 60_000];
        let mut b = PollBackoff::default();
        for (i, want_ms) in expected.iter().enumerate() {
            let step = b.observe(Some(&PollError::transient("boom")));
            let PollStep::Retry { wait, consecutive, .. } = step else {
                panic!("a transient failure must retry, got {step:?}");
            };
            assert_eq!(wait, Duration::from_millis(*want_ms), "failure #{}", i + 1);
            assert_eq!(consecutive, i as u32 + 1);
        }
        // …and it can never wrap back to a short delay, however long the outage runs.
        assert_eq!(backoff_delay(u32::MAX), BACKOFF_MAX);
        assert_eq!(backoff_delay(1_000_000), BACKOFF_MAX);
    }

    /// A permanent failure STOPS, on the FIRST one — no counter, no schedule, nothing to resume.
    #[test]
    fn a_permanent_failure_stops_immediately() {
        let mut b = PollBackoff::default();
        assert_eq!(b.observe(Some(&PollError::from_status(404))), PollStep::Stop);
        assert_eq!(b.consecutive(), 0, "there is no next attempt to schedule");
    }

    /// One success clears the run, and the pass that clears it says how long the outage was so the
    /// caller can announce the recovery exactly once.
    #[test]
    fn a_success_resets_the_run_and_reports_the_recovery_once() {
        let mut b = PollBackoff::default();
        for _ in 0..3 {
            b.observe(Some(&PollError::transient("boom")));
        }
        assert_eq!(b.observe(None), PollStep::Ok { wait: POLL_GAP, recovered_after: Some(3) });
        // Only the FIRST healthy pass reports it — a recovered channel does not keep saying so.
        assert_eq!(b.observe(None), PollStep::Ok { wait: POLL_GAP, recovered_after: None });
        // …and the next outage starts from the bottom of the schedule, not where the last ended.
        let step = b.observe(Some(&PollError::transient("boom")));
        assert_eq!(step, PollStep::Retry { wait: POLL_GAP, consecutive: 1, announce: true });
    }

    /// ⚠ **THE MEASUREMENT** — the numbers in this file's module doc, computed rather than
    /// asserted by eye. A wrong bot token used to cost ~164,000 requests and ~164,000 WARN lines a
    /// day (~2/s, no backoff, no give-up). This walks a full 24 h of wall clock through the real
    /// state machine and pins what each verdict now costs.
    #[test]
    fn a_day_of_failure_costs_a_bounded_number_of_requests_and_log_lines() {
        const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
        // The volume the un-backed-off loop produced, for the ratio below: an instant failure plus
        // a 500 ms gap is one request every 500 ms.
        const OLD_PER_DAY: u64 = DAY_MS / 500;

        // ── PERMANENT (the measured defect: a mistyped token) ───────────────────────────────
        // One request, one line, ever: the loop is GONE, so there is no horizon to sum over. The
        // old cost of the same 24 h was `OLD_PER_DAY` (172,800) of each.
        let mut b = PollBackoff::default();
        assert_eq!(b.observe(Some(&PollError::from_status(404))), PollStep::Stop);

        // ── TRANSIENT (a genuinely down endpoint) ───────────────────────────────────────────
        let mut b = PollBackoff::default();
        let (mut elapsed_ms, mut requests, mut announced) = (0u64, 0u64, 0u64);
        while elapsed_ms < DAY_MS {
            requests += 1;
            let PollStep::Retry { wait, announce, .. } =
                b.observe(Some(&PollError::transient("http 503")))
            else {
                panic!("a transient failure must never stop the channel");
            };
            if announce {
                announced += 1;
            }
            elapsed_ms += wait.as_millis() as u64;
        }
        assert_eq!(requests, 1_446, "24 h of outage, backed off to the 60 s ceiling");
        assert_eq!(
            announced, 11,
            "consecutive failures 1, 2, 4, … 1024 — never a flood, never mute"
        );
        assert!(
            requests * 100 < OLD_PER_DAY,
            "{requests} requests/day must be under 1% of the old {OLD_PER_DAY}"
        );
        // The channel is still RUNNING: a transient outage must recover on its own.
        assert_eq!(b.observe(None), PollStep::Ok { wait: POLL_GAP, recovered_after: Some(1_446) });
    }
}
