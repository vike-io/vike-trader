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
pub struct Pending {
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
pub struct DealRefs {
    by_ref: HashMap<String, String>,
    by_coid: HashMap<String, Pending>,
}

impl DealRefs {
    /// Record a submitted order's `dealReference` in BOTH directions. Called by the exec thread the
    /// moment the submit POST returns, i.e. BEFORE the confirm is fetched — so a fill that lands in
    /// between still correlates.
    pub fn record(&mut self, coid: &str, deal_ref: &str, market: bool) {
        self.by_ref.insert(deal_ref.to_string(), coid.to_string());
        self.by_coid.insert(coid.to_string(), Pending { deal_ref: deal_ref.to_string(), market });
    }

    /// The order a streamed `CONFIRMS` belongs to, by the `dealReference` it echoes.
    pub fn coid_for(&self, deal_ref: &str) -> Option<String> {
        self.by_ref.get(deal_ref).cloned()
    }

    /// What a re-confirm of `coid` needs. `None` = this process never got a `dealReference` for
    /// that order, so there is nothing IG can be asked.
    pub fn pending_for(&self, coid: &str) -> Option<Pending> {
        self.by_coid.get(coid).cloned()
    }
}

/// Shared submit-time state (populated by exec at submit) — see [`DealRefs`].
pub type DealRefMap = Arc<Mutex<DealRefs>>;

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
pub struct LsParams {
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
pub fn stream_trades(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Replays the reconnect loop's DECISION against a synthetic clock, turn for turn as
    /// [`stream_trades`] drives it: the dial costs `dial_cost`, then [`RetryState::plan`] is
    /// consulted with the resulting reading, then the planned sleep runs.
    ///
    /// ⚠ **This is the whole reason the decision was extracted.** The dial itself goes through
    /// `ureq` to `LsParams::endpoint`, so driving `stream_trades` end-to-end would need an HTTP
    /// server standing in for IG's Lightstreamer gateway — and the thing that needs proving is not
    /// the transport but the LOGGING POLICY over a day of wall time, which no server can be made to
    /// demonstrate quickly. Against a synthetic clock a day costs microseconds.
    struct Replay {
        state: RetryState,
        now: Duration,
        dial_cost: Duration,
        /// `(when the plan was made, the plan)` for every turn, in order.
        turns: Vec<(Duration, RetryPlan)>,
    }

    impl Replay {
        fn new(dial_cost: Duration) -> Self {
            Self { state: RetryState::default(), now: Duration::ZERO, dial_cost, turns: Vec::new() }
        }

        fn dial(&mut self, outcome: DialOutcome) -> RetryPlan {
            self.now += self.dial_cost;
            let plan = self.state.plan(outcome, self.now);
            self.turns.push((self.now, plan));
            self.now += plan.sleep;
            plan
        }

        fn dial_n(&mut self, n: usize, outcome: DialOutcome) {
            for _ in 0..n {
                self.dial(outcome);
            }
        }

        fn says(&self) -> Vec<Say> {
            self.turns.iter().map(|(_, p)| p.say).collect()
        }

        fn sleeps(&self) -> Vec<Duration> {
            self.turns.iter().map(|(_, p)| p.sleep).collect()
        }

        fn relogins(&self) -> Vec<bool> {
            self.turns.iter().map(|(_, p)| p.relogin).collect()
        }

        /// Every turn that would print a line at all, with the clock reading it printed at.
        fn lines(&self, want: fn(&Say) -> bool) -> Vec<(Duration, Say)> {
            self.turns.iter().filter(|(_, p)| want(&p.say)).map(|(t, p)| (*t, p.say)).collect()
        }

        /// Every turn that would print an `error` line — the permanent-refused lane.
        fn error_lines(&self) -> Vec<(Duration, Say)> {
            self.lines(|s| matches!(s, Say::FillLaneDown { .. } | Say::FillLaneStillDown { .. }))
        }

        /// Every turn that would print a cannot-open-a-session `warn`.
        fn dial_lines(&self) -> Vec<(Duration, Say)> {
            self.lines(|s| matches!(s, Say::DialFailed { .. } | Say::DialStillFailing { .. }))
        }
    }

    /// Both persistent lanes must obey the same two rules, so they are asserted by one helper
    /// rather than two hand copies that can drift: never two lines closer than the FIRST interval
    /// (the floor of the decay), and never a silence longer than the CAPPED one, which is the
    /// promise that stops a decaying cadence from fading into apparent recovery.
    fn assert_paced(lines: &[(Duration, Say)], least: usize) {
        assert!(
            lines.len() >= least,
            "only {} lines — not enough to prove a rate at all",
            lines.len()
        );
        for pair in lines.windows(2) {
            let gap = pair[1].0.saturating_sub(pair[0].0);
            assert!(gap >= DOWN_HEARTBEAT, "two lines {gap:?} apart — the rate limit leaked");
            // The loop can only speak on a turn, so the worst case is one whole backoff late.
            assert!(
                gap <= MAX_DOWN_HEARTBEAT + MAX_RECONNECT_BACKOFF,
                "the lane went quiet for {gap:?} — past the capped interval, which is exactly the \
                 silence an operator reads as recovery"
            );
        }
    }

    /// A dial against a gateway that refuses instantly. ~100 ms is what the measured flood implies:
    /// 53,160 lines a day at a flat 1 s sleep leaves ~0.6 s per turn for the round trip and the
    /// loop, and the exact figure changes nothing here — the backoff dominates within ten turns.
    const FAST_REFUSAL: Duration = Duration::from_millis(100);

    /// The transition is a TRANSITION. The defect being fixed is precisely that this `error` line
    /// was emitted on every turn of a one-second loop while its own text says the condition is
    /// permanent, so "exactly one" is the assertion that matters, not "at least one".
    #[test]
    fn a_refusal_streak_logs_the_fill_lane_down_exactly_once_on_the_transition() {
        let mut r = Replay::new(FAST_REFUSAL);
        r.dial_n(43, DialOutcome::Refused);
        let says = r.says();

        assert_eq!(
            says[..3],
            [
                Say::RefusedRetryingLogin { attempt: 1 },
                Say::RefusedRetryingLogin { attempt: 2 },
                Say::RefusedRetryingLogin { attempt: 3 },
            ],
            "the bounded re-login path is unchanged: MAX_REFUSED_HANDSHAKES attempts, each at warn"
        );
        assert_eq!(
            says[usize::try_from(MAX_REFUSED_HANDSHAKES).unwrap()],
            Say::FillLaneDown { streak: MAX_REFUSED_HANDSHAKES + 1, next_in: DOWN_HEARTBEAT },
            "the FIRST turn past the budget is the transition line"
        );

        let onsets = says.iter().filter(|s| matches!(s, Say::FillLaneDown { .. })).count();
        assert_eq!(
            onsets, 1,
            "the transition fired {onsets} times — it is a transition, not a per-turn line"
        );

        let heartbeats = says.iter().filter(|s| matches!(s, Say::FillLaneStillDown { .. })).count();
        assert!(heartbeats > 0, "an operator must still learn the lane is down; got {heartbeats}");
        assert!(
            heartbeats < 43 - 4,
            "{heartbeats} heartbeats over 39 permanent-refused turns is the flood again"
        );
        assert_eq!(
            says.iter().filter(|s| matches!(s, Say::Dropped)).count(),
            0,
            "a refusal is never a transport drop"
        );
    }

    /// The rate limit, asserted as a property of the emitted timeline rather than of a counter:
    /// no two `error` lines closer together than the heartbeat, and no gap so long the operator
    /// would conclude the outage ended.
    #[test]
    fn the_permanent_refused_heartbeat_is_rate_limited_to_one_line_per_interval() {
        let mut r = Replay::new(FAST_REFUSAL);
        r.dial_n(200, DialOutcome::Refused);
        let lines = r.error_lines();

        assert!(
            matches!(lines[0].1, Say::FillLaneDown { .. }),
            "the first error line is the transition, not a heartbeat"
        );
        assert!(
            lines[1..].iter().all(|(_, s)| matches!(s, Say::FillLaneStillDown { .. })),
            "every line after the transition is a heartbeat"
        );
        assert_paced(&lines, 5);
    }

    /// The backoff itself: exponential from the flat delay, capped, monotonic, and TOTAL. A streak
    /// large enough to overflow the shift must answer the CEILING — an arithmetic wrap there would
    /// hand back a one-second retry and with it the whole flood.
    #[test]
    fn the_refused_backoff_doubles_from_the_flat_delay_and_caps_at_the_ceiling() {
        assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 1), Duration::from_secs(1));
        assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 2), Duration::from_secs(2));
        assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 3), Duration::from_secs(4));
        assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 4), Duration::from_secs(8));
        assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 5), Duration::from_secs(16));
        assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 6), Duration::from_secs(32));
        assert_eq!(
            refused_backoff(MAX_REFUSED_HANDSHAKES + 7),
            MAX_RECONNECT_BACKOFF,
            "64s is above the ceiling and must clamp to it"
        );
        assert_eq!(refused_backoff(MAX_REFUSED_HANDSHAKES + 40), MAX_RECONNECT_BACKOFF);
        assert_eq!(refused_backoff(u32::MAX), MAX_RECONNECT_BACKOFF, "the shift must not wrap");

        let mut prev = Duration::ZERO;
        let mut checked = 0u32;
        for streak in (MAX_REFUSED_HANDSHAKES + 1)..(MAX_REFUSED_HANDSHAKES + 200) {
            let d = refused_backoff(streak);
            assert!(d >= prev, "backoff shrank at streak {streak}: {d:?} < {prev:?}");
            assert!(d >= RECONNECT_DELAY, "backoff dipped below the flat delay at streak {streak}");
            assert!(d <= MAX_RECONNECT_BACKOFF, "backoff exceeded the ceiling at streak {streak}");
            prev = d;
            checked += 1;
        }
        assert_eq!(checked, 199, "the monotonicity sweep must actually have run");
    }

    /// The two paths' sleeps, read off the PLAN rather than the helper — the re-login path keeps
    /// the flat one-second delay it has always had, and only the permanent path backs off.
    #[test]
    fn only_the_permanent_refused_path_backs_off_and_only_it_stops_re_logging_in() {
        let mut r = Replay::new(FAST_REFUSAL);
        r.dial_n(10, DialOutcome::Refused);

        let sleeps = r.sleeps();
        assert_eq!(sleeps[..3], [RECONNECT_DELAY; 3], "the bounded re-login path is unchanged");
        assert_eq!(
            sleeps[3..],
            [1u64, 2, 4, 8, 16, 32, 60].map(Duration::from_secs),
            "the permanent path doubles from the flat delay and clamps at the ceiling"
        );

        let relogins = r.relogins();
        assert_eq!(
            relogins[..3],
            [true; 3],
            "the first MAX_REFUSED_HANDSHAKES turns re-authenticate"
        );
        assert_eq!(
            relogins[3..],
            [false; 7],
            "past the budget the driver keeps dialling but stops spending the account's login budget"
        );
    }

    /// A session that RAN clears everything: the streak, the backoff and the heartbeat. Without
    /// this a lane that recovered would keep dialling once a minute and stay silent about the next
    /// outage — the opposite defect to the one being fixed.
    #[test]
    fn a_clean_session_resets_the_streak_the_backoff_and_the_heartbeat() {
        let mut r = Replay::new(FAST_REFUSAL);
        r.dial_n(12, DialOutcome::Refused);
        assert_eq!(r.sleeps()[11], MAX_RECONNECT_BACKOFF, "precondition: deep in the backoff");

        assert_eq!(
            r.dial(DialOutcome::Ok),
            RetryPlan { relogin: false, say: Say::Nothing, sleep: RECONNECT_DELAY },
            "a session that ran says nothing and dials again promptly"
        );
        assert_eq!(
            r.dial(DialOutcome::Refused),
            RetryPlan {
                relogin: true,
                say: Say::RefusedRetryingLogin { attempt: 1 },
                sleep: RECONNECT_DELAY
            },
            "the next refusal is attempt 1 of a NEW streak, not a resumption"
        );

        r.dial_n(2, DialOutcome::Refused);
        assert_eq!(
            r.dial(DialOutcome::Refused).say,
            Say::FillLaneDown { streak: MAX_REFUSED_HANDSHAKES + 1, next_in: DOWN_HEARTBEAT },
            "re-entering the permanent state is a NEW outage and logs its own transition"
        );
    }

    /// A session that STREAMED and then dropped keeps the original behaviour exactly: a warn per
    /// drop, no backoff, no login spent, both streaks reset. Reaching here costs a full handshake
    /// and a subscription, so the loop cannot spin at dial speed — that is the whole argument, and
    /// it is why this path must NOT be rate-limited into silence along with the other two.
    #[test]
    fn a_session_that_ran_and_dropped_is_still_logged_every_time() {
        let mut r = Replay::new(FAST_REFUSAL);
        r.dial_n(12, DialOutcome::Refused);
        r.dial_n(5, DialOutcome::Dropped);

        assert_eq!(r.says()[12..], [Say::Dropped; 5], "every drop is news");
        assert_eq!(r.sleeps()[12..], [RECONNECT_DELAY; 5], "a drop never backs off");
        assert_eq!(r.relogins()[12..], [false; 5], "a drop never spends a login");
        assert_eq!(
            r.dial(DialOutcome::Refused).say,
            Say::RefusedRetryingLogin { attempt: 1 },
            "a drop resets the refusal streak, exactly as it always did"
        );
    }

    /// ⚠ **The second half of the flood, and the one that was left in place by the first fix.**
    /// Most `Transient` values were never mid-stream drops at all — `create_session failed`,
    /// `create_session: empty response`, `create_session read`, a non-`CONOK` first line and
    /// `subscribe failed` all leave the loop having established NOTHING. Nothing is spent, so
    /// nothing self-limits: a hard-down endpoint fails in milliseconds forever, which at the flat
    /// delay is a `warn` a second. This path now gets the same three treatments the refusal path
    /// got — its own streak, exponential backoff, and one line on the transition then a heartbeat.
    #[test]
    fn a_dial_that_never_opens_a_session_backs_off_and_warns_on_a_heartbeat() {
        let mut r = Replay::new(FAST_REFUSAL);
        r.dial_n(200, DialOutcome::NeverEstablished);

        let says = r.says();
        assert_eq!(
            says[0],
            Say::DialFailed { attempt: 1, next_in: DOWN_HEARTBEAT },
            "the FIRST failed dial is news and says so immediately"
        );
        assert_eq!(
            says.iter().filter(|s| matches!(s, Say::DialFailed { .. })).count(),
            1,
            "…and exactly once: this is a transition, not a per-turn line"
        );
        assert_eq!(
            says.iter().filter(|s| matches!(s, Say::Dropped)).count(),
            0,
            "a dial that never opened a session is NOT a drop and must not borrow its unlimited rate"
        );

        let sleeps = r.sleeps();
        assert_eq!(
            sleeps[..7],
            [1u64, 2, 4, 8, 16, 32, 60].map(Duration::from_secs),
            "the first retry keeps the flat delay, then it doubles to the ceiling"
        );
        assert!(
            sleeps[7..].iter().all(|s| *s == MAX_RECONNECT_BACKOFF),
            "and stays at the ceiling — it never stops dialling, a hard-down venue does recover"
        );
        assert_eq!(r.relogins(), vec![false; 200], "a failed dial never spends a login");

        assert_paced(&r.dial_lines(), 5);
    }

    /// The two persistent conditions clear each other, because each is POSITIVE evidence against
    /// the other: a `CONERR` is the gateway answering (so it is reachable), and a failed dial says
    /// nothing at all about the credentials. Without this, whichever condition ran first would
    /// suppress the other's transition line and an operator would never learn the failure changed.
    #[test]
    fn a_refusal_and_a_failed_dial_clear_each_others_streaks() {
        let mut r = Replay::new(FAST_REFUSAL);
        r.dial_n(10, DialOutcome::NeverEstablished);
        assert_eq!(r.sleeps()[9], MAX_RECONNECT_BACKOFF, "precondition: deep in the dial backoff");

        assert_eq!(
            r.dial(DialOutcome::Refused),
            RetryPlan {
                relogin: true,
                say: Say::RefusedRetryingLogin { attempt: 1 },
                sleep: RECONNECT_DELAY
            },
            "the gateway answered, so the unreachable condition is over and the budget is whole"
        );

        r.dial_n(10, DialOutcome::Refused);
        assert_eq!(
            r.dial(DialOutcome::NeverEstablished),
            RetryPlan {
                relogin: false,
                say: Say::DialFailed { attempt: 1, next_in: DOWN_HEARTBEAT },
                sleep: RECONNECT_DELAY
            },
            "and back the other way: a failed dial is a NEW outage with its own transition line"
        );
    }

    /// A session that actually ran clears the dial-failure condition too — the same reset `Ok` has
    /// always performed for the refusal streak, extended to the condition added beside it.
    #[test]
    fn a_session_that_ran_clears_the_dial_failure_streak() {
        for recovery in [DialOutcome::Ok, DialOutcome::Dropped] {
            let mut r = Replay::new(FAST_REFUSAL);
            r.dial_n(10, DialOutcome::NeverEstablished);
            let plan = r.dial(recovery);
            assert_eq!(plan.sleep, RECONNECT_DELAY, "{recovery:?} must drop the backoff");

            assert_eq!(
                r.dial(DialOutcome::NeverEstablished),
                RetryPlan {
                    relogin: false,
                    say: Say::DialFailed { attempt: 1, next_in: DOWN_HEARTBEAT },
                    sleep: RECONNECT_DELAY
                },
                "after {recovery:?} the next failed dial is attempt 1 of a NEW outage"
            );
        }
    }

    /// The ladder itself: geometric from the first interval, capped, monotonic and TOTAL. The cap
    /// is the load-bearing half — an interval that kept growing would eventually be silence, and
    /// silence is what the heartbeat exists to NOT produce.
    #[test]
    fn the_heartbeat_interval_decays_geometrically_and_caps() {
        assert_eq!(heartbeat_interval(0), DOWN_HEARTBEAT, "the first quiet period is responsive");
        assert_eq!(heartbeat_interval(1), Duration::from_secs(900), "5m -> 15m");
        assert_eq!(heartbeat_interval(2), Duration::from_secs(2700), "15m -> 45m");
        assert_eq!(
            heartbeat_interval(3),
            MAX_DOWN_HEARTBEAT,
            "135m is past the ceiling and must clamp to it"
        );

        let mut prev = Duration::ZERO;
        let mut checked = 0u32;
        for steps in 0..200 {
            let d = heartbeat_interval(steps);
            assert!(d >= prev, "the interval shrank at step {steps}: {d:?} < {prev:?}");
            assert!(d >= DOWN_HEARTBEAT, "the interval dipped below the floor at step {steps}");
            assert!(d <= MAX_DOWN_HEARTBEAT, "the interval passed the ceiling at step {steps}");
            prev = d;
            checked += 1;
        }
        assert_eq!(checked, 200, "the monotonicity sweep must actually have run");

        // ⚠ TOTAL: an outage long enough to overflow the power must answer the CEILING. A wrap here
        // would hand back a five-minute cadence forever, which is the volume this decay removes.
        assert_eq!(heartbeat_interval(u32::MAX), MAX_DOWN_HEARTBEAT, "the power must not wrap");
        assert_eq!(heartbeat_interval(1_000_000), MAX_DOWN_HEARTBEAT);
    }

    /// The rate limiter itself, asserted once rather than twice — both persistent lanes share it,
    /// which is the point of it being a type. Covers the whole contract: the interval is measured
    /// from the LAST LINE (measuring from onset would fire every turn once the outage outlived one
    /// interval — the flood wearing a rate limit), it DECAYS, and a clear resets the decay.
    #[test]
    fn the_shared_rate_limiter_decays_from_the_last_line_and_resets_on_recovery() {
        let t0 = Duration::from_secs(7);
        let mut p = Persistent::default();

        assert_eq!(p.tick(t0), Speak::Onset { next_in: DOWN_HEARTBEAT });
        assert_eq!(p.tick(t0), Speak::Quiet);
        assert_eq!(
            p.tick(t0 + DOWN_HEARTBEAT - Duration::from_nanos(1)),
            Speak::Quiet,
            "one nanosecond short of the interval is still silent"
        );
        assert_eq!(
            p.tick(t0 + DOWN_HEARTBEAT),
            Speak::Heartbeat { down_for: DOWN_HEARTBEAT, next_in: heartbeat_interval(1) },
            "down_for is measured from ONSET; next_in is the DECAYED interval that follows"
        );

        // ⚠ The next line is due one DECAYED interval after the last one — not one base interval,
        // and not at a multiple of the outage age.
        let second = t0 + DOWN_HEARTBEAT;
        assert_eq!(
            p.tick(second + DOWN_HEARTBEAT),
            Speak::Quiet,
            "the flat interval has passed but the decayed one has not"
        );
        assert_eq!(
            p.tick(second + heartbeat_interval(1)),
            Speak::Heartbeat {
                down_for: DOWN_HEARTBEAT + heartbeat_interval(1),
                next_in: heartbeat_interval(2),
            }
        );

        p.clear();
        assert_eq!(
            p.tick(Duration::from_secs(9)),
            Speak::Onset { next_in: DOWN_HEARTBEAT },
            "a recovery resets the DECAY too — a second outage starts responsive rather than \
             inheriting an hour-long interval from the first"
        );
    }

    /// The cap holds forever: once the interval reaches the ceiling it stays there, so a lane down
    /// for days keeps saying so hourly. Driven through the real `plan`, not the limiter alone.
    #[test]
    fn a_capped_heartbeat_keeps_speaking_hourly_forever() {
        let mut r = Replay::new(FAST_REFUSAL);
        // A week of a permanently refused lane.
        let week = Duration::from_secs(7 * 24 * 60 * 60);
        while r.now < week {
            r.dial(DialOutcome::Refused);
        }
        let lines = r.error_lines();

        // Every gap after the ladder tops out is the capped interval, to within one dial turn.
        let tail = &lines[6..];
        assert!(tail.len() > 100, "not enough capped gaps to prove the cap: {}", tail.len());
        for pair in tail.windows(2) {
            let gap = pair[1].0.saturating_sub(pair[0].0);
            assert!(
                gap >= MAX_DOWN_HEARTBEAT && gap <= MAX_DOWN_HEARTBEAT + MAX_RECONNECT_BACKOFF,
                "a capped gap of {gap:?} — the cadence must settle at the ceiling, not drift"
            );
        }
    }

    /// EITHER streak running long enough to saturate must stay where it is. A wrap to zero on the
    /// refusal side would resume re-logging-in and point a login storm at the one endpoint IG
    /// meters per account; on the dial side it would drop the backoff to one second and restore the
    /// flood. Both are `saturating_add`, and both are asserted, because a `+= 1` added to only one
    /// of them later would otherwise pass.
    #[test]
    fn a_saturated_streak_neither_wraps_nor_leaves_its_state() {
        let pinned =
            Persistent { since: Some(Duration::ZERO), last_said: Some(Duration::ZERO), steps: 0 };

        let mut state =
            RetryState { refused_streak: u32::MAX, refused_down: pinned, ..RetryState::default() };
        let plan = state.plan(DialOutcome::Refused, Duration::from_secs(10));
        assert_eq!(state.refused_streak, u32::MAX, "the streak saturates rather than wrapping");
        assert!(!plan.relogin, "a wrapped streak would resume re-logging-in forever");
        assert_eq!(plan.sleep, MAX_RECONNECT_BACKOFF);
        assert_eq!(plan.say, Say::Nothing, "and the heartbeat still holds");

        let mut state =
            RetryState { dial_streak: u32::MAX, dial_down: pinned, ..RetryState::default() };
        let plan = state.plan(DialOutcome::NeverEstablished, Duration::from_secs(10));
        assert_eq!(state.dial_streak, u32::MAX, "the dial streak saturates too");
        assert!(!plan.relogin);
        assert_eq!(plan.sleep, MAX_RECONNECT_BACKOFF, "a wrap here would restore the flood");
        assert_eq!(plan.say, Say::Nothing);
    }

    /// **The other half of the flood, replayed.** A hard-down / DNS-broken / TLS-broken gateway
    /// fails before a session exists, in milliseconds, forever. At the flat delay that is a `warn`
    /// on every turn — ~86,400 lines a day, and the CI box runs `VIKE_LOG_FILE_LEVEL=warn`, so every one
    /// of them is written to disk. It was never measured on a live box because the day that WAS
    /// measured failed as a refusal; the rate is arithmetic, not an estimate.
    #[test]
    fn a_hard_down_endpoint_no_longer_writes_a_warn_a_second_all_day() {
        const DAY: Duration = Duration::from_secs(24 * 60 * 60);

        let mut r = Replay::new(FAST_REFUSAL);
        while r.now < DAY {
            r.dial(DialOutcome::NeverEstablished);
        }

        let dials = r.turns.len();
        let warns = r.dial_lines().len();

        // What the OLD loop did on the same day: one turn per `RECONNECT_DELAY` + dial, warning on
        // every one of them.
        let old = DAY.as_millis() / (RECONNECT_DELAY + FAST_REFUSAL).as_millis();
        assert!(old > 78_000, "sanity: the old rate really is a warn a second ({old})");

        // MEASURED by replaying this exact state machine. The bounds are a band rather than
        // equalities, so re-tuning a constant is a deliberate re-argument and not a rebaseline.
        // MEASURED: 78,545 -> 26 lines, over 1,443 dials. (It was 288 under the flat heartbeat that
        // first bounded this; the decay is what took it the rest of the way.)
        assert!(warns >= 20, "an operator must still see the lane is down all day: {warns} lines");
        assert!(warns < 40, "{warns} lines a day is more than a known-permanent state needs");
        assert!(dials < 1_600, "{dials} dials a day — the ceiling must bound the retry rate too");
        assert!(
            u128::try_from(warns).unwrap() * 2_000 < old,
            "only a {}x reduction, from {old} to {warns}",
            old / u128::try_from(warns.max(1)).unwrap()
        );
    }

    /// **The defect, replayed.** Measured on the CI box 2026-08-23: this one loop wrote **53,160 `error`
    /// lines / 23 MB in one day**, dead flat at ~3,290/hour for 17 hours — 99.5% of the box's entire
    /// error volume — because a permanent refusal was logged on every turn of a one-second retry.
    /// Same day, same permanent refusal, through the new decision.
    #[test]
    fn a_permanently_refused_day_no_longer_floods_the_log() {
        /// What the box actually wrote in 24h, and roughly how wide each line was (23 MB / 53,160).
        const MEASURED_LINES_PER_DAY: usize = 53_160;
        const MEASURED_BYTES_PER_LINE: usize = 433;
        const DAY: Duration = Duration::from_secs(24 * 60 * 60);

        let mut r = Replay::new(FAST_REFUSAL);
        while r.now < DAY {
            r.dial(DialOutcome::Refused);
        }

        let dials = r.turns.len();
        let errors = r.error_lines().len();

        // MEASURED by replaying this exact state machine, against the 24 hours that produced
        // 53,160. The bounds are a band rather than equalities, so re-tuning a constant is a
        // deliberate re-argument and not a rebaseline.
        // MEASURED: 53,160 -> 26 lines (23 MB -> ~11 KB), over 1,446 dials. It was 288 under the
        // flat heartbeat that first bounded this; the decay took it the rest of the way.
        assert!(
            errors >= 20,
            "an operator must still see the lane is down all day: {errors} lines"
        );
        assert!(errors < 40, "{errors} lines a day is more than a known-permanent state needs");
        assert!(
            dials < 1_600,
            "{dials} dials a day — the backoff ceiling is supposed to bound the retry rate too"
        );
        assert!(
            errors * 1_500 < MEASURED_LINES_PER_DAY,
            "only a {}x reduction, from {MEASURED_LINES_PER_DAY} to {errors}",
            MEASURED_LINES_PER_DAY / errors.max(1)
        );
        assert!(
            errors * MEASURED_BYTES_PER_LINE < 20_000,
            "{} KB/day from one refused venue is more than this needs",
            errors * MEASURED_BYTES_PER_LINE / 1024
        );
    }

    #[test]
    fn conok_parses_session_and_control_link() {
        let (sid, link) = parse_conok("CONOK,S1a2b3c4d5e6f,50000,5000,myhost.ig.com").unwrap();
        assert_eq!(sid, "S1a2b3c4d5e6f");
        assert_eq!(link, "myhost.ig.com");
        let (sid2, link2) = parse_conok("CONOK,S9,50000,5000,*").unwrap();
        assert_eq!(sid2, "S9");
        assert_eq!(link2, "*");
        assert!(parse_conok("CONERR,2,Wrong credentials").is_none());
    }

    /// The classification the reconnect loop's recovery hangs off: a `CONERR` first line is the
    /// server REFUSING our credentials (re-authenticate), everything else that is not a `CONOK` is
    /// a protocol surprise (retry with the same pair). Getting this backwards is invisible at
    /// runtime — it just means an expired session never recovers, which is the defect this whole
    /// path exists to close.
    #[test]
    fn conerr_is_classified_as_a_refusal_and_nothing_else_is() {
        assert!(is_conerr("CONERR,2,Wrong credentials"));
        assert!(is_conerr("CONERR,1,Requested Adapter Set not available"));
        // Real TLCP frames can carry leading whitespace after a chunked read.
        assert!(is_conerr("  CONERR,2,x"));
        assert!(!is_conerr("CONOK,S1,50000,5000,*"));
        assert!(!is_conerr("PROBE"));
        assert!(!is_conerr("LOOP,0"));
        assert!(!is_conerr(""));
        // ⚠ Not a substring test: a CONFIRMS payload mentioning the word must not read as a refusal.
        assert!(!is_conerr(r#"U,1,1|{"reason":"CONERR"}"#));
    }

    /// ⚠ **The `LS_cid` this lane sends is a LICENSING field, not a name for us**, and it is why
    /// this stream had almost certainly never worked on any account. TLCP 2.1.0's `create_session`
    /// mandates one special string "for all custom developed clients"; anything else is answered
    /// `CONERR,71 License not valid for this Client type`, which is a refusal of the CLIENT and has
    /// nothing to do with the account, the credentials or the subscription. `stream.rs` sent
    /// `"vike-trader-rust"` — a made-up identifier — while `lightstreamer.rs`, the market-data lane
    /// that works, sent the mandated one from its own `LS_CID`.
    ///
    /// This test is the twin of `lightstreamer.rs`'s `request_builders_pin_the_wire_bytes`, and it
    /// asserts the ENCODED body: the mandated string contains a space, so the wire form carries
    /// `%20` (`form_percent_encodes_group_and_schema` below pins that encoding independently).
    #[test]
    fn the_trade_stream_sends_the_licensed_client_id_the_spec_mandates() {
        let body = create_session_body("ABC123", "CST-tok|XST-tok");

        assert!(
            body.contains("LS_cid=mgQkwtwdysogQz2BJ4Ji%20kOj2Bg"),
            "the create_session body must carry the SPEC's client id; got: {body}"
        );
        assert!(
            !body.contains("vike-trader-rust"),
            "a made-up client id is refused with CONERR,71 whatever the account says; got: {body}"
        );
        // The value is the shared constant, not a second copy that can drift from it again.
        assert!(body.contains(&format!("LS_cid={}", crate::lightstreamer::urlencode(LS_CID))));
    }

    /// The whole body, verbatim, both requests — the market-data lane pins its two the same way.
    /// A verbatim pin is what makes an accidental edit to a licensing or protocol field a test
    /// failure rather than a silent `CONERR` nobody can attribute.
    #[test]
    fn trade_stream_request_bodies_pin_the_wire_bytes() {
        assert_eq!(
            create_session_body("ABC123", "CST-abc|XST-def"),
            "LS_adapter_set=DEFAULT&LS_user=ABC123&LS_password=CST-abc%7CXST-def\
             &LS_cid=mgQkwtwdysogQz2BJ4Ji%20kOj2Bg"
        );
        assert_eq!(
            subscribe_body("S1a2b3", "ABC123"),
            "LS_session=S1a2b3&LS_reqId=1&LS_op=add&LS_subId=1&LS_data_adapter=DEFAULT\
             &LS_group=TRADE%3AABC123&LS_schema=CONFIRMS%20OPU%20WOU&LS_mode=DISTINCT\
             &LS_snapshot=true"
        );
    }

    /// One handshake, one spelling. Both lanes speak TLCP 2.1.0 to the same vendor's server, and
    /// `stream.rs` re-spelling the protocol version (and `form`/`urlencode`) as its own local copies
    /// is the duplication that let the `LS_cid` above diverge in the first place — the wrong cid was
    /// the COST, the duplication was the defect.
    #[test]
    fn the_two_tlcp_lanes_agree_on_the_protocol_version() {
        assert_eq!(LS_PROTOCOL, crate::lightstreamer::LS_PROTOCOL);
        assert_eq!(LS_CID, crate::lightstreamer::LS_CID);
        assert_eq!(
            crate::lightstreamer::LS_WS_SUBPROTOCOL,
            format!("{LS_PROTOCOL}.lightstreamer.com"),
            "the WS subprotocol is the same version wearing the vendor's suffix"
        );
    }

    #[test]
    fn form_percent_encodes_group_and_schema() {
        let body = form(&[("LS_group", "TRADE:ABC123"), ("LS_schema", "CONFIRMS OPU WOU")]);
        assert_eq!(body, "LS_group=TRADE%3AABC123&LS_schema=CONFIRMS%20OPU%20WOU");
    }

    /// Both directions, from ONE write. The `coid -> dealReference` half is what makes
    /// `ExecutionClient::confirm` answerable on this venue at all: `GET /confirms/{ref}` is IG's
    /// only per-order status read, and before this map existed nothing in the process could turn a
    /// coid back into the reference it needs.
    #[test]
    fn deal_refs_answers_in_both_directions_from_one_write() {
        let mut refs = DealRefs::default();
        refs.record("coid-market", "REF-A", true);
        refs.record("coid-working", "REF-B", false);

        assert_eq!(refs.coid_for("REF-A").as_deref(), Some("coid-market"));
        assert_eq!(refs.coid_for("REF-B").as_deref(), Some("coid-working"));
        assert_eq!(refs.coid_for("REF-UNKNOWN"), None, "a foreign confirm is not ours to route");

        // `market` travels with the reference: the re-confirm needs it to decide whether the
        // confirm it fetches carries a fill or only an accept.
        assert_eq!(
            refs.pending_for("coid-market"),
            Some(Pending { deal_ref: "REF-A".into(), market: true })
        );
        assert_eq!(
            refs.pending_for("coid-working"),
            Some(Pending { deal_ref: "REF-B".into(), market: false })
        );
        assert_eq!(
            refs.pending_for("coid-never-submitted"),
            None,
            "no reference means IG never acknowledged the deal — there is nothing to ask about"
        );
    }

    #[test]
    fn rebase_host_keeps_scheme() {
        assert_eq!(
            rebase_host("https://demo-apd.marketdatasystems.com", "push.ig.com"),
            "https://push.ig.com"
        );
    }
}
