//! The venue-neutral live **market-feed WS session driver** — the market-data sibling of
//! [`crate::websocket_session::user`]'s pump and of [`crate::depth`]'s book driver.
//!
//! Every venue's public market feed (klines/trades/quotes) shares one LIFECYCLE — connect,
//! subscribe, read frames on a stop-poll read timeout, keep the venue alive with an app-level ping,
//! watch the subscribe handshake ([`crate::sub_ack::WebsocketSessionSubscribeAck`]), and reconnect with a
//! stop-aware backoff on any fault — while the PROTOCOL (URL, subscribe frame, frame decode, sink
//! emission) differs per venue. This module owns the lifecycle; the venue supplies its protocol as
//! closures: `on_text` (parse + emit; its [`WebsocketSessionFrameOutcome`] tells the driver what the frame meant
//! for the handshake) and `on_session_status` ([`WebsocketSessionStatus::Live`] the FIRST time a session
//! confirms, [`WebsocketSessionStatus::Error`] when one ends as a fault, [`WebsocketSessionStatus::Announced`] when
//! the venue announced its end). The subscribe frame is replayed
//! VERBATIM at the start of every session, so reconnect == resubscribe by construction, and
//! per-feed closure state lives OUTSIDE the session and survives a blip.
//!
//! Deliberately no dependency on `vike-data`: the driver never sees a `DataClientSink` or a
//! `DataClientSubscriptionId`. The venue wires its own sink inside `on_text`; per-subscription stop/join
//! bookkeeping lives in `vike_data::DataClientRegistry`. Every real socket is configured through
//! [`configure_websocket_stream`] (read timeout + `TCP_NODELAY`), so a venue on this driver cannot
//! hand-roll a drifting copy.
//!
//! **Behavior pins:**
//! - stop flag polled at the top of every read tick; a requested stop ends the session `Ok` and
//!   the feed loop exits without reconnecting.
//! - subscribe-ack watchdog: armed the moment the subscribe is sent
//!   (`WebsocketSessionSubscribeAck::new(now_ms(), …)`), checked on each read-timeout tick, disarmed by ANY
//!   [`WebsocketSessionFrameOutcome::Confirm`] (venue ack or first data); the trip message is the exact
//!   `"no subscribe ack/data within {N}s"` string the venues surface.
//! - reconnect backoff: `(backoff/100ms).max(1)` stop-aware 100 ms ticks.
//! - the keepalive cadence and those ticks read [`crate::pump_clock`] (the wall clock in every shipped thread), so
//!   a harness runs a row's real keepalive and reconnect waits on a virtual clock.
//! - optional idle watchdog (`idle_threshold`): no inbound frame of ANY kind for longer than the
//!   threshold ⇒ session error `"no frames within {N}s (silent stall)"`. `None` disables it.
//! - optional app-level keepalive ([`WebsocketSessionMarketKeepalive`]): sent on the pump clock's cadence, checked
//!   before each read (the depth driver's shape). `None` sends nothing.
//! - the reconnect ladder is the lane's `vike_venues::venue_timings` row ([`VenueTimingsReconnect`]): per-consecutive-
//!   fault doubling from `initial` to `max`, reset to `initial` on a successful connect, and a fixed wait where the two
//!   are equal. It runs on the one reconnect ladder, [`ReconnectBackoff`] (`crates/vike-bridge-core/src/websocket_session/market_tests.rs`'s
//!   `the_market_pumps_reconnect_waits_are_pinned_verbatim`).
//! - `connect_timeout`: ONE wall-clock bound over the WHOLE TCP phase — name resolution AND every
//!   address it resolves to ([`vike_transport::websocket_proxy::connect_websocket`]), then `tungstenite::client_tls` with each
//!   read of the handshake under the same window — so a black-holed route, a dead resolver or a peer
//!   that accepts and goes silent can't pin a feed thread mid-dial past the stop flag;
//!   `None` keeps plain `tungstenite::connect` — NO connect bound at all, so the thread waits out
//!   the OS's own SYN ladder (minutes) with the stop flag already raised.
//! - `on_tick`: a hook run on every transport-ALIVE read-timeout tick (after the ack/idle
//!   watchdogs pass) — the seam a venue's data-freshness judgment rides (`Stale` behind a live
//!   socket can only be judged on a dataless tick).
//! - optional notice hook (`notice`, [`WebsocketSessionMarketNotice`]): **the market pump's ONE planned reconnect**,
//!   the twin of [`crate::websocket_session::user`]'s `run_user_with_notice`. The hook reads every text frame before
//!   `on_text`; a frame it answers ends the session in its words, disclosed as
//!   [`WebsocketSessionStatus::Announced`] (the producer writes `Connecting`, never `Error`), said at `info`, and the
//!   next dial comes at once after a session past the ladder's reset and on the ladder after a younger one
//!   ([`ReconnectBackoff::announced`]). No venue is named here: each bridge hands its own hook (binance spot's
//!   `serverShutdown`, okx's `64008`), and `None` reads no frame for it.
//!
//! ⚠ `connect_timeout` bounds a THREAD, not a protocol, so it is never "inert" for a venue: every
//! [`crate::websocket_session::spec`] row sets one (that module's `CONNECT_10S` carries the window and
//! the measurement) and its `every_on_driver_row_bounds_its_dial` refuses a row that does not. The
//! field stays an `Option` only because [`run_market_session`] and [`run_market_on`] hand
//! connecting to their caller, where it means nothing.
//!
//! **Optional SOCKS5 egress:** [`connect_market_stream_via`] takes an
//! `Option<&`[`vike_transport::websocket_proxy::WebsocketProxy`]`>`, and [`connect_market_stream`] IS that call with
//! `None`; see [`vike_transport::websocket_proxy`] for why this shared home could take the extension additively.
//!
//! Which venues ride this driver is `vike_venues::venue_connection`'s market `driver`, and their knobs are
//! [`crate::websocket_session::spec`]'s `row`, read from the venue tables; each on-driver venue's opts consume it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::pump_clock;
use crate::sub_ack::WebsocketSessionSubscribeAck;
use crate::websocket::WebsocketSessionUserStreamTungstenite;
use crate::websocket_session::user::{
    WebsocketSessionUserError, WebsocketSessionUserMessage, WebsocketSessionUserStream,
};
use vike_transport::websocket::{WsSocket, configure_websocket_stream};
use vike_transport::websocket_proxy::{WebsocketProxy, connect_websocket};
use vike_venues::reconnect::ReconnectBackoff;
use vike_venues::venue_timings::VenueTimingsReconnect;

/// What one decoded TEXT frame meant to the venue's `on_text` — the driver only needs the
/// handshake/fault classification; all data emission happens INSIDE the closure (the venue's
/// protocol, sink included, never crosses into this crate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebsocketSessionFrameOutcome {
    /// A venue subscribe ACK **or** a data frame — either proves the venue accepted the
    /// subscription, so the driver disarms the [`WebsocketSessionSubscribeAck`] watchdog (idempotent; returning
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
/// ⚠ **A fault-only disclosure latches.** A venue whose healthy status is written once at spawn
/// keeps its last fault forever if the driver reports only errors: the pump redials and resumes
/// streaming, and nothing rewrites it. `vike_tradehub::reconcile_config`'s
/// `health_from_feed_status` reads that status and SUPPRESSES the venue's reconcile leg,
/// indefinitely. `crates/vike-ops/tests/venues/feed_success_disclosure_gate.rs` records the
/// incident and holds every call site to a non-empty `Live` arm.
///
/// ⚠ **It is the TYPE of the existing status hook, not an extra `on_live` parameter, on purpose:**
/// `match` exhaustiveness makes the `Live` case a WRITTEN LINE at every call site, where a separate
/// closure could be passed as `|| {}` and leave nothing to review (and an 8th parameter would trip
/// `clippy::too_many_arguments` on both entry points).
///
/// ⚠ **The driver does NOT author the status.** The words are per-venue and the GUI renders them
/// verbatim, and the STATE beside them is the producer's too (a venue's planned resubscribe ends a
/// session through `Error` and is still `Connecting`): `crates/vike-bridge-core/src/feed_ctx.rs`'s
/// `set_status` picks the journal LEVEL from that state. The producer is the side that knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebsocketSessionStatus<'a> {
    /// The venue ACCEPTED this session's subscription and is delivering on it: the FIRST
    /// [`WebsocketSessionFrameOutcome::Confirm`] of the session (a subscribe ack, or first data). At most ONCE per
    /// session — never per frame.
    Live,
    /// This session ended as a fault, or the connect failed — the fault's own text, verbatim.
    Error(&'a str),
    /// The venue announced this session's end (the options' `notice` hook answered a frame), in the hook's words: a
    /// planned reopen, which the producer writes as `Connecting`, never `Error`.
    Announced(&'a str),
}

/// A venue's notice hook ([`WebsocketSessionMarketOptions::notice`]): a text frame by which the venue says it is about
/// to end the session (`vike_venues::venue_connection`'s market `Handled` notice) to the words the pump reports it in;
/// `None` for every other frame. It reads the raw text because it runs on every frame of a market lane: a bridge
/// screens for its notice's literal before it parses anything.
pub type WebsocketSessionMarketNotice = fn(&str) -> Option<String>;

/// App-level keepalive: `payload` sent every `every` (the pump clock's cadence, checked before each
/// read — the read timeout guarantees it runs at least once per read tick, so it fires both while
/// data flows and while idle).
#[derive(Debug, Clone, Copy)]
pub struct WebsocketSessionMarketKeepalive<'a> {
    pub payload: &'a str,
    pub every: Duration,
}

/// The venue-tunable knobs of one market-feed pump — the config half (the behavior half rides the
/// `on_text`/`on_session_status` closures).
#[derive(Debug, Clone, Copy)]
pub struct WebsocketSessionMarketOptions<'a> {
    /// Sent once at the start of EVERY session (so reconnect == resubscribe). `None` for a venue
    /// whose URL itself carries the subscription (binance-style stream paths).
    pub subscribe: Option<&'a str>,
    /// Optional app-level keepalive (`None` = the venue needs none on this stream).
    pub keepalive: Option<WebsocketSessionMarketKeepalive<'a>>,
    /// Subscribe-ack watchdog window: if neither a venue ack nor first data
    /// ([`WebsocketSessionFrameOutcome::Confirm`]) arrives within this long of the subscribe, the session errors
    /// with the attributable `"no subscribe ack/data within {N}s"`. `None` disables the watchdog.
    pub ack_timeout: Option<Duration>,
    /// Silent-stall watchdog: no inbound frame of ANY kind (data or control keepalive) for longer
    /// than this ⇒ session error (the transport is dead behind an open socket). `None` disables.
    pub idle_threshold: Option<Duration>,
    /// Socket read timeout — the stop-flag poll cadence (applied via [`configure_websocket_stream`],
    /// which also sets `TCP_NODELAY`).
    pub read_timeout: Duration,
    /// Stop-aware reconnect backoff after a session fault, walked in 100 ms stop-poll ticks
    /// (`(wait/100ms).max(1)` ticks): the lane's reconnect row, run on [`ReconnectBackoff::from_row`]'s ladder (a fixed
    /// wait where `initial == max`; doubling per consecutive fault and reset by a connect that worked otherwise,
    /// polymarket's 500 ms → 30 s). NOT the REST retry ladder — that is [`crate::retry::RetryBackoffPolicy`].
    pub backoff: VenueTimingsReconnect,
    /// Bound on the WHOLE TCP phase of
    /// [`run_market`]/[`connect_market_stream`]/[`connect_market_socket`]: `Some` dials via
    /// [`vike_transport::websocket_proxy::connect_websocket`]'s bounded arm — resolution and every resolved address under
    /// this one window — then `tungstenite::client_tls`, so neither a black-holed route nor a dead
    /// resolver can pin the feed thread past the stop flag for the OS's own (often very long)
    /// connect timeout; `None` keeps plain `tungstenite::connect` (no bound). Ignored by
    /// [`run_market_session`]/[`run_market_on`], whose caller owns connecting — which is
    /// exactly why a venue with a custom connect closure must dial through
    /// [`connect_market_socket`] rather than reaching for `tungstenite::connect`: on that path
    /// nothing else can apply this field for it.
    ///
    /// Every [`crate::websocket_session::spec`] row sets it (see that module's `CONNECT_10S`), so
    /// `None` here is a TEST/caller-owned-connect shape, not a venue's.
    pub connect_timeout: Option<Duration>,
    /// The venue's notice hook (the module doc's planned reconnect); `None` where the bridge reads none. A bridge's
    /// fact, never the tables': [`crate::websocket_session::spec::WebsocketSessionKnobs::opts`] leaves it `None`.
    pub notice: Option<WebsocketSessionMarketNotice>,
}

/// A read/write seam over one market-WS session. Production wraps tungstenite
/// ([`WebsocketSessionMarketStreamTungstenite`], via [`run_market`]), or reads an HTTP body as lines
/// ([`crate::websocket_session::user::line_stream::LineStream`], dialled by the lane's own `connect`); scripted tests
/// drive a canned frame queue with zero real-time sleeps.
/// Mirrors [`crate::depth::WebsocketSessionDepthStream`] one lane over: `read_frame` returns exactly one TEXT
/// frame per call; Ping/Pong/other control frames are auto-handled INSIDE the impl (never
/// surfaced), each stamping the liveness clock so keepalive traffic never false-trips the idle
/// watchdog.
pub trait WebsocketSessionMarketStream {
    /// One decoded TEXT frame, or a read-timeout tick ([`WebsocketSessionUserError::Timeout`] — poll `stop`,
    /// the ack watchdog, the idle watchdog), or a close/fault ([`WebsocketSessionUserError::Closed`]).
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError>;
    /// Send an app-level text frame (the subscribe, or a venue keepalive ping).
    fn send_text(&mut self, s: &str) -> Result<(), WebsocketSessionUserError>;
    /// Time since the last inbound frame of ANY kind. The default `ZERO` (never idle) suits
    /// scripted seams that don't model wall-clock time; override to drive the idle path.
    fn since_last_frame(&self) -> Duration {
        Duration::ZERO
    }
}

/// A boxed stream is the stream it holds, every method forwarded (the idle clock included): what a
/// venue that dials for itself hands the data engine (`crate::data_client::ws`'s `SessionOn` plan).
impl<M: WebsocketSessionMarketStream + ?Sized> WebsocketSessionMarketStream for Box<M> {
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        (**self).read_frame()
    }

    fn send_text(&mut self, s: &str) -> Result<(), WebsocketSessionUserError> {
        (**self).send_text(s)
    }

    fn since_last_frame(&self) -> Duration {
        (**self).since_last_frame()
    }
}

/// tungstenite [`WebsocketSessionMarketStream`] over [`WebsocketSessionUserStreamTungstenite`]: a Ping is auto-ponged and looped past;
/// a Close surfaces as `Err(Closed)`; every inbound frame — Text, Ping, or Other — stamps `last_rx`
/// for the idle watchdog. The ONE stamped stream: this driver's, [`crate::depth`]'s (its
/// [`crate::depth::WebsocketSessionDepthStream`] impl) and ig's Lightstreamer session (`vike_ig::feed`).
pub struct WebsocketSessionMarketStreamTungstenite {
    stream: WebsocketSessionUserStreamTungstenite,
    last_rx: Instant,
}

impl WebsocketSessionMarketStreamTungstenite {
    /// Wrap a dialled, configured socket; the liveness clock starts now.
    pub fn new(stream: WebsocketSessionUserStreamTungstenite) -> Self {
        WebsocketSessionMarketStreamTungstenite { stream, last_rx: Instant::now() }
    }
}

impl WebsocketSessionMarketStream for WebsocketSessionMarketStreamTungstenite {
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        loop {
            match self.stream.recv() {
                Ok(WebsocketSessionUserMessage::Text(t)) => {
                    self.last_rx = Instant::now();
                    return Ok(t);
                }
                Ok(WebsocketSessionUserMessage::Ping(p)) => {
                    self.last_rx = Instant::now();
                    self.stream.pong(p)?;
                }
                Ok(WebsocketSessionUserMessage::Other) => self.last_rx = Instant::now(),
                Err(e) => return Err(e),
            }
        }
    }

    fn send_text(&mut self, s: &str) -> Result<(), WebsocketSessionUserError> {
        self.stream.send_text(s)
    }

    fn since_last_frame(&self) -> Duration {
        self.last_rx.elapsed()
    }
}

/// The message a failed driver-side send/read surfaces — the raw transport text (`"server
/// closed"`, the tungstenite `Display`, …).
fn stream_error_message(e: WebsocketSessionUserError) -> String {
    match e {
        WebsocketSessionUserError::Closed(m) => m,
        WebsocketSessionUserError::Timeout => "read timeout".into(), // unreachable for sends
    }
}

/// One WS session over an already-connected `stream`: send the subscribe (arming the ack
/// watchdog), then read frames until a requested stop (`Ok(())`), a transport fault, an ack/idle
/// watchdog trip, or a [`WebsocketSessionFrameOutcome::Fatal`] frame (all `Err(message)` — the feed loop
/// discloses and reconnects). `now_ms` is injected (production wall-clock; tests a scripted
/// counter) so the ack watchdog is unit-testable with zero sleeps. `on_tick` runs on every
/// transport-ALIVE read-timeout tick — after the stop poll and both watchdogs pass — the seam a
/// venue's dataless-tick judgment (polymarket's data-freshness disclosure) rides; venues without
/// one pass `&mut || {}`.
///
/// `on_live` fires ONCE per session, on the FIRST [`WebsocketSessionFrameOutcome::Confirm`] — edge-triggered HERE
/// rather than at the caller's `connect()`-Ok, because this is the seam BOTH entry points cross
/// (some production call sites drive this function directly, never [`run_market_on`]), and it
/// reuses the ONE definition of "this session is working" the driver trusts —
/// [`WebsocketSessionSubscribeAck::confirm`]'s — instead of standing a second, weaker one beside it.
///
/// A session the venue announced the end of (`opts.notice`) returns the notice's words as its `Err`: this entry point
/// owns no reconnect, so the planned one is [`run_market_on`]'s.
pub fn run_market_session<S: WebsocketSessionMarketStream>(
    stream: &mut S,
    opts: &WebsocketSessionMarketOptions<'_>,
    stop: &AtomicBool,
    now_ms: &dyn Fn() -> i64,
    on_text: &mut dyn FnMut(&str) -> WebsocketSessionFrameOutcome,
    on_tick: &mut dyn FnMut(),
    on_live: &mut dyn FnMut(),
) -> Result<(), String> {
    session(stream, opts, stop, now_ms, on_text, on_tick, on_live).map_err(|end| match end {
        SessionEnd::Fault(why) | SessionEnd::Announced(why, _) => why,
    })
}

/// How a session ended, other than by a requested stop.
enum SessionEnd {
    /// A fault, in its own words.
    Fault(String),
    /// The venue announced the end, in the notice hook's words, after the session lived this long.
    Announced(String, Duration),
}

impl From<String> for SessionEnd {
    fn from(why: String) -> Self {
        SessionEnd::Fault(why)
    }
}

/// [`run_market_session`]'s body, telling an announced end from a fault.
fn session<S: WebsocketSessionMarketStream>(
    stream: &mut S,
    opts: &WebsocketSessionMarketOptions<'_>,
    stop: &AtomicBool,
    now_ms: &dyn Fn() -> i64,
    on_text: &mut dyn FnMut(&str) -> WebsocketSessionFrameOutcome,
    on_tick: &mut dyn FnMut(),
    on_live: &mut dyn FnMut(),
) -> Result<(), SessionEnd> {
    let opened = pump_clock::now();
    if let Some(sub) = opts.subscribe {
        stream.send_text(sub).map_err(stream_error_message)?;
    }
    // Arm the subscribe-ack watchdog the moment the subscribe is sent. Clock-free
    // `WebsocketSessionSubscribeAck`, fed `now_ms()` only here and on each read-timeout poll tick below.
    let mut ack = opts
        .ack_timeout
        .map(|t| (WebsocketSessionSubscribeAck::new(now_ms(), t.as_millis() as i64), t.as_secs()));
    let mut last_ka = pump_clock::now();
    // Edge trigger for `on_live`. One predictable-false bool test per frame after the first, on a
    // venue WS thread — NOT `vike-core`'s fold, so the hot-fold no-logging rule is not engaged.
    // Per SESSION, never per frame: a venue's hook takes a status mutex, and taking it on every
    // kline tick is the cost this flag exists to refuse.
    let mut disclosed_live = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        // App-level keepalive on the pump clock's cadence (checked before each read; the read timeout
        // guarantees this runs at least every read tick) — the depth driver's shape.
        if let Some(ka) = &opts.keepalive
            && pump_clock::since(last_ka) >= ka.every
        {
            stream.send_text(ka.payload).map_err(stream_error_message)?;
            last_ka = pump_clock::now();
        }
        match stream.read_frame() {
            Ok(txt) => {
                if let Some(words) = opts.notice.and_then(|notice| notice(&txt)) {
                    // `info`: a planned reopen is `Connecting` (the feed status's level rule).
                    tracing::info!(
                        notice = %words,
                        "market WS: the venue announced the session's end — reconnecting"
                    );
                    return Err(SessionEnd::Announced(words, pump_clock::since(opened)));
                }
                match on_text(&txt) {
                    WebsocketSessionFrameOutcome::Confirm => {
                        if let Some((a, _)) = &mut ack {
                            a.confirm();
                        }
                        if !disclosed_live {
                            disclosed_live = true;
                            on_live();
                        }
                    }
                    WebsocketSessionFrameOutcome::Ignore => {}
                    WebsocketSessionFrameOutcome::Fatal(msg) => return Err(msg.into()),
                }
            }
            // Read-timeout poll tick: the stop flag re-checks at the loop top; here the two
            // watchdogs run — ack first, then idle — and only when both pass (the transport is
            // judged alive) does the venue's `on_tick` hook fire.
            Err(WebsocketSessionUserError::Timeout) => {
                if let Some((a, secs)) = &ack
                    && a.overdue(now_ms())
                {
                    return Err(format!("no subscribe ack/data within {secs}s").into());
                }
                if let Some(idle) = opts.idle_threshold
                    && stream.since_last_frame() >= idle
                {
                    return Err(
                        format!("no frames within {}s (silent stall)", idle.as_secs()).into()
                    );
                }
                on_tick();
            }
            Err(WebsocketSessionUserError::Closed(m)) => return Err(m.into()),
        }
    }
}

/// The generic reconnect loop over `connect` + [`run_market_session`] — the testable core of
/// [`run_market`]. A session fault (or a failed connect) is disclosed as
/// [`WebsocketSessionStatus::Error`], then the stop-aware backoff runs and the next session reconnects +
/// resubscribes; a clean stop ends the loop. The SAME `on_text`/`on_tick` closures are reused
/// across reconnects, so any per-feed state they hold survives a blip (hyperliquid's
/// rollover-cursor rule; polymarket's book/health state).
///
/// ⚠ **A successful connect does TWO things.** It resets the doubling ladder
/// to `initial` (a no-op for a fixed wait), and the session it starts discloses [`WebsocketSessionStatus::Live`]
/// on its first confirmed frame. Without the second, a venue's status string latches its last
/// error behind a perfectly live socket — see [`WebsocketSessionStatus`].
///
/// The two disclosures are SEQUENTIAL borrows of `on_session_status` (the session returns before
/// the `match` runs), so no `RefCell` shim is needed on the venue's side.
///
/// A clean stop discloses NOTHING: the `Ok(()) => break` arm stays silent, the same rule
/// [`crate::depth`]'s `WebsocketSessionDepthFaultLog` states and for the same reason — a requested teardown is not
/// a fault and not a recovery.
///
/// A session the venue announced the end of (`opts.notice`) is disclosed as [`WebsocketSessionStatus::Announced`],
/// and its reopen waits [`ReconnectBackoff::announced`]'s wait: none after a session past the ladder's reset, the
/// ladder's after a younger one (a venue announcing the end of every new session never loops hot).
pub fn run_market_on<S: WebsocketSessionMarketStream>(
    connect: impl FnMut() -> Result<S, String>,
    opts: &WebsocketSessionMarketOptions<'_>,
    stop: &AtomicBool,
    now_ms: &dyn Fn() -> i64,
    on_text: impl FnMut(&str) -> WebsocketSessionFrameOutcome,
    on_tick: impl FnMut(),
    on_session_status: impl FnMut(WebsocketSessionStatus<'_>),
) {
    run_market_on_paced(connect, opts, stop, now_ms, on_text, on_tick, on_session_status, |wait| {
        pause_in_ticks(stop, wait)
    });
}

/// [`run_market_on`] with its reconnect pause handed in: `pause(wait)` waits `wait` and answers `false` when a stop
/// cut it short. Production's is [`pause_in_ticks`]; a test records each wait instead of sleeping it.
fn run_market_on_paced<S: WebsocketSessionMarketStream>(
    mut connect: impl FnMut() -> Result<S, String>,
    opts: &WebsocketSessionMarketOptions<'_>,
    stop: &AtomicBool,
    now_ms: &dyn Fn() -> i64,
    mut on_text: impl FnMut(&str) -> WebsocketSessionFrameOutcome,
    mut on_tick: impl FnMut(),
    mut on_session_status: impl FnMut(WebsocketSessionStatus<'_>),
    mut pause: impl FnMut(Duration) -> bool,
) {
    let mut backoff = ReconnectBackoff::from_row(&opts.backoff);
    while !stop.load(Ordering::Relaxed) {
        let res = match connect() {
            Ok(mut stream) => {
                backoff.reset(); // a successful connect starts the doubling ladder over
                session(&mut stream, opts, stop, now_ms, &mut on_text, &mut on_tick, &mut || {
                    on_session_status(WebsocketSessionStatus::Live)
                })
            }
            // connect failed — same disclosure + backoff path as a session fault
            Err(e) => Err(SessionEnd::Fault(e)),
        };
        let wait = match res {
            Ok(()) => break, // requested stop
            Err(SessionEnd::Fault(e)) => {
                on_session_status(WebsocketSessionStatus::Error(&e));
                // An exponential policy doubles the NEXT wait after each consecutive fault.
                backoff.failed()
            }
            Err(SessionEnd::Announced(words, lived)) => {
                on_session_status(WebsocketSessionStatus::Announced(&words));
                backoff.announced(lived)
            }
        };
        if !pause(wait) {
            return;
        }
    }
}

/// The reconnect pause of [`run_market_on`]: `wait` in stop-aware 100 ms stop-poll ticks, `(wait/100ms).max(1)` of
/// them; `false` when the stop was raised. No wait (an announced end's reopen at once) takes no tick: the loop reads
/// the stop before it dials.
fn pause_in_ticks(stop: &AtomicBool, wait: Duration) -> bool {
    if wait.is_zero() {
        return true;
    }
    let steps = (wait.as_millis() / 100).max(1);
    for _ in 0..steps {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        pump_clock::sleep(Duration::from_millis(100));
    }
    true
}

/// Dial `url` and wrap the socket as a production [`WebsocketSessionMarketStream`] (read timeout + `TCP_NODELAY`
/// via [`configure_websocket_stream`]). With `connect_timeout: Some`, the whole TCP phase —
/// resolution and every resolved address — is bounded by [`vike_transport::websocket_proxy::connect_websocket`]'s one
/// window and the TLS + WS handshake rides
/// `tungstenite::client_tls` (the same rustls backend plain `connect` uses) — plain
/// `tungstenite::connect` has NO connect bound at all, so a black-holed route would otherwise pin
/// the thread until the OS's own TCP timeout, ignoring the stop flag. Public so a venue with a
/// custom connect closure (polymarket wraps this in its raw-frame `WebsocketSessionMarketStreamTapped`) dials the same
/// way [`run_market`] does.
///
/// Exactly [`connect_market_stream_via`] with **no proxy**.
pub fn connect_market_stream(
    url: &str,
    read_timeout: Duration,
    connect_timeout: Option<Duration>,
) -> Result<impl WebsocketSessionMarketStream, String> {
    connect_market_stream_via(url, read_timeout, connect_timeout, None)
}

/// The dialed-and-configured RAW socket [`connect_market_stream_via`] wraps — the SAME dial, one
/// layer lower, for a caller that wraps the `WsSocket` in [`WebsocketSessionMarketStreamTungstenite`]
/// itself and so can name the stream's type: a dial that serves every URL, which the opaque return of
/// [`connect_market_stream`] (it captures the URL's lifetime) cannot be. The data-client engine's
/// `Session` dial (`crate::data_client::ws`) is one, binance's trade tape another
/// (`crates/bridges/binance/src/family/feed/tick/trades.rs`'s `dial_trades_session`, whose post-warmup
/// drain reads that stream). Such a caller dials HERE rather than re-spelling `tungstenite::connect`,
/// which applies no bound and ignores its pump row's `connect_timeout`.
///
/// Every bound, pin and proxy arm is [`vike_transport::websocket_proxy::connect_websocket`]'s, verbatim — there is exactly
/// one dial in this module and both entry points are it.
pub fn connect_market_socket(
    url: &str,
    read_timeout: Duration,
    connect_timeout: Option<Duration>,
    proxy: Option<&WebsocketProxy>,
) -> Result<WsSocket, String> {
    let socket = connect_websocket(url, proxy, connect_timeout)?;
    configure_websocket_stream(&socket, read_timeout);
    Ok(socket)
}

/// [`connect_market_stream`] with an OPTIONAL SOCKS5 egress ([`vike_transport::websocket_proxy`]).
///
/// `proxy: None` is the plain dial; only a venue that explicitly resolves a [`WebsocketProxy`]
/// (polymarket, whose CLOB market/user/RTDS hosts are geo-blocked) takes the tunnelled arm. See
/// [`vike_transport::websocket_proxy::connect_websocket`] for the three arms; the tunnelled one spends the same
/// `connect_timeout`.
pub fn connect_market_stream_via(
    url: &str,
    read_timeout: Duration,
    connect_timeout: Option<Duration>,
    proxy: Option<&WebsocketProxy>,
) -> Result<impl WebsocketSessionMarketStream, String> {
    let socket = connect_market_socket(url, read_timeout, connect_timeout, proxy)?;
    Ok(WebsocketSessionMarketStreamTungstenite::new(WebsocketSessionUserStreamTungstenite(socket)))
}

/// Connect `url` (via [`connect_market_stream`] — `opts.connect_timeout` bounds the dial when set)
/// and run the reconnect loop — the whole thread body a venue's market-feed spawn runs. See
/// [`run_market_on`] for the loop semantics and [`WebsocketSessionMarketOptions`] for the knobs.
pub fn run_market(
    url: &str,
    opts: &WebsocketSessionMarketOptions<'_>,
    stop: &AtomicBool,
    now_ms: &dyn Fn() -> i64,
    on_text: impl FnMut(&str) -> WebsocketSessionFrameOutcome,
    on_tick: impl FnMut(),
    on_session_status: impl FnMut(WebsocketSessionStatus<'_>),
) {
    run_market_on(
        || connect_market_stream(url, opts.read_timeout, opts.connect_timeout),
        opts,
        stop,
        now_ms,
        on_text,
        on_tick,
        on_session_status,
    )
}

#[path = "market_tests.rs"]
#[cfg(test)]
mod market_tests;
