//! The Binance-grammar public MARKET-DATA protocol (R8 HFT tick track) — shared by vike-binance
//! (`crate::feed::tick`) and vike-aster (`vike_aster::feed::tick`), each passing its own venue
//! string + resolved hosts (F8/F11, dedup rung 2).
//!
//! Aster forks Binance's public streams verbatim — same `<sym>@bookTicker` / `<sym>@trade` /
//! `<sym>@depth@100ms` names, same field letters — so the two venues' `feed/tick/mod.rs` files were
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
//!   `U > last_sequence + 1` is a hole. No `pu` field is sent.
//! * **USDⓈ-M futures** (`fstream.binance.com`) — update ids are deliberately NON-contiguous
//!   between a symbol's events; continuity is carried by `pu` ("final update id in the last
//!   stream"), and the documented rule is `pu == previous u`, with the FIRST event after a seed
//!   required only to straddle the snapshot (`U <= lastUpdateId <= u`).
//!
//! **MEASURED 2026-09-10 from the CI box**, 200 `@depth@100ms` frames per stream, raw WebSocket, no vike
//! code — `U == previous.u + 1` held on **0 of 199** consecutive futures pairs and **199 of 199** spot
//! pairs, while `pu == previous.u` held on **199 of 199** futures pairs; the futures overshoot
//! `U - (previous.u + 1)` ran min 4 / median 93 / max 781. So on a perp stream the SECOND diff of every
//! session read as a sequence gap, `run_depth_session` returned `Err`, and the driver spent
//! [`crate::family::feed`]'s `DEPTH_TIMINGS` backoff and re-seeded — forever, publishing the seed
//! plus one diff per ~4.3 s cycle instead of ~10 publishes/s.
//!
//! ⚠ **Aster speaks the futures grammar on BOTH planes.** Same probe, same box, same day, 100
//! frames each: `fstream.asterdex.com` 100/100 carry `pu`, `U == previous.u + 1` on 0/99;
//! `sstream.asterdex.com` **also** 100/100 carry `pu`, and `U == previous.u + 1` on only 18/99 — so
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
//! **Layering note.** `vike-bridge-core`'s own `depth.rs` owns the venue-NEUTRAL lifecycle
//! (connect/seed/fold/publish/backoff) and leaves the PROTOCOL per-venue. That split stands: this
//! module shares the protocol between the two SAME-PROTOCOL venues only, and is not a second
//! lifecycle driver. BOTH lanes on these streams run that one driver with the family's one `depth`
//! timings row: the DOM lane ([`crate::family::feed`]'s `depth_main`, onto a `DataClientSink`) and the
//! HFT tick pump below ([`spawn_feed_tick`], onto the core's `TickSender` lanes).
//!
//! ⚠ **The pump DISCLOSES its link state** (`vike_bridge_core::depth::FeedTickSender::link`) — one
//! `Disconnected` per outage, `Stale` when the book stops updating behind a live socket, and one
//! `Live` on the first book back — because it is the lane `vike-tradehub`'s CEX start actually
//! subscribes, and therefore the only one through which the connection-state dead-man can hear a
//! binance/aster link die on that daemon. The DOM lane's disclosure goes through a `DataClientSink`;
//! this one goes onto the tick lane, because that is the seam this pump has.

pub mod trades;

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use vike_bridge_core::data_client::event::DataClientEvent;
use vike_bridge_core::depth::{
    FeedTickSender, WebsocketSessionDepthOperation, parse_levels, run_depth, spawn_tick,
};
use vike_bridge_core::is_valid_trade;
use vike_bridge_core::json::str_f64;
use vike_bridge_core::websocket_session::spec::READ_POLL;
use vike_execution::TickSender;
use vike_log::poller::StopHandle;
use vike_marketdata::{L2Book, L2BookLevel, TickQuote, TickTrade};

use crate::family::feed::DEPTH_TIMINGS;
use crate::family::feed::decode::{FeedScan, scan_frame};

// ---------------------------------------------------------------------------------------------
// Pure decoders (fixture-tested through each venue's own wrapper)
// ---------------------------------------------------------------------------------------------

/// `<sym>@bookTicker` payload → best bid/ask TickQuote. `ts` is the frame's event time `E`, else its
/// transaction time `T` (the futures stream carries both); the raw SPOT stream carries neither, so `ts`
/// is 0 here and the pump stamps the arrival (`vike_bridge_core::depth::send_quote`'s one rule).
pub fn decode_book_ticker(data: &Value, symbol: &str) -> Option<TickQuote> {
    Some(TickQuote {
        timestamp: data
            .get("E")
            .and_then(Value::as_i64)
            .or_else(|| data.get("T").and_then(Value::as_i64))
            .unwrap_or(0),
        // A pure decoder stamps nothing; the pump does, at receive time (`send_quote`).
        local_timestamp: 0,
        bid: str_f64(data.get("b"))?,
        ask: str_f64(data.get("a"))?,
        bid_size: str_f64(data.get("B")).unwrap_or(0.0),
        ask_size: str_f64(data.get("A")).unwrap_or(0.0),
        symbol: symbol.to_string(),
    })
}

/// `<sym>@trade` payload → TickTrade (ts = trade time `T`, `m` = is-buyer-maker). `None` for a print
/// [`is_valid_trade`] refuses (a zero, negative, NaN or infinite price or size), as the recorded tape
/// refuses it.
pub fn decode_trade(data: &Value, symbol: &str) -> Option<TickTrade> {
    let (price, size) = (str_f64(data.get("p"))?, str_f64(data.get("q"))?);
    if !is_valid_trade(price, size) {
        return None;
    }
    Some(TickTrade {
        timestamp: data.get("T").and_then(Value::as_i64).unwrap_or(0),
        // Not stamped here: the TRADE tape that is recorded and drawn is `feed/tick/trades.rs`'s feed, which
        // stamps at receive. This pump's trades go to the strategy lane, which reads `ts` only.
        local_timestamp: 0,
        price,
        size,
        is_buyer_maker: data.get("m").and_then(Value::as_bool).unwrap_or(false),
        symbol: symbol.to_string(),
    })
}

/// Outcome of folding a `depthUpdate` diff into the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthOutcome {
    /// applied — the caller should publish the updated book
    Applied,
    /// `u <= last_sequence` — already reflected in the snapshot; drop
    Stale,
    /// events were missed; the caller must re-snapshot. Which test decides that is per-GRAMMAR —
    /// see [`apply_depth_event`], and do NOT restate it as "`U > last_sequence+1`": that spelling is the
    /// SPOT half alone and naming it here as the whole rule is what made the futures defect
    /// invisible to every reader of this enum.
    Gap,
    /// malformed frame
    Ignored,
}

/// Fold a `depthUpdate` diff into `book`. The book's `last_sequence` MUST already be seeded from the
/// REST snapshot's `lastUpdateId`.
///
/// **The frame declares its own grammar.** `pu` (previous final update id) is sent by the USDⓈ-M
/// futures stream and by every aster stream, and never by binance spot — measured on both venues
/// and both planes, see the module doc. So there is no `is_perp` parameter here and none is wanted:
/// the sync rule is a property of the WIRE, and a caller-supplied flag would have to be threaded
/// correctly through `route_frame`, [`crate::family::feed`]'s `depth_main`, both venues' spawn faces
/// and the shared conformance harness before it could be right anywhere.
///
/// The three tests, in the order a session meets them:
///
/// 1. `u <= last_sequence` → [`DepthOutcome::Stale`]. Shared by both grammars, and it must stay FIRST:
///    it is exactly what `L2Book::apply_delta`'s `L2BookSequencePolicy::Monotonic` refuses, so admitting such
///    a frame would make `Applied` a lie — the caller would publish a book that was never mutated.
///    It is also what drops the pre-seed WS backlog.
/// 2. **`pu` present (futures grammar)** → contiguous when EITHER `pu == last_sequence` (the documented
///    chain, which is what carries every steady-state frame) OR `U <= last_sequence` (the frame's own
///    span already covers the anchor, which is the documented FIRST-post-seed shape — the snapshot's
///    `lastUpdateId` is no event's `u`, so nothing can chain onto it). Both arms are hole-free by
///    construction, which is why the pair needs no per-session "have we applied one yet" bit: after
///    a diff has applied, `last_sequence` is the previous event's `u` and a real next frame has
///    `U > last_sequence` (measured overshoot: min 4), so arm two cannot fire in steady state — and a
///    frame that FOLLOWS a dropped one has an even larger `U` and a `pu` pointing at the dropped
///    frame, so it still gaps.
/// 3. **`pu` absent (spot grammar)** → the unchanged `U <= last_sequence + 1` test.
///
/// Note that arm two is `U <= last_sequence`, NOT the spot arm's `U <= last_sequence + 1`: the `+1` encodes
/// spot's own "`U` is the previous `u` plus one" promise, which the futures stream does not make.
pub fn apply_depth_event(book: &mut L2Book, data: &Value) -> DepthOutcome {
    let (Some(first_u), Some(final_u)) =
        (data.get("U").and_then(Value::as_u64), data.get("u").and_then(Value::as_u64))
    else {
        return DepthOutcome::Ignored;
    };
    if let Some(verdict) =
        depth_verdict(book, first_u, final_u, data.get("pu").and_then(Value::as_u64))
    {
        return verdict;
    }
    let bids = parse_levels(data.get("b"));
    let asks = parse_levels(data.get("a"));
    book.apply_delta(final_u, &bids, &asks);
    DepthOutcome::Applied
}

/// The sync rule of [`apply_depth_event`], over the three ids it reads: `Some(Stale)` / `Some(Gap)`
/// when the diff must NOT fold, `None` when it must. Split out so the `Value` decode and the typed
/// scan ([`crate::family::feed::decode`]) cannot hold two spellings of it.
fn depth_verdict(
    book: &L2Book,
    first_u: u64,
    final_u: u64,
    previous_final_u: Option<u64>,
) -> Option<DepthOutcome> {
    if final_u <= book.last_sequence {
        return Some(DepthOutcome::Stale);
    }
    let contiguous = match previous_final_u {
        // FUTURES grammar (and aster on both planes): the chain, or a span that covers the anchor.
        Some(previous_final_u) => {
            previous_final_u == book.last_sequence || first_u <= book.last_sequence
        }
        // SPOT grammar: `U` is the previous event's `u + 1`, so anything past that is a hole.
        None => first_u <= book.last_sequence + 1,
    };
    if !contiguous {
        return Some(DepthOutcome::Gap); // missed the intervening events — resync
    }
    None
}

/// Route ONE combined-stream text frame (`{"stream":…,"data":…}`) to a lane, folding depth into
/// `book`, as the one [`DataClientEvent`] the pump translates to `TickSender` pushes (a quote, ONE
/// print, the book changed with no venue time, a depth gap to re-snapshot, nothing). Pure over
/// `book`'s state — the whole decode path is testable without a socket.
///
/// The frame is read by `depth_scan::scan_frame` first — straight into the tick, trade or
/// depth diff, no `Value` tree — and only a frame that scan DECLINES (anything but the exact shape
/// the venue sends) reaches the `Value` decoder below, so the answer is the same on every input.
pub fn route_frame(text: &str, symbol: &str, book: &mut L2Book) -> DataClientEvent {
    match scan_frame(text, symbol) {
        Ok(FeedScan::Quote(q)) => DataClientEvent::Quote(q),
        // The scan's print meets the same guard `decode_trade` applies on the `Value` path.
        Ok(FeedScan::Trade(t)) if is_valid_trade(t.price, t.size) => DataClientEvent::Trade(t),
        Ok(FeedScan::Trade(_)) => DataClientEvent::Ignored,
        Ok(FeedScan::Ignored) => DataClientEvent::Ignored,
        Ok(FeedScan::Depth(d)) => {
            match depth_verdict(book, d.first_u, d.final_u, d.previous_final_u) {
                Some(DepthOutcome::Gap) => DataClientEvent::Resync,
                Some(_) => DataClientEvent::Ignored,
                None => {
                    // Whole frame already read and valid — only now does the book move.
                    book.apply_delta(d.final_u, &d.bids, &d.asks);
                    DataClientEvent::BookUpdated { timestamp_ms: 0 }
                }
            }
        }
        Err(_) => route_frame_value(text, symbol, book),
    }
}

/// The `Value` decode of one combined-stream frame: the fallback for every frame the typed scan
/// declines.
fn route_frame_value(text: &str, symbol: &str, book: &mut L2Book) -> DataClientEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return DataClientEvent::Ignored;
    };
    let (Some(stream), Some(data)) = (v.get("stream").and_then(Value::as_str), v.get("data"))
    else {
        return DataClientEvent::Ignored;
    };
    if stream.ends_with("@bookTicker") {
        decode_book_ticker(data, symbol).map_or(DataClientEvent::Ignored, DataClientEvent::Quote)
    } else if stream.ends_with("@trade") {
        decode_trade(data, symbol).map_or(DataClientEvent::Ignored, DataClientEvent::Trade)
    } else if stream.contains("@depth") {
        match apply_depth_event(book, data) {
            DepthOutcome::Applied => DataClientEvent::BookUpdated { timestamp_ms: 0 },
            DepthOutcome::Gap => DataClientEvent::Resync,
            _ => DataClientEvent::Ignored,
        }
    } else {
        DataClientEvent::Ignored
    }
}

// ---------------------------------------------------------------------------------------------
// REST depth snapshot (the pure halves — each venue owns its own host resolution)
// ---------------------------------------------------------------------------------------------

/// `(lastUpdateId, bids, asks)` — a REST depth snapshot ready to seed an [`L2Book`].
pub type DepthSnapshot = (u64, Vec<L2BookLevel>, Vec<L2BookLevel>);

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
/// This is the exact defect `crates/bridges/binance/src/family/feed/tick/trades.rs`'s `WARMUP_TIMEOUT` fixed
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
/// (`vike_bridge_core::websocket_session::spec`'s `CONNECT_10S`) and as the trades warmup, which is what keeps the
/// feed-stop budget a single largest-position number rather than a growing sum.
///
/// **What a timeout costs, so the trade is visible:** the seed fails, and both the DOM lane
/// (`crate::family::feed`'s `depth_main`) and the pump below already treat a failed seed as
/// transient — an empty book whose `last_sequence = 0` makes the first diff a gap, so the driver backs
/// off, reconnects and re-seeds. Nothing is lost that a 30 s wait would have kept; it is one more
/// reconnect cycle against a venue that was not answering anyway.
pub const DEPTH_SEED_TIMEOUT: Duration = Duration::from_secs(10);

/// The agent every venue's `fetch_depth_snapshot` issues its one request through — a named rung so
/// the bound is OBSERVABLE (`agent.config().timeouts().global`) rather than merely written down, the
/// same shape `crates/bridges/binance/src/family/feed/tick/trades.rs`'s `warmup_agent` gives the trades warmup.
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

/// Everything the pump needs that differs per venue, resolved by the caller before the thread is
/// spawned: the venue key stamped on each tick, the combined-stream URL, and how to fetch the REST
/// depth snapshot that seeds the book. Boxing the fetcher keeps each venue's own host/`env`
/// resolution entirely on its side of the seam.
pub struct FeedTickSpec {
    /// Venue key stamped on every `QuoteUpdate`/`TradeUpdate`/`BookUpdate`, and the thread's label.
    pub venue: &'static str,
    /// The fully-resolved combined-stream URL (see [`combined_stream_url`]).
    pub websocket_url: String,
    /// Fetch the REST depth snapshot that seeds the book: once per session, and once per timed
    /// re-seed interval, which re-seeds IN PLACE: the driver fetches it on a thread of its own while
    /// the pump keeps reading (quotes and trades go out at once, the book keeps folding), then splices
    /// the diffs that arrived meanwhile onto it by `depth_verdict`, the session's own rule. A gap ends
    /// the session, and the next one seeds after the driver's backoff. An `Err` at a session's start
    /// leaves it unseeded, so its first diff is a gap; at a timed re-seed it ends the session the same
    /// way (the full reconnect). Hence `Sync`.
    pub fetch_snapshot:
        Box<dyn Fn() -> Result<DepthSnapshot, Box<dyn std::error::Error>> + Send + Sync>,
}

/// Spawn the venue's market-data feed: combined bookTicker + trade + depth streams pushed into the
/// core's tick/L2 lanes via `ticks`. `tick_size` sizes the `L2Book` price grid. The thread is
/// `vike_bridge_core::depth::spawn_tick`'s (`gw-market-data`, the market-data core pin), and
/// its socket is the shared depth driver, `vike_bridge_core::depth::run_depth`, on the family's
/// `depth` timings row ([`crate::family::feed`]'s `DEPTH_TIMINGS`, the one the DOM lane reads for the
/// same `@depth@100ms` stream). Stoppable/joinable: the returned [`StopHandle`] is the shared
/// stop-and-join owner handle — `shutdown(self)` flags the thread and joins it (deterministic
/// teardown), and a plain drop does the same.
///
/// ⚠ **A gap re-dials and re-seeds after the row's backoff; a gap never re-seeds in place.** The
/// hand-written session this replaced fetched the REST snapshot again on every gap, at once: with a
/// seed that kept failing (a 429, a REST outage) every following diff, ten a second, fetched again —
/// past Binance's per-minute weight within a minute, then a 418 IP ban that would block the box's
/// ORDER calls too. Now a gap ends the session and the next seed waits the backoff. The ONE in-place
/// fetch is the timed re-seed's, once per re-seed interval on the open socket (the driver's doc).
pub fn spawn_feed_tick(
    spec: FeedTickSpec,
    ticks: TickSender,
    symbol: &str,
    tick_size: f64,
) -> StopHandle {
    let owned_symbol = symbol.to_string();
    spawn_tick(spec.venue, symbol, ticks, move |pump| {
        // An unseeded session folds into this empty book: its `last_sequence = 0` makes the first diff a
        // gap. It never takes a diff, so it is never published.
        let mut unseeded = L2Book::new(tick_size);
        run_depth(
            &spec.websocket_url,
            None, // the combined-stream URL subscribes
            None, // the venue pings and the stream answers; no app keepalive
            &[],  // REST-seeded: the timed re-seed is a fresh seed spliced in place
            || {
                let (seq, bids, asks) = (spec.fetch_snapshot)().ok()?;
                let mut book = L2Book::new(tick_size);
                book.apply_snapshot(seq, &bids, &asks);
                Some(book)
            },
            |txt, book| {
                let fold = match book {
                    Some(b) => Arc::make_mut(b),
                    None => &mut unseeded,
                };
                pump_frame(route_frame(txt, &owned_symbol, fold), pump)
            },
            |book| pump.book(book),
            |event| pump.link(event),
            pump.stop(),
            READ_POLL,
            DEPTH_TIMINGS.idle_threshold(),
            DEPTH_TIMINGS.freshness_threshold(),
            Some(DEPTH_TIMINGS.reseed_interval()),
            &vike_model::now_ms,
            DEPTH_TIMINGS.backoff(),
        );
    })
}

/// What one routed frame does on the pump: a quote or a trade goes straight onto the tick lane, a
/// folded diff is the book the driver publishes, and a gap ends the session. The diff's own `E` is
/// not read on this path (the typed scan keeps no event time), so the driver's freshness check
/// clocks the receive time.
fn pump_frame(event: DataClientEvent, pump: &FeedTickSender) -> WebsocketSessionDepthOperation {
    match event {
        DataClientEvent::Quote(quote) => pump.quote(quote),
        DataClientEvent::Trade(trade) => pump.trade(trade),
        DataClientEvent::BookUpdated { .. } => return WebsocketSessionDepthOperation::Updated(0),
        DataClientEvent::Resync => return WebsocketSessionDepthOperation::Gap,
        // `route_frame` answers none of these: a binance frame holds one print and no bar or mark.
        DataClientEvent::Trades(_)
        | DataClientEvent::Bar(_)
        | DataClientEvent::InferredClose(_)
        | DataClientEvent::Mark(_)
        | DataClientEvent::Funding(_)
        | DataClientEvent::Book(_)
        | DataClientEvent::DepthSnapshot { .. }
        | DataClientEvent::Ignored => {}
    }
    WebsocketSessionDepthOperation::Ignored
}

#[path = "pump_tests.rs"]
#[cfg(test)]
mod pump_tests;

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
#[path = "tick_grammar_tests.rs"]
#[cfg(test)]
mod tick_grammar_tests;

#[path = "seed_bound_tests.rs"]
#[cfg(test)]
mod seed_bound_tests;

#[path = "quote_stamp_tests.rs"]
#[cfg(test)]
mod quote_stamp_tests;
