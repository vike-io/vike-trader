//! The venue-neutral user-data WS pump: a venue-supplied `open_ws()` (connects + auths +
//! subscribes inside) and a pure `decode(frame)`; a recv loop on a short poll timeout,
//! exponential-backoff reconnect on transport hiccups, a clean stop path, and auth/protocol
//! failures RETURNED immediately (never reconnect-looped). The stream is a trait seam
//! ([`UserStream`]), so the loop is offline-testable with scripted frames.
//!
//! **Silent-stall watchdog (opt-in).** [`run_user_data_forever_with_idle`] is the exec-lane twin of
//! the market pump's `idle_threshold`: a half-dead socket (open, no frames, no error, no close)
//! otherwise parks in the read-timeout arm forever. It matters MORE here than on the market lane,
//! because this lane's recovery (`on_reconnect` → [`run_resync_supervisor`]) is reconnect-gated: no
//! reconnect means no gen bump means no resync, so a terminal landing during the silence is lost
//! (private WS streams don't replay). OFF by default ([`run_user_data_forever`] passes `None`); a
//! venue's threshold must key off its SERVER-PING contract, never data cadence — an idle account
//! emits no events for hours, so only inbound control frames prove liveness.
//!
//! ## History-replay floor (the restart law, second half)
//! [`run_resync_supervisor`] is the SECOND consumer of the venue `resync` closures; the first,
//! `exec_actor::run_loop`'s gap-sentinel, is floored too (that module's doc is the full argument).
//! Each closure fetches history bounded by a ROW COUNT only (each venue's `RESYNC_HISTORY_LIMIT`,
//! no start time), and the only thing stopping a replayed row from folding twice through the
//! non-idempotent `Account::apply_fill` is the core's in-memory `seen_trade_ids`, EMPTY in a fresh
//! process. The `opened_once` guard in [`run_user_data_forever_with_idle`] means a fresh process
//! does not resync at mount, but every LATER reconnect does, replaying the pre-mount window.
//!
//! **The floor is here, in this one loop, not in the venue closures**: binance perp and BOTH aster
//! lanes reach it through `vike_binance::family::listenkey`'s `spawn_user_data_with_resync`, which a
//! grep for [`spawn_pump_with_resync`] does not see; a per-venue floor would miss them.
//!
//! **Why a blanket floor is safe.** Every consumer's closure returns FILL-like and TERMINAL events
//! only: each venue's mapper skips still-open orders by construction (binance/aster `NEW`, bybit's
//! `/v5/order/history` is closed orders only, okx's non-`canceled` states, deribit's
//! `open`/`untriggered`/`triggered`, polymarket's `user_ws::decode_order` ignores `PLACEMENT` and its
//! REST `/data/orders` rows carry no `type`). **No consumer synthesizes open-order state, so no floor
//! can drop a live resting order.** Polymarket stamps `ts: 0` on every event its history path emits
//! (`decode_trade` reads only the WS `timestamp`, absent from the `/data/trades` REST rows), and
//! `exec_actor::is_pre_spawn` lets an unstamped event ride through, so the floor is a no-op there.
//!
//! `spawn_ms = 0` disables the floor: a consumer that genuinely replays PRE-MOUNT state opts out AT
//! ITS CALL SITE, visibly, instead of the floor being weakened for everyone.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_exec::EventSender;
use vike_model::events::Event;

/// Auth/protocol error — surfaced, never reconnect-looped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDataAuthError(pub String);

impl std::fmt::Display for UserDataAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for UserDataAuthError {}

pub enum StreamMsg {
    Text(String),
    Ping(Vec<u8>),
    /// binary/pong/other control frames — ignored
    Other,
}

#[derive(Debug)]
pub enum StreamError {
    /// read-timeout poll tick — NOT a failure; loop back to check stop()
    Timeout,
    /// transport failure/close — reconnect with backoff
    Closed(String),
}

/// One authenticated venue stream (post-handshake).
pub trait UserStream {
    fn recv(&mut self) -> Result<StreamMsg, StreamError>;
    fn pong(&mut self, payload: Vec<u8>) -> Result<(), StreamError>;
}

/// App-level keepalive: (interval, hook). Fires while a session is up.
pub type Keepalive<'a, S> = (Duration, &'a mut dyn FnMut(&mut S));

/// `open_ws()` outcome: an authed stream, a transport failure (reconnectable), an auth
/// failure (fatal), or a stop observed mid-handshake (clean exit).
pub enum OpenOutcome<S> {
    Ready(S),
    Transport(String),
    Auth(UserDataAuthError),
    Stopped,
}

/// Persistent pump: open_ws → recv loop → decode → emit, with reconnect + stop.
///
/// The reconnect backoff starts at 1 s and doubles per transport fault up to `max_backoff`; a
/// successful re-open does NOT reset it (deliberate: a socket that keeps dropping keeps its
/// accumulated wait). `emit` returning false means the core is gone — treated as stop. Auth errors
/// return `Err` immediately.
pub fn run_user_data_forever<S: UserStream>(
    open_ws: impl FnMut() -> OpenOutcome<S>,
    decode: impl Fn(&Value) -> Vec<Event>,
    emit: impl FnMut(Event) -> bool,
    stop: &AtomicBool,
    poll: Duration,
    max_backoff: Duration,
    // app-level keepalive: fires every `interval` while the session is up (Binance perp's
    // listenKey PUT; None for venues whose server pings)
    keepalive: Option<Keepalive<'_, S>>,
    // fires on a RE-open (never the first), so a supervisor can trigger a post-reconnect
    // resync — a terminal that landed during the gap is otherwise lost (private WS streams
    // don't replay). Must be CHEAP (bump a gen / try_send a marker), never block the recv
    // loop. Pass `|| {}` when no resync is wired.
    on_reconnect: impl FnMut(),
) -> Result<(), UserDataAuthError> {
    // `None` = the silent-stall watchdog is OFF.
    run_user_data_forever_with_idle(
        open_ws,
        decode,
        emit,
        stop,
        poll,
        max_backoff,
        keepalive,
        on_reconnect,
        None,
    )
}

/// [`run_user_data_forever`] plus the opt-in silent-stall watchdog (the module doc's rationale).
///
/// `idle_threshold` = the longest silence tolerated before the session is declared dead. Silence
/// means NO inbound frame of any kind — Text, Ping, or Other — so a venue whose server pings keeps
/// the session alive with control frames alone, exactly as the market pump's twin does. `None`
/// disables the check entirely and this behaves identically to [`run_user_data_forever`].
///
/// On a trip the session ends like any transport hiccup (`break false` → backoff → re-open), which
/// fires `on_reconnect` and lets [`run_resync_supervisor`] replay whatever landed in the hole. The
/// watchdog therefore adds no new recovery policy — it only makes the EXISTING one reachable.
///
/// ⚠ Pick the threshold from the venue's server-heartbeat contract, NOT from data cadence: on this
/// lane an idle account is legitimately silent for hours, so a data-derived threshold would
/// reconnect a perfectly healthy socket in a loop.
pub fn run_user_data_forever_with_idle<S: UserStream>(
    mut open_ws: impl FnMut() -> OpenOutcome<S>,
    decode: impl Fn(&Value) -> Vec<Event>,
    mut emit: impl FnMut(Event) -> bool,
    stop: &AtomicBool,
    poll: Duration,
    max_backoff: Duration,
    keepalive: Option<Keepalive<'_, S>>,
    mut on_reconnect: impl FnMut(),
    idle_threshold: Option<Duration>,
) -> Result<(), UserDataAuthError> {
    let mut backoff = Duration::from_secs(1);
    let mut keepalive = keepalive;
    let mut opened_once = false;
    while !stop.load(Ordering::Relaxed) {
        let mut ws = match open_ws() {
            OpenOutcome::Ready(ws) => {
                if opened_once {
                    on_reconnect(); // re-open — signal the resync supervisor
                    tracing::info!("user-data WS reconnected");
                } else {
                    tracing::info!("user-data WS connected");
                }
                opened_once = true;
                ws
            }
            OpenOutcome::Auth(e) => {
                tracing::error!(error = %e, "user-data WS auth failed — pump exiting");
                return Err(e); // raise, do NOT reconnect-loop
            }
            OpenOutcome::Stopped => break,
            OpenOutcome::Transport(msg) => {
                let backoff_dur = backoff;
                if sleep_backoff(stop, poll, &mut backoff, max_backoff) {
                    break;
                }
                tracing::warn!(
                    error = %msg,
                    backoff_ms = backoff_dur.as_millis() as u64,
                    "user-data WS transport error — reconnecting"
                );
                continue;
            }
        };
        let mut last_ping = Instant::now(); // init right after connect (Python parity)
        // Silent-stall watchdog clock. Tracked in the loop (not behind a `UserStream` method like
        // the market pump's `since_last_frame`) so it works for EVERY existing stream impl with no
        // trait change — a defaulted trait method would leave the watchdog silently inert on any
        // impl that forgot to override it.
        let mut last_rx = Instant::now(); // init right after connect, same as `last_ping`
        let clean = loop {
            if stop.load(Ordering::Relaxed) {
                break true;
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
                Err(StreamError::Timeout) => match idle_threshold {
                    Some(idle) if last_rx.elapsed() >= idle => {
                        tracing::warn!(
                            idle_ms = idle.as_millis() as u64,
                            "user-data WS silent stall — reconnecting"
                        );
                        break false;
                    }
                    _ => continue, // idle — poll stop()
                },
                Err(StreamError::Closed(msg)) => {
                    tracing::warn!(error = %msg, "user-data WS closed mid-session — reconnecting");
                    break false; // transport hiccup → reconnect
                }
                Ok(StreamMsg::Ping(p)) => {
                    last_rx = Instant::now(); // control frames ARE liveness on this lane
                    if ws.pong(p).is_err() {
                        break false;
                    }
                }
                Ok(StreamMsg::Other) => last_rx = Instant::now(),
                Ok(StreamMsg::Text(raw)) => {
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
        if clean {
            tracing::info!("user-data pump stopped");
            break;
        }
        if sleep_backoff(stop, poll, &mut backoff, max_backoff) {
            break;
        }
    }
    Ok(())
}

/// The reconnect resync supervisor (venue-agnostic). Reacts to the pump's reconnect-generation
/// counter (bumped by `run_user_data_forever`'s `on_reconnect` on each WS re-open): after a
/// `settle` delay to let the freshly-reopened stream deliver its own backlog first, it runs
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
/// **`on_reconcile`** is an optional poke fired AFTER the event replay completes, once per
/// reconnect-gen bump — never on the initial connect. It is a plain `mpsc::Sender<()>` on purpose:
/// this crate sits below `vike-core` and cannot name its reconcile manager, so the caller hands in a
/// clone of the manager's trigger sender (a mount receives it as `MountRequest::recon_trigger`).
/// Sending is fire-and-forget (unbounded channel; a torn-down manager's disconnected receiver is
/// ignored). `None` = event replay only. The two passes are complementary and NEITHER is complete
/// alone: this replay can miss a terminal that lands entirely inside the settle window, and the
/// venue-side reconcile fetch has its own blind spots (Binance's `openOrders` report has no time
/// filter, so a canceled-with-zero-fills order can leave no report row at all).
///
/// **`initial_gen` is the baseline, and it is a PARAMETER on purpose.** Sampled here, off the live
/// counter, it would depend on when this thread is scheduled: a pump that bumped the gen first would
/// become the baseline, and `cur == last_gen` would then hold FOREVER — the reconnect permanently
/// invisible, at any deadline. The caller samples it where it cannot move:
/// [`spawn_pump_with_resync`] reads it on the builder thread before the pump thread exists, and
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
pub fn run_resync_supervisor(
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
        let cur = g.load(Ordering::Relaxed);
        drop(g);
        if cur == last_gen {
            continue; // no reconnect since last check
        }
        last_gen = cur;
        // Let the reopened WS drain its own backlog before we replay REST history (avoid racing it).
        let deadline = Instant::now() + settle;
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(poll.min(deadline.saturating_duration_since(Instant::now())));
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // The restart law (module doc): a replayed row older than the MOUNT is dropped here,
        // whatever the venue closure returned. Counted, then reported ONCE per pass: this fires at
        // most once per WS reconnect, never in the core fold, so one line is visible and bounded.
        let mut pre_spawn = 0usize;
        for ev in fetch_and_map() {
            if crate::exec_actor::is_pre_spawn(&ev, spawn_ms) {
                pre_spawn += 1;
                continue;
            }
            if !emit(ev) {
                return; // core gone
            }
        }
        if pre_spawn > 0 {
            tracing::info!(
                target: "vike_bridge_core",
                dropped = pre_spawn,
                spawn_ms,
                "reconnect resync: dropped rows older than this process (restart law) — \
                 pre-mount state is reconcile's job, not the reconnect replay's"
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
/// Takes `&AtomicBool` (not `&Arc<AtomicBool>`) to match [`run_user_data_forever`]'s stop seam; an
/// `&Arc<AtomicBool>` call site deref-coerces. This is the FIXED-delay sleep a hand-rolled
/// reconnect loop wants, deliberately NOT [`sleep_backoff`], the pump's private
/// exponential-doubling twin (its schedule is [`run_user_data_forever`]'s doc).
pub fn sleep_unless_stopped(stop: &AtomicBool, total: Duration) {
    let mut left = total;
    let slice = Duration::from_millis(100);
    while left > Duration::ZERO && !stop.load(Ordering::Relaxed) {
        let nap = slice.min(left);
        std::thread::sleep(nap);
        left = left.saturating_sub(nap);
    }
}

/// Stop-aware backoff sleep; returns true if stop fired. Doubles + caps afterwards.
fn sleep_backoff(
    stop: &AtomicBool,
    poll: Duration,
    backoff: &mut Duration,
    max_backoff: Duration,
) -> bool {
    let deadline = Instant::now() + *backoff;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(poll.min(deadline - Instant::now()));
    }
    *backoff = (*backoff * 2).min(max_backoff);
    stop.load(Ordering::Relaxed)
}

/// Join half of a spawned user-data pump.
pub struct UserDataFeed {
    pub stop: Arc<AtomicBool>,
    pub handle: std::thread::JoinHandle<Result<(), UserDataAuthError>>,
}

impl UserDataFeed {
    /// Deterministic teardown: raise stop, join, surface any auth error.
    pub fn shutdown(self) -> Result<(), UserDataAuthError> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().expect("user-data thread panicked")
    }
}

/// Join half of a pump PLUS its resync supervisor (the with-resync twin of [`UserDataFeed`]).
pub struct UserDataResyncFeed {
    pub stop: Arc<AtomicBool>,
    pub pump: std::thread::JoinHandle<Result<(), UserDataAuthError>>,
    pub resync: std::thread::JoinHandle<()>,
}

impl UserDataResyncFeed {
    /// Deterministic teardown: raise stop, join BOTH threads (pump first — its auth error is the
    /// return value), then the supervisor (which returns `()` and never propagates).
    pub fn shutdown(self) -> Result<(), UserDataAuthError> {
        self.stop.store(true, Ordering::Relaxed);
        let pump_result = self.pump.join().expect("user-data pump panicked");
        let _ = self.resync.join(); // supervisor returns () and never propagates
        pump_result
    }
}

/// The reconnect-generation bump hook handed to a pump body by [`spawn_pump_with_resync`]. Boxed
/// because the builder constructs it once (capturing the SOLE strong `Arc<AtomicU64>` that drives
/// the resync supervisor's `Weak` self-exit) and hands it into the venue's pump body, which passes
/// it straight to [`run_user_data_forever`]'s `on_reconnect` seam. `Box<dyn FnMut()>` implements
/// `FnMut()`, so it satisfies that `impl FnMut()` param unchanged.
pub type ReconnectHook = Box<dyn FnMut() + Send + 'static>;

/// Everything venue-specific the [`spawn_pump_with_resync`] builder needs. Only the closures/config
/// here differ between venues; the spawn glue (stop flag, session-gen, the `Weak`/`drop` self-exit
/// trick, the two named threads, the `UserDataResyncFeed` assembly) lives ONCE in the builder.
pub struct ResyncPumpSpec<Body, Fetch> {
    /// Pump thread name, e.g. `"binance-userdata-BTCUSDT"`.
    pub pump_thread_name: String,
    /// Resync-supervisor thread name, e.g. `"binance-resync-BTCUSDT"`.
    pub resync_thread_name: String,
    /// Supervisor recv/stop poll cadence (each venue's `POLL`, typically 1s).
    pub poll: Duration,
    /// Post-reconnect settle before the supervisor replays — lets the reopened WS drain its own
    /// backlog first (each venue's 1s `RESYNC_SETTLE`).
    pub settle: Duration,
    /// The core ingest sink the supervisor emits replayed events into.
    pub events: EventSender,
    /// The venue REST/RPC history fetch → replay events (runs on the supervisor thread).
    pub resync_fetch: Fetch,
    /// Optional reconcile poke ([`run_resync_supervisor`]'s `on_reconcile`); `None` = event replay
    /// only.
    pub on_reconcile: Option<mpsc::Sender<()>>,
    /// The venue pump body: runs ON the spawned pump thread. It sets up venue-local state
    /// (signer/gate/keepalive/span) and calls [`run_user_data_forever`] with the supplied `stop`
    /// and the reconnect hook. Handed `Arc<AtomicBool>` (the open closure and the pump both borrow
    /// it) and the [`ReconnectHook`] to pass straight to `run_user_data_forever`.
    pub pump_body: Body,
}

/// Spawn a persistent user-data pump PLUS its resync supervisor, wiring the fragile lifetime trick
/// ONCE; the venue supplies only its closures, names and cadences via [`ResyncPumpSpec`].
///
/// **The `Weak`/`drop` self-exit trick:** the builder creates the strong `session_gen`, moves it (as
/// `gen_p`) into the reconnect hook it hands to the pump body, then drops its own strong ref — so
/// the pump thread holds the ONLY strong `Arc<AtomicU64>`. When that thread ends, the supervisor's
/// `Weak::upgrade` fails and it self-exits. Holding a second strong ref, or dropping before the hook
/// is moved, leaks the supervisor silently. Proven by `spawn_pump_with_resync_tests`.
///
/// **The supervisor's gen baseline is read HERE, not there.** See `initial_gen` below and
/// [`run_resync_supervisor`]'s doc: the pump is spawned first, so a supervisor that sampled the
/// counter on its own thread could adopt a value the pump had already bumped, and then never fire.
pub fn spawn_pump_with_resync<Body, Fetch>(spec: ResyncPumpSpec<Body, Fetch>) -> UserDataResyncFeed
where
    Body: FnOnce(Arc<AtomicBool>, ReconnectHook) -> Result<(), UserDataAuthError> + Send + 'static,
    Fetch: FnMut() -> Vec<Event> + Send + 'static,
{
    let ResyncPumpSpec {
        pump_thread_name,
        resync_thread_name,
        poll,
        settle,
        events,
        resync_fetch,
        on_reconcile,
        pump_body,
    } = spec;

    let stop = Arc::new(AtomicBool::new(false));
    let session_gen = Arc::new(AtomicU64::new(0));
    // The supervisor's baseline, sampled HERE — on the builder thread, BEFORE the pump thread that
    // bumps this counter exists; `Builder::spawn` publishes it (`run_resync_supervisor`'s doc).
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
    let on_reconnect: ReconnectHook = Box::new(move || {
        gen_p.fetch_add(1, Ordering::Relaxed);
    });

    let stop_pump = Arc::clone(&stop);
    let pump = std::thread::Builder::new()
        .name(pump_thread_name)
        .spawn(move || pump_body(stop_pump, on_reconnect))
        .expect("spawn user-data pump");

    let (stop_s, weak_gen) = (Arc::clone(&stop), Arc::downgrade(&session_gen));
    // Release the builder's strong ref — the pump's reconnect hook now holds the only one.
    drop(session_gen);

    let resync = std::thread::Builder::new()
        .name(resync_thread_name)
        .spawn(move || {
            run_resync_supervisor(
                weak_gen,
                initial_gen,
                spawn_ms,
                &stop_s,
                poll,
                settle,
                resync_fetch,
                |ev| events.blocking_send(ev).is_ok(),
                on_reconcile,
            );
        })
        .expect("spawn resync supervisor");

    UserDataResyncFeed { stop, pump, resync }
}

/// Spawn a single user-data pump thread (no resync supervisor) — the stop-flag + named-thread +
/// [`UserDataFeed`] assembly the venues' non-resync `spawn_*` copies share. The venue supplies only
/// its pump body (which sets up venue-local state and calls [`run_user_data_forever`] with a no-op
/// `on_reconnect`).
pub fn spawn_pump<Body>(pump_thread_name: String, pump_body: Body) -> UserDataFeed
where
    Body: FnOnce(Arc<AtomicBool>) -> Result<(), UserDataAuthError> + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop_pump = Arc::clone(&stop);
    let handle = std::thread::Builder::new()
        .name(pump_thread_name)
        .spawn(move || pump_body(stop_pump))
        .expect("spawn user-data pump");
    UserDataFeed { stop, handle }
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
/// baseline sampled on the supervisor's own thread, `run_resync_supervisor`'s `initial_gen`), which
/// no deadline fixes — a looser one only turns a permanent hang into a slow one called flake.
#[cfg(test)]
const TEST_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);

/// One replayed fill, stamped — the restart law's test subject. Sits here because BOTH test
/// modules need it: `spawn_pump_with_resync_tests` proves the builder ARMS the floor,
/// `resync_supervisor_tests` proves the loop APPLIES it, and neither proves the other.
#[cfg(test)]
fn fill_at(coid: &str, trade_id: &str, ts: i64) -> Event {
    let fill: vike_model::events::FillEvent = serde_json::from_value(serde_json::json!({
        "trade_id": trade_id, "client_order_id": coid, "venue": "test", "symbol": "BTCUSDT",
        "side": 1, "last_qty": 1.0, "last_px": 50000.0, "commission": 0.1,
        "commission_asset": "USDT", "ts": ts
    }))
    .unwrap();
    Event::Fill(fill)
}

#[cfg(test)]
mod spawn_pump_with_resync_tests;

#[cfg(test)]
mod resync_supervisor_tests;

#[cfg(test)]
mod idle_watchdog_tests;
