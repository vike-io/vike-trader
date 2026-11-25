//! The venue-neutral user-data WS pump: a venue-supplied `open_websocket()` (connects + auths +
//! subscribes inside) and a pure `decode(frame)`; a recv loop on a short poll timeout,
//! exponential-backoff reconnect on transport hiccups, a clean stop path, and auth/protocol
//! failures RETURNED immediately (never reconnect-looped). The stream is a trait seam
//! ([`WebsocketSessionUserStream`]), so the loop is offline-testable with scripted frames, and a private
//! stream that is an HTTP body (server-sent events, newline-delimited JSON) runs on it as a [`line_stream`].
//! Its time is `crate::pump_clock`'s (the wall clock, or a test's virtual one), so it runs a venue's real
//! durations offline too.
//!
//! **Silent-stall watchdog (opt-in).** [`run_user`] is the exec-lane twin of
//! the market pump's `idle_threshold`: a half-dead socket (open, no frames, no error, no close)
//! otherwise parks in the read-timeout arm forever. It matters MORE here than on the market lane,
//! because this lane's recovery (`on_reconnect` → [`run_user_resync`]) is reconnect-gated: no
//! reconnect means no gen bump means no resync, so a terminal landing during the silence is lost
//! (private WS streams don't replay). OFF when `idle_threshold` is `None`; a venue's threshold must
//! key off its SERVER-PING contract, never data cadence — an idle account emits no events for
//! hours, so only inbound control frames prove liveness.
//!
//! ## History-replay floor (the restart law, second half)
//! [`run_user_resync`] is the SECOND consumer of the venue `resync` closures; the first,
//! `exec_actor::run_loop`'s gap-sentinel, is floored too (that module's doc is the full argument).
//! Each closure fetches history bounded by a ROW COUNT only (each venue's `RESYNC_HISTORY_LIMIT`,
//! no start time), and the only thing stopping a replayed row from folding twice through the
//! non-idempotent `Account::apply_fill` is the core's in-memory `seen_trade_ids`, EMPTY in a fresh
//! process. The `opened_once` guard in [`run_user`] means a fresh process
//! does not resync at start, but every LATER reconnect does, replaying the pre-assembly window.
//!
//! **The floor is here, in this one loop, not in the venue closures**: binance perp and BOTH aster
//! lanes reach it through `vike_binance::family::execution::user_data`'s `spawn_user_data_with_resync`, which a
//! grep for a venue's own [`spawn_user_with_resync`] call does not see; a per-venue floor would miss them.
//!
//! **Why a blanket floor is safe.** Every consumer's closure returns FILL-like and TERMINAL events
//! only: each venue's mapper skips still-open orders by construction (binance/aster `NEW`, bybit's
//! `/v5/order/history` is closed orders only, okx's non-`canceled` states, deribit's
//! `open`/`untriggered`/`triggered`, polymarket's `user_websocket::decode_order` ignores `PLACEMENT` and its
//! REST `/data/orders` rows carry no `type`). **No consumer synthesizes open-order state, so no floor
//! can drop a live resting order.** Polymarket stamps `ts: 0` on every event its history path emits
//! (`decode_trade` reads only the WS `timestamp`, absent from the `/data/trades` REST rows), and
//! `exec_actor::is_pre_spawn` lets an unstamped event ride through, so the floor is a no-op there.
//!
//! `spawn_ms = 0` disables the floor: a consumer that genuinely replays PRE-MOUNT state opts out AT
//! ITS CALL SITE, visibly, instead of the floor being weakened for everyone.
//!
//! ## One spawn for every private WS stream
//! [`spawn_user_with_resync`] is the whole pump of a venue's private socket from one
//! [`WebsocketSessionUserResyncSpec`]: the venue hands over its open (dial with [`dial_user_stream`],
//! then its login steps through `crate::websocket::send_and_await`), its decoder, its notice hook (the session's
//! announced end, [`run_user_with_notice`]), its keepalive, its idle threshold, its history lag (a second replay,
//! [`run_user_resync`]'s doc) and its fence, and the builder does the rest ONCE: the thread names, the `user_data_pump`
//! span carrying the venue, both producers through the fence, the mechanism's poll and settle, the
//! venue's reconnect ladder (its spec's `backoff`: the exec row's, with the venue's modes) under the rules of
//! `vike_venues::reconnect` (owner ruling B-13), its failed-open line paced with [`reconnect::RECONNECT_LOG`]. A replay reads at most [`RESYNC_HISTORY_LIMIT`] rows. A stream with no
//! REST replay (hyperliquid's re-sends its own snapshot on every subscribe) is [`spawn_user`] from a
//! [`WebsocketSessionUserSpec`]: the same pump, its own `on_reconnect` hook, no resync supervisor.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::Duration;
// The pump reads its time from `crate::pump_clock`; the child test modules read the wall clock through `super::*`.
#[cfg(test)]
use std::time::Instant;

use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use vike_execution::EventSender;
use vike_model::events::Event;
use vike_venues::reconnect::ReconnectBackoff;

use crate::exec_actor::submit_fence::SubmitFence;
use crate::pump_clock;
use reconnect::{RECONNECT_LOG, ReconnectLog, ReconnectLogLine};
use vike_transport::websocket::{WsSocket, configure_websocket_stream};

pub mod line_stream;
pub mod reconnect;
pub mod stream_link;

// ---------------------------------------------------------------------------------------------
// MECHANISM defaults of this pump (decision 0160). None of these is a fact about a venue, so none
// is a row of `vike_venues::venue_timings`, which holds only what has a venue's reason
// behind it: they are the numbers every bridge's pump spawn used to copy, written once. The reconnect
// ladder is not one of them: the table declares each user-data venue's (`vike_venues::venue_timings`'s
// `user_data_backoff`, 1 s doubling to 30 s), and the pump runs that declaration (its spec's `backoff`).
// ---------------------------------------------------------------------------------------------

/// The stop-flag poll cadence of a user-data socket read, of the backoff nap and of the resync
/// supervisor's wake: how long a raised stop can wait on a quiet socket. The `poll` argument of
/// [`run_user`] and [`run_user_resync`], as [`spawn_user`] and [`spawn_user_with_resync`] run them.
pub const USER_DATA_POLL: Duration = Duration::from_secs(1);

/// The settle a resync supervisor waits after a reconnect before it replays history: long enough
/// for the reopened socket to drain its own backlog first. The `settle` of [`run_user_resync`], as
/// [`spawn_user_with_resync`] runs it.
pub const RESYNC_SETTLE: Duration = Duration::from_secs(1);

/// The most history rows one reconnect replay reads (each venue's closure asks for this many, no start
/// time; the module doc's floor drops what predates the process). One number for every venue, owner
/// ruling B-13: the five bridges that held a copy read 50, and none stated a venue's reason for it.
pub const RESYNC_HISTORY_LIMIT: u32 = 50;

/// Auth/protocol error — surfaced, never reconnect-looped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsocketSessionUserAuthenticationError(pub String);

impl std::fmt::Display for WebsocketSessionUserAuthenticationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for WebsocketSessionUserAuthenticationError {}

#[derive(Debug, PartialEq, Eq)]
pub enum WebsocketSessionUserMessage {
    Text(String),
    Ping(Vec<u8>),
    /// binary/pong/other control frames — ignored
    Other,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WebsocketSessionUserError {
    /// read-timeout poll tick — NOT a failure; loop back to check stop()
    Timeout,
    /// transport failure/close — reconnect with backoff
    Closed(String),
}

/// One authenticated venue stream (post-handshake).
pub trait WebsocketSessionUserStream {
    fn recv(&mut self) -> Result<WebsocketSessionUserMessage, WebsocketSessionUserError>;
    fn pong(&mut self, payload: Vec<u8>) -> Result<(), WebsocketSessionUserError>;
}

/// `open_websocket()` outcome: an authed stream, a transport failure (reconnectable), an open the venue
/// refused that a retry may still cure (reconnectable, on the backoff's refusal modes), an auth failure
/// (fatal), or a stop observed mid-handshake (clean exit).
pub enum WebsocketSessionUserOpenOutcome<S> {
    Ready(S),
    Transport(String),
    /// The venue refused the open in words a retry may cure (OANDA's 401 to a token it accepts a minute later):
    /// a failed open that waits [`ReconnectBackoff::refused`]'s wait, never the pump's end.
    Refused(String),
    Auth(WebsocketSessionUserAuthenticationError),
    Stopped,
}

/// Persistent pump: open_websocket → recv loop → decode → emit, with reconnect + stop, plus the opt-in
/// silent-stall watchdog (the module doc's rationale).
///
/// The reconnect wait is `backoff`, the venue's ladder as its spec hands it
/// ([`ReconnectBackoff::from_row`] over the exec row's `reconnect`, with the modes the venue states). Without
/// [`ReconnectBackoff::with_reset_after`] its one reset rule holds: a successful re-open does not reset it,
/// **a session that then LIVED twice the ceiling (60 s on every pump-driven row) does**, so a longer session
/// reopens at most once a minute, against the 30 s a failing open retries at. A
/// [`WebsocketSessionUserOpenOutcome::Refused`] open waits [`ReconnectBackoff::refused`]'s wait (the refusal
/// modes), and an open that works ends a run of refusals ([`ReconnectBackoff::opened`]). `emit` returning false
/// means the core is gone — treated as stop. Auth errors return `Err` immediately. Every failed open is a `warn` line when `failed_open_log` is `None`; the pump of
/// [`spawn_user`] and [`spawn_user_with_resync`] paces it with [`RECONNECT_LOG`] (owner ruling B-13).
///
/// **A refusal the venue's refusal mode still spares is no failure** (owner, "do", 2026-11-22: one rule for every
/// venue): while [`ReconnectBackoff::refusal_spared`] holds (inside a refusal budget, short of a refusal wait's bound) a
/// `Refused` open counts nothing on the link, which reads `Connecting`; once the run reaches the bound it is a failed
/// open like a `Transport` one. A spared refusal never undoes an earlier failure: an open that failed, or a stall, since
/// the last open that worked keeps the reading `Error`.
///
/// **Its lines take the order path's level rule** (`crate::feed_ctx::set_status`'s: `Error` at `warn`, every other
/// state at `info`), the state being what the account's lane then reads (the pump's sub-path of
/// `crate::venue_link::venue_link`'s cell, written through [`stream_link`], its `connection_state`): a session the venue or the socket ended and a spared
/// refusal are a reopen, `Connecting`, said at `info`; a silent stall and a failed open are `Error`, said at `warn`; a
/// refused key gives up at `error`.
///
/// **A silence the venue's ladder spares is no stall** ([`ReconnectBackoff::silence_spared`], a lane whose venue
/// documents no heartbeat cadence): the watchdog still ends the session at `idle_threshold` and the pump reopens on the
/// same wait, but the session's end is a reopen, `Connecting`, said at `info`.
///
/// `idle_threshold` = the longest silence tolerated before the session is declared dead. Silence
/// means NO inbound frame of any kind — Text, Ping, or Other — so a venue whose server pings keeps
/// the session alive with control frames alone, exactly as the market pump's twin does. `None`
/// disables the check entirely (every test driver and capture tap passes `None`).
///
/// On a trip the session ends like any transport hiccup (`break false` → backoff → re-open), which
/// fires `on_reconnect` and lets [`run_user_resync`] replay whatever landed in the hole. The
/// watchdog therefore adds no new recovery policy — it only makes the EXISTING one reachable.
///
/// ⚠ Pick the threshold from the venue's server-heartbeat contract, NOT from data cadence: on this
/// lane an idle account is legitimately silent for hours, so a data-derived threshold would
/// reconnect a perfectly healthy socket in a loop.
///
/// A venue that announces the end of a session runs this loop as [`run_user_with_notice`].
pub fn run_user<S: WebsocketSessionUserStream>(
    open_websocket: impl FnMut() -> WebsocketSessionUserOpenOutcome<S>,
    decode: impl Fn(&Value) -> Vec<Event>,
    emit: impl FnMut(Event) -> bool,
    stop: &AtomicBool,
    poll: Duration,
    backoff: ReconnectBackoff,
    keepalive: Option<WebsocketSessionUserKeepalive<S>>,
    on_reconnect: impl FnMut(),
    idle_threshold: Option<Duration>,
    failed_open_log: Option<ReconnectLog>,
) -> Result<(), WebsocketSessionUserAuthenticationError> {
    run_user_with_notice(
        open_websocket,
        decode,
        None,
        emit,
        stop,
        poll,
        backoff,
        keepalive,
        on_reconnect,
        idle_threshold,
        failed_open_log,
    )
}

/// The venue's notice hook ([`WebsocketSessionUserResyncSpec`]'s `notice`): a frame by which the venue says it is
/// about to end the session (`vike_venues::venue_connection`'s `Handled` notice) to the words the pump reports it in;
/// `None` for every other frame.
pub type WebsocketSessionUserNotice = fn(&Value) -> Option<String>;

/// [`run_user`]'s loop with the venue's `notice` hook: **the ONE planned reconnect.** The hook reads every JSON
/// frame before `decode`; a frame it answers ends the session in its words, the order path reads `Connecting`
/// (a reopen, never `Error`), it is said at `info`, and the pump opens again at once
/// ([`ReconnectBackoff::announced`]: after a session that lived past the ladder's reset; a younger one waits the
/// ladder), so `on_reconnect` fires and [`run_user_resync`] replays the hole. No venue is named here: each bridge
/// hands its own hook (binance spot's `serverShutdown`, okx's `64008`, deribit's `platform_state` maintenance).
#[expect(clippy::too_many_arguments)]
pub fn run_user_with_notice<S: WebsocketSessionUserStream>(
    mut open_websocket: impl FnMut() -> WebsocketSessionUserOpenOutcome<S>,
    decode: impl Fn(&Value) -> Vec<Event>,
    notice: Option<WebsocketSessionUserNotice>,
    mut emit: impl FnMut(Event) -> bool,
    stop: &AtomicBool,
    poll: Duration,
    // the reconnect ladder and its modes: the spec's `backoff`
    mut backoff: ReconnectBackoff,
    // app-level keepalive: fires every `interval` while the session is up (Binance perp's
    // listenKey PUT; None for venues whose server pings)
    keepalive: Option<WebsocketSessionUserKeepalive<S>>,
    // fires on a RE-open (never the first), so a supervisor can trigger a post-reconnect
    // resync — a terminal that landed during the gap is otherwise lost (private WS streams
    // don't replay). Must be CHEAP (bump a gen / try_send a marker), never block the recv
    // loop. Pass `|| {}` when no resync is wired.
    mut on_reconnect: impl FnMut(),
    // `None` = the silent-stall watchdog is OFF.
    idle_threshold: Option<Duration>,
    // the failed-open line's pace; `None` = a line per failed open (every test driver and capture tap)
    mut failed_open_log: Option<ReconnectLog>,
) -> Result<(), WebsocketSessionUserAuthenticationError> {
    let mut keepalive = keepalive;
    let mut opened_once = false;
    // What the gatewayhub's status verb reads (the `stream_link` module). Written on a transition, never per frame.
    let mut lane = stream_link::VenueLinkStreamLane::of_this_thread();
    let mut failed_opens = 0u32;
    // Whether an open since the last one that worked failed for the link: a refusal the ladder spares did not.
    let mut failing = false;
    // The log pace's clock: any monotonic reading will do.
    let started = pump_clock::now();
    while !stop.load(Ordering::Relaxed) {
        let outcome = match open_websocket() {
            WebsocketSessionUserOpenOutcome::Ready(ws) => Ok(ws),
            WebsocketSessionUserOpenOutcome::Auth(e) => {
                tracing::error!(error = %e, "user-data WS auth failed — pump exiting");
                lane.gave_up(&e.0, failed_opens + 1);
                return Err(e); // raise, do NOT reconnect-loop
            }
            WebsocketSessionUserOpenOutcome::Stopped => break,
            // The two failed opens differ in the ladder's step (a refusal takes its refusal modes) and in whether the
            // link counts them: a refusal the modes still spare is a reopen.
            WebsocketSessionUserOpenOutcome::Transport(msg) => {
                Err(("transport error", msg, backoff.failed(), false))
            }
            WebsocketSessionUserOpenOutcome::Refused(msg) => {
                let wait = backoff.refused();
                Err(("open refused", msg, wait, backoff.refusal_spared()))
            }
        };
        let mut ws = match outcome {
            Ok(ws) => {
                backoff.opened();
                lane.open();
                failed_opens = 0;
                if let Some(log) = failed_open_log.as_mut() {
                    log.clear();
                }
                if opened_once {
                    on_reconnect(); // re-open — signal the resync supervisor
                    tracing::info!("user-data WS reconnected");
                } else {
                    tracing::info!("user-data WS connected");
                }
                opened_once = true;
                ws
            }
            Err((what, msg, wait, spared)) => {
                failed_opens += 1;
                failing |= !spared;
                // A spared refusal counts nothing, so the reading is `Connecting` unless an earlier open failed.
                lane.down(&msg, if failing { failed_opens } else { 0 }, wait);
                if nap_unless_stopped(stop, poll, wait) {
                    break;
                }
                let backoff_ms = wait.as_millis() as u64;
                if !failing {
                    // `info`: a reopen, by the order path's level rule; bounded by the venue's refusal mode.
                    tracing::info!(
                        error = %msg,
                        backoff_ms,
                        "user-data WS open refused inside the venue's refusal budget — reconnecting"
                    );
                    continue;
                }
                match failed_open_log.as_mut().map(|log| log.tick(pump_clock::since(started))) {
                    None => tracing::warn!(
                        error = %msg,
                        backoff_ms,
                        "user-data WS {what} — reconnecting"
                    ),
                    Some(ReconnectLogLine::Onset { next_in }) => tracing::warn!(
                        error = %msg,
                        backoff_ms,
                        next_line_in_s = next_in.as_secs(),
                        "user-data WS {what} — reconnecting"
                    ),
                    Some(ReconnectLogLine::Heartbeat { down_for, next_in }) => tracing::warn!(
                        error = %msg,
                        backoff_ms,
                        failed_opens,
                        down_for_s = down_for.as_secs(),
                        next_line_in_s = next_in.as_secs(),
                        "user-data WS still cannot open — reconnecting"
                    ),
                    Some(ReconnectLogLine::Quiet) => {}
                }
                continue;
            }
        };
        let opened = pump_clock::now();
        let mut last_ping = pump_clock::now(); // init right after connect (Python parity)
        // Silent-stall watchdog clock. Tracked in the loop (not behind a `WebsocketSessionUserStream` method like
        // the market pump's `since_last_frame`) so it works for EVERY existing stream impl with no
        // trait change — a defaulted trait method would leave the watchdog silently inert on any
        // impl that forgot to override it.
        let mut last_rx = pump_clock::now(); // init right after connect, same as `last_ping`
        // A session the pump ends for its silence is a failure (`Error`), unless its ladder spares a silence; one the
        // venue or the socket ended is a reopen (`Connecting`): `vike_model::feed_status::order_path_state` reads the
        // lane's count.
        let mut stalled = false;
        // The venue announced this session's end: the reopen is at once (`ReconnectBackoff::announced`).
        let mut announced = false;
        // `None`: stopped. `Some`: why the session ended.
        let ended = loop {
            if stop.load(Ordering::Relaxed) {
                break None;
            }
            if let Some((interval, ping)) = keepalive.as_mut()
                && pump_clock::since(last_ping) >= *interval
            {
                ping(&mut ws);
                last_ping = pump_clock::now();
            }
            match ws.recv() {
                // The half-dead socket: open, silent, no error. Without the watchdog arm the loop
                // parks here forever, so `on_reconnect` never fires and the resync never replays
                // what was missed. A trip ends the session as an ordinary transport hiccup, which
                // is what makes the EXISTING recovery reachable.
                Err(WebsocketSessionUserError::Timeout) => match idle_threshold {
                    Some(idle) if pump_clock::since(last_rx) >= idle => {
                        let idle_ms = idle.as_millis() as u64;
                        if backoff.silence_spared() {
                            // `info`: no heartbeat cadence is documented, so this is a reopen.
                            tracing::info!(
                                idle_ms,
                                "user-data WS quiet past its idle bound — reopening"
                            );
                        } else {
                            tracing::warn!(idle_ms, "user-data WS silent stall — reconnecting");
                            stalled = true;
                        }
                        break Some(format!("no frame from the venue for {} s", idle.as_secs()));
                    }
                    _ => continue, // idle — poll stop()
                },
                Err(WebsocketSessionUserError::Closed(msg)) => {
                    // `info`: a reopen is `Connecting`, and the order path's level rule (`crate::feed_ctx::set_status`'s)
                    // warns only on `Error`. Hyperliquid's testnet ends every session after ~10 min ("1000 Expired").
                    tracing::info!(error = %msg, "user-data WS closed mid-session — reconnecting");
                    break Some(msg); // transport hiccup → reconnect
                }
                Ok(WebsocketSessionUserMessage::Ping(p)) => {
                    last_rx = pump_clock::now(); // control frames ARE liveness on this lane
                    if ws.pong(p).is_err() {
                        break Some("a pong could not be sent".to_string());
                    }
                }
                Ok(WebsocketSessionUserMessage::Other) => last_rx = pump_clock::now(),
                Ok(WebsocketSessionUserMessage::Text(raw)) => {
                    // Stamped BEFORE the parse: a non-JSON keepalive is still proof of liveness.
                    last_rx = pump_clock::now();
                    let Ok(frame) = serde_json::from_str::<Value>(&raw) else {
                        continue; // non-JSON keepalive — skip, do NOT reconnect
                    };
                    if let Some(words) = notice.and_then(|notice| notice(&frame)) {
                        // `info`: a planned reopen is `Connecting` (the order path's level rule).
                        tracing::info!(
                            notice = %words,
                            "user-data WS: the venue announced the session's end — reconnecting now"
                        );
                        announced = true;
                        break Some(words);
                    }
                    for event in decode(&frame) {
                        if !emit(event) {
                            return Ok(()); // core gone — nothing left to feed
                        }
                    }
                }
            }
        };
        let Some(why) = ended else {
            tracing::info!("user-data pump stopped");
            break;
        };
        let lived = pump_clock::since(opened);
        let wait = if announced { backoff.announced(lived) } else { backoff.ended(lived) };
        // The run of failures starts over with the session's end: a stall failed, and a refusal spared after it does not
        // undo that.
        failing = stalled;
        lane.down(&why, u32::from(stalled), wait);
        if nap_unless_stopped(stop, poll, wait) {
            break;
        }
    }
    Ok(())
}

/// The reconnect resync supervisor (venue-agnostic). Reacts to the pump's reconnect-generation
/// counter (bumped by `run_user`'s `on_reconnect` on each WS re-open):
/// after a `settle` delay to let the freshly-reopened stream deliver its own backlog first, it runs
/// `fetch_and_map` (the venue REST history fetch → replay events) and pushes each event via `emit`.
/// The core's `trade_id` dedup + FSM absorb the overlap; only events that landed during the
/// reconnect gap actually apply.
///
/// **The replay schedule is the venue's row.** `history_lag` is the exec lane's
/// `vike_venues::venue_timings::VenueTimingsLane::history_lag`: when it is `Some`, the same replay
/// runs a SECOND time, `settle + history_lag` after the reconnect was seen, for a fill the venue's
/// history had not listed yet at the first (bybit's `execution/list`, 2026-11-20 20:40: the replay
/// 3 s after the fill missed it, the reconcile pass at 20 s saw it). `None` is one replay. A reconnect
/// that lands during the schedule is replayed after it (the gen is read again at the next poll).
///
/// **Each replay asks `charge` first** (the request budget, `crate::read_budget`'s `charge` as
/// [`spawn_user_with_resync`] builds it). A refused replay is SKIPPED, never queued: the next one of
/// the schedule covers it, and after the last the gap-sentinel and the reconcile nudge do.
///
/// Bounded + live-gate-safe: it self-exits when the pump drops the gen `Arc` (its `Weak` upgrade
/// fails) or the core is gone (`emit` → false); it idles between polls (never spins), and a
/// dead/bad-cred session never re-opens, so the gen never bumps — the supervisor stays quiet and
/// exits with the pump. A REST error inside `fetch_and_map` is that closure's concern (bounded
/// retry/log there); the supervisor runs one schedule per bump.
///
/// **`on_reconcile`** is an optional trigger fired AFTER the LAST replay of the schedule completes,
/// once per reconnect-gen bump — never on the initial connect. It is a plain `mpsc::Sender<()>` on purpose:
/// this crate sits below `vike-core` and cannot name its reconcile manager, so the caller hands in a
/// clone of the manager's trigger sender (a start receives it as `AdapterRequest::reconcile_trigger`).
/// Sending is fire-and-forget (unbounded channel; a torn-down manager's disconnected receiver is
/// ignored). `None` = event replay only. The two passes are complementary and NEITHER is complete
/// alone: this replay can miss a terminal that lands entirely inside the settle window, and the
/// venue-side reconcile fetch has its own blind spots (Binance's `openOrders` report has no time
/// filter, so a canceled-with-zero-fills order can leave no report row at all).
///
/// **`initial_gen` is the baseline, and it is a PARAMETER on purpose.** Sampled here, off the live
/// counter, it would depend on when this thread is scheduled: a pump that bumped the gen first would
/// become the baseline, and `current == last_gen` would then hold FOREVER — the reconnect permanently
/// invisible, at any deadline. The caller samples it where it cannot move:
/// [`spawn_user_with_resync`] reads it on the builder thread before the pump thread exists, and
/// `Builder::spawn`'s happens-before edge carries it across. Callers that own the counter (the
/// tests) pass the value it held when they created it.
///
/// **`spawn_ms` is the history-replay floor** (the module doc). A replayed event stamped strictly
/// before it is DROPPED: it is a previous session's activity, already embedded in the venue-reported
/// balance/position this process runs on, and re-emitting it folds its fee and realized PnL a second
/// time through `Account::apply_fill`. `0` disables the floor (every event rides through) — the
/// escape hatch that keeps this a blanket default rather than a trap.
///
/// It is a PARAMETER for the same race as `initial_gen`: the pump is spawned FIRST and can reconnect
/// before this thread is scheduled, so a floor sampled here could land AFTER a genuine gap fill and
/// drop it — the dangerous direction. `vike_bridge_core::exec_actor::run_loop` samples its own floor
/// inside itself, correctly: that call IS the exec thread's spawn, with no earlier point to read.
#[expect(clippy::too_many_arguments)]
pub fn run_user_resync(
    session_gen: Weak<AtomicU64>,
    initial_gen: u64,
    spawn_ms: i64,
    stop: &AtomicBool,
    poll: Duration,
    settle: Duration,
    history_lag: Option<Duration>,
    mut charge: impl FnMut() -> bool,
    mut fetch_and_map: impl FnMut() -> Vec<Event>,
    mut emit: impl FnMut(Event) -> bool,
    on_reconcile: Option<mpsc::Sender<()>>,
) {
    // The CALLER's baseline (see `initial_gen` above): reconnects before it are covered by the
    // initial session snapshot, not replayed here.
    let mut last_gen = initial_gen;
    if session_gen.upgrade().is_none() {
        return; // pump already torn down — nothing to supervise
    }
    while !stop.load(Ordering::Relaxed) {
        pump_clock::sleep(poll);
        let Some(g) = session_gen.upgrade() else {
            break; // pump torn down — nothing left to resync
        };
        let current = g.load(Ordering::Relaxed);
        drop(g);
        if current == last_gen {
            continue; // no reconnect since last check
        }
        last_gen = current;
        let seen = pump_clock::now();
        // Each replay's offset from the reconnect: the settle (let the reopened WS drain its own
        // backlog before we replay REST history), then the row's history lag (the doc above).
        for offset in std::iter::once(settle).chain(history_lag.map(|lag| settle + lag)) {
            let wait = (seen + offset).saturating_duration_since(pump_clock::now());
            if nap_unless_stopped(stop, poll, wait) {
                return;
            }
            if !charge() {
                continue; // refused by the request budget: skipped, the next step covers it (the doc above)
            }
            // The restart law (module doc): a replayed row older than the START is dropped here,
            // whatever the venue closure returned. Counted, then reported ONCE per replay: at most
            // two per WS reconnect, never in the core fold, so the line is visible and bounded.
            let mut pre_spawn = 0usize;
            for event in fetch_and_map() {
                if crate::exec_actor::is_pre_spawn(&event, spawn_ms) {
                    pre_spawn += 1;
                    continue;
                }
                if !emit(event) {
                    return; // core gone
                }
            }
            if pre_spawn > 0 {
                tracing::info!(
                    target: "vike_bridge_core",
                    dropped = pre_spawn,
                    spawn_ms,
                    "reconnect resync: dropped rows older than this process (restart law) — \
                     pre-assembly state is reconcile's job, not the reconnect replay's"
                );
            }
        }
        // Event-replay first, reconcile second (complementary halves — see the doc above).
        // Fire-and-forget: never blocks this thread on the manager's blocking REST fetch.
        if let Some(tx) = &on_reconcile {
            // Receiver gone = the reconcile manager has shut down; nothing to nudge.
            let _ = tx.send(());
        }
    }
}

/// Sleep up to `total`, waking early if `stop` fires — the reconnect-backoff nap every bridge
/// thread takes between sessions. Polls `stop` in 100ms slices, so a shutdown is observed within
/// ~100ms instead of after the full delay.
///
/// Takes `&AtomicBool` (not `&Arc<AtomicBool>`) to match [`run_user`]'s
/// stop seam; an `&Arc<AtomicBool>` call site deref-coerces. This is the FIXED-delay sleep a
/// hand-rolled reconnect loop wants; a growing one asks a [`ReconnectBackoff`] for each wait.
pub fn sleep_unless_stopped(stop: &AtomicBool, total: Duration) {
    let mut left = total;
    let slice = Duration::from_millis(100);
    while left > Duration::ZERO && !stop.load(Ordering::Relaxed) {
        let nap = slice.min(left);
        std::thread::sleep(nap);
        left = left.saturating_sub(nap);
    }
}

/// The pump's reconnect nap: `wait` in `poll` slices; true if stop fired.
fn nap_unless_stopped(stop: &AtomicBool, poll: Duration, wait: Duration) -> bool {
    let deadline = pump_clock::now() + wait;
    while pump_clock::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        pump_clock::sleep(poll.min(deadline.saturating_duration_since(pump_clock::now())));
    }
    stop.load(Ordering::Relaxed)
}

/// Join half of a spawned user-data pump.
pub struct WebsocketSessionUserHandle {
    pub stop: Arc<AtomicBool>,
    pub handle: std::thread::JoinHandle<Result<(), WebsocketSessionUserAuthenticationError>>,
}

impl WebsocketSessionUserHandle {
    /// Deterministic teardown: raise stop, join, surface any auth error.
    pub fn shutdown(self) -> Result<(), WebsocketSessionUserAuthenticationError> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().expect("user-data thread panicked")
    }
}

/// Join half of a pump PLUS its resync supervisor (the with-resync twin of [`WebsocketSessionUserHandle`]).
pub struct WebsocketSessionUserResyncHandle {
    pub stop: Arc<AtomicBool>,
    pub pump: std::thread::JoinHandle<Result<(), WebsocketSessionUserAuthenticationError>>,
    pub resync: std::thread::JoinHandle<()>,
}

impl WebsocketSessionUserResyncHandle {
    /// Deterministic teardown: raise stop, join BOTH threads (pump first — its auth error is the
    /// return value), then the supervisor (which returns `()` and never propagates).
    pub fn shutdown(self) -> Result<(), WebsocketSessionUserAuthenticationError> {
        self.stop.store(true, Ordering::Relaxed);
        let pump_result = self.pump.join().expect("user-data pump panicked");
        let _ = self.resync.join(); // supervisor returns () and never propagates
        pump_result
    }
}

/// The reconnect-generation bump hook [`spawn_with_resync`] hands its pump body. Boxed because the
/// builder constructs it once (capturing the SOLE strong `Arc<AtomicU64>` that drives the resync
/// supervisor's `Weak` self-exit) and hands it into the pump body, which passes it straight to
/// [`run_user`]'s `on_reconnect` seam. `Box<dyn FnMut()>` implements `FnMut()`, so it satisfies that
/// `impl FnMut()` param unchanged.
type WebsocketSessionUserReconnectHook = Box<dyn FnMut() + Send + 'static>;

/// The two threads of [`spawn_user_with_resync`]: the pump (`pump_body`, on the thread named
/// `pump_thread_name`, which reports to the account's `stream_link` cell) PLUS its resync supervisor
/// ([`run_user_resync`] on `resync_thread_name`, at `poll`, `settle` and the row's `history_lag`,
/// replaying `resync_fetch` into `events` once `charge` admits it and firing `on_reconcile`), wiring
/// the fragile lifetime trick ONCE. Its cadences are parameters only so its own tests can run the trick in milliseconds.
///
/// **The `Weak`/`drop` self-exit trick:** the builder creates the strong `session_gen`, moves it (as
/// `gen_p`) into the reconnect hook it hands to the pump body, then drops its own strong ref — so
/// the pump thread holds the ONLY strong `Arc<AtomicU64>`. When that thread ends, the supervisor's
/// `Weak::upgrade` fails and it self-exits. Holding a second strong ref, or dropping before the hook
/// is moved, leaks the supervisor silently. Proven by `spawn_with_resync_tests`.
///
/// **The supervisor's gen baseline is read HERE, not there.** See `initial_gen` below and
/// [`run_user_resync`]'s doc: the pump is spawned first, so a supervisor that sampled the
/// counter on its own thread could adopt a value the pump had already bumped, and then never fire.
fn spawn_with_resync<Body, Charge, Fetch>(
    pump_thread_name: String,
    resync_thread_name: String,
    poll: Duration,
    settle: Duration,
    history_lag: Option<Duration>,
    events: EventSender,
    charge: Charge,
    resync_fetch: Fetch,
    on_reconcile: Option<mpsc::Sender<()>>,
    pump_body: Body,
) -> WebsocketSessionUserResyncHandle
where
    Body: FnOnce(
            Arc<AtomicBool>,
            WebsocketSessionUserReconnectHook,
        ) -> Result<(), WebsocketSessionUserAuthenticationError>
        + Send
        + 'static,
    Charge: FnMut() -> bool + Send + 'static,
    Fetch: FnMut() -> Vec<Event> + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let session_gen = Arc::new(AtomicU64::new(0));
    // The supervisor's baseline, sampled HERE — on the builder thread, BEFORE the pump thread that
    // bumps this counter exists; `Builder::spawn` publishes it (`run_user_resync`'s doc).
    let initial_gen = session_gen.load(Ordering::Relaxed);
    // The history-replay floor (module doc), sampled at the same point for the same reason. Nothing
    // this process placed can have filled before it, and the earliest defensible floor is also the
    // SAFEST: every millisecond later is a millisecond of legitimate gap fills at risk.
    let spawn_ms = vike_model::now_ms();

    // The reconnect hook is the SOLE strong owner of `session_gen` once we drop ours below: it is
    // moved into the pump thread (via `pump_body`), so when that thread ends the supervisor's
    // `Weak` upgrade fails and it self-exits. CHEAP: just record the re-open — the supervisor does
    // the REST/RPC work off the pump thread.
    let gen_p = Arc::clone(&session_gen);
    let on_reconnect: WebsocketSessionUserReconnectHook = Box::new(move || {
        gen_p.fetch_add(1, Ordering::Relaxed);
    });

    let stop_pump = Arc::clone(&stop);
    let link = stream_link::VenueLinkStream::of(&events);
    let pump = std::thread::Builder::new()
        .name(pump_thread_name)
        .spawn(move || {
            stream_link::report_on_this_thread(link);
            caught(|| pump_body(stop_pump, on_reconnect))
        })
        .expect("spawn user-data pump");

    let (stop_s, weak_gen) = (Arc::clone(&stop), Arc::downgrade(&session_gen));
    // Release the builder's strong ref — the pump's reconnect hook now holds the only one.
    drop(session_gen);

    let resync = std::thread::Builder::new()
        .name(resync_thread_name)
        .spawn(move || {
            // A panic in the venue's replay (its REST mapper) ends the replays and nothing else: the pump streams on, and
            // the account's link says the replay is gone (`stream_link::replay_panicked`). `AssertUnwindSafe`: nothing the
            // replay held is touched again.
            let replayed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_user_resync(
                    weak_gen,
                    initial_gen,
                    spawn_ms,
                    &stop_s,
                    poll,
                    settle,
                    history_lag,
                    charge,
                    resync_fetch,
                    |event| events.blocking_send(event).is_ok(),
                    on_reconcile,
                );
            }));
            // A supervisor that is stopping says nothing: its cell may be the next start's.
            if replayed.is_err() && !stop_s.load(Ordering::Acquire) {
                stream_link::replay_panicked(&events);
            }
        })
        .expect("spawn resync supervisor");

    WebsocketSessionUserResyncHandle { stop, pump, resync }
}

/// Dial a private stream within `window` (the venue's exec-lane `dial_timeout`, or
/// `crate::websocket_session::spec::CONNECT_10S` where it states none) and set the socket up for the pump: a read
/// timeout of [`USER_DATA_POLL`] so the pump hears its stop, and no Nagle (`vike_transport::websocket::configure_websocket_stream`).
/// `request` is a URL, or a request built from one when the upgrade carries a header of its own. The
/// error is the open's transport failure in the words every lane used (`connect: …`); a lane whose URL
/// holds a secret redacts it before it reports the words.
pub fn dial_user_stream(
    request: impl IntoClientRequest,
    window: Duration,
) -> Result<WsSocket, String> {
    let request = request.into_client_request().map_err(|e| format!("connect: {e}"))?;
    let socket =
        vike_transport::websocket_proxy::connect_websocket_request(request, None, Some(window))
            .map_err(|e| format!("connect: {e}"))?;
    configure_websocket_stream(&socket, USER_DATA_POLL);
    Ok(socket)
}

/// A spec's app-level keepalive: its cadence, and the hook that sends it on the open session (owned, so
/// the spec can cross to the pump thread). [`run_user`] fires it every cadence while a session is up.
pub type WebsocketSessionUserKeepalive<S> = (Duration, Box<dyn FnMut(&mut S) + Send>);

/// One venue's private WS stream, as [`spawn_user_with_resync`] runs it: every field is the venue's own,
/// the rest of the pump is the builder's (the module doc's "One spawn for every private WS stream").
pub struct WebsocketSessionUserResyncSpec<S, Open, Decode, Fetch> {
    /// The canonical venue id: the `user_data_pump` span's `venue`, carried by every line the pump says.
    pub venue: &'static str,
    /// The threads' prefix: `{lane}-userdata-{symbol}` and `{lane}-resync-{symbol}` (`"binance-perp"`,
    /// `"okx-algo"`).
    pub lane: &'static str,
    /// The mounted symbol: the span's `symbol` and the threads' suffix (empty: a stream of the whole
    /// account, named `{lane}-userdata`).
    pub symbol: String,
    /// The account's lane: the sink when there is no fence, and the `stream_link` cell the pump reports to.
    pub events: EventSender,
    /// The fence the exec thread's submits run through: BOTH producers pass it, the pump's sink and the
    /// replay (`crate::exec_actor::submit_fence`'s module doc). `None`: straight to `events`.
    pub fence: Option<Arc<SubmitFence>>,
    /// The reconnect replay's history fetch, at most [`RESYNC_HISTORY_LIMIT`] rows, on the supervisor
    /// thread ([`run_user_resync`]).
    pub resync_fetch: Fetch,
    /// [`run_user_resync`]'s `on_reconcile`.
    pub on_reconcile: Option<mpsc::Sender<()>>,
    /// Dial ([`dial_user_stream`]), log in and subscribe (`crate::websocket::send_and_await`), handed the pump's
    /// stop. Runs on the pump thread, once per open.
    pub open: Open,
    /// One frame to its events.
    pub decode: Decode,
    /// [`run_user_with_notice`]'s `notice`: the frame by which the venue announces the session's end, for the row
    /// whose `announced_close` is `Handled` (`vike_venues::venue_connection`); `None` where the bridge reads none.
    pub notice: Option<WebsocketSessionUserNotice>,
    /// The app-level keepalive and its cadence while a session is up; `None` where the server pings.
    pub keepalive: Option<WebsocketSessionUserKeepalive<S>>,
    /// [`run_user`]'s `idle_threshold`, from the venue's server-heartbeat contract.
    pub idle_threshold: Option<Duration>,
    /// [`run_user`]'s reconnect ladder: [`ReconnectBackoff::from_row`] over the exec lane row's `reconnect`
    /// (`vike_venues::venue_timings`), with the modes the venue states (a refusal wait, a reset after a lived session).
    pub backoff: ReconnectBackoff,
    /// [`run_user_resync`]'s `history_lag`: the exec lane row's `history_lag`
    /// (`vike_venues::venue_timings`), read by the bridge, never written here. `None`: one replay.
    pub history_lag: Option<Duration>,
}

/// Spawn a venue's private stream from its [`WebsocketSessionUserResyncSpec`]: the pump and its resync
/// supervisor ([`spawn_with_resync`]) on the mechanism's [`USER_DATA_POLL`] and [`RESYNC_SETTLE`] and the
/// venue's `history_lag`, with a pump body that enters the `user_data_pump` span and runs the venue's
/// open, decoder and keepalive on
/// [`run_user`]'s loop over the spec's `backoff`, its failed-open line paced by
/// [`RECONNECT_LOG`] (owner ruling B-13), and both producers through the fence when there is one. Each
/// replay is charged to the venue's read budget first, one unit for the mounted `symbol`
/// (`crate::read_budget`'s `charge`).
pub fn spawn_user_with_resync<S, Open, Decode, Fetch>(
    spec: WebsocketSessionUserResyncSpec<S, Open, Decode, Fetch>,
) -> WebsocketSessionUserResyncHandle
where
    S: WebsocketSessionUserStream + 'static,
    Open: FnMut(&AtomicBool) -> WebsocketSessionUserOpenOutcome<S> + Send + 'static,
    Decode: Fn(&Value) -> Vec<Event> + Send + 'static,
    Fetch: FnMut() -> Vec<Event> + Send + 'static,
{
    let WebsocketSessionUserResyncSpec {
        venue,
        lane,
        symbol,
        events,
        fence,
        resync_fetch,
        on_reconcile,
        open,
        decode,
        notice,
        keepalive,
        idle_threshold,
        backoff,
        history_lag,
    } = spec;
    let (pump_thread_name, resync_thread_name) =
        (thread_name(lane, "userdata", &symbol), thread_name(lane, "resync", &symbol));
    let resync_fetch: Box<dyn FnMut() -> Vec<Event> + Send> = match &fence {
        Some(fence) => fence.fenced_fetch(resync_fetch),
        None => Box::new(resync_fetch),
    };
    let charged = symbol.clone();
    let charge = move || crate::read_budget::charge(venue, &charged, 1);
    let pump = UserPump {
        venue,
        symbol,
        sink: events.clone(),
        fence,
        open,
        decode,
        notice,
        keepalive,
        idle_threshold,
        backoff,
    };
    spawn_with_resync(
        pump_thread_name,
        resync_thread_name,
        USER_DATA_POLL,
        RESYNC_SETTLE,
        history_lag,
        events,
        charge,
        resync_fetch,
        on_reconcile,
        move |stop: Arc<AtomicBool>, on_reconnect: WebsocketSessionUserReconnectHook| {
            pump.run(&stop, on_reconnect)
        },
    )
}

/// One venue's private stream with NO reconnect replay, as [`spawn_user`] runs it: the fields of
/// [`WebsocketSessionUserResyncSpec`] but its replay's and its `notice` (no venue on this spawn announces a close),
/// and the venue's own `on_reconnect` in their place.
pub struct WebsocketSessionUserSpec<S, Open, Decode, Reconnect> {
    /// The canonical venue id: the `user_data_pump` span's `venue`.
    pub venue: &'static str,
    /// The thread's prefix: `{lane}-userdata-{symbol}` (`{lane}-userdata` for a stream of the whole account).
    pub lane: &'static str,
    /// The mounted symbol: the span's `symbol` and the thread's suffix; empty for a stream of the whole account.
    pub symbol: String,
    /// The account's lane: the sink when there is no fence, and the `stream_link` cell the pump reports to.
    pub events: EventSender,
    /// The fence the exec thread's submits run through (`crate::exec_actor::submit_fence`); `None`: straight to `events`.
    pub fence: Option<Arc<SubmitFence>>,
    /// Dial ([`dial_user_stream`]), log in and subscribe, handed the pump's stop; on the pump thread, once per open.
    pub open: Open,
    /// One frame to its events.
    pub decode: Decode,
    /// The app-level keepalive and its cadence while a session is up; `None` where the server pings.
    pub keepalive: Option<WebsocketSessionUserKeepalive<S>>,
    /// [`run_user`]'s `idle_threshold`, from the venue's server-heartbeat contract.
    pub idle_threshold: Option<Duration>,
    /// [`run_user`]'s reconnect ladder, as [`WebsocketSessionUserResyncSpec`]'s `backoff`.
    pub backoff: ReconnectBackoff,
    /// Fires on a RE-open, never the first ([`run_user`]'s `on_reconnect`), on the pump thread:
    /// cheap, never blocking (hyperliquid arms its reconcile nudge for the snapshot its stream re-sends).
    pub on_reconnect: Reconnect,
}

/// Spawn a venue's private stream from its [`WebsocketSessionUserSpec`]: [`spawn_user_with_resync`]'s pump, on one named
/// thread that reports its `stream_link` cell, with the venue's `on_reconnect` where that one bumps its replay's
/// generation, and no resync supervisor.
pub fn spawn_user<S, Open, Decode, Reconnect>(
    spec: WebsocketSessionUserSpec<S, Open, Decode, Reconnect>,
) -> WebsocketSessionUserHandle
where
    S: WebsocketSessionUserStream + 'static,
    Open: FnMut(&AtomicBool) -> WebsocketSessionUserOpenOutcome<S> + Send + 'static,
    Decode: Fn(&Value) -> Vec<Event> + Send + 'static,
    Reconnect: FnMut() + Send + 'static,
{
    let WebsocketSessionUserSpec {
        venue,
        lane,
        symbol,
        events,
        fence,
        open,
        decode,
        keepalive,
        idle_threshold,
        backoff,
        on_reconnect,
    } = spec;
    let name = thread_name(lane, "userdata", &symbol);
    let link = stream_link::VenueLinkStream::of(&events);
    let pump = UserPump {
        venue,
        symbol,
        sink: events,
        fence,
        open,
        decode,
        notice: None,
        keepalive,
        idle_threshold,
        backoff,
    };
    let stop = Arc::new(AtomicBool::new(false));
    let stop_pump = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            stream_link::report_on_this_thread(link);
            caught(|| pump.run(&stop_pump, on_reconnect))
        })
        .expect("spawn user-data pump");
    WebsocketSessionUserHandle { stop, handle }
}

/// A pump body run so its panic ends the pump and nothing else: the panic is in the log (the process's panic hook), the
/// account's link reads failed (`stream_link`'s lane, dropped while the thread unwinds, gives its path up), and the
/// join that `shutdown` makes on the exec thread's teardown is no second panic there. `AssertUnwindSafe`: nothing the
/// body held is touched again.
fn caught(
    body: impl FnOnce() -> Result<(), WebsocketSessionUserAuthenticationError>,
) -> Result<(), WebsocketSessionUserAuthenticationError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or(Ok(()))
}

/// `{lane}-{role}-{symbol}`, or `{lane}-{role}` for a stream of the whole account.
fn thread_name(lane: &str, role: &str, symbol: &str) -> String {
    match symbol {
        "" => format!("{lane}-{role}"),
        symbol => format!("{lane}-{role}-{symbol}"),
    }
}

/// The pump body [`spawn_user`] and [`spawn_user_with_resync`] run on the pump thread: the venue's half of a spec.
struct UserPump<S, Open, Decode> {
    venue: &'static str,
    symbol: String,
    sink: EventSender,
    fence: Option<Arc<SubmitFence>>,
    open: Open,
    decode: Decode,
    notice: Option<WebsocketSessionUserNotice>,
    keepalive: Option<WebsocketSessionUserKeepalive<S>>,
    idle_threshold: Option<Duration>,
    backoff: ReconnectBackoff,
}

impl<S, Open, Decode> UserPump<S, Open, Decode>
where
    S: WebsocketSessionUserStream,
    Open: FnMut(&AtomicBool) -> WebsocketSessionUserOpenOutcome<S>,
    Decode: Fn(&Value) -> Vec<Event>,
{
    /// Enter the `user_data_pump` span and run the venue's open, decoder, notice hook and keepalive on
    /// [`run_user_with_notice`]'s loop over the spec's `backoff`, its failed-open line paced by
    /// [`RECONNECT_LOG`] (owner ruling B-13), the sink through the fence when there is one.
    fn run(
        self,
        stop: &AtomicBool,
        on_reconnect: impl FnMut(),
    ) -> Result<(), WebsocketSessionUserAuthenticationError> {
        let Self {
            venue,
            symbol,
            sink,
            fence,
            mut open,
            decode,
            notice,
            keepalive,
            idle_threshold,
            backoff,
        } = self;
        let _span = tracing::info_span!("user_data_pump", venue, %symbol).entered();
        run_user_with_notice(
            || open(stop),
            decode,
            notice,
            |event| match &fence {
                Some(fence) => fence.deliver(event),
                None => sink.blocking_send(event).is_ok(),
            },
            stop,
            USER_DATA_POLL,
            backoff,
            keepalive,
            on_reconnect,
            idle_threshold,
            Some(RECONNECT_LOG),
        )
    }
}

#[cfg(test)]
mod sleep_unless_stopped_tests {
    use super::*;

    /// The whole point of the helper: a reconnect nap must NOT hold shutdown hostage
    /// for its full duration. A stop raised mid-nap is observed within ~a slice.
    #[test]
    fn a_stop_mid_nap_wakes_it_early() {
        let stop = Arc::new(AtomicBool::new(false));
        let st = Arc::clone(&stop);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            st.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        sleep_unless_stopped(&stop, Duration::from_secs(30));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "stop must cut the nap short, took {:?}",
            started.elapsed()
        );
    }

    /// Absent a stop it sleeps the FULL duration (it is a delay, not a poll that returns early),
    /// and an already-stopped flag returns immediately without napping at all.
    #[test]
    fn sleeps_the_full_span_then_returns_instantly_once_stopped() {
        let stop = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        sleep_unless_stopped(&stop, Duration::from_millis(250));
        assert!(started.elapsed() >= Duration::from_millis(250), "must not return early");

        stop.store(true, Ordering::Relaxed);
        let started = Instant::now();
        sleep_unless_stopped(&stop, Duration::from_secs(30));
        assert!(started.elapsed() < Duration::from_secs(1), "already stopped → no nap");
    }
}

/// How long the pump/resync tests wait for a CROSS-THREAD handshake before declaring failure.
///
/// Every use sits in a poll-until-success loop that sleeps ~2 ms and breaks the instant the
/// condition holds, so this bounds only how long a BROKEN test takes to fail. Each handshake is
/// bounded by one `poll` (1 ms) plus `settle` (0) plus a scheduling hop, so 5 s is ~1000x the real
/// cost. ⚠ Never widen it to cure a "never fired" failure: that shape has been a real race (a gen
/// baseline sampled on the supervisor's own thread, `run_user_resync`'s `initial_gen`), which
/// no deadline fixes — a looser one only turns a permanent hang into a slow one called flake.
#[cfg(test)]
const TEST_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);

/// One replayed fill, stamped — the restart law's test subject. Sits here because BOTH test
/// modules need it: `spawn_with_resync_tests` proves the builder ARMS the floor,
/// `resync_supervisor_tests` proves the loop APPLIES it, and neither proves the other.
#[cfg(test)]
fn fill_at(order_client_id: &str, trade_id: &str, ts: i64) -> Event {
    let fill: vike_model::events::FillEvent = serde_json::from_value(serde_json::json!({
        "trade_id": trade_id, "order_client_id": order_client_id, "venue": "test", "symbol": "BTCUSDT",
        "side": 1, "last_qty": 1.0, "last_price": 50000.0, "commission": 0.1,
        "commission_asset": "USDT", "timestamp": ts
    }))
    .unwrap();
    Event::Fill(fill)
}

#[cfg(test)]
mod spawn_with_resync_tests;

#[cfg(test)]
mod resync_supervisor_tests;

#[cfg(test)]
mod idle_watchdog_tests;

#[cfg(test)]
mod spawn_user_with_resync_tests;

#[cfg(test)]
mod announced_close_tests;
