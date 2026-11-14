//! The venue-neutral user-data WS pump: a venue-supplied `open_websocket()` (connects + auths +
//! subscribes inside) and a pure `decode(frame)`; a recv loop on a short poll timeout,
//! exponential-backoff reconnect on transport hiccups, a clean stop path, and auth/protocol
//! failures RETURNED immediately (never reconnect-looped). The stream is a trait seam
//! ([`WebsocketSessionUserStream`]), so the loop is offline-testable with scripted frames.
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
//! then its login steps through `crate::websocket::send_and_await`), its decoder, its keepalive, its idle
//! threshold and its fence, and the builder does the rest ONCE: the thread names, the `user_data_pump`
//! span carrying the venue, both producers through the fence, the mechanism's poll, ceiling and settle,
//! and the reconnect rules of `reconnect` (owner ruling B-13), whose failed-open line it paces with
//! [`reconnect::RECONNECT_LOG`]. A replay reads at most [`RESYNC_HISTORY_LIMIT`] rows. A stream with no
//! REST replay (hyperliquid's re-sends its own snapshot on every subscribe) is [`spawn_user`] from a
//! [`WebsocketSessionUserSpec`]: the same pump, its own `on_reconnect` hook, no resync supervisor.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use vike_execution::EventSender;
use vike_model::events::Event;

use crate::exec_actor::submit_fence::SubmitFence;
use crate::websocket::{WsSocket, configure_websocket_stream};
use reconnect::{RECONNECT_LOG, ReconnectBackoff, ReconnectLog, ReconnectLogLine};

pub mod reconnect;
pub mod stream_link;

// ---------------------------------------------------------------------------------------------
// MECHANISM defaults of this pump (decision 0160). None of these is a fact about a venue, so none
// is a row of `vike_venues::venue_timings`, which holds only what has a venue's reason
// behind it: they are the numbers every bridge's pump spawn used to copy, written once. The table
// DOES declare each user-data venue's reconnect policy, and a test in `websocket_session/spec_tests.rs` holds
// that declaration equal to the two backoff constants below, so the two cannot drift.
// ---------------------------------------------------------------------------------------------

/// The stop-flag poll cadence of a user-data socket read, of the backoff nap and of the resync
/// supervisor's wake: how long a raised stop can wait on a quiet socket. The `poll` argument of
/// [`run_user`] and [`run_user_resync`], as [`spawn_user`] and [`spawn_user_with_resync`] run them.
pub const USER_DATA_POLL: Duration = Duration::from_secs(1);

/// The first reconnect wait of [`run_user`]; the wait doubles per transport
/// fault up to the `max_backoff` the caller passes (see [`USER_DATA_MAX_BACKOFF`]).
pub const USER_DATA_INITIAL_BACKOFF: Duration = Duration::from_secs(1);

/// The reconnect ceiling every user-data pump passes as `max_backoff`.
pub const USER_DATA_MAX_BACKOFF: Duration = Duration::from_secs(30);

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

pub enum WebsocketSessionUserMessage {
    Text(String),
    Ping(Vec<u8>),
    /// binary/pong/other control frames — ignored
    Other,
}

#[derive(Debug)]
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

/// `open_websocket()` outcome: an authed stream, a transport failure (reconnectable), an auth
/// failure (fatal), or a stop observed mid-handshake (clean exit).
pub enum WebsocketSessionUserOpenOutcome<S> {
    Ready(S),
    Transport(String),
    Auth(WebsocketSessionUserAuthenticationError),
    Stopped,
}

/// Persistent pump: open_websocket → recv loop → decode → emit, with reconnect + stop, plus the opt-in
/// silent-stall watchdog (the module doc's rationale).
///
/// The reconnect wait is a [`ReconnectBackoff`] from [`USER_DATA_INITIAL_BACKOFF`] to `max_backoff`
/// (every caller passes the value of [`USER_DATA_MAX_BACKOFF`]), with its one reset rule: a successful
/// re-open does not reset it, **a session that then LIVED twice the ceiling (60 s at every caller)
/// does**, so a longer session reopens at most once a minute, against the 30 s a failing open retries
/// at. `emit` returning false means the core is gone — treated as stop. Auth errors return `Err`
/// immediately. Every failed open is a `warn` line when `failed_open_log` is `None`; the pump of
/// [`spawn_user`] and [`spawn_user_with_resync`] paces it with [`RECONNECT_LOG`] (owner ruling B-13).
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
pub fn run_user<S: WebsocketSessionUserStream>(
    mut open_websocket: impl FnMut() -> WebsocketSessionUserOpenOutcome<S>,
    decode: impl Fn(&Value) -> Vec<Event>,
    mut emit: impl FnMut(Event) -> bool,
    stop: &AtomicBool,
    poll: Duration,
    max_backoff: Duration,
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
    let mut backoff = ReconnectBackoff::new(USER_DATA_INITIAL_BACKOFF, max_backoff);
    let mut keepalive = keepalive;
    let mut opened_once = false;
    // What the gatewayhub's status verb reads (the `stream_link` module). Written on a transition, never per frame.
    let mut lane = stream_link::VenueLinkStreamLane::of_this_thread();
    let mut failed_opens = 0u32;
    // The log pace's clock: any monotonic reading will do.
    let started = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let mut ws = match open_websocket() {
            WebsocketSessionUserOpenOutcome::Ready(ws) => {
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
            WebsocketSessionUserOpenOutcome::Auth(e) => {
                tracing::error!(error = %e, "user-data WS auth failed — pump exiting");
                lane.gave_up(&e.0, failed_opens + 1);
                return Err(e); // raise, do NOT reconnect-loop
            }
            WebsocketSessionUserOpenOutcome::Stopped => break,
            WebsocketSessionUserOpenOutcome::Transport(msg) => {
                failed_opens += 1;
                let wait = backoff.failed();
                lane.down(&msg, failed_opens, wait);
                if nap_unless_stopped(stop, poll, wait) {
                    break;
                }
                let backoff_ms = wait.as_millis() as u64;
                match failed_open_log.as_mut().map(|log| log.tick(started.elapsed())) {
                    None => tracing::warn!(
                        error = %msg,
                        backoff_ms,
                        "user-data WS transport error — reconnecting"
                    ),
                    Some(ReconnectLogLine::Onset { next_in }) => tracing::warn!(
                        error = %msg,
                        backoff_ms,
                        next_line_in_s = next_in.as_secs(),
                        "user-data WS transport error — reconnecting"
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
        let opened = Instant::now();
        let mut last_ping = Instant::now(); // init right after connect (Python parity)
        // Silent-stall watchdog clock. Tracked in the loop (not behind a `WebsocketSessionUserStream` method like
        // the market pump's `since_last_frame`) so it works for EVERY existing stream impl with no
        // trait change — a defaulted trait method would leave the watchdog silently inert on any
        // impl that forgot to override it.
        let mut last_rx = Instant::now(); // init right after connect, same as `last_ping`
        // `None`: stopped. `Some`: why the session ended.
        let ended = loop {
            if stop.load(Ordering::Relaxed) {
                break None;
            }
            if let Some((interval, ping)) = keepalive.as_mut()
                && last_ping.elapsed() >= *interval
            {
                ping(&mut ws);
                last_ping = Instant::now();
            }
            match ws.recv() {
                // The half-dead socket: open, silent, no error. Without the watchdog arm the loop
                // parks here forever, so `on_reconnect` never fires and the resync never replays
                // what was missed. A trip ends the session as an ordinary transport hiccup, which
                // is what makes the EXISTING recovery reachable.
                Err(WebsocketSessionUserError::Timeout) => match idle_threshold {
                    Some(idle) if last_rx.elapsed() >= idle => {
                        tracing::warn!(
                            idle_ms = idle.as_millis() as u64,
                            "user-data WS silent stall — reconnecting"
                        );
                        break Some(format!("no frame from the venue for {} s", idle.as_secs()));
                    }
                    _ => continue, // idle — poll stop()
                },
                Err(WebsocketSessionUserError::Closed(msg)) => {
                    tracing::warn!(error = %msg, "user-data WS closed mid-session — reconnecting");
                    break Some(msg); // transport hiccup → reconnect
                }
                Ok(WebsocketSessionUserMessage::Ping(p)) => {
                    last_rx = Instant::now(); // control frames ARE liveness on this lane
                    if ws.pong(p).is_err() {
                        break Some("a pong could not be sent".to_string());
                    }
                }
                Ok(WebsocketSessionUserMessage::Other) => last_rx = Instant::now(),
                Ok(WebsocketSessionUserMessage::Text(raw)) => {
                    // Stamped BEFORE the parse: a non-JSON keepalive is still proof of liveness.
                    last_rx = Instant::now();
                    let Ok(frame) = serde_json::from_str::<Value>(&raw) else {
                        continue; // non-JSON keepalive — skip, do NOT reconnect
                    };
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
        let wait = backoff.ended(opened.elapsed());
        lane.down(&why, 0, wait);
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
/// Bounded + live-gate-safe: it self-exits when the pump drops the gen `Arc` (its `Weak` upgrade
/// fails) or the core is gone (`emit` → false); it idles between polls (never spins), and a
/// dead/bad-cred session never re-opens, so the gen never bumps — the supervisor stays quiet and
/// exits with the pump. A REST error inside `fetch_and_map` is that closure's concern (bounded
/// retry/log there); the supervisor treats one resync per bump.
///
/// **`on_reconcile`** is an optional trigger fired AFTER the event replay completes, once per
/// reconnect-gen bump — never on the initial connect. It is a plain `mpsc::Sender<()>` on purpose:
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
pub fn run_user_resync(
    session_gen: Weak<AtomicU64>,
    initial_gen: u64,
    spawn_ms: i64,
    stop: &AtomicBool,
    poll: Duration,
    settle: Duration,
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
        std::thread::sleep(poll);
        let Some(g) = session_gen.upgrade() else {
            break; // pump torn down — nothing left to resync
        };
        let current = g.load(Ordering::Relaxed);
        drop(g);
        if current == last_gen {
            continue; // no reconnect since last check
        }
        last_gen = current;
        // Let the reopened WS drain its own backlog before we replay REST history (avoid racing it).
        let deadline = Instant::now() + settle;
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(poll.min(deadline.saturating_duration_since(Instant::now())));
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // The restart law (module doc): a replayed row older than the START is dropped here,
        // whatever the venue closure returned. Counted, then reported ONCE per pass: this fires at
        // most once per WS reconnect, never in the core fold, so one line is visible and bounded.
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
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(poll.min(deadline - Instant::now()));
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
/// ([`run_user_resync`] on `resync_thread_name`, at `poll` and `settle`, replaying `resync_fetch`
/// into `events` and firing `on_reconcile`), wiring the fragile lifetime trick ONCE. Its cadences are
/// parameters only so its own tests can run the trick in milliseconds.
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
fn spawn_with_resync<Body, Fetch>(
    pump_thread_name: String,
    resync_thread_name: String,
    poll: Duration,
    settle: Duration,
    events: EventSender,
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
            pump_body(stop_pump, on_reconnect)
        })
        .expect("spawn user-data pump");

    let (stop_s, weak_gen) = (Arc::clone(&stop), Arc::downgrade(&session_gen));
    // Release the builder's strong ref — the pump's reconnect hook now holds the only one.
    drop(session_gen);

    let resync = std::thread::Builder::new()
        .name(resync_thread_name)
        .spawn(move || {
            run_user_resync(
                weak_gen,
                initial_gen,
                spawn_ms,
                &stop_s,
                poll,
                settle,
                resync_fetch,
                |event| events.blocking_send(event).is_ok(),
                on_reconcile,
            );
        })
        .expect("spawn resync supervisor");

    WebsocketSessionUserResyncHandle { stop, pump, resync }
}

/// Dial a private stream within `window` (the venue's exec-lane `dial_timeout`, or
/// `crate::websocket_session::spec::CONNECT_10S` where it states none) and set the socket up for the pump: a read
/// timeout of [`USER_DATA_POLL`] so the pump hears its stop, and no Nagle (`crate::websocket::configure_websocket_stream`).
/// `request` is a URL, or a request built from one when the upgrade carries a header of its own. The
/// error is the open's transport failure in the words every lane used (`connect: …`); a lane whose URL
/// holds a secret redacts it before it reports the words.
pub fn dial_user_stream(
    request: impl IntoClientRequest,
    window: Duration,
) -> Result<WsSocket, String> {
    let request = request.into_client_request().map_err(|e| format!("connect: {e}"))?;
    let socket = crate::websocket_proxy::connect_websocket_request(request, None, Some(window))
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
    /// The app-level keepalive and its cadence while a session is up; `None` where the server pings.
    pub keepalive: Option<WebsocketSessionUserKeepalive<S>>,
    /// [`run_user`]'s `idle_threshold`, from the venue's server-heartbeat contract.
    pub idle_threshold: Option<Duration>,
}

/// Spawn a venue's private stream from its [`WebsocketSessionUserResyncSpec`]: the pump and its resync
/// supervisor ([`spawn_with_resync`]) on the mechanism's [`USER_DATA_POLL`] and [`RESYNC_SETTLE`], with a
/// pump body that enters the `user_data_pump` span and runs the venue's open, decoder and keepalive on
/// [`run_user`]'s loop to [`USER_DATA_MAX_BACKOFF`], its failed-open line paced by
/// [`RECONNECT_LOG`] (owner ruling B-13), and both producers through the fence when there is one.
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
        keepalive,
        idle_threshold,
    } = spec;
    let (pump_thread_name, resync_thread_name) =
        (thread_name(lane, "userdata", &symbol), thread_name(lane, "resync", &symbol));
    let resync_fetch: Box<dyn FnMut() -> Vec<Event> + Send> = match &fence {
        Some(fence) => fence.fenced_fetch(resync_fetch),
        None => Box::new(resync_fetch),
    };
    let pump = UserPump {
        venue,
        symbol,
        sink: events.clone(),
        fence,
        open,
        decode,
        keepalive,
        idle_threshold,
    };
    spawn_with_resync(
        pump_thread_name,
        resync_thread_name,
        USER_DATA_POLL,
        RESYNC_SETTLE,
        events,
        resync_fetch,
        on_reconcile,
        move |stop: Arc<AtomicBool>, on_reconnect: WebsocketSessionUserReconnectHook| {
            pump.run(&stop, on_reconnect)
        },
    )
}

/// One venue's private stream with NO reconnect replay, as [`spawn_user`] runs it: the fields of
/// [`WebsocketSessionUserResyncSpec`] but its replay's, and the venue's own `on_reconnect` in their place.
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
    /// Fires on a RE-open, never the first ([`run_user`]'s `on_reconnect`), on the pump thread:
    /// cheap, never blocking (hyperliquid nudges its reconcile pass; its stream re-sends its own snapshot).
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
        on_reconnect,
    } = spec;
    let name = thread_name(lane, "userdata", &symbol);
    let link = stream_link::VenueLinkStream::of(&events);
    let pump =
        UserPump { venue, symbol, sink: events, fence, open, decode, keepalive, idle_threshold };
    let stop = Arc::new(AtomicBool::new(false));
    let stop_pump = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            stream_link::report_on_this_thread(link);
            pump.run(&stop_pump, on_reconnect)
        })
        .expect("spawn user-data pump");
    WebsocketSessionUserHandle { stop, handle }
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
    keepalive: Option<WebsocketSessionUserKeepalive<S>>,
    idle_threshold: Option<Duration>,
}

impl<S, Open, Decode> UserPump<S, Open, Decode>
where
    S: WebsocketSessionUserStream,
    Open: FnMut(&AtomicBool) -> WebsocketSessionUserOpenOutcome<S>,
    Decode: Fn(&Value) -> Vec<Event>,
{
    /// Enter the `user_data_pump` span and run the venue's open, decoder and keepalive on
    /// [`run_user`]'s loop to [`USER_DATA_MAX_BACKOFF`], its failed-open line paced by
    /// [`RECONNECT_LOG`] (owner ruling B-13), the sink through the fence when there is one.
    fn run(
        self,
        stop: &AtomicBool,
        on_reconnect: impl FnMut(),
    ) -> Result<(), WebsocketSessionUserAuthenticationError> {
        let Self { venue, symbol, sink, fence, mut open, decode, keepalive, idle_threshold } = self;
        let _span = tracing::info_span!("user_data_pump", venue, %symbol).entered();
        run_user(
            || open(stop),
            decode,
            |event| match &fence {
                Some(fence) => fence.deliver(event),
                None => sink.blocking_send(event).is_ok(),
            },
            stop,
            USER_DATA_POLL,
            USER_DATA_MAX_BACKOFF,
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
