//! The Binance-grammar public MARKET-DATA protocol (R8 HFT tick track) — shared by vike-binance
//! (`crate::market_data`) and vike-aster (`vike_aster::market_data`), each passing its own venue
//! string + resolved hosts (F8/F11, dedup rung 2).
//!
//! Aster forks Binance's public streams verbatim — same `<sym>@bookTicker` / `<sym>@trade` /
//! `<sym>@depth@100ms` names, same field letters, same `U`/`u` depth-sync rule — so the two venues'
//! `market_data.rs` files were byte-for-byte copies. The decoders + the depth-sync state machine
//! live here once; the venues keep only their host resolution and their spawn face.
//!
//! Depth sync is Binance's documented dance: a REST depth snapshot seeds `lastUpdateId`, then diff
//! events apply under the `U`/`u` rule — an event whose `u <= last_seq` is stale, one whose
//! `U > last_seq+1` means a gap (the pump re-snapshots), otherwise it folds via the seq-checked
//! `L2Book::apply_delta`.
//!
//! **Layering note.** `vike-bridge-core`'s own `depth.rs` deliberately owns the venue-NEUTRAL
//! lifecycle (connect/seed/fold/publish/backoff — see [`crate::family::market_feed`]'s DOM lane,
//! which drives it) and leaves the PROTOCOL per-venue. That split stands: this module shares the
//! protocol between the two SAME-PROTOCOL venues only, and is not a second lifecycle driver. The
//! pump below is the HFT tick track's own (`TickSender` lanes, not `LiveDataSink`), which never
//! went through the bridge-core driver on either venue.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;
use tungstenite::Message;

use vike_bridge_core::ws::{configure_ws_stream, is_timeout};
use vike_exec::{BookUpdate, QuoteUpdate, TickSender, TradeUpdate};
use vike_model::{L2Book, Level, QuoteTick, TradeTick};

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

/// Parse a depth side (`[["px","qty"], …]`) into `Level`s (qty 0 = remove, per L2Book). Private —
/// every caller reaches it through [`apply_depth_event`] or [`parse_depth_snapshot`], which is what
/// lets both venues keep it off their own public surface (matching their pre-rung-2 visibility).
fn parse_levels(v: Option<&Value>) -> Vec<Level> {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|lvl| {
                    let l = lvl.as_array()?;
                    Some((f(l.first())?, f(l.get(1))?))
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
    /// `U > last_seq+1` — events were missed; the caller must re-snapshot
    Gap,
    /// malformed frame
    Ignored,
}

/// Fold a `depthUpdate` diff into `book` under the U/u sequence rule (see the module doc). The
/// book's `last_seq` MUST already be seeded from the REST snapshot's `lastUpdateId`.
pub fn apply_depth_event(book: &mut L2Book, data: &Value) -> DepthOutcome {
    let (Some(first_u), Some(final_u)) =
        (data.get("U").and_then(Value::as_u64), data.get("u").and_then(Value::as_u64))
    else {
        return DepthOutcome::Ignored;
    };
    if final_u <= book.last_seq {
        return DepthOutcome::Stale;
    }
    if first_u > book.last_seq + 1 {
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
pub type DepthSnapshot = (u64, Vec<Level>, Vec<Level>);

/// Wall-clock ceiling on a `…/depth` REST book SEED — the blocking call every depth session makes
/// between its (bounded) dial and its first socket read.
///
/// **Why it needs its own number, and why the dial's bound did not cover it.** A depth session's
/// shape is dial → seed → read loop, and `vike_bridge_core::depth::run_depth_session` polls the stop
/// flag only once the read loop starts. So a stop landing during the dial is followed by a fresh REST
/// round trip on the shared `vike_bridge_core::http::blocking_agent`, whose 30 s global timeout is
/// sized for a backfill pager: three times the dial that was just bounded, in the same feed thread,
/// two lines further down, and outside
/// `crates/vike-recorder/src/recorder_cli.rs`'s `FEED_STOP_BUDGET_SECS` derivation entirely.
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

/// Reconnect loop: (re)snapshot the book, then pump the combined stream until stop/socket error.
fn md_main(
    spec: &PumpSpec,
    symbol: &str,
    tick_size: f64,
    ticks: TickSender,
    stop: &Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        if run_session(spec, symbol, tick_size, &ticks, stop).is_ok() {
            return; // clean stop
        }
        // socket/stream error — brief backoff, then reconnect + re-snapshot
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
) -> Result<(), Box<dyn std::error::Error>> {
    // The dial is BOUNDED — `vike_bridge_core::pump_spec`'s `CONNECT_10S`, through the same
    // `connect_ws` arm the market pump and the bridge-core depth driver take. It was a bare
    // `tungstenite::connect` until 2026-08-08, which applies no connect bound of any kind, so this
    // thread sat out the OS's own SYN ladder (~127 s on Linux defaults) on a black-holed route with
    // `stop` already raised behind it — the same defect the market pump's rows and
    // `crates/vike-bridge-core/src/depth.rs`'s `connect_depth` carried. This pump is the HFT tick
    // track (vike-app, not the recorder), so it does not spend the recorder's budget; it is bounded
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

#[cfg(test)]
mod seed_bound_tests {
    use super::*;
    use vike_bridge_core::pump_spec::CONNECT_10S;

    /// **The depth book SEED is bounded, and bounded to fit the dial** — the test this defect needed
    /// and did not have.
    ///
    /// The dial-bounding work stopped at the dial, and a depth session is dial → seed → read loop.
    /// The seed ran on the shared `vike_bridge_core::http::blocking_agent`, a **30 s** global timeout
    /// sized for a backfill pager, so the longest window a feed thread could sit in at `systemctl
    /// stop` was never the newly-bounded 10 s dial: it was a 30 s HTTP call immediately behind it,
    /// absent from every derivation and from `crates/vike-recorder/src/recorder_cli.rs`'s
    /// `FEED_STOP_BUDGET_SECS`. Exactly the shape `crates/bridges/binance/src/family/trades.rs`'s
    /// `the_startup_warmup_runs_on_a_bounded_agent_not_the_pagers` caught one lane over.
    ///
    /// Three assertions, each failing on a different way of reopening it: the seed agent really
    /// carries [`DEPTH_SEED_TIMEOUT`] (revert to `blocking_agent()` and this goes red); that window is
    /// genuinely SHORTER than the shared default (so the test cannot be satisfied by the two
    /// converging on 30 s); and it fits inside the dial window the recorder's budget is derived from
    /// (so a future widening has to move that budget deliberately rather than silently).
    ///
    /// MUTATION PROOF: point [`depth_seed_agent`] back at
    /// `vike_bridge_core::http::blocking_agent()` — assertion one fails on the value, assertion two
    /// on the ordering. Reads only agent configuration, so it fails identically on any box.
    #[test]
    fn the_depth_seed_runs_on_a_bounded_agent_not_the_pagers() {
        let seed = depth_seed_agent().config().timeouts().global;
        assert_eq!(
            seed,
            Some(DEPTH_SEED_TIMEOUT),
            "the depth book seed must run on its own bounded agent — it is the last blocking call \
             between a depth session's dial and its first stop poll"
        );

        let shared = vike_bridge_core::http::blocking_agent()
            .config()
            .timeouts()
            .global
            .expect("the shared agent has always carried a global timeout");
        assert!(
            DEPTH_SEED_TIMEOUT < shared,
            "the seed bound ({DEPTH_SEED_TIMEOUT:?}) must be shorter than the shared pager agent's \
             ({shared:?}), or this test is satisfied by the defect"
        );

        assert!(
            DEPTH_SEED_TIMEOUT <= CONNECT_10S,
            "the depth seed ({DEPTH_SEED_TIMEOUT:?}) is now the LARGEST window a feed thread can be \
             caught in, bigger than the dial ({CONNECT_10S:?}) the recorder's FEED_STOP_BUDGET_SECS \
             is derived from. Shorten it, or re-derive that budget deliberately."
        );
    }
}
