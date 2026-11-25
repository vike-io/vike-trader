//! The exec trade stream's PURE retry and log-pacing policy: `RetryState::plan` and its bounds. The log pace of a
//! persistent failure is the shared one, `vike_bridge_core::websocket_session::user::reconnect`'s [`RECONNECT_LOG`], lifted from
//! this file (it carries this lane's measurement), and so is the wait before the next dial, the shared
//! [`ReconnectBackoff`] in its refusal-budget mode with a reset on every session that ran (owner ruling B-13: one
//! rule, each venue's values).

use std::time::Duration;

use vike_bridge_core::websocket_session::user::reconnect::{
    RECONNECT_LOG, ReconnectLog, ReconnectLogLine,
};
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_timings;

#[cfg(doc)]
use super::create_session_body;
#[cfg(doc)]
use crate::transport::IgSession;

/// How many CONSECUTIVE refused handshakes are allowed before the driver stops trying to fix the
/// problem by re-authenticating and says so at `error` instead. A refusal that survives a fresh
/// login is not an expiry, and repeating the login cannot cure it. The driver keeps RECONNECTING
/// after that (a venue-side outage does recover), it just stops re-logging-in.
///
/// ⚠ **It does NOT follow that the account was rejected, and this doc and the `error` line both
/// used to say it did.** A `CONERR` is the server refusing the SESSION REQUEST, and the request
/// carries more than credentials — `CONERR,71` is `License not valid for this Client type`, i.e.
/// the `LS_cid` this client sends, which no credential change can fix. That is not hypothetical:
/// this lane sent a made-up `LS_cid` from the day it was written (see [`create_session_body`]), so
/// the one refusal ever observed here was almost certainly OUR bug being correctly reported while
/// this text sent every reader after the credentials. **A driver that cannot distinguish causes
/// must report evidence, not verdicts.**
pub(super) const MAX_REFUSED_HANDSHAKES: u32 = 3;

/// The delay between dials while the driver still believes the NEXT attempt can work — a clean end,
/// a session that streamed and then dropped, or a refusal a re-login may still cure. It is also
/// the first step of the backoff, so the FIRST retry of any failure is this long whatever the
/// failure was.
///
/// The `initial` of the exec lane's `reconnect` policy in `vike_venues::venue_timings::IG`,
/// which carries the reason; [`MAX_RECONNECT_BACKOFF`] is the same policy's `max`.
const RECONNECT_DELAY: Duration = venue_timings::IG.exec().reconnect().initial;

/// The ceiling every backoff in this file climbs to. It exists because neither a rejected account
/// nor an unreachable gateway heals in the next second, and it is a CEILING rather than a stop
/// because both DO recover eventually — the driver keeps dialling at this cadence forever.
///
/// The `max` of the exec lane's `reconnect` policy in `vike_venues::venue_timings::IG`.
const MAX_RECONNECT_BACKOFF: Duration = venue_timings::IG.exec().reconnect().max;

/// One dial's outcome, reduced to the ONLY thing the retry decision keys off.
///
/// The error STRING is deliberately absent: it is a message, not a decision input. Keeping it out
/// is what lets [`RetryState::plan`] be a pure function of `(outcome, streak, elapsed)` and be
/// tested exhaustively without an HTTP server in front of it — the dial itself goes through `ureq`
/// to a live IG endpoint, so the loop's LOGGING policy could not otherwise be proven at all.
///
/// ⚠ **[`DialOutcome::Dropped`] and [`DialOutcome::NeverEstablished`] were ONE variant** (a
/// `Transient`, warned about on every turn at a flat delay) and splitting them is a fix, not
/// tidiness — see [`DialOutcome::Dropped`]'s own doc for why one of them is self-limiting and the
/// other is a flood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DialOutcome {
    /// The session ran and ended cleanly (stop asked, or the server asked for a rebind).
    Ok,
    /// A session that RAN — CONOK, subscribed, streaming — and then died on a read.
    ///
    /// ⚠ **This is the one transport failure that is genuinely self-limiting, and the limit is
    /// narrower than it looks.** Reaching here costs a full handshake and a subscription, so the
    /// loop cannot spin at dial speed. That is why this path keeps warning on EVERY turn at the
    /// flat [`RECONNECT_DELAY`] — a drop is real news each time it happens. The DECLARED residual:
    /// a gateway that accepts a session and drops it instantly, forever, would still warn per turn.
    /// Nothing observed does that, and the cure would be a third streak keyed on how long the
    /// session lasted; it is written down here rather than assumed away.
    Dropped,
    /// The dial never produced a subscribed session at all — the POST never connected, the body was
    /// empty or unreadable, the first line was not a `CONOK`, or the subscribe POST failed.
    ///
    /// ⚠ **Nothing is spent here, so nothing self-limits.** An unreachable / DNS-broken /
    /// TLS-broken / hard-down IG endpoint fails this way in milliseconds, forever — which at a flat
    /// delay is a warn a second. It gets its own streak, backoff and heartbeat.
    NeverEstablished,
    /// The server REFUSED the handshake (`CONERR`).
    Refused,
}

/// What ONE turn of the reconnect loop should SAY. Each variant names a level and a rate, and the
/// loop's only job is to render it — no branch in the loop decides whether to speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RetrySay {
    /// Nothing. The condition has not changed and its heartbeat is not due.
    Nothing,
    /// `warn`: a session that STREAMED then dropped. Not rate-limited, and
    /// [`DialOutcome::Dropped`] carries the argument for why — including the residual that argument
    /// does not cover.
    Dropped,
    /// `warn`, exactly ONCE, on the TRANSITION into "cannot open a session at all".
    DialFailed { attempt: u32, next_in: Duration },
    /// `warn`, on the DECAYING heartbeat, while that state persists.
    DialStillFailing { attempt: u32, down_for: Duration, next_in: Duration },
    /// `warn`: refused, and a re-login may still cure it. Carries the attempt number.
    RefusedRetryingLogin { attempt: u32 },
    /// `error`, exactly ONCE, on the TRANSITION into the permanent-refused state.
    FillLaneDown { streak: u32, next_in: Duration },
    /// `error`, on the DECAYING heartbeat, while that state persists — so an operator still sees
    /// the lane is down without the flood the transition line replaced.
    ///
    /// ⚠ Every one of these carries `next_in` for a reason: with a decaying cadence, a reader who
    /// does not know when the next line is due cannot tell a lengthening quiet period from a lane
    /// that came back.
    FillLaneStillDown { streak: u32, down_for: Duration, next_in: Duration },
}

/// What one turn of the reconnect loop must DO, decided with no clock, no socket and no logger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RetryPlan {
    /// Re-authenticate the [`IgSession`] before the next dial.
    pub(super) relogin: bool,
    /// What this turn should say — see [`RetrySay`].
    pub(super) say: RetrySay,
    /// How long to wait before the next dial.
    pub(super) sleep: Duration,
}

/// The reconnect loop's whole memory, and the whole of its retry/log POLICY.
///
/// ⚠ **Pure by construction**: [`RetryState::plan`] takes the monotonic reading it should reason
/// about rather than reading a clock, which is what makes the heartbeat's rate limit and the
/// backoff's growth assertable in unit tests instead of being a property of a running daemon.
///
/// Two independent conditions are tracked, because they fail independently and each must clear the
/// other: a `CONERR` proves the gateway is REACHABLE, and a failed dial proves nothing about the
/// credentials.
#[derive(Debug)]
pub(super) struct RetryState {
    /// Consecutive refused handshakes. Saturates rather than wrapping — a wrap would drop the
    /// driver back into the re-login path and point a login storm at the one endpoint IG meters
    /// per account.
    refused_streak: u32,
    /// The permanent-refused condition's log pace ([`RECONNECT_LOG`]).
    refused_down: ReconnectLog,
    /// Consecutive dials that never produced a session. Saturates, for the same reason.
    dial_streak: u32,
    /// The cannot-open-a-session condition's log pace ([`RECONNECT_LOG`]).
    dial_down: ReconnectLog,
    /// The wait before the next dial: flat inside the re-login budget, the ladder past it and for a dial that never
    /// produced a session, each kind its own run, and every session that ran starting it over.
    backoff: ReconnectBackoff,
}

impl Default for RetryState {
    /// Nothing failing: both streaks at zero, both conditions off, each on the shared pace, the backoff at its start.
    fn default() -> Self {
        Self {
            refused_streak: 0,
            refused_down: RECONNECT_LOG,
            dial_streak: 0,
            dial_down: RECONNECT_LOG,
            backoff: ReconnectBackoff::new(RECONNECT_DELAY, MAX_RECONNECT_BACKOFF)
                .with_refusal_budget(MAX_REFUSED_HANDSHAKES)
                .with_reset_after(Duration::ZERO),
        }
    }
}

impl RetryState {
    /// Fold one dial outcome into the state and answer what the loop should do about it.
    /// `now` is any monotonically non-decreasing reading (the driver passes its own uptime).
    pub(super) fn plan(&mut self, outcome: DialOutcome, now: Duration) -> RetryPlan {
        match outcome {
            // A session that RAN — cleanly, or into a mid-stream drop — clears BOTH conditions, so
            // whichever fails next starts a new streak and gets its own transition line. `Ok` says
            // nothing (it always did); a drop warns every time (see `DialOutcome::Dropped`).
            DialOutcome::Ok | DialOutcome::Dropped => {
                self.clear();
                let say = match outcome {
                    DialOutcome::Dropped => RetrySay::Dropped,
                    _ => RetrySay::Nothing,
                };
                // A session that ran, however briefly, starts the ladder over: its end waits the first step.
                RetryPlan { relogin: false, say, sleep: self.backoff.ended(Duration::ZERO) }
            }
            DialOutcome::NeverEstablished => {
                // A failed dial says nothing about the credentials, so the refusal streak resets —
                // exactly as it did when this and `Dropped` were one `Transient` variant.
                self.refused_streak = 0;
                self.refused_down.clear();
                // There is no budget to spend first, unlike a refusal: no login can cure an
                // endpoint that never answered, so the very first failure is already step 0 of the
                // backoff and already the transition line.
                self.dial_streak = self.dial_streak.saturating_add(1);
                let attempt = self.dial_streak;
                let say = match self.dial_down.tick(now) {
                    ReconnectLogLine::Onset { next_in } => {
                        RetrySay::DialFailed { attempt, next_in }
                    }
                    ReconnectLogLine::Heartbeat { down_for, next_in } => {
                        RetrySay::DialStillFailing { attempt, down_for, next_in }
                    }
                    ReconnectLogLine::Quiet => RetrySay::Nothing,
                };
                RetryPlan { relogin: false, say, sleep: self.backoff.failed() }
            }
            DialOutcome::Refused => {
                // A `CONERR` is the gateway ANSWERING, so it clears the unreachable condition.
                self.dial_streak = 0;
                self.dial_down.clear();
                self.refused_streak = self.refused_streak.saturating_add(1);
                let streak = self.refused_streak;
                // Flat inside the budget, the ladder from its first step past it (the backoff's budget is this one).
                let sleep = self.backoff.refused();
                if streak <= MAX_REFUSED_HANDSHAKES {
                    // Still inside the budget: this may be nothing worse than an aged-out token
                    // pair, which only a fresh login cures. Flat delay, warn, re-authenticate.
                    return RetryPlan {
                        relogin: true,
                        say: RetrySay::RefusedRetryingLogin { attempt: streak },
                        sleep,
                    };
                }
                // Past the budget the refusal has survived a fresh login, so it is a rejected
                // account rather than an expiry. Keep dialling (a venue-side outage does recover),
                // but back off and speak on a schedule instead of once per turn.
                let say = match self.refused_down.tick(now) {
                    ReconnectLogLine::Onset { next_in } => {
                        RetrySay::FillLaneDown { streak, next_in }
                    }
                    ReconnectLogLine::Heartbeat { down_for, next_in } => {
                        RetrySay::FillLaneStillDown { streak, down_for, next_in }
                    }
                    ReconnectLogLine::Quiet => RetrySay::Nothing,
                };
                RetryPlan { relogin: false, say, sleep }
            }
        }
    }

    /// Forget both conditions — called whenever a session actually ran.
    fn clear(&mut self) {
        self.refused_streak = 0;
        self.refused_down.clear();
        self.dial_streak = 0;
        self.dial_down.clear();
    }
}

#[path = "retry_tests.rs"]
#[cfg(test)]
mod retry_tests;
