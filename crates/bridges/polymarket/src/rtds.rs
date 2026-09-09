//! Polymarket **RTDS** (Real-Time Data Service) — the keyless WS carrying the UNDERLYING reference
//! prices this venue's crypto/equity binary markets are struck on ("will BTC be above $X at 4pm"),
//! mapped onto the live seam as a REFERENCE/MARK series ([`vike_data::LiveDataSink::mark_tick`]).
//!
//! This is NOT the CLOB market channel (`market_feed`, `WS_MARKET`): that one carries the
//! outcome tokens' own books and prints. RTDS carries the underlying the market RESOLVES against,
//! which is exactly the series a strike-relative strategy needs and which the CLOB never publishes.
//! `mark_tick` is the right verb by the seam's own contract — a venue-published valuation price,
//! filed in the `PriceBoard` MARK slot — and NOT `bar_close_tick` (there is no candle here).
//!
//! **Endpoint: VERIFIED live 2026-07-22** (probed through the Dublin host — the endpoint
//! uncertainty this module used to flag is RESOLVED). `wss://ws-live-data.polymarket.com`
//! ([`crate::RTDS_WS`]) is correct and KEYLESS — no credentials for any of the three
//! topics below. Everything remains an overridable [`RtdsConfig`] field anyway.
//!
//! **Topics** (exact wire names): [`TOPIC_CRYPTO_PRICES`] (`crypto_prices`, Binance-sourced),
//! [`TOPIC_CRYPTO_PRICES_CHAINLINK`] (`crypto_prices_chainlink`, Chainlink-sourced — its symbols
//! carry a slash, `btc/usd`), and [`TOPIC_EQUITY_PRICES`] (`equity_prices`, Pyth-sourced:
//! stocks/ETFs/forex/metals/commodities).
//!
//! **Protocol.** One subscribe frame per session, replayed verbatim on every reconnect by the
//! shared driver: `{"action":"subscribe","subscriptions":[{"topic":"crypto_prices","type":"update"}]}`
//! ([`rtds_subscribe_message`]).
//!
//! **⚠ FILTERS — the documented CSV form is a SILENT-FAILURE TRAP.** The comma-separated
//! `"filters":"btcusdt,ethusdt"` shape the docs show returns ZERO messages, forever, while the
//! socket stays perfectly healthy — no error, no close, just silence. Only two forms work and this
//! module emits only those: NO `filters` key at all (⇒ every symbol streams, the shape
//! [`RtdsConfig::crypto_prices`] defaults to) or a JSON-OBJECT STRING filter
//! (`"filters":"{\"symbol\":\"btcusdt\"}"` ⇒ that one symbol) built by [`rtds_symbol_filter`] /
//! [`RtdsConfig::with_symbol_filter`]. **Never hand-build the CSV form.**
//!
//! **Message shape (live capture).**
//! `{"connection_id":"…","payload":{"full_accuracy_value":"66038.47000000","symbol":"btcusdt",
//! "timestamp":1784735164000,"value":66038.47},"timestamp":1784735164208,"topic":"crypto_prices",
//! "type":"update"}`. `parse_entry` prefers the STRING `full_accuracy_value` over the lossy f64
//! `value` when present (parsed as a string, then converted, so no precision is dropped), and every
//! unknown field (`connection_id`; `received_at`/`is_carried_forward` on `equity_prices`) is
//! ignored — this decoder is key-driven, never a fixed struct.
//!
//! **Two payload arms.** A FILTERED subscribe answers first with a SNAPSHOT (`"type":"subscribe"`)
//! whose `payload` wraps a `data` array of `{timestamp, value}` entries — roughly the last two
//! minutes at 1 s granularity — and only THEN streams live `"type":"update"` ticks (payload = one
//! entry object). [`decode_ref_prices`] folds both arms through one path.
//!
//! **Empty frame.** An empty TEXT frame (`""`) arrives immediately after connect. It is skipped
//! before any JSON parse (`on_frame`) — never an error, never data.
//!
//! **Symbols.** The docs list only `btcusdt`/`ethusdt`/`solusdt`/`xrpusdt`, but SIX stream live
//! ([`RTDS_CRYPTO_SYMBOLS_OBSERVED`]). Nothing in this module GATES on either list — a symbol is
//! whatever the frame says it is; the constant is documentation, not a filter.
//!
//! **Keepalive — VERIFIED REQUIREMENT.** RTDS needs an APPLICATION-level keepalive: the client must
//! send the literal TEXT frame `PING` (the four ASCII characters — NOT a WS protocol ping) every
//! [`RTDS_KEEPALIVE_INTERVAL`]. That plus a conservative [`RTDS_IDLE_THRESHOLD`] silent-stall
//! watchdog is what makes a half-open/silently-reaped socket redial instead of hanging forever; both
//! stay overridable ([`RtdsConfig::keepalive_payload`]/[`RtdsConfig::keepalive_interval`]/
//! [`RtdsConfig::idle_threshold`]).
//!
//! **Opt-in, and nothing else changes.** Nothing in this crate constructs an [`RtdsFeed`]: a caller
//! must build one and call [`RtdsFeed::start`]. With that call absent, no socket is dialed, no
//! thread is spawned, and no sink verb ever fires from this module — the CLOB market feed, the exec
//! path, and `DataClient`'s declared capability matrix are all untouched (RTDS is a separate handle,
//! deliberately NOT a `subscribe_*` verb: it publishes an underlying reference series, not one of
//! the venue's own declared market-data lanes).
//!
//! **Lifecycle.** The connect → subscribe → read → backoff → reconnect loop is the shared
//! [`vike_bridge_core::market_pump`] driver, dialed through the same `connect_market_stream_via`
//! ([`vike_bridge_core::ws::configure_ws_stream`]: read timeout + `TCP_NODELAY`) the CLOB market
//! feed uses; the per-subscription stop-flag/join bookkeeping is `vike_data::FeedRegistry`. No new
//! transport, no second WS stack.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use vike_bridge_core::json::{json_int, json_num, json_str};
use vike_bridge_core::market_pump::{
    FrameOutcome, Keepalive, MarketPumpOpts, MarketStream, PumpBackoff, connect_market_stream_via,
    run_market_feed_on, run_market_session,
};
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_data::{FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId};
use vike_model::now_ms;

use super::config::RTDS_WS;
use crate::VENUE;

/// The RTDS topic carrying spot prices of the crypto underlyings (BTC/ETH/…) Polymarket's
/// up/down markets are struck on — Binance-sourced. VERIFIED live.
pub const TOPIC_CRYPTO_PRICES: &str = "crypto_prices";
/// The Chainlink-sourced crypto topic. Its symbols carry a SLASH (`btc/usd`), unlike
/// [`TOPIC_CRYPTO_PRICES`]'s exchange-pair form (`btcusdt`). VERIFIED live.
pub const TOPIC_CRYPTO_PRICES_CHAINLINK: &str = "crypto_prices_chainlink";
/// The Pyth-sourced equity topic — stocks/ETFs, and also forex/metals/commodities. VERIFIED live.
pub const TOPIC_EQUITY_PRICES: &str = "equity_prices";
/// The subscription `type` the live (post-snapshot) stream is requested under.
pub const RTDS_TYPE_UPDATE: &str = "update";
/// The `type` the venue stamps on the SNAPSHOT frame a filtered subscribe answers with (the live
/// ticks that follow carry [`RTDS_TYPE_UPDATE`]). Both arms decode through [`decode_ref_prices`].
pub const RTDS_TYPE_SUBSCRIBE: &str = "subscribe";

/// The RTDS **activity** topic — the platform-wide, wallet-attributed trade tape (every fill on the
/// CLOB, tagged with the taker's proxy wallet). Subscribed with no `filters` ⇒ platform-wide. This
/// is a SEPARATE stream from the [`TOPIC_CRYPTO_PRICES`] reference feed; it decodes through
/// [`decode_activity_trades`] into [`ActivityTrade`]s, NOT [`RefPrice`]s. VERIFIED live 2026-07-22.
pub const TOPIC_ACTIVITY: &str = "activity";
/// The subscription `type` the [`TOPIC_ACTIVITY`] trade tape is requested under:
/// `{"topic":"activity","type":"trades"}`.
pub const RTDS_TYPE_TRADES: &str = "trades";

/// The application-level keepalive payload RTDS REQUIRES: the literal four ASCII characters `PING`
/// as a TEXT frame — NOT a WS protocol ping. Verified live: without it the server drops the
/// connection.
pub const RTDS_PING: &str = "PING";
/// How often [`RTDS_PING`] must go out (verified live: every 5 seconds).
pub const RTDS_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
/// Conservative silent-stall threshold: no inbound frame of ANY kind for this long ⇒ the session
/// errors and the driver redials. Deliberately far above the observed cadence (~69 messages per
/// 12 s on an unfiltered crypto subscribe) so a quiet-but-healthy feed is never reconnect-looped.
pub const RTDS_IDLE_THRESHOLD: Duration = Duration::from_secs(120);

/// The crypto symbols OBSERVED streaming live on [`TOPIC_CRYPTO_PRICES`] (the docs list only the
/// first four). Documentation ONLY — nothing in this module gates on it: a frame's symbol is taken
/// verbatim, so a seventh symbol appearing tomorrow needs no code change.
pub const RTDS_CRYPTO_SYMBOLS_OBSERVED: [&str; 6] =
    ["btcusdt", "ethusdt", "bnbusdt", "xrpusdt", "dogeusdt", "solusdt"];

/// The ONLY working per-symbol filter form: a JSON-OBJECT **string**, `{"symbol":"btcusdt"}`.
///
/// ⚠ The documented comma-separated form (`"btcusdt,ethusdt"`) is a silent failure — it yields
/// ZERO messages on a socket that looks perfectly healthy (module doc). Never build that shape.
pub fn rtds_symbol_filter(symbol: &str) -> String {
    serde_json::json!({ "symbol": symbol }).to_string()
}

/// Build the RTDS subscribe frame with an optional per-subscription `filters` string:
/// `{"action":"subscribe","subscriptions":[{"topic":…,"type":…,"filters":…},…]}`. The `filters`
/// key is OMITTED entirely when `None` (⇒ all symbols stream, the verified default behavior);
/// when present it must be the JSON-object form from [`rtds_symbol_filter`], never CSV.
pub fn rtds_subscribe_message_filtered(subscriptions: &[(&str, &str, Option<&str>)]) -> String {
    let subs: Vec<serde_json::Value> = subscriptions
        .iter()
        .map(|(topic, msg_type, filters)| {
            let mut sub = serde_json::json!({ "topic": topic, "type": msg_type });
            if let Some(f) = filters {
                sub["filters"] = serde_json::Value::String((*f).to_string());
            }
            sub
        })
        .collect();
    serde_json::json!({ "action": "subscribe", "subscriptions": subs }).to_string()
}

/// Build the unfiltered RTDS subscribe frame (every symbol on each topic streams) — the
/// no-`filters` case of [`rtds_subscribe_message_filtered`]. Each entry is a `(topic, type)` pair;
/// the frame is sent once per session (the driver replays it verbatim on every reconnect, so
/// reconnect == resubscribe).
pub fn rtds_subscribe_message(subscriptions: &[(&str, &str)]) -> String {
    let with_filters: Vec<(&str, &str, Option<&str>)> =
        subscriptions.iter().map(|(t, m)| (*t, *m, None)).collect();
    rtds_subscribe_message_filtered(&with_filters)
}

/// One decoded reference-price observation: the underlying `symbol` (verbatim as the venue names
/// it — no case folding or aliasing here, that is a catalog concern), its `value`, and `ts` in
/// epoch-MILLISECONDS ([`normalize_ts_ms`] has already run). `ts == 0` means the frame carried no
/// timestamp at all; the pump then stamps local receive time, exactly as the CLOB market feed does.
#[derive(Debug, Clone, PartialEq)]
pub struct RefPrice {
    pub symbol: String,
    pub value: f64,
    pub ts: i64,
}

/// Coerce a wire timestamp to epoch-MILLISECONDS. RTDS entries have been seen carrying epoch
/// SECONDS, so a value below `100_000_000_000` (epoch-ms for 1973, epoch-seconds for the year 5138
/// — no live stamp can be ambiguous between the two) is scaled by 1000; anything else, including
/// the absent-`0` sentinel, passes through unchanged.
pub fn normalize_ts_ms(ts: i64) -> i64 {
    if ts > 0 && ts < 100_000_000_000 { ts * 1000 } else { ts }
}

/// Parse ONE entry object. The price is read from `full_accuracy_value` FIRST — the live wire
/// carries it as a decimal STRING (`"66038.47000000"`) alongside a lossy f64 `value`, and parsing
/// the string is what keeps the full precision — then `value`, then `price`. An entry without a
/// usable number is dropped (not the whole frame); `timestamp`/`ts` is optional (`0` = absent); the
/// symbol falls back to `default_symbol` when the entry itself does not name one. Unknown keys
/// (`connection_id`, `received_at`, `is_carried_forward`, …) are ignored by construction.
fn parse_entry(v: &serde_json::Value, default_symbol: &str) -> Option<RefPrice> {
    let value = v
        .get("full_accuracy_value")
        .and_then(json_num)
        .or_else(|| v.get("value").or_else(|| v.get("price")).and_then(json_num))?;
    if !value.is_finite() {
        return None;
    }
    let ts = v.get("timestamp").or_else(|| v.get("ts")).and_then(json_int).unwrap_or(0);
    let symbol = v
        .get("symbol")
        .map(json_str)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_symbol.to_string());
    Some(RefPrice { symbol, value, ts: normalize_ts_ms(ts) })
}

/// Decode a frame BODY (whatever sat under `data`/`payload`, or the envelope itself once gated):
/// an ARRAY is a list of entries; an OBJECT that itself wraps a `data` key is the SNAPSHOT arm
/// (`"type":"subscribe"` — its own `symbol`, when present, defaults the entries inside); any other
/// object is one entry.
fn decode_body(body: &serde_json::Value, default_symbol: &str) -> Vec<RefPrice> {
    match body {
        serde_json::Value::Array(arr) => {
            arr.iter().filter_map(|e| parse_entry(e, default_symbol)).collect()
        }
        serde_json::Value::Object(_) => match body.get("data") {
            Some(inner) => {
                let sym = body.get("symbol").map(json_str).filter(|s| !s.is_empty());
                decode_body(inner, sym.as_deref().unwrap_or(default_symbol))
            }
            None => parse_entry(body, default_symbol).into_iter().collect(),
        },
        _ => Vec::new(),
    }
}

/// Decode one RTDS envelope. The body is looked for under `data` then `payload` (the live shape is
/// `payload`; the snapshot nests its `data` array one level deeper inside it — see [`decode_body`]).
/// An envelope-level `symbol` (present when the entries omit theirs) is the per-frame default,
/// itself falling back to `default_symbol`.
///
/// **The bare-envelope arm is GATED**: when an envelope carries NEITHER `data` NOR `payload`, it is
/// only read as an entry itself if it is attributable to this stream — its `topic` equals `topic`,
/// or it names its own `symbol`. Without that gate a control/ack/error frame carrying a top-level
/// `value`/`price` key would publish a bogus tick.
fn decode_one(ev: &serde_json::Value, topic: &str, default_symbol: &str) -> Vec<RefPrice> {
    let envelope_symbol = ev.get("symbol").map(json_str).filter(|s| !s.is_empty());
    let entry_default = envelope_symbol.as_deref().unwrap_or(default_symbol);
    match ev.get("data").or_else(|| ev.get("payload")) {
        Some(body) => decode_body(body, entry_default),
        None => {
            let topic_matches = ev.get("topic").map(json_str).is_some_and(|t| t == topic);
            if topic_matches || envelope_symbol.is_some() {
                parse_entry(ev, entry_default).into_iter().collect()
            } else {
                Vec::new()
            }
        }
    }
}

/// Decode an RTDS frame into zero or more [`RefPrice`]s — the SNAPSHOT (`"type":"subscribe"`, a
/// nested `data` array) and the live UPDATE messages fold through this one path. A top-level array
/// of envelopes is also accepted (mirroring the CLOB market decoder's tolerance). Anything
/// unrecognized decodes to nothing: an unknown control/ack frame is never an error, just no data.
///
/// `topic` is the subscribed topic. It gates the BARE-envelope arm: an envelope carrying neither
/// `data` nor `payload` is only read as an entry itself when it is attributable to this stream —
/// its own `topic` equals `topic`, or it names its own `symbol`. Without that gate a control/ack/
/// error frame carrying a top-level `value`/`price` key would publish a bogus tick.
pub fn decode_ref_prices(
    frame: &serde_json::Value,
    topic: &str,
    default_symbol: &str,
) -> Vec<RefPrice> {
    match frame {
        serde_json::Value::Array(arr) => {
            arr.iter().flat_map(|f| decode_one(f, topic, default_symbol)).collect()
        }
        obj => decode_one(obj, topic, default_symbol),
    }
}

/// Everything one RTDS subscription needs. Every endpoint-shaped field stays overridable — the
/// host/topics/keepalive are now live-VERIFIED defaults (module doc), not guesses, but a caller can
/// still point this anywhere without touching the crate.
#[derive(Debug, Clone)]
pub struct RtdsConfig {
    /// WS endpoint (default `config::RTDS_WS` — verified).
    pub url: String,
    /// Subscription topic (default [`TOPIC_CRYPTO_PRICES`]).
    pub topic: String,
    /// Subscription type (default [`RTDS_TYPE_UPDATE`]).
    pub msg_type: String,
    /// Optional per-subscription `filters` string. `None` (default) omits the key entirely ⇒ every
    /// symbol on the topic streams. When set it MUST be the JSON-object form
    /// ([`rtds_symbol_filter`]) — the documented CSV form silently yields nothing (module doc).
    pub filters: Option<String>,
    /// The symbol `mark_tick` is published under when a frame names none itself.
    pub default_symbol: String,
    /// Silent-stall watchdog — [`RTDS_IDLE_THRESHOLD`] by default, so a half-open socket is
    /// redialed instead of hanging silently forever. `None` disables it.
    pub idle_threshold: Option<Duration>,
    /// The app-level keepalive payload — [`RTDS_PING`] by default (the literal TEXT frame RTDS
    /// requires; NOT a WS protocol ping).
    pub keepalive_payload: String,
    /// How often [`RtdsConfig::keepalive_payload`] goes out — [`RTDS_KEEPALIVE_INTERVAL`] by
    /// default. `None` sends no keepalive at all (the server then reaps the connection — only set
    /// it for a test seam or a deployment proven not to need one).
    pub keepalive_interval: Option<Duration>,
    /// Socket read timeout — the stop-flag poll cadence.
    pub read_timeout: Duration,
    /// Bounded TCP dial (so a black-holed route cannot pin the feed thread past the stop flag).
    pub connect_timeout: Option<Duration>,
    /// Stop-aware reconnect backoff.
    pub backoff: PumpBackoff,
}

impl RtdsConfig {
    /// A config for an arbitrary `topic`, with the LIFECYCLE knobs taken from this venue's own
    /// declared `MarketPumpSpec` row (read timeout / exponential backoff / bounded dial — consumed,
    /// never copied), the verified `PING`/5 s keepalive, and the conservative idle watchdog.
    /// `default_symbol` is what `mark_tick` is keyed on for frames that name no symbol.
    pub fn for_topic(topic: impl Into<String>, default_symbol: impl Into<String>) -> Self {
        let knobs = market_pump_spec(VENUE).knobs();
        RtdsConfig {
            url: RTDS_WS.to_string(),
            topic: topic.into(),
            msg_type: RTDS_TYPE_UPDATE.to_string(),
            filters: None,
            default_symbol: default_symbol.into(),
            idle_threshold: Some(RTDS_IDLE_THRESHOLD),
            keepalive_payload: RTDS_PING.to_string(),
            keepalive_interval: Some(RTDS_KEEPALIVE_INTERVAL),
            read_timeout: knobs.read_timeout,
            connect_timeout: knobs.connect_timeout,
            backoff: knobs.backoff,
        }
    }

    /// The [`TOPIC_CRYPTO_PRICES`] topic against the default host (see [`RtdsConfig::for_topic`]).
    pub fn crypto_prices(default_symbol: impl Into<String>) -> Self {
        Self::for_topic(TOPIC_CRYPTO_PRICES, default_symbol)
    }

    /// The [`TOPIC_ACTIVITY`]/[`RTDS_TYPE_TRADES`] wallet-attributed trade tape (see
    /// [`RtdsConfig::for_topic`]). Platform-wide (no `filters`). `default_symbol` is unused on this
    /// topic — the [`decode_activity_trades`] decoder keys each row off the trade's own `asset`
    /// token id — so it is left empty; the lifecycle knobs (keepalive/idle/backoff) are shared.
    pub fn activity_trades() -> Self {
        let mut cfg = Self::for_topic(TOPIC_ACTIVITY, "");
        cfg.msg_type = RTDS_TYPE_TRADES.to_string();
        cfg
    }

    /// Narrow this subscription to ONE symbol, using the only filter form that works — the
    /// JSON-object string ([`rtds_symbol_filter`]). A filtered subscribe is answered with a
    /// SNAPSHOT (`data` array, ~2 minutes at 1 s) before the live ticks.
    pub fn with_symbol_filter(mut self, symbol: &str) -> Self {
        self.filters = Some(rtds_symbol_filter(symbol));
        self
    }

    /// This config's subscribe frame (see [`rtds_subscribe_message_filtered`]).
    pub fn subscribe_frame(&self) -> String {
        rtds_subscribe_message_filtered(&[(
            self.topic.as_str(),
            self.msg_type.as_str(),
            self.filters.as_deref(),
        )])
    }

    /// The driver opts for this config: the verified app-level `PING` keepalive plus the
    /// silent-stall watchdog. No subscribe-ack watchdog — an UNFILTERED subscribe is answered with
    /// data and nothing else (no ack frame), so arming one would trip on a legitimately quiet
    /// moment; the idle threshold is what catches a dead socket here.
    fn opts<'a>(&'a self, subscribe: &'a str) -> MarketPumpOpts<'a> {
        MarketPumpOpts {
            subscribe: Some(subscribe),
            keepalive: self
                .keepalive_interval
                .map(|every| Keepalive { payload: self.keepalive_payload.as_str(), every }),
            ack_timeout: None,
            idle_threshold: self.idle_threshold,
            read_timeout: self.read_timeout,
            backoff: self.backoff,
            connect_timeout: self.connect_timeout,
        }
    }
}

/// One inbound TEXT frame → the driver's [`FrameOutcome`]: decode and publish every reference price
/// it carried as a `mark_tick`. An EMPTY frame (RTDS sends one right after connect), a malformed
/// (non-JSON) frame, and a recognized-but-dataless one are all `Ignore`d, never an error; a frame
/// that yielded at least one price is `Confirm` (this venue arms no ack watchdog, so the outcome is
/// inert to the lifecycle — it is still classified honestly).
fn on_frame(txt: &str, cfg: &RtdsConfig, sink: &dyn LiveDataSink) -> FrameOutcome {
    // The empty text frame RTDS sends immediately after connect — skipped BEFORE any JSON parse.
    if txt.trim().is_empty() {
        return FrameOutcome::Ignore;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(txt) else {
        return FrameOutcome::Ignore;
    };
    let prices = decode_ref_prices(&value, &cfg.topic, &cfg.default_symbol);
    if prices.is_empty() {
        return FrameOutcome::Ignore;
    }
    for p in &prices {
        // A frame without its own stamp degrades to local receive time — the same fallback the
        // CLOB market feed documents, never a plausible-but-wrong timestamp.
        let ts = if p.ts > 0 { p.ts } else { now_ms() };
        sink.mark_tick(VENUE, &p.symbol, p.value, ts);
    }
    FrameOutcome::Confirm
}

/// Drive ONE RTDS session against `stream` (the shared driver's session core): subscribe, then read
/// frames until `stop` fires (clean `Ok(())`), the stream errs, or the idle watchdog trips. This is
/// the test/replay seam — [`RtdsFeed`] runs the SAME [`on_frame`] body under the driver's reconnect
/// loop, so production and the scripted tests cannot drift.
pub fn run_rtds_session<S: MarketStream>(
    stream: &mut S,
    sink: &dyn LiveDataSink,
    cfg: &RtdsConfig,
    stop: &AtomicBool,
) -> Result<(), String> {
    let sub = cfg.subscribe_frame();
    let opts = cfg.opts(&sub);
    run_market_session(
        stream,
        &opts,
        stop,
        &now_ms,
        &mut |txt| on_frame(txt, cfg, sink),
        &mut || {},
    )
}

/// The thread body one started RTDS subscription runs: the shared connect → session → backoff →
/// reconnect loop, with the subscribe frame replayed verbatim each session.
fn rtds_main(cfg: RtdsConfig, sink: Arc<dyn LiveDataSink>, stop: Arc<AtomicBool>) {
    let sub = cfg.subscribe_frame();
    let opts = cfg.opts(&sub);
    // Optional SOCKS5 egress (spec §0.1) — same gate + endpoint as the CLOB feed; `None` (the
    // default) is the direct dial verbatim. Resolved once, reused across reconnects.
    let ws_proxy = crate::egress::ws_proxy();
    run_market_feed_on(
        || {
            connect_market_stream_via(
                &cfg.url,
                cfg.read_timeout,
                cfg.connect_timeout,
                ws_proxy.as_ref(),
            )
        },
        &opts,
        &stop,
        &now_ms,
        |txt| on_frame(txt, &cfg, sink.as_ref()),
        || {},
        |e| {
            tracing::warn!(venue = VENUE, error = %e, "polymarket RTDS ws error (reconnecting)");
        },
    );
}

/// The opt-in RTDS feed handle. Constructing one starts NOTHING — [`RtdsFeed::start`] is what
/// spawns the single feed thread; [`RtdsFeed::shutdown`] stops and JOINs it (deterministic
/// teardown, nothing detached), exactly like the CLOB feed's registry lifecycle.
pub struct RtdsFeed {
    cfg: RtdsConfig,
    sink: Arc<dyn LiveDataSink>,
    registry: FeedRegistry,
}

impl RtdsFeed {
    /// A feed bound to `cfg` and `sink`. NO socket is dialed and NO thread is spawned until
    /// [`RtdsFeed::start`] is called.
    pub fn new(cfg: RtdsConfig, sink: Arc<dyn LiveDataSink>) -> Self {
        RtdsFeed {
            cfg,
            sink,
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "polymarket",
                );
            }),
        }
    }

    /// Start the RTDS stream on its own thread. Returns the [`SubscriptionId`] to pass to
    /// [`RtdsFeed::stop`]. Callable more than once (e.g. one stream per topic) — each call spawns
    /// its own thread with its own stop flag.
    pub fn start(&mut self) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(rtds_main)
    }

    /// Shared spawn bookkeeping; `body` is [`rtds_main`] in production, a network-free stand-in in
    /// the lifecycle tests.
    fn spawn_with(
        &mut self,
        body: impl FnOnce(RtdsConfig, Arc<dyn LiveDataSink>, Arc<AtomicBool>) + Send + 'static,
    ) -> Result<SubscriptionId, LiveDataError> {
        let cfg = self.cfg.clone();
        let sink = Arc::clone(&self.sink);
        let label = format!("feed-poly-rtds-{}", cfg.topic);
        let topic = cfg.topic.clone();
        self.registry
            .spawn(label, move |stop| body(cfg, sink, stop))
            .map_err(|e| LiveDataError::Subscribe(format!("polymarket rtds ({topic}): {e}")))
    }

    /// Stop + JOIN exactly the stream `id` names. Unknown ids are a no-op.
    pub fn stop(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    /// Stop + JOIN every stream this feed started.
    pub fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

// ---- activity/trades: the wallet-attributed trade tape (Wave 5c) --------------------------------
// A SECOND RTDS lane over the SAME keyless socket/driver as the reference-price feed above, carrying
// the platform-wide, wallet-tagged trade tape (`{"topic":"activity","type":"trades"}`). Everything
// below is opt-in and self-contained: nothing constructs an `RtdsActivityFeed`, so a build that
// never calls `RtdsActivityFeed::start` is byte-identical to before this lane existed — the
// `crypto_prices` path (its `RtdsConfig`, `on_frame`, `run_rtds_session`, `RtdsFeed`) is untouched.

/// Which way a taker crossed the book on an [`ActivityTrade`]. Mirrors the crate's order-side
/// vocabulary ([`crate::order::Side`]) but is decoded from the RTDS wire's `"BUY"`/`"SELL"` string
/// rather than the V2 numeric code — kept a distinct type so the RTDS decoder never depends on the
/// order-signing module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeSide {
    Buy,
    Sell,
}

impl TradeSide {
    /// Parse the RTDS wire side. `"BUY"`/`"SELL"` (case-insensitive) → the variant; anything else →
    /// `None`, so a row with a missing or unrecognized side is a tolerant skip, not a batch failure.
    pub fn from_wire(s: &str) -> Option<TradeSide> {
        if s.eq_ignore_ascii_case("BUY") {
            Some(TradeSide::Buy)
        } else if s.eq_ignore_ascii_case("SELL") {
            Some(TradeSide::Sell)
        } else {
            None
        }
    }
}

/// One decoded wallet-attributed trade from the [`TOPIC_ACTIVITY`] tape: the taker's `proxy_wallet`,
/// the `side` it crossed, the `size`/`price` it traded, the market coordinates (`condition_id`,
/// `asset` = the ERC-1155 outcome token id, `outcome` label), the on-chain `tx_hash`, and `ts` in
/// epoch-MILLISECONDS ([`normalize_ts_ms`] has already scaled the wire's epoch-SECONDS stamp).
/// `ts == 0` means the frame carried no timestamp. No `Eq` (it holds `f64` fields) — same shape as
/// [`RefPrice`].
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityTrade {
    pub proxy_wallet: String,
    pub side: TradeSide,
    pub size: f64,
    pub price: f64,
    pub condition_id: String,
    pub asset: String,
    pub outcome: String,
    pub tx_hash: String,
    pub ts: i64,
}

/// The delivery seam for decoded [`ActivityTrade`]s — the activity-lane mirror of
/// [`vike_data::LiveDataSink`]: the pump calls [`ActivityTradeSink::on_activity_trade`] once per
/// decoded row, exactly as the reference-price lane calls `sink.mark_tick` per [`RefPrice`]. The
/// MAKER-side consumer is a deferred follow-up; this crate ships only the producer + seam.
pub trait ActivityTradeSink: Send + Sync {
    /// Fire-and-forget delivery of one decoded trade (borrowed — the sink clones only if it keeps it).
    fn on_activity_trade(&self, trade: &ActivityTrade);
}

/// Parse ONE activity entry object. The REQUIRED fields — `proxyWallet` (non-empty), `side`
/// (parseable), `size`/`price` (finite numbers), `asset` (non-empty token id) — must all be present
/// and well-typed; a row missing or mis-typing any of them is dropped (not the whole batch), exactly
/// like [`parse_entry`]. `conditionId`/`outcome`/`transactionHash` default to `""` when absent;
/// `timestamp` (`0` = absent) is normalized from epoch-seconds. Unknown keys (`outcomeIndex`,
/// `connection_id`, …) are ignored by construction.
fn parse_activity_entry(v: &serde_json::Value, default_ts: i64) -> Option<ActivityTrade> {
    let proxy_wallet = v.get("proxyWallet").map(json_str).filter(|s| !s.is_empty())?;
    let side = v.get("side").map(json_str).and_then(|s| TradeSide::from_wire(&s))?;
    let size = v.get("size").and_then(json_num)?;
    let price = v.get("price").and_then(json_num)?;
    if !size.is_finite() || !price.is_finite() {
        return None;
    }
    let asset = v.get("asset").map(json_str).filter(|s| !s.is_empty())?;
    // The per-trade payload usually omits its own timestamp; fall back to the envelope's `default_ts`.
    let ts = v.get("timestamp").or_else(|| v.get("ts")).and_then(json_int).unwrap_or(default_ts);
    Some(ActivityTrade {
        proxy_wallet,
        side,
        size,
        price,
        condition_id: v.get("conditionId").map(json_str).unwrap_or_default(),
        asset,
        outcome: v.get("outcome").map(json_str).unwrap_or_default(),
        tx_hash: v.get("transactionHash").map(json_str).unwrap_or_default(),
        ts: normalize_ts_ms(ts),
    })
}

/// Decode an activity frame BODY (whatever sat under `data`/`payload`): an ARRAY is a list of trade
/// entries; an OBJECT wrapping a `data` key nests one level deeper (snapshot-style); any other object
/// is one entry. Mirrors [`decode_body`]'s tolerance.
fn decode_activity_body(body: &serde_json::Value, default_ts: i64) -> Vec<ActivityTrade> {
    match body {
        serde_json::Value::Array(arr) => {
            arr.iter().filter_map(|e| parse_activity_entry(e, default_ts)).collect()
        }
        serde_json::Value::Object(_) => match body.get("data") {
            Some(inner) => decode_activity_body(inner, default_ts),
            None => parse_activity_entry(body, default_ts).into_iter().collect(),
        },
        _ => Vec::new(),
    }
}

/// Decode one activity envelope: the trade(s) sit under `data`/`payload` (the live shape is
/// `payload`); a bare envelope with neither is read as one entry itself — a control/ack frame lacks
/// the required `proxyWallet`/`side`/`asset` fields, so it naturally decodes to nothing (no bogus
/// trade), which is why this lane needs no explicit topic gate the way [`decode_one`] does.
fn decode_activity_one(ev: &serde_json::Value) -> Vec<ActivityTrade> {
    // The RTDS envelope carries the `timestamp`; the payload trade object usually does not — so the
    // envelope stamp is the fallback for any entry that doesn't carry its own.
    let default_ts = ev.get("timestamp").or_else(|| ev.get("ts")).and_then(json_int).unwrap_or(0);
    match ev.get("data").or_else(|| ev.get("payload")) {
        Some(body) => decode_activity_body(body, default_ts),
        None => parse_activity_entry(ev, default_ts).into_iter().collect(),
    }
}

/// Decode an RTDS activity frame into zero or more [`ActivityTrade`]s — the PURE, key-driven,
/// tolerant twin of [`decode_ref_prices`]. A single-object `payload`, a `payload` array, a nested
/// snapshot `data` array, and a top-level array of envelopes all fold through this one path; an
/// unrecognized control/ack frame decodes to nothing (never an error). A row missing or mis-typing a
/// required field is skipped, the rest of the batch survives.
pub fn decode_activity_trades(frame: &serde_json::Value) -> Vec<ActivityTrade> {
    match frame {
        serde_json::Value::Array(arr) => arr.iter().flat_map(decode_activity_one).collect(),
        obj => decode_activity_one(obj),
    }
}

/// One inbound TEXT frame → the driver's [`FrameOutcome`], the activity-lane twin of [`on_frame`]:
/// decode and deliver every trade it carried to `sink`. An EMPTY frame, a malformed (non-JSON)
/// frame, and a recognized-but-dataless one are all `Ignore`d; a frame that yielded at least one
/// trade is `Confirm` (inert to the lifecycle — this lane arms no ack watchdog either).
fn on_activity_frame(txt: &str, sink: &dyn ActivityTradeSink) -> FrameOutcome {
    if txt.trim().is_empty() {
        return FrameOutcome::Ignore;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(txt) else {
        return FrameOutcome::Ignore;
    };
    let trades = decode_activity_trades(&value);
    if trades.is_empty() {
        return FrameOutcome::Ignore;
    }
    for t in &trades {
        sink.on_activity_trade(t);
    }
    FrameOutcome::Confirm
}

/// Drive ONE activity session against `stream` — the test/replay seam mirroring [`run_rtds_session`],
/// reusing the same [`RtdsConfig`] driver opts (keepalive/idle/backoff) so production and the
/// scripted tests cannot drift. `cfg` should be an [`RtdsConfig::activity_trades`] config.
pub fn run_rtds_activity_session<S: MarketStream>(
    stream: &mut S,
    sink: &dyn ActivityTradeSink,
    cfg: &RtdsConfig,
    stop: &AtomicBool,
) -> Result<(), String> {
    let sub = cfg.subscribe_frame();
    let opts = cfg.opts(&sub);
    run_market_session(
        stream,
        &opts,
        stop,
        &now_ms,
        &mut |txt| on_activity_frame(txt, sink),
        &mut || {},
    )
}

/// The thread body one started activity subscription runs — the activity-lane twin of [`rtds_main`]:
/// the shared connect → session → backoff → reconnect loop, subscribe frame replayed each session.
fn activity_main(cfg: RtdsConfig, sink: Arc<dyn ActivityTradeSink>, stop: Arc<AtomicBool>) {
    let sub = cfg.subscribe_frame();
    let opts = cfg.opts(&sub);
    // Same optional SOCKS5 egress gate + endpoint as the reference-price lane; `None` = direct dial.
    let ws_proxy = crate::egress::ws_proxy();
    run_market_feed_on(
        || {
            connect_market_stream_via(
                &cfg.url,
                cfg.read_timeout,
                cfg.connect_timeout,
                ws_proxy.as_ref(),
            )
        },
        &opts,
        &stop,
        &now_ms,
        |txt| on_activity_frame(txt, sink.as_ref()),
        || {},
        |e| {
            tracing::warn!(venue = VENUE, error = %e, "polymarket RTDS activity ws error (reconnecting)");
        },
    );
}

/// The opt-in RTDS **activity** feed handle — the wallet-attributed trade-tape twin of [`RtdsFeed`].
/// Constructing one starts NOTHING; [`RtdsActivityFeed::start`] spawns the single feed thread and
/// [`RtdsActivityFeed::shutdown`] stops and JOINs it (deterministic teardown), exactly like
/// [`RtdsFeed`]'s registry lifecycle.
pub struct RtdsActivityFeed {
    cfg: RtdsConfig,
    sink: Arc<dyn ActivityTradeSink>,
    registry: FeedRegistry,
}

impl RtdsActivityFeed {
    /// A feed bound to `cfg` (typically [`RtdsConfig::activity_trades`]) and `sink`. NO socket is
    /// dialed and NO thread is spawned until [`RtdsActivityFeed::start`] is called.
    pub fn new(cfg: RtdsConfig, sink: Arc<dyn ActivityTradeSink>) -> Self {
        RtdsActivityFeed {
            cfg,
            sink,
            registry: FeedRegistry::with_spawn_hook(|| {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    "polymarket",
                );
            }),
        }
    }

    /// Start the activity stream on its own thread. Returns the [`SubscriptionId`] to pass to
    /// [`RtdsActivityFeed::stop`]. Callable more than once — each call spawns its own thread with its
    /// own stop flag.
    pub fn start(&mut self) -> Result<SubscriptionId, LiveDataError> {
        self.spawn_with(activity_main)
    }

    /// Shared spawn bookkeeping; `body` is [`activity_main`] in production, a network-free stand-in in
    /// the lifecycle tests.
    fn spawn_with(
        &mut self,
        body: impl FnOnce(RtdsConfig, Arc<dyn ActivityTradeSink>, Arc<AtomicBool>) + Send + 'static,
    ) -> Result<SubscriptionId, LiveDataError> {
        let cfg = self.cfg.clone();
        let sink = Arc::clone(&self.sink);
        let label = format!("feed-poly-rtds-activity-{}", cfg.topic);
        let topic = cfg.topic.clone();
        self.registry.spawn(label, move |stop| body(cfg, sink, stop)).map_err(|e| {
            LiveDataError::Subscribe(format!("polymarket rtds activity ({topic}): {e}"))
        })
    }

    /// Stop + JOIN exactly the stream `id` names. Unknown ids are a no-op.
    pub fn stop(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    /// Stop + JOIN every stream this feed started.
    pub fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    use vike_data::RecordingSink;

    const BTC: &str = "btc";

    fn cfg() -> RtdsConfig {
        RtdsConfig::crypto_prices(BTC)
    }

    /// The decoder under the default topic — the shape every test below reads through.
    fn decode(frame: &serde_json::Value) -> Vec<RefPrice> {
        decode_ref_prices(frame, TOPIC_CRYPTO_PRICES, BTC)
    }

    #[test]
    fn the_subscribe_frame_is_the_documented_shape() {
        let s = rtds_subscribe_message(&[(TOPIC_CRYPTO_PRICES, RTDS_TYPE_UPDATE)]);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["action"], "subscribe");
        assert_eq!(v["subscriptions"][0]["topic"], "crypto_prices");
        assert_eq!(v["subscriptions"][0]["type"], "update");
        assert!(
            v["subscriptions"][0].get("filters").is_none(),
            "no filters key at all = the verified all-symbols subscribe"
        );
        assert_eq!(v["subscriptions"].as_array().unwrap().len(), 1);
        // the config builds exactly that frame
        assert_eq!(cfg().subscribe_frame(), s);
    }

    #[test]
    fn multiple_subscriptions_ride_one_frame() {
        let s = rtds_subscribe_message(&[("crypto_prices", "update"), ("equity_prices", "update")]);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["subscriptions"].as_array().unwrap().len(), 2);
        assert_eq!(v["subscriptions"][1]["topic"], "equity_prices");
    }

    /// The ONLY working filter form is the JSON-OBJECT string. The documented comma-separated
    /// form silently returns nothing forever, so this crate must never emit it.
    #[test]
    fn a_symbol_filter_is_the_json_object_string_never_csv() {
        assert_eq!(rtds_symbol_filter("btcusdt"), r#"{"symbol":"btcusdt"}"#);

        let frame = cfg().with_symbol_filter("btcusdt").subscribe_frame();
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        let filters = v["subscriptions"][0]["filters"].as_str().expect("filters is a STRING");
        assert_eq!(filters, r#"{"symbol":"btcusdt"}"#);
        assert!(!filters.contains(','), "the CSV form is the silent-failure trap — never emit it");
        // and the filter string is itself parseable JSON carrying the symbol
        let inner: serde_json::Value = serde_json::from_str(filters).unwrap();
        assert_eq!(inner["symbol"], "btcusdt");
    }

    #[test]
    fn epoch_seconds_are_scaled_and_epoch_ms_pass_through() {
        assert_eq!(normalize_ts_ms(1_700_000_000), 1_700_000_000_000); // seconds → ms
        assert_eq!(normalize_ts_ms(1_700_000_000_000), 1_700_000_000_000); // already ms
        assert_eq!(normalize_ts_ms(0), 0); // the absent sentinel
        assert_eq!(normalize_ts_ms(-5), -5); // nonsense passes through untouched
    }

    #[test]
    fn the_snapshot_data_array_decodes_every_entry() {
        let frame = serde_json::json!({
            "topic": "crypto_prices",
            "type": "update",
            "data": [
                {"symbol": "btc", "timestamp": 1_700_000_000_i64, "value": 64123.5},
                {"symbol": "eth", "timestamp": "1700000001000", "value": "3200.25"}
            ]
        });
        assert_eq!(
            decode(&frame),
            vec![
                RefPrice { symbol: "btc".into(), value: 64123.5, ts: 1_700_000_000_000 },
                RefPrice { symbol: "eth".into(), value: 3200.25, ts: 1_700_000_001_000 },
            ],
            "string AND number wire forms both decode; epoch-seconds are normalized"
        );
    }

    /// The LIVE update envelope, verbatim from the 2026-07-22 probe: unknown keys are ignored and
    /// the STRING `full_accuracy_value` wins over the lossy f64 `value`.
    #[test]
    fn the_live_update_envelope_decodes_and_prefers_full_accuracy_value() {
        let frame = serde_json::json!({
            "connection_id": "abc-123",
            "payload": {
                "full_accuracy_value": "66038.47123456",
                "symbol": "btcusdt",
                "timestamp": 1_784_735_164_000_i64,
                "value": 66038.47
            },
            "timestamp": 1_784_735_164_208_i64,
            "topic": "crypto_prices",
            "type": "update"
        });
        assert_eq!(
            decode(&frame),
            vec![RefPrice {
                symbol: "btcusdt".into(),
                value: 66038.47123456,
                ts: 1_784_735_164_000
            }],
            "the entry timestamp (not the envelope's) and the full-accuracy string are used"
        );
    }

    /// The SNAPSHOT arm of a filtered subscribe: `type:"subscribe"`, and the `data` array sits
    /// INSIDE `payload`, one level deeper than the live update's entry object.
    #[test]
    fn the_filtered_subscribe_snapshot_arm_decodes_its_nested_data_array() {
        let frame = serde_json::json!({
            "connection_id": "abc-123",
            "topic": "crypto_prices",
            "type": "subscribe",
            "payload": {
                "symbol": "btcusdt",
                "data": [
                    {"timestamp": 1_784_735_163_000_i64, "value": 66037.0},
                    {"timestamp": 1_784_735_164_000_i64, "value": 66038.47}
                ]
            }
        });
        assert_eq!(
            decode(&frame),
            vec![
                RefPrice { symbol: "btcusdt".into(), value: 66037.0, ts: 1_784_735_163_000 },
                RefPrice { symbol: "btcusdt".into(), value: 66038.47, ts: 1_784_735_164_000 },
            ],
            "the payload's own symbol defaults every entry inside its data array"
        );
        assert_eq!(RTDS_TYPE_SUBSCRIBE, "subscribe");
    }

    #[test]
    fn a_single_object_update_decodes_under_data_payload_or_bare() {
        let want = vec![RefPrice { symbol: "btc".into(), value: 64200.0, ts: 1_700_000_002_000 }];
        let entry = serde_json::json!({"timestamp": 1_700_000_002_000_i64, "value": 64200.0});
        for frame in [
            serde_json::json!({"topic": "crypto_prices", "data": entry.clone()}),
            serde_json::json!({"topic": "crypto_prices", "payload": entry.clone()}),
            // bare: allowed because the envelope's own topic matches the subscription
            serde_json::json!({
                "topic": "crypto_prices",
                "timestamp": 1_700_000_002_000_i64,
                "value": 64200.0
            }),
        ] {
            assert_eq!(decode(&frame), want, "frame: {frame}");
        }
    }

    /// The bare-envelope gate: a control/ack/error frame carrying a top-level `value` must NOT
    /// publish a tick unless it is attributable to this stream (matching topic, or its own symbol).
    #[test]
    fn a_bare_envelope_only_decodes_when_it_is_attributable_to_this_stream() {
        // neither a matching topic nor a symbol → dropped
        let foreign = serde_json::json!({"topic": "equity_prices", "value": 1.0});
        assert!(decode(&foreign).is_empty(), "another topic's frame is not our data");
        let control = serde_json::json!({"type": "error", "message": "nope", "value": 42.0});
        assert!(decode(&control).is_empty(), "a control frame's stray `value` is never a tick");
        // its own symbol IS attribution enough (the top-level-array shape below relies on it)
        let owned = serde_json::json!({"symbol": "btc", "value": 1.0});
        assert_eq!(decode(&owned).len(), 1);
    }

    #[test]
    fn the_symbol_falls_back_entry_then_envelope_then_config() {
        // entry-level wins
        let f = serde_json::json!({"symbol": "eth", "data": [{"symbol": "sol", "value": 1.0}]});
        assert_eq!(decode(&f)[0].symbol, "sol");
        // envelope-level when the entry names none
        let f = serde_json::json!({"symbol": "eth", "data": [{"value": 1.0}]});
        assert_eq!(decode(&f)[0].symbol, "eth");
        // the config default when neither does
        let f = serde_json::json!({"data": [{"value": 1.0}]});
        assert_eq!(decode(&f)[0].symbol, BTC);
    }

    #[test]
    fn unusable_entries_are_dropped_without_dropping_the_frame() {
        let frame = serde_json::json!({"data": [
            {"symbol": "btc", "value": "not-a-number"},
            {"symbol": "eth"},
            {"symbol": "sol", "value": 1.5}
        ]});
        assert_eq!(
            decode(&frame),
            vec![RefPrice { symbol: "sol".into(), value: 1.5, ts: 0 }],
            "only the usable entry survives; ts 0 = the frame carried no stamp"
        );
    }

    #[test]
    fn control_frames_decode_to_nothing() {
        for frame in [
            serde_json::json!({"action": "subscribe", "status": "ok"}),
            serde_json::json!({}),
            serde_json::json!([]),
        ] {
            assert!(decode(&frame).is_empty(), "frame: {frame}");
        }
    }

    #[test]
    fn a_top_level_array_of_envelopes_decodes_each() {
        let frame = serde_json::json!([
            {"symbol": "btc", "value": 1.0, "timestamp": 1_700_000_000_000_i64},
            {"symbol": "eth", "value": 2.0, "timestamp": 1_700_000_000_000_i64}
        ]);
        assert_eq!(decode(&frame).len(), 2);
    }

    #[test]
    fn on_frame_publishes_mark_ticks_and_ignores_junk() {
        let sink = RecordingSink::default();
        let cfg = cfg();
        assert_eq!(on_frame("not json at all", &cfg, &sink), FrameOutcome::Ignore);
        assert_eq!(on_frame(r#"{"action":"subscribe"}"#, &cfg, &sink), FrameOutcome::Ignore);
        // the empty text frame RTDS sends right after connect
        assert_eq!(on_frame("", &cfg, &sink), FrameOutcome::Ignore);
        assert_eq!(on_frame("   ", &cfg, &sink), FrameOutcome::Ignore);
        assert!(sink.calls().is_empty(), "neither junk nor a control frame emits anything");

        let frame = serde_json::json!({"data": [
            {"symbol": "btc", "timestamp": 1_700_000_000_000_i64, "value": 64123.5}
        ]})
        .to_string();
        assert_eq!(on_frame(&frame, &cfg, &sink), FrameOutcome::Confirm);
        assert_eq!(
            sink.calls(),
            vec!["mark_tick(polymarket,btc,64123.5,1700000000000)".to_string()]
        );
    }

    /// The VERIFIED liveness contract: the literal `PING` text frame every 5 s, plus a
    /// conservative silent-stall watchdog so a half-open socket is redialed.
    #[test]
    fn the_session_arms_the_verified_ping_keepalive_and_an_idle_watchdog() {
        let cfg = cfg();
        assert_eq!(cfg.keepalive_payload, "PING");
        assert_eq!(cfg.keepalive_interval, Some(Duration::from_secs(5)));
        assert_eq!(cfg.idle_threshold, Some(Duration::from_secs(120)));

        let sub = cfg.subscribe_frame();
        let opts = cfg.opts(&sub);
        let ka = opts.keepalive.expect("RTDS requires an app-level keepalive");
        assert_eq!(ka.payload, RTDS_PING);
        assert_eq!(ka.every, RTDS_KEEPALIVE_INTERVAL);
        assert_eq!(opts.idle_threshold, Some(RTDS_IDLE_THRESHOLD));
        assert_eq!(opts.ack_timeout, None, "no ack frame exists to wait for");
    }

    #[test]
    fn the_verified_topics_are_pinned() {
        assert_eq!(TOPIC_CRYPTO_PRICES, "crypto_prices");
        assert_eq!(TOPIC_CRYPTO_PRICES_CHAINLINK, "crypto_prices_chainlink");
        assert_eq!(TOPIC_EQUITY_PRICES, "equity_prices");
        let eq = RtdsConfig::for_topic(TOPIC_EQUITY_PRICES, "aapl");
        assert_eq!(eq.topic, "equity_prices");
        assert_eq!(eq.default_symbol, "aapl");
        // the observed roster is documentation only — six stream live, the docs name four
        assert_eq!(RTDS_CRYPTO_SYMBOLS_OBSERVED.len(), 6);
        assert!(RTDS_CRYPTO_SYMBOLS_OBSERVED.contains(&"dogeusdt"));
    }

    /// The OFF/default path: constructing the handle dials nothing, spawns nothing, and emits
    /// nothing — RTDS only exists once a caller explicitly calls `start`.
    #[test]
    fn constructing_the_feed_starts_nothing() {
        let sink = Arc::new(RecordingSink::default());
        let feed = RtdsFeed::new(cfg(), Arc::clone(&sink) as Arc<dyn LiveDataSink>);
        assert!(feed.registry.is_empty(), "no thread until start()");
        assert!(sink.calls().is_empty(), "no sink verb fires from an unstarted feed");
    }

    /// A network-free stand-in for [`rtds_main`]: polls its own stop flag, never touching a socket.
    fn fake_body(_cfg: RtdsConfig, _sink: Arc<dyn LiveDataSink>, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn start_stop_and_shutdown_are_a_deterministic_lifecycle() {
        let sink = Arc::new(RecordingSink::default()) as Arc<dyn LiveDataSink>;
        let mut feed = RtdsFeed::new(cfg(), sink);
        let a = feed.spawn_with(fake_body).expect("spawn ok");
        let b = feed.spawn_with(fake_body).expect("spawn ok");
        assert_ne!(a, b, "distinct ids per start");
        feed.stop(a);
        assert_eq!(feed.registry.len(), 1, "stopping one leaves the other running");
        assert!(feed.registry.contains(b));
        feed.shutdown();
        assert!(feed.registry.is_empty());
    }

    // ---- activity/trades lane (Wave 5c) --------------------------------------------------------

    /// An `ActivityTradeSink` test double: records every delivered trade for assertion.
    #[derive(Default)]
    struct RecordingActivitySink {
        trades: std::sync::Mutex<Vec<ActivityTrade>>,
    }

    impl ActivityTradeSink for RecordingActivitySink {
        fn on_activity_trade(&self, trade: &ActivityTrade) {
            self.trades.lock().unwrap().push(trade.clone());
        }
    }

    #[test]
    fn the_activity_subscribe_frame_is_the_documented_shape() {
        let cfg = RtdsConfig::activity_trades();
        let s = cfg.subscribe_frame();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["action"], "subscribe");
        assert_eq!(v["subscriptions"][0]["topic"], "activity");
        assert_eq!(v["subscriptions"][0]["type"], "trades");
        assert!(
            v["subscriptions"][0].get("filters").is_none(),
            "no filters key = the platform-wide activity subscribe"
        );
        assert_eq!(v["subscriptions"].as_array().unwrap().len(), 1);
        assert_eq!(TOPIC_ACTIVITY, "activity");
        assert_eq!(RTDS_TYPE_TRADES, "trades");
        // the builder-produced frame is exactly the hand-built one
        assert_eq!(
            s,
            rtds_subscribe_message(&[(TOPIC_ACTIVITY, RTDS_TYPE_TRADES)]),
            "activity_trades() builds the {{\"topic\":\"activity\",\"type\":\"trades\"}} frame"
        );
    }

    #[test]
    fn trade_side_parses_case_insensitively_else_none() {
        assert_eq!(TradeSide::from_wire("BUY"), Some(TradeSide::Buy));
        assert_eq!(TradeSide::from_wire("sell"), Some(TradeSide::Sell));
        assert_eq!(TradeSide::from_wire("Buy"), Some(TradeSide::Buy));
        assert_eq!(TradeSide::from_wire("HODL"), None);
        assert_eq!(TradeSide::from_wire(""), None);
    }

    /// The LIVE activity/trades envelope shape: a single trade object under `payload`, unknown keys
    /// (`outcomeIndex`, `connection_id`) ignored, the epoch-ms stamp passed through.
    #[test]
    fn a_single_activity_trade_decodes_from_the_payload() {
        let frame = serde_json::json!({
            "connection_id": "abc-123",
            "topic": "activity",
            "type": "trades",
            "timestamp": 1_700_000_000_208_i64,
            "payload": {
                "proxyWallet": "0xWALLET",
                "side": "BUY",
                "size": 100.0,
                "price": 0.62,
                "conditionId": "0xCOND",
                "asset": "123456789",
                "outcome": "Yes",
                "outcomeIndex": 0,
                "transactionHash": "0xDEAD"
            }
        });
        assert_eq!(
            decode_activity_trades(&frame),
            vec![ActivityTrade {
                proxy_wallet: "0xWALLET".into(),
                side: TradeSide::Buy,
                size: 100.0,
                price: 0.62,
                condition_id: "0xCOND".into(),
                asset: "123456789".into(),
                outcome: "Yes".into(),
                tx_hash: "0xDEAD".into(),
                ts: 1_700_000_000_208,
            }]
        );
    }

    /// A `payload` ARRAY of trades, with one malformed row (non-numeric `size`) tolerantly skipped
    /// and the epoch-SECONDS stamps normalized to ms. The batch survives the bad row.
    #[test]
    fn an_activity_trade_array_skips_a_malformed_row() {
        let frame = serde_json::json!({
            "topic": "activity",
            "type": "trades",
            "payload": [
                {"proxyWallet": "0xA", "side": "BUY", "size": 10.0, "price": 0.5,
                 "asset": "111", "timestamp": 1_700_000_000_i64},
                // malformed: size is not a number → this row is dropped, not the batch
                {"proxyWallet": "0xB", "side": "SELL", "size": "not-a-number", "price": 0.4,
                 "asset": "222", "timestamp": 1_700_000_000_i64},
                // malformed: missing required proxyWallet → dropped
                {"side": "SELL", "size": 7.0, "price": 0.3, "asset": "444"},
                {"proxyWallet": "0xC", "side": "SELL", "size": 5.0, "price": 0.9,
                 "asset": "333", "timestamp": 1_700_000_001_i64}
            ]
        });
        let got = decode_activity_trades(&frame);
        assert_eq!(got.len(), 2, "the two malformed rows are skipped; the two good rows survive");
        assert_eq!(got[0].proxy_wallet, "0xA");
        assert_eq!(got[0].side, TradeSide::Buy);
        assert_eq!(got[0].ts, 1_700_000_000_000, "epoch-seconds normalized to ms");
        assert_eq!(got[1].proxy_wallet, "0xC");
        assert_eq!(got[1].side, TradeSide::Sell);
        assert_eq!(got[1].size, 5.0);
    }

    #[test]
    fn a_control_or_empty_activity_frame_decodes_to_nothing() {
        for frame in [
            serde_json::json!({"type": "error", "message": "nope"}),
            serde_json::json!({"action": "subscribe", "status": "ok"}),
            serde_json::json!({}),
            serde_json::json!([]),
        ] {
            assert!(decode_activity_trades(&frame).is_empty(), "frame: {frame}");
        }
    }

    #[test]
    fn on_activity_frame_delivers_trades_and_ignores_junk() {
        let sink = RecordingActivitySink::default();
        assert_eq!(on_activity_frame("not json", &sink), FrameOutcome::Ignore);
        assert_eq!(on_activity_frame("", &sink), FrameOutcome::Ignore);
        assert_eq!(on_activity_frame("   ", &sink), FrameOutcome::Ignore);
        assert_eq!(on_activity_frame(r#"{"type":"error"}"#, &sink), FrameOutcome::Ignore);
        assert!(sink.trades.lock().unwrap().is_empty(), "junk emits nothing");

        let frame = serde_json::json!({
            "topic": "activity",
            "type": "trades",
            "payload": {"proxyWallet": "0xW", "side": "SELL", "size": 3.0, "price": 0.71,
                        "asset": "999", "timestamp": 1_700_000_000_000_i64}
        })
        .to_string();
        assert_eq!(on_activity_frame(&frame, &sink), FrameOutcome::Confirm);
        let recorded = sink.trades.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].proxy_wallet, "0xW");
        assert_eq!(recorded[0].side, TradeSide::Sell);
        assert_eq!(recorded[0].asset, "999");
    }

    /// A network-free stand-in for [`activity_main`]: polls its own stop flag, never a socket.
    fn fake_activity_body(
        _cfg: RtdsConfig,
        _sink: Arc<dyn ActivityTradeSink>,
        stop: Arc<AtomicBool>,
    ) {
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn constructing_the_activity_feed_starts_nothing() {
        let sink = Arc::new(RecordingActivitySink::default());
        let feed = RtdsActivityFeed::new(
            RtdsConfig::activity_trades(),
            Arc::clone(&sink) as Arc<dyn ActivityTradeSink>,
        );
        assert!(feed.registry.is_empty(), "no thread until start()");
        assert!(sink.trades.lock().unwrap().is_empty(), "no trade fires from an unstarted feed");
    }

    #[test]
    fn activity_feed_start_stop_shutdown_is_a_deterministic_lifecycle() {
        let sink = Arc::new(RecordingActivitySink::default()) as Arc<dyn ActivityTradeSink>;
        let mut feed = RtdsActivityFeed::new(RtdsConfig::activity_trades(), sink);
        let a = feed.spawn_with(fake_activity_body).expect("spawn ok");
        let b = feed.spawn_with(fake_activity_body).expect("spawn ok");
        assert_ne!(a, b, "distinct ids per start");
        feed.stop(a);
        assert_eq!(feed.registry.len(), 1, "stopping one leaves the other running");
        assert!(feed.registry.contains(b));
        feed.shutdown();
        assert!(feed.registry.is_empty());
    }
}
