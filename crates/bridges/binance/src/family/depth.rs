//! The Binance-grammar public MARKET-DATA protocol (R8 HFT tick track) — shared by vike-binance
//! (`crate::market_data`) and vike-aster (`vike_aster::market_data`), each passing its own venue
//! string + resolved hosts (F8/F11, dedup rung 2).
//!
//! Aster forks Binance's public streams verbatim — same `<sym>@bookTicker` / `<sym>@trade` /
//! `<sym>@depth@100ms` names, same field letters — so the two venues' `market_data.rs` files were
//! byte-for-byte copies. The decoders + the depth-sync state machine live here once; the venues
//! keep only their host resolution and their spawn face.
//!
//! # Depth sync speaks TWO grammars, and the frame says which
//!
//! A REST depth snapshot seeds `lastUpdateId`, then diff events fold into the book. Which
//! contiguity rule those diffs obey is **not** the same on every stream, and treating it as if it
//! were cost this workspace forty days of `kind=depth/venue=binance/symbol=BTCUSDT.P`:
//!
//! * **Spot** (`stream.binance.com`) — each event's `U` is exactly the previous event's `u + 1`, so
//!   `U > last_seq + 1` is a hole. No `pu` field is sent.
//! * **USDⓈ-M futures** (`fstream.binance.com`) — update ids are deliberately NON-contiguous
//!   between a symbol's events; continuity is carried by `pu` ("final update id in the last
//!   stream"), and the documented rule is `pu == previous u`, with the FIRST event after a seed
//!   required only to straddle the snapshot (`U <= lastUpdateId <= u`).
//!
//! **MEASURED 2026-09-10 from the CI box**, 200 `@depth@100ms` frames per stream, raw WebSocket, no vike
//! code — `U == prev.u + 1` held on **0 of 199** consecutive futures pairs and **199 of 199** spot
//! pairs, while `pu == prev.u` held on **199 of 199** futures pairs; the futures overshoot
//! `U - (prev.u + 1)` ran min 4 / median 93 / max 781. So on a perp stream the SECOND diff of every
//! session read as a sequence gap, `run_depth_session` returned `Err`, and the driver spent
//! [`super::market_feed`]'s `DEPTH_BACKOFF` and re-seeded — forever, publishing the seed plus one
//! diff per ~4.3 s cycle instead of ~10 publishes/s.
//!
//! ⚠ **Aster speaks the futures grammar on BOTH planes.** Same probe, same box, same day, 100
//! frames each: `fstream.asterdex.com` 100/100 carry `pu`, `U == prev.u + 1` on 0/99;
//! `sstream.asterdex.com` **also** 100/100 carry `pu`, and `U == prev.u + 1` on only 18/99 — so
//! aster's SPOT depth lane was mis-synced too, and the sentence above that used to call the two
//! venues' depth rule identical was wrong about aster in the other direction as well.
//!
//! [`apply_depth_event`] therefore discriminates on the FRAME's own self-description (`pu` present
//! or absent) rather than on an `is_perp` flag plumbed through `route_frame`/`depth_main`/the
//! conformance harness — which is what lets one function serve four (venue × plane) combinations
//! whose grammar is a property of the wire, not of the caller's configuration.
//!
//! # The two rules, REPLAYED over live frames
//!
//! Both contiguity rules were run over the same real capture on 2026-09-10 from the CI box, in the
//! driver's own order (dial, buffer, REST seed, fold), 30 s of frames per lane — `applied` counts
//! frames folded before the first gap:
//!
//! | lane | frames | OLD (spot rule only) | NEW (this function) |
//! |---|---|---|---|
//! | binance `fstream` perp | 309 | 1 applied, then GAP | 295 applied, 0 gaps |
//! | binance `stream` spot | 314 | 300 applied, 0 gaps | 300 applied, 0 gaps |
//! | aster `fstream` perp | 281 | 1 applied, then GAP | 267 applied, 0 gaps |
//! | aster `sstream` spot | 206 | 1 applied, then GAP | 194 applied, 0 gaps |
//!
//! "1 applied, then GAP" IS the production failure, reproduced offline: the straddling first diff
//! folds, the second is misread, the session dies. Note the spot row — the two rules are
//! indistinguishable there, which is what "byte-identical on spot" means in practice rather than by
//! assertion. The stale prefix (the frames buffered while the REST seed was in flight, 12–14 per
//! run) is identical under both rules and is dropped by the shared arm.
//!
//! **Layering note.** `vike-bridge-core`'s own `depth.rs` deliberately owns the venue-NEUTRAL
//! lifecycle (connect/seed/fold/publish/backoff — see [`crate::family::market_feed`]'s DOM lane,
//! which drives it) and leaves the PROTOCOL per-venue. That split stands: this module shares the
//! protocol between the two SAME-PROTOCOL venues only, and is not a second lifecycle driver. The
//! pump below is the HFT tick track's own (`TickSender` lanes, not `LiveDataSink`), which never
//! went through the bridge-core driver on either venue.
//!
//! ⚠ **This pump DISCLOSES its transport state** ([`disclose_link`]) — one `Disconnected` per
//! outage and one `Live` on the first frame back — because it is the lane `vike-tradehub`'s CEX
//! mount actually subscribes, and therefore the only one through which the connection-state
//! dead-man can hear a binance/aster link die on that daemon. The DOM lane's disclosure
//! ([`super::market_feed`]'s `depth_main`) goes through a `LiveDataSink`; this one goes onto the
//! tick lane, because that is the seam this pump has.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;
use tungstenite::Message;

use vike_bridge_core::stream_health::{HealthEvent, StreamHealth};
use vike_bridge_core::ws::{configure_ws_stream, is_timeout};
use vike_exec::{BookUpdate, QuoteUpdate, StreamStatusUpdate, TickSender, TradeUpdate};
use vike_model::{BookLevel, L2Book, QuoteTick, TradeTick};

/// Stop-flag poll cadence for `socket.read()` — how long a shutdown request may take to land.
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------------------------
// Pure decoders (fixture-tested through each venue's own wrapper)
// ---------------------------------------------------------------------------------------------

fn f(v: Option<&Value>) -> Option<f64> {
    v.and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
}

/// `<sym>@bookTicker` payload → best bid/ask QuoteTick. The raw stream carries no event time, so
/// `ts` is 0 (the strategy acts on bid/ask, not the quote's clock).
pub fn decode_book_ticker(data: &Value, symbol: &str) -> Option<QuoteTick> {
    Some(QuoteTick {
        ts: 0,
        // TODO(book-recording plan): stamp at WS receive
        local_ts: 0,
        bid: f(data.get("b"))?,
        ask: f(data.get("a"))?,
        bid_size: f(data.get("B")).unwrap_or(0.0),
        ask_size: f(data.get("A")).unwrap_or(0.0),
        symbol: symbol.to_string(),
    })
}

/// `<sym>@trade` payload → TradeTick (ts = trade time `T`, `m` = is-buyer-maker).
pub fn decode_trade(data: &Value, symbol: &str) -> Option<TradeTick> {
    Some(TradeTick {
        ts: data.get("T").and_then(Value::as_i64).unwrap_or(0),
        // TODO(book-recording plan): stamp at WS receive
        local_ts: 0,
        price: f(data.get("p"))?,
        size: f(data.get("q"))?,
        is_buyer_maker: data.get("m").and_then(Value::as_bool).unwrap_or(false),
        symbol: symbol.to_string(),
    })
}

/// Parse a depth side (`[["px","qty"], …]`) into `BookLevel`s (qty 0 = remove, per L2Book). Private —
/// every caller reaches it through [`apply_depth_event`] or [`parse_depth_snapshot`], which is what
/// lets both venues keep it off their own public surface (matching their pre-rung-2 visibility).
fn parse_levels(v: Option<&Value>) -> Vec<BookLevel> {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|lvl| {
                    let l = lvl.as_array()?;
                    Some(BookLevel { price: f(l.first())?, qty: f(l.get(1))? })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Outcome of folding a `depthUpdate` diff into the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthOutcome {
    /// applied — the caller should publish the updated book
    Applied,
    /// `u <= last_seq` — already reflected in the snapshot; drop
    Stale,
    /// events were missed; the caller must re-snapshot. Which test decides that is per-GRAMMAR —
    /// see [`apply_depth_event`], and do NOT restate it as "`U > last_seq+1`": that spelling is the
    /// SPOT half alone and naming it here as the whole rule is what made the futures defect
    /// invisible to every reader of this enum.
    Gap,
    /// malformed frame
    Ignored,
}

/// Fold a `depthUpdate` diff into `book`. The book's `last_seq` MUST already be seeded from the
/// REST snapshot's `lastUpdateId`.
///
/// **The frame declares its own grammar.** `pu` (previous final update id) is sent by the USDⓈ-M
/// futures stream and by every aster stream, and never by binance spot — measured on both venues
/// and both planes, see the module doc. So there is no `is_perp` parameter here and none is wanted:
/// the sync rule is a property of the WIRE, and a caller-supplied flag would have to be threaded
/// correctly through `route_frame`, [`super::market_feed`]'s `depth_main`, both venues' spawn faces
/// and the shared conformance harness before it could be right anywhere.
///
/// The three tests, in the order a session meets them:
///
/// 1. `u <= last_seq` → [`DepthOutcome::Stale`]. Shared by both grammars, and it must stay FIRST:
///    it is exactly what `L2Book::apply_delta`'s `SeqPolicy::Monotonic` refuses, so admitting such
///    a frame would make `Applied` a lie — the caller would publish a book that was never mutated.
///    It is also what drops the pre-seed WS backlog.
/// 2. **`pu` present (futures grammar)** → contiguous when EITHER `pu == last_seq` (the documented
///    chain, which is what carries every steady-state frame) OR `U <= last_seq` (the frame's own
///    span already covers the anchor, which is the documented FIRST-post-seed shape — the snapshot's
///    `lastUpdateId` is no event's `u`, so nothing can chain onto it). Both arms are hole-free by
///    construction, which is why the pair needs no per-session "have we applied one yet" bit: after
///    a diff has applied, `last_seq` is the previous event's `u` and a real next frame has
///    `U > last_seq` (measured overshoot: min 4), so arm two cannot fire in steady state — and a
///    frame that FOLLOWS a dropped one has an even larger `U` and a `pu` pointing at the dropped
///    frame, so it still gaps.
/// 3. **`pu` absent (spot grammar)** → the unchanged `U <= last_seq + 1` test.
///
/// Note that arm two is `U <= last_seq`, NOT the spot arm's `U <= last_seq + 1`: the `+1` encodes
/// spot's own "`U` is the previous `u` plus one" promise, which the futures stream does not make.
pub fn apply_depth_event(book: &mut L2Book, data: &Value) -> DepthOutcome {
    let (Some(first_u), Some(final_u)) =
        (data.get("U").and_then(Value::as_u64), data.get("u").and_then(Value::as_u64))
    else {
        return DepthOutcome::Ignored;
    };
    if final_u <= book.last_seq {
        return DepthOutcome::Stale;
    }
    let contiguous = match data.get("pu").and_then(Value::as_u64) {
        // FUTURES grammar (and aster on both planes): the chain, or a span that covers the anchor.
        Some(prev_final_u) => prev_final_u == book.last_seq || first_u <= book.last_seq,
        // SPOT grammar: `U` is the previous event's `u + 1`, so anything past that is a hole.
        None => first_u <= book.last_seq + 1,
    };
    if !contiguous {
        return DepthOutcome::Gap; // missed the intervening events — resync
    }
    let bids = parse_levels(data.get("b"));
    let asks = parse_levels(data.get("a"));
    book.apply_delta(final_u, &bids, &asks);
    DepthOutcome::Applied
}

/// What one decoded market-data frame becomes. The pump translates these to `TickSender` pushes;
/// tests assert them directly (scripted-stream, no socket).
#[derive(Debug, Clone, PartialEq)]
pub enum MdEvent {
    Quote(QuoteTick),
    Trade(TradeTick),
    /// the L2Book was mutated — publish it
    BookUpdated,
    /// depth gap — the pump must re-snapshot
    Resync,
    Ignored,
}

/// Route ONE combined-stream text frame (`{"stream":…,"data":…}`) to a lane, folding depth into
/// `book`. Pure over `book`'s state — the whole decode path is testable without a socket.
pub fn route_frame(text: &str, symbol: &str, book: &mut L2Book) -> MdEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return MdEvent::Ignored;
    };
    let (Some(stream), Some(data)) = (v.get("stream").and_then(Value::as_str), v.get("data"))
    else {
        return MdEvent::Ignored;
    };
    if stream.ends_with("@bookTicker") {
        decode_book_ticker(data, symbol).map_or(MdEvent::Ignored, MdEvent::Quote)
    } else if stream.ends_with("@trade") {
        decode_trade(data, symbol).map_or(MdEvent::Ignored, MdEvent::Trade)
    } else if stream.contains("@depth") {
        match apply_depth_event(book, data) {
            DepthOutcome::Applied => MdEvent::BookUpdated,
            DepthOutcome::Gap => MdEvent::Resync,
            _ => MdEvent::Ignored,
        }
    } else {
        MdEvent::Ignored
    }
}

// ---------------------------------------------------------------------------------------------
// REST depth snapshot (the pure halves — each venue owns its own host resolution)
// ---------------------------------------------------------------------------------------------

/// `(lastUpdateId, bids, asks)` — a REST depth snapshot ready to seed an [`L2Book`].
pub type DepthSnapshot = (u64, Vec<BookLevel>, Vec<BookLevel>);

/// Wall-clock ceiling on a `…/depth` REST book SEED — the blocking call every depth session makes
/// between its (bounded) dial and its first socket read.
///
/// **Why it needs its own number, and why the dial's bound did not cover it.** A depth session's
/// shape is dial → seed → read loop, and `vike_bridge_core::depth::run_depth_session` polls the stop
/// flag only once the read loop starts. So a stop landing during the dial is followed by a fresh REST
/// round trip on the shared `vike_bridge_core::http::blocking_agent`, whose 30 s global timeout is
/// sized for a backfill pager: three times the dial that was just bounded, in the same feed thread,
/// two lines further down, and outside
/// `crates/vike-datahub/src/recorder.rs`'s `FEED_STOP_BUDGET_SECS` derivation entirely.
/// This is the exact defect `crates/bridges/binance/src/family/trades.rs`'s `WARMUP_TIMEOUT` fixed
/// on the trades lane, in the lane next door — bounding a dial while a longer unbudgeted call hides
/// behind it moves the ceiling, it does not lower it.
///
/// **Ten seconds, measured.** MEASURED 2026-08-08 from the CI box (DE), `curl -w %{time_total}`, ten
/// samples against each host — the exact requests [`depth_snapshot_url`] builds, `limit` and all:
/// `api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=1000` (spot) 0.272–0.290 s, and
/// `fapi.binance.com/fapi/v1/depth?symbol=BTCUSDT&limit=1000` (perp) 0.266–0.282 s. A whole seed —
/// TCP + TLS + the request + a thousand levels of body — is a quarter second on both hosts, and the
/// spread across twenty samples is under 25 ms, so ten seconds is ~35× the observed cost with no
/// slow tail to accommodate. Deliberately the SAME window as the dial
/// (`vike_bridge_core::pump_spec`'s `CONNECT_10S`) and as the trades warmup, which is what keeps the
/// feed-stop budget a single largest-position number rather than a growing sum.
///
/// **What a timeout costs, so the trade is visible:** the seed fails, and both the DOM lane
/// (`crate::family::market_feed`'s `depth_main`) and the pump below already treat a failed seed as
/// transient — an empty book whose `last_seq = 0` makes the first diff a gap, so the driver backs
/// off, reconnects and re-seeds. Nothing is lost that a 30 s wait would have kept; it is one more
/// reconnect cycle against a venue that was not answering anyway.
pub const DEPTH_SEED_TIMEOUT: Duration = Duration::from_secs(10);

/// The agent every venue's `fetch_depth_snapshot` issues its one request through — a named rung so
/// the bound is OBSERVABLE (`agent.config().timeouts().global`) rather than merely written down, the
/// same shape `crates/bridges/binance/src/family/trades.rs`'s `warmup_agent` gives the trades warmup.
/// Shared here rather than copied per venue because both venues' seeds are the same call on the same
/// grammar, and a per-venue copy is a second thing to forget. See
/// `the_depth_seed_runs_on_a_bounded_agent_not_the_pagers`.
pub fn depth_seed_agent() -> ureq::Agent {
    vike_bridge_core::http::blocking_agent_with_timeout(DEPTH_SEED_TIMEOUT)
}

/// Build the `GET …/depth` URL for a resolved REST host + path — pure so the host/path swap is
/// unit-testable without network. `path` is caller-supplied (no leading slash) rather than derived
/// from a `perp` flag: the two venues' futures depth paths are NOT the same endpoint version, and
/// Binance's tick track never fetches perp depth at all, so encoding either venue's choice here
/// would be a latent trap for the other.
pub fn depth_snapshot_url(rest: &str, path: &str, symbol: &str) -> String {
    format!("{rest}/{path}?symbol={symbol}&limit=1000")
}

/// The pure tail of both venues' `fetch_depth_snapshot`: a `…/depth` response body → the snapshot
/// that seeds the book. Split from the fetch itself so the host resolution (the one real per-venue
/// delta) stays with the venue while the parse is shared.
pub fn parse_depth_snapshot(body: &str) -> Result<DepthSnapshot, Box<dyn std::error::Error>> {
    let v: Value = serde_json::from_str(body)?;
    let seq = v.get("lastUpdateId").and_then(Value::as_u64).ok_or("no lastUpdateId")?;
    Ok((seq, parse_levels(v.get("bids")), parse_levels(v.get("asks"))))
}

/// Build the combined bookTicker+trade+depth stream URL for a resolved (bare) WS host — pure so the
/// host swap is unit-testable without network.
pub fn combined_stream_url(ws: &str, symbol: &str) -> String {
    let low = symbol.to_lowercase();
    format!("{ws}/stream?streams={low}@bookTicker/{low}@trade/{low}@depth@100ms")
}

// ---------------------------------------------------------------------------------------------
// WS pump (stoppable / joinable — live-smoke tested through each venue's own spawn face)
// ---------------------------------------------------------------------------------------------

/// Handle for the market-data thread; `shutdown` flags + joins it (deterministic teardown).
pub struct MarketDataFeed {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl MarketDataFeed {
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for MarketDataFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Everything the pump needs that differs per venue, resolved by the caller before the thread is
/// spawned: the venue key stamped on each tick, the combined-stream URL, and how to (re)fetch the
/// REST depth snapshot that seeds the book. Boxing the fetcher keeps each venue's own host/`env`
/// resolution entirely on its side of the seam.
pub struct PumpSpec {
    /// Venue key stamped on every `QuoteUpdate`/`TradeUpdate`/`BookUpdate`.
    pub venue: &'static str,
    /// The fully-resolved combined-stream URL (see [`combined_stream_url`]).
    pub ws_url: String,
    /// (Re)fetch the REST depth snapshot that seeds/re-seeds the book. Called once per session and
    /// again on every [`MdEvent::Resync`]; an `Err` is swallowed exactly as before (the book simply
    /// stays un-seeded and the next diff trips a gap).
    pub fetch_snapshot: Box<dyn Fn() -> Result<DepthSnapshot, Box<dyn std::error::Error>> + Send>,
}

/// Spawn the venue's market-data feed: combined bookTicker + trade + depth streams pushed into the
/// core's tick/L2 lanes via `ticks`. `tick_size` sizes the `L2Book` price grid. Stoppable/joinable.
///
/// `thread_label` names the OS thread (`md-{label}-{symbol}`) and is also the affinity pin label —
/// both venues pass their venue key, keeping the pre-rung-2 names byte-identical.
pub fn spawn_market_data(
    spec: PumpSpec,
    thread_label: &'static str,
    ticks: TickSender,
    symbol: &str,
    tick_size: f64,
) -> MarketDataFeed {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let sym = symbol.to_string();
    let join = std::thread::Builder::new()
        .name(format!("md-{thread_label}-{sym}"))
        .spawn(move || {
            vike_exec::affinity::pin_current_thread(
                vike_exec::affinity::Role::MarketData,
                thread_label,
            );
            md_main(&spec, &sym, tick_size, ticks, &stop_thread)
        })
        .expect("spawn market-data thread");
    MarketDataFeed { stop, join: Some(join) }
}

/// The stream label stamped on this pump's [`StreamStatusUpdate`]s. Informational only — the
/// connection-state dead-man keys on `(venue, symbol)` and destructures the label away
/// (`vike_core`'s `Ingest::StreamStatus` arm) — but it is what tells an operator reading a
/// `on_feed_status` trace WHICH of a venue's sockets died, and this pump is not the DOM lane
/// (`crates/bridges/binance/src/family/market_feed.rs`'s `depth_main`, label `"depth"`).
const LINK_STREAM_LABEL: &str = "ticks";

/// Disclose this pump's TRANSPORT state onto the core's tick lane.
///
/// ⚠ **This is the lane the connection-state dead-man actually hears** on a `vike-tradehub` CEX
/// mount, and the reason it exists at all. That switch (`vike_core::LinkDeadManConfig`, decision
/// `docs/decisions/0038-the-dead-man-observes-the-connection-not-silence.md`) is ON by default and
/// arms per venue, but a venue arms only where the MOUNT subscribed a lane that carries a
/// disconnect. This family's only other emitter is the DOM depth feed
/// (`crates/bridges/binance/src/family/market_feed.rs`'s `depth_main`, through a `LiveDataSink`),
/// which that daemon does not subscribe — so before this call site existed, a default CEX daemon
/// built no switch at all, whatever the `policy` settings said.
///
/// No `LiveDataSink` is involved and none is wanted: this pump writes onto a bare
/// [`TickSender`], and `vike_model::FeedStatus` is exactly what `vike_core::CoreLaneSink` converts
/// a sink-side `StreamStatus` into anyway ([`HealthEvent::feed_status`] is the one map). A send
/// failure is `CoreGone` — the core thread has exited — and is dropped for the same reason every
/// other push on this loop drops it: a feed thread has no channel back to its owner.
fn disclose_link(ticks: &TickSender, venue: &str, symbol: &str, ev: HealthEvent) {
    let _ = ticks.stream_status(StreamStatusUpdate {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        stream: LINK_STREAM_LABEL.to_string(),
        status: ev.feed_status(),
    });
}

/// Reconnect loop: (re)snapshot the book, then pump the combined stream until stop/socket error.
///
/// Owns the ONE [`StreamHealth`] for the whole feed lifetime (across reconnects), exactly as
/// `vike_bridge_core::depth`'s `run_depth_feed` owns its own: the transport half dedups an outage
/// to ONE `Gap`/`Live` pair however many re-dials it takes, so a venue that flaps for ten minutes
/// discloses one disconnect and one recovery rather than three hundred. Only the transport half is
/// used — this loop runs no freshness watchdog (a read timeout here is a stop-poll tick and nothing
/// judges data age), which is why the constructor is [`StreamHealth::transport_only`].
fn md_main(
    spec: &PumpSpec,
    symbol: &str,
    tick_size: f64,
    ticks: TickSender,
    stop: &Arc<AtomicBool>,
) {
    let mut health = StreamHealth::transport_only();
    while !stop.load(Ordering::Relaxed) {
        if run_session(spec, symbol, tick_size, &ticks, stop, &mut health).is_ok() {
            return; // clean stop
        }
        // socket/stream error — disclose the outage ONCE, then a brief backoff, then reconnect +
        // re-snapshot. A failed CONNECT lands here too (`connect_ws`'s `?` below), which is what
        // makes the FIRST link death of a mount visible: the pump that never comes up at all
        // discloses a disconnect and never a recovery, and that is the outage the dead-man exists
        // for (`vike_core::runtime::link_deadman`'s module doc argues it).
        if let Some(ev) = health.enter_gap(vike_model::clock::now_ms()) {
            disclose_link(&ticks, spec.venue, symbol, ev);
        }
        for _ in 0..5 {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn run_session(
    spec: &PumpSpec,
    symbol: &str,
    tick_size: f64,
    ticks: &TickSender,
    stop: &Arc<AtomicBool>,
    health: &mut StreamHealth,
) -> Result<(), Box<dyn std::error::Error>> {
    // The dial is BOUNDED — `vike_bridge_core::pump_spec`'s `CONNECT_10S`, through the same
    // `connect_ws` arm the market pump and the bridge-core depth driver take. It was a bare
    // `tungstenite::connect` until 2026-08-08, which applies no connect bound of any kind, so this
    // thread sat out the OS's own SYN ladder (~127 s on Linux defaults) on a black-holed route with
    // `stop` already raised behind it — the same defect the market pump's rows and
    // `crates/vike-bridge-core/src/depth.rs`'s `connect_depth` carried. This pump is the HFT tick
    // track (the live daemon's — `vike-tradehub`'s CEX feed arm — not the recorder's; it said
    // `vike-app` until 2026-09-28), so it does not spend the recorder's budget; it is bounded
    // anyway because the property is thread liveness, which is not a per-consumer decision.
    let mut socket = vike_bridge_core::ws_proxy::connect_ws(
        spec.ws_url.as_str(),
        None,
        Some(vike_bridge_core::pump_spec::CONNECT_10S),
    )?;
    configure_ws_stream(&socket, READ_TIMEOUT);

    // A dial can spend its whole bounded window, and `MarketDataFeed::shutdown` raises the flag
    // before it joins — so by the time we get here the answer may already be "stop". The REST seed
    // below is a fresh network call ([`DEPTH_SEED_TIMEOUT`]); starting one after the caller has
    // already asked to stop is exactly the window this check exists to refuse.
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }

    // The ONE standing book this session folds every diff into. Held behind an `Arc` so a
    // `BookUpdated` emission is a refcount bump, not a deep clone of two ~1000-entry `BTreeMap`s
    // (perf audit finding #1 — see `vike_exec::BookUpdate`). Every mutation goes through
    // `Arc::make_mut`: O(1) while this pump is the only holder, and a copy-on-write clone (on THIS
    // thread, never on the single-writer core) only while a consumer still holds the last one sent.
    // Seeded from a REST snapshot; diffs that predate it are dropped by apply_depth_event.
    let mut book = Arc::new(L2Book::new(tick_size));
    if let Ok((seq, bids, asks)) = (spec.fetch_snapshot)() {
        Arc::make_mut(&mut book).apply_snapshot(seq, &bids, &asks);
    }

    let venue = spec.venue;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) if is_timeout(&e) => continue, // stop-poll tick
            Err(e) => return Err(e.into()),
        };
        // TRANSPORT RECOVERY, disclosed here and NOT at connect. Any inbound frame — data, or a
        // server Ping the socket answers — proves the wire carries traffic again; a successful
        // dial proves only that the venue accepted a TCP+TLS handshake. ⚠ The difference is not
        // cosmetic: a venue that accepts a connection and immediately drops it (a rate-limit ban,
        // a half-dead edge) would, on a connect-time `Live`, emit `Live`/`Gap` every cycle and keep
        // resetting the dead-man's grace window — so the switch could never trip on the outage it
        // most needs to. Disclosed on the first frame instead, a flapping venue that never delivers
        // one stays disclosed DOWN. Same rule and same reason as
        // `crates/bridges/polymarket/src/market_feed.rs`'s `on_frame`, and as the depth driver's
        // first-publish recovery (`vike_bridge_core::depth`'s `run_depth_session`).
        //
        // COST: one `Option::take` per inbound frame on THIS venue thread, beside a `serde_json`
        // parse — the same unguarded per-frame `recover()` the polymarket feed already makes in
        // `on_frame`. Nothing is added to `vike-core`'s fold: the core sees a `StreamStatus`
        // message only on a transition, which is the property the p99 gate rests on
        // (`crates/vike-core/src/runtime/link_deadman.rs`'s "Off the hot fold").
        if let Some(ev) = health.recover() {
            disclose_link(ticks, spec.venue, symbol, ev);
        }
        match msg {
            Message::Text(txt) => {
                // Bound OUT of the `match` scrutinee on purpose: `Arc::make_mut`'s `&mut` borrow of
                // `book` must end before the arms below read `book` again (a scrutinee's
                // temporaries otherwise live to the end of the match).
                let ev = route_frame(txt.as_str(), symbol, Arc::make_mut(&mut book));
                match ev {
                    MdEvent::Quote(quote) => {
                        if ticks
                            .quote(QuoteUpdate {
                                venue: venue.into(),
                                symbol: symbol.into(),
                                quote,
                            })
                            .is_err()
                        {
                            return Ok(()); // core gone
                        }
                    }
                    MdEvent::Trade(trade) => {
                        if ticks
                            .trade(TradeUpdate {
                                venue: venue.into(),
                                symbol: symbol.into(),
                                trade,
                            })
                            .is_err()
                        {
                            return Ok(());
                        }
                    }
                    MdEvent::BookUpdated => {
                        // Refcount bump, NOT a deep copy — the pump keeps folding into the same
                        // allocation until a consumer is still holding it at the next mutation.
                        if ticks
                            .book(BookUpdate {
                                venue: venue.into(),
                                symbol: symbol.into(),
                                book: Arc::clone(&book),
                            })
                            .is_err()
                        {
                            return Ok(());
                        }
                    }
                    MdEvent::Resync => {
                        if let Ok((seq, bids, asks)) = (spec.fetch_snapshot)() {
                            Arc::make_mut(&mut book).apply_snapshot(seq, &bids, &asks);
                        }
                    }
                    MdEvent::Ignored => {}
                }
            }
            Message::Ping(p) => socket.send(Message::Pong(p))?,
            Message::Close(_) => return Err("server closed".into()),
            _ => {}
        }
    }
}

/// The depth-sync grammar tests, driven by frames CAPTURED FROM THE REAL VENUES.
///
/// ⚠ **Every id below is a real one**, taken 2026-09-10 from the CI box with a raw WebSocket and no vike
/// code in the path (the probe is described in the module doc) — the one exception is the
/// pre-seed backlog frame, which is DERIVED from the real snapshot id because a backlog frame is
/// exactly what a probe that dials before it seeds does not keep. That is load-bearing rather than
/// decoration: the reason this defect survived forty days of green CI is that every synthetic
/// fixture in the tree — this file's old tests, both venues' `route_frame` tests, and
/// `crates/vike-bridge-core/tests/market_data_conformance.rs`'s `delta` builder — encoded a
/// CONTIGUOUS diff as `U = last + 1` and carried no `pu`. That is the spot grammar, so every one of
/// them certified the spot half twice and the futures half never.
#[path = "depth_grammar_tests.rs"]
#[cfg(test)]
mod depth_grammar_tests;

#[path = "seed_bound_tests.rs"]
#[cfg(test)]
mod seed_bound_tests;
