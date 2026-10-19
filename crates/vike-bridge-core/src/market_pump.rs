//! The venue-neutral live **market-feed WS session driver** — the market-data sibling of
//! [`crate::user_data`]'s pump and of [`crate::depth`]'s book driver.
//!
//! Every venue's public market feed (klines/trades/quotes) shares one LIFECYCLE — connect,
//! subscribe, read frames on a stop-poll read timeout, keep the venue alive with an app-level ping,
//! watch the subscribe handshake ([`crate::sub_ack::SubscribeAck`]), and reconnect with a
//! stop-aware backoff on any fault — while the PROTOCOL (URL, subscribe frame, frame decode, sink
//! emission) differs per venue. This module owns the lifecycle; the venue supplies its protocol as
//! closures: `on_text` (parse + emit; its [`FrameOutcome`] tells the driver what the frame meant
//! for the handshake) and `on_session_status` ([`SessionStatus::Live`] the FIRST time a session
//! confirms, [`SessionStatus::Error`] when one ends as a fault). The subscribe frame is replayed
//! VERBATIM at the start of every session, so reconnect == resubscribe by construction, and
//! per-feed closure state lives OUTSIDE the session and survives a blip.
//!
//! Deliberately no dependency on `vike-data`: the driver never sees a `LiveDataSink` or a
//! `SubscriptionId`. The venue wires its own sink inside `on_text`; per-subscription stop/join
//! bookkeeping lives in `vike_data::FeedRegistry`. Every real socket is configured through
//! [`configure_ws_stream`] (read timeout + `TCP_NODELAY`), so a venue on this driver cannot
//! hand-roll a drifting copy.
//!
//! **Behavior pins:**
//! - stop flag polled at the top of every read tick; a requested stop ends the session `Ok` and
//!   the feed loop exits without reconnecting.
//! - subscribe-ack watchdog: armed the moment the subscribe is sent
//!   (`SubscribeAck::new(now_ms(), …)`), checked on each read-timeout tick, disarmed by ANY
//!   [`FrameOutcome::Confirm`] (venue ack or first data); the trip message is the exact
//!   `"no subscribe ack/data within {N}s"` string the venues surface.
//! - reconnect backoff: `(backoff/100ms).max(1)` stop-aware 100 ms ticks.
//! - optional idle watchdog (`idle_threshold`): no inbound frame of ANY kind for longer than the
//!   threshold ⇒ session error `"no frames within {N}s (silent stall)"`. `None` disables it.
//! - optional app-level keepalive ([`Keepalive`]): sent on a wall-clock cadence checked before
//!   each read (the depth driver's shape). `None` sends nothing.
//! - [`PumpBackoff::Exponential`]: per-consecutive-fault doubling from `initial` to `max`, reset to
//!   `initial` on a successful connect; `Fixed` waits the same after every fault.
//! - `connect_timeout`: ONE wall-clock bound over the WHOLE TCP phase — name resolution AND every
//!   address it resolves to ([`crate::ws_proxy::connect_ws`]), then `tungstenite::client_tls` — so a
//!   black-holed route or a dead resolver can't pin a feed thread mid-dial past the stop flag;
//!   `None` keeps plain `tungstenite::connect` — NO connect bound at all, so the thread waits out
//!   the OS's own SYN ladder (minutes) with the stop flag already raised.
//! - `on_tick`: a hook run on every transport-ALIVE read-timeout tick (after the ack/idle
//!   watchdogs pass) — the seam a venue's data-freshness judgment rides (`Stale` behind a live
//!   socket can only be judged on a dataless tick).
//!
//! ⚠ `connect_timeout` bounds a THREAD, not a protocol, so it is never "inert" for a venue: every
//! [`crate::pump_spec`] `OnDriver` row sets one (that table's `CONNECT_10S` carries the window and
//! the measurement) and its `every_on_driver_row_bounds_its_dial` refuses a row that does not. The
//! field stays an `Option` only because [`run_market_session`] and [`run_market_feed_on`] hand
//! connecting to their caller, where it means nothing.
//!
//! **Optional SOCKS5 egress:** [`connect_market_stream_via`] takes an
//! `Option<&`[`crate::ws_proxy::WsProxy`]`>`, and [`connect_market_stream`] IS that call with
//! `None`; see [`crate::ws_proxy`] for why this shared home could take the extension additively.
//!
//! Which venues ride this driver, and with which knobs, is [`crate::pump_spec`]'s table; each
//! on-driver venue's opts consume its row.

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
    /// ends the session as an error with exactly this message (never silently dropped). The feed
    /// loop discloses it and reconnects.
    Fatal(String),
}

/// What one session disclosed about ITSELF — a RECOVERY as well as a fault.
///
/// ⚠ **A fault-only disclosure latches.** A venue whose healthy status string is written once at
/// spawn keeps its last fault text forever if the driver reports only errors: the pump redials and
/// resumes streaming, and nothing rewrites it. `vike_tradehub::reconcile_config`'s
/// `health_from_feed_status` reads that text and SUPPRESSES the venue's reconcile leg,
/// indefinitely. `crates/vike-ops/tests/venues/feed_success_disclosure_gate.rs` records the
/// incident and holds every call site to a non-empty `Live` arm.
///
/// ⚠ **It is the TYPE of the existing status hook, not an extra `on_live` parameter, on purpose:**
/// `match` exhaustiveness makes the `Live` case a WRITTEN LINE at every call site, where a separate
/// closure could be passed as `|| {}` and leave nothing to review (and an 8th parameter would trip
/// `clippy::too_many_arguments` on both entry points).
///
/// ⚠ **The driver does NOT author the text.** The healthy spelling is per-venue, the GUI renders it
/// verbatim, and `crates/bridges/binance/src/family/market_feed.rs`'s `HEALTHY_STATUS_PREFIX`
/// classifies a journal LEVEL on it: the producer is the side that knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus<'a> {
    /// The venue ACCEPTED this session's subscription and is delivering on it: the FIRST
    /// [`FrameOutcome::Confirm`] of the session (a subscribe ack, or first data). At most ONCE per
    /// session — never per frame.
    Live,
    /// This session ended as a fault, or the connect failed — the fault's own text, verbatim.
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

/// Reconnect-backoff policy after a session fault. NOT the REST retry ladder — that is
/// [`crate::retry::BackoffPolicy`]; this one paces WS reconnects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpBackoff {
    /// The same delay after EVERY fault, walked in stop-aware 100 ms ticks (`Fixed(3 s)` = 30
    /// ticks).
    Fixed(Duration),
    /// Consecutive-fault doubling: the first fault after a successful connect waits `initial`,
    /// each further consecutive fault doubles the wait up to `max`, and ANY successful connect
    /// resets the ladder to `initial` (polymarket's 500 ms → 30 s row).
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
/// `on_text`/`on_session_status` closures).
#[derive(Debug, Clone, Copy)]
pub struct MarketPumpOpts<'a> {
    /// Sent once at the start of EVERY session (so reconnect == resubscribe). `None` for a venue
    /// whose URL itself carries the subscription (binance-style stream paths).
    pub subscribe: Option<&'a str>,
    /// Optional app-level keepalive (`None` = the venue needs none on this stream).
    pub keepalive: Option<Keepalive<'a>>,
    /// Subscribe-ack watchdog window: if neither a venue ack nor first data
    /// ([`FrameOutcome::Confirm`]) arrives within this long of the subscribe, the session errors
    /// with the attributable `"no subscribe ack/data within {N}s"`. `None` disables the watchdog.
    pub ack_timeout: Option<Duration>,
    /// Silent-stall watchdog: no inbound frame of ANY kind (data or control keepalive) for longer
    /// than this ⇒ session error (the transport is dead behind an open socket). `None` disables.
    pub idle_threshold: Option<Duration>,
    /// Socket read timeout — the stop-flag poll cadence (applied via [`configure_ws_stream`],
    /// which also sets `TCP_NODELAY`).
    pub read_timeout: Duration,
    /// Stop-aware reconnect backoff after a session fault, walked in 100 ms stop-poll ticks
    /// (`(wait/100ms).max(1)` ticks).
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

/// The message a failed driver-side send/read surfaces — the raw transport text (`"server
/// closed"`, the tungstenite `Display`, …).
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
/// venue's dataless-tick judgment (polymarket's data-freshness disclosure) rides; venues without
/// one pass `&mut || {}`.
///
/// `on_live` fires ONCE per session, on the FIRST [`FrameOutcome::Confirm`] — edge-triggered HERE
/// rather than at the caller's `connect()`-Ok, because this is the seam BOTH entry points cross
/// (some production call sites drive this function directly, never [`run_market_feed_on`]), and it
/// reuses the ONE definition of "this session is working" the driver trusts —
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
    // Arm the subscribe-ack watchdog the moment the subscribe is sent. Clock-free
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
            // watchdogs run — ack first, then idle — and only when both pass (the transport is
            // judged alive) does the venue's `on_tick` hook fire.
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
/// ⚠ **A successful connect does TWO things.** It resets the [`PumpBackoff::Exponential`] ladder
/// to `initial` (a no-op for `Fixed`), and the session it starts discloses [`SessionStatus::Live`]
/// on its first confirmed frame. Without the second, a venue's status string latches its last
/// error behind a perfectly live socket — see [`SessionStatus`].
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
        // Stop-aware reconnect backoff in 100 ms stop-poll ticks; an exponential policy doubles
        // the NEXT wait after each consecutive fault.
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
/// via [`configure_ws_stream`]). With `connect_timeout: Some`, the whole TCP phase —
/// resolution and every resolved address — is bounded by [`crate::ws_proxy::connect_ws`]'s one
/// window and the TLS + WS handshake rides
/// `tungstenite::client_tls` (the same rustls backend plain `connect` uses) — plain
/// `tungstenite::connect` has NO connect bound at all, so a black-holed route would otherwise pin
/// the thread until the OS's own TCP timeout, ignoring the stop flag. Public so a venue with a
/// custom connect closure (polymarket wraps this in its raw-frame `TappedStream`) dials the same
/// way [`run_market_feed`] does.
///
/// Exactly [`connect_market_stream_via`] with **no proxy**.
pub fn connect_market_stream(
    url: &str,
    read_timeout: Duration,
    connect_timeout: Option<Duration>,
) -> Result<impl MarketStream, String> {
    connect_market_stream_via(url, read_timeout, connect_timeout, None)
}

/// The dialed-and-configured RAW socket [`connect_market_stream_via`] wraps — the SAME dial, one
/// layer lower, for a venue whose startup needs the `WsSocket` itself before the driver takes over
/// (`crates/bridges/binance/src/family/trades.rs`'s `connect_trades_ws`, whose post-warmup drain
/// reads the raw socket). Such a venue dials HERE rather than re-spelling `tungstenite::connect`,
/// which applies no bound and ignores its `MarketPumpSpec` row's `connect_timeout`.
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

/// [`connect_market_stream`] with an OPTIONAL SOCKS5 egress ([`crate::ws_proxy`]).
///
/// `proxy: None` is the plain dial; only a venue that explicitly resolves a [`WsProxy`]
/// (polymarket, whose CLOB market/user/RTDS hosts are geo-blocked) takes the tunnelled arm. See
/// [`crate::ws_proxy::connect_ws`] for the three arms and the documented handshake-timeout
/// residual.
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
