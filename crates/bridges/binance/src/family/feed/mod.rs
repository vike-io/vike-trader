//! The Binance-grammar live market-data lanes — shared by vike-binance (`crate::feed`'s
//! `DataClientSpecBinance`) and vike-aster (`vike_aster::feed`'s `DataClientSpecAster`), each
//! passing its own [`FamilySpec`] (F17, dedup rung 2).
//!
//! Aster's kline/depth wire grammar is Binance-verbatim (same `@kline_<interval>` /
//! `@depth@100ms` stream names, 12-element klines, the same two depth-sequence grammars — see
//! [`crate::family::feed::tick`]'s module doc, which is where the two are told apart), so the two venues'
//! `feed/mod.rs` files were copies. The frame decoders and each lane's plan live here once.
//!
//! **The one data-client engine runs them** (`vike_bridge_core::data_client::ws::DataClientWebsocket`,
//! W2.4): every lane's thread, its session lifecycle on the shared `market_pump` and depth drivers,
//! its status words and the sink emission are the engine's. Only the PROTOCOL stays here: the
//! URL-path subscription (no subscribe frame — see `pump_opts`'s br7 note), the fixture-tested frame
//! decoders, the REST seeds and the DOM book fold. [`feed_main`] (bars), [`mark_main`] (the perp
//! mark), [`depth_main`] (the DOM) and [`trades_main`] (the trade tape, whose loop stays in
//! [`crate::family::feed::tick::trades`]) each answer one lane's `DataClientLanePlan`: they keep the names they carried as
//! the lanes' thread bodies, which every citation of them still reads.
//!
//! **Host resolution is the one real delta** and is passed in, never decided here: Binance hands in
//! a [`super::UrlTable`] from its `venue_url` rows ([`crate::feed::BinanceTables`], its linear or its
//! COIN-M table), never env-resolved; Aster maps its own rows into the same shape. The kline lane also takes the
//! book's volume column ([`feed_main`]'s `volume`): binance COIN-M states its base-asset amount in the
//! frame's `q` where every other book of both venues states it in `v`.

pub(crate) mod decode;
pub mod tick;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::data_client::event::{BarKline, DataClientEvent, DataClientMark};
use vike_bridge_core::data_client::ws::DataClientLanePlan;
use vike_bridge_core::depth::{WebsocketSessionDepthOperation, infer_tick_size};
use vike_bridge_core::klines::kline_to_bar;
use vike_bridge_core::websocket_session::market::{
    WebsocketSessionFrameOutcome, WebsocketSessionMarketOptions,
};
use vike_bridge_core::websocket_session::spec;
use vike_marketdata::{Bar, FundingRate, L2Book};
use vike_venues::venue_timings::{BINANCE_SPOT, VenueTimingsDepth};

use super::{FamilySpec, UrlTable};
use crate::family::feed::tick::{DepthOutcome, DepthSnapshot, apply_depth_event};
use crate::family::history::klines::VolumeColumn;

/// Newest N klines for the REST warmup seed.
pub const SEED_LIMIT: usize = 1000;

// --- kline frames --------------------------------------------------------------------------

#[derive(Deserialize)]
struct KlineMessage {
    k: KlineData,
}
#[derive(Deserialize)]
struct KlineData {
    t: i64,
    o: String,
    h: String,
    l: String,
    c: String,
    v: String,
    /// The OTHER amount (quote asset on spot, USDⓈ-M and aster; the base asset on COIN-M), read
    /// only for [`VolumeColumn::Index7`]. A raw value, so that on the `v` books a `q` of any shape
    /// costs the frame nothing, as when it was not declared.
    #[serde(default)]
    q: Option<serde_json::Value>,
    x: bool,
}

/// Decode ONE `@kline_<interval>` text frame — the bar plus whether the venue marked it CLOSED (`x`) —
/// the bar's volume read from the field `volume` names
/// (`v` for [`VolumeColumn::Index5`], `q` for [`VolumeColumn::Index7`]: the frame twin of the REST
/// row's column, see [`VolumeColumn`]). `Ok(None)` = not a kline frame (the caller skips it); `Err` =
/// a malformed numeric field (the caller drops the session so it reconnects) — exactly the two-tier
/// handling both venues' pre-driver session loops had inline. The volume defaults to 0.0 on a miss.
pub fn decode_kline_frame(
    txt: &str,
    volume: VolumeColumn,
) -> Result<Option<BarKline>, std::num::ParseFloatError> {
    let Ok(m) = serde_json::from_str::<KlineMessage>(txt) else {
        return Ok(None);
    };
    let amount = match volume {
        VolumeColumn::Index5 => Some(m.k.v.as_str()),
        VolumeColumn::Index7 => m.k.q.as_ref().and_then(serde_json::Value::as_str),
    };
    let bar = kline_to_bar(
        m.k.t,
        m.k.o.parse::<f64>()?,
        m.k.h.parse::<f64>()?,
        m.k.l.parse::<f64>()?,
        m.k.c.parse::<f64>()?,
        amount.and_then(|a| a.parse::<f64>().ok()).unwrap_or(0.0),
    );
    Ok(Some(BarKline { bar, closed: m.k.x }))
}

/// The `@kline_<interval>` WS URL for a resolved (bare) stream host.
pub fn kline_websocket_url(websocket_host: &str, api_symbol: &str, interval: &str) -> String {
    format!("{websocket_host}/ws/{}@kline_{}", api_symbol.to_lowercase(), interval)
}

/// The `@depth@100ms` WS URL for a resolved (bare) stream host.
pub fn depth_websocket_url(websocket_host: &str, symbol: &str) -> String {
    format!("{websocket_host}/ws/{}@depth@100ms", symbol.to_lowercase())
}

// --- the kline lane --------------------------------------------------------------------------

/// The shared [`WebsocketSessionMarketOptions`] the family's aggTrade pump ([`crate::family::feed::tick::trades`]) runs with — CONSUMED
/// from the calling venue's `WebsocketSessionSpec` row ([`spec::row`], row ownership; the binance and
/// aster rows are pinned identical since the family shares these pumps). The kline and mark lanes run
/// on the same row: the engine builds their opts from it with this family's payloads, which are none:
///
/// - `subscribe: None` — this family encodes the subscription IN THE URL PATH
///   (`/ws/<symbol>@kline_<interval>` / `…@aggTrade`); no subscribe frame is ever sent.
/// - `ack_timeout: None` — **net-hardening br7: why the subscribe-ack handshake the OKX/Bybit
///   kline feeds gained does NOT apply here.** Those venues open ONE public socket and send an
///   `{op:"subscribe"}` frame the venue replies to with an ACK or an `error`; a silently-dropped
///   ack/error was the hole br7 closed. This family has no subscribe frame and thus NO ack frame
///   to time out on and NO `event:"error"` reject frame to attribute. A bad symbol/interval fails
///   at `tungstenite::connect` (already a disclosed session error → reconnect) or opens but
///   streams nothing — the data-starvation case a FRESHNESS watchdog covers, which the kline bar
///   feed doesn't run (only the depth feed does; see [`depth_main`]'s `DEPTH_TIMINGS` freshness
///   threshold). So there is nothing to ack and a fake ack was deliberately NOT forced; the
///   URL-subscribe contract is the note.
/// - `keepalive: None` — the venue server pings and the driver's stream auto-pongs; these lanes
///   never sent an app-level ping, so none is invented here.
/// - `idle_threshold: None` — no idle watchdog on these lanes (as before).
pub(super) fn pump_opts(venue: &str) -> WebsocketSessionMarketOptions<'static> {
    spec::row(venue).knobs().opts(None, None)
}

/// The bars lane's plan. `fetch_seed` is the venue's own kline REST warm-up (each crate's
/// `history::fetch_klines_latest`, already bound to its hosts), which the engine runs ONCE per feed before
/// the first dial: a reconnect does not re-seed. Then the `@kline_<interval>` socket, on the market
/// host the spec's [`UrlTable`] names for `is_perp` (the `KlineMessage` frame shape is identical on both).
///
/// `api_symbol` is the `.P`-stripped exchange symbol the REST seed and the WS URL use; the engine keys
/// every sink call on the SERIES symbol it subscribed (`.P`-suffixed for a perp — the core key). Each
/// decoded kline is one `Bar` event (the engine ticks the bar-close cache, then the lossless
/// `close_bar` or conflating `forming_bar` lane); a malformed numeric field ends the session as an
/// error so it reconnects — the pre-driver `decode_kline_frame(txt)?` propagation, same status text.
///
/// `volume` is the frame field the book states its base-asset amount in ([`VolumeColumn`]): `Index5`
/// (`v`) on spot, USDⓈ-M and every aster book, `Index7` (`q`) on binance COIN-M, where `v` is a
/// contract count. The caller names the book, as it names the hosts; `fetch_seed` must read the same
/// column, or the seed and the stream disagree in one series.
///
/// ⚠ **The seed's healthy write (the engine's) matters most on this family.** It subscribes by URL
/// PATH, so its `pump_spec` row declares BOTH `ack_timeout: None` and `idle_threshold: None`: a session
/// that connects and delivers nothing never confirms, and a Confirm-keyed disclosure alone would leave
/// the FIRST session with no healthy string at all. The mark and trades lanes have no seed write, so on
/// them a session that connects, delivers nothing this venue's classifier calls a frame, and therefore
/// neither CONFIRMS nor FAULTS, leaves whatever error string that lane last wrote standing on the shared
/// line — the 2026-09-10 latch, narrowed to those two lanes. bybit/okx/deribit/ig cannot reach that
/// state: their rows declare `ack_timeout: Some(10s)`. `StrategyLanesLive::reconcile_feed_statuses` withholds the
/// CEX row on any start running more than one lane on this handle, so the remaining exposure is a GUI
/// string, not a suppressed reconcile leg. Structural closure is an `idle_threshold` on the
/// `"binance" | "aster"` row of `vike_bridge_core::websocket_session::spec`'s `row`: a behaviour change
/// that would start faulting quiet sessions, so it needs its own measurement — NAMED FOLLOW-UP.
pub fn feed_main(
    api_symbol: &str,
    interval: &str,
    is_perp: bool,
    volume: VolumeColumn,
    urls: &UrlTable,
    fetch_seed: impl FnOnce(&str, &str, bool) -> Result<Vec<Bar>, String> + 'static,
) -> DataClientLanePlan {
    let (api, iv) = (api_symbol.to_string(), interval.to_string());
    DataClientLanePlan::Session {
        url: kline_websocket_url(&urls.market_websocket(is_perp), api_symbol, interval),
        subscribe: None,
        keepalive: None,
        seed: Some(Box::new(move || fetch_seed(&api, &iv, is_perp))),
        decode: Box::new(move |txt, emit| match decode_kline_frame(txt, volume) {
            Ok(Some(kline)) => {
                emit(DataClientEvent::Bar(kline));
                WebsocketSessionFrameOutcome::Confirm
            }
            Ok(None) => WebsocketSessionFrameOutcome::Ignore, // not a kline frame
            // A malformed numeric field drops the session so it reconnects (the pre-driver
            // two-tier handling); the `ParseFloatError` Display is the same status text as before.
            Err(e) => WebsocketSessionFrameOutcome::Fatal(e.to_string()),
        }),
    }
}

// --- live perp mark-price stream (mark-slot semantics, W2-T4) --------------------------------
// The USDⓈ-M `<symbol>@markPrice@1s` stream: the venue's REAL mark price (the price liquidation
// and funding key off), pushed every second. Feeds `DataClientSink::mark_tick` — the `PriceBoard`
// MARK slot — while the kline pump's candle closes ride `bar_close_tick`. Perp-only: spot has no
// mark price. Same URL-path-subscribe pump shape as the kline lane (no subscribe frame, no ack —
// the br7 note on `pump_opts` applies verbatim).

#[derive(Deserialize)]
struct MarkMessage {
    e: String,
    #[serde(rename = "E")]
    event_ts: i64,
    p: String,
    /// The current funding rate (a decimal string; empty on a contract with none). Read as a raw value
    /// so a malformed funding field can never cost the frame its mark.
    #[serde(default)]
    r: Option<serde_json::Value>,
    /// The next funding settlement time (ms), raw for the same reason.
    #[serde(rename = "T", default)]
    next_funding_ts: Option<serde_json::Value>,
}

/// One decoded `markPriceUpdate` frame: the venue's REAL mark price + its event time (ms), and the
/// public funding rate with its next settlement time when the frame states both (`r`, `T`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkFrame {
    pub px: f64,
    pub ts: i64,
    /// `(rate, next settlement ms)`; `None` when either is absent or `r` is not a finite decimal (a
    /// funding field never ends the session: the mark is the lane's data).
    pub funding: Option<(f64, i64)>,
}

impl MarkFrame {
    /// The frame's funding as the sink's [`FundingRate`] for `series_symbol`, when it states one. The
    /// frame states no interval (binance's is per symbol), so `interval_ms` is `None`.
    pub fn funding_rate(&self, series_symbol: &str) -> Option<FundingRate> {
        let (rate, next_ts) = self.funding?;
        Some(FundingRate {
            timestamp: self.ts,
            symbol: series_symbol.to_string(),
            rate,
            interval_ms: None,
            next_timestamp: Some(next_ts),
            predicted: None,
            price: None,
        })
    }
}

/// Decode ONE `@markPrice@1s` text frame. `Ok(None)` = not a mark frame OR a dead (0/neg/NaN)
/// mark price (skipped in place — a conflating lane tolerates a dropped frame); `Err` = a
/// malformed `p` decimal (the caller drops the session so it reconnects) — the same two-tier
/// handling as [`decode_kline_frame`].
pub fn decode_mark_frame(txt: &str) -> Result<Option<MarkFrame>, std::num::ParseFloatError> {
    let Ok(m) = serde_json::from_str::<MarkMessage>(txt) else {
        return Ok(None);
    };
    if m.e != "markPriceUpdate" {
        return Ok(None);
    }
    let px = m.p.parse::<f64>()?;
    if !vike_bridge_core::is_valid_mark(px) {
        return Ok(None);
    }
    let rate =
        m.r.as_ref()
            .and_then(serde_json::Value::as_str)
            .and_then(|r| r.parse::<f64>().ok())
            .filter(|r| r.is_finite());
    let next = m.next_funding_ts.as_ref().and_then(serde_json::Value::as_i64).filter(|t| *t > 0);
    let funding = rate.zip(next);
    Ok(Some(MarkFrame { px, ts: m.event_ts, funding }))
}

/// The `@markPrice@1s` WS URL for a resolved (bare) PERP stream host.
pub fn mark_websocket_url(websocket_host: &str, api_symbol: &str) -> String {
    format!("{websocket_host}/ws/{}@markPrice@1s", api_symbol.to_lowercase())
}

/// The perp mark-price lane's plan: the `@markPrice@1s` socket on the perp market host. Every valid
/// mark is a `Mark` event (the engine lands it on `mark_tick` under the SERIES symbol), and the frame's
/// funding rate, when it states one, a `Funding` event for `series_symbol`; a malformed decimal ends the
/// session as an error so it reconnects (the kline lane's two-tier rule). No REST seed — the mark is a
/// live-only valuation stream; the resolver's bar-close fallback covers the pre-first-frame window, and
/// the residual stated on [`feed_main`] lands here in full (no seed write, no watchdog on this row).
pub fn mark_main(series_symbol: &str, api_symbol: &str, urls: &UrlTable) -> DataClientLanePlan {
    let series = series_symbol.to_string();
    DataClientLanePlan::Session {
        url: mark_websocket_url(&urls.market_websocket(true), api_symbol),
        subscribe: None,
        keepalive: None,
        seed: None,
        decode: Box::new(move |txt, emit| match decode_mark_frame(txt) {
            Ok(Some(m)) => {
                emit(DataClientEvent::Mark(DataClientMark { px: m.px, ts: m.ts }));
                // The same frame states the public funding rate and its next settlement (`r`, `T`).
                if let Some(funding) = m.funding_rate(&series) {
                    emit(DataClientEvent::Funding(funding));
                }
                WebsocketSessionFrameOutcome::Confirm
            }
            Ok(None) => WebsocketSessionFrameOutcome::Ignore,
            Err(e) => WebsocketSessionFrameOutcome::Fatal(e.to_string()),
        }),
    }
}

// --- L2 depth (full managed book) feed ------------------------------------------------------
// The diff-depth stream `<symbol>@depth@100ms`, managed the documented way: seed from a REST depth
// snapshot, then fold diffs under whichever sequence rule the FRAME declares — `crate::family::feed::tick`'s
// `apply_depth_event` reads that off the frame's `pu` field; a gap re-snapshots — reusing the
// pure, fixture-tested primitives in [`crate::family::feed::tick`]. Delivers the top [`DEPTH_LEVELS`] of the
// maintained book through the conflating lane [`DataClientSink::l2_snapshot`] on every applied diff,
// so the DOM renders a deep book (hundreds of levels) rather than a 20-level partial. The GUI-side
// store re-infers the display tick from the delivered levels.

/// How many levels per side to publish to the DOM (the managed book holds far more; this is the
/// render/transmit window — deep enough for a grouped ladder, cheap to clone each 100 ms).
const DEPTH_LEVELS: usize = 200;
/// The depth driver's four knobs, and the reason behind each number, are the `depth` block of
/// `vike_venues::venue_timings`' `BINANCE_SPOT` row — read here rather than restated:
///
/// - `backoff` — the reconnect pause after a socket error or sequence gap; it throttles the resync so
///   a persistent gap can't hammer the weight-50 depth snapshot into a rate-limit ban.
/// - `idle_threshold` — net-hardening §B dead-transport watchdog: no depth frame of ANY kind (a
///   `@depth@100ms` diff OR a server WS Ping) for this long means the socket is silently dead, so the
///   driver discloses a gap and reconnects + re-seeds.
/// - `freshness_threshold` — §B data-freshness threshold: how long the book may go without an applied
///   update, behind a still-live transport, before disclosing `Stale` (disclose-only, no auto-action;
///   the fast dead-socket signal is the idle threshold).
/// - `reseed_interval` — net-hardening timed book re-seed, a forced fresh snapshot on a cadence,
///   IN PLACE: the driver calls the same `seed` on a thread of its own while the lane keeps reading
///   and publishing, then splices the diffs that arrived meanwhile onto it by `apply_depth_event`'s
///   own rule — no reconnect, no transport gap (a scheduled refresh, not an outage); an absent
///   interval would disable it.
///
/// ONE `depth_main` serves binance spot, binance perp and BOTH aster markets, so one block is read
/// for all four: `BINANCE_PERP` shares `BINANCE_SPOT`'s block and aster's rows declare equal
/// numbers, which `depth_timings_tests::every_row_this_driver_serves_declares_the_same_knobs` holds
/// (aster's freshness threshold is binance's carried over, provisional until its testnet smoke runs).
///
/// The family's tick pump ([`crate::family::feed::tick`]'s `spawn_feed_tick`) reads this same block: it folds the
/// same `@depth@100ms` stream on its combined socket, so it runs on the same four numbers.
pub(super) const DEPTH_TIMINGS: VenueTimingsDepth = BINANCE_SPOT.depth();
/// Placeholder tick for the empty book seeded when a snapshot fetch fails transiently — never used
/// for quantization (the empty book carries no levels and is replaced by the next real seed); it
/// only needs to be positive so `L2Book` construction is valid.
const DEPTH_SEED_FALLBACK_TICK: f64 = 0.01;

/// The DOM depth lane's plan (`interval` has no part in it). The engine runs it on the shared
/// [`vike_bridge_core::depth`] driver — connect/seed/read-loop/publish/backoff, the top [`DEPTH_LEVELS`]
/// to the DOM, its health on the `depth` stream, and nothing on the status line; this family supplies
/// its PROTOCOL: REST-seed the managed book (post-connect, so no diff is missed in the fetch window —
/// the tick is inferred once from the dense snapshot and reused), and fold each `@depth@100ms` diff under
/// the rule its own grammar declares. A sequence gap surfaces as [`WebsocketSessionDepthOperation::Gap`] → the driver does a
/// throttled reconnect + re-seed (so a persistent gap can't hammer the snapshot endpoint).
///
/// `fetch_snapshot` is the venue's own REST depth fetch, already bound to the host it should hit —
/// the one part that isn't shared, because each venue resolves that host differently. `Sync`: the
/// timed re-seed calls it on a thread of its own while the lane keeps reading.
///
/// **Splits `.P` like the kline and trades feeds do.** It did not, and that was a live data bug.
///
/// This lane hardcoded the SPOT host and passed the RAW core symbol, so a perp subscription built
/// `wss://stream.binance.com:9443/ws/btcusdt.p@depth@100ms` — a stream no venue resolves. It failed
/// in the worst possible shape: the socket CONNECTS, the REST seed is rejected, the empty fallback
/// book's `last_sequence = 0` trips [`WebsocketSessionDepthOperation::Gap`] on the first diff, and the driver reconnects and
/// re-seeds forever — publishing one EMPTY book per cycle. `vike-recorder` stored those as
/// `kind=depth` rows (`price 0, size 0, tick 0.01`) for hours with nothing logged anywhere.
///
/// Both halves must move together, which is why `fetch_snapshot` takes the flag rather than a
/// pre-bound host: the WS host comes from `urls.public_websocket(is_perp)`, and the REST seed has to hit the
/// matching host AND path or it 404s against a host that does not serve that route.
pub fn depth_main(
    symbol: &str,
    family: &FamilySpec,
    fetch_snapshot: impl Fn(&str, bool) -> Result<DepthSnapshot, Box<dyn std::error::Error>>
    + Sync
    + 'static,
) -> DataClientLanePlan {
    // TWO names, deliberately: `api_sym` is the WIRE form (`.P` stripped) that builds the URL and
    // the REST seed, while `symbol` stays the SERIES label the engine keys the sink on — a perp must
    // not land under its spot twin's key. Collapsing them is a real mistake, caught live: the depth
    // series wrote to `symbol=BTCUSDT` while its trade sibling wrote `symbol=BTCUSDT.P`.
    //
    // ⚠ The split is asked OF THE VENUE, and this lane is why that function exists.
    // `docs/decisions/0061`'s Phase 1 pins that "every bridge that uses the suffix calls
    // `split_perp` unconditionally without ever asking whether its venue is a suffix venue" — and
    // this is a FAMILY lane: the venue is a runtime value, `family.venue`, and a third venue joining
    // the family (the whole point of the module) would have had its symbols stripped on a convention
    // it does not use. Byte-identical for the two venues on it today, binance and aster, both
    // `vike_catalog::VenueAddressingNaming::PerpSuffix`.
    let (api_sym, is_perp) = vike_catalog::split_perp_at(family.venue, symbol);
    let url = depth_websocket_url(&family.urls.public_websocket(is_perp), api_sym);
    let api_sym = api_sym.to_string();
    let seed = move || {
        // On a transient snapshot failure DON'T return None: the driver reads None as "WS-seeded,
        // wait for the snapshot frame" (correct for Bybit/OKX) and would then Ignore every diff
        // forever with no reconnect — the DOM would stay blank until the socket drops (~24h). Seed an
        // EMPTY book instead: its last_sequence=0 makes the first @depth diff trip BookOp::Gap, so the
        // driver backs off, reconnects, and re-seeds (retrying the snapshot) — the same self-heal the
        // HFT twin (family::feed::tick's pump) uses. A real snapshot replaces the book (and its tick) next
        // time.
        match fetch_snapshot(&api_sym, is_perp) {
            Ok((seq, bids, asks)) => {
                let tick = infer_tick_size(&bids, &asks);
                let mut b = L2Book::new(tick);
                b.apply_snapshot(seq, &bids, &asks);
                Some(b)
            }
            // Empty book with a placeholder tick (replaced on the next successful seed); last_sequence=0
            // forces the Gap→resync above.
            Err(_) => Some(L2Book::new(DEPTH_SEED_FALLBACK_TICK)),
        }
    };
    let decode = |txt: &str, book: &mut Option<Arc<L2Book>>| -> WebsocketSessionDepthOperation {
        // seed always returns Some here
        let Some(b) = book.as_mut().map(Arc::make_mut) else {
            return WebsocketSessionDepthOperation::Ignored;
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(txt) else {
            return WebsocketSessionDepthOperation::Ignored;
        };
        match apply_depth_event(b, &event) {
            // §B data-freshness: the depthUpdate frame's top-level `E` is the venue EVENT-TIME
            // (epoch-ms) the driver clocks staleness off (0 if absent → receive-time fallback).
            DepthOutcome::Applied => WebsocketSessionDepthOperation::Updated(
                event.get("E").and_then(serde_json::Value::as_i64).unwrap_or(0),
            ),
            DepthOutcome::Gap => WebsocketSessionDepthOperation::Gap,
            DepthOutcome::Stale | DepthOutcome::Ignored => WebsocketSessionDepthOperation::Ignored,
        }
    };
    // The raw @depth stream needs no app-level keepalive (server pings are answered). REST-seeded,
    // so the timed re-seed is a fresh `seed` spliced onto the queued diffs: no re-subscribe.
    DataClientLanePlan::Depth {
        url,
        subscribe: None,
        keepalive: None,
        resubscribe: Vec::new(),
        seed: Box::new(seed),
        decode: Box::new(decode),
        levels: DEPTH_LEVELS,
        timings: DEPTH_TIMINGS,
    }
}

/// The trade tape's plan: a loop of its own ([`crate::family::feed::tick::trades::run_trades_feed`]: WS-buffer → REST
/// warm-up → splice, then the live read), run on the lane's thread with the lane's context. `.P` splits
/// as in [`depth_main`], asked of `family.venue`: `series_symbol` stays the SERIES label (sink/core key),
/// the stripped `api_symbol` drives the WS URL and REST warm-up, `is_perp` picks the host pair; spot
/// (`series == api`, `is_perp = false`) is the pre-perp behaviour. `earliest_ids` is the venue spec's
/// `earliest_live_ids` map.
pub fn trades_main(
    series_symbol: &str,
    family: FamilySpec,
    earliest_ids: Arc<Mutex<HashMap<String, u64>>>,
) -> DataClientLanePlan {
    let symbol = series_symbol.to_string();
    DataClientLanePlan::Own(Box::new(move |ctx: &DataClientContext| {
        let (api_symbol, is_perp) = vike_catalog::split_perp_at(family.venue, &symbol);
        crate::family::feed::tick::trades::run_trades_feed(
            &family,
            &symbol,
            api_symbol,
            is_perp,
            ctx.sink.as_ref(),
            ctx.wake.as_ref(),
            &|s| ctx.set_status(s),
            &ctx.stop,
            &earliest_ids,
        );
    }))
}

#[cfg(test)]
mod mark_tests {
    use super::{MarkFrame, decode_mark_frame, mark_websocket_url};

    /// A real-shaped `markPriceUpdate` frame decodes to the exact `p` bits + the event time `E`.
    #[test]
    fn mark_price_update_decodes_price_and_event_time() {
        let frame = serde_json::json!({
            "e": "markPriceUpdate",
            "E": 1_562_305_380_000_i64,
            "s": "BTCUSDT",
            "p": "11794.15000000",
            "i": "11784.62659091",
            "P": "11784.25641265",
            "r": "0.00038167",
            "T": 1_562_306_400_000_i64
        })
        .to_string();
        let m = decode_mark_frame(&frame).unwrap().expect("a mark frame");
        assert_eq!(m.px.to_bits(), 11794.15_f64.to_bits(), "the MARK price `p`, not the index");
        assert_eq!(m.ts, 1_562_305_380_000);
        assert_eq!(
            m,
            MarkFrame {
                px: 11794.15,
                ts: 1_562_305_380_000,
                funding: Some((0.00038167, 1_562_306_400_000))
            }
        );
    }

    /// **The funding fields of a real frame.** The values are BTCUSDT's of 2026-11-08 15:59 UTC, read from
    /// `GET /fapi/v1/premiumIndex` (`markPrice`, `lastFundingRate`, `nextFundingTime`, `time`: the REST twin
    /// of this stream's `p`, `r`, `T`, `E`; the dev box reaches binance's REST host and not its stream
    /// host), put in the stream's documented frame. The rate is the CURRENT period's, for the settlement
    /// at `T`, and it reaches the sink under the series symbol with no interval (the frame states none).
    #[test]
    fn a_recorded_mark_frame_states_the_funding_rate_and_its_settlement() {
        let frame = r#"{"e":"markPriceUpdate","E":1794153563000,"s":"BTCUSDT","p":"76034.80000000","i":"76071.80847826","P":"76071.14700806","r":"0.00003420","T":1794153600000}"#;
        let m = decode_mark_frame(frame).unwrap().expect("a mark frame");
        assert_eq!(m.funding, Some((0.0000342, 1_794_153_600_000)));
        let f = m.funding_rate("BTCUSDT.P").expect("the frame states a funding rate");
        assert_eq!(
            f,
            vike_marketdata::FundingRate {
                timestamp: 1_794_153_563_000,
                symbol: "BTCUSDT.P".to_string(),
                rate: 0.0000342,
                interval_ms: None,
                next_timestamp: Some(1_794_153_600_000),
                predicted: None,
                price: None,
            }
        );
        // A frame without the funding fields, or with a malformed one, keeps its mark and states none.
        for frame in [
            r#"{"e":"markPriceUpdate","E":1,"p":"100.0"}"#,
            r#"{"e":"markPriceUpdate","E":1,"p":"100.0","r":"","T":1794153600000}"#,
            r#"{"e":"markPriceUpdate","E":1,"p":"100.0","r":"0.0001","T":"soon"}"#,
            r#"{"e":"markPriceUpdate","E":1,"p":"100.0","r":7,"T":1794153600000}"#,
        ] {
            let m = decode_mark_frame(frame).unwrap().expect("the mark still decodes");
            assert_eq!((m.px, m.funding), (100.0, None), "{frame}");
        }
    }

    /// Non-mark frames and dead marks are skipped in place; a malformed decimal is the session
    /// error (the kline lane's two-tier rule).
    #[test]
    fn non_mark_and_dead_frames_are_skipped_malformed_is_fatal() {
        // a kline frame on the same socket shape is not a mark frame
        let kline = r#"{"e":"kline","E":1,"k":{}}"#;
        assert!(decode_mark_frame(kline).unwrap().is_none());
        // not JSON at all (keepalives)
        assert!(decode_mark_frame("ping").unwrap().is_none());
        // a dead (zero) mark is dropped, not emitted
        let dead = r#"{"e":"markPriceUpdate","E":1,"p":"0.00000000"}"#;
        assert!(decode_mark_frame(dead).unwrap().is_none());
        // a malformed decimal drops the session so it reconnects
        let bad = r#"{"e":"markPriceUpdate","E":1,"p":"not-a-number"}"#;
        assert!(decode_mark_frame(bad).is_err());
    }

    /// URL grammar: lowercase symbol in the path, the documented `@markPrice@1s` suffix, perp host.
    #[test]
    fn mark_url_targets_the_1s_stream() {
        assert_eq!(
            mark_websocket_url("wss://fstream.binance.com", "BTCUSDT"),
            "wss://fstream.binance.com/ws/btcusdt@markPrice@1s"
        );
    }
}

#[cfg(test)]
mod healthy_string_pin {
    //! **One healthy string per VENUE, not per lane.** Every lane of this family, for BOTH venues it
    //! serves, writes its healthy text through the engine's one spelling
    //! (`vike_bridge_core::data_client::ws::live_status`): the engine itself for the kline, mark and
    //! depth lanes, `crate::family::feed::tick::trades` for the trade tape. Every lane of a venue shares ONE status
    //! line and the writer journals a text TRANSITION, so a per-lane spelling would turn every session
    //! boundary on either lane into a transition against the other's. `feed/tick/trades.rs` once spelled
    //! `"LIVE · {} trades {series_symbol}"` — exactly the per-lane spelling this refuses — in the lane
    //! whose healthy write sat inside `if !started`, so every reconnect after the splice skipped it.

    const MARKET_FEED_SOURCE: &str = include_str!("mod.rs");
    const TRADES_SOURCE: &str = include_str!("tick/trades.rs");

    /// The code lines of `src` (comments skipped, and this module's own lines, which name the literal
    /// being refused) that spell a healthy literal.
    fn healthy_literals(src: &str) -> Vec<&str> {
        src.lines()
            .take_while(|l| !l.trim_start().starts_with("mod healthy_string_pin"))
            .filter(|l| !l.trim_start().starts_with("//"))
            .filter(|l| l.contains(concat!('"', "LIVE ·")))
            .collect()
    }

    /// Neither file spells a healthy text of its own; the trade tape writes the engine's.
    #[test]
    fn the_family_lanes_spell_no_healthy_string_of_their_own() {
        for (file, src) in [("market_feed.rs", MARKET_FEED_SOURCE), ("trades.rs", TRADES_SOURCE)] {
            assert_eq!(healthy_literals(src), Vec::<&str>::new(), "{file}");
        }
        assert!(
            TRADES_SOURCE.contains("live_status(spec.display)"),
            "the trade tape's healthy write is the engine's `live_status` of the venue's display name"
        );
    }

    /// …and what that text is for BOTH venues classifies the way both consumers expect: `Connected`
    /// for `vike_model::parse_feed_status` (hence `Healthy` for the reconcile gate) and `info!` for the
    /// writer's level split. Aster rides every line of this family and is MAINNET in practice, so a
    /// family edit is a two-venue change.
    #[test]
    fn both_venues_healthy_strings_read_as_connected_and_log_as_info() {
        use vike_model::feed_status::{DataClientConnectionState, parse_feed_status};
        for display in ["Binance", "Aster"] {
            let s = vike_bridge_core::data_client::ws::live_status(display);
            assert_eq!(parse_feed_status(&s), DataClientConnectionState::Connected, "{s}");
            assert!(s.starts_with(vike_bridge_core::feed_ctx::HEALTHY_STATUS_PREFIX), "{s}");
        }
    }
}

#[cfg(test)]
mod depth_timings_tests {
    use std::time::Duration;

    use vike_venues::venue_timings::{ASTER_PERP, ASTER_SPOT, BINANCE_PERP};

    use vike_bridge_core::websocket_session::spec::{self, READ_POLL};

    use super::DEPTH_TIMINGS;

    /// The knobs `depth_main` hands the driver, as the literals the private constants held before
    /// they moved into `venue_timings`: a retune of the row is then a deliberate edit here too,
    /// beside the matrix pin in vike-model.
    #[test]
    fn the_depth_driver_runs_with_the_knobs_it_had_before_the_table() {
        assert_eq!(DEPTH_TIMINGS.idle_threshold(), Duration::from_secs(30), "dead-socket watchdog");
        assert_eq!(DEPTH_TIMINGS.freshness_threshold(), Duration::from_secs(120));
        assert_eq!(DEPTH_TIMINGS.reseed_interval(), Duration::from_secs(300));
        assert_eq!(DEPTH_TIMINGS.backoff(), Duration::from_secs(3));
        // The driver's stop-flag poll (the former `READ_TIMEOUT` here and in `family::feed::tick`).
        assert_eq!(READ_POLL, Duration::from_secs(2));
        // ...which the engine reads off the venue's pump row, the one read timeout of its lanes.
        assert_eq!(spec::row("binance").knobs().read_timeout, READ_POLL);
    }

    /// `depth_main` reads ONE block and serves binance spot, binance perp and both aster markets, so
    /// every row it serves must declare the same four numbers (their `why`s differ on purpose). A
    /// divergent row would be a declaration the shared driver does not honour.
    #[test]
    fn every_row_this_driver_serves_declares_the_same_knobs() {
        for (who, row) in
            [("binance perp", BINANCE_PERP), ("aster spot", ASTER_SPOT), ("aster perp", ASTER_PERP)]
        {
            let depth = row.depth();
            assert_eq!(depth.idle_threshold(), DEPTH_TIMINGS.idle_threshold(), "{who}");
            assert_eq!(depth.freshness_threshold(), DEPTH_TIMINGS.freshness_threshold(), "{who}");
            assert_eq!(depth.reseed_interval(), DEPTH_TIMINGS.reseed_interval(), "{who}");
            assert_eq!(depth.backoff(), DEPTH_TIMINGS.backoff(), "{who}");
        }
    }
}
