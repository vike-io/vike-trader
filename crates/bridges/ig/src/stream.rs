//! IG Lightstreamer trade-update stream (TLCP text mode) — IG's async fill/terminal lane, which
//! the sync `/confirms` call cannot carry (a resting working order fills LATER).
//!
//! Flow: `POST {endpoint}/lightstreamer/create_session.txt` opens a streaming text body; the first
//! `CONOK,<sessionId>,...,<controlLink>` line names the session, then `POST .../control.txt`
//! subscribes to `TRADE:{accountId}` over the `CONFIRMS OPU WOU` schema, and update lines stream
//! back. Each `CONFIRMS` field is decoded by the PURE [`crate::event_mapper`] (dual-publish fill / cancel
//! / reject) and pushed to the core.
//!
//! Audit A3: the subscription requests a SNAPSHOT (`LS_snapshot=true`), so on a reconnect the
//! current trade state is re-delivered — a fill/terminal that landed during the drop is recovered,
//! and the core's `trade_id`/FSM dedup absorbs the overlap with anything already seen. Blocks — run
//! on a dedicated thread. NOTE: transport correctness (framing, rebind, keepalive) is verifiable
//! only against a live IG Lightstreamer server; the decode/parse layer is unit-tested.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_exec::EventSender;

use crate::event_mapper::{decode_trade_confirm, parse_update_line};
// ⚠ ONE handshake, ONE spelling. This lane used to carry its own `LS_PROTOCOL`, `form` and
// `urlencode` — behaviourally identical copies of `lightstreamer.rs`'s, whose own `form` doc had
// claimed since it was written that `crate::stream` shared it. That untrue claim IS the defect
// shape: two spellings of one wire drifted, and `LS_cid` is where the drift landed. Take a TLCP
// constant or encoder from `lightstreamer.rs`; never re-spell one here.
use crate::lightstreamer::{LS_CID, LS_PROTOCOL, form};
use crate::rest::IgSession;

/// What the exec thread learned at submit about ONE order, for the lanes that run afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    /// The `dealReference` IG answered the submit POST with — the ONLY key `GET /confirms/{ref}`
    /// accepts, and therefore the only way to ask IG about this order again.
    pub deal_ref: String,
    /// Whether the submit was a MARKET deal (a position open, which fills inline) rather than a
    /// working order. `exec::map_confirm` needs it to decide whether a confirm carries a fill.
    pub market: bool,
}

/// The exec thread's submit-time knowledge, shared with everything that has to ask about an order
/// LATER. Both directions are kept because the two readers ask opposite questions and neither can
/// answer with a scan: the Lightstreamer driver holds a `dealReference` and needs the coid, while
/// [`vike_exec::ExecutionClient::confirm`] holds a coid and needs the `dealReference`.
///
/// ⚠ **This used to be the `by_ref` half alone**, which is why an IG order whose `/confirms` call
/// failed could never be re-asked about: nothing in the process could turn its coid back into the
/// one key IG's confirm endpoint takes.
#[derive(Debug, Default)]
pub(crate) struct DealRefs {
    by_ref: HashMap<String, String>,
    by_coid: HashMap<String, Pending>,
}

impl DealRefs {
    /// Record a submitted order's `dealReference` in BOTH directions. Called by the exec thread the
    /// moment the submit POST returns, i.e. BEFORE the confirm is fetched — so a fill that lands in
    /// between still correlates.
    pub(crate) fn record(&mut self, coid: &str, deal_ref: &str, market: bool) {
        self.by_ref.insert(deal_ref.to_string(), coid.to_string());
        self.by_coid.insert(coid.to_string(), Pending { deal_ref: deal_ref.to_string(), market });
    }

    /// The order a streamed `CONFIRMS` belongs to, by the `dealReference` it echoes.
    pub(crate) fn coid_for(&self, deal_ref: &str) -> Option<String> {
        self.by_ref.get(deal_ref).cloned()
    }

    /// What a re-confirm of `coid` needs. `None` = this process never got a `dealReference` for
    /// that order, so there is nothing IG can be asked.
    pub(crate) fn pending_for(&self, coid: &str) -> Option<Pending> {
        self.by_coid.get(coid).cloned()
    }
}

/// Shared submit-time state (populated by exec at submit) — see [`DealRefs`].
pub(crate) type DealRefMap = Arc<Mutex<DealRefs>>;

/// Our fixed subscription id (one subscription per session). Stream-local on purpose: the
/// market-data lane takes its `sub_id` as a parameter because it opens many, this lane opens one.
const SUB_ID: u32 = 1;

/// Parameters the driver needs, sourced from a logged-in [`IgSession`] the driver KEEPS.
///
/// ⚠ **The session is held, not sampled.** This used to be three `String`s including a
/// `password` snapshot taken once at spawn; when IG expired the tokens behind it, every reconnect
/// re-presented the same dead pair and the fill lane stayed down for the life of the process with
/// nothing in the log saying so. `IgSession::ls_password` renders the CURRENT pair, so reading it
/// per dial is what makes [`IgSession::relogin`] reach this lane at all.
pub(crate) struct LsParams {
    pub session: Arc<IgSession>,
    pub endpoint: String,
    pub account_id: String,
}

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
const MAX_REFUSED_HANDSHAKES: u32 = 3;

/// The delay between dials while the driver still believes the NEXT attempt can work — a clean end,
/// a session that streamed and then dropped, or a refusal a re-login may still cure. It is also
/// step 0 of [`backoff_for_step`], so the FIRST retry of any failure is this long whatever the
/// failure was.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// The ceiling every backoff in this file climbs to. It exists because neither a rejected account
/// nor an unreachable gateway heals in the next second, and it is a CEILING rather than a stop
/// because both DO recover eventually — the driver keeps dialling at this cadence forever.
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(60);

/// How long a PERSISTENT failure stays QUIET between lines, whichever failure it is.
///
/// ⚠ **This constant is a DISK-SAFETY bound, not a preference**, and the measurement behind it is
/// the `error` half only. On the CI box on 2026-08-23 the permanent-refused arm logged at `error` on
/// EVERY turn of a one-second loop: 53,160 lines and 23 MB in one day from this one loop — dead
/// flat at ~3,290/hour for 17 hours, 99.5% of the box's entire error volume — about a condition its
/// own message calls permanent. The `warn` half was never measured on a box because that day's
/// failure was a refusal, but it is arithmetically WORSE: an unreachable endpoint turns the same
/// flat loop with a warn on every turn — a warn a SECOND, 78,545 lines a day at a 100 ms dial and
/// 86,400 at an instant one — and the CI box runs `VIKE_LOG_FILE_LEVEL=warn`, so those reach the disk
/// too. Both lanes now speak once on the transition and then only on this interval — which then
/// DECAYS, because the flat 300s that first bounded the flood was still 288 lines a day about a
/// condition already concluded permanent. See [`heartbeat_interval`].
const DOWN_HEARTBEAT: Duration = Duration::from_secs(300);

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
enum DialOutcome {
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
enum Say {
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
struct RetryPlan {
    /// Re-authenticate the [`IgSession`] before the next dial.
    relogin: bool,
    /// What this turn should say — see [`Say`].
    say: Say,
    /// How long to wait before the next dial.
    sleep: Duration,
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
struct RetryState {
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
    fn plan(&mut self, outcome: DialOutcome, now: Duration) -> RetryPlan {
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

/// Drive the IG Lightstreamer TRADE stream until `stop`, reconnecting on any drop.
///
/// ⚠ **A refused handshake and a dropped socket are NOT the same failure and are not treated the
/// same.** `CONERR` means the credentials this dial presented were rejected — which, for a session
/// whose tokens simply aged out, is fixed by [`IgSession::relogin`] and by nothing else. A
/// transport drop means the network moved; re-logging-in on one would spend the account's login
/// budget on an outage. The refusal path therefore re-authenticates (bounded by
/// [`MAX_REFUSED_HANDSHAKES`]) and the transport path does not.
///
/// ⚠ **Past that bound the driver keeps dialling but stops SHOUTING, and the two are separate
/// decisions.** This doc used to claim it "stops pretending the next attempt is routine"; it stopped
/// re-logging-in and never stopped LOGGING, so a rejected account wrote an `error` line per second
/// forever (see [`DOWN_HEARTBEAT`] for the measurement). Every retry-and-log decision
/// now belongs to [`RetryState::plan`], which is pure and unit-tested; this loop only dials, renders
/// and sleeps.
pub(crate) fn stream_trades(
    params: LsParams,
    deal_refs: DealRefMap,
    events: EventSender,
    stop: Arc<AtomicBool>,
) {
    let agent = ureq::Agent::config_builder()
        .timeout_recv_body(Some(Duration::from_secs(30)))
        .user_agent("vike-trader-rust")
        .build()
        .new_agent();

    let started = Instant::now();
    let mut retry = RetryState::default();
    while !stop.load(Ordering::Relaxed) {
        let (outcome, error) = match run_once(&agent, &params, &deal_refs, &events, &stop) {
            Ok(()) => (DialOutcome::Ok, String::new()),
            Err(RunErr::CoreGone) => return, // ingest closed — nothing to reconnect for
            Err(RunErr::Dropped(e)) => (DialOutcome::Dropped, e),
            Err(RunErr::NeverEstablished(e)) => (DialOutcome::NeverEstablished, e),
            Err(RunErr::Refused(e)) => (DialOutcome::Refused, e),
        };
        let plan = retry.plan(outcome, started.elapsed());
        match plan.say {
            Say::Nothing => {}
            Say::Dropped => {
                tracing::warn!(target: "vike_ig::stream", error = %error, "LS trade stream dropped; reconnecting");
            }
            Say::DialFailed { attempt, next_in } => {
                tracing::warn!(
                    target: "vike_ig::stream",
                    error = %error,
                    attempt,
                    retry_in_s = plan.sleep.as_secs(),
                    next_update_in_s = next_in.as_secs(),
                    "LS trade stream could not open a session at all — no working-order fill or \
                     cancel can arrive while this holds. Dialling continues with exponential \
                     backoff; this line repeats on a DECAYING heartbeat, NOT once per attempt, so \
                     expect the next update in `next_update_in_s` and not sooner"
                );
            }
            Say::DialStillFailing { attempt, down_for, next_in } => {
                tracing::warn!(
                    target: "vike_ig::stream",
                    error = %error,
                    attempt,
                    down_for_s = down_for.as_secs(),
                    retry_in_s = plan.sleep.as_secs(),
                    next_update_in_s = next_in.as_secs(),
                    "LS trade stream STILL cannot open a session — every dial since the line above \
                     has failed before the stream began"
                );
            }
            Say::RefusedRetryingLogin { attempt } => {
                tracing::warn!(
                    target: "vike_ig::stream",
                    error = %error,
                    attempt,
                    "LS trade stream REFUSED our credentials; re-authenticating the IG session \
                     before the next dial"
                );
            }
            Say::FillLaneDown { streak, next_in } => {
                tracing::error!(
                    target: "vike_ig::stream",
                    error = %error,
                    streak,
                    retry_in_s = plan.sleep.as_secs(),
                    next_update_in_s = next_in.as_secs(),
                    "LS trade stream refused the handshake again after a fresh login, so the cause \
                     is NOT an expired token. The IG fill lane is DOWN: working-order fills and \
                     cancels will not arrive. What this driver knows stops there — read the \
                     CONERR code in `error` for the cause, e.g. 71 = the LS_cid this client sends \
                     is not licensed (a CLIENT defect, nothing to do with the account), 1/2 = the \
                     credentials or adapter set were rejected. Dialling continues with exponential \
                     backoff; this line repeats on a DECAYING heartbeat, NOT once per retry, so \
                     expect the next update in `next_update_in_s` and not sooner"
                );
            }
            Say::FillLaneStillDown { streak, down_for, next_in } => {
                tracing::error!(
                    target: "vike_ig::stream",
                    error = %error,
                    streak,
                    down_for_s = down_for.as_secs(),
                    retry_in_s = plan.sleep.as_secs(),
                    next_update_in_s = next_in.as_secs(),
                    "LS trade stream fill lane STILL DOWN — IG has refused every handshake since \
                     the line above. Working-order fills and cancels are not arriving"
                );
            }
        }
        if plan.relogin {
            // Recovery, not a guess: `relogin` no-ops when a sibling already refreshed, and logs
            // its own failure at `error`.
            let _ = params.session.relogin(params.session.token_generation());
        }
        sleep_unless_stopped(&stop, plan.sleep);
    }
}

/// Why a session run ended.
///
/// ⚠ **The transport half is TWO variants, and which one a site constructs is a disk-safety
/// decision.** They were one `Transient` for both, warned about on every turn at a flat delay,
/// which is correct for a mid-stream drop and a warn-a-second flood for an unreachable gateway —
/// see [`DialOutcome`]. Construct [`RunErr::Dropped`] only from INSIDE the streaming read loop;
/// everything up to and including the subscribe POST is [`RunErr::NeverEstablished`].
enum RunErr {
    /// The core ingest closed — stop entirely, there is nothing to reconnect for.
    CoreGone,
    /// A read failed after the stream was already running. Retry with the SAME credentials.
    Dropped(String),
    /// The dial produced no subscribed session. Retry with the SAME credentials, backing off.
    NeverEstablished(String),
    /// The server REFUSED the handshake (`CONERR`, or anything else where a session was not
    /// established). Re-authenticate before retrying — this is what token expiry looks like here.
    Refused(String),
}

/// One session lifetime: create → subscribe (with snapshot) → read until drop/stop.
fn run_once(
    agent: &ureq::Agent,
    params: &LsParams,
    deal_refs: &DealRefMap,
    events: &EventSender,
    stop: &Arc<AtomicBool>,
) -> Result<(), RunErr> {
    // 1. create_session — streaming response.
    let create_url =
        format!("{}/lightstreamer/create_session.txt?LS_protocol={LS_PROTOCOL}", params.endpoint);
    // Read the password PER DIAL (never cached in `params`): after a re-login this is the only way
    // the fresh CST/XST pair reaches the wire.
    let create_body = create_session_body(&params.account_id, &params.session.ls_password());
    let resp = agent
        .post(&create_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(create_body.as_bytes())
        .map_err(|e| RunErr::NeverEstablished(format!("create_session failed: {e}")))?;
    let reader = BufReader::new(resp.into_body().into_reader());
    let mut lines = reader.lines();

    // First control line must be CONOK,<sessionId>,<reqLimit>,<keepalive>,<controlLink>.
    let first = lines
        .next()
        .ok_or_else(|| RunErr::NeverEstablished("create_session: empty response".into()))?
        .map_err(|e| RunErr::NeverEstablished(format!("create_session read: {e}")))?;
    let (session_id, control_link) = parse_conok(&first).ok_or_else(|| {
        // ⚠ The classification, not just the message, is the fix: a `CONERR` here is IG telling us
        // the CST/XST pair we presented is dead, which the driver can cure. Anything else that is
        // not a CONOK is a protocol surprise we cannot cure by logging in again.
        let msg = format!("create_session: expected CONOK, got: {first}");
        if is_conerr(&first) { RunErr::Refused(msg) } else { RunErr::NeverEstablished(msg) }
    })?;
    let control_base = if control_link.is_empty() || control_link == "*" {
        params.endpoint.clone()
    } else {
        rebase_host(&params.endpoint, &control_link)
    };

    // 2. subscribe to TRADE:{account} WITH SNAPSHOT (audit A3 recovery on reconnect).
    let control_url = format!("{control_base}/lightstreamer/control.txt?LS_protocol={LS_PROTOCOL}");
    let sub_body = subscribe_body(&session_id, &params.account_id);
    // ⚠ NEVER-ESTABLISHED even though a `CONOK` arrived: the self-limiting argument that lets a
    // drop warn per turn is that the session STREAMED, and this one never did. A gateway that
    // CONOKs and then refuses `control.txt` turns this loop as fast as an unreachable one does.
    agent
        .post(&control_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(sub_body.as_bytes())
        .map_err(|e| RunErr::NeverEstablished(format!("subscribe failed: {e}")))?;

    // 3. read update lines until stop/drop.
    for line in lines {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let line = line.map_err(|e| RunErr::Dropped(format!("stream read: {e}")))?;
        let line = line.trim();
        if line.is_empty() || line == "PROBE" {
            continue; // keepalive
        }
        if line.starts_with("LOOP") || line.starts_with("END") {
            return Ok(()); // server asked us to rebind / ended — reconnect
        }
        let Some(update) = parse_update_line(line) else {
            continue;
        };
        if update.sub_id != SUB_ID {
            continue;
        }
        // Schema order is `CONFIRMS OPU WOU`; the fill/terminal lane is CONFIRMS (field 0).
        let Some(Some(confirms)) = update.fields.first() else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(confirms) else {
            continue;
        };
        let deal_ref = v.get("dealReference").and_then(|d| d.as_str()).unwrap_or_default();
        let Some(coid) = deal_refs.lock().unwrap().coid_for(deal_ref) else {
            continue; // not one of ours (or submit hasn't recorded the ref yet)
        };
        let ts = v.get("date").and_then(|d| d.as_i64()).unwrap_or(0);
        for ev in decode_trade_confirm(&v, &coid, ts) {
            if events.blocking_send(ev).is_err() {
                return Err(RunErr::CoreGone);
            }
        }
    }
    Ok(())
}

/// The `create_session.txt` form body for the TRADE stream.
///
/// ⚠ **Extracted so the WIRE can be pinned.** Inline in [`run_once`] these bytes were reachable
/// only through a live `ureq` POST, so nothing in the tree ever compared them to the specification
/// or to the market-data lane that speaks the same protocol — which is how a wrong `LS_cid` sat
/// here unnoticed. Mirrors `crate::lightstreamer::create_session_request`, which is the WS
/// transport's twin and cannot be reused directly (it emits the WS request-name framing, not an
/// HTTP form body).
fn create_session_body(account_id: &str, ls_password: &str) -> String {
    form(&[
        ("LS_adapter_set", "DEFAULT"),
        ("LS_user", account_id),
        ("LS_password", ls_password),
        // ⚠ LICENSING, not identity. See the test that pins this byte-for-byte.
        ("LS_cid", LS_CID),
    ])
}

/// The `control.txt` `LS_op=add` form body subscribing `TRADE:{account}` WITH SNAPSHOT
/// (audit A3 recovery on reconnect). Extracted for the same reason as
/// [`create_session_body`].
fn subscribe_body(session_id: &str, account_id: &str) -> String {
    form(&[
        ("LS_session", session_id),
        ("LS_reqId", "1"),
        ("LS_op", "add"),
        ("LS_subId", &SUB_ID.to_string()),
        ("LS_data_adapter", "DEFAULT"),
        ("LS_group", &format!("TRADE:{account_id}")),
        ("LS_schema", "CONFIRMS OPU WOU"),
        ("LS_mode", "DISTINCT"),
        ("LS_snapshot", "true"),
    ])
}

/// Whether a control line is Lightstreamer's `CONERR` — the server REFUSING the session request.
/// Split out (rather than inlined as a `starts_with`) so the credential-expiry classification the
/// reconnect loop keys off is one named, tested predicate.
fn is_conerr(line: &str) -> bool {
    line.trim_start().starts_with("CONERR")
}

/// Parse `CONOK,<sessionId>,<reqLimit>,<keepalive>,<controlLink>` → `(sessionId, controlLink)`.
fn parse_conok(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("CONOK,")?;
    let mut parts = rest.split(',');
    let session_id = parts.next()?.to_string();
    let _req_limit = parts.next()?;
    let _keepalive = parts.next()?;
    let control_link = parts.next().unwrap_or("").to_string();
    Some((session_id, control_link))
}

/// Replace the host of `endpoint` with `host` (a Lightstreamer controlLink), keeping the scheme.
fn rebase_host(endpoint: &str, host: &str) -> String {
    match endpoint.split_once("://") {
        Some((scheme, _)) => format!("{scheme}://{host}"),
        None => format!("https://{host}"),
    }
}

#[path = "stream_tests.rs"]
#[cfg(test)]
mod stream_tests;

#[path = "stream_props.rs"]
#[cfg(test)]
mod stream_props;
