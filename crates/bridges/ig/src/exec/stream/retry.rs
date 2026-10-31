//! The exec trade stream's PURE retry and log-pacing policy: `RetryState::plan` and its bounds.

use std::time::Duration;

use vike_venues::venue_timings;

#[cfg(doc)]
use super::create_session_body;
#[cfg(doc)]
use crate::rest::IgSession;

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
/// step 0 of [`backoff_for_step`], so the FIRST retry of any failure is this long whatever the
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

/// How long a PERSISTENT failure stays QUIET between lines, whichever failure it is.
///
/// ⚠ **This constant is a DISK-SAFETY bound, not a preference**, and the measurement behind it is
/// the `error` half only. On the CI box on 2026-08-23 the permanent-refused arm logged at `error` on
/// EVERY turn of a one-second loop: 53,160 lines and 23 MB in one day from this one loop — dead
/// flat at ~3,290/hour for 17 hours, 99.5% of the box's entire error volume — about a condition its
/// own message calls permanent. The `warn` half was never measured on a box because that day's
/// failure was a refusal, but it is arithmetically WORSE: an unreachable endpoint turns the same
/// flat loop with a warn on every turn — a warn a SECOND, 78,545 lines a day at a 100 ms dial and
/// 86,400 at an instant one — and the CI box's file level is `warn`, so those reach the disk
/// too. Both lanes now speak once on the transition and then only on this interval — which then
/// DECAYS, because the flat 300s that first bounded the flood was still 288 lines a day about a
/// condition already concluded permanent. See [`heartbeat_interval`].
pub(super) const DOWN_HEARTBEAT: Duration = Duration::from_secs(300);

/// How much longer each heartbeat's quiet period is than the one before it.
const DOWN_HEARTBEAT_DECAY: u32 = 3;

/// The ceiling the decaying heartbeat climbs to, and the reason the decay is not a slow fade into
/// silence. Past this the lane speaks hourly forever: an outage that stops being MENTIONED is
/// indistinguishable from one that ended, and telling an operator a lane recovered when it did not
/// is a worse failure than a line an hour.
const MAX_DOWN_HEARTBEAT: Duration = Duration::from_secs(3600);

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
pub(super) enum Say {
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
    /// What this turn should say — see [`Say`].
    pub(super) say: Say,
    /// How long to wait before the next dial.
    pub(super) sleep: Duration,
}

/// Whether a PERSISTENT condition should speak on this turn — the answer [`Persistent`] gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Speak {
    /// The condition has just STARTED. Say so, at whatever level it deserves.
    Onset {
        /// How long the lane will now stay quiet, so the line can name when the next is due.
        next_in: Duration,
    },
    /// The condition is still on and its interval has elapsed. `down_for` is measured from onset;
    /// `next_in` is the DECAYED interval that follows this line, not the one that preceded it.
    Heartbeat { down_for: Duration, next_in: Duration },
    /// Still on, interval not elapsed. Say nothing.
    Quiet,
}

/// The rate limiter for ONE persistent condition: speak on the transition, then on a DECAYING
/// interval, and forget everything the moment the condition clears.
///
/// Shared by the two conditions that can pin this loop indefinitely — a rejected account and an
/// unreachable gateway — because they wear the same flood shape and had better not grow two
/// implementations of the same bound.
///
/// ⚠ **The decay is why `steps` exists, and the CAP is why the decay is safe.** A flat interval is
/// the wrong shape for a state the driver has already concluded is permanent: a lane down for
/// seventeen hours does not need reminding every five minutes. But an interval that grew without a
/// ceiling would eventually be indistinguishable from the lane having RECOVERED, which is the exact
/// failure the heartbeat exists to prevent — so it climbs to [`MAX_DOWN_HEARTBEAT`] and stops.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Persistent {
    /// When the condition began; `None` while it is not on.
    since: Option<Duration>,
    /// When it last said anything, i.e. what the interval is measured FROM. Measuring from `since`
    /// instead would fire on every turn once the outage outlived one interval — the flood wearing
    /// a rate limit.
    last_said: Option<Duration>,
    /// How many HEARTBEATS have been emitted for this outage (the onset is not one). Indexes the
    /// decay ladder, and is reset by [`Persistent::clear`] so a SECOND outage starts responsive
    /// rather than inheriting an hour-long interval from the first.
    steps: u32,
}

impl Persistent {
    /// Fold one turn on which the condition HOLDS.
    fn tick(&mut self, now: Duration) -> Speak {
        match (self.since, self.last_said) {
            (Some(since), Some(said)) => {
                if now.saturating_sub(said) >= heartbeat_interval(self.steps) {
                    self.last_said = Some(now);
                    self.steps = self.steps.saturating_add(1);
                    Speak::Heartbeat {
                        down_for: now.saturating_sub(since),
                        next_in: heartbeat_interval(self.steps),
                    }
                } else {
                    Speak::Quiet
                }
            }
            _ => {
                self.since = Some(now);
                self.last_said = Some(now);
                self.steps = 0;
                Speak::Onset { next_in: heartbeat_interval(0) }
            }
        }
    }

    /// The condition cleared. The next occurrence is a NEW outage: its own onset line, and its own
    /// responsive first interval.
    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The quiet period that FOLLOWS the `steps`-th heartbeat of one outage (`steps == 0` is the period
/// after the onset line). Geometric from [`DOWN_HEARTBEAT`] by [`DOWN_HEARTBEAT_DECAY`], capped at
/// [`MAX_DOWN_HEARTBEAT`]. Total (saturating) — an outage long enough to overflow the power returns
/// the cap, never a short interval.
fn heartbeat_interval(steps: u32) -> Duration {
    // `checked_pow` is `None` past the width, and `checked_mul` `None` past `Duration`'s range;
    // both fall through to the ceiling, which is where the `min` would have put them anyway.
    let factor = DOWN_HEARTBEAT_DECAY.checked_pow(steps).unwrap_or(u32::MAX);
    DOWN_HEARTBEAT.checked_mul(factor).unwrap_or(MAX_DOWN_HEARTBEAT).min(MAX_DOWN_HEARTBEAT)
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
#[derive(Debug, Default)]
pub(super) struct RetryState {
    /// Consecutive refused handshakes. Saturates rather than wrapping — a wrap would drop the
    /// driver back into the re-login path and point a login storm at the one endpoint IG meters
    /// per account.
    refused_streak: u32,
    /// The permanent-refused condition's rate limiter.
    refused_down: Persistent,
    /// Consecutive dials that never produced a session. Saturates, for the same reason.
    dial_streak: u32,
    /// The cannot-open-a-session condition's rate limiter.
    dial_down: Persistent,
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
                    DialOutcome::Dropped => Say::Dropped,
                    _ => Say::Nothing,
                };
                RetryPlan { relogin: false, say, sleep: RECONNECT_DELAY }
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
                    Speak::Onset { next_in } => Say::DialFailed { attempt, next_in },
                    Speak::Heartbeat { down_for, next_in } => {
                        Say::DialStillFailing { attempt, down_for, next_in }
                    }
                    Speak::Quiet => Say::Nothing,
                };
                RetryPlan { relogin: false, say, sleep: dial_backoff(attempt) }
            }
            DialOutcome::Refused => {
                // A `CONERR` is the gateway ANSWERING, so it clears the unreachable condition.
                self.dial_streak = 0;
                self.dial_down.clear();
                self.refused_streak = self.refused_streak.saturating_add(1);
                let streak = self.refused_streak;
                if streak <= MAX_REFUSED_HANDSHAKES {
                    // Still inside the budget: this may be nothing worse than an aged-out token
                    // pair, which only a fresh login cures. Flat delay, warn, re-authenticate.
                    return RetryPlan {
                        relogin: true,
                        say: Say::RefusedRetryingLogin { attempt: streak },
                        sleep: RECONNECT_DELAY,
                    };
                }
                // Past the budget the refusal has survived a fresh login, so it is a rejected
                // account rather than an expiry. Keep dialling (a venue-side outage does recover),
                // but back off and speak on a schedule instead of once per turn.
                let say = match self.refused_down.tick(now) {
                    Speak::Onset { next_in } => Say::FillLaneDown { streak, next_in },
                    Speak::Heartbeat { down_for, next_in } => {
                        Say::FillLaneStillDown { streak, down_for, next_in }
                    }
                    Speak::Quiet => Say::Nothing,
                };
                RetryPlan { relogin: false, say, sleep: refused_backoff(streak) }
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

/// Exponential backoff by STEP: step 0 is [`RECONNECT_DELAY`], each step doubles, and the whole
/// thing is capped at [`MAX_RECONNECT_BACKOFF`]. Total (saturating) — a step large enough to
/// overflow the shift returns the cap, never a short delay, because an arithmetic wrap here would
/// hand back a one-second retry and with it the flood this file exists to bound.
fn backoff_for_step(step: u32) -> Duration {
    // `checked_shl` is `None` past the width, and `checked_mul` `None` past `Duration`'s range;
    // both fall through to the ceiling, which is where the `min` would have put them anyway.
    let factor = 1u32.checked_shl(step).unwrap_or(u32::MAX);
    RECONNECT_DELAY.checked_mul(factor).unwrap_or(MAX_RECONNECT_BACKOFF).min(MAX_RECONNECT_BACKOFF)
}

/// The dial delay for a refusal that has outlived [`MAX_REFUSED_HANDSHAKES`]. The bounded
/// re-login attempts keep the flat delay; the first turn PAST them is step 0.
fn refused_backoff(streak: u32) -> Duration {
    backoff_for_step(streak.saturating_sub(MAX_REFUSED_HANDSHAKES + 1))
}

/// The delay after a dial that never produced a session. Unlike a refusal there is no budget to
/// spend first — nothing was established, so nothing is worth re-trying at speed — and the very
/// first failure is step 0, i.e. still [`RECONNECT_DELAY`].
fn dial_backoff(attempt: u32) -> Duration {
    backoff_for_step(attempt.saturating_sub(1))
}

#[path = "retry_tests.rs"]
#[cfg(test)]
mod retry_tests;
