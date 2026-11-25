//! Live Hyperliquid market data → the vike-data live seam: [`DataClientSpecHyperliquid`], the venue's
//! half of the shared [`vike_data::DataClient`] engine (`vike_bridge_core::data_client::ws::DataClientWebsocket`),
//! the HL sibling of bybit's/okx's specs. The pure decoders live in [`crate::feed::decode`].
//!
//! The engine runs one STOPPABLE thread per subscription. Each lane connects the ONE public WS (its
//! network's `data` row, [`crate::config::websocket_url`]), sends its subscribe frame, and runs a
//! read-loop that polls a per-subscription stop flag on the socket read timeout.
//!
//! **Verbs served** (declared caps `{bars, quotes, trades, depth}`; `book:false`):
//! - `subscribe_bars` → `candle` → `close_bar` (lossless) / `forming_bar` + `bar_close_tick`
//!   (conflating). A PERP bars subscription also opens `activeAssetCtx`, feeding the
//!   REAL-mark verb `mark_tick` (markPx; default ON, the venue's `mark_streams` row through the
//!   engine's `with_mark_streams`).
//!
//! **Every lane is asked for a USER name** (`BTCUSDC.P`, `HYPEUSDC`; decision 0290) and labels what it emits with
//! it; the venue's `coin` (`BTC`, `@107`) is read off the feed's [`Symbology`], loaded in the background from its
//! network's `meta` + `spotMeta` ([`DataClientSpecHyperliquid::with_background_symbology`]). A lane HOLDS until that
//! list is loaded ([`SYMBOLOGY_WAIT`] at most), then subscribes the row's coin or refuses in words; the raw name
//! is never subscribed (audit flag HL-3: the venue answers a coin it does not list with nothing).
//! - `subscribe_quotes` → `bbo` → `DataClientSink::quote`.
//! - `subscribe_trades` → `trades` → `DataClientSink::trade`.
//! - `subscribe_depth` → `l2Book` → `DataClientSink::l2_snapshot` (the DOM conflating lane).
//! - `subscribe_book` → **`Unsupported`**, the caps row's refusal: HL's `l2Book` is a full snapshot
//!   every frame with no delta lane to record, so the lossless `book`/`book_update` verb has nothing
//!   to carry.
//!
//! Every lane is an engine session: a rollover close is the engine's inferred close (`close_bar`
//! alone), and an `l2Book` frame its full-depth snapshot (`l2_snapshot` at the venue's time).
//!
//! **HL specifics that shape the pump** (`docs/research/2026-07-16-hyperliquid-adapters` §7/§9):
//! - **Subscribe** = `{"method":"subscribe","subscription":{"type":<t>,"coin":<coin>,…}}`; keepalive
//!   is an APP-LEVEL `{"method":"ping"}` sent every 30 s (server drops a connection idle > 60 s —
//!   the cadence is this venue's market lane, `crates/vike-venues/src/venue_timings.rs`'s `HYPERLIQUID`), answered
//!   with a `{"channel":"pong"}` DATA
//!   frame.
//! - **`l2Book` is ALWAYS a full snapshot** (no deltas, no removals, no sequence — `time` the only
//!   ordering key), so depth emits a fresh `l2_snapshot` each frame, never a delta.
//! - **Candles carry no `confirm` flag** — closed-vs-forming is decided here by **open-time
//!   rollover**: a `candle` with a strictly-newer open-time (`t`) means the previously-forming candle
//!   is final. (There is no REST candle warm-up seed in this feed — WS-only, live-only, like the
//!   trades tape; a chart fills from the first live close forward.)
//! - **Reconnect** re-runs the same subscribe (the shape is captured in each lane's closure) and
//!   preserves the rollover state across the blip.
//!
//! The WS session LIFECYCLE (connect + subscribe replay, app-level ping cadence, silent-stall
//! watchdog, read-timeout stop poll, stop-aware reconnect backoff) rides the shared
//! [`vike_bridge_core::websocket_session::market`] driver — the generalization OF this module's original crate-local
//! `reconnect_loop`/`run_session` pair, so the shape is identical by construction: the SAME `on_text`
//! closure is reused across reconnects (the rollover cursor survives a blip) and the subscribe frame is
//! replayed verbatim each session. The `{"method":"ping"}` keepalive is evaluated before EVERY read (the
//! driver's wall-clock cadence) — HL answers with a `pong` data frame, and the 30 s cadence satisfies the
//! 60 s idle-drop rule.

pub mod decode;

use std::sync::{Arc, Mutex};

use serde_json::Value;

use vike_bridge_core::data_client::event::{BarKline, DataClientEvent, DataClientMark};
use vike_bridge_core::data_client::ws::{DataClientLane, DataClientLanePlan, DataClientSpec};
use vike_bridge_core::depth::infer_tick_size;
use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
use vike_marketdata::{Bar, FundingRate};
use vike_model::now_ms;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, VenueUrls};

use crate::instrument::symbology::{InstrumentRef, Symbology};
use crate::refusals::HyperliquidFeedRefusal;
use crate::transport::consts::VENUE;
use vike_model::{AssetClass, VenueNetwork};

/// The app-level keepalive frame (the cadence knob lives in `crates/vike-venues/src/venue_timings.rs`'s `HYPERLIQUID`
/// market lane). Public for the link
/// harness, which holds the market lanes' sent frames to it (`crates/vike-bridge-core/tests/link_conformance/market.rs`).
pub const PING_MESSAGE: &str = r#"{"method":"ping"}"#;

// --- Subscribe-frame builders (pure; fixture-tested) -----------------------------------------

/// `{"method":"subscribe","subscription":{"type":"candle","coin":<coin>,"interval":<i>}}`.
pub fn subscribe_candle_message(coin: &str, interval: &str) -> String {
    serde_json::json!({
        "method": "subscribe",
        "subscription": {"type": "candle", "coin": coin, "interval": interval}
    })
    .to_string()
}

/// `{"method":"subscribe","subscription":{"type":<kind>,"coin":<coin>}}`: the one frame shape the
/// coin-keyed channels below share.
fn coin_subscribe_message(kind: &str, coin: &str) -> String {
    serde_json::json!({"method": "subscribe", "subscription": {"type": kind, "coin": coin}})
        .to_string()
}

/// The `bbo` channel's subscribe frame (top-of-book quotes).
pub fn subscribe_bbo_message(coin: &str) -> String {
    coin_subscribe_message("bbo", coin)
}

/// The `trades` channel's subscribe frame (executed prints).
pub fn subscribe_trades_message(coin: &str) -> String {
    coin_subscribe_message("trades", coin)
}

/// The `l2Book` channel's subscribe frame (full-snapshot depth).
pub fn subscribe_l2book_message(coin: &str) -> String {
    coin_subscribe_message("l2Book", coin)
}

/// The `activeAssetCtx` channel's subscribe frame: the perp asset-context stream carrying the venue's
/// REAL `markPx` (mark-slot semantics, W2-T4).
pub fn subscribe_active_asset_ctx_msg(coin: &str) -> String {
    coin_subscribe_message("activeAssetCtx", coin)
}

/// PURE: the venue mark price out of one `activeAssetCtx` frame —
/// `{"channel":"activeAssetCtx","data":{"coin":…,"ctx":{"markPx":"…",…}}}` → `markPx` (a decimal
/// string; tolerated as a bare number too). `None` for any other channel, a malformed ctx, or a
/// dead (0/neg/NaN) mark. The frame carries no timestamp — the pump stamps receive time.
pub fn mark_from_frame(v: &Value) -> Option<f64> {
    if v.get("channel").and_then(Value::as_str) != Some("activeAssetCtx") {
        return None;
    }
    let mark = v.get("data")?.get("ctx")?.get("markPx")?;
    let px = match mark {
        Value::String(s) => s.parse::<f64>().ok()?,
        other => other.as_f64()?,
    };
    vike_bridge_core::is_valid_mark(px).then_some(px)
}

/// Hyperliquid pays funding every hour, on the hour (its documented schedule; the frame states no
/// settlement time).
pub const FUNDING_INTERVAL_MS: i64 = 3_600_000;

/// PURE: the venue's public funding rate out of one `activeAssetCtx` frame — `ctx.funding`, the HOURLY
/// rate in force — as the sink's [`FundingRate`] for `series`, observed at `now_ms`: the settlement it
/// applies to is the next whole hour after `now_ms`, and its price is `ctx.oraclePx` (Hyperliquid charges
/// funding on the ORACLE price, not the mark; measured 4.7 bps apart). Decimal strings, bare numbers
/// tolerated. `None` for any other channel, a missing or non-finite rate; an oracle price that is not a
/// usable price is left out (the charge then falls back to the mark).
pub fn funding_from_frame(v: &Value, series: &str, now_ms: i64) -> Option<FundingRate> {
    if v.get("channel").and_then(Value::as_str) != Some("activeAssetCtx") {
        return None;
    }
    let ctx = v.get("data")?.get("ctx")?;
    let number = |k: &str| match ctx.get(k)? {
        Value::String(s) => s.parse::<f64>().ok(),
        other => other.as_f64(),
    };
    let rate = number("funding").filter(|r| r.is_finite())?;
    let next_ts = (now_ms.div_euclid(FUNDING_INTERVAL_MS) + 1) * FUNDING_INTERVAL_MS;
    Some(FundingRate {
        timestamp: now_ms,
        symbol: series.to_string(),
        rate,
        interval_ms: Some(FUNDING_INTERVAL_MS as u64),
        next_timestamp: Some(next_ts),
        predicted: None,
        price: number("oraclePx").filter(|p| vike_bridge_core::is_valid_mark(*p)),
    })
}

// --- The lanes ------------------------------------------------------------------------------

/// One `candle` frame, with open-time rollover close detection (HL has no `confirm` flag). `open_bar`
/// is the rollover cursor: the most recent snapshot of the currently forming candle. A NEW open-time
/// closes the previous candle (an [`DataClientEvent::InferredClose`]: `close_bar` alone); the SAME
/// open-time is an in-progress update; a strictly-OLDER open-time is a stale replay (only possible
/// right after a reconnect) and is dropped. Every non-stale frame is the forming bar, a forming
/// [`DataClientEvent::Bar`]: it ticks the bar-close cache, NOT `mark_tick` (a candle close is not the
/// venue mark; the real markPx rides the `activeAssetCtx` lane, `decode_mark`). Returns the driver's
/// frame classification: any decoded candle (even a dropped stale one) is venue DATA →
/// [`WebsocketSessionFrameOutcome::Confirm`]; a non-candle frame → [`WebsocketSessionFrameOutcome::Ignore`] (inert either way — HL
/// runs no ack watchdog, its `pump_spec` row).
fn decode_candle(
    txt: &str,
    open_bar: &mut Option<Bar>,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return WebsocketSessionFrameOutcome::Ignore; // e.g. the app-level pong / a non-JSON keepalive
    };
    let Some(bar) = crate::feed::decode::candle_to_bar(&v) else {
        return WebsocketSessionFrameOutcome::Ignore; // subscribeResponse / another channel / a malformed candle
    };
    if let Some(previous) = open_bar.as_ref() {
        if bar.timestamp < previous.timestamp {
            return WebsocketSessionFrameOutcome::Confirm; // stale replay — the current forming bar stands
        }
        if bar.timestamp > previous.timestamp {
            emit(DataClientEvent::InferredClose(previous.clone())); // previous candle is final
        }
    }
    *open_bar = Some(bar.clone());
    emit(DataClientEvent::Bar(BarKline { bar, closed: false }));
    WebsocketSessionFrameOutcome::Confirm
}

/// `subscribe_bars`'s plan, an engine session: `candle`, closed/forming by open-time rollover
/// ([`decode_candle`]; no REST seed). The cursor lives in the decode, so it survives a reconnect.
fn bars_main(websocket_url: &str, coin: &str, series: &str, interval: &str) -> DataClientLanePlan {
    let mut open_bar: Option<Bar> = None;
    session_plan(
        websocket_url,
        subscribe_candle_message(coin, interval),
        series,
        move |txt, _, emit| decode_candle(txt, &mut open_bar, emit),
    )
}

/// One engine session on the network's socket: `subscribe` replayed per session with this venue's
/// app-level ping, no seed (every HL lane is live-only), every frame through `decode` under `series`.
fn session_plan(
    websocket_url: &str,
    subscribe: String,
    series: &str,
    mut decode: impl FnMut(&str, &str, &mut dyn FnMut(DataClientEvent)) -> WebsocketSessionFrameOutcome
    + 'static,
) -> DataClientLanePlan {
    let series = series.to_string();
    DataClientLanePlan::Session {
        url: websocket_url.to_string(),
        subscribe: Some(subscribe),
        keepalive: Some(PING_MESSAGE),
        notice: None, // hyperliquid documents none: disconnects "may happen periodically and without announcement"
        seed: None,
        decode: Box::new(move |txt, emit| decode(txt, &series, emit)),
    }
}

/// One `activeAssetCtx` frame of the perp mark lane: the hourly funding rate in force is the engine's
/// [`DataClientEvent::Funding`]; the venue mark is [`DataClientEvent::Mark`] (the `PriceBoard` MARK slot)
/// and confirms. The frame carries no venue timestamp, so both are stamped with machine receive time.
fn decode_mark(
    txt: &str,
    series: &str,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return WebsocketSessionFrameOutcome::Ignore;
    };
    let now = now_ms();
    // The same frame states the hourly funding rate in force.
    if let Some(funding) = funding_from_frame(&v, series, now) {
        emit(DataClientEvent::Funding(funding));
    }
    match mark_from_frame(&v) {
        Some(px) => {
            emit(DataClientEvent::Mark(DataClientMark { px, ts: now }));
            WebsocketSessionFrameOutcome::Confirm
        }
        None => WebsocketSessionFrameOutcome::Ignore,
    }
}

/// One `bbo` frame: the quote, under the series label, is the engine's [`DataClientEvent::Quote`] (it
/// stamps the receipt time, and a quote with no venue time takes it as its `ts`, B-21) and confirms.
fn decode_quote(
    txt: &str,
    series: &str,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return WebsocketSessionFrameOutcome::Ignore;
    };
    let Some(mut q) = crate::feed::decode::bbo_to_quote(&v) else {
        return WebsocketSessionFrameOutcome::Ignore;
    };
    q.symbol = series.into();
    emit(DataClientEvent::Quote(q));
    WebsocketSessionFrameOutcome::Confirm
}

/// One `trades` frame: its prints, re-labeled to the series, are the engine's
/// [`DataClientEvent::Trades`] (it stamps each at receipt) and confirm; a frame with none is ignored.
fn decode_trades(
    txt: &str,
    series: &str,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return WebsocketSessionFrameOutcome::Ignore;
    };
    let mut ticks = crate::feed::decode::trades_from_frame(&v);
    if ticks.is_empty() {
        return WebsocketSessionFrameOutcome::Ignore;
    }
    let series = vike_marketdata::Symbol::new(series);
    for t in &mut ticks {
        t.symbol = series;
    }
    emit(DataClientEvent::Trades(ticks));
    WebsocketSessionFrameOutcome::Confirm
}

/// `subscribe_depth`'s plan, an engine session: `l2Book` → the engine's
/// [`DataClientEvent::DepthSnapshot`] (`DataClientSink::l2_snapshot`). HL sends a FULL snapshot every
/// frame (no deltas), so each frame is published verbatim as the DOM's conflating book; the tick is
/// inferred from the snapshot levels (HL has no fixed tick size), and the venue `time` is the snapshot
/// stamp. On the session driver, not the depth driver: a full-snapshot feed has no chain to watch.
fn depth_main(websocket_url: &str, coin: &str, series: &str) -> DataClientLanePlan {
    session_plan(websocket_url, subscribe_l2book_message(coin), series, |txt, _, emit| {
        let Ok(v) = serde_json::from_str::<Value>(txt) else {
            return WebsocketSessionFrameOutcome::Ignore;
        };
        let Some(snap) = crate::feed::decode::l2book_to_snapshot(&v) else {
            return WebsocketSessionFrameOutcome::Ignore;
        };
        let tick_size = infer_tick_size(&snap.bids, &snap.asks);
        let (bids, asks, ts) = (snap.bids, snap.asks, snap.time);
        emit(DataClientEvent::DepthSnapshot { tick_size, bids, asks, ts });
        WebsocketSessionFrameOutcome::Confirm
    })
}

/// **hyperliquid's half of the data-client engine** (`vike_bridge_core::data_client::ws::DataClientWebsocket`):
/// the network's public socket and REST base, and the late-populatable symbology cell every lane resolves its
/// user name's wire coin through.
pub struct DataClientSpecHyperliquid {
    network: VenueNetwork,
    /// The network's `data` socket (its `ws` row plus `/ws`) every lane dials.
    ws: String,
    /// The network's `data` REST base the background symbology reads.
    rest: String,
    /// Shared, late-populatable resolver cell. Held behind an `Arc<Mutex<…>>` (not a plain
    /// `Option`) so the app can construct the client synchronously at startup — no blocking network
    /// I/O, matching every other venue — then fill in the symbology from a background thread
    /// via [`DataClientSpecHyperliquid::symbology_cell`]. Read once per lane (rare), so the lock never
    /// contends.
    symbology: Arc<Mutex<Option<Arc<Symbology>>>>,
}

/// Whether a `subscribe_bars` on an instrument of `class` should ALSO open the `activeAssetCtx` mark stream (the
/// engine adds the venue's `mark_streams` row): every row but a SPOT pair, whose ctx channel is
/// `activeSpotAssetCtx` and carries no `markPx`. `None` (the list is not loaded yet) pairs it: the mark lane then
/// waits for the list itself and refuses a spot pair in words ([`DataClientSpecHyperliquid`]'s `plan`). Pure — the
/// ONE place the pairing predicate lives; the class is the row's, never read off the name.
fn should_pair_mark(class: Option<AssetClass>) -> bool {
    class != Some(AssetClass::CryptoSpot)
}

impl DataClientSpecHyperliquid {
    /// The spec reading `network`'s public `data` rows out of hyperliquid's rows `urls`
    /// (`VenueNetwork::Mainnet` for a caller that names no network: public market data is mainnet's),
    /// with no symbology: a lane then waits for one and refuses ([`resolve_coin`]); give it one with
    /// [`Self::with_symbology`] or [`Self::with_background_symbology`].
    ///
    /// # Errors
    /// A row's refusal: nothing is dialled.
    pub fn new(network: VenueNetwork, urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(DataClientSpecHyperliquid {
            network,
            ws: crate::config::websocket_url(network, urls, UrlChannel::Data)?,
            rest: crate::config::base_url(network, urls, UrlChannel::Data, UrlProtocol::Rest)?
                .to_string(),
            symbology: Arc::new(Mutex::new(None)),
        })
    }

    /// Thread in the resolved [`Symbology`] so a user name (`"HYPEUSDC"`, `"BTCUSDC.P"`) maps to its venue
    /// `coin` (`"@107"`, `"BTC"`) for the WS subscription.
    pub fn with_symbology(self, symbology: Arc<Symbology>) -> Self {
        *self.symbology.lock().unwrap() = Some(symbology);
        self
    }

    /// **Fill the symbology cell from the venue's own `meta` + `spotMeta`, on a background thread** (decision
    /// 0218): the non-blocking path [`Self::symbology_cell`] describes, so every user name (`HYPEUSDC`,
    /// `BTCUSDC.P`) resolves to its wire coin (`@107`, `BTC`) in the production client table. Reads on THIS feed's
    /// network (its `data` REST row). Three attempts, [`SYMBOLOGY_RETRY`] apart, then it gives up and a lane
    /// refuses in words once [`SYMBOLOGY_WAIT`] has passed.
    #[must_use]
    pub fn with_background_symbology(self) -> Self {
        let (cell, network, rest) = (self.symbology_cell(), self.network, self.rest.clone());
        let spawned = std::thread::Builder::new()
            .name("hyperliquid-feed-symbology".to_string())
            .spawn(move || {
                let transport = crate::transport::HyperliquidTransport::new(network, &rest);
                // Asked once per try: the tries below are this read's one retry layer, so a throttle is not
                // asked again three times inside each of them (`info_once`).
                let read = |kind: &str| transport.info_once(&serde_json::json!({ "type": kind }));
                let read_both = |attempt: u32| match (read("meta"), read("spotMeta")) {
                    (Ok(meta), Ok(spot)) => Some(Symbology::from_meta(&meta, &spot)),
                    (meta, spot) => {
                        let why = meta.err().or(spot.err()).map(|e| e.to_string());
                        tracing::warn!(
                            target: "vike_hyperliquid", attempt, why = ?why,
                            "the feed's spot symbology could not be read; a spot pair is priced only once it is"
                        );
                        None
                    }
                };
                let pauses = [SYMBOLOGY_RETRY; SYMBOLOGY_ATTEMPTS as usize - 1];
                if let Some(symbology) = vike_utility::retry::retry_on_pauses(
                    &pauses,
                    read_both,
                    Option::is_some,
                    std::thread::sleep,
                ) {
                    *cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(Arc::new(symbology));
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(target: "vike_hyperliquid", error = %e, "the feed's symbology thread did not start");
        }
        self
    }

    /// A clone of the shared symbology cell so a caller can populate it *after* construction — the
    /// non-blocking path: build the feed at startup, then fetch `meta`/`spotMeta` on a background
    /// thread and `*cell.lock().unwrap() = Some(Arc::new(symbol))`. Until it's filled, a lane waits
    /// ([`resolve_coin`]).
    pub fn symbology_cell(&self) -> Arc<Mutex<Option<Arc<Symbology>>>> {
        Arc::clone(&self.symbology)
    }
}

impl DataClientSpec for DataClientSpecHyperliquid {
    const VENUE: &'static str = VENUE;
    const DISPLAY: &'static str = "Hyperliquid";
    /// This venue's mark wire is documented, so its mark stream ships ON;
    /// `venue.hyperliquid.mark_streams = 0` turns it off (decision 0095).
    const MARK_STREAMS_DEFAULT_ON: bool = true;

    fn pairs_mark(&self, symbol: &str) -> bool {
        let loaded =
            self.symbology.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        should_pair_mark(loaded.as_deref().and_then(|s| s.by_symbol(symbol)).map(|i| i.asset_class))
    }

    /// Runs on the LANE's thread: the user name's coin may wait for the background symbology
    /// ([`resolve_coin`], up to [`SYMBOLOGY_WAIT`]), which must never stall the caller's thread.
    fn plan(
        &self,
        lane: DataClientLane,
        symbol: &str,
        interval: &str,
    ) -> Result<DataClientLanePlan, String> {
        let ws = &self.ws;
        let coin = || resolve_coin(&self.symbology, symbol).map_err(|refusal| refusal.to_string());
        Ok(match lane {
            DataClientLane::Bars => bars_main(ws, &coin()?, symbol, interval),
            DataClientLane::Mark => {
                let instrument = resolve_instrument(&self.symbology, symbol, SYMBOLOGY_WAIT)
                    .map_err(|refusal| refusal.to_string())?;
                if instrument.asset_class == AssetClass::CryptoSpot {
                    let symbol = symbol.to_string();
                    return Err(HyperliquidFeedRefusal::SpotHasNoMark { symbol }.to_string());
                }
                session_plan(
                    ws,
                    subscribe_active_asset_ctx_msg(&instrument.coin),
                    symbol,
                    decode_mark,
                )
            }
            DataClientLane::Quotes => {
                session_plan(ws, subscribe_bbo_message(&coin()?), symbol, decode_quote)
            }
            DataClientLane::Trades => {
                session_plan(ws, subscribe_trades_message(&coin()?), symbol, decode_trades)
            }
            DataClientLane::Depth => depth_main(ws, &coin()?, symbol),
            // The caps row refuses it before a thread exists (`book: false`): HL's `l2Book` is a full
            // snapshot every frame with no delta lane, so the lossless verb has nothing to carry.
            DataClientLane::Book => {
                return Err(HyperliquidFeedRefusal::BookSnapshotOnly.to_string());
            }
        })
    }
}

/// Max time a lane waits (on its own feed thread) for the background symbology load before it refuses: long
/// enough for [`DataClientSpecHyperliquid::with_background_symbology`]'s three attempts [`SYMBOLOGY_RETRY`] apart.
const SYMBOLOGY_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// How many times [`DataClientSpecHyperliquid::with_background_symbology`] tries the two reads, and the pause between tries.
const SYMBOLOGY_ATTEMPTS: u32 = 3;
const SYMBOLOGY_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Resolve a USER name (`"BTCUSDC.P"`, `"HYPEUSDC"`) to the venue `coin` (`"BTC"`, `"@107"`) for a WS
/// subscription: [`resolve_instrument`]'s row's coin.
///
/// # Errors
/// [`resolve_instrument`]'s.
fn resolve_coin(
    cell: &Arc<Mutex<Option<Arc<Symbology>>>>,
    symbol: &str,
) -> Result<String, HyperliquidFeedRefusal> {
    resolve_instrument(cell, symbol, SYMBOLOGY_WAIT).map(|instrument| instrument.coin)
}

/// The row of the USER name `symbol` in the feed's [`Symbology`], which is loaded on a BACKGROUND thread after the
/// feed is constructed (so startup stays network-free) — so this WAITS up to `wait` (the lanes pass
/// [`SYMBOLOGY_WAIT`]) for the cell to fill, but only WHILE it is still empty. Runs on the subscription's own feed thread, never the GUI thread, so
/// the wait is safe. This closes the startup / restored-workspace race: a chart present at launch subscribes once
/// the list is read.
///
/// # Errors
/// The list is not loaded within `wait`, or it holds no row of that user name (an old spelling such
/// as `BTC` or `HYPE/USDC` included): each in words, and nothing is subscribed. The raw name is never sent as a
/// coin (audit flag HL-3). A miss names the loaded listing again by the kept list as it is now
/// (`Symbology::named_again`, no venue read) first, so a list refreshed after the load counts.
fn resolve_instrument(
    cell: &Arc<Mutex<Option<Arc<Symbology>>>>,
    symbol: &str,
    wait: std::time::Duration,
) -> Result<InstrumentRef, HyperliquidFeedRefusal> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        let loaded = cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(symbology) = loaded {
            // Symbology loaded: resolve now — a miss won't change with more waiting.
            if let Some(instrument) = symbology.by_symbol(symbol) {
                return Ok(instrument.clone());
            }
            let again = Arc::new(symbology.named_again());
            *cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(Arc::clone(&again));
            return again.by_symbol(symbol).cloned().ok_or_else(|| {
                let fix = again.why_unnamed().map(|why| format!("; {why}")).unwrap_or_default();
                HyperliquidFeedRefusal::NoInstrument { symbol: symbol.to_string(), fix }
            });
        }
        if std::time::Instant::now() >= deadline {
            let symbol = symbol.to_string();
            return Err(HyperliquidFeedRefusal::ListNotLoaded {
                symbol,
                wait_ms: wait.as_millis(),
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;
