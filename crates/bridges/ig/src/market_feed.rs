//! Live IG market data → the vike-data live seam. [`Feeds`] implements
//! [`vike_data::DataClient`] — IG's twin of the crypto venues' `market_feed`, over a transport no
//! other venue has: Lightstreamer TLCP 2.1.0 spoken RAW over the shared tungstenite WS stack
//! (`Sec-WebSocket-Protocol: TLCP-2.1.0.lightstreamer.com` — no Lightstreamer client crate, per
//! the one-transport-stack rule). The pure codec is [`crate::lightstreamer`]; the field
//! normalizers are [`crate::market_data`]; this module owns only transport + lifecycle.
//!
//! **Verbs served** (declared caps `{bars, quotes}` — see `vike_model::venue_caps::IG`):
//! - `subscribe_quotes` → `MARKET:{epic}` (MERGE, snapshot) → [`LiveDataSink::quote`]. L1 only —
//!   IG's streaming API serves NO depth, so `book`/`depth` are structurally refusable.
//! - `subscribe_bars` → `CHART:{epic}:{scale}` (MERGE, snapshot) → `forming_bar` +
//!   `close_bar`/`bar_close_tick`. Streaming scales only ([`crate::market_data::ig_scale`]:
//!   1s/1m/5m/1h); other intervals are a `Subscribe` refusal naming the set. ⚠ NO REST seed
//!   before streaming, deliberately: IG meters `/prices` on a weekly per-account allowance
//!   (~10k points), which an auto-seed on every chart open would silently burn — a chart fills
//!   from the first live close forward (the hyperliquid convention). `trades` is refused via
//!   [`vike_data::require_live_verb`]: IG is a dealer venue with no public trade tape.
//!
//! **Lifecycle** — one STOPPABLE thread per subscription ([`vike_data::FeedRegistry`]), each
//! riding the SHARED market-pump session loop ([`vike_bridge_core::run_market_feed_on`]) with the
//! knobs of this venue's `MarketPumpSpec` row: stop-poll read timeout, subscribe-ack + silent-
//! stall watchdogs, stop-aware fixed backoff. The TLCP handshake cannot be a replayed subscribe
//! frame (`create_session` must answer CONOK before `control` may be sent), so it lives in the
//! CONNECT closure — the binance-trades shape (a venue-owned connect consuming the row's
//! `connect_timeout`) — and the row declares `subscribe_frame: false`.
//!
//! **Sessions and tokens.** Each thread logs in with [`IgSession::login`] (the crate's ONE login
//! path) and caches the CST/XST pair across reconnects; a `CONERR` drops the cache so the next
//! attempt re-logs-in with fresh tokens. This lane therefore re-authenticates after token expiry
//! — unlike the exec trade stream's fire-once login (see the crate CLAUDE.md). Cost: one IG REST
//! session per live subscription (IG tolerates concurrent sessions; the exec mount already opens
//! several), renewed only on failure, never per rebind.

use std::cell::{Cell, RefCell};
use std::net::{TcpStream, ToSocketAddrs};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;

use vike_bridge_core::user_data::{StreamError, StreamMsg, UserStream};
use vike_bridge_core::ws::{configure_ws_stream, TungsteniteStream, WsSocket};
use vike_bridge_core::{market_pump_spec, run_market_feed_on, FrameOutcome, MarketStream};
use vike_data::{
    require_live_verb, DataClient, FeedRegistry, LiveDataError, LiveDataSink, StreamStatus,
    SubscriptionId,
};
use vike_model::{now_ms, LiveVerb, QuoteTick};

use crate::config::IgConfig;
use crate::lightstreamer::{
    create_session_request, parse_frame, subscribe_request, LsFrame, LS_WS_SUBPROTOCOL,
};
use crate::market_data::{
    chart_item, ig_scale, market_item, CandleEvent, CandleFold, QuoteFold, CANDLE_SCHEMA,
    QUOTE_SCHEMA,
};
use crate::rest::IgSession;

const VENUE: &str = "ig";
/// One subscription per TLCP session, so the id is fixed.
const SUB_ID: u32 = 1;
/// Wall-clock budget for the create_session → CONOK handshake inside the connect closure.
const HANDSHAKE_BUDGET: Duration = Duration::from_secs(15);

/// Live IG market data client (one per mounted venue; subscribe many).
pub struct Feeds {
    sink: Arc<dyn LiveDataSink>,
    /// Human-readable connection status (GUI Connections tool convention).
    pub status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    registry: FeedRegistry,
    config: IgConfig,
}

/// Which lane a feed thread drives.
enum Lane {
    Quotes,
    Candles { interval: String, scale: &'static str },
}

impl Feeds {
    /// `sink` receives every quote/forming/close call from every subscription; `wake` is the GUI
    /// repaint nudge (pass `|| {}` headless); `config` is the SAME login shape the exec side uses
    /// ([`crate::load_ig_config_from`]) — this client opens its own sessions from it and never
    /// shares the exec side's. The registry's spawn hook is the venue's HFT affinity pin.
    pub fn new(
        sink: Arc<dyn LiveDataSink>,
        wake: impl Fn() + Send + Sync + 'static,
        config: IgConfig,
    ) -> Self {
        Feeds {
            sink,
            status: Arc::new(Mutex::new("connecting to IG…".into())),
            wake: Arc::new(wake),
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    VENUE,
                );
            }),
            config,
        }
    }

    fn spawn(&mut self, epic: &str, lane: Lane) -> Result<SubscriptionId, LiveDataError> {
        let (sink, status, wake) =
            (Arc::clone(&self.sink), Arc::clone(&self.status), Arc::clone(&self.wake));
        let (config, epic_owned) = (self.config.clone(), epic.to_string());
        let label = match &lane {
            Lane::Quotes => "quotes".to_string(),
            Lane::Candles { interval, .. } => interval.clone(),
        };
        self.registry
            .spawn(format!("feed-ig-{epic}@{label}"), move |stop| {
                feed_main(epic_owned, lane, FeedCtx { sink, status, wake, stop, config })
            })
            .map_err(|e| LiveDataError::Subscribe(format!("ig {epic}@{label}: {e}")))
    }
}

impl DataClient for Feeds {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        let Some(scale) = ig_scale(interval) else {
            return Err(LiveDataError::Subscribe(format!(
                "ig streams no {interval} candles (Lightstreamer scales: 1s/1m/5m/1h; other \
                 resolutions are REST-history only)"
            )));
        };
        self.spawn(symbol, Lane::Candles { interval: interval.to_string(), scale })
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        require_live_verb(VENUE, LiveVerb::Quotes)?;
        self.spawn(symbol, Lane::Quotes)
    }

    fn subscribe_trades(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (ig live_data.trades = false): a dealer venue has no trade tape.
        require_live_verb(VENUE, LiveVerb::Trades)?;
        unreachable!("ig declares no live trades")
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (ig live_data.book = false): IG streams L1 BID/OFFER only.
        require_live_verb(VENUE, LiveVerb::Book)?;
        unreachable!("ig declares no live book")
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

/// Everything a feed thread owns.
struct FeedCtx {
    sink: Arc<dyn LiveDataSink>,
    status: Arc<Mutex<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
    config: IgConfig,
}

/// Cached Lightstreamer credentials (from one [`IgSession::login`]), reused across reconnects
/// until a `CONERR` proves them dead.
struct LsAuth {
    ws_url: String,
    user: String,
    password: String,
}

/// The whole thread body for one subscription: login/handshake in the connect closure, decode +
/// emit in `on_text`, gap disclosure in `on_session_error`, lifecycle in the shared driver.
fn feed_main(epic: String, lane: Lane, ctx: FeedCtx) {
    let knobs = market_pump_spec(VENUE).knobs();
    let opts = knobs.opts(None, None);

    let (group, schema, label) = match &lane {
        Lane::Quotes => (market_item(&epic), QUOTE_SCHEMA.join(" "), "quotes".to_string()),
        Lane::Candles { interval, scale } => {
            (chart_item(&epic, scale), CANDLE_SCHEMA.join(" "), interval.clone())
        }
    };

    // Shared across the driver's closures (all run on THIS thread; Rc is deliberate).
    let auth: Rc<RefCell<Option<LsAuth>>> = Rc::new(RefCell::new(None));
    let live = Rc::new(Cell::new(false));
    let gap_started: Rc<Cell<Option<i64>>> = Rc::new(Cell::new(None));

    let connect = {
        let auth = Rc::clone(&auth);
        let (ctx_status, ctx_config, stop) =
            (Arc::clone(&ctx.status), ctx.config.clone(), Arc::clone(&ctx.stop));
        let (group, schema) = (group.clone(), schema.clone());
        move || -> Result<LsStream, String> {
            if auth.borrow().is_none() {
                let session = IgSession::login(&ctx_config).map_err(|e| {
                    *ctx_status.lock().unwrap() = format!("IG login failed: {e}");
                    format!("login failed: {e}")
                })?;
                if session.lightstreamer_endpoint.is_empty() {
                    return Err("login returned no lightstreamerEndpoint".into());
                }
                *auth.borrow_mut() = Some(LsAuth {
                    ws_url: ls_ws_url(&session.lightstreamer_endpoint),
                    user: session.account_id.clone(),
                    password: session.ls_password(),
                });
            }
            let res = {
                let borrowed = auth.borrow();
                let a = borrowed.as_ref().expect("auth cached above");
                open_ls_session(a, &group, &schema, knobs.read_timeout, opts.connect_timeout, &stop)
            };
            match res {
                Ok(stream) => {
                    *ctx_status.lock().unwrap() = format!("IG stream up ({group})");
                    Ok(stream)
                }
                Err(HandshakeErr::Refused(msg)) => {
                    // CONERR: the cached CST/XST pair is dead (or the server refused the
                    // session) — drop it so the next attempt re-logs-in fresh.
                    *auth.borrow_mut() = None;
                    Err(msg)
                }
                Err(HandshakeErr::Transport(msg)) => Err(msg),
            }
        }
    };

    let on_text = {
        let (auth, live, gap_started) =
            (Rc::clone(&auth), Rc::clone(&live), Rc::clone(&gap_started));
        let (sink, wake, status) =
            (Arc::clone(&ctx.sink), Arc::clone(&ctx.wake), Arc::clone(&ctx.status));
        let (epic, label) = (epic.clone(), label.clone());
        let mut quote_fold = QuoteFold::new();
        let mut candle_fold = CandleFold::new();
        let mut lane_kind = match &lane {
            Lane::Quotes => LaneKind::Quotes,
            Lane::Candles { interval, .. } => LaneKind::Candles { interval: interval.clone() },
        };
        move |text: &str| -> FrameOutcome {
            let mut outcome = FrameOutcome::Ignore;
            // One WS message may bundle several CR-LF-terminated TLCP lines.
            for line in text.lines().map(str::trim_end) {
                let frame = match parse_frame(line) {
                    Some(f) => f,
                    None => continue,
                };
                match frame {
                    LsFrame::Update(u) if u.sub_id == SUB_ID => {
                        let emitted = match &mut lane_kind {
                            LaneKind::Quotes => emit_quote(&mut quote_fold, &u, &sink, &epic),
                            LaneKind::Candles { interval } => {
                                emit_candles(&mut candle_fold, &u, &sink, &epic, interval)
                            }
                        };
                        if emitted {
                            if !live.get() {
                                live.set(true);
                                *status.lock().unwrap() = format!("IG live ({epic}@{label})");
                                sink.stream_status(
                                    VENUE,
                                    &epic,
                                    &label,
                                    StreamStatus::Live { gap_started_ts_ms: gap_started.take() },
                                );
                            }
                            (wake)();
                        }
                        outcome = FrameOutcome::Confirm;
                    }
                    LsFrame::Update(_) | LsFrame::Reqok | LsFrame::Probe | LsFrame::Other(_) => {}
                    LsFrame::Subok { .. } => outcome = FrameOutcome::Confirm,
                    LsFrame::Conok { .. } => {} // handshake already consumed one; late/dup is inert
                    LsFrame::Loop => {
                        return FrameOutcome::Fatal("server requested rebind (LOOP)".into())
                    }
                    LsFrame::End { code, message } => {
                        return FrameOutcome::Fatal(format!("session ended (END,{code},{message})"))
                    }
                    LsFrame::Conerr { code, message } => {
                        *auth.borrow_mut() = None;
                        return FrameOutcome::Fatal(format!("CONERR,{code},{message}"));
                    }
                    LsFrame::Reqerr { req_id, code, message } => {
                        return FrameOutcome::Fatal(format!(
                            "subscribe refused (REQERR,{req_id},{code},{message})"
                        ))
                    }
                }
            }
            outcome
        }
    };

    let on_session_error = {
        let (live, gap_started) = (Rc::clone(&live), Rc::clone(&gap_started));
        let (sink, wake, status) =
            (Arc::clone(&ctx.sink), Arc::clone(&ctx.wake), Arc::clone(&ctx.status));
        let (epic, label) = (epic.clone(), label.clone());
        move |e: &str| {
            tracing::warn!(target: "vike_ig::market_feed", %epic, %label, error = %e, "IG market stream dropped; reconnecting");
            *status.lock().unwrap() = format!("IG stream error: {e}; reconnecting");
            if live.replace(false) {
                let at = now_ms();
                gap_started.set(Some(at));
                sink.stream_status(VENUE, &epic, &label, StreamStatus::GapStart { at_ts_ms: at });
            }
            (wake)();
        }
    };

    run_market_feed_on(connect, &opts, &ctx.stop, &now_ms, on_text, || {}, on_session_error);
}

/// The lane discriminant `on_text` matches on (owned clone of [`Lane`]'s data).
enum LaneKind {
    Quotes,
    Candles { interval: String },
}

/// Fold one update into the quote lane; returns whether anything was emitted.
fn emit_quote(
    fold: &mut QuoteFold,
    u: &crate::lightstreamer::LsUpdate,
    sink: &Arc<dyn LiveDataSink>,
    epic: &str,
) -> bool {
    let Some(q) = fold.on_update(u) else {
        return false;
    };
    let ts = now_ms();
    sink.quote(
        VENUE,
        epic,
        QuoteTick {
            ts,
            local_ts: ts,
            bid: q.bid,
            ask: q.offer,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: epic.to_string(),
        },
    );
    true
}

/// Fold one update into the candle lane; returns whether anything was emitted.
fn emit_candles(
    fold: &mut CandleFold,
    u: &crate::lightstreamer::LsUpdate,
    sink: &Arc<dyn LiveDataSink>,
    epic: &str,
    interval: &str,
) -> bool {
    let events = fold.on_update(u);
    let emitted = !events.is_empty();
    for ev in events {
        match ev {
            CandleEvent::Forming(bar) => sink.forming_bar(VENUE, epic, interval, bar),
            CandleEvent::Closed(bar) => {
                sink.bar_close_tick(VENUE, epic, bar.close, bar.ts);
                sink.close_bar(VENUE, epic, interval, bar);
            }
        }
    }
    emitted
}

/// Why an attempted TLCP session did not come up.
enum HandshakeErr {
    /// The server answered `CONERR` — the credentials (or session request) were refused; the
    /// cached tokens must be dropped.
    Refused(String),
    /// Anything transport-shaped (dial/read/timeout) — retry with the same tokens.
    Transport(String),
}

/// Dial the Lightstreamer WS, run the TLCP handshake (`create_session` → `CONOK`) and send the
/// one `control LS_op=add` subscribe. Returns the stream ready for the driver's read loop (the
/// `SUBOK`/first-`U` ack is observed there).
fn open_ls_session(
    auth: &LsAuth,
    group: &str,
    schema: &str,
    read_timeout: Duration,
    connect_timeout: Option<Duration>,
    stop: &AtomicBool,
) -> Result<LsStream, HandshakeErr> {
    let sock = dial_ls(&auth.ws_url, connect_timeout).map_err(HandshakeErr::Transport)?;
    configure_ws_stream(&sock, read_timeout);
    let mut stream = LsStream { inner: TungsteniteStream(sock), last_rx: Instant::now() };

    stream
        .send_text(&create_session_request(&auth.user, &auth.password))
        .map_err(|e| HandshakeErr::Transport(format!("create_session send: {e:?}")))?;

    // Wait for CONOK/CONERR (read ticks bounded by HANDSHAKE_BUDGET, stop-aware).
    let deadline = Instant::now() + HANDSHAKE_BUDGET;
    'conok: loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(HandshakeErr::Transport("stop requested mid-handshake".into()));
        }
        if Instant::now() >= deadline {
            return Err(HandshakeErr::Transport("no CONOK within handshake budget".into()));
        }
        let text = match stream.read_frame() {
            Ok(t) => t,
            Err(StreamError::Timeout) => continue,
            Err(StreamError::Closed(m)) => {
                return Err(HandshakeErr::Transport(format!("closed during handshake: {m}")))
            }
        };
        for line in text.lines().map(str::trim_end) {
            match parse_frame(line) {
                Some(LsFrame::Conok { .. }) => break 'conok,
                Some(LsFrame::Conerr { code, message }) => {
                    return Err(HandshakeErr::Refused(format!("CONERR,{code},{message}")))
                }
                Some(LsFrame::End { code, message }) => {
                    return Err(HandshakeErr::Transport(format!("END,{code},{message}")))
                }
                _ => {} // SERVNAME/CLIENTIP/CONS/... — inert preamble
            }
        }
    }

    stream
        .send_text(&subscribe_request(1, SUB_ID, "MERGE", group, schema, true))
        .map_err(|e| HandshakeErr::Transport(format!("subscribe send: {e:?}")))?;
    Ok(stream)
}

/// `https://apd.marketdatasystems.com` (the login's `lightstreamerEndpoint`) → the TLCP WS URL.
fn ls_ws_url(endpoint: &str) -> String {
    let e = endpoint.trim_end_matches('/');
    let (scheme, rest) = match e.split_once("://") {
        Some(("http", r)) => ("ws", r),
        Some((_, r)) => ("wss", r),
        None => ("wss", e),
    };
    format!("{scheme}://{rest}/lightstreamer")
}

/// Dial the Lightstreamer WS with the mandatory TLCP subprotocol header, ALWAYS under a connect
/// bound.
///
/// ⚠ **This cannot call `vike_bridge_core::ws_proxy::connect_ws`, and the reason is a header.**
/// TLCP 2.1.0 requires the WS handshake to carry
/// `Sec-WebSocket-Protocol: TLCP-2.1.0.lightstreamer.com`; `connect_ws` takes a URL and builds its
/// own request, so it has nowhere to put one. Extending it would mean editing a shared home every
/// venue dials through for one venue's protocol quirk, which the shared-home rule forbids in
/// passing. So the BOUND is reproduced here rather than the dial being re-spelled loosely:
/// `connect_timeout` is spent across name resolution AND every address the name resolves to, in
/// order — the same accounting `connect_ws`'s bounded arm does, and for the same reason (a
/// per-address bound would make the true ceiling `n x window` for an `n` DNS picks).
///
/// There is deliberately **no unbounded arm**. A `None` row falls back to
/// [`vike_bridge_core::pump_spec::CONNECT_10S`] rather than to a bare `tungstenite::connect`,
/// because that call applies no connect bound at all: a feed thread whose SYNs are black-holed
/// ignores its stop flag for the OS's own SYN ladder (~127 s on Linux defaults) with the flag
/// already raised. `crates/vike-ops/tests/feed_stop_windows_gate.rs`'s
/// `no_feed_path_file_opens_an_unbounded_window` is the gate, and it caught exactly that arm here.
///
/// Residual, shared with `connect_ws`: the bound covers the TCP phase only — the TLS + WS
/// handshake that follows is not inside it.
fn dial_ls(ws_url: &str, connect_timeout: Option<Duration>) -> Result<WsSocket, String> {
    let bound = connect_timeout.unwrap_or(vike_bridge_core::pump_spec::CONNECT_10S);
    let mut req = ws_url.into_client_request().map_err(|e| format!("bad LS url {ws_url}: {e}"))?;
    req.headers_mut().insert("Sec-WebSocket-Protocol", HeaderValue::from_static(LS_WS_SUBPROTOCOL));

    let (host, port) = ws_host_port(ws_url)?;
    let started = Instant::now();
    let addrs: Vec<_> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}:{port}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("resolve {host}:{port}: no addresses"));
    }
    let mut last_err = String::new();
    for addr in addrs {
        let Some(remaining) = bound.checked_sub(started.elapsed()) else { break };
        match TcpStream::connect_timeout(&addr, remaining) {
            Ok(tcp) => return Ok(tungstenite::client_tls(req, tcp).map_err(|e| e.to_string())?.0),
            Err(e) => last_err = format!("{addr}: {e}"),
        }
    }
    Err(format!("dial {host}:{port} exhausted its {bound:?} connect bound ({last_err})"))
}

/// Host + port of a `ws(s)://host[:port]/…` URL (default 443, or 80 for plain `ws://`).
fn ws_host_port(url: &str) -> Result<(String, u16), String> {
    let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("no scheme in {url}"))?;
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    match authority.split_once(':') {
        Some((h, p)) => Ok((h.to_string(), p.parse().map_err(|e| format!("bad port: {e}"))?)),
        None => Ok((authority.to_string(), if scheme == "ws" { 80 } else { 443 })),
    }
}

/// tungstenite [`MarketStream`] for the driver (the same wrap as the shared driver's private
/// stream): a Ping is auto-ponged and looped past; a Close surfaces as `Err(Closed)`; every
/// inbound frame stamps `last_rx` for the idle watchdog.
struct LsStream {
    inner: TungsteniteStream,
    last_rx: Instant,
}

impl LsStream {
    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
        self.inner.send_text(s)
    }
}

impl MarketStream for LsStream {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        loop {
            match self.inner.recv() {
                Ok(StreamMsg::Text(t)) => {
                    self.last_rx = Instant::now();
                    return Ok(t);
                }
                Ok(StreamMsg::Ping(p)) => {
                    self.last_rx = Instant::now();
                    self.inner.pong(p)?;
                }
                Ok(StreamMsg::Other) => self.last_rx = Instant::now(),
                Err(e) => return Err(e),
            }
        }
    }

    fn send_text(&mut self, s: &str) -> Result<(), StreamError> {
        self.inner.send_text(s)
    }

    fn since_last_frame(&self) -> Duration {
        self.last_rx.elapsed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_ws_url_rewrites_scheme_and_appends_path() {
        assert_eq!(
            ls_ws_url("https://demo-apd.marketdatasystems.com"),
            "wss://demo-apd.marketdatasystems.com/lightstreamer"
        );
        assert_eq!(
            ls_ws_url("https://apd.marketdatasystems.com:443/"),
            "wss://apd.marketdatasystems.com:443/lightstreamer"
        );
        assert_eq!(ls_ws_url("http://local:8080"), "ws://local:8080/lightstreamer");
        assert_eq!(ls_ws_url("push.ig.com"), "wss://push.ig.com/lightstreamer");
    }

    #[test]
    fn ws_host_port_parses_defaults_and_explicit_ports() {
        assert_eq!(
            ws_host_port("wss://demo-apd.marketdatasystems.com/lightstreamer").unwrap(),
            ("demo-apd.marketdatasystems.com".to_string(), 443)
        );
        assert_eq!(ws_host_port("wss://h:8443/x").unwrap(), ("h".to_string(), 8443));
        assert_eq!(ws_host_port("ws://h/x").unwrap(), ("h".to_string(), 80));
        assert!(ws_host_port("no-scheme").is_err());
    }
}
