//! Venue-neutral user-data WS pump. Exact port of `exec/user_data_core.py` semantics,
//! threaded instead of asyncio: a venue-supplied `open_ws()` (connects + auths +
//! subscribes inside) and a pure `decode(frame)`; recv loop with a short poll timeout,
//! exponential-backoff reconnect on transport hiccups, a clean stop path, and
//! auth/protocol failures RAISED immediately (never reconnect-looped).
//!
//! The stream is a trait seam so the reliability loop itself is offline-testable with
//! scripted frames (reconnect, stop, auth-fail, bad-JSON tolerance).
//!
//! **Silent-stall watchdog (opt-in).** [`run_user_data_forever_with_idle`] adds the exec-lane
//! twin of the market pump's `idle_threshold` (`market_pump.rs`, net-hardening §B): a half-dead
//! socket — open, no frames, no error, no close — otherwise parks in the read-timeout arm
//! forever. That matters MORE here than on the market-data lane, because this lane's recovery
//! (`on_reconnect` → [`run_resync_supervisor`]) is *reconnect-gated*: no reconnect means no gen
//! bump means no resync, so a terminal landing during the silence is lost outright (private WS
//! streams don't replay). Tripping the watchdog turns an undetectable stall into the
//! already-handled reconnect case. OFF by default ([`run_user_data_forever`] passes `None`), and
//! per-venue thresholds must key off the venue's SERVER-PING contract, never data cadence — on
//! this lane silence is normal (an idle account emits no events for hours), so only inbound
//! control frames prove liveness.
//!
//! ## History-replay floor (the restart law, second half)
//! [`run_resync_supervisor`] is the SECOND consumer of the venue `resync` closures whose first
//! consumer — `exec_actor::run_loop`'s gap-sentinel — the restart law already floors (that module's
//! doc is the full argument). Every one of those closures fetches order/execution history bounded
//! by a ROW COUNT and by nothing else (`RESYNC_HISTORY_LIMIT = 50`; no venue closure passes a start
//! time), and the only thing stopping a replayed row from folding twice is the core's in-memory
//! `seen_trade_ids`, which is EMPTY in a fresh process. Measured on the CI box: each unexplained equity
//! step equalled the sum of PRIOR sessions' costs, exactly.
//!
//! **Why this lane was a live hazard even though the first-connect never replays.** The
//! `opened_once` guard in [`run_user_data_forever_with_idle`] means a fresh process does NOT resync
//! at mount — so this lane looked safe. It is not: it fires on every LATER reconnect, and a restart
//! followed by a WS reconnect hours on replays exactly the same pre-mount window through exactly
//! the same non-idempotent `Account::apply_fill`.
//!
//! **The floor is here, in this one loop, not in the venue closures** — eight production consumers
//! reach this machinery and only five call [`spawn_pump_with_resync`] by name: binance perp and
//! BOTH aster lanes enter through `vike_binance::family::listenkey`'s `spawn_user_data_with_resync`,
//! which a grep for the shared function does not see. A per-venue floor would have been written
//! five times and missed three.
//!
//! **What every consumer's closure actually returns, and why a blanket floor is safe.** Audited one
//! by one: FILL-like and TERMINAL events only. Every venue's mapper SKIPS still-open orders by
//! construction — binance/aster `NEW`, bybit's `/v5/order/history` (closed orders only), okx's
//! non-`canceled` states, deribit's `open`/`untriggered`/`triggered`, and polymarket's
//! `user_ws::decode_order`, whose `PLACEMENT` arm is the ignored `_ => {}` and whose REST
//! `/data/orders` rows carry no `type` field at all. **No consumer synthesizes open-order state, so
//! no floor can drop a live resting order.** Polymarket additionally stamps `ts: 0` on every event
//! its history path can emit (`decode_trade` reads only the WS `timestamp` field, which the
//! `/data/trades` REST rows — which use `match_time` — do not carry), and `exec_actor::is_pre_spawn`
//! lets an unstamped event ride through: the floor is a provable NO-OP there, which is why the
//! venue the blanket rule was feared for is the one it cannot touch.
//!
//! `spawn_ms = 0` disables the floor, and that hatch is the reason this stays a blanket default: a
//! future consumer that genuinely replays PRE-MOUNT state opts out AT ITS CALL SITE, visibly,
//! instead of the floor being weakened for everyone.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_exec::EventSender;
use vike_model::events::Event;

/// Auth/protocol error — surfaced, never reconnect-looped (Python `UserDataAuthError`).
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
/// Python-parity notes: backoff resets to 1s only after a CLEAN (stop-driven) session
/// end — a mid-session drop keeps doubling from the previous value, capped at
/// `max_backoff` (ported semantics, not an accident). `emit` returning false means the
/// core is gone — treated as stop. Auth errors return Err immediately.
#[allow(clippy::too_many_arguments)] // venue-neutral pump: each param is a distinct seam
pub fn run_user_data_forever<S: UserStream>(
    open_ws: impl FnMut() -> OpenOutcome<S>,
    decode: impl Fn(&Value) -> Vec<Event>,
    emit: impl FnMut(Event) -> bool,
    stop: &AtomicBool,
    poll: Duration,
    max_backoff: Duration,
    // app-level keepalive: fires every `interval` while the session is up (Python's
    // `ping` hook — Binance perp's listenKey PUT; None for venues whose server pings)
    keepalive: Option<Keepalive<'_, S>>,
    // audit A3: fires on a RE-open (never the first), so a supervisor can trigger a
    // post-reconnect resync — a terminal that landed during the gap is otherwise lost
    // (private WS streams don't replay). Must be CHEAP (bump a gen / try_send a marker),
    // never block the recv loop. Pass `|| {}` when no resync is wired.
    on_reconnect: impl FnMut(),
) -> Result<(), UserDataAuthError> {
    // `None` = the silent-stall watchdog is OFF: byte-identical to the pre-watchdog pump.
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
#[allow(clippy::too_many_arguments)] // venue-neutral pump: each param is a distinct seam
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
            if let Some((interval, ping)) = keepalive.as_mut() {
                if last_ping.elapsed() >= *interval {
                    ping(&mut ws);
                    last_ping = Instant::now();
                }
            }
            match ws.recv() {
                // The half-dead socket: open, silent, no error. Without the watchdog arm the loop
                // parks here forever, so `on_reconnect` never fires and the A3 resync never
                // replays what was missed. A trip ends the session as an ordinary transport
                // hiccup, which is what makes the EXISTING recovery reachable.
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
            // Python resets backoff=1.0 here — dead in both languages, since a clean
            // (stop-driven) session end always exits the outer loop right after.
            tracing::info!("user-data pump stopped");
            break;
        }
        if sleep_backoff(stop, poll, &mut backoff, max_backoff) {
            break;
        }
    }
    Ok(())
}

/// The audit-A3 resync supervisor (venue-agnostic). Reacts to the pump's reconnect-generation
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
/// **Task 13 (reconnect-triggered reconcile):** `on_reconcile` is an optional, venue-agnostic poke
/// fired AFTER the event-replay above completes, once per reconnect-gen bump — never on the
/// initial connect. It is a plain `mpsc::Sender<()>` rather than a `vike-core` type on purpose:
/// this crate depends on `vike-exec`/`vike-model` only, never `vike-core`, so it cannot name
/// `ReconManager` directly. The caller (vike-core's app-root wiring) hands in a clone of the
/// manager's own trigger sender; sending is fire-and-forget (`Sender::send` never blocks — an
/// unbounded channel — and a disconnected receiver, i.e. a torn-down manager, is silently
/// ignored). `None` (every caller as of this task) reproduces the prior event-replay-only
/// behavior byte-for-byte. The two passes are complementary, NEITHER is complete alone: this
/// event-replay path can miss a terminal that lands entirely inside the settle window, while the
/// venue-side reconcile fetch has its own blind spots (e.g. Binance's `openOrders` report has no
/// time filter, so a canceled-with-zero-fills order can leave no report row at all) — a
/// journal cross-check (Task 14) is the eventual closer, not this pair by itself.
///
/// **`initial_gen` is the baseline, and it is a PARAMETER on purpose.** This used to be sampled here,
/// in this function's prologue, off the live counter — which made the baseline depend on when this
/// thread happened to be scheduled. If the pump bumped the gen before that read landed, the
/// supervisor adopted the bumped value as its baseline and `cur == last_gen` then held *forever*: the
/// reconnect was not merely late, it was permanently invisible, at any deadline. The caller now
/// samples it where the answer cannot move under it — see [`spawn_pump_with_resync`], which reads it
/// on the builder thread before the pump thread exists, so `Builder::spawn`'s happens-before edge
/// carries it across. Callers that own the counter themselves (the tests) pass the value it held when
/// they created it.
///
/// **`spawn_ms` is the history-replay floor — the restart law's second half.** See the module doc.
/// A replayed event stamped strictly before it is DROPPED, because it is a previous session's
/// activity that is already embedded in the venue-reported balance/position this process runs on,
/// and re-emitting it folds its fee and realized PnL a second time through the non-idempotent
/// `Account::apply_fill`. `0` disables the floor entirely (every event rides through) — the shape
/// [`spawn_pump_with_resync`]'s tests and any future consumer that genuinely replays PRE-MOUNT
/// state need, and the escape hatch that keeps this a blanket default rather than a trap.
///
/// It is a PARAMETER for the same reason `initial_gen` is, and it is the same race: the pump is
/// spawned FIRST and can reconnect before this thread is scheduled. Sampled here, in this
/// prologue, the floor could land AFTER a genuine gap fill and drop it — the dangerous direction.
/// Sampled by the caller before the pump thread exists ([`spawn_pump_with_resync`], beside
/// `initial_gen`), it cannot. `vike_bridge_core::exec_actor::run_loop` samples its own floor inside
/// itself instead, correctly: that call IS the exec thread's spawn, with no earlier point to read.
#[allow(clippy::too_many_arguments)] // venue-neutral supervisor: each param is a distinct seam
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
    // Start from the gen as it stood when the CALLER wired us up — reconnects before that are
    // covered by the initial session snapshot, not replayed here. Sampling it here instead would
    // reintroduce the permanently-lost-edge race documented above.
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
        // The restart law (module doc): a replayed row older than the MOUNT is a previous
        // session's activity and is dropped here, whatever the venue closure returned — one
        // venue-agnostic guarantee in the ONE shared loop, so a venue that reaches this machinery
        // through an alias (aster, via the binance listenKey family) cannot miss it.
        // Counted, then reported ONCE per pass: this fires at most once per WS reconnect, never in
        // the core fold, so one aggregated line is both visible and bounded.
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
            let _ = tx.send(());
        }
    }
}

/// Sleep up to `total`, waking early if `stop` fires — the reconnect-backoff nap every bridge
/// thread takes between sessions. Polls `stop` in 100ms slices, so a shutdown is observed within
/// ~100ms instead of after the full delay.
///
/// Hoisted from the byte-identical copies (doc comment included) in the alpaca/ig/oanda stream
/// drivers, plus the hand-rolled `for _ in 0..N { if stop { return } sleep(..) }` twins in the
/// crypto `market_data` pumps (dedup F16). Takes `&AtomicBool` (not `&Arc<AtomicBool>`) to match
/// [`run_user_data_forever`]'s stop seam — an `&Arc<AtomicBool>` call site deref-coerces, so
/// callers are unchanged.
///
/// This is the FIXED-delay sleep the hand-rolled reconnect loops want; it is deliberately NOT
/// [`sleep_backoff`], the pump's private exponential-doubling twin whose reset-only-on-clean-stop
/// schedule is Python-parity-pinned.
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

/// Join half of a pump PLUS its audit-A3 resync supervisor (the with-resync twin of
/// [`UserDataFeed`]). Hoisted from the six byte-identical per-venue `<Venue>UserDataResync` structs
/// (binance spot+perp, bybit, deribit, okx, polymarket); each now keeps only a thin `pub type` alias
/// for its public name, so the venue API surface is unchanged.
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
    /// The vt-core ingest sink the supervisor emits replayed events into.
    pub events: EventSender,
    /// The venue REST/RPC history fetch → replay events (runs on the supervisor thread).
    pub resync_fetch: Fetch,
    /// Optional reconcile poke (reconciliation-activation Task 7/13). `None` reproduces the
    /// event-replay-only behavior byte-for-byte.
    pub on_reconcile: Option<mpsc::Sender<()>>,
    /// The venue pump body: runs ON the spawned pump thread. It sets up venue-local state
    /// (signer/gate/keepalive/span) and calls [`run_user_data_forever`] with the supplied `stop`
    /// and the reconnect hook. Handed `Arc<AtomicBool>` (the open closure and the pump both borrow
    /// it) and the [`ReconnectHook`] to pass straight to `run_user_data_forever`.
    pub pump_body: Body,
}

/// Spawn a persistent user-data pump PLUS its audit-A3 resync supervisor, wiring the fragile
/// lifetime trick ONCE (finding A8). Six venues (binance spot + the listenKey family, bybit, okx,
/// deribit, polymarket) previously pasted this glue with identical comments; the only genuine
/// deltas are the venue closures + names + cadences, all supplied via [`ResyncPumpSpec`].
///
/// **The `Weak`/`drop` self-exit trick, centralized:** the builder creates the strong
/// `session_gen`, moves it (as `gen_p`) into the reconnect hook it hands to the pump body, then
/// drops its own strong ref — so the pump thread holds the ONLY strong `Arc<AtomicU64>`. When that
/// thread ends, the supervisor's `Weak::upgrade` fails and it self-exits. Re-pasting this wrong
/// (holding a second strong ref, or dropping before the hook is moved) is exactly the silent leak
/// A8 removes. Proven by `spawn_pump_with_resync_tests` below.
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
    // bumps this counter exists at all. `Builder::spawn` publishes it, so the supervisor cannot
    // sample it late. Sampling inside the supervisor (as it once did) meant a pump that reconnected
    // before that thread was scheduled moved the baseline out from under it, and its `cur ==
    // last_gen` check then held forever — the reconnect was lost permanently, not merely delayed.
    let initial_gen = session_gen.load(Ordering::Relaxed);
    // The history-replay floor (the restart law — see the module doc), sampled HERE for the same
    // reason and at the same point as `initial_gen`: this is the earliest instant the supervisor
    // can be handed, taken before the pump thread that reconnects exists at all. Nothing this
    // process placed can have been filled before it, so a replayed row stamped earlier belongs to
    // a PREVIOUS session — and the earliest defensible floor is also the SAFEST one, since every
    // millisecond later is a millisecond of legitimate gap fills at risk.
    let spawn_ms = vike_model::clock::now_ms();

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

    /// The whole point of the helper (dedup F16): a reconnect nap must NOT hold shutdown hostage
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

/// How long the pump/resync tests below wait for a CROSS-THREAD handshake before declaring failure.
///
/// **Generous on purpose, and it costs nothing.** Every use sits in a poll-until-success loop that
/// sleeps ~2 ms and breaks the instant the condition holds, so this bounds only how long an actually
/// broken test takes to fail — never the happy path.
///
/// It was a hardcoded 3 s, widened to 30 s because two tests in this module flaked within an hour on
/// 2026-08-02 with their own "never fired" deadline messages. **That widening was treating a
/// symptom.** The cause was a race in [`run_resync_supervisor`], which sampled its gen baseline on
/// its own thread: a pump that bumped the counter first moved the baseline out from under it, and
/// `cur == last_gen` then held FOREVER. The proof it was never slowness — the 30 s deadline blew at
/// 30.083 s, so a 10x widening bought nothing. With the baseline now sampled by the caller (see
/// `spawn_pump_with_resync`'s `initial_gen`), every handshake below is bounded by one `poll` (1 ms)
/// plus `settle` (0) plus a scheduling hop, so 5 s is ~1000x the real cost and there is no longer an
/// unbounded "never fires" case for a bigger number to paper over.
///
/// A too-tight deadline in a timing test is worse than a slow one: it trains everybody to re-run red
/// CI, which is how a genuine failure eventually gets waved through. A too-LOOSE one on a broken
/// gate is worse still: it converts a permanent hang into a 30-second one and calls it flake.
#[cfg(test)]
const TEST_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(5);

/// One replayed fill, stamped — the restart law's test subject. Sits at file scope because BOTH
/// test modules below need it: `spawn_pump_with_resync_tests` proves the builder ARMS the floor,
/// `resync_supervisor_tests` proves the loop APPLIES it, and neither proves the other. Same helper
/// shape as `exec_actor`'s sentinel-side twin, because this lane replays the SAME venue closures
/// through the SAME predicate.
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
mod spawn_pump_with_resync_tests {
    //! Coverage for finding A8's central deliverable: the `Weak`/`drop` self-exit trick and the
    //! stop/join semantics live ONCE in [`spawn_pump_with_resync`], driven here without any venue
    //! I/O (the pump body is a plain closure, no WS).
    use super::*;
    use std::sync::Mutex;
    use vike_exec::event_channel;

    /// A pump body that bumps the reconnect hook `bumps` times (spaced so the supervisor observes
    /// each), then RETURNS on its own — never touching `stop`. This is the crux: because the
    /// builder dropped its `session_gen` strong ref, the hook is the ONLY strong owner, so when the
    /// body returns the supervisor's `Weak` upgrade fails and it self-exits WITHOUT a shutdown.
    #[test]
    fn resync_self_exits_when_the_pump_body_returns() {
        let (events, _ingest) = event_channel(16);
        let fetches = Arc::new(AtomicU64::new(0));
        let fetches_f = Arc::clone(&fetches);

        let feed = spawn_pump_with_resync(ResyncPumpSpec {
            pump_thread_name: "test-pump-selfexit".into(),
            resync_thread_name: "test-resync-selfexit".into(),
            poll: Duration::from_millis(1),
            settle: Duration::from_millis(0),
            events,
            resync_fetch: move || {
                fetches_f.fetch_add(1, Ordering::Relaxed);
                Vec::new() // nothing to emit — this test cares about the lifetime, not replay content
            },
            on_reconcile: None,
            pump_body: move |_stop: Arc<AtomicBool>, mut on_reconnect: ReconnectHook| {
                on_reconnect(); // simulate a WS re-open → gen bump
                std::thread::sleep(Duration::from_millis(30));
                on_reconnect();
                std::thread::sleep(Duration::from_millis(30));
                Ok(()) // pump ends ON ITS OWN — no stop was ever raised
            },
        });

        // The resync supervisor must finish on its own, purely because the pump's hook (the sole
        // strong `session_gen` ref) was dropped when the body returned.
        let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
        while !feed.resync.is_finished() {
            assert!(Instant::now() < deadline, "supervisor did not self-exit when the pump ended");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(fetches.load(Ordering::Relaxed) >= 1, "a reconnect bump must have driven a resync");
        // shutdown is still safe (idempotent) and joins both threads without hanging.
        feed.shutdown().unwrap();
    }

    /// Stop/join semantics: a pump body that loops until `stop` fires; `shutdown()` raises stop,
    /// joins the pump (returning its `Ok`), then the supervisor — no hang.
    #[test]
    fn shutdown_stops_and_joins_both_threads() {
        let (events, _ingest) = event_channel(16);
        let feed = spawn_pump_with_resync(ResyncPumpSpec {
            pump_thread_name: "test-pump-stop".into(),
            resync_thread_name: "test-resync-stop".into(),
            poll: Duration::from_millis(1),
            settle: Duration::from_millis(0),
            events,
            resync_fetch: Vec::<Event>::new,
            on_reconcile: None,
            pump_body: move |stop: Arc<AtomicBool>, _on_reconnect| {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            },
        });
        // Not finished until we ask.
        std::thread::sleep(Duration::from_millis(20));
        assert!(!feed.pump.is_finished(), "pump must run until stop is raised");
        feed.shutdown().unwrap();
    }

    /// The `on_reconcile` poke is forwarded straight into the supervisor and fires once per
    /// reconnect — the shared-builder home of the wiring each venue's `_with_resync` used to paste
    /// (was binance's `resync_trigger_tests`, now covered here for ALL venues at once).
    #[test]
    fn on_reconcile_is_forwarded_and_fires_once_per_reconnect() {
        let (events, _ingest) = event_channel(16);
        let (recon_tx, recon_rx) = mpsc::channel::<()>();
        let armed = Arc::new(AtomicBool::new(false));
        let armed_p = Arc::clone(&armed);

        let feed = spawn_pump_with_resync(ResyncPumpSpec {
            pump_thread_name: "test-pump-recon".into(),
            resync_thread_name: "test-resync-recon".into(),
            poll: Duration::from_millis(1),
            settle: Duration::from_millis(0),
            events,
            resync_fetch: Vec::<Event>::new,
            on_reconcile: Some(recon_tx),
            pump_body: move |stop: Arc<AtomicBool>, mut on_reconnect: ReconnectHook| {
                // Wait to be armed, then bump exactly once, then idle until shutdown.
                while !armed_p.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                on_reconnect();
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            },
        });

        // No reconnect yet → the trigger must not fire.
        std::thread::sleep(Duration::from_millis(15));
        assert!(recon_rx.try_recv().is_err(), "reconcile must not fire before a reconnect");

        armed.store(true, Ordering::Relaxed); // let the pump bump once
        let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
        loop {
            match recon_rx.try_recv() {
                Ok(()) => break,
                Err(mpsc::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "reconcile trigger never fired");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(mpsc::TryRecvError::Disconnected) => panic!("supervisor dropped the sender"),
            }
        }
        assert!(recon_rx.try_recv().is_err(), "reconcile must fire exactly once per reconnect");
        feed.shutdown().unwrap();
    }

    /// **The permanently-lost edge, end to end.** The pump is spawned BEFORE the supervisor, so a
    /// pump body whose very first act is a reconnect races the supervisor's startup. While the
    /// baseline was sampled on the supervisor's own thread, losing that race did not delay the
    /// reconcile — it deleted it: the supervisor adopted the already-bumped value, and its
    /// `cur == last_gen` check then held for the life of the session. Both flakes #1020 widened
    /// deadlines for were this, which is why the widening changed nothing (one blew at 30.083 s).
    ///
    /// With the baseline sampled on the builder thread before `Builder::spawn`, this is
    /// DETERMINISTIC — the supervisor cannot observe anything but 0, so the bump always registers,
    /// and the loop below is not a flaky retry: it is a mutation gate. Restore the old
    /// sample-on-the-supervisor-thread behavior and the rounds start losing the coin flip.
    #[test]
    fn a_reconnect_racing_the_supervisor_spawn_still_reconciles() {
        let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
        for round in 0..64 {
            let (events, _ingest) = event_channel(16);
            let (recon_tx, recon_rx) = mpsc::channel::<()>();

            let feed = spawn_pump_with_resync(ResyncPumpSpec {
                pump_thread_name: format!("test-pump-startrace-{round}"),
                resync_thread_name: format!("test-resync-startrace-{round}"),
                poll: Duration::from_millis(1),
                settle: Duration::from_millis(0),
                events,
                resync_fetch: Vec::<Event>::new,
                on_reconcile: Some(recon_tx),
                pump_body: move |stop: Arc<AtomicBool>, mut on_reconnect: ReconnectHook| {
                    // No arming handshake, no sleep: bump as the FIRST act, so this reconnect is
                    // as early as a reconnect can possibly be relative to the supervisor's start.
                    on_reconnect();
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(())
                },
            });

            loop {
                match recon_rx.try_recv() {
                    Ok(()) => break,
                    Err(mpsc::TryRecvError::Empty) => {
                        assert!(
                            Instant::now() < deadline,
                            "round {round}: a reconnect that beat the supervisor's start was LOST \
                             — the gen baseline is being sampled on the supervisor thread again"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        panic!("supervisor dropped the sender")
                    }
                }
            }
            feed.shutdown().unwrap();
        }
    }

    /// **The builder actually ARMS the history floor** — the half a direct
    /// [`run_resync_supervisor`] test cannot see. `spawn_ms` is a parameter with `0` meaning "no
    /// floor", so every venue's real path depends on this ONE line sampling a live clock; a builder
    /// that passed `0` (or forwarded a stale value) would leave all eight production consumers
    /// replaying previous sessions' fills while `resync_supervisor_tests` stayed green, because
    /// those drive the supervisor directly and supply their own floor.
    ///
    /// Drives the REAL builder — its own pump body, its own reconnect hook, its own supervisor
    /// thread — with a resync that returns one fill stamped an hour ago and one stamped an hour
    /// ahead, and reads the ingest the builder wired itself.
    #[test]
    fn the_builder_arms_the_history_floor_for_every_venue() {
        let (events, mut ingest) = event_channel(16);
        let now = vike_model::clock::now_ms();
        let rows = Arc::new(Mutex::new(Some(vec![
            super::fill_at("c_prev", "e_old", now - 3_600_000),
            super::fill_at("c_this", "e_new", now + 3_600_000),
        ])));
        let rows_f = Arc::clone(&rows);

        let feed = spawn_pump_with_resync(ResyncPumpSpec {
            pump_thread_name: "test-pump-floor".into(),
            resync_thread_name: "test-resync-floor".into(),
            poll: Duration::from_millis(1),
            settle: Duration::from_millis(0),
            events,
            resync_fetch: move || rows_f.lock().unwrap().take().unwrap_or_default(),
            on_reconcile: None,
            pump_body: move |stop: Arc<AtomicBool>, mut on_reconnect: ReconnectHook| {
                on_reconnect(); // the WS re-open this lane replays on
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            },
        });

        // Wait for the post-spawn fill to arrive; the pre-spawn one would precede it in the same
        // batch, so seeing `e_new` first proves `e_old` was dropped rather than merely late.
        let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
        let mut seen: Vec<String> = Vec::new();
        while !seen.iter().any(|t| t == "e_new") {
            assert!(Instant::now() < deadline, "the builder's resync never delivered: {seen:?}");
            while let Ok(ev) = ingest.try_recv() {
                if let vike_exec::lanes::Ingest::Event(Event::Fill(f)) = ev {
                    seen.push(f.trade_id.to_string());
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        feed.shutdown().unwrap();
        while let Ok(ev) = ingest.try_recv() {
            if let vike_exec::lanes::Ingest::Event(Event::Fill(f)) = ev {
                seen.push(f.trade_id.to_string());
            }
        }
        assert!(
            !seen.iter().any(|t| t == "e_old"),
            "`spawn_pump_with_resync` did not arm the floor it samples — every venue's resync is \
             still replaying previous sessions' fills into `apply_fill`: {seen:?}"
        );
    }
}

#[cfg(test)]
mod resync_supervisor_tests {
    use super::*;
    use std::sync::Mutex;
    use vike_model::events::OrderSubmitted;

    /// Audit A3: the supervisor resyncs once per reconnect (gen bump), never without one, and
    /// self-exits when the pump drops the gen Arc (live-gate: a dead session never bumps → quiet).
    #[test]
    fn resync_fires_on_gen_bump_and_exits_with_the_pump() {
        let gen = Arc::new(AtomicU64::new(0));
        let weak = Arc::downgrade(&gen);
        let stop = Arc::new(AtomicBool::new(false));
        let fetches = Arc::new(AtomicU64::new(0));
        let emitted = Arc::new(Mutex::new(Vec::<String>::new()));

        let (stop_t, fetches_t, emitted_t) = (stop.clone(), fetches.clone(), emitted.clone());
        let handle = std::thread::spawn(move || {
            run_resync_supervisor(
                weak,
                0, // the gen as it stood when this test created it
                0, // no history floor — this test covers the gen edge, not the restart law
                &stop_t,
                Duration::from_millis(1),
                Duration::from_millis(0),
                || {
                    fetches_t.fetch_add(1, Ordering::Relaxed);
                    vec![Event::OrderSubmitted(OrderSubmitted {
                        client_order_id: "resynced".into(),
                        ts: 0,
                    })]
                },
                |ev| {
                    if let Event::OrderSubmitted(e) = ev {
                        emitted_t.lock().unwrap().push(e.client_order_id);
                    }
                    true
                },
                None, // Task 13: no reconcile trigger wired — this test covers event-replay only
            );
        });

        // No reconnect yet → no resync.
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(fetches.load(Ordering::Relaxed), 0, "no resync without a reconnect");

        // Simulate a reconnect.
        gen.fetch_add(1, Ordering::Relaxed);
        let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
        while fetches.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline, "resync never fired on the gen bump");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(emitted.lock().unwrap().clone(), vec!["resynced".to_string()]);

        // Pump torn down → supervisor must self-exit.
        drop(gen);
        let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
        while !handle.is_finished() {
            assert!(Instant::now() < deadline, "supervisor did not exit when the pump was gone");
            std::thread::sleep(Duration::from_millis(2));
        }
        handle.join().unwrap();
        assert_eq!(fetches.load(Ordering::Relaxed), 1, "exactly one resync for one reconnect");
    }

    /// **The losing interleaving, made deterministic — no sleeps, no scheduling luck.** The gen is
    /// ALREADY at 1 before the supervisor thread exists: this is exactly the state
    /// `spawn_pump_with_resync` can hand it, since the pump is spawned first and can reconnect
    /// before the supervisor is scheduled. The baseline the caller sampled at wiring time was 0, so
    /// that reconnect must still register.
    ///
    /// While the supervisor sampled its own baseline this was unfixable by waiting: it would read 1,
    /// compare 1 == 1, and sit quiet for the life of the session. The test then does not fail slowly
    /// — it fails at whatever deadline you pick, which is why widening the deadline from 3 s to 30 s
    /// (#1020) produced a 30.083 s failure instead of a 3 s one.
    #[test]
    fn a_gen_bump_that_predates_the_supervisor_is_not_lost() {
        let gen = Arc::new(AtomicU64::new(1)); // the pump already re-opened once
        let weak = Arc::downgrade(&gen);
        let stop = Arc::new(AtomicBool::new(false));
        let fetches = Arc::new(AtomicU64::new(0));

        let (stop_t, fetches_t) = (stop.clone(), fetches.clone());
        let handle = std::thread::spawn(move || {
            run_resync_supervisor(
                weak,
                0, // ...but the caller's baseline, taken before the pump could bump, was 0
                0, // no history floor — this test covers the gen edge, not the restart law
                &stop_t,
                Duration::from_millis(1),
                Duration::from_millis(0),
                || {
                    fetches_t.fetch_add(1, Ordering::Relaxed);
                    Vec::new()
                },
                |_ev| true,
                None,
            );
        });

        // Fires with NO further bump — the pre-existing one is what it must not have swallowed.
        let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
        while fetches.load(Ordering::Relaxed) == 0 {
            assert!(
                Instant::now() < deadline,
                "a reconnect that predated the supervisor was swallowed by its own baseline read"
            );
            std::thread::sleep(Duration::from_millis(2));
        }

        drop(gen);
        stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
        while !handle.is_finished() {
            assert!(Instant::now() < deadline, "supervisor did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        handle.join().unwrap();
    }

    /// Drive one real reconnect resync and collect everything that reached the sink.
    fn replay_once(spawn_ms: i64, rows: Vec<Event>) -> Vec<Event> {
        let gen = Arc::new(AtomicU64::new(0));
        let weak = Arc::downgrade(&gen);
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::<Event>::new()));
        let fetched = Arc::new(AtomicBool::new(false));

        let (stop_t, seen_t, fetched_t) = (stop.clone(), seen.clone(), fetched.clone());
        let handle = std::thread::spawn(move || {
            let mut rows = Some(rows);
            run_resync_supervisor(
                weak,
                0,
                spawn_ms,
                &stop_t,
                Duration::from_millis(1),
                Duration::from_millis(0),
                move || {
                    let out = rows.take().unwrap_or_default();
                    fetched_t.store(true, Ordering::Relaxed);
                    out
                },
                |ev| {
                    seen_t.lock().unwrap().push(ev);
                    true
                },
                None,
            );
        });

        gen.fetch_add(1, Ordering::Relaxed); // the WS reconnect this lane fires on
        let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
        while !fetched.load(Ordering::Relaxed) {
            assert!(Instant::now() < deadline, "the reconnect resync never fired");
            std::thread::sleep(Duration::from_millis(2));
        }
        // The emit loop runs after the fetch returns; give it the one hop it needs, then tear down
        // deterministically (drop the gen so the supervisor self-exits and we JOIN before reading).
        std::thread::sleep(Duration::from_millis(20));
        drop(gen);
        stop.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + TEST_HANDSHAKE_DEADLINE;
        while !handle.is_finished() {
            assert!(Instant::now() < deadline, "supervisor did not exit");
            std::thread::sleep(Duration::from_millis(2));
        }
        handle.join().unwrap();
        let out = seen.lock().unwrap().clone();
        out
    }

    fn trade_ids(evs: &[Event]) -> Vec<String> {
        evs.iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some(f.trade_id.to_string()),
                _ => None,
            })
            .collect()
    }

    /// **The restart law, second half** (module doc). This lane does NOT replay on first connect
    /// (`run_user_data_forever_with_idle`'s `opened_once` guard), which is exactly why it looked
    /// safe and was not: it fires on every LATER reconnect, so a restart followed by a WS reconnect
    /// hours on replays the row-count-bounded venue history — a window with no time bound at all —
    /// into a process whose `seen_trade_ids` dedup is empty. `Account::apply_fill` is not
    /// idempotent (`balance -= commission`, `realized_pnl +=`); measured on the CI box, each unexplained
    /// equity step equalled the sum of PRIOR sessions' costs exactly.
    #[test]
    fn a_reconnect_resync_drops_fills_older_than_the_mount() {
        let now = vike_model::clock::now_ms();
        let seen = replay_once(
            now,
            vec![
                // a PREVIOUS session's fill — retained by the venue, never placed by this process
                fill_at("c_prev_session", "e_old", now - 3_600_000),
                // ...and the WS-gap fill this lane exists to recover
                fill_at("c_this_session", "e_new", now + 3_600_000),
            ],
        );
        let ids = trade_ids(&seen);
        assert!(
            !ids.iter().any(|t| t == "e_old"),
            "a fill stamped BEFORE the mount is a previous session's — it must never reach the \
             ingest, where apply_fill folds its fee and PnL a second time: {seen:?}"
        );
        assert!(
            ids.iter().any(|t| t == "e_new"),
            "a fill stamped after the mount is the reconnect-gap recovery this lane exists for and \
             must still be delivered: {seen:?}"
        );
    }

    /// **The half that matters: a legitimately OLD resting order still gets through.** Polymarket
    /// is the consumer whose `resync_fetch` reads `get_orders` — live orders whose creation
    /// legitimately predates the mount — and erring toward "dropped a live resting order" is far
    /// worse than erring toward "re-booked a fill". The audit's answer is that its history path
    /// stamps `ts: 0` on every event it can emit (`user_ws::decode_trade` reads only the WS
    /// `timestamp` field, absent from the `/data/trades` REST rows, which carry `match_time`), and
    /// `exec_actor::is_pre_spawn` lets an unstamped event ride through *by construction*.
    ///
    /// This pins that end to end on the shared loop; `vike-polymarket`'s
    /// `offline::resync_history_floor` pins the other end — that the REAL `map_polymarket_history`
    /// over a REAL `/data/orders` resting-order row is what produces these shapes.
    #[test]
    fn an_unstamped_replay_row_rides_through_however_old_the_order_is() {
        let now = vike_model::clock::now_ms();
        let seen = replay_once(
            now,
            vec![
                // polymarket's cancel arm: `OrderCanceled { ts: 0 }`, for an order placed long ago
                Event::OrderCanceled(vike_model::events::OrderCanceled {
                    client_order_id: "c_old_resting".to_string(),
                    reason: String::new().into(),
                    ts: 0,
                }),
                // ...and its terminal-fill arm, likewise unstamped
                fill_at("c_old_resting", "0xTRADE:0xORDER", 0),
            ],
        );
        assert!(
            seen.iter().any(
                |e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "c_old_resting")
            ),
            "an UNSTAMPED event carries no evidence of being historical; dropping it would lose \
             live order state, the one direction that is worse than a double-booked fill: {seen:?}"
        );
        assert!(
            trade_ids(&seen).iter().any(|t| t == "0xTRADE:0xORDER"),
            "an unstamped fill must ride through too — no timestamp is ever fabricated: {seen:?}"
        );
    }

    /// The escape hatch, and the reason this stays a blanket default rather than a per-venue
    /// opt-in: `spawn_ms = 0` is "no floor", so a future consumer that genuinely must replay
    /// PRE-MOUNT state opts out AT ITS CALL SITE, visibly, instead of the floor being weakened for
    /// the seven consumers that need it. Also the boundary a mutation would flip.
    #[test]
    fn a_zero_floor_replays_everything() {
        let now = vike_model::clock::now_ms();
        let seen = replay_once(0, vec![fill_at("c", "e_ancient", 1)]);
        assert_eq!(trade_ids(&seen), vec!["e_ancient".to_string()], "{seen:?}");
        // ...and the floor itself is exclusive at its own instant (the other mutation boundary).
        let seen = replay_once(now, vec![fill_at("c", "e_at_floor", now)]);
        assert_eq!(trade_ids(&seen), vec!["e_at_floor".to_string()], "{seen:?}");
    }
}

#[cfg(test)]
mod idle_watchdog_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// The half-dead socket: open, never a frame, never an error, never a close. This is the
    /// shape `StreamError::Timeout => continue` parks on forever, so it is exactly what the
    /// watchdog exists to catch. Each `recv` sleeps a slice so wall-clock actually advances.
    struct StalledStream {
        tick: Duration,
        ticks: Arc<AtomicUsize>,
        /// Raise this stop flag once `ticks` reaches N — lets a watchdog-DISABLED run terminate.
        stop_at: Option<(usize, Arc<AtomicBool>)>,
    }

    impl UserStream for StalledStream {
        fn recv(&mut self) -> Result<StreamMsg, StreamError> {
            let n = self.ticks.fetch_add(1, Ordering::Relaxed) + 1;
            if let Some((at, stop)) = &self.stop_at {
                if n >= *at {
                    stop.store(true, Ordering::Relaxed);
                }
            }
            std::thread::sleep(self.tick);
            Err(StreamError::Timeout)
        }
        fn pong(&mut self, _payload: Vec<u8>) -> Result<(), StreamError> {
            Ok(())
        }
    }

    /// With a threshold set, a silent socket is declared dead and the session RECONNECTS — which
    /// is the whole point: the re-open fires `on_reconnect`, so the A3 resync supervisor gets its
    /// gen bump and can replay whatever landed in the hole.
    #[test]
    fn silent_stall_trips_the_watchdog_and_reconnects() {
        let stop = Arc::new(AtomicBool::new(false));
        let opens = Arc::new(AtomicUsize::new(0));
        let reconnects = Arc::new(AtomicUsize::new(0));
        let ticks = Arc::new(AtomicUsize::new(0));

        let (opens_o, stop_o, ticks_o) = (opens.clone(), stop.clone(), ticks.clone());
        let open_ws = move || {
            // Stop on the SECOND open: by then the watchdog has already proven it fires.
            if opens_o.fetch_add(1, Ordering::Relaxed) + 1 >= 2 {
                stop_o.store(true, Ordering::Relaxed);
            }
            OpenOutcome::Ready(StalledStream {
                tick: Duration::from_millis(2),
                ticks: ticks_o.clone(),
                stop_at: None,
            })
        };

        let reconnects_r = reconnects.clone();
        let result = run_user_data_forever_with_idle(
            open_ws,
            |_| vec![],
            |_| true,
            &stop,
            Duration::from_millis(1),
            Duration::from_millis(5), // cap the post-trip backoff so the test stays quick
            None,
            move || {
                reconnects_r.fetch_add(1, Ordering::Relaxed);
            },
            Some(Duration::from_millis(20)),
        );

        assert!(result.is_ok(), "a stall is a transport hiccup, never an auth error");
        assert!(
            opens.load(Ordering::Relaxed) >= 2,
            "watchdog must end the dead session and re-open (opens={})",
            opens.load(Ordering::Relaxed)
        );
        assert!(
            reconnects.load(Ordering::Relaxed) >= 1,
            "the re-open must fire on_reconnect — that gen bump is what drives the A3 resync"
        );
    }

    /// `None` (every venue today) = the pre-watchdog pump, byte-identical: the same silent socket
    /// is tolerated indefinitely, no session end, no reconnect. This pins that the new arm cannot
    /// change behavior until a venue explicitly opts in.
    #[test]
    fn no_threshold_tolerates_silence_forever() {
        let stop = Arc::new(AtomicBool::new(false));
        let opens = Arc::new(AtomicUsize::new(0));
        let reconnects = Arc::new(AtomicUsize::new(0));
        let ticks = Arc::new(AtomicUsize::new(0));

        // 40 ticks x 2ms = ~80ms of silence — many times over any threshold the sibling test uses.
        let (opens_o, stop_o, ticks_o) = (opens.clone(), stop.clone(), ticks.clone());
        let open_ws = move || {
            opens_o.fetch_add(1, Ordering::Relaxed);
            OpenOutcome::Ready(StalledStream {
                tick: Duration::from_millis(2),
                ticks: ticks_o.clone(),
                stop_at: Some((40, stop_o.clone())),
            })
        };

        let reconnects_r = reconnects.clone();
        let result = run_user_data_forever_with_idle(
            open_ws,
            |_| vec![],
            |_| true,
            &stop,
            Duration::from_millis(1),
            Duration::from_millis(5),
            None,
            move || {
                reconnects_r.fetch_add(1, Ordering::Relaxed);
            },
            None, // watchdog OFF
        );

        assert!(result.is_ok());
        assert!(ticks.load(Ordering::Relaxed) >= 40, "the stream really did stay silent");
        assert_eq!(opens.load(Ordering::Relaxed), 1, "no threshold ⇒ the session never ends early");
        assert_eq!(reconnects.load(Ordering::Relaxed), 0, "and therefore never reconnects");
    }

    /// A server that pings on a WALL-CLOCK cadence, which is the only cadence a real server has.
    ///
    /// The previous double pinged on every 3rd `recv` — a CALL COUNT — while the pump it feeds
    /// measures elapsed time. That made the two disagree under exactly the condition a fake exists
    /// to model: a scheduling stall stretches the wall-clock gap between pings without advancing the
    /// call count at all, so the pump saw silence the "server" believed it had broken. `hiccup`
    /// injects one such stall deliberately, so the faithfulness is gated rather than assumed.
    ///
    /// `ping_every: None` is the negative control — the same stream with the pings taken away.
    struct PingingStream {
        ticks: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        /// Whole-test deadline, shared across re-opens so a run that reconnects still terminates.
        run_until: Instant,
        ping_every: Option<Duration>,
        last_ping: Instant,
        /// One-shot injected scheduling stall, sprung on the tick below.
        hiccup: Option<Duration>,
        hiccup_at: usize,
    }

    impl UserStream for PingingStream {
        fn recv(&mut self) -> Result<StreamMsg, StreamError> {
            let n = self.ticks.fetch_add(1, Ordering::Relaxed) + 1;
            if Instant::now() >= self.run_until {
                self.stop.store(true, Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_millis(2));
            if n == self.hiccup_at {
                if let Some(h) = self.hiccup.take() {
                    std::thread::sleep(h); // the CI stall, modelled
                }
            }
            // A real server's ping is already queued when we resume from a stall, so a gap that has
            // outrun the cadence yields a ping on the very next read. No DATA ever, only control.
            match self.ping_every {
                Some(every) if self.last_ping.elapsed() >= every => {
                    self.last_ping = Instant::now();
                    Ok(StreamMsg::Ping(vec![]))
                }
                _ => Err(StreamError::Timeout),
            }
        }
        fn pong(&mut self, _payload: Vec<u8>) -> Result<(), StreamError> {
            Ok(())
        }
    }

    /// Threshold for the pair of runs below. 300 ms against a 6 ms ping cadence leaves ~294 ms of
    /// slack, where the old 25 ms threshold left 19 ms — and the stall that broke this test on CI
    /// was ~21 ms. The run is deliberately LONGER than this (see `PING_RUN`), because a threshold
    /// the run cannot reach would make the positive assertion vacuous: it would pass with the pings
    /// deleted. `a_ping_less_stream_is_declared_dead_at_the_same_threshold` is that control.
    const PING_IDLE_THRESHOLD: Duration = Duration::from_millis(300);
    /// Stream time per run — must exceed `PING_IDLE_THRESHOLD` or neither run proves anything.
    const PING_RUN: Duration = Duration::from_millis(500);
    /// Nominal server ping cadence, unchanged from the call-count double it replaces (3 x 2 ms).
    const PING_EVERY: Duration = Duration::from_millis(6);

    /// Control frames are liveness: a server that pings on a cadence SHORTER than the threshold
    /// keeps the session up forever even with zero data. This is the property that makes an
    /// idle-account user-data stream safe to watchdog at all — and it must survive a scheduling
    /// stall, since a stalled reader is not a dead socket.
    #[test]
    fn server_pings_alone_keep_the_session_alive() {
        let stop = Arc::new(AtomicBool::new(false));
        let opens = Arc::new(AtomicUsize::new(0));
        let ticks = Arc::new(AtomicUsize::new(0));
        let run_until = Instant::now() + PING_RUN;

        let (opens_o, stop_o, ticks_o) = (opens.clone(), stop.clone(), ticks.clone());
        let open_ws = move || {
            opens_o.fetch_add(1, Ordering::Relaxed);
            OpenOutcome::Ready(PingingStream {
                ticks: ticks_o.clone(),
                stop: stop_o.clone(),
                run_until,
                ping_every: Some(PING_EVERY),
                last_ping: Instant::now(),
                // A 40 ms stall — twice the ~21 ms one that actually broke this test on CI.
                hiccup: Some(Duration::from_millis(40)),
                hiccup_at: 5,
            })
        };

        let result = run_user_data_forever_with_idle(
            open_ws,
            |_| vec![],
            |_| true,
            &stop,
            Duration::from_millis(1),
            Duration::from_millis(5),
            None,
            || {},
            Some(PING_IDLE_THRESHOLD),
        );

        assert!(result.is_ok());
        assert_eq!(
            opens.load(Ordering::Relaxed),
            1,
            "server pings are inbound frames — they must reset the idle clock, so no reconnect \
             (a stall must not read as a dead socket either)"
        );
        assert!(
            ticks.load(Ordering::Relaxed) >= 50,
            "the stream must really have run the whole window (ticks={})",
            ticks.load(Ordering::Relaxed)
        );
    }

    /// The negative control for the test above, and the reason its threshold may be generous: strip
    /// the pings and NOTHING else, and the watchdog must still declare the socket dead inside the
    /// same run. Without this, raising `PING_IDLE_THRESHOLD` past `PING_RUN` would silently turn its
    /// sibling into a test that passes whether or not pings reset the idle clock.
    #[test]
    fn a_ping_less_stream_is_declared_dead_at_the_same_threshold() {
        let stop = Arc::new(AtomicBool::new(false));
        let opens = Arc::new(AtomicUsize::new(0));
        let ticks = Arc::new(AtomicUsize::new(0));
        let run_until = Instant::now() + PING_RUN;

        let (opens_o, stop_o, ticks_o) = (opens.clone(), stop.clone(), ticks.clone());
        let open_ws = move || {
            // Stop on the SECOND open: by then the threshold has proven it is reachable in-run.
            if opens_o.fetch_add(1, Ordering::Relaxed) + 1 >= 2 {
                stop_o.store(true, Ordering::Relaxed);
            }
            OpenOutcome::Ready(PingingStream {
                ticks: ticks_o.clone(),
                stop: stop_o.clone(),
                run_until, // backstop: if the watchdog never trips, end rather than hang
                ping_every: None,
                last_ping: Instant::now(),
                hiccup: None,
                hiccup_at: 0,
            })
        };

        let result = run_user_data_forever_with_idle(
            open_ws,
            |_| vec![],
            |_| true,
            &stop,
            Duration::from_millis(1),
            Duration::from_millis(5),
            None,
            || {},
            Some(PING_IDLE_THRESHOLD),
        );

        assert!(result.is_ok());
        assert!(
            opens.load(Ordering::Relaxed) >= 2,
            "the threshold must be reachable inside this run, or its sibling proves nothing \
             (opens={})",
            opens.load(Ordering::Relaxed)
        );
    }
}
