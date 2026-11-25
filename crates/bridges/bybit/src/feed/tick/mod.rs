//! Live Bybit-V5 public MARKET data → the vike-core tick/L2 lanes (R8 HFT track). Same shape as the
//! Binance/OKX market-data modules. Bybit has no standalone book-ticker, so the QUOTE is derived
//! from the L2 top: `orderbook.50` (snapshot then deltas) → `L2Book` → both `on_order_book` and a
//! top-of-book `TickQuote`; `publicTrade` → `TickTrade`. Formats per the Bybit V5 WS docs; the live
//! smoke verifies them against the real stream.
//!
//! The pump's socket is the shared depth driver (`vike_bridge_core::depth::run_depth`, through
//! `spawn_tick`), on this venue's `depth` timings row and keepalive — the ones the DOM lane
//! (`crates/bridges/bybit/src/feed/depth.rs`'s `depth_main`) runs on `orderbook.200`.
//!
//! ⚠ **This pump DISCLOSES its link state** (`vike_bridge_core::depth::FeedTickSender::link`) — one
//! `Disconnected` per outage, `Stale` when the book stops updating behind a live socket, and one
//! `Live` on the first book back — because it is the lane `vike-tradehub`'s CEX start actually
//! subscribes, and therefore the only one through which the connection-state dead-man
//! (`vike_core::DeadManLinkConfig`) can hear a bybit link die on that daemon. The DOM lane discloses
//! through a `DataClientSink`; this one goes onto the tick lane, because that is the seam this pump
//! has.

pub mod trades;

use std::cell::Cell;
use std::sync::Arc;

use serde_json::Value;

use vike_bridge_core::data_client::event::DataClientEvent;
use vike_bridge_core::depth::{
    FeedTickSender, WebsocketSessionDepthOperation, parse_levels, run_depth, spawn_tick,
};
use vike_bridge_core::is_valid_trade;
use vike_bridge_core::json::str_f64;
use vike_bridge_core::websocket_session::spec::READ_POLL;
use vike_execution::TickSender;
use vike_marketdata::{
    L2Book, L2BookDeltaDecision, L2BookLevel, L2BookSequencePolicy, TickQuote, TickTrade,
};
use vike_model::now_ms;
use vike_utility::poller::StopHandle;
use vike_venues::venue_timings::BYBIT;

use crate::feed::decode::{BookKind, FeedScan, scan_frame};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VenueUrls};

const VENUE: &str = "bybit";

/// Bybit drops a client that doesn't ping within ~20 s, so BOTH book sockets (this pump and the DOM
/// lane, `crate::feed`'s `depth_main`) send this every ~15 s (the shared depth driver's
/// cadence); its `pong` reply is what keeps the driver's dead-socket watchdog quiet on a quiet book.
/// The kline, mark and trades lanes send it too, at their pump row's cadence
/// (`vike_bridge_core::websocket_session::spec::row`), and the reply is what their idle watchdog hears.
pub const KEEPALIVE: &str = r#"{"op":"ping"}"#;

/// `publicTrade` datum (`{T,p,v,S}`) → TickTrade. `S` is the taker side, so a taker "Sell" means
/// the buyer was the maker. `None` for a print [`is_valid_trade`] refuses (a zero, negative, NaN or
/// infinite price or size), as the recorded tape refuses it.
pub fn decode_trade(d: &Value, symbol: &str) -> Option<TickTrade> {
    let (price, size) = (str_f64(d.get("p"))?, str_f64(d.get("v"))?);
    if !is_valid_trade(price, size) {
        return None;
    }
    Some(TickTrade {
        timestamp: d.get("T").and_then(Value::as_i64).unwrap_or(0),
        // Not stamped here: the TRADE tape that is recorded and drawn is `feed/tick/trades.rs`'s feed,
        // which stamps at receive. This pump's trades go to the strategy lane, which reads `ts` only.
        local_timestamp: 0,
        price,
        size,
        is_buyer_maker: d.get("S").and_then(Value::as_str) == Some("Sell"),
        symbol: symbol.into(),
    })
}

/// Best bid/ask of the current book → a TickQuote (Bybit has no book-ticker channel). None until
/// the book has a two-sided top. `ts` is the venue time of the orderbook frame the book last folded
/// (its top-level `ts`, [`DataClientEvent::BookUpdated`]'s `ts_ms`); `0` when the frame stated none, which
/// `vike_bridge_core::depth::send_quote` turns into the receive time.
pub fn quote_from_book(book: &L2Book, symbol: &str, ts: i64) -> Option<TickQuote> {
    let L2BookLevel { price: bid, qty: bid_size } = book.best_bid()?;
    let L2BookLevel { price: ask, qty: ask_size } = book.best_ask()?;
    Some(TickQuote {
        timestamp: ts,
        // A pure derivation stamps nothing; the pump does, at receive time (`send_quote`).
        local_timestamp: 0,
        bid,
        ask,
        bid_size,
        ask_size,
        symbol: symbol.into(),
    })
}

/// Route ONE Bybit WS text frame to a lane, folding `orderbook` snapshot/delta into `book`, as one
/// [`DataClientEvent`]: EVERY print of a `publicTrade` frame (`Trades`, never empty); the book changed,
/// with the frame's top-level venue `ts` (the pump publishes the book AND a top-of-book quote, and the
/// DOM lane's freshness watchdog needs no second parse); a seq gap (`Resync`: a forward `u` jump or a
/// counter restart, the delta NOT folded, the pump must re-seed); or nothing. Pure
/// over `book` — testable without a socket.
///
/// Bybit `orderbook.*` deltas increment `u` by exactly 1, so the delta arm consults the shared
/// [`vike_marketdata::L2Book::delta_decision`] law under [`L2BookSequencePolicy::Strict`] (the SAME decision
/// replay runs over recorded books): in-sequence folds, a duplicate (`u == last_sequence`) is
/// [`DataClientEvent::Ignored`], and anything else — a dropped frame OR a venue-restart `u` regression
/// (Bybit signals restarts by resetting `u`, e.g. a `u=1` delta) — is [`DataClientEvent::Resync`]
/// instead of silently corrupting the book until the timed reseed.
///
/// The frame is read by `feed::decode::scan_frame` first — straight into the book diff or the
/// trades, no `Value` tree — and only a frame that scan DECLINES (anything but the exact shape the
/// venue sends) reaches the `Value` decoder below, so the answer is the same on every input.
pub fn route_frame(text: &str, symbol: &str, book: &mut L2Book) -> DataClientEvent {
    match scan_frame(text, symbol) {
        // The scan's prints meet the same guard `decode_trade` applies on the `Value` path.
        Ok(FeedScan::Trades(mut t)) => {
            t.retain(|t| is_valid_trade(t.price, t.size));
            if t.is_empty() { DataClientEvent::Ignored } else { DataClientEvent::Trades(t) }
        }
        Ok(FeedScan::Ignored) => DataClientEvent::Ignored,
        Ok(FeedScan::Book(f)) => {
            let ts_ms = f.timestamp_ms;
            match f.kind {
                BookKind::Snapshot => {
                    book.apply_snapshot(f.u, &f.bids, &f.asks);
                    DataClientEvent::BookUpdated { timestamp_ms: ts_ms }
                }
                BookKind::Delta => match book.delta_decision(f.u, L2BookSequencePolicy::Strict) {
                    L2BookDeltaDecision::Apply => {
                        book.apply_delta(f.u, &f.bids, &f.asks);
                        DataClientEvent::BookUpdated { timestamp_ms: ts_ms }
                    }
                    L2BookDeltaDecision::Stale => DataClientEvent::Ignored,
                    L2BookDeltaDecision::Gap => DataClientEvent::Resync,
                },
            }
        }
        Err(_) => route_frame_value(text, symbol, book),
    }
}

/// The `Value` decode of one Bybit frame: the fallback for every frame the typed scan declines.
fn route_frame_value(text: &str, symbol: &str, book: &mut L2Book) -> DataClientEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return DataClientEvent::Ignored;
    };
    let Some(topic) = v.get("topic").and_then(Value::as_str) else {
        return DataClientEvent::Ignored; // subscribe acks carry no topic
    };
    if topic.starts_with("orderbook.") {
        let message_type = v.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(data) = v.get("data") else {
            return DataClientEvent::Ignored;
        };
        let u = data.get("u").and_then(Value::as_u64).unwrap_or(0);
        let (b, a) = (parse_levels(data.get("b")), parse_levels(data.get("a")));
        let ts_ms = v.get("ts").and_then(Value::as_i64).unwrap_or(0);
        match message_type {
            "snapshot" => {
                book.apply_snapshot(u, &b, &a);
                DataClientEvent::BookUpdated { timestamp_ms: ts_ms }
            }
            "delta" => match book.delta_decision(u, L2BookSequencePolicy::Strict) {
                L2BookDeltaDecision::Apply => {
                    book.apply_delta(u, &b, &a);
                    DataClientEvent::BookUpdated { timestamp_ms: ts_ms }
                }
                L2BookDeltaDecision::Stale => DataClientEvent::Ignored, // duplicate — book stays trustworthy
                L2BookDeltaDecision::Gap => DataClientEvent::Resync, // dropped frames / venue restart
            },
            _ => DataClientEvent::Ignored, // unknown type
        }
    } else if topic.starts_with("publicTrade") {
        // EVERY readable row, in frame order (a row it cannot read is dropped, the others stay).
        let trades: Vec<TickTrade> = v
            .get("data")
            .and_then(Value::as_array)
            .map(|rows| rows.iter().filter_map(|d| decode_trade(d, symbol)).collect())
            .unwrap_or_default();
        if trades.is_empty() { DataClientEvent::Ignored } else { DataClientEvent::Trades(trades) }
    } else {
        DataClientEvent::Ignored
    }
}

/// Parse a Bybit `orderbook` SNAPSHOT frame's raw levels + sequence. `None` if the frame isn't an
/// orderbook snapshot (a delta, ack, or trade). The DOM depth feed uses this to infer the tick +
/// seed the book before handing subsequent frames to [`route_frame`] (which needs a pre-built book).
pub fn parse_orderbook_snapshot(text: &str) -> Option<(u64, Vec<L2BookLevel>, Vec<L2BookLevel>)> {
    let v: Value = serde_json::from_str(text).ok()?;
    if !v.get("topic").and_then(Value::as_str)?.starts_with("orderbook.") {
        return None;
    }
    if v.get("type").and_then(Value::as_str) != Some("snapshot") {
        return None;
    }
    let data = v.get("data")?;
    let u = data.get("u").and_then(Value::as_u64).unwrap_or(0);
    Some((u, parse_levels(data.get("b")), parse_levels(data.get("a"))))
}

/// The in-place re-seed of one bybit book `topic` on an open socket (the shared depth driver's
/// `resubscribe`, both book lanes): unsubscribe it, then subscribe it again, which bybit answers with
/// a fresh `snapshot` frame — the way a session's first book is seeded. Only the book topic moves; a
/// trades topic on the same socket is untouched.
pub(crate) fn book_resubscribe(topic: &str) -> [String; 2] {
    [
        serde_json::json!({"op": "unsubscribe", "args": [topic]}).to_string(),
        serde_json::json!({"op": "subscribe", "args": [topic]}).to_string(),
    ]
}

/// The subscribe frame: L2-50 book + public trades for `symbol`.
pub fn subscribe_frame(symbol: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [format!("orderbook.50.{symbol}"), format!("publicTrade.{symbol}")]
    })
    .to_string()
}

/// Spawn the Bybit public market-data feed for `symbol` (e.g. "BTCUSDT") into the tick/L2 lanes.
///
/// The thread is `vike_bridge_core::depth::spawn_tick`'s and its socket the shared depth
/// driver, on [`BYBIT`]'s `depth` timings row (the DOM lane's) and [`KEEPALIVE`]: WS-seeded (the
/// subscription's `snapshot` frame builds the book), a seq gap ends the session and the next one
/// re-subscribes after the row's backoff, and every re-seed interval the book is re-seeded IN PLACE
/// (bybit's book carries no checksum): [`book_resubscribe`] on the same socket, whose fresh snapshot
/// rebuilds it while the trades keep flowing. The returned [`StopHandle`]
/// is the shared stop-and-join owner handle: `shutdown(self)` flags the thread and joins it, and a
/// plain drop does the same.
///
/// `urls` are the venue's rows: the public socket base is the `live` `data` `ws` row, looked up ONCE
/// here (every account reads MAINNET prices, a demo one included, as it always has).
///
/// # Errors
/// The refusal naming the missing row, or the user name's book refused in words ([`pump_route`]): no thread is
/// spawned.
pub fn spawn_bybit_feed_tick(
    urls: &VenueUrls,
    ticks: TickSender,
    symbol: &str,
    tick_size: f64,
) -> Result<StopHandle, String> {
    let websocket_base = urls
        .url(UrlType::Live, UrlChannel::Data, UrlProtocol::Websocket, "")
        .map_err(|why| why.to_string())?;
    let (host, wire) = pump_route(websocket_base, symbol)?;
    let (subscribe, label) = (subscribe_frame(&wire), symbol.to_string());
    let resubscribe = book_resubscribe(&format!("orderbook.50.{wire}"));
    Ok(spawn_tick(VENUE, symbol, ticks, move |pump| {
        let depth = BYBIT.depth();
        // The venue time of the frame the book last folded: the derived quote's `ts`.
        let book_ts = Cell::new(0);
        run_depth(
            &host,
            Some(&subscribe),
            Some(KEEPALIVE),
            &resubscribe.each_ref().map(String::as_str), // the timed re-seed, on this socket
            || None, // WS-seeded: the subscription's `snapshot` frame builds the book
            |txt, book| pump_frame(txt, &wire, book, tick_size, pump, &book_ts),
            |book| {
                // publish the book, then a derived top-of-book quote
                pump.book(book);
                if let Some(quote) = quote_from_book(book, &label, book_ts.get()) {
                    pump.quote(quote);
                }
            },
            |event| pump.link(event),
            pump.stop(),
            READ_POLL,
            depth.idle_threshold(),
            depth.freshness_threshold(),
            Some(depth.reseed_interval()),
            &now_ms,
            depth.backoff(),
        );
    }))
}

/// What one frame does on the pump: its trades go straight onto the tick lane, a folded book is the
/// book the driver publishes (its venue time kept in `book_ts` for the derived quote), and a seq gap
/// (a dropped frame or a venue counter restart) ends the session, whose successor's `snapshot`
/// rebuilds the book. The book starts empty on the catalog grid (`tick_size`) at the first frame
/// after the driver cleared it (a session's start), and a delta that reaches it before the snapshot
/// does is no gap: it predates the book, which the snapshot on its way builds (the DOM lane's rule,
/// `crates/bridges/bybit/src/feed/depth.rs`'s `fold_depth_frame`).
fn pump_frame(
    txt: &str,
    wire: &str,
    book: &mut Option<Arc<L2Book>>,
    tick_size: f64,
    pump: &FeedTickSender,
    book_ts: &Cell<i64>,
) -> WebsocketSessionDepthOperation {
    let fold = book.get_or_insert_with(|| Arc::new(L2Book::new(tick_size)));
    let unseeded = fold.last_sequence == 0; // no snapshot has reached this book yet
    match route_frame(txt, wire, Arc::make_mut(fold)) {
        DataClientEvent::Trades(trades) => trades.into_iter().for_each(|trade| pump.trade(trade)),
        DataClientEvent::BookUpdated { timestamp_ms: ts_ms } => {
            book_ts.set(ts_ms);
            return WebsocketSessionDepthOperation::Updated(ts_ms);
        }
        DataClientEvent::Resync if unseeded => {}
        DataClientEvent::Resync => return WebsocketSessionDepthOperation::Gap,
        // `route_frame` answers none of these: bybit's L1 is derived from the book, not decoded.
        DataClientEvent::Quote(_)
        | DataClientEvent::Trade(_)
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

/// **The public socket and the wire name of the symbol this pump was spawned for** (decisions 0231 and 0290). The
/// pump feeds the price the control door waits for before it admits a symbol, and an admitted symbol is no longer
/// always a linear contract: the USER name's KEPT row says the book and the venue's name
/// (`crate::instrument::class::venue_symbol`): `BTCUSDT.P` is the linear socket's `BTCUSDT`, the spot pair `BTCUSDT`
/// the spot socket's, `BTCUSD.P` and `BTCUSD_261225` the inverse socket's `BTCUSD` and `BTCUSDZ26`
/// (`crate::feed::websocket_host` over `websocket_base`). ⚠ A name with no kept row is NO pump, refused in words,
/// never a default to `linear` (BYB-6: a wrong-host subscription is answered `handler not found` and streams nothing,
/// so the door would refuse the order for want of a price with no word of why). The ticks keep the CALLER's label.
fn pump_route(websocket_base: &str, symbol: &str) -> Result<(String, String), String> {
    let (wire, book) = crate::instrument::class::venue_symbol(symbol)?;
    Ok((crate::feed::websocket_host(websocket_base, book), wire))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use vike_bridge_core::depth::send_quote;

    /// **The WS re-seed on the open socket, over the real fold.** At the timed re-seed the pump
    /// re-subscribes its book topic on the SAME socket ([`book_resubscribe`]) and the book is cleared;
    /// a delta still in flight from the old subscription waits (no gap), and the fresh snapshot
    /// rebuilds the book, whose next delta chains. What the strategy gets after the re-seed is exactly
    /// that snapshot plus that delta, and the session goes on.
    #[test]
    fn a_timed_reseed_resubscribes_the_book_and_the_fresh_snapshot_rebuilds_it() {
        use std::cell::RefCell;
        use std::sync::atomic::AtomicBool;
        use vike_bridge_core::depth::run_depth_session;
        use vike_bridge_core::scripted::WebsocketSessionMarketStreamScript;
        use vike_bridge_core::stream_health::StreamHealth;
        let (events, _rx) = vike_execution::event_channel(64);
        let ticks = TickSender { ingest: events.ingest.clone() };
        let pump = FeedTickSender::new(VENUE, "BTCUSDT", ticks, Arc::new(AtomicBool::new(false)));
        let frame = |typ: &str, u: u64, bid: &str| {
            serde_json::json!({
                "topic": "orderbook.50.BTCUSDT", "type": typ, "ts": 1_700_000_000_000_i64,
                "data": {"s":"BTCUSDT","b":[[bid,"5"]],"a":[["60100.0","4"]],"u":u,"seq":u}
            })
            .to_string()
        };
        let frames = [
            frame("snapshot", 10, "60000.0"),
            frame("delta", 11, "60000.1"),
            "late".to_string(),
            frame("delta", 12, "60000.2"), // in flight from the old subscription: waits
            frame("snapshot", 500, "59990.0"),
            frame("delta", 501, "59990.5"),
        ];
        let (clock, book_ts) = (Cell::new(0_i64), Cell::new(0));
        let (published, reseeded) = (RefCell::new(Vec::new()), Cell::new(false));
        let mut stream = WebsocketSessionMarketStreamScript::from_texts(frames);
        let resubscribe = book_resubscribe("orderbook.50.BTCUSDT");
        let ended = run_depth_session(
            &mut stream,
            Some(&subscribe_frame("BTCUSDT")),
            None,
            &resubscribe.each_ref().map(String::as_str),
            &|| None::<L2Book>,
            &mut |txt: &str, book: &mut Option<Arc<L2Book>>| {
                if txt == "late" {
                    clock.set(clock.get() + 600_000); // the re-seed is due at the next loop pass
                    return WebsocketSessionDepthOperation::Ignored;
                }
                pump_frame(txt, "BTCUSDT", book, 0.1, &pump, &book_ts)
            },
            &mut |b: &Arc<L2Book>| {
                if reseeded.get() {
                    published.borrow_mut().push(Arc::clone(b));
                }
            },
            &mut |_event: vike_model::StreamStatus| {},
            &mut || reseeded.set(true),
            &mut StreamHealth::new(86_400_000),
            &AtomicBool::new(false),
            Duration::from_secs(30),
            Some(Duration::from_secs(300)),
            &|| clock.get(),
        );
        let ended = ended.expect_err("the script runs out").to_string();
        assert!(ended.contains("script exhausted"), "the session went on: {ended}");
        assert_eq!(
            stream.sent(),
            [subscribe_frame("BTCUSDT"), resubscribe[0].clone(), resubscribe[1].clone()],
            "the book topic re-subscribed on the same socket"
        );
        let mut want = L2Book::new(0.1);
        want.apply_snapshot(
            500,
            &[L2BookLevel::new(59990.0, 5.0)],
            &[L2BookLevel::new(60100.0, 4.0)],
        );
        want.apply_delta(501, &[L2BookLevel::new(59990.5, 5.0)], &[L2BookLevel::new(60100.0, 4.0)]);
        let published = published.into_inner();
        assert_eq!(published.len(), 2, "the fresh snapshot, then its delta");
        assert_eq!((published[1].last_sequence, published[1].top_n(10)), (501, want.top_n(10)));
    }

    /// A delta that reaches the pump before the book's snapshot is no gap (it predates the book the
    /// snapshot builds): ignored, the session kept. Once the snapshot has landed, a dropped delta is.
    #[test]
    fn a_delta_before_the_snapshot_waits_for_it_and_is_no_gap() {
        use std::sync::atomic::AtomicBool;
        let (events, _rx) = vike_execution::event_channel(8);
        let ticks = TickSender { ingest: events.ingest.clone() };
        let pump = FeedTickSender::new(VENUE, "BTCUSDT", ticks, Arc::new(AtomicBool::new(false)));
        let (mut book, book_ts) = (None, Cell::new(0));
        let mut step = |frame: serde_json::Value| {
            pump_frame(&frame.to_string(), "BTCUSDT", &mut book, 0.1, &pump, &book_ts)
        };
        let frame = |typ: &str, u: u64| {
            serde_json::json!({
                "topic": "orderbook.50.BTCUSDT", "type": typ, "ts": 1_700_000_000_000_i64,
                "data": {"s":"BTCUSDT","b":[["60000.0","5"]],"a":[["60000.1","4"]],"u":u,"seq":u}
            })
        };
        assert_eq!(
            step(frame("delta", 41)),
            WebsocketSessionDepthOperation::Ignored,
            "before the snapshot: no gap"
        );
        assert_eq!(
            step(frame("snapshot", 40)),
            WebsocketSessionDepthOperation::Updated(1_700_000_000_000)
        );
        assert_eq!(
            step(frame("delta", 41)),
            WebsocketSessionDepthOperation::Updated(1_700_000_000_000)
        );
        assert_eq!(
            step(frame("delta", 43)),
            WebsocketSessionDepthOperation::Gap,
            "after it, a dropped delta is a gap"
        );
    }

    /// The pump's frame step: the first frame opens an empty book on the catalog grid, a snapshot is
    /// the book the driver publishes with the frame's venue time (kept for the derived quote), a
    /// trade goes straight onto the tick lane under the CALLER's label, and a gapped delta ends the
    /// session.
    #[test]
    fn the_pump_step_publishes_the_book_with_its_time_and_gaps_on_a_dropped_delta() {
        use std::sync::atomic::AtomicBool;
        let (events, mut rx) = vike_execution::event_channel(8);
        let ticks = TickSender { ingest: events.ingest.clone() };
        let pump =
            FeedTickSender::new(VENUE, "DOGEUSDT.P", ticks, Arc::new(AtomicBool::new(false)));
        let (mut book, book_ts) = (None, Cell::new(0));
        let mut step = |frame: serde_json::Value| {
            pump_frame(&frame.to_string(), "DOGEUSDT", &mut book, 0.0001, &pump, &book_ts)
        };
        let snap = serde_json::json!({
            "topic": "orderbook.50.DOGEUSDT", "type": "snapshot", "ts": 1_700_000_000_000_i64,
            "data": {"s":"DOGEUSDT","b":[["0.1000","5"]],"a":[["0.1001","4"]],"u":10,"seq":1}
        });
        assert_eq!(step(snap), WebsocketSessionDepthOperation::Updated(1_700_000_000_000));
        let trade = serde_json::json!({
            "topic": "publicTrade.DOGEUSDT",
            "data": [{"T": 1700, "s": "DOGEUSDT", "S": "Buy", "v": "10", "p": "0.1"}]
        });
        assert_eq!(step(trade), WebsocketSessionDepthOperation::Ignored);
        let gap = serde_json::json!({
            "topic": "orderbook.50.DOGEUSDT", "type": "delta", "ts": 1_700_000_000_100_i64,
            "data": {"s":"DOGEUSDT","b":[["0.1000","9"]],"a":[],"u":12,"seq":3}
        });
        assert_eq!(step(gap), WebsocketSessionDepthOperation::Gap);
        assert_eq!(book_ts.get(), 1_700_000_000_000, "the time of the frame the book last folded");
        assert_eq!(book.as_ref().map(|b| b.tick_size), Some(0.0001), "the catalog grid");
        let Ok(vike_execution::Ingest::Trade(t)) = rx.try_recv() else {
            panic!("the print is on the lane")
        };
        assert_eq!(t.symbol, "DOGEUSDT.P", "the caller's label, not the wire's");
        assert!(
            rx.try_recv().is_err(),
            "the step publishes no book itself: the driver's on_book does"
        );
    }

    #[test]
    fn public_trade_decodes_taker_side() {
        let frame = serde_json::json!({
            "topic": "publicTrade.BTCUSDT",
            "data": [{"T": 1700, "s": "BTCUSDT", "S": "Sell", "v": "0.4", "p": "60000.7"}]
        })
        .to_string();
        let mut book = L2Book::new(0.1);
        match route_frame(&frame, "BTCUSDT", &mut book) {
            DataClientEvent::Trades(trades) => {
                assert_eq!(trades.len(), 1);
                let t = &trades[0];
                assert_eq!(t.timestamp, 1700);
                assert_eq!(t.price, 60000.7);
                assert_eq!(t.size, 0.4);
                assert!(t.is_buyer_maker, "taker Sell → buyer was the maker");
            }
            other => panic!("expected Trades, got {other:?}"),
        }
    }

    /// The pump refuses the prints the recorded tape refuses ([`is_valid_trade`]): a zero price, a
    /// negative size and a `NaN` price each drop their row, and the readable row stays.
    #[test]
    fn a_print_the_trade_guard_refuses_is_dropped() {
        let frame = serde_json::json!({
            "topic": "publicTrade.BTCUSDT", "type": "snapshot", "ts": 1700,
            "data": [
                {"T": 1, "s": "BTCUSDT", "S": "Buy", "v": "0.4", "p": "0"},
                {"T": 2, "s": "BTCUSDT", "S": "Buy", "v": "-0.4", "p": "60000.7"},
                {"T": 3, "s": "BTCUSDT", "S": "Buy", "v": "0.4", "p": "NaN"},
                {"T": 4, "s": "BTCUSDT", "S": "Sell", "v": "0.4", "p": "60000.7"},
            ]
        })
        .to_string();
        match route_frame(&frame, "BTCUSDT", &mut L2Book::new(0.1)) {
            DataClientEvent::Trades(trades) => {
                assert_eq!(trades.iter().map(|t| t.timestamp).collect::<Vec<_>>(), [4]);
            }
            other => panic!("expected Trades, got {other:?}"),
        }
    }

    #[test]
    fn orderbook_snapshot_then_delta() {
        let mut book = L2Book::new(0.1);
        let snap = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "snapshot",
            "data": {"s":"BTCUSDT","b":[["60000.0","5"],["59999.0","2"]],"a":[["60001.0","4"]],"u":10,"seq":1}
        })
        .to_string();
        assert_eq!(
            route_frame(&snap, "BTCUSDT", &mut book),
            DataClientEvent::BookUpdated { timestamp_ms: 0 }
        );
        assert_eq!(book.best_bid(), Some(L2BookLevel::new(60000.0, 5.0)));
        assert_eq!(book.best_ask(), Some(L2BookLevel::new(60001.0, 4.0)));

        // delta: update best bid qty, remove the ask (size 0)
        let delta = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "delta",
            "data": {"s":"BTCUSDT","b":[["60000.0","8"]],"a":[["60001.0","0"]],"u":11,"seq":2}
        })
        .to_string();
        assert_eq!(
            route_frame(&delta, "BTCUSDT", &mut book),
            DataClientEvent::BookUpdated { timestamp_ms: 0 }
        );
        assert_eq!(book.best_bid(), Some(L2BookLevel::new(60000.0, 8.0)));
        assert_eq!(book.best_ask(), None);

        // a top-of-book quote can be derived once both sides exist again
        assert!(quote_from_book(&book, "BTCUSDT", 0).is_none(), "one-sided → no quote");

        // stale delta (u <= last_sequence) is dropped
        let stale = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "delta",
            "data": {"s":"BTCUSDT","b":[["60000.0","99"]],"a":[],"u":11,"seq":3}
        })
        .to_string();
        assert_eq!(route_frame(&stale, "BTCUSDT", &mut book), DataClientEvent::Ignored);
        assert_eq!(book.best_bid(), Some(L2BookLevel::new(60000.0, 8.0)), "stale must not apply");
    }

    /// A dropped frame (snapshot u=10, next delta u=12 — Bybit deltas increment `u` by 1) is a
    /// detectable gap: the frame answers `Resync` and the delta is NOT folded into the book —
    /// previously it silently applied (any `u > last_sequence`), corrupting the book until the timed
    /// reseed.
    #[test]
    fn dropped_frame_is_a_resync_and_the_delta_is_not_folded() {
        let mut book = L2Book::new(0.1);
        let snap = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "snapshot",
            "data": {"s":"BTCUSDT","b":[["60000.0","5"]],"a":[["60001.0","4"]],"u":10,"seq":1}
        })
        .to_string();
        assert_eq!(
            route_frame(&snap, "BTCUSDT", &mut book),
            DataClientEvent::BookUpdated { timestamp_ms: 0 }
        );

        // u jumps 10 → 12: the u=11 frame was dropped somewhere upstream
        let gap = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "delta",
            "data": {"s":"BTCUSDT","b":[["60000.0","99"]],"a":[],"u":12,"seq":3}
        })
        .to_string();
        assert_eq!(route_frame(&gap, "BTCUSDT", &mut book), DataClientEvent::Resync);
        assert_eq!(
            book.best_bid(),
            Some(L2BookLevel::new(60000.0, 5.0)),
            "a gapped delta must NOT fold"
        );
        assert_eq!(book.last_sequence, 10, "the book's seq anchor is untouched by a gapped delta");
    }

    /// A `u` REGRESSION (a venue restart resets the counter — Bybit signals restarts with a low
    /// `u`, e.g. `u=1`) is a gap → `Resync`, NOT a stale drop: previously every post-restart
    /// delta read as stale (`u <= last_sequence`) and the book froze until the timed reseed.
    #[test]
    fn u_regression_is_a_resync_not_a_stale_drop() {
        let mut book = L2Book::new(0.1);
        let snap = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "snapshot",
            "data": {"s":"BTCUSDT","b":[["60000.0","5"]],"a":[["60001.0","4"]],"u":1000,"seq":1}
        })
        .to_string();
        assert_eq!(
            route_frame(&snap, "BTCUSDT", &mut book),
            DataClientEvent::BookUpdated { timestamp_ms: 0 }
        );

        let restart = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "delta",
            "data": {"s":"BTCUSDT","b":[["59999.0","1"]],"a":[],"u":1,"seq":2}
        })
        .to_string();
        assert_eq!(route_frame(&restart, "BTCUSDT", &mut book), DataClientEvent::Resync);
        assert_eq!(
            book.best_bid(),
            Some(L2BookLevel::new(60000.0, 5.0)),
            "a regressed delta must NOT fold"
        );
        // an exact duplicate of the last applied u stays a plain stale drop, not a resync storm
        let dup = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "delta",
            "data": {"s":"BTCUSDT","b":[["59999.0","1"]],"a":[],"u":1000,"seq":2}
        })
        .to_string();
        assert_eq!(route_frame(&dup, "BTCUSDT", &mut book), DataClientEvent::Ignored);
    }

    #[test]
    fn subscribe_ack_ignored() {
        let mut book = L2Book::new(0.1);
        assert_eq!(
            route_frame(r#"{"success":true,"op":"subscribe"}"#, "BTCUSDT", &mut book),
            DataClientEvent::Ignored
        );
    }

    /// The stop-flag poll of the socket reads in this file and in `market_feed`'s depth lane is the
    /// 2 s each held as a private `READ_TIMEOUT` before `vike_bridge_core::websocket_session::spec::READ_POLL`
    /// (the venue-free mechanism default, decision 0160) replaced the copies.
    #[test]
    fn the_sockets_poll_their_stop_flag_every_two_seconds() {
        assert_eq!(vike_bridge_core::websocket_session::spec::READ_POLL, Duration::from_secs(2));
    }

    /// A derived quote leaves the pump stamped with the machine receive time (`send_quote`) and keeps
    /// the frame's venue time; the pure derivation ([`quote_from_book`]) stamps nothing.
    #[test]
    fn a_quote_leaves_the_pump_stamped_with_the_machine_receive_time() {
        let (events, mut rx) = vike_execution::event_channel(4);
        let ticks = TickSender { ingest: events.ingest.clone() };
        let mut book = L2Book::new(0.1);
        book.apply_snapshot(
            1,
            &[L2BookLevel::new(60000.0, 5.0)],
            &[L2BookLevel::new(60000.1, 3.0)],
        );
        let quote = quote_from_book(&book, "BTCUSDT", 1_700).expect("a two-sided book has a top");
        assert_eq!(
            (quote.timestamp, quote.local_timestamp),
            (1_700, 0),
            "the frame's time; no stamp"
        );

        let before = vike_model::now_ms();
        send_quote(&ticks, VENUE, "BTCUSDT", quote).expect("the channel is open");
        let after = vike_model::now_ms();

        let Ok(vike_execution::Ingest::Quote(update)) = rx.try_recv() else {
            panic!("a QuoteUpdate must be on the lane")
        };
        assert_eq!((update.venue.as_str(), update.symbol.as_str()), ("bybit", "BTCUSDT"));
        assert_eq!(
            update.quote.timestamp, 1_700,
            "the venue's own time is untouched, not stamped 0"
        );
        // a book price is `tick x tick_size`, so it is the level's price up to one rounding
        assert!(
            (update.quote.bid - 60000.0).abs() < 1e-6 && (update.quote.ask - 60000.1).abs() < 1e-6
        );
        assert!(
            (before..=after).contains(&update.quote.local_timestamp),
            "local_ts {} must be the receive time, between {before} and {after}",
            update.quote.local_timestamp
        );
    }

    /// **The price lane dials the socket of the book the USER name's kept row names** (decisions 0231 and 0290): a
    /// wrong-host subscription is answered `handler not found` and streams nothing, and the control door refuses an
    /// opening order on a symbol it can get no price for. The perpetual `BTCUSDT.P` is the LINEAR book's `BTCUSDT`,
    /// the pair `BTCUSDT` the spot book's; the label stays the caller's. ⚠ A name with no kept row (an old exec
    /// spelling, an unknown name) gets NO pump, refused in words, never the linear socket (BYB-6: `BTCUSDZ26` with no
    /// row went to `linear` and streamed nothing).
    #[test]
    fn the_pump_dials_the_socket_of_the_symbols_book() {
        crate::history::kept_list_for_tests();
        // The seed's `live` `data` `ws` row: the three sockets byte for byte as the constants held them.
        let urls = vike_venues::venue_urls::VenueUrls::seed(VENUE);
        let ws =
            urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Websocket, "").expect("seeded");
        const LINEAR: &str = "wss://stream.bybit.com/v5/public/linear";
        const INVERSE: &str = "wss://stream.bybit.com/v5/public/inverse";
        const SPOT: &str = "wss://stream.bybit.com/v5/public/spot";
        for (symbol, host, wire) in [
            ("BTCUSDT.P", LINEAR, "BTCUSDT"),
            ("BTCUSDC.P", LINEAR, "BTCPERP"),
            ("ETHUSDT_261127", LINEAR, "ETHUSDT-27NOV26"),
            ("BTCUSD.P", INVERSE, "BTCUSD"),
            ("BTCUSD_261225", INVERSE, "BTCUSDZ26"),
            ("BTCUSDT", SPOT, "BTCUSDT"),
            ("DOGEUSDT", SPOT, "DOGEUSDT"),
        ] {
            let want = (host.to_string(), wire.to_string());
            assert_eq!(pump_route(ws, symbol), Ok(want), "{symbol}");
        }
        for unplaced in ["XYZ", "BTCUSDZ26", "DOGEUSDT.S", "SOLUSD_261225"] {
            let why = pump_route(ws, unplaced).expect_err("no kept row: no pump");
            assert!(why.contains(unplaced), "{why}");
        }
        let (events, _rx) = vike_execution::event_channel(1);
        let ticks = TickSender { ingest: events.ingest.clone() };
        let none = vike_venues::venue_urls::VenueUrls::default();
        assert!(spawn_bybit_feed_tick(&none, ticks, "BTCUSDT", 0.1).is_err(), "no row, no pump");
    }
}
