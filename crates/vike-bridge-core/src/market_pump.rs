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
//! the driver knows what the frame meant for the handshake) and `on_session_error` (status-line
//! disclosure). The subscribe frame is replayed VERBATIM at the start of every session, so
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
use crate::ws::{configure_ws_stream, TungsteniteStream, WsSocket};
use crate::ws_proxy::{connect_ws, WsProxy};

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
/// `on_text`/`on_session_error` closures), mirroring `spawn_pump_with_resync`'s spec-struct style.
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
pub fn run_market_session<S: MarketStream>(
    stream: &mut S,
    opts: &MarketPumpOpts<'_>,
    stop: &AtomicBool,
    now_ms: &dyn Fn() -> i64,
    on_text: &mut dyn FnMut(&str) -> FrameOutcome,
    on_tick: &mut dyn FnMut(),
) -> Result<(), String> {
    if let Some(sub) = opts.subscribe {
        stream.send_text(sub).map_err(stream_error_msg)?;
    }
    // br7: arm the subscribe-ack watchdog the moment the subscribe is sent. Clock-free
    // `SubscribeAck`, fed `now_ms()` only here and on each read-timeout poll tick below.
    let mut ack =
        opts.ack_timeout.map(|t| (SubscribeAck::new(now_ms(), t.as_millis() as i64), t.as_secs()));
    let mut last_ka = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        // App-level keepalive on a wall-clock cadence (checked before each read; the read timeout
        // guarantees this runs at least every read tick) — the depth driver's shape.
        if let Some(ka) = &opts.keepalive {
            if last_ka.elapsed() >= ka.every {
                stream.send_text(ka.payload).map_err(stream_error_msg)?;
                last_ka = Instant::now();
            }
        }
        match stream.read_frame() {
            Ok(txt) => match on_text(&txt) {
                FrameOutcome::Confirm => {
                    if let Some((a, _)) = &mut ack {
                        a.confirm();
                    }
                }
                FrameOutcome::Ignore => {}
                FrameOutcome::Fatal(msg) => return Err(msg),
            },
            // Read-timeout poll tick: the stop flag re-checks at the loop top; here the two
            // watchdogs run — ack first (the venues' pre-driver order), then idle — and only when
            // both pass (the transport is judged alive) does the venue's `on_tick` hook fire.
            Err(StreamError::Timeout) => {
                if let Some((a, secs)) = &ack {
                    if a.overdue(now_ms()) {
                        return Err(format!("no subscribe ack/data within {secs}s"));
                    }
                }
                if let Some(idle) = opts.idle_threshold {
                    if stream.since_last_frame() >= idle {
                        return Err(format!("no frames within {}s (silent stall)", idle.as_secs()));
                    }
                }
                on_tick();
            }
            Err(StreamError::Closed(m)) => return Err(m),
        }
    }
}

/// The generic reconnect loop over `connect` + [`run_market_session`] — the testable core of
/// [`run_market_feed`]. A session fault (or a failed connect) is disclosed via
/// `on_session_error`, then the stop-aware backoff runs and the next session reconnects +
/// resubscribes; a clean stop ends the loop. The SAME `on_text`/`on_tick` closures are reused
/// across reconnects, so any per-feed state they hold survives a blip (hyperliquid's
/// rollover-cursor rule; polymarket's book/health state). A successful connect resets the
/// [`PumpBackoff::Exponential`] ladder to `initial` (a no-op for `Fixed`).
pub fn run_market_feed_on<S: MarketStream>(
    mut connect: impl FnMut() -> Result<S, String>,
    opts: &MarketPumpOpts<'_>,
    stop: &AtomicBool,
    now_ms: &dyn Fn() -> i64,
    mut on_text: impl FnMut(&str) -> FrameOutcome,
    mut on_tick: impl FnMut(),
    mut on_session_error: impl FnMut(&str),
) {
    let mut backoff = opts.backoff.initial();
    while !stop.load(Ordering::Relaxed) {
        let res = match connect() {
            Ok(mut stream) => {
                backoff = opts.backoff.initial(); // a successful connect resets the doubling ladder
                run_market_session(&mut stream, opts, stop, now_ms, &mut on_text, &mut on_tick)
            }
            Err(e) => Err(e), // connect failed — same disclosure + backoff path as a session fault
        };
        match res {
            Ok(()) => break, // requested stop
            Err(e) => on_session_error(&e),
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
    on_session_error: impl FnMut(&str),
) {
    run_market_feed_on(
        || connect_market_stream(url, opts.read_timeout, opts.connect_timeout),
        opts,
        stop,
        now_ms,
        on_text,
        on_tick,
        on_session_error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    // The shared scripted MarketStream double (testing-arch Phase 4c) — the canonical copy of the
    // `Step`/`ScriptedStream` pair that used to live inline here. `send_text` records into a
    // shareable log (`with_sent_log`) so a feed test can assert across sessions; the shared
    // `clock` (`clocked`) advances before every read so consuming the script ages the ack
    // watchdog with zero real sleeps.
    use crate::scripted::{ScriptStep as Step, ScriptedStream};

    fn opts_with(ack: Option<Duration>, idle: Option<Duration>) -> MarketPumpOpts<'static> {
        MarketPumpOpts {
            subscribe: Some(r#"{"op":"subscribe"}"#),
            keepalive: None,
            ack_timeout: ack,
            idle_threshold: idle,
            read_timeout: Duration::from_secs(2),
            // 1 stop-aware 100 ms tick — keeps tests fast
            backoff: PumpBackoff::Fixed(Duration::from_millis(1)),
            connect_timeout: None,
        }
    }

    /// The subscribe frame is replayed VERBATIM at the start of EVERY session — the
    /// reconnect-resubscribes pin. Session 1 ends by exhaustion (a disconnect); the driver backs
    /// off, reconnects (stream 2), and the shared send log shows the subscribe twice, in order,
    /// before the second session's data flows.
    #[test]
    fn reconnect_resends_the_subscribe_each_session() {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let stop = AtomicBool::new(false);
        let errors = RefCell::new(Vec::<String>::new());
        let mut streams = VecDeque::from([
            ScriptedStream::from_steps(vec![Step::Text("data1".into())])
                .with_sent_log(Rc::clone(&sent)),
            ScriptedStream::from_steps(vec![Step::Text("stop-now".into())])
                .with_sent_log(Rc::clone(&sent)),
        ]);
        let connects = Cell::new(0);
        run_market_feed_on(
            || {
                connects.set(connects.get() + 1);
                streams.pop_front().ok_or_else(|| "no more scripted streams".to_string())
            },
            &opts_with(None, None),
            &stop,
            &|| 0,
            |txt| {
                if txt == "stop-now" {
                    stop.store(true, Ordering::Relaxed);
                }
                FrameOutcome::Confirm
            },
            || {},
            |e| errors.borrow_mut().push(e.to_string()),
        );
        assert_eq!(connects.get(), 2, "one reconnect after the first session's disconnect");
        let sent = sent.borrow();
        assert_eq!(
            sent.iter().filter(|s| s.contains("subscribe")).count(),
            2,
            "the subscribe frame is re-sent on the reconnected session: {sent:?}"
        );
        assert_eq!(
            errors.borrow().len(),
            1,
            "exactly the first session's disconnect was disclosed: {:?}",
            errors.borrow()
        );
    }

    /// A pre-raised stop never connects at all — the deterministic-teardown property at the feed
    /// level.
    #[test]
    fn a_pre_raised_stop_never_connects() {
        let stop = AtomicBool::new(true);
        let connects = Cell::new(0);
        run_market_feed_on(
            || {
                connects.set(connects.get() + 1);
                Ok(ScriptedStream::from_steps(vec![]))
            },
            &opts_with(None, None),
            &stop,
            &|| 0,
            |_| FrameOutcome::Ignore,
            || {},
            |_| {},
        );
        assert_eq!(connects.get(), 0, "a pre-raised stop must not open a socket");
    }

    /// A stop raised mid-session ends the session `Ok` on the next read tick and the feed loop
    /// exits WITHOUT disclosing an error or reconnecting — the prompt-exit pin.
    #[test]
    fn a_stop_mid_session_exits_without_reconnect() {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let stop = AtomicBool::new(false);
        let connects = Cell::new(0);
        let errors = Cell::new(0);
        let mut stream = Some(
            ScriptedStream::from_steps(vec![Step::Text("x".into()), Step::Timeout, Step::Timeout])
                .with_sent_log(Rc::clone(&sent)),
        );
        run_market_feed_on(
            || {
                connects.set(connects.get() + 1);
                stream.take().ok_or_else(|| "reconnected after a requested stop".to_string())
            },
            &opts_with(None, None),
            &stop,
            &|| 0,
            |_| {
                stop.store(true, Ordering::Relaxed);
                FrameOutcome::Confirm
            },
            || {},
            |_| errors.set(errors.get() + 1),
        );
        assert_eq!(connects.get(), 1, "no reconnect after a requested stop");
        assert_eq!(errors.get(), 0, "a requested stop is not an error");
    }

    /// br7: a subscribe that never acks and never delivers data trips the ack watchdog once the
    /// window elapses — the exact attributable `"no subscribe ack/data within {N}s"` error, which
    /// the feed loop discloses (then the test's error hook raises stop, so exactly one session
    /// runs).
    #[test]
    fn an_unacked_subscribe_trips_the_watchdog_with_the_attributable_error() {
        let clock = Rc::new(Cell::new(0_i64));
        let now = {
            let c = Rc::clone(&clock);
            move || c.get()
        };
        let stop = AtomicBool::new(false);
        let errors = RefCell::new(Vec::<String>::new());
        // 3 s of clock per read tick; deadline = 5 s (armed at t=0): tick1 t=3s (fresh),
        // tick2 t=6s → overdue.
        let mut stream = Some(
            ScriptedStream::from_steps(vec![Step::Timeout, Step::Timeout])
                .clocked(Rc::clone(&clock), 3_000),
        );
        run_market_feed_on(
            || stream.take().ok_or_else(|| "second connect".to_string()),
            &opts_with(Some(Duration::from_secs(5)), None),
            &stop,
            &now,
            |_| FrameOutcome::Confirm,
            || {},
            |e| {
                errors.borrow_mut().push(e.to_string());
                stop.store(true, Ordering::Relaxed);
            },
        );
        assert_eq!(
            errors.borrow().as_slice(),
            ["no subscribe ack/data within 5s"],
            "the trip is the attributable br7 error"
        );
    }

    /// First DATA disarms the ack watchdog exactly like a venue ack: after one Confirm frame, the
    /// clock may run arbitrarily far past the deadline without a trip — the session ends on the
    /// scripted disconnect instead, and that error must NOT be the ack message.
    #[test]
    fn first_data_disarms_the_ack_watchdog() {
        let clock = Rc::new(Cell::new(0_i64));
        let now = {
            let c = Rc::clone(&clock);
            move || c.get()
        };
        let stop = AtomicBool::new(false);
        let errors = RefCell::new(Vec::<String>::new());
        let mut stream = Some(
            ScriptedStream::from_steps(vec![
                Step::Text("data".into()),
                Step::Timeout,
                Step::Timeout,
                Step::Timeout,
            ])
            .clocked(Rc::clone(&clock), 3_000),
        );
        run_market_feed_on(
            || stream.take().ok_or_else(|| "second connect".to_string()),
            &opts_with(Some(Duration::from_secs(5)), None),
            &stop,
            &now,
            |_| FrameOutcome::Confirm,
            || {},
            |e| {
                errors.borrow_mut().push(e.to_string());
                stop.store(true, Ordering::Relaxed);
            },
        );
        let errors = errors.borrow();
        assert_eq!(errors.len(), 1, "the session ends on the scripted disconnect: {errors:?}");
        assert!(
            !errors[0].contains("subscribe ack"),
            "a confirmed handshake never trips the ack watchdog: {errors:?}"
        );
    }

    /// A [`FrameOutcome::Fatal`] frame (a venue subscribe REJECT) ends the session with exactly
    /// the venue's message — attributable, never silently dropped (br7).
    #[test]
    fn a_fatal_frame_ends_the_session_with_its_message() {
        let stop = AtomicBool::new(false);
        let errors = RefCell::new(Vec::<String>::new());
        let mut stream = Some(ScriptedStream::from_steps(vec![Step::Text("reject".into())]));
        run_market_feed_on(
            || stream.take().ok_or_else(|| "second connect".to_string()),
            &opts_with(None, None),
            &stop,
            &|| 0,
            |_| FrameOutcome::Fatal("Bybit subscribe rejected: Invalid symbol".into()),
            || {},
            |e| {
                errors.borrow_mut().push(e.to_string());
                stop.store(true, Ordering::Relaxed);
            },
        );
        assert_eq!(
            errors.borrow().as_slice(),
            ["Bybit subscribe rejected: Invalid symbol"],
            "the venue's reject message is surfaced verbatim"
        );
    }

    /// The idle watchdog (hyperliquid's silent-stall shape): a stalled-but-open stream past the
    /// threshold errs so the feed reconnects; the threshold disabled (`None`) never trips.
    #[test]
    fn the_idle_watchdog_trips_only_when_enabled_and_stalled() {
        let stop = AtomicBool::new(false);
        let errors = RefCell::new(Vec::<String>::new());
        let mut stream = Some(
            ScriptedStream::from_steps(vec![Step::Timeout]).with_stall(Duration::from_secs(90)),
        );
        run_market_feed_on(
            || stream.take().ok_or_else(|| "second connect".to_string()),
            &opts_with(None, Some(Duration::from_secs(60))),
            &stop,
            &|| 0,
            |_| FrameOutcome::Confirm,
            || {},
            |e| {
                errors.borrow_mut().push(e.to_string());
                stop.store(true, Ordering::Relaxed);
            },
        );
        assert_eq!(
            errors.borrow().as_slice(),
            ["no frames within 60s (silent stall)"],
            "a stalled transport trips the idle watchdog"
        );

        // Disabled (`None`): the same stalled stream absorbs the timeout tick quietly and the
        // session ends on the scripted disconnect instead (the bybit kline/trades shape).
        let stop2 = AtomicBool::new(false);
        let errors2 = RefCell::new(Vec::<String>::new());
        let mut stream2 = Some(
            ScriptedStream::from_steps(vec![Step::Timeout]).with_stall(Duration::from_secs(90)),
        );
        run_market_feed_on(
            || stream2.take().ok_or_else(|| "second connect".to_string()),
            &opts_with(None, None),
            &stop2,
            &|| 0,
            |_| FrameOutcome::Confirm,
            || {},
            |e| {
                errors2.borrow_mut().push(e.to_string());
                stop2.store(true, Ordering::Relaxed);
            },
        );
        assert_eq!(
            errors2.borrow().as_slice(),
            ["script exhausted"],
            "with the watchdog off, a stalled stream is not an idle fault"
        );
    }

    /// The app-level keepalive rides the read ticks on its cadence: with `every = ZERO` it is due
    /// before every read, so the send log interleaves pings after the subscribe.
    #[test]
    fn the_keepalive_is_sent_on_cadence() {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let stop = AtomicBool::new(false);
        let mut stream = Some(
            ScriptedStream::from_steps(vec![Step::Timeout, Step::Timeout, Step::Timeout])
                .with_sent_log(Rc::clone(&sent)),
        );
        let opts = MarketPumpOpts {
            subscribe: Some(r#"{"op":"subscribe"}"#),
            keepalive: Some(Keepalive { payload: r#"{"op":"ping"}"#, every: Duration::ZERO }),
            ack_timeout: None,
            idle_threshold: None,
            read_timeout: Duration::from_secs(2),
            backoff: PumpBackoff::Fixed(Duration::from_millis(1)),
            connect_timeout: None,
        };
        run_market_feed_on(
            || stream.take().ok_or_else(|| "second connect".to_string()),
            &opts,
            &stop,
            &|| 0,
            |_| FrameOutcome::Confirm,
            || {},
            |_| stop.store(true, Ordering::Relaxed),
        );
        let sent = sent.borrow();
        assert_eq!(sent[0], r#"{"op":"subscribe"}"#, "subscribe first: {sent:?}");
        assert!(
            sent.iter().filter(|s| s.contains("ping")).count() >= 3,
            "one keepalive per due read tick: {sent:?}"
        );
    }

    /// `on_tick` (polymarket knob): fires on every transport-ALIVE read-timeout tick — never on a
    /// data frame, and never on the tick that trips a watchdog (the trip returns first).
    #[test]
    fn on_tick_fires_on_alive_timeout_ticks_only() {
        let stop = AtomicBool::new(false);
        let ticks = Cell::new(0);
        let mut stream = Some(ScriptedStream::from_steps(vec![
            Step::Text("data".into()),
            Step::Timeout,
            Step::Timeout,
            Step::Timeout,
        ]));
        run_market_feed_on(
            || stream.take().ok_or_else(|| "second connect".to_string()),
            &opts_with(None, None),
            &stop,
            &|| 0,
            |_| FrameOutcome::Confirm,
            || ticks.set(ticks.get() + 1),
            |_| stop.store(true, Ordering::Relaxed),
        );
        assert_eq!(ticks.get(), 3, "one on_tick per alive timeout tick, none for the data frame");

        // An idle-watchdog trip consumes its tick BEFORE the hook: a stalled stream's only
        // timeout tick errors out, so on_tick never fires.
        let stop2 = AtomicBool::new(false);
        let ticks2 = Cell::new(0);
        let mut stream2 = Some(
            ScriptedStream::from_steps(vec![Step::Timeout]).with_stall(Duration::from_secs(90)),
        );
        run_market_feed_on(
            || stream2.take().ok_or_else(|| "second connect".to_string()),
            &opts_with(None, Some(Duration::from_secs(60))),
            &stop2,
            &|| 0,
            |_| FrameOutcome::Confirm,
            || ticks2.set(ticks2.get() + 1),
            |_| stop2.store(true, Ordering::Relaxed),
        );
        assert_eq!(ticks2.get(), 0, "a tripping tick returns before the on_tick hook");
    }

    /// [`PumpBackoff`] (polymarket knob): the exponential ladder doubles per consecutive fault,
    /// caps at `max`, and restarts at `initial`; the fixed policy never moves.
    #[test]
    fn pump_backoff_ladder() {
        let exp = PumpBackoff::Exponential {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(30),
        };
        assert_eq!(exp.initial(), Duration::from_millis(500));
        let mut cur = exp.initial();
        let mut walk = Vec::new();
        for _ in 0..8 {
            cur = exp.next(cur);
            walk.push(cur.as_millis());
        }
        assert_eq!(
            walk,
            vec![1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000],
            "500ms doubling capped at 30s — polymarket's pre-driver shape"
        );

        let fixed = PumpBackoff::Fixed(Duration::from_secs(3));
        assert_eq!(fixed.initial(), Duration::from_secs(3));
        assert_eq!(fixed.next(Duration::from_secs(3)), Duration::from_secs(3));

        // The reset contract [`run_market_feed_on`] applies on every successful connect: the next
        // wait after a success is `initial()` again, however far the ladder had climbed.
        assert_eq!(exp.initial(), Duration::from_millis(500), "success resets to initial");
    }
}
