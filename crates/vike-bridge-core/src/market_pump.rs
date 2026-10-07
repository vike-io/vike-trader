//! The venue-neutral live **market-feed WS session driver** (dedup A6, wave 3) — the MARKET-data
//! sibling of [`crate::user_data`]'s pump (whose spawn glue #473's `spawn_pump_with_resync`
//! centralized) and of [`crate::depth`]'s book driver (the first proven lifecycle/protocol split).
//!
//! Every venue's public market feed (klines/trades/quotes) shares one LIFECYCLE — connect,
//! subscribe, read frames on a stop-poll read timeout, keep the venue alive with an app-level ping,
//! watch the subscribe handshake ([`crate::sub_ack::SubscribeAck`], net-hardening br7), and
//! reconnect with a stop-aware backoff on any fault — while the PROTOCOL (URL, subscribe frame,
//! frame decode, sink emission) differs per venue. This module owns the lifecycle; the venue crate
//! supplies its protocol as closures: `on_text` (parse + emit — it returns a [`FrameOutcome`] so
//! the driver knows what the frame meant for the handshake) and `on_session_status` (the status-line
//! disclosure — [`SessionStatus::Live`] the FIRST time a session confirms, [`SessionStatus::Error`]
//! when one ends as a fault; that hook took only the error half until the 42-hour the CI box latch, and
//! the type carries the incident). The subscribe frame is replayed VERBATIM at the start of every session, so
//! reconnect == resubscribe by construction (the same rule hyperliquid's crate-local
//! `reconnect_loop` — the template this generalizes — established: per-feed closure state lives
//! OUTSIDE the session and survives a blip).
//!
//! Deliberately no dependency on `vike-data`: the driver never sees a `LiveDataSink` or a
//! `SubscriptionId`. The venue wires its own sink inside `on_text`; the per-subscription
//! stop/join bookkeeping lives in `vike_data::FeedRegistry` (the registry half of A6), which this
//! crate never names. Every real socket is configured through [`configure_ws_stream`] (read
//! timeout + `TCP_NODELAY`, the one #466 pin), so a venue on this driver can no longer hand-roll
//! a drifting copy (audit A5's failure mode).
//!
//! **Behavior pins** (byte-identical to the bybit copy this replaces, the wave-3 proof):
//! - stop flag polled at the top of every read tick; a requested stop ends the session `Ok` and
//!   the feed loop exits without reconnecting.
//! - subscribe-ack watchdog: armed the moment the subscribe is sent
//!   (`SubscribeAck::new(now_ms(), …)`), checked on each read-timeout tick, disarmed by ANY
//!   [`FrameOutcome::Confirm`] (venue ack or first data); the trip message is the exact
//!   `"no subscribe ack/data within {N}s"` string the venues already surface.
//! - reconnect backoff: `(backoff/100ms).max(1)` stop-aware 100 ms ticks — `backoff = 3 s` is the
//!   venues' pre-driver `for _ in 0..30 { sleep(100ms) }` loop verbatim.
//! - optional idle watchdog (`idle_threshold`, hyperliquid's silent-stall shape): no inbound frame
//!   of ANY kind for longer than the threshold ⇒ session error `"no frames within {N}s (silent
//!   stall)"`. `None` (bybit's kline/trades shape) disables it — zero behavior change.
//! - optional app-level keepalive ([`Keepalive`]): sent on a wall-clock cadence checked before
//!   each read (the depth driver's shape). `None` sends nothing.
//!
//! **The three knobs polymarket arrived with** (the last-venue extension):
//! - [`PumpBackoff::Exponential`]: per-consecutive-fault doubling from `initial` to `max`, reset
//!   to `initial` on a successful connect (polymarket's 500 ms → 30 s shape); `Fixed` is the
//!   classic 3 s loop verbatim.
//! - `connect_timeout`: ONE wall-clock bound over the WHOLE TCP phase — name resolution AND every
//!   address it resolves to ([`crate::ws_proxy::connect_ws`]), then `tungstenite::client_tls` — so a
//!   black-holed route or a dead resolver can't pin a feed thread mid-dial past the stop flag;
//!   `None` keeps plain `tungstenite::connect` — NO connect bound at all, so the thread waits out
//!   the OS's own SYN ladder (minutes) with the stop flag already raised.
//! - `on_tick`: a hook run on every transport-ALIVE read-timeout tick (after the ack/idle
//!   watchdogs pass) — the seam polymarket's §B data-freshness judgment rides (`Stale`-behind-a-
//!   live-socket disclosure, which by definition can only be judged on a dataless tick).
//!
//! ⚠ The first and third are still polymarket-only, and were genuinely inert for every other
//! venue. The MIDDLE one never was, and reading it as inert is what left the hole: `connect_timeout`
//! bounds a THREAD, not a protocol, so "inert elsewhere" meant "every other venue's feed thread is
//! unbounded mid-dial". Every [`crate::pump_spec`] `OnDriver` row sets one now — that table's
//! `CONNECT_10S` carries the window and the measurement — and its
//! `every_on_driver_row_bounds_its_dial` refuses a new row that does not. The field stays an
//! `Option` only because [`run_market_session`] and [`run_market_feed_on`] hand connecting to their
//! caller, where it means nothing.
//!
//! **Optional SOCKS5 egress** (spec §0.1): [`connect_market_stream_via`] takes an
//! `Option<&`[`crate::ws_proxy::WsProxy`]`>`, and [`connect_market_stream`] IS that call with
//! `None` — the dial every venue has always done, unchanged. Only polymarket (whose CLOB hosts are
//! geo-blocked) resolves a proxy today; see [`crate::ws_proxy`] for why this shared home could take
//! the extension additively.
//!
//! Converted: **bybit** (kline + trades), **binance/aster** (the `family` kline/trades pumps),
//! **okx**, **hyperliquid** (its crate-local `reconnect_loop` retired), **polymarket** (its
//! `market_feed` run loop — all three knobs). The per-venue knob matrix is DECLARED in
//! [`crate::pump_spec`] (the MarketPumpSpec capability map, playbook step 1) and each on-driver
//! venue's `pump_opts` consumes its row.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::sub_ack::SubscribeAck;
use crate::user_data::{StreamError, StreamMsg, UserStream};
use crate::ws::{TungsteniteStream, WsSocket, configure_ws_stream};
use crate::ws_proxy::{WsProxy, connect_ws};

/// What one decoded TEXT frame meant to the venue's `on_text` — the driver only needs the
/// handshake/fault classification; all data emission happens INSIDE the closure (the venue's
/// protocol, sink included, never crosses into this crate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameOutcome {
    /// A venue subscribe ACK **or** a data frame — either proves the venue accepted the
    /// subscription, so the driver disarms the [`SubscribeAck`] watchdog (idempotent; returning
    /// this on every data frame is the norm).
    Confirm,
    /// Nothing notable (a keepalive pong reply, an unrelated topic) — does NOT confirm the
    /// handshake.
    Ignore,
    /// A venue-attributable fatal frame (e.g. a subscribe REJECT carrying the venue's message) —
    /// ends the session as an error with exactly this message (net-hardening br7: never silently
    /// dropped). The feed loop discloses it and reconnects.
    Fatal(String),
}

/// What one session disclosed about ITSELF — the widened form of what used to be a bare error
/// `&str`, so a RECOVERY has somewhere to be said.
///
/// ⚠ **This type exists because the driver's status disclosure was one-directional, and that cost
/// 42 hours of unreconciled live trading.** [`run_market_feed_on`] already reset the backoff ladder
/// on a successful connect — it KNEW the session was working — while the only thing it ever told
/// the venue was `on_session_error`. So a venue whose healthy string is written once at spawn (the
/// shape every CEX bridge had) latched its last fault forever: one transient blip wrote
/// `"… ws error (reconnecting): …"`, the pump silently redialled and resumed streaming, and nothing
/// rewrote it. `vike_tradehub::reconcile_config`'s `health_from_feed_status` reads that text and
/// SUPPRESSES the venue's reconcile leg, so on the CI box bybit's leg was skipped once a minute for 42
/// hours — 2,516 times, zero gaps — while four sockets stayed ESTABLISHED, ~2,100 events a minute
/// flowed in and the venue's authoritative wallet figure went unread. Only a process restart
/// cleared it. That function's own doc had PREDICTED this in 2026-07, warning that "a pass wrongly
/// suppressed can stay suppressed indefinitely (as observed live)".
///
/// ⚠ **Widened rather than added as an 8th parameter, and both halves of that are deliberate.**
/// Changing an existing parameter's TYPE makes every call site a compile error and `match`
/// exhaustiveness makes the `Live` case a WRITTEN LINE in the diff — a venue cannot pass `||{}` and
/// leave nothing to review. An 8th parameter would also trip `clippy::too_many_arguments`
/// (threshold 7, and `-D warnings` is a merge gate) on both entry points, needing the `#[allow]`
/// [`crate::depth`]'s `run_depth_feed` already carries. The `begin_detach` precedent argues FOR
/// this rather than against it: that one is a TRAIT method, where a required method is a flag day
/// across every implementor and the cost of a miss is teardown LATENCY — here the cost of a miss is
/// this defect reproduced in eleven more bridges.
///
/// ⚠ **The driver does NOT author the text.** `vike-bridge-core` carries no `vike-data`/`vike-model`
/// feed-status vocabulary, the healthy spelling is per-venue, `vike_app_core::ui::status_dot`'s
/// `feed_dot_color` renders it verbatim, and
/// `crates/bridges/binance/src/family/market_feed.rs`'s `HEALTHY_STATUS_PREFIX` classifies a
/// journal LEVEL on it. `vike_app_core::backend::observe_bridge`'s `observing_status` already states the
/// rule this follows: the producer is the side that knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus<'a> {
    /// The venue ACCEPTED this session's subscription and is delivering on it: the FIRST
    /// [`FrameOutcome::Confirm`] of the session (a subscribe ack, or first data). At most ONCE per
    /// session — never per frame.
    Live,
    /// This session ended as a fault, or the connect failed. Exactly the `&str` the old
    /// error-only hook received, verbatim — no venue's fault text changed when this type landed.
    Error(&'a str),
}

/// App-level keepalive: `payload` sent every `every` (wall-clock cadence, checked before each
/// read — the read timeout guarantees it runs at least once per read tick, so it fires both while
/// data flows and while idle).
#[derive(Debug, Clone, Copy)]
pub struct Keepalive<'a> {
    pub payload: &'a str,
    pub every: Duration,
}

/// Reconnect-backoff policy after a session fault (one of the three polymarket knobs). NOT the
/// REST retry ladder — that is [`crate::retry::BackoffPolicy`]; this one paces WS reconnects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpBackoff {
    /// The same delay after EVERY fault — the classic stop-aware 30×100 ms loop (`Fixed(3 s)` is
    /// the crypto venues' pre-driver `for _ in 0..30 { sleep(100ms) }` verbatim).
    Fixed(Duration),
    /// Consecutive-fault doubling: the first fault after a successful connect waits `initial`,
    /// each further consecutive fault doubles the wait up to `max`, and ANY successful connect
    /// resets the ladder to `initial` (polymarket's pre-driver 500 ms → 30 s shape).
    Exponential { initial: Duration, max: Duration },
}

impl PumpBackoff {
    /// The wait for the first fault after a successful connect (also the fixed policy's only wait).
    fn initial(self) -> Duration {
        match self {
            PumpBackoff::Fixed(d) => d,
            PumpBackoff::Exponential { initial, .. } => initial,
        }
    }

    /// The wait following `current` after one MORE consecutive fault.
    fn next(self, current: Duration) -> Duration {
        match self {
            PumpBackoff::Fixed(d) => d,
            PumpBackoff::Exponential { max, .. } => (current * 2).min(max),
        }
    }
}

/// The venue-tunable knobs of one market-feed pump — the config half (the behavior half rides the
/// `on_text`/`on_session_status` closures), mirroring `spawn_pump_with_resync`'s spec-struct style.
#[derive(Debug, Clone, Copy)]
pub struct MarketPumpOpts<'a> {
    /// Sent once at the start of EVERY session (so reconnect == resubscribe). `None` for a venue
    /// whose URL itself carries the subscription (binance-style stream paths).
    pub subscribe: Option<&'a str>,
    /// Optional app-level keepalive (`None` = the venue needs none on this stream).
    pub keepalive: Option<Keepalive<'a>>,
    /// Subscribe-ack watchdog window (net-hardening br7): if neither a venue ack nor first data
    /// ([`FrameOutcome::Confirm`]) arrives within this long of the subscribe, the session errors
    /// with the attributable `"no subscribe ack/data within {N}s"`. `None` disables the watchdog.
    pub ack_timeout: Option<Duration>,
    /// Silent-stall watchdog: no inbound frame of ANY kind (data or control keepalive) for longer
    /// than this ⇒ session error (the transport is dead behind an open socket). `None` disables.
    pub idle_threshold: Option<Duration>,
    /// Socket read timeout — the stop-flag poll cadence (applied via [`configure_ws_stream`],
    /// which also sets `TCP_NODELAY`, the #466 pin).
    pub read_timeout: Duration,
    /// Stop-aware reconnect backoff after a session fault, walked in 100 ms stop-poll ticks
    /// (`(wait/100ms).max(1)` ticks). [`PumpBackoff::Fixed`]`(3 s)` is the venues' classic
    /// 30×100 ms loop; [`PumpBackoff::Exponential`] is polymarket's doubling ladder.
    pub backoff: PumpBackoff,
    /// Bound on the WHOLE TCP phase of
    /// [`run_market_feed`]/[`connect_market_stream`]/[`connect_market_socket`]: `Some` dials via
    /// [`crate::ws_proxy::connect_ws`]'s bounded arm — resolution and every resolved address under
    /// this one window — then `tungstenite::client_tls`, so neither a black-holed route nor a dead
    /// resolver can pin the feed thread past the stop flag for the OS's own (often very long)
    /// connect timeout; `None` keeps plain `tungstenite::connect` (no bound). Ignored by
    /// [`run_market_session`]/[`run_market_feed_on`], whose caller owns connecting — which is
    /// exactly why a venue with a custom connect closure must dial through
    /// [`connect_market_socket`] rather than reaching for `tungstenite::connect`: on that path
    /// nothing else can apply this field for it.
    ///
    /// Every [`crate::pump_spec`] `OnDriver` row sets it (see that table's `CONNECT_10S`), so
    /// `None` here is a TEST/caller-owned-connect shape, not a venue's.
    pub connect_timeout: Option<Duration>,
}

/// A read/write seam over one market-WS session. Production wraps tungstenite (private, via
/// [`run_market_feed`]); scripted tests drive a canned frame queue with zero real-time sleeps.
/// Mirrors [`crate::depth::DepthStream`] one lane over: `read_frame` returns exactly one TEXT
/// frame per call; Ping/Pong/other control frames are auto-handled INSIDE the impl (never
/// surfaced), each stamping the liveness clock so keepalive traffic never false-trips the idle
/// watchdog.
pub trait MarketStream {
    /// One decoded TEXT frame, or a read-timeout tick ([`StreamError::Timeout`] — poll `stop`,
    /// the ack watchdog, the idle watchdog), or a close/fault ([`StreamError::Closed`]).
    fn read_frame(&mut self) -> Result<String, StreamError>;
    /// Send an app-level text frame (the subscribe, or a venue keepalive ping).
    fn send_text(&mut self, s: &str) -> Result<(), StreamError>;
    /// Time since the last inbound frame of ANY kind. The default `ZERO` (never idle) suits
    /// scripted seams that don't model wall-clock time; override to drive the idle path.
    fn since_last_frame(&self) -> Duration {
        Duration::ZERO
    }
}

/// tungstenite [`MarketStream`] over [`TungsteniteStream`] (the same wrap as depth's private
/// stream): a Ping is auto-ponged and looped past; a Close surfaces as `Err(Closed)`; every
/// inbound frame — Text, Ping, or Other — stamps `last_rx` for the idle watchdog.
struct TungsteniteMarketStream {
    stream: TungsteniteStream,
    last_rx: Instant,
}

impl MarketStream for TungsteniteMarketStream {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        loop {
            match self.stream.recv() {
                Ok(StreamMsg::Text(t)) => {
                    self.last_rx = Instant::now();
                    return Ok(t);
                }
                Ok(StreamMsg::Ping(p)) => {
                    self.last_rx = Instant::now();
                    self.stream.pong(p)?;
                }
                Ok(StreamMsg::Other) => self.last_rx = Instant::now(),
                Err(e) => return Err(e),
            }
        }
    }

    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
        self.stream.send_text(s)
    }

    fn since_last_frame(&self) -> Duration {
        self.last_rx.elapsed()
    }
}

/// The message a failed driver-side send/read surfaces — the raw transport text, so the venue's
/// status line reads exactly as it did when the venue owned the socket (`"server closed"`, the
/// tungstenite `Display`, …).
fn stream_error_msg(e: StreamError) -> String {
    match e {
        StreamError::Closed(m) => m,
        StreamError::Timeout => "read timeout".into(), // unreachable for sends
    }
}

/// One WS session over an already-connected `stream`: send the subscribe (arming the ack
/// watchdog), then read frames until a requested stop (`Ok(())`), a transport fault, an ack/idle
/// watchdog trip, or a [`FrameOutcome::Fatal`] frame (all `Err(message)` — the feed loop
/// discloses and reconnects). `now_ms` is injected (production wall-clock; tests a scripted
/// counter) so the ack watchdog is unit-testable with zero sleeps. `on_tick` runs on every
/// transport-ALIVE read-timeout tick — after the stop poll and both watchdogs pass — the seam a
/// venue's dataless-tick judgment (polymarket's §B data-freshness disclosure) rides; venues
/// without one pass `&mut || {}`.
///
/// `on_live` fires ONCE per session, on the FIRST [`FrameOutcome::Confirm`] — see
/// [`SessionStatus::Live`] for why the disclosure is edge-triggered HERE rather than at the
/// caller's `connect()`-Ok. The short version: this is the seam BOTH entry points cross (five
/// production call sites drive this function directly and never touch [`run_market_feed_on`]), and
/// it reuses the ONE definition of "this session is working" the driver already trusts —
/// [`SubscribeAck::confirm`]'s — instead of standing a second, weaker one beside it.
pub fn run_market_session<S: MarketStream>(
    stream: &mut S,
    opts: &MarketPumpOpts<'_>,
    stop: &AtomicBool,
    now_ms: &dyn Fn() -> i64,
    on_text: &mut dyn FnMut(&str) -> FrameOutcome,
    on_tick: &mut dyn FnMut(),
    on_live: &mut dyn FnMut(),
) -> Result<(), String> {
    if let Some(sub) = opts.subscribe {
        stream.send_text(sub).map_err(stream_error_msg)?;
    }
    // br7: arm the subscribe-ack watchdog the moment the subscribe is sent. Clock-free
    // `SubscribeAck`, fed `now_ms()` only here and on each read-timeout poll tick below.
    let mut ack =
        opts.ack_timeout.map(|t| (SubscribeAck::new(now_ms(), t.as_millis() as i64), t.as_secs()));
    let mut last_ka = Instant::now();
    // Edge trigger for `on_live`. One predictable-false bool test per frame after the first, on a
    // venue WS thread — NOT `vike-core`'s fold, so the hot-fold no-logging rule is not engaged.
    // Per SESSION, never per frame: a venue's hook takes a status mutex, and taking it on every
    // kline tick is the cost this flag exists to refuse.
    let mut disclosed_live = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        // App-level keepalive on a wall-clock cadence (checked before each read; the read timeout
        // guarantees this runs at least every read tick) — the depth driver's shape.
        if let Some(ka) = &opts.keepalive
            && last_ka.elapsed() >= ka.every
        {
            stream.send_text(ka.payload).map_err(stream_error_msg)?;
            last_ka = Instant::now();
        }
        match stream.read_frame() {
            Ok(txt) => match on_text(&txt) {
                FrameOutcome::Confirm => {
                    if let Some((a, _)) = &mut ack {
                        a.confirm();
                    }
                    if !disclosed_live {
                        disclosed_live = true;
                        on_live();
                    }
                }
                FrameOutcome::Ignore => {}
                FrameOutcome::Fatal(msg) => return Err(msg),
            },
            // Read-timeout poll tick: the stop flag re-checks at the loop top; here the two
            // watchdogs run — ack first (the venues' pre-driver order), then idle — and only when
            // both pass (the transport is judged alive) does the venue's `on_tick` hook fire.
            Err(StreamError::Timeout) => {
                if let Some((a, secs)) = &ack
                    && a.overdue(now_ms())
                {
                    return Err(format!("no subscribe ack/data within {secs}s"));
                }
                if let Some(idle) = opts.idle_threshold
                    && stream.since_last_frame() >= idle
                {
                    return Err(format!("no frames within {}s (silent stall)", idle.as_secs()));
                }
                on_tick();
            }
            Err(StreamError::Closed(m)) => return Err(m),
        }
    }
}

/// The generic reconnect loop over `connect` + [`run_market_session`] — the testable core of
/// [`run_market_feed`]. A session fault (or a failed connect) is disclosed as
/// [`SessionStatus::Error`], then the stop-aware backoff runs and the next session reconnects +
/// resubscribes; a clean stop ends the loop. The SAME `on_text`/`on_tick` closures are reused
/// across reconnects, so any per-feed state they hold survives a blip (hyperliquid's
/// rollover-cursor rule; polymarket's book/health state).
///
/// ⚠ **A successful connect does TWO things, and this doc used to name only the first.** It resets
/// the [`PumpBackoff::Exponential`] ladder to `initial` (a no-op for `Fixed`), and the session it
/// starts discloses [`SessionStatus::Live`] on its first confirmed frame. The missing half was the
/// defect: the driver knew the session was healthy — it acted on that knowledge every time it reset
/// the ladder — and told the venue only about faults, which is how a status string could latch an
/// error for 42 hours behind a perfectly live socket. See [`SessionStatus`].
///
/// The two disclosures are SEQUENTIAL borrows of `on_session_status` (the session returns before
/// the `match` runs), so no `RefCell` shim is needed on the venue's side.
///
/// A clean stop discloses NOTHING: the `Ok(()) => break` arm stays silent, the same rule
/// [`crate::depth`]'s `DepthFaultLog` states and for the same reason — a requested teardown is not
/// a fault and not a recovery.
pub fn run_market_feed_on<S: MarketStream>(
    mut connect: impl FnMut() -> Result<S, String>,
    opts: &MarketPumpOpts<'_>,
    stop: &AtomicBool,
    now_ms: &dyn Fn() -> i64,
    mut on_text: impl FnMut(&str) -> FrameOutcome,
    mut on_tick: impl FnMut(),
    mut on_session_status: impl FnMut(SessionStatus<'_>),
) {
    let mut backoff = opts.backoff.initial();
    while !stop.load(Ordering::Relaxed) {
        let res = match connect() {
            Ok(mut stream) => {
                backoff = opts.backoff.initial(); // a successful connect resets the doubling ladder
                run_market_session(
                    &mut stream,
                    opts,
                    stop,
                    now_ms,
                    &mut on_text,
                    &mut on_tick,
                    &mut || on_session_status(SessionStatus::Live),
                )
            }
            Err(e) => Err(e), // connect failed — same disclosure + backoff path as a session fault
        };
        match res {
            Ok(()) => break, // requested stop
            Err(e) => on_session_status(SessionStatus::Error(&e)),
        }
        // Stop-aware reconnect backoff: 100 ms stop-poll ticks (3 s = the classic 30×100 ms loop;
        // an exponential policy doubles the NEXT wait after each consecutive fault).
        let steps = (backoff.as_millis() / 100).max(1);
        for _ in 0..steps {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        backoff = opts.backoff.next(backoff);
    }
}

/// Dial `url` and wrap the socket as a production [`MarketStream`] (read timeout + `TCP_NODELAY`
/// via [`configure_ws_stream`], the #466 pin). With `connect_timeout: Some`, the whole TCP phase —
/// resolution and every resolved address — is bounded by [`crate::ws_proxy::connect_ws`]'s one
/// window and the TLS + WS handshake rides
/// `tungstenite::client_tls` (the same rustls backend plain `connect` uses) — plain
/// `tungstenite::connect` has NO connect bound at all, so a black-holed route would otherwise pin
/// the thread until the OS's own TCP timeout, ignoring the stop flag. Public so a venue with a
/// custom connect closure (polymarket wraps this in its raw-frame `TappedStream`) dials the same
/// way [`run_market_feed`] does.
///
/// Exactly [`connect_market_stream_via`] with **no proxy** — the dial every venue on this driver
/// has always done, unchanged.
pub fn connect_market_stream(
    url: &str,
    read_timeout: Duration,
    connect_timeout: Option<Duration>,
) -> Result<impl MarketStream, String> {
    connect_market_stream_via(url, read_timeout, connect_timeout, None)
}

/// The dialed-and-configured RAW socket [`connect_market_stream_via`] wraps — the SAME dial, one
/// layer lower, for a venue whose startup needs the `WsSocket` itself before the driver takes over.
///
/// `crates/bridges/binance/src/family/trades.rs`'s `connect_trades_ws` is the reason this exists:
/// that lane must read frames off the raw socket for its bounded post-warmup drain, so it cannot
/// take `connect_market_stream`'s wrapped return — and because there was no lower rung to call, it
/// hand-rolled `tungstenite::connect` instead and ignored its own `MarketPumpSpec` row's
/// `connect_timeout` entirely. A venue that has to re-spell a dial will eventually spell it
/// differently; this is the rung that means it does not have to.
///
/// Every bound, pin and proxy arm is [`crate::ws_proxy::connect_ws`]'s, verbatim — there is exactly
/// one dial in this module and both entry points are it.
pub fn connect_market_socket(
    url: &str,
    read_timeout: Duration,
    connect_timeout: Option<Duration>,
    proxy: Option<&WsProxy>,
) -> Result<WsSocket, String> {
    let socket = connect_ws(url, proxy, connect_timeout)?;
    configure_ws_stream(&socket, read_timeout);
    Ok(socket)
}

/// [`connect_market_stream`] with an OPTIONAL SOCKS5 egress ([`crate::ws_proxy`], spec §0.1).
///
/// `proxy: None` is the pre-existing dial verbatim — this is a SHARED HOME (`market_pump` serves
/// every venue), so the extension is additive and inert by construction: only a venue that
/// explicitly resolves a [`WsProxy`] (today: polymarket, whose CLOB market/user/RTDS hosts are
/// geo-blocked) takes the tunnelled arm. See [`crate::ws_proxy::connect_ws`] for the three arms
/// and the documented handshake-timeout residual.
pub fn connect_market_stream_via(
    url: &str,
    read_timeout: Duration,
    connect_timeout: Option<Duration>,
    proxy: Option<&WsProxy>,
) -> Result<impl MarketStream, String> {
    let socket = connect_market_socket(url, read_timeout, connect_timeout, proxy)?;
    Ok(TungsteniteMarketStream { stream: TungsteniteStream(socket), last_rx: Instant::now() })
}

/// Connect `url` (via [`connect_market_stream`] — `opts.connect_timeout` bounds the dial when set)
/// and run the reconnect loop — the whole thread body a venue's market-feed spawn runs. See
/// [`run_market_feed_on`] for the loop semantics and [`MarketPumpOpts`] for the knobs.
pub fn run_market_feed(
    url: &str,
    opts: &MarketPumpOpts<'_>,
    stop: &AtomicBool,
    now_ms: &dyn Fn() -> i64,
    on_text: impl FnMut(&str) -> FrameOutcome,
    on_tick: impl FnMut(),
    on_session_status: impl FnMut(SessionStatus<'_>),
) {
    run_market_feed_on(
        || connect_market_stream(url, opts.read_timeout, opts.connect_timeout),
        opts,
        stop,
        now_ms,
        on_text,
        on_tick,
        on_session_status,
    )
}

#[path = "market_pump_tests.rs"]
#[cfg(test)]
mod market_pump_tests;
