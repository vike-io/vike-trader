//! Live Deribit public market data → the vike-data live seam (split-plane I9): [`DataClientSpecDeribit`],
//! the venue's half of the shared [`vike_data::DataClient`] engine
//! (`vike_bridge_core::data_client::ws::DataClientWebsocket`) — the pump half of the new market-data plane
//! ([`crate::feed::decode`] holds the pure decoders). Until this module the venue HAD no live
//! market `DataClient` at all (`pump_spec` said `WebsocketSessionSpec::No`; `DataClientCapabilities::NONE`); the three public
//! feeds in [`crate::feed::dvol`]/[`crate::feed::options`] stream mark/IV/ticker overlays, not
//! `DataClient` verbs.
//!
//! The engine runs one STOPPABLE thread per subscription; every WS session is an engine `Session`
//! plan on the shared `vike_bridge_core::websocket_session::market` driver (the depth lane rides the shared depth
//! driver, below) with the knobs
//! CONSUMED from this venue's `vike_bridge_core::websocket_session::spec` row (row ownership — the values live
//! there once). All five lanes are keyless public MAINNET ([`crate::feed::options::mainnet_websocket`], the
//! `live` `data` `venue_url` row —
//! the crate's two-networks split puts every public read on the real book; the authed exec half
//! is testnet).
//!
//! The five lanes, and what each emits:
//! * **bars** (`chart.trades.{inst}.{res}`, and on a perpetual `ticker.{inst}.agg2` on the same socket
//!   for its funding rate, [`vike_data::DataClientSink::funding`]): REST warmup seed via [`crate::history`]'s pager (the
//!   SAME endpoint + column rules as backfill, so live and historical bars agree), then the live
//!   OHLCV channel. ⚠ Deribit pushes NO closed-bar flag — [`BarFolder`] infers a close when a push
//!   opens a NEWER bucket, so a bucket's `close_bar` lands with the first push of its successor
//!   (trade-driven: on a quiet instrument that can be late). Empty buckets are never fabricated,
//!   and a bucket that spanned a reconnect closes from the pre-outage state — best-effort by
//!   construction; the lossless history is the REST lane.
//! * **quotes** (`quote.{inst}`): venue-throttled top-of-book → [`vike_data::DataClientSink::quote`].
//! * **trades** (`trades.{inst}.100ms`): executed prints → [`vike_data::DataClientSink::trade`] (`amount`
//!   verbatim in venue contract units — the module-doc rule in [`crate::feed::decode`]).
//! * **book** (`book.{inst}.100ms`): the DeltaSync `change_id` chain folded into ONE standing
//!   [`L2Book`] behind an `Arc` (copy-on-write via `Arc::make_mut`, the bybit pattern) →
//!   [`vike_data::DataClientSink::book`]. A chain gap ([`DataClientEvent::Resync`]) ends the session — the reconnect's
//!   fresh subscribe delivers a fresh snapshot, so resync == resubscribe with the driver's
//!   backoff bounding the outage. The tick grid is inferred from each snapshot's own levels
//!   ([`infer_tick_size`]), so a re-seed can never keep a stale grid.
//! * **depth** (`book.{inst}.100ms`, the SAME channel the book lane folds): the conflating DOM
//!   lane — the top `DEPTH_LEVELS` (200) of the folded book, published as
//!   [`vike_data::DataClientSink::l2_snapshot`] (the bybit/okx shape). It is a SECOND socket on the same
//!   channel rather than a tap on the book lane's, because a DOM and a lossless consumer subscribe
//!   independently (one client serves both through the datahub's feed broker) and each needs its
//!   own anchor. It rides the shared `vike_bridge_core::depth::run_depth` driver — the one every CEX sibling's depth
//!   lane uses — for its dead-transport and data-freshness watchdogs, its `stream_status`
//!   disclosure and its journal voice, with `decode_depth_frame` as the venue's whole protocol.
//!   ⚠ **Why this channel and not Deribit's grouped `book.{inst}.{group}.{depth}.{interval}`**:
//!   that one is a full snapshot per frame (no chain to track) but its `depth` parameter tops out
//!   at 20 levels (measured 2026-10-04: `20` is served; `50` and `100` answer an empty `result`),
//!   and a ladder wants the 200 bybit publishes.
//!   Sizes are the venue's CONTRACT unit, verbatim (USD notional on the inverse perpetuals, base
//!   coin on the linear/USDC ones and on spot), exactly as the trades and book lanes carry them.
//!
//! Honestly not wired: the recording-lane `DataClientSink::book_update` raw deltas (the folded-state
//! `book` verb is what this feed serves — polymarket remains the one `book_update` emitter);
//! mark-stream pairing (deribit marks stream chain-wide via [`crate::feed::options`]'s markprice
//! feed, not a per-bars companion socket).
//!
//! Keepalive: every lane sends the app-level `public/test` ping the row's cadence declares — its
//! reply is the idle watchdog's only dependable inbound on a QUIET channel (a dead option's book
//! can be silent for minutes), the exact idiom [`crate::execution::user_data`] proved on the private side.

pub mod decode;
pub mod dvol;
pub mod options;

use std::sync::Arc;

use serde_json::Value;

use vike_bridge_core::data_client::event::{BarKline, DataClientBook, DataClientEvent};
use vike_bridge_core::data_client::ws::{DataClientLane, DataClientLanePlan, DataClientSpec};
use vike_bridge_core::depth::{FAULT_LOG_EVERY, WebsocketSessionDepthOperation, infer_tick_size};
use vike_bridge_core::websocket_session::market::WebsocketSessionFrameOutcome;
use vike_marketdata::{Bar, L2Book};
use vike_model::now_ms;
use vike_venues::venue_timings::DERIBIT;

use crate::feed::decode::{
    FeedRpcReply, FundingIntegral, book_channel, chart_channel, classify_rpc_reply,
    parse_book_snapshot, parse_chart_bar, parse_quote, parse_ticker_funding, parse_trades,
    public_subscribe_frame, public_unsubscribe_frame, quote_channel, route_frame, ticker_channel,
    trades_channel,
};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// The two PUBLIC mainnet addresses every lane dials, looked up once by [`DataClientSpecDeribit::new`]:
/// the market socket ([`crate::feed::options::mainnet_websocket`]) and the REST base the bars lane's warmup seed
/// reads (the `live` `data` rest row).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mainnet {
    ws: String,
    rest: String,
}

const VENUE: &str = "deribit";

/// REST warmup depth: newest N bars (the last one still forming) — the binance/bybit seed size,
/// and comfortably inside one page of the deribit chart endpoint's ~5001-row cap.
const SEED_LIMIT: i64 = 1000;

/// The app-level `public/test` keepalive payload (the row declares the CADENCE; only the payload
/// text lives in the venue — [`vike_bridge_core::websocket_session::spec::WebsocketSessionKnobs::opts`]'s contract). Its
/// reply is a JSON-RPC result OBJECT, classified [`FeedRpcReply::Other`] — inbound activity that
/// resets the idle watchdog, never mistaken for a subscribe ack.
const KEEPALIVE_PING: &str = r#"{"jsonrpc":"2.0","id":9929,"method":"public/test","params":{}}"#;

/// REST warmup seed: the newest [`SEED_LIMIT`] bars via [`crate::history::fetch_klines_range`] — the
/// same endpoint, resolution map and column rules as the backfill pager, end-anchored at now.
fn fetch_seed(rest: &str, symbol: &str, interval: &str) -> Result<Vec<Bar>, String> {
    let step = vike_marketdata::interval::interval_ms(interval)
        .ok_or_else(|| format!("deribit: unsupported interval {interval:?}"))?;
    let end = now_ms();
    crate::history::fetch_klines_range(rest, symbol, interval, end - SEED_LIMIT * step, end)
}

/// The shared first step of every lane's `on_text`: decode the frame and settle the JSON-RPC
/// reply lane — `Ok(v)` hands the parsed NOTIFICATION back to the lane; `Err(outcome)` is the
/// driver verdict for everything else (ack → `Confirm`, venue error / dead subscribe → `Fatal`,
/// keepalive reply / junk → `Ignore`).
fn settle_reply(txt: &str) -> Result<Value, WebsocketSessionFrameOutcome> {
    let Ok(v) = serde_json::from_str::<Value>(txt) else {
        return Err(WebsocketSessionFrameOutcome::Ignore);
    };
    match classify_rpc_reply(&v) {
        FeedRpcReply::Ack => Err(WebsocketSessionFrameOutcome::Confirm),
        FeedRpcReply::Error(msg) => Err(WebsocketSessionFrameOutcome::Fatal(msg)),
        FeedRpcReply::Other => Err(WebsocketSessionFrameOutcome::Ignore),
        FeedRpcReply::NotReply => Ok(v),
    }
}

// ── bars: the closed-bar inference ───────────────────────────────────────────────────────────────

/// What [`BarFolder::fold`] made of one live chart push that MOVED the picture. A struct behind an
/// `Option` rather than a `{Stale, Roll{..}}` enum: the two arms differ by a whole [`Bar`] each, so
/// the enum shape is a `clippy::large_enum_variant` (CI runs `-D warnings`) and `None`-for-stale
/// reads at least as well.
#[derive(Debug, Clone, PartialEq)]
pub struct BarRoll {
    /// The PREVIOUS bucket, present exactly when this push opened a newer one — Deribit sends no
    /// closed flag, so the successor's first push IS the close signal.
    pub closed: Option<Bar>,
    /// The current bucket's latest state.
    pub forming: Bar,
}

/// The stateful closed-bar inference over the flag-less `chart.trades` stream (module doc). One
/// per bars subscription; the state lives in the feed's `on_text` closure, so it survives
/// reconnects (the shared driver reuses the closure across sessions).
#[derive(Default)]
pub struct BarFolder {
    forming: Option<Bar>,
}

impl BarFolder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one pushed bar: same bucket → an updated forming picture; a NEWER bucket → the held
    /// bucket closes and the new one forms; an OLDER bucket (a replayed/out-of-order push) →
    /// `None`, emitting nothing.
    pub fn fold(&mut self, bar: Bar) -> Option<BarRoll> {
        let closed = match &self.forming {
            Some(current) if bar.timestamp < current.timestamp => return None,
            Some(current) if bar.timestamp > current.timestamp => Some(current.clone()),
            _ => None,
        };
        self.forming = Some(bar.clone());
        Some(BarRoll { closed, forming: bar })
    }
}

/// One frame of a bars session: a perpetual's ticker frame lands its funding (the engine's
/// [`DataClientEvent::Funding`]) and does NOT confirm the session (the bars channel's liveness is the
/// chart's alone, so a stalled chart never reads live behind the ticker); a chart push folds through
/// `folder` and confirms: the bucket it closed is an [`DataClientEvent::InferredClose`] (Deribit marks
/// none), then the forming bucket is a forming [`DataClientEvent::Bar`], which ticks the bar-close cache.
fn bars_frame(
    txt: &str,
    symbol: &str,
    folder: &mut BarFolder,
    funding_integral: &mut FundingIntegral,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    let v = match settle_reply(txt) {
        Ok(v) => v,
        Err(outcome) => return outcome,
    };
    if let Some(funding) = parse_ticker_funding(&v, symbol, funding_integral) {
        emit(DataClientEvent::Funding(funding));
        return WebsocketSessionFrameOutcome::Ignore;
    }
    let Some(bar) = parse_chart_bar(&v) else {
        return WebsocketSessionFrameOutcome::Ignore;
    };
    let Some(roll) = folder.fold(bar) else {
        return WebsocketSessionFrameOutcome::Ignore; // an older bucket — a replayed/out-of-order push
    };
    if let Some(c) = roll.closed {
        // the successor's first push IS the close signal — lossless lane
        emit(DataClientEvent::InferredClose(c));
    }
    emit(DataClientEvent::Bar(BarKline { bar: roll.forming, closed: false }));
    WebsocketSessionFrameOutcome::Confirm
}

/// **The bar lane's refusal of an interval the `chart.trades` resolution table has no code for** (the table
/// has gaps: no 4h, no weekly, no monthly), asked by `subscribe_bars` on the CALLER's thread, so the
/// subscription is refused rather than spawned into a lane that sets a status line and exits (a silent dead
/// bar lane: closed bars are what fire a strategy's per-bar watchdogs). Moved here from the gatewayhub's
/// `BarIntervalGate` (owner ruling B-22), its text verbatim (the tradehub's old `deribit_plan` words).
pub fn refuse_bars_interval(interval: &str) -> Result<(), String> {
    crate::history::resolution_code(interval).map(|_| ()).map_err(|e| {
        format!(
            "deribit streams `chart.trades` by RESOLUTION code and has none for interval {interval:?} \
             ({e} — `vike_deribit::history::resolution_code` is the table, and its enum has gaps: \
             there is no 4h), so the bar lane's thread would set a status line and exit — and \
             closed bars are what fire the margin-call watchdog and the drawdown latch. Name an \
             interval this venue serves"
        )
    })
}

/// `subscribe_bars`'s plan, an engine session: the REST warmup ONCE (the engine's seed, the newest bar
/// still forming; not per session, the sibling venues' rule), then the live chart channel through
/// [`BarFolder`] ([`bars_frame`]). The resolution was checked by the plan (and, on the caller's thread,
/// by [`refuse_bars_interval`]).
fn bars_main(net: &Mainnet, symbol: &str, interval: &str, resolution: &str) -> DataClientLanePlan {
    // A PERPETUAL's bars socket also carries its ticker, for the funding rate alone
    // (`parse_ticker_funding`: Deribit's continuous accrual integrated over each hour, settled hourly
    // on the paper side).
    let mut channels = vec![chart_channel(symbol, resolution)];
    if symbol.ends_with("-PERPETUAL") {
        channels.push(ticker_channel(symbol));
    }
    let (rest, series, interval) = (net.rest.clone(), symbol.to_string(), interval.to_string());
    let seed_series = series.clone();
    let mut folder = BarFolder::new();
    let mut funding_integral = FundingIntegral::new();
    DataClientLanePlan::Session {
        url: net.ws.clone(),
        subscribe: Some(public_subscribe_frame(&channels)),
        keepalive: Some(KEEPALIVE_PING),
        seed: Some(Box::new(move || fetch_seed(&rest, &seed_series, &interval))),
        decode: Box::new(move |txt, emit| {
            bars_frame(txt, &series, &mut folder, &mut funding_integral, emit)
        }),
    }
}

/// One live-only lane (`quotes`, `trades`): `channel`'s public subscribe, replayed on every session, with
/// this venue's `public/test` keepalive; every frame through `decode` (live-only: no seed — a quote or
/// print stream has no history to splice). The engine writes the healthy text and
/// `"{symbol} {lane} ws error (reconnecting): {e}"`.
fn live_plan(
    ws: &str,
    channel: String,
    decode: fn(&str, &mut dyn FnMut(DataClientEvent)) -> WebsocketSessionFrameOutcome,
) -> DataClientLanePlan {
    DataClientLanePlan::Session {
        url: ws.to_string(),
        subscribe: Some(public_subscribe_frame(&[channel])),
        keepalive: Some(KEEPALIVE_PING),
        seed: None,
        decode: Box::new(decode),
    }
}

/// One `quote.{inst}` frame (the venue-throttled top of book): the quote is the engine's
/// [`DataClientEvent::Quote`] (it stamps the receipt time, and a quote with no venue time takes it, B-21)
/// and confirms; the JSON-RPC reply lane is settled first ([`settle_reply`]).
fn decode_quote(txt: &str, emit: &mut dyn FnMut(DataClientEvent)) -> WebsocketSessionFrameOutcome {
    let v = match settle_reply(txt) {
        Ok(v) => v,
        Err(outcome) => return outcome,
    };
    let Some(q) = parse_quote(&v) else {
        return WebsocketSessionFrameOutcome::Ignore;
    };
    emit(DataClientEvent::Quote(q));
    WebsocketSessionFrameOutcome::Confirm
}

/// One `trades.{inst}.100ms` frame: its prints are the engine's [`DataClientEvent::Trades`] (it stamps
/// each at receipt) and confirm; a frame with none is ignored.
fn decode_trades(txt: &str, emit: &mut dyn FnMut(DataClientEvent)) -> WebsocketSessionFrameOutcome {
    let v = match settle_reply(txt) {
        Ok(v) => v,
        Err(outcome) => return outcome,
    };
    let ticks = parse_trades(&v);
    if ticks.is_empty() {
        return WebsocketSessionFrameOutcome::Ignore;
    }
    emit(DataClientEvent::Trades(ticks));
    WebsocketSessionFrameOutcome::Confirm
}

/// One `book.*` frame of the lossless book lane, folded into `book`: ONE standing [`L2Book`] behind
/// an `Arc` (publish = refcount bump; folds copy-on-write via `Arc::make_mut`, off the single-writer
/// core — the bybit pattern), fed by [`route_frame`]'s DeltaSync chain. Each session's SNAPSHOT
/// rebuilds the book on the tick grid inferred from its own levels; every folded change is the
/// engine's [`DataClientEvent::Book`] (the whole book on the lossless `book` verb) and confirms.
///
/// ⚠ A chain gap is a session fault BY DESIGN: the driver's reconnect + resubscribe IS the resync
/// (fresh subscribe → fresh snapshot), so a perfectly healthy deribit book feed writes the engine's
/// `Degraded`-reading `"{symbol} book ws error (reconnecting): …"` as ORDINARY OPERATION, and the next
/// session's first confirmed frame writes the healthy text back. (It is also the concrete writer
/// `StrategyLanesLive::reconcile_feed_statuses` withholds deribit's row for; that verdict is unchanged — see the
/// write-frequency asymmetry argument in its doc.)
fn book_frame(
    txt: &str,
    symbol: &str,
    book: &mut Arc<L2Book>,
    emit: &mut dyn FnMut(DataClientEvent),
) -> WebsocketSessionFrameOutcome {
    let v = match settle_reply(txt) {
        Ok(v) => v,
        Err(outcome) => return outcome,
    };
    if let Some((_sequence, bids, asks)) = parse_book_snapshot(&v) {
        // A snapshot (re)builds the standing book on ITS inferred tick grid before the
        // routed apply below folds it — so a resync can never inherit a stale grid.
        *book = Arc::new(L2Book::new(infer_tick_size(&bids, &asks)));
    }
    match route_frame(txt, symbol, Arc::make_mut(book)) {
        DataClientEvent::BookUpdated { .. } => {
            emit(DataClientEvent::Book(DataClientBook(Arc::clone(book))));
            WebsocketSessionFrameOutcome::Confirm
        }
        DataClientEvent::Resync => WebsocketSessionFrameOutcome::Fatal(format!(
            "{symbol} book change_id chain broke (frames dropped) — resyncing via a \
             fresh subscribe"
        )),
        DataClientEvent::Quote(_)
        | DataClientEvent::Trade(_)
        | DataClientEvent::Trades(_)
        | DataClientEvent::Bar(_)
        | DataClientEvent::InferredClose(_)
        | DataClientEvent::Mark(_)
        | DataClientEvent::Funding(_)
        | DataClientEvent::Book(_)
        | DataClientEvent::DepthSnapshot { .. }
        | DataClientEvent::Ignored => WebsocketSessionFrameOutcome::Ignore,
    }
}

/// `subscribe_book`'s plan, an engine session on `book.{inst}.100ms` ([`book_frame`]), with no seed:
/// each session's subscribe delivers the snapshot that anchors the chain.
fn book_main(ws: &str, symbol: &str) -> DataClientLanePlan {
    let series = symbol.to_string();
    let mut book = Arc::new(L2Book::new(0.0)); // rebuilt (grid inferred) on the first snapshot
    DataClientLanePlan::Session {
        url: ws.to_string(),
        subscribe: Some(public_subscribe_frame(&[book_channel(symbol)])),
        keepalive: Some(KEEPALIVE_PING),
        seed: None,
        decode: Box::new(move |txt, emit| book_frame(txt, &series, &mut book, emit)),
    }
}

// ── depth: the DOM's conflating lane ─────────────────────────────────────────────────────────────

/// Levels per side published to the DOM — bybit's number (`crates/bridges/bybit/src/feed/depth.rs`'s
/// `DEPTH_LEVELS`) and the datahub's own per-subscriber ceiling
/// (`crates/vike-datahub-client/src/market.rs`'s `MARKET_DATA_DEPTH_LEVELS_CEILING`), so this feed never
/// withholds a row any consumer can ask for.
const DEPTH_LEVELS: usize = 200;

// The four depth timings are this venue's `depth` block in `crates/vike-venues/src/venue_timings.rs`
// (`DERIBIT`), which carries each value and its reason (decision 0160), handed to the driver as the
// row: the siblings' 3 s backoff; a 60 s dead-transport watchdog (three 20 s `public/test` ping cycles,
// the SAME figure this venue's pump row declares for its other four lanes,
// `crates/vike-bridge-core/src/websocket_session/spec_tests.rs`'s `the_tables_agree_where_they_state_one_fact`
// holds the two equal); the siblings' 120 s freshness limit (⚠ a far-dated OPTION's book can be silent
// for longer and will read `Stale` there — true, a disclosure rather than a fault); and the siblings'
// 300 s timed re-seed — parity rather than a second line of defence, since the `prev_change_id` chain
// already turns every dropped frame into a [`WebsocketSessionDepthOperation::Gap`] (MEASURED 2026-10-04: `BTC-PERPETUAL`'s
// snapshot is 46 KB; a deep spot book's, `BTC_USDC`'s, is 1.1 MB). The stop-poll cadence is the pump
// row's.

/// The venue EVENT-TIME (epoch-ms) of a `book.*` notification — `params.data.timestamp` — for the
/// §B data-freshness watchdog (`0` if absent → the driver falls back to receive-time). Re-parses
/// just that field, so it serves ONLY the once-per-seed first-snapshot arm of [`fold_depth_frame`]
/// (which reads that snapshot with `parse_book_snapshot`, not `route_frame`): every anchored frame
/// takes its time from `DataClientEvent::BookUpdated { ts_ms }`, one parse per change.
fn frame_ts_ms(txt: &str) -> i64 {
    serde_json::from_str::<Value>(txt)
        .ok()
        .and_then(|v| v.pointer("/params/data/timestamp").and_then(Value::as_i64))
        .unwrap_or(0)
}

/// The venue's REFUSAL of the subscribe itself, if `txt` is one: a JSON-RPC `error` envelope, or a
/// `result` array naming no channel — what a misspelled or expired instrument gets. MEASURED
/// 2026-10-04: `book.NOT-AN-INSTRUMENT.100ms` answers `{"result":[]}` and then NOTHING, ever, so
/// without this a bad key would idle behind a keepalive'd socket until the freshness watchdog said
/// `Stale` two minutes later. `None` for every other frame — an ack, the keepalive's reply, data.
fn subscribe_refusal(txt: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(txt).ok()?;
    match classify_rpc_reply(&v) {
        FeedRpcReply::Error(why) => Some(why),
        FeedRpcReply::Ack | FeedRpcReply::Other | FeedRpcReply::NotReply => None,
    }
}

/// Fold one `book.*` WS frame into the (maybe not-yet-anchored) book slot — the depth driver's
/// `decode` body, named so the frame→[`WebsocketSessionDepthOperation`] mapping is unit-testable without a socket.
///
/// Before the first `snapshot` every frame is [`WebsocketSessionDepthOperation::Ignored`] (the ack, a keepalive reply, a
/// change that outran its anchor). The snapshot builds the book on the tick grid inferred from its
/// own levels and anchors the chain at its `change_id`; from then on frames fold through the
/// fixture-tested [`route_frame`], whose `prev_change_id` rule maps a dropped frame to
/// [`WebsocketSessionDepthOperation::Gap`] — on which the shared driver discloses the gap and reconnects + re-seeds
/// immediately. A replayed frame (`change_id` already reflected) and a quote/trade row on a mixed
/// socket are [`WebsocketSessionDepthOperation::Ignored`] with the book untouched.
fn fold_depth_frame(
    txt: &str,
    symbol: &str,
    book: &mut Option<Arc<L2Book>>,
) -> WebsocketSessionDepthOperation {
    match book.as_mut().map(Arc::make_mut) {
        None => {
            let Ok(v) = serde_json::from_str::<Value>(txt) else {
                return WebsocketSessionDepthOperation::Ignored;
            };
            let Some((seq, bids, asks)) = parse_book_snapshot(&v) else {
                return WebsocketSessionDepthOperation::Ignored;
            };
            let mut bk = L2Book::new(infer_tick_size(&bids, &asks));
            bk.apply_snapshot(seq, &bids, &asks);
            *book = Some(Arc::new(bk));
            WebsocketSessionDepthOperation::Updated(frame_ts_ms(txt))
        }
        Some(bk) => match route_frame(txt, symbol, bk) {
            DataClientEvent::BookUpdated { timestamp_ms: ts_ms } => {
                WebsocketSessionDepthOperation::Updated(ts_ms)
            }
            DataClientEvent::Resync => WebsocketSessionDepthOperation::Gap,
            DataClientEvent::Quote(_)
            | DataClientEvent::Trade(_)
            | DataClientEvent::Trades(_)
            | DataClientEvent::Bar(_)
            | DataClientEvent::InferredClose(_)
            | DataClientEvent::Mark(_)
            | DataClientEvent::Funding(_)
            | DataClientEvent::Book(_)
            | DataClientEvent::DepthSnapshot { .. }
            | DataClientEvent::Ignored => WebsocketSessionDepthOperation::Ignored,
        },
    }
}

/// The depth driver's whole `decode`: [`fold_depth_frame`], preceded — only while the book is still
/// unanchored, which is the one window a subscribe refusal can arrive in — by the refusal check.
///
/// ⚠ **A refused subscribe is spoken, and throttled.** The venue answers an unknown instrument with
/// an empty `result` and then silence ([`subscribe_refusal`]), so it maps to [`WebsocketSessionDepthOperation::Gap`]: the
/// driver reconnects after the depth row's backoff, exactly as this venue's other four lanes do on the
/// same refusal (`classify_rpc_reply` → `Fatal`). The driver's own journal line for that fault
/// would say "sequence gap", which is wrong, so the real words go out here, at most once per
/// [`FAULT_LOG_EVERY`] — the driver's own cadence for a lane that keeps faulting. `spoke_at` is the
/// caller's throttle state (on the injected clock `now`), kept across reconnects the way the
/// driver's own fault log is.
fn decode_depth_frame(
    txt: &str,
    symbol_name: &str,
    book: &mut Option<Arc<L2Book>>,
    spoke_at: &mut Option<i64>,
    now: i64,
) -> WebsocketSessionDepthOperation {
    if book.is_none()
        && let Some(why) = subscribe_refusal(txt)
    {
        if spoke_at.is_none_or(|t| now - t >= FAULT_LOG_EVERY.as_millis() as i64) {
            *spoke_at = Some(now);
            tracing::warn!(
                target: "vike_deribit::feed",
                symbol = %symbol_name,
                "depth subscribe refused — {why}"
            );
        }
        return WebsocketSessionDepthOperation::Gap;
    }
    fold_depth_frame(txt, symbol_name, book)
}

/// `subscribe_depth`'s plan: the DOM's conflating lane on the shared depth driver. Deribit's protocol is
/// WS-seeded (no REST): the seed answers `None`, the first `snapshot` frame builds the book and every
/// change after it is folded and re-published as an `l2_snapshot` of the top [`DEPTH_LEVELS`]
/// ([`decode_depth_frame`] is the whole protocol). The timed re-seed drops and re-subscribes the book
/// channel on the open socket, its fresh `snapshot` rebuilding the book ([`fold_depth_frame`]'s `None`
/// arm, the session's own seed).
fn depth_main(ws: &str, symbol: &str) -> DataClientLanePlan {
    let sub = public_subscribe_frame(&[book_channel(symbol)]);
    let unsubscribe = public_unsubscribe_frame(&[book_channel(symbol)]);
    let owned_symbol = symbol.to_string();
    let mut refusal_spoke_at: Option<i64> = None;
    DataClientLanePlan::Depth {
        url: ws.to_string(),
        subscribe: Some(sub.clone()),
        keepalive: Some(KEEPALIVE_PING),
        resubscribe: vec![unsubscribe, sub],
        seed: Box::new(|| None),
        decode: Box::new(move |txt, book| {
            decode_depth_frame(txt, &owned_symbol, book, &mut refusal_spoke_at, now_ms())
        }),
        levels: DEPTH_LEVELS,
        timings: DERIBIT.depth(),
    }
}

/// **deribit's half of the data-client engine** (`vike_bridge_core::data_client::ws::DataClientWebsocket`): the
/// two public mainnet addresses, looked up once.
#[derive(Debug)]
pub struct DataClientSpecDeribit {
    net: Mainnet,
}

impl DataClientSpecDeribit {
    /// The spec over deribit's address rows `urls`: every lane dials the `live` `data` ones, mainnet
    /// whatever any account's tier.
    ///
    /// # Errors
    /// A row's refusal: nothing is dialled.
    pub fn new(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        Ok(DataClientSpecDeribit {
            net: Mainnet {
                ws: crate::feed::options::mainnet_websocket(urls)?,
                rest: urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?.to_string(),
            },
        })
    }
}

impl DataClientSpec for DataClientSpecDeribit {
    const VENUE: &'static str = VENUE;
    const DISPLAY: &'static str = "Deribit";
    /// No mark pairs a bars subscription here (deribit's marks stream chain-wide through
    /// [`crate::feed::options`]'s markprice feed), and the venue has no `mark_streams` row.
    const MARK_STREAMS_DEFAULT_ON: bool = false;

    /// An interval the `chart.trades` resolution table has no code for, refused on the caller's thread
    /// (owner ruling B-22).
    fn refuse_bars(&self, interval: &str) -> Result<(), String> {
        refuse_bars_interval(interval)
    }

    fn plan(
        &self,
        lane: DataClientLane,
        symbol: &str,
        interval: &str,
    ) -> Result<DataClientLanePlan, String> {
        let net = &self.net;
        Ok(match lane {
            DataClientLane::Bars => {
                // A permanent config error ends the lane (no reconnect fixes an interval the venue does
                // not serve); `refuse_bars` already refused it on the caller's thread.
                let resolution = crate::history::resolution_code(interval)?;
                bars_main(net, symbol, interval, resolution)
            }
            DataClientLane::Quotes => live_plan(&net.ws, quote_channel(symbol), decode_quote),
            DataClientLane::Trades => live_plan(&net.ws, trades_channel(symbol), decode_trades),
            DataClientLane::Book => book_main(&net.ws, symbol),
            DataClientLane::Depth => depth_main(&net.ws, symbol),
            DataClientLane::Mark => return Err("deribit pairs no mark stream".into()),
        })
    }
}

#[path = "feed_tests.rs"]
#[cfg(test)]
mod feed_tests;
