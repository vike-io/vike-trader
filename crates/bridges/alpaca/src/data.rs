//! Alpaca market-data WS -> `vike_data::live::{DataClient, LiveDataSink}` (Task 11, Phase 2).
//!
//! This file has ONE pure half ([`decode_ws_message`], fixture-tested in `tests/mapper_data.rs`,
//! no network) and one impure half ([`AlpacaDataClient`], the WS connect/auth/subscribe/read loop).
//!
//! **One shared connection per asset-class stream (verified live 2026-07-14).** Alpaca's
//! market-data WS enforces ONE connection per key per stream — a 2nd concurrent connection to the
//! same endpoint returns `{"T":"error","code":406,"msg":"connection limit exceeded"}`. But the two
//! streams are SEPARATE endpoints, so a crypto and an equity connection on the same OAuth key run
//! concurrently. So this client keeps at most one shared connection per [`StreamRoute`], each
//! multiplexing ALL its symbols/verbs: `subscribe_*` resolves the symbol's asset class and routes
//! on THAT ([`StreamRoute::for_class`]), lazily opening that route's connection on first use and
//! sending incremental `subscribe`/`unsubscribe`
//! frames over a command channel; the connection is torn down when its last subscription drops.
//! Each connection re-sends every active subscription after a reconnect, so a drop doesn't silently
//! lose a feed.
//!
//! **WS endpoints** (`AlpacaConfig::hosts.data_ws` resolves sandbox-vs-live, see `hosts.rs`): crypto
//! ⇒ `{data_ws}/v1beta3/crypto/us`; equities ⇒ `{data_ws}/v2/iex` — the free IEX feed. `sip` needs
//! a paid entitlement (the sandbox OAuth token gets `409 insufficient subscription` on `/v2/sip`),
//! so `iex` is used for both sandbox and live (verified live 2026-07-14).
//!
//! **WS OAuth auth handshake (verified live 2026-07-14 against the sandbox crypto stream).**
//! Alpaca's market-data WS authenticates OAuth integrations with the literal key `"oauth"` and the
//! OAuth Bearer as the `secret`: `{"action":"auth","key":"oauth","secret":"<bearer>"}` → the server
//! replies `{"T":"success","msg":"authenticated"}` then the subscription confirmation, and live
//! `q`/`t`/`b` frames flow. (The earlier `{"action":"auth","token":...}` guess and passing the
//! client_id/secret as key/secret both return `{"T":"error","code":402,"msg":"auth failed"}`.) The
//! same Bearer [`crate::auth::TokenSource`] mints for every other plane (REST broker, REST data,
//! SSE trade events) is reused here. See [`auth_frame`].
//!
//! Alpaca WS bars are fixed 1-minute closed bars (no intrabar "forming" frame is published), so
//! every decoded bar maps to `interval = "1m"`, `closed = true`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use tungstenite::Message;
use vike_bridge_core::ws::{configure_ws_stream, is_timeout};
use vike_data::live::{DataClient, LiveDataError, LiveDataSink, SubscriptionId, require_live_verb};
use vike_model::{AssetClass, Bar, LiveVerb, QuoteTick, TradeTick};

use crate::auth::TokenSource;
use crate::config::AlpacaConfig;

const VENUE: &str = "alpaca";
/// Stop-flag poll cadence (matches the binance/OKX/Bybit feed convention).
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// Reconnect backoff after a socket error, in stop-aware 100ms slices (3s total).
const RECONNECT_BACKOFF_SLICES: u32 = 30;

/// One decoded Alpaca market-data WS payload, mapped onto vike's wire types. Control frames
/// (`success`/`error`/`subscription` acks) decode to nothing — see [`decode_ws_message`].
#[derive(Debug, Clone)]
pub enum DataMsg {
    /// A closed 1-minute bar (`T:"b"`). Alpaca publishes no intrabar/forming bar frame, so
    /// `closed` is always `true` today; the field exists so a future forming-bar source (if
    /// Alpaca ever adds one) doesn't need a shape change here.
    Bar { symbol: String, interval: String, bar: Bar, closed: bool },
    /// An L1 quote update (`T:"q"`).
    Quote { symbol: String, q: QuoteTick },
    /// An executed-trade print (`T:"t"`).
    Trade { symbol: String, t: TradeTick },
}

/// Decode one Alpaca market-data WS frame — a JSON array of message objects — into the mapped
/// [`DataMsg`]s it carries. Pure (no I/O); fixture-tested in `tests/mapper_data.rs`. Unknown/
/// malformed elements (including the `success`/`error`/`subscription` control acks, and a
/// non-JSON-array frame) are silently skipped rather than erroring — a single bad element in a
/// batch must not drop the rest.
///
/// `venue_prefix` names which feed this frame came from (`'c'` crypto, `'s'` equities) — reserved
/// for a future symbol-namespacing need (e.g. disambiguating a ticker that exists on both feeds);
/// today's mapping doesn't depend on it, since Alpaca's `T`/`S` fields are already unambiguous
/// within one connection.
pub fn decode_ws_message(venue_prefix: char, text: &str) -> Vec<DataMsg> {
    let _ = venue_prefix;
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(text) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for v in &arr {
        let Some(msg_type) = v.get("T").and_then(|t| t.as_str()) else { continue };
        let Some(symbol) = v.get("S").and_then(|s| s.as_str()) else { continue };
        let ts = v.get("t").and_then(|t| t.as_str()).map(parse_alpaca_ts_ms).unwrap_or(0);
        match msg_type {
            "b" => {
                let (Some(o), Some(h), Some(l), Some(c)) = (
                    v.get("o").and_then(serde_json::Value::as_f64),
                    v.get("h").and_then(serde_json::Value::as_f64),
                    v.get("l").and_then(serde_json::Value::as_f64),
                    v.get("c").and_then(serde_json::Value::as_f64),
                ) else {
                    continue;
                };
                let volume = v.get("v").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let bar = Bar {
                    ts,
                    open: o,
                    high: h,
                    low: l,
                    close: c,
                    volume,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some(symbol.to_string()),
                };
                out.push(DataMsg::Bar {
                    symbol: symbol.to_string(),
                    interval: "1m".to_string(),
                    bar,
                    closed: true,
                });
            }
            "q" => {
                let (Some(bp), Some(ap)) = (
                    v.get("bp").and_then(serde_json::Value::as_f64),
                    v.get("ap").and_then(serde_json::Value::as_f64),
                ) else {
                    continue;
                };
                let bid_size = v.get("bs").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let ask_size = v.get("as").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let q = QuoteTick {
                    ts,
                    local_ts: 0,
                    bid: bp,
                    ask: ap,
                    bid_size,
                    ask_size,
                    symbol: symbol.to_string(),
                };
                out.push(DataMsg::Quote { symbol: symbol.to_string(), q });
            }
            "t" => {
                let Some(p) = v.get("p").and_then(serde_json::Value::as_f64) else { continue };
                let size = v.get("s").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
                let t = TradeTick {
                    ts,
                    local_ts: 0,
                    price: p,
                    size,
                    // Alpaca's trade message carries no maker/taker side flag.
                    is_buyer_maker: false,
                    symbol: symbol.to_string(),
                };
                out.push(DataMsg::Trade { symbol: symbol.to_string(), t });
            }
            // "success" / "error" / "subscription" control acks, or any other verb — ignored.
            _ => continue,
        }
    }
    out
}

/// Tolerant RFC3339-UTC -> epoch-ms; folds to `0` on any parse failure (the same "core stamps its
/// own receive time" fallback `event_mapper::parse_ts_ms` uses elsewhere in this crate). Dependency-free
/// fixed-field parse mirroring `vike_polymarket::gamma`'s `parse_rfc3339_utc_ms`, reusing the
/// shared civil-calendar helper [`vike_model::time::days_from_civil`] rather than pulling in
/// `chrono` for this one call site.
fn parse_alpaca_ts_ms(s: &str) -> i64 {
    parse_rfc3339_ms(s).unwrap_or(0)
}

/// Tolerant fixed-field RFC3339-UTC -> epoch-ms parser (dependency-free, no `chrono`). Shared with
/// `crate::event_mapper::parse_ts_ms`, which prefers this for Alpaca's SSE `timestamp` field (a real
/// RFC3339 string, e.g. `"2026-07-14T05:39:31.4Z"`) before falling back to a bare numeric-epoch-ms
/// parse.
pub(crate) fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once('T').or_else(|| s.split_once(' '))?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut tp = time.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    // seconds may carry a `.fff...` fraction and/or a `Z`/`+HH:MM` suffix.
    let sec_field = tp.next()?;
    let sec_str: String = sec_field.chars().take_while(|c| c.is_ascii_digit()).collect();
    let se: i64 = sec_str.parse().ok()?;
    let ms: i64 = sec_field
        .split_once('.')
        .map(|(_, frac)| {
            let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).take(3).collect();
            format!("{digits:0<3}").parse().unwrap_or(0)
        })
        .unwrap_or(0);
    if !(0..=23).contains(&h) || !(0..=59).contains(&mi) || !(0..=60).contains(&se) {
        return None;
    }
    let days = vike_model::time::days_from_civil(y, mo as u32, d as u32);
    Some((((days * 24 + h) * 60 + mi) * 60 + se) * 1000 + ms)
}

/// A market-data subscribe verb. All verbs for a route multiplex over that route's one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Verb {
    Bars,
    Quotes,
    Trades,
}

impl Verb {
    /// The Alpaca subscribe/unsubscribe channel key for this verb.
    fn channel(self) -> &'static str {
        match self {
            Verb::Bars => "bars",
            Verb::Quotes => "quotes",
            Verb::Trades => "trades",
        }
    }
}

/// The two Alpaca market-data streams. Each is a SEPARATE endpoint with its own one-connection-per-
/// key limit, so the client keeps at most one shared connection per route (see the module doc).
///
/// ⚠ **This is a ROUTE, not a taxonomy, and it was called `AssetClass` until it was renamed.** With
/// two variants spelling the two classes this venue lists, it read as a second, venue-local copy of
/// the workspace vocabulary — `docs/decisions/0061` names that shadowing as one of this axis's
/// partial encodings. It never was one. It answers "which WS URL does this subscription open", a
/// question with two answers here and NO answer for a class Alpaca does not stream; the taxonomy
/// answers "what kind of instrument is this", a question with eleven. [`StreamRoute::for_class`] is
/// the seam between them, so the class is what DECIDES and this enum is only what is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum StreamRoute {
    Crypto,
    Equity,
}

impl StreamRoute {
    /// Which stream serves an instrument of `class`, or `None` when Alpaca streams none.
    ///
    /// The route is taken FROM THE CLASS rather than from the shape of the symbol string — the
    /// implicit encoding `docs/decisions/0061` exists to remove. What this venue LISTS is declared
    /// by `crates/bridges/alpaca/src/catalog.rs`'s `asset_classes` (`Equity` + `CryptoSpot`), and
    /// those are the two that route. [`AssetClass::Etf`] joins the equity arm because the equity
    /// stream is IEX's tape, which carries listed ETFs — and because Alpaca's own `/v1/assets` returns
    /// `class == "us_equity"` for an ETF, so `parse_assets` in that same file hands one back as
    /// `Equity` and this arm only ever matters for a class threaded in from another source.
    ///
    /// Everything else is `None`: Alpaca lists no perp, no future, no option, no FX, no index and
    /// no CFD, so there is no endpoint to pick and guessing one would open a socket that
    /// authenticates and then answers silence (the failure this crate's `CLAUDE.md` records for a
    /// mis-routed symbol). The match is exhaustive rather than `_`-terminated so a new variant in
    /// the taxonomy is a compile error HERE, where somebody must decide what Alpaca does with it.
    fn for_class(class: AssetClass) -> Option<StreamRoute> {
        match class {
            AssetClass::CryptoSpot => Some(StreamRoute::Crypto),
            AssetClass::Equity | AssetClass::Etf => Some(StreamRoute::Equity),
            AssetClass::CryptoPerp
            | AssetClass::CryptoFuture
            | AssetClass::Option
            | AssetClass::Fx
            | AssetClass::Future
            | AssetClass::Index
            | AssetClass::PredictionMarket
            | AssetClass::Cfd => None,
        }
    }
}

/// The asset class of an Alpaca symbol, GUESSED from its shape: a slash-delimited pair
/// (`"BTC/USD"`) is crypto spot, anything else is an equity.
///
/// ⚠ **This is the guess `docs/decisions/0061` wants gone, and it is still here because the class
/// is not available where it is called.** [`DataClient::subscribe_bars`] and its `subscribe_quotes`
/// / `subscribe_trades` siblings take a symbol and an interval and nothing else, so this file has
/// no class to route on. The value EXISTS and stops one call short:
/// `crates/vike-app-core/src/feed_lifecycle.rs`'s `SeriesSpec` carries an
/// `Option<vike_model::AssetClass>` all the way into `ensure_feed_on`, which spends it on
/// `venue_bar_instrument` and then calls `subscribe_bars` without it. Threading it would mean
/// widening that trait's signature to carry an `Option<AssetClass>` — `Option`, because the
/// quick-pick path genuinely has no catalog hit behind it — and falling back to this function
/// only for the `None` side. Until then the guess is quarantined HERE, answering in the shared
/// vocabulary rather than in a venue-local enum, and [`StreamRoute::for_class`] — not this
/// function — picks the endpoint.
///
/// ⚠ It routes on the symbol VERBATIM and never normalizes: hand it the vike spelling of a crypto
/// pair (`"BTCUSD"`) and it answers `Equity`, which subscribes a ticker IEX does not carry. This
/// crate's `CLAUDE.md` carries what that costs and which smoke encodes it.
fn class_of(symbol: &str) -> AssetClass {
    if symbol.contains('/') { AssetClass::CryptoSpot } else { AssetClass::Equity }
}

fn ws_url(data_ws: &str, route: StreamRoute) -> String {
    match route {
        StreamRoute::Crypto => format!("{data_ws}/v1beta3/crypto/us"),
        // IEX (free) — `sip` needs a paid entitlement (`409 insufficient subscription`).
        StreamRoute::Equity => format!("{data_ws}/v2/iex"),
    }
}

fn auth_frame(bearer: &str) -> serde_json::Value {
    // OAuth market-data WS handshake, verified live 2026-07-14: key = literal "oauth", secret =
    // the OAuth Bearer. `{"action":"auth","token":..}` returns 402 "auth failed".
    serde_json::json!({"action": "auth", "key": "oauth", "secret": bearer})
}

/// Build a `{"action":<action>,"<channel>":["<symbol>"]}` frame. `action` is `"subscribe"` or
/// `"unsubscribe"`; the channel key is dynamic (`verb.channel()`), so it's built on a `Map` rather
/// than via `json!` (which needs a literal key).
fn sub_action(action: &str, verb: Verb, symbol: &str) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("action".to_string(), serde_json::Value::String(action.to_string()));
    m.insert(verb.channel().to_string(), serde_json::json!([symbol]));
    serde_json::Value::Object(m)
}

/// A subscribe/unsubscribe request sent to a route's connection thread over the command channel.
enum ConnCmd {
    Sub(Verb, String),
    Unsub(Verb, String),
}

/// The wire action a [`SubRegistry`] op implies.
#[derive(Debug, PartialEq, Eq)]
enum WireAction {
    /// Send `{"action":"subscribe",...}` — this `(verb, symbol)` pair just became live.
    Subscribe,
    /// Send `{"action":"unsubscribe",...}` — this pair's LAST subscriber dropped (connection stays).
    Unsubscribe,
    /// No wire frame: a redundant add (pair already live) or a non-final remove.
    None,
}

/// Per-connection subscription bookkeeping. Reference-counts each `(verb, symbol)` so a wire
/// subscribe/unsubscribe frame is sent only on the FIRST add / LAST remove of that pair — two
/// `SubscriptionId`s for the same instrument+verb must not let one's `unsubscribe` kill the other's
/// feed. Also tracks the connection-wide total, so the connection is torn down when its last
/// subscription drops. Pure — unit-tested below.
#[derive(Default)]
struct SubRegistry {
    total: usize,
    per: HashMap<(Verb, String), usize>,
}

impl SubRegistry {
    /// Record a subscription; `Subscribe` iff this `(verb, symbol)` just went from 0 → 1.
    fn add(&mut self, verb: Verb, symbol: String) -> WireAction {
        self.total += 1;
        let n = self.per.entry((verb, symbol)).or_insert(0);
        *n += 1;
        if *n == 1 { WireAction::Subscribe } else { WireAction::None }
    }

    /// Drop one subscription. Returns `(wire action, connection-now-empty)`. When the connection is
    /// now empty the caller tears it down (so no `Unsubscribe` frame is needed); otherwise
    /// `Unsubscribe` iff this pair's refcount just hit 0.
    fn remove(&mut self, verb: Verb, symbol: &str) -> (WireAction, bool) {
        self.total = self.total.saturating_sub(1);
        let key = (verb, symbol.to_string());
        let last_pair = match self.per.get_mut(&key) {
            Some(n) => {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    self.per.remove(&key);
                    true
                } else {
                    false
                }
            }
            None => false,
        };
        let teardown = self.total == 0;
        let action =
            if teardown || !last_pair { WireAction::None } else { WireAction::Unsubscribe };
        (action, teardown)
    }
}

/// The controller-side handle to one route's connection thread.
struct Conn {
    cmd_tx: Sender<ConnCmd>,
    stop: Arc<AtomicBool>,
    join: std::thread::JoinHandle<()>,
    /// per-`(verb, symbol)` + connection-wide refcounts; the connection is torn down at total 0.
    reg: SubRegistry,
}

/// The thread-side context for one route's connection.
struct ConnCtx {
    route: StreamRoute,
    sink: Arc<dyn LiveDataSink>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
    token: Arc<TokenSource>,
    data_ws: String,
    cmd_rx: Receiver<ConnCmd>,
}

/// Fan one decoded WS text frame out to the sink, then nudge the GUI.
fn dispatch(ctx: &ConnCtx, prefix: char, txt: &str) {
    for m in decode_ws_message(prefix, txt) {
        match m {
            DataMsg::Bar { symbol, interval, bar, closed } => {
                // `bar_close_tick`, NOT `mark_tick`: a candle close is not a venue mark
                // (mark-slot semantics); alpaca streams no mark price, so the bar-close slot is
                // this venue's only conflated price lane.
                ctx.sink.bar_close_tick(VENUE, &symbol, bar.close, bar.ts);
                if closed {
                    ctx.sink.close_bar(VENUE, &symbol, &interval, bar);
                } else {
                    ctx.sink.forming_bar(VENUE, &symbol, &interval, bar);
                }
            }
            DataMsg::Quote { symbol, q } => ctx.sink.quote(VENUE, &symbol, q),
            DataMsg::Trade { symbol, t } => ctx.sink.trade(VENUE, &symbol, t),
        }
    }
    (ctx.wake)();
}

/// One connection session: mint/refresh the Bearer, connect, auth, (re)subscribe everything in
/// `active`, then interleave draining the command channel (subscribe/unsubscribe frames) with
/// reading data frames until stopped or the socket errors. `active` persists across reconnects (the
/// caller in [`conn_main`] owns it) so every live subscription is re-sent on re-open — no feed is
/// silently lost to a reconnect.
fn run_conn(
    ctx: &ConnCtx,
    active: &mut HashSet<(Verb, String)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let bearer = ctx.token.bearer()?;
    let url = ws_url(&ctx.data_ws, ctx.route);
    let (mut socket, _resp) = tungstenite::connect(url.as_str())?;
    configure_ws_stream(&socket, READ_TIMEOUT);
    socket.send(Message::Text(auth_frame(&bearer).to_string().into()))?;
    for (verb, symbol) in active.iter() {
        socket.send(Message::Text(sub_action("subscribe", *verb, symbol).to_string().into()))?;
    }
    let prefix = match ctx.route {
        StreamRoute::Crypto => 'c',
        StreamRoute::Equity => 's',
    };
    loop {
        if ctx.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Drain pending subscribe/unsubscribe commands (non-blocking).
        loop {
            match ctx.cmd_rx.try_recv() {
                Ok(ConnCmd::Sub(v, s)) => {
                    active.insert((v, s.clone()));
                    socket
                        .send(Message::Text(sub_action("subscribe", v, &s).to_string().into()))?;
                }
                Ok(ConnCmd::Unsub(v, s)) => {
                    active.remove(&(v, s.clone()));
                    socket
                        .send(Message::Text(sub_action("unsubscribe", v, &s).to_string().into()))?;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()), // client dropped
            }
        }
        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) if is_timeout(&e) => continue, // stop-flag / command poll tick
            Err(e) => return Err(e.into()),
        };
        match msg {
            Message::Text(txt) => dispatch(ctx, prefix, txt.as_str()),
            Message::Ping(p) => socket.send(Message::Pong(p))?,
            Message::Close(_) => return Err("server closed".into()),
            _ => {}
        }
    }
}

/// Reconnect loop for one route's connection: retry [`run_conn`] (stop-aware backoff) until `stop`
/// is raised or a requested stop returns `Ok(())`. `active` (the set of live subscriptions) lives
/// here so it survives reconnects, seeded with the subscription that first opened the connection.
fn conn_main(ctx: ConnCtx, initial: (Verb, String)) {
    let mut active: HashSet<(Verb, String)> = HashSet::new();
    active.insert(initial);
    while !ctx.stop.load(Ordering::Relaxed) {
        match run_conn(&ctx, &mut active) {
            Ok(()) => break, // requested stop / client dropped
            Err(_e) => {
                // `_e` is intentionally not interpolated: it may wrap the connect URL/response,
                // never a secret. A generic boundary log matches the OANDA sibling's density.
                tracing::warn!(
                    venue = "alpaca",
                    route = ?ctx.route,
                    "market-data WS reconnect: connection lost"
                );
            }
        }
        for _ in 0..RECONNECT_BACKOFF_SLICES {
            if ctx.stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Live Alpaca market-data client. Keeps at most one shared, multiplexed WS connection per
/// [`StreamRoute`] (Alpaca allows one connection per key per stream — see the module doc). `sink`
/// receives every bar/quote/trade; `wake` is the GUI repaint nudge (pass `|| {}` headless).
pub struct AlpacaDataClient {
    sink: Arc<dyn LiveDataSink>,
    wake: Arc<dyn Fn() + Send + Sync>,
    token: Arc<TokenSource>,
    data_ws: String,
    next_id: u64,
    conns: HashMap<StreamRoute, Conn>,
    subs: HashMap<SubscriptionId, (StreamRoute, Verb, String)>,
}

impl AlpacaDataClient {
    pub fn new(
        config: AlpacaConfig,
        sink: Arc<dyn LiveDataSink>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let token = Arc::new(TokenSource::new(
            config.client_id.clone(),
            config.client_secret.clone(),
            config.hosts.authx.to_string(),
        ));
        Self {
            sink,
            wake: Arc::new(wake),
            token,
            data_ws: config.hosts.data_ws.to_string(),
            next_id: 0,
            conns: HashMap::new(),
            subs: HashMap::new(),
        }
    }

    /// Route `(symbol, verb)` to its stream's connection — lazily opening the connection (seeding
    /// it with this subscription) or sending an incremental subscribe frame to an existing one.
    ///
    /// The endpoint is decided by the ASSET CLASS ([`StreamRoute::for_class`]); only the class
    /// itself is still read off the symbol string, because the `DataClient` seam carries no class
    /// — [`class_of`] argues that and names what would have to be threaded.
    fn subscribe(&mut self, symbol: &str, verb: Verb) -> Result<SubscriptionId, LiveDataError> {
        let class = class_of(symbol);
        // Unreachable while [`class_of`] is the only source of `class` — it answers with one of
        // the two classes this venue lists, and both route. The arm stops being dead the day a
        // real class is threaded in, which is the point: an unstreamable class must REFUSE rather
        // than fall into whichever endpoint a string test happened to pick.
        let Some(route) = StreamRoute::for_class(class) else {
            return Err(LiveDataError::Unsupported(
                "alpaca streams no market data for this asset class",
            ));
        };
        let sym = symbol.to_string();
        match self.conns.get_mut(&route) {
            Some(conn) => {
                // Only send a wire subscribe when this (verb, symbol) first goes live on the
                // connection; a duplicate subscribe just bumps the refcount.
                if conn.reg.add(verb, sym.clone()) == WireAction::Subscribe
                    && let Err(e) = conn.cmd_tx.send(ConnCmd::Sub(verb, sym.clone()))
                {
                    // the connection thread is gone — drop the dead handle so a later
                    // subscribe respawns it cleanly, and don't record this subscription.
                    self.conns.remove(&route);
                    return Err(LiveDataError::Subscribe(format!("alpaca {sym} {verb:?}: {e}")));
                }
            }
            None => {
                let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
                let stop = Arc::new(AtomicBool::new(false));
                let ctx = ConnCtx {
                    route,
                    sink: Arc::clone(&self.sink),
                    wake: Arc::clone(&self.wake),
                    stop: Arc::clone(&stop),
                    token: Arc::clone(&self.token),
                    data_ws: self.data_ws.clone(),
                    cmd_rx,
                };
                let initial = (verb, sym.clone());
                let join = std::thread::Builder::new()
                    .name(format!("alpaca-data-{route:?}"))
                    .spawn(move || conn_main(ctx, initial))
                    .map_err(|e| LiveDataError::Subscribe(format!("alpaca {sym} {verb:?}: {e}")))?;
                // The connection thread subscribes `initial` itself (via its `active` set), so the
                // registry just records it — no wire command is sent for the seed.
                let mut reg = SubRegistry::default();
                reg.add(verb, sym.clone());
                self.conns.insert(route, Conn { cmd_tx, stop, join, reg });
            }
        }
        let id = SubscriptionId(self.next_id);
        self.next_id += 1;
        self.subs.insert(id, (route, verb, sym));
        Ok(id)
    }
}

impl DataClient for AlpacaDataClient {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        // Alpaca's market-data WS serves only fixed 1-minute bars.
        if interval != "1m" {
            return Err(LiveDataError::Unsupported("alpaca market-data WS serves 1m bars only"));
        }
        self.subscribe(symbol, Verb::Bars)
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(symbol, Verb::Quotes)
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subscribe(symbol, Verb::Trades)
    }

    fn subscribe_book(&mut self, _symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Caps-driven refusal (alpaca live_data.book = false): stays in lockstep with the table.
        require_live_verb(VENUE, LiveVerb::Book)?;
        unreachable!("alpaca declares no lossless book lane")
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        let Some((route, verb, sym)) = self.subs.remove(&id) else { return };
        // Send a wire unsubscribe only when this (verb, symbol)'s LAST subscriber drops (so a
        // sibling SubscriptionId on the same pair keeps its feed); tear the whole connection down
        // once its last subscription of any kind drops.
        let teardown = match self.conns.get_mut(&route) {
            Some(conn) => {
                let (action, teardown) = conn.reg.remove(verb, &sym);
                if action == WireAction::Unsubscribe {
                    let _ = conn.cmd_tx.send(ConnCmd::Unsub(verb, sym));
                }
                teardown
            }
            None => false,
        };
        if teardown && let Some(conn) = self.conns.remove(&route) {
            conn.stop.store(true, Ordering::Relaxed);
            let _ = conn.join.join();
        }
    }

    fn shutdown(&mut self) {
        for conn in self.conns.values() {
            conn.stop.store(true, Ordering::Relaxed);
        }
        for (_, conn) in self.conns.drain() {
            let _ = conn.join.join();
        }
        self.subs.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape guess answers in the SHARED vocabulary — the property that stopped this file
    /// carrying a second, venue-local `AssetClass`.
    #[test]
    fn class_guess_answers_in_the_shared_taxonomy() {
        assert_eq!(class_of("BTC/USD"), AssetClass::CryptoSpot);
        assert_eq!(class_of("ETH/USDT"), AssetClass::CryptoSpot);
        assert_eq!(class_of("AAPL"), AssetClass::Equity);
        assert_eq!(class_of("SPY"), AssetClass::Equity);
    }

    /// The route is a function of the CLASS, and the classes Alpaca does not stream route NOWHERE
    /// rather than to whichever endpoint a string test would have picked (`"BTC-PERP"` carries no
    /// slash, so the old `contains('/')` test would have opened the EQUITY socket for a perp).
    #[test]
    fn route_is_taken_from_the_class() {
        assert_eq!(StreamRoute::for_class(AssetClass::CryptoSpot), Some(StreamRoute::Crypto));
        assert_eq!(StreamRoute::for_class(AssetClass::Equity), Some(StreamRoute::Equity));
        // an ETF is `us_equity` on this venue's own `/v1/assets`, and IEX carries its tape
        assert_eq!(StreamRoute::for_class(AssetClass::Etf), Some(StreamRoute::Equity));
        for unlisted in [
            AssetClass::CryptoPerp,
            AssetClass::CryptoFuture,
            AssetClass::Option,
            AssetClass::Fx,
            AssetClass::Future,
            AssetClass::Index,
            AssetClass::PredictionMarket,
            AssetClass::Cfd,
        ] {
            assert_eq!(StreamRoute::for_class(unlisted), None, "{unlisted:?} is not listed here");
        }
    }

    /// ...and the two that DO route are exactly the two this venue's catalog declares, so the
    /// routing table and `catalog.rs`'s `asset_classes` cannot drift apart silently.
    #[test]
    fn every_class_the_catalog_declares_has_a_stream() {
        use vike_catalog::CatalogProvider;

        // Credential-less: `new` does no I/O, and `asset_classes` is the venue's static claim.
        let catalog = crate::catalog::AlpacaCatalog::new(&HashMap::new());
        for class in catalog.asset_classes().iter().copied() {
            assert!(
                StreamRoute::for_class(class).is_some(),
                "{class:?} is listed by the catalog but routes to no stream"
            );
        }
    }

    #[test]
    fn ws_urls_by_route() {
        let h = "wss://stream.data.sandbox.alpaca.markets";
        assert_eq!(ws_url(h, StreamRoute::Crypto), format!("{h}/v1beta3/crypto/us"));
        // equity must be the free IEX feed, NOT sip (sip → 409 insufficient subscription)
        assert_eq!(ws_url(h, StreamRoute::Equity), format!("{h}/v2/iex"));
    }

    #[test]
    fn sub_action_frames() {
        assert_eq!(
            sub_action("subscribe", Verb::Quotes, "BTC/USD"),
            serde_json::json!({"action": "subscribe", "quotes": ["BTC/USD"]})
        );
        assert_eq!(
            sub_action("unsubscribe", Verb::Trades, "AAPL"),
            serde_json::json!({"action": "unsubscribe", "trades": ["AAPL"]})
        );
        assert_eq!(
            sub_action("subscribe", Verb::Bars, "ETH/USD"),
            serde_json::json!({"action": "subscribe", "bars": ["ETH/USD"]})
        );
    }

    #[test]
    fn auth_frame_is_oauth_key_bearer_secret() {
        assert_eq!(
            auth_frame("tok-123"),
            serde_json::json!({"action": "auth", "key": "oauth", "secret": "tok-123"})
        );
    }

    #[test]
    fn registry_refcounts_pairs_and_connection() {
        let mut r = SubRegistry::default();
        // first subscribe to a pair → wire subscribe
        assert_eq!(r.add(Verb::Quotes, "BTC/USD".into()), WireAction::Subscribe);
        // duplicate subscribe to the SAME (verb, symbol) → no wire frame (already live)
        assert_eq!(r.add(Verb::Quotes, "BTC/USD".into()), WireAction::None);
        // a different verb on the same symbol → its own wire subscribe
        assert_eq!(r.add(Verb::Trades, "BTC/USD".into()), WireAction::Subscribe);
        // remove ONE of the two duplicate quote subs → not the last for the pair → no unsub/teardown
        assert_eq!(r.remove(Verb::Quotes, "BTC/USD"), (WireAction::None, false));
        // remove the LAST quote sub → wire unsubscribe; trades still live so no teardown
        assert_eq!(r.remove(Verb::Quotes, "BTC/USD"), (WireAction::Unsubscribe, false));
        // remove the trades sub → last pair AND last on the connection → teardown, no unsub frame
        assert_eq!(r.remove(Verb::Trades, "BTC/USD"), (WireAction::None, true));
    }

    /// `subscribe_book` refuses THROUGH `require_live_verb` (driven off
    /// `VenueCaps.live_data.book` = false) rather than a hand-rolled message, so its refusal stays
    /// pinned to the declared matrix. Same observable outcome (an `Unsupported` variant), asserted
    /// here on a client built with a stub config + no-op sink (no network — the refusal short-
    /// circuits before any connection).
    #[test]
    fn subscribe_book_is_caps_refused() {
        use crate::hosts::hosts_for;
        use vike_bridge_core::credentials::Environment;

        struct NoopSink;
        impl LiveDataSink for NoopSink {
            fn seed_bars(&self, _v: &str, _s: &str, _i: &str, _b: Vec<Bar>) {}
            fn close_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
            fn forming_bar(&self, _v: &str, _s: &str, _i: &str, _b: Bar) {}
            fn mark_tick(&self, _v: &str, _s: &str, _px: f64, _ts: i64) {}
            fn quote(&self, _v: &str, _s: &str, _q: QuoteTick) {}
            fn trade(&self, _v: &str, _s: &str, _t: TradeTick) {}
            fn book(&self, _v: &str, _s: &str, _b: std::sync::Arc<vike_model::L2Book>) {}
        }

        let env = Environment::Sim;
        let config = AlpacaConfig {
            client_id: String::new(),
            client_secret: String::new(),
            account_id: String::new(),
            env,
            hosts: hosts_for(env),
        };
        let mut client = AlpacaDataClient::new(config, Arc::new(NoopSink), || {});
        assert!(matches!(client.subscribe_book("BTC/USD"), Err(LiveDataError::Unsupported(_))));
    }

    #[test]
    fn registry_distinct_symbols_are_independent() {
        let mut r = SubRegistry::default();
        assert_eq!(r.add(Verb::Quotes, "BTC/USD".into()), WireAction::Subscribe);
        assert_eq!(r.add(Verb::Quotes, "ETH/USD".into()), WireAction::Subscribe);
        // dropping one symbol unsubscribes only it; the other keeps the connection alive
        assert_eq!(r.remove(Verb::Quotes, "BTC/USD"), (WireAction::Unsubscribe, false));
        assert_eq!(r.remove(Verb::Quotes, "ETH/USD"), (WireAction::None, true));
    }

    #[test]
    fn registry_remove_unknown_is_safe() {
        let mut r = SubRegistry::default();
        assert_eq!(r.remove(Verb::Bars, "AAPL"), (WireAction::None, true));
    }
}
