//! Live OKX-V5 public MARKET data → the vike-core tick/L2 lanes (R8 HFT track). Mirrors the
//! Binance market-data module's shape (pure `route_frame` decoders + a stoppable WS pump), but for
//! OKX's public channels: `bbo-tbt` → `TickQuote`, `trades` → `TickTrade`, `books5` (a FULL top-5
//! snapshot each push, so no delta-sync) → `L2Book`. Message formats per the OKX V5 WS docs; the
//! live smoke verifies them against the real stream.
//!
//! The pump's socket is the shared depth driver (`vike_bridge_core::depth::run_depth`, through
//! `spawn_tick`), on this venue's `depth` timings row and keepalive — the ones the DOM lane
//! (`crates/bridges/okx/src/feed/depth.rs`'s `depth_main`) runs on `books`.
//!
//! ⚠ **This pump DISCLOSES its link state** (`vike_bridge_core::depth::FeedTickSender::link`) — one
//! `Disconnected` per outage, `Stale` when the book stops updating behind a live socket, and one
//! `Live` on the first book back — because it is the lane `vike-tradehub`'s CEX start actually
//! subscribes, and therefore the only one through which the connection-state dead-man
//! (`vike_core::DeadManLinkConfig`) can hear an okx link die on that daemon. The DOM lane discloses
//! through a `DataClientSink`; this one goes onto the tick lane, because that is the seam this pump
//! has.

pub mod trades;

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
use vike_marketdata::{L2Book, L2BookLevel, TickQuote, TickTrade};
use vike_model::now_ms;
use vike_utility::poller::StopHandle;
use vike_venues::venue_timings::OKX;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

const VENUE: &str = "okx";

/// OKX drops a client that doesn't ping within ~30 s, so BOTH book sockets on the public socket
/// (this pump and the DOM lane, `crate::feed`'s `depth_main`) send this raw text every ~15 s
/// (the shared depth driver's cadence); its raw `pong` reply is what keeps the driver's dead-socket
/// watchdog quiet on a quiet book. The kline, mark and trades lanes send it too, at their pump row's
/// cadence (`vike_bridge_core::websocket_session::spec::row`), so a quiet instrument's lane is not cut.
pub const KEEPALIVE: &str = "ping";
/// OKX V5 public WS path, on the `okx` `live` `data` `ws` row (product `""`): the socket this L2 feed and
/// `market_feed`'s mark, trades and depth lanes dial. NOT the candle lane: OKX serves the `candle<bar>`
/// channels on the [`PATH_WEBSOCKET_BUSINESS`] socket only.
pub const PATH_WEBSOCKET_PUBLIC: &str = "/ws/v5/public";

/// OKX V5 BUSINESS WS path, on the `business` product's `ws` row: the only socket that serves the
/// `candle<bar>` channels (`market_feed`'s kline lane, public data, no login) and the exec plane's
/// `orders-algo` (logged in, `crate::execution::rest::OkxNet`'s `business_websocket`). The public socket answers a candle
/// subscribe with an `event:"error"` "wrong URL or channel" frame and then nothing, so a kline lane dialled
/// there never delivers a bar (MEASURED 2026-10-31: `candle1m` and `candle3m` of `BTC-USDT-SWAP`; `trades`,
/// `mark-price`, `books`, `books5` and `bbo-tbt` answer on the public socket). Port 443 where the public
/// row is still on 8443: the venue's notice on 8443 (code 64010) says it "will be retired soon".
pub const PATH_WEBSOCKET_BUSINESS: &str = "/ws/v5/business";

/// The `venue_url` product word of okx's BUSINESS socket rows (the public and private sockets are
/// product `""`).
pub const PRODUCT_BUSINESS: &str = "business";

/// The public market-data socket a TIER-LESS feed dials: `path` on the `okx` `live` `data` `ws` row of
/// `product` (`""` with [`PATH_WEBSOCKET_PUBLIC`], [`PRODUCT_BUSINESS`] with [`PATH_WEBSOCKET_BUSINESS`]). Every demo
/// account's prices come from mainnet by design, so nothing here has a type to choose.
///
/// # Errors
/// The lookup's refusal (no such row): the feed is not built, and the caller says why.
pub fn feed_websocket(urls: &VenueUrls, product: &str, path: &str) -> Result<String, UrlRefusal> {
    Ok(format!(
        "{}{path}",
        urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Websocket, product)?
    ))
}

/// First level `[px, sz, …]` of an OKX bids/asks array → (price, size).
fn top(side: Option<&Value>) -> Option<(f64, f64)> {
    let level = side?.as_array()?.first()?.as_array()?;
    Some((str_f64(level.first())?, str_f64(level.get(1))?))
}

/// `bbo-tbt` datum (`{bids:[[px,sz,..]],asks:[[px,sz,..]],ts}`) → TickQuote.
pub fn decode_bbo(d: &Value, symbol: &str) -> Option<TickQuote> {
    let (bid, bid_size) = top(d.get("bids"))?;
    let (ask, ask_size) = top(d.get("asks"))?;
    Some(TickQuote {
        timestamp: d.get("ts").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or(0),
        // A pure decoder stamps nothing; the pump does, at receive time (`send_quote`).
        local_timestamp: 0,
        bid,
        ask,
        bid_size,
        ask_size,
        symbol: symbol.into(),
    })
}

/// `trades` datum (`{px,sz,side,ts}`) → TickTrade. OKX `side` is the taker side, so a taker "sell"
/// means the buyer was the maker. `None` for a print [`is_valid_trade`] refuses (a zero, negative,
/// NaN or infinite price or size), as the recorded tape refuses it.
pub fn decode_trade(d: &Value, symbol: &str) -> Option<TickTrade> {
    let (price, size) = (str_f64(d.get("px"))?, str_f64(d.get("sz"))?);
    if !is_valid_trade(price, size) {
        return None;
    }
    Some(TickTrade {
        timestamp: d.get("ts").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or(0),
        // Not stamped here: the TRADE tape that is recorded and drawn is `feed/tick/trades.rs`'s feed,
        // which stamps at receive. This pump's trades go to the strategy lane, which reads `ts` only.
        local_timestamp: 0,
        price,
        size,
        is_buyer_maker: d.get("side").and_then(Value::as_str) == Some("sell"),
        symbol: symbol.into(),
    })
}

/// Route ONE OKX WS text frame to a lane, rebuilding `book` from a `books5` snapshot, as one
/// [`DataClientEvent`]: a quote, EVERY print of one `trades` frame (`Trades`, never empty), the book
/// changed (no venue time read: `ts_ms` 0), or nothing. Pure over `book` — testable without a socket.
/// Non-data frames (subscribe acks, `pong`) → Ignored.
pub fn route_frame(text: &str, symbol: &str, book: &mut L2Book) -> DataClientEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return DataClientEvent::Ignored;
    };
    let channel = v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str);
    let rows = v.get("data").and_then(Value::as_array);
    if channel == Some("trades") {
        // EVERY readable row, in frame order (a row it cannot read is dropped, the others stay).
        let trades: Vec<TickTrade> = rows
            .map(|rows| rows.iter().filter_map(|d| decode_trade(d, symbol)).collect())
            .unwrap_or_default();
        return if trades.is_empty() {
            DataClientEvent::Ignored
        } else {
            DataClientEvent::Trades(trades)
        };
    }
    match (channel, rows.and_then(|d| d.first())) {
        (Some("bbo-tbt"), Some(d)) => {
            decode_bbo(d, symbol).map_or(DataClientEvent::Ignored, DataClientEvent::Quote)
        }
        (Some("books5"), Some(d)) => {
            // full top-5 snapshot each push — clear + rebuild (ts as the monotonic seq)
            let seq = d.get("ts").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or(0);
            book.apply_snapshot(seq, &parse_levels(d.get("bids")), &parse_levels(d.get("asks")));
            DataClientEvent::BookUpdated { timestamp_ms: 0 }
        }
        _ => DataClientEvent::Ignored,
    }
}

/// A decoded OKX `books` (400-level, snapshot+update) frame — the DEEP book for the DOM, distinct
/// from the shallow `books5` handled by [`route_frame`]. `Other` = a non-`books` frame / ack / pong.
#[derive(Debug, Clone, PartialEq)]
pub enum BooksFrame {
    /// full 400-level state (`action:"snapshot"`) — clear + rebuild. `ts_ms` is `data[0].ts` (a
    /// decimal STRING on the wire) as epoch-ms, `0` when absent or unparsable.
    Snapshot {
        seq: u64,
        timestamp_ms: i64,
        bids: Vec<L2BookLevel>,
        asks: Vec<L2BookLevel>,
    },
    /// incremental changes (`action:"update"`; `sz` 0 removes). `previous_sequence` chains to the previously
    /// applied `seqId` — a break signals a gap (the caller resyncs). `ts_ms` as on `Snapshot`.
    Update {
        seq: u64,
        previous_sequence: u64,
        timestamp_ms: i64,
        bids: Vec<L2BookLevel>,
        asks: Vec<L2BookLevel>,
    },
    Other,
}

/// Decode one OKX `books` channel frame. Uses `seqId` as the book sequence and `prevSeqId` for the
/// gap-chain check. Carries only f64 levels, from ONE parse of the frame. The frame's `checksum` is
/// not read: OKX fixed it to `0` on 2026-06-23 (deprecated), so there is nothing to validate and the
/// raw-string CRC32 mirror that used to do it is gone (`feed::depth::okx_depth_decode`).
pub fn parse_books_frame(text: &str) -> BooksFrame {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return BooksFrame::Other;
    };
    if v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str) != Some("books") {
        return BooksFrame::Other;
    }
    let action = v.get("action").and_then(Value::as_str);
    let Some(d) = v.get("data").and_then(Value::as_array).and_then(|a| a.first()) else {
        return BooksFrame::Other;
    };
    let seq = d.get("seqId").and_then(Value::as_u64).unwrap_or(0);
    let ts_ms =
        d.get("ts").and_then(Value::as_str).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let bids = parse_levels(d.get("bids"));
    let asks = parse_levels(d.get("asks"));
    match action {
        Some("snapshot") => BooksFrame::Snapshot { seq, timestamp_ms: ts_ms, bids, asks },
        Some("update") => {
            let previous_sequence =
                d.get("prevSeqId").and_then(Value::as_i64).unwrap_or(-1).max(0) as u64;
            BooksFrame::Update { seq, previous_sequence, timestamp_ms: ts_ms, bids, asks }
        }
        _ => BooksFrame::Other,
    }
}

/// The subscribe frame for the three channels on `instrument`.
pub fn subscribe_frame(instrument: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [
            {"channel": "bbo-tbt", "instId": instrument},
            {"channel": "trades", "instId": instrument},
            {"channel": "books5", "instId": instrument},
        ]
    })
    .to_string()
}

/// Spawn the OKX public market-data feed for `instrument` (e.g. "BTC-USDT-SWAP") into the tick/L2 lanes.
///
/// The thread is `vike_bridge_core::depth::spawn_tick`'s and its socket the shared depth
/// driver, on [`OKX`]'s `depth` timings row (the DOM lane's) and [`KEEPALIVE`], with ONE exception:
/// no timed re-seed. That row's re-seed interval exists for the DOM lane's `books` channel, whose
/// deltas can corrupt silently; this pump's `books5` is a full top-5 snapshot on every push, so a
/// re-seed has nothing to repair and would only cost a reconnect. The returned [`StopHandle`] is the
/// shared stop-and-join owner handle: `shutdown(self)` flags the thread and joins it, and a plain
/// drop does the same.
///
/// The socket is the public one of `urls` ([`feed_websocket`] with [`PATH_WEBSOCKET_PUBLIC`]), resolved once here.
/// `symbol` is a USER name (decision 0290: `BTCUSDT.P`): the pump subscribes its KEPT row's instId and labels every
/// tick with `symbol`.
///
/// # Errors
/// The lookup's refusal, or `symbol` names no kept row (`crate::instrument::inst_id_of`'s words): no thread is
/// spawned.
pub fn spawn_okx_feed_tick(
    urls: &VenueUrls,
    ticks: TickSender,
    symbol: &str,
    tick_size: f64,
) -> Result<StopHandle, String> {
    let url = feed_websocket(urls, "", PATH_WEBSOCKET_PUBLIC).map_err(|e| e.to_string())?;
    let instrument = crate::instrument::inst_id_of(symbol)?;
    let (subscribe, label) = (subscribe_frame(&instrument), symbol.to_string());
    Ok(spawn_tick(VENUE, symbol, ticks, move |pump| {
        let depth = OKX.depth();
        run_depth(
            &url,
            Some(&subscribe),
            Some(KEEPALIVE),
            &[],     // no timed re-seed (below), so nothing to re-subscribe
            || None, // WS-seeded: every `books5` push is the whole book
            |txt, book| pump_frame(txt, &label, book, tick_size, pump),
            |book| pump.book(book),
            |event| pump.link(event),
            pump.stop(),
            READ_POLL,
            depth.idle_threshold(),
            depth.freshness_threshold(),
            None, // `books5` is a full snapshot each push: nothing for a timed re-seed to repair
            &now_ms,
            depth.backoff(),
        );
    }))
}

/// What one frame does on the pump: a `bbo-tbt` quote and the frame's trades go straight onto the
/// tick lane, and a `books5` snapshot is the book the driver publishes (clocked on receive time).
/// The session's book starts empty on the catalog grid (`tick_size`) at its first frame, as the
/// hand-written session's did.
fn pump_frame(
    txt: &str,
    instrument: &str,
    book: &mut Option<Arc<L2Book>>,
    tick_size: f64,
    pump: &FeedTickSender,
) -> WebsocketSessionDepthOperation {
    let fold = book.get_or_insert_with(|| Arc::new(L2Book::new(tick_size)));
    match route_frame(txt, instrument, Arc::make_mut(fold)) {
        DataClientEvent::Quote(quote) => pump.quote(quote),
        DataClientEvent::Trades(trades) => trades.into_iter().for_each(|trade| pump.trade(trade)),
        DataClientEvent::BookUpdated { .. } => return WebsocketSessionDepthOperation::Updated(0),
        // `route_frame` answers none of these: an okx frame here holds no single print, bar or mark,
        // and `books5` is a whole snapshot (no sequence to break).
        DataClientEvent::Trade(_)
        | DataClientEvent::Bar(_)
        | DataClientEvent::InferredClose(_)
        | DataClientEvent::Mark(_)
        | DataClientEvent::Funding(_)
        | DataClientEvent::Book(_)
        | DataClientEvent::DepthSnapshot { .. }
        | DataClientEvent::Resync
        | DataClientEvent::Ignored => {}
    }
    WebsocketSessionDepthOperation::Ignored
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use vike_bridge_core::depth::send_quote;

    /// The pump's frame step: a `bbo-tbt` quote and a trade go straight onto the tick lane under the
    /// caller's label, a `books5` snapshot is the book the driver publishes (on the catalog grid the
    /// first frame opened it with), and an ack is nothing.
    #[test]
    fn the_pump_step_sends_quotes_and_trades_and_publishes_books5() {
        use std::sync::atomic::AtomicBool;
        let (events, mut rx) = vike_execution::event_channel(8);
        let ticks = TickSender { ingest: events.ingest.clone() };
        let pump =
            FeedTickSender::new(VENUE, "BTC-USDT-SWAP", ticks, Arc::new(AtomicBool::new(false)));
        let mut book = None;
        let mut step = |txt: &str| pump_frame(txt, "BTC-USDT-SWAP", &mut book, 0.1, &pump);
        let ack = r#"{"event":"subscribe","arg":{"channel":"books5","instId":"BTC-USDT-SWAP"}}"#;
        assert_eq!(step(ack), WebsocketSessionDepthOperation::Ignored);
        let bbo = frame(
            "bbo-tbt",
            serde_json::json!({"bids":[["60000.1","1.5","0","2"]],"asks":[["60000.2","2.0","0","3"]],"ts":"111"}),
        );
        assert_eq!(step(&bbo), WebsocketSessionDepthOperation::Ignored);
        let trade =
            frame("trades", serde_json::json!({"px":"60000","sz":"0.3","side":"sell","ts":"9"}));
        assert_eq!(step(&trade), WebsocketSessionDepthOperation::Ignored);
        let books5 = frame(
            "books5",
            serde_json::json!({"bids":[["60000.0","5","0","1"]],"asks":[["60001.0","4","0","1"]],"ts":"222"}),
        );
        assert_eq!(step(&books5), WebsocketSessionDepthOperation::Updated(0));
        let b = book.as_ref().expect("the first frame opened the session's book");
        assert_eq!((b.tick_size, b.best_bid()), (0.1, Some(L2BookLevel::new(60000.0, 5.0))));
        let Ok(vike_execution::Ingest::Quote(q)) = rx.try_recv() else { panic!("the quote") };
        assert_eq!((q.symbol.as_str(), q.quote.timestamp), ("BTC-USDT-SWAP", 111));
        let Ok(vike_execution::Ingest::Trade(t)) = rx.try_recv() else { panic!("the trade") };
        assert_eq!((t.symbol.as_str(), t.trade.price), ("BTC-USDT-SWAP", 60000.0));
        assert!(
            rx.try_recv().is_err(),
            "the step publishes no book itself: the driver's on_book does"
        );
    }

    fn frame(channel: &str, data: serde_json::Value) -> String {
        serde_json::json!({"arg": {"channel": channel, "instId": "BTC-USDT-SWAP"}, "data": [data]})
            .to_string()
    }

    #[test]
    fn bbo_decodes_to_quote() {
        let f = frame(
            "bbo-tbt",
            serde_json::json!({"bids":[["60000.1","1.5","0","2"]],"asks":[["60000.2","2.0","0","3"]],"ts":"111"}),
        );
        let mut book = L2Book::new(0.1);
        match route_frame(&f, "BTC-USDT-SWAP", &mut book) {
            DataClientEvent::Quote(q) => {
                assert_eq!(q.bid, 60000.1);
                assert_eq!(q.ask, 60000.2);
                assert_eq!(q.bid_size, 1.5);
                assert_eq!(q.timestamp, 111);
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    #[test]
    fn trade_decodes_taker_side() {
        let sell =
            frame("trades", serde_json::json!({"px":"60000","sz":"0.3","side":"sell","ts":"9"}));
        let mut book = L2Book::new(0.1);
        match route_frame(&sell, "BTC-USDT-SWAP", &mut book) {
            DataClientEvent::Trades(trades) => {
                assert_eq!(trades.len(), 1);
                let t = &trades[0];
                assert_eq!(t.price, 60000.0);
                assert_eq!(t.size, 0.3);
                assert!(t.is_buyer_maker, "taker sell → buyer was the maker");
            }
            other => panic!("expected Trades, got {other:?}"),
        }
    }

    /// A `trades` frame whose `data` holds `rows`, verbatim.
    fn trades_frame(rows: serde_json::Value) -> String {
        serde_json::json!({"arg": {"channel": "trades", "instId": "BTC-USDT-SWAP"}, "data": rows})
            .to_string()
    }

    #[test]
    fn a_three_row_trades_frame_yields_three_ticks_in_order() {
        let f = trades_frame(serde_json::json!([
            {"px":"60000","sz":"0.1","side":"buy","ts":"1"},
            {"px":"60001","sz":"0.2","side":"sell","ts":"2"},
            {"px":"60002","sz":"0.3","side":"buy","ts":"3"},
        ]));
        let mut book = L2Book::new(0.1);
        match route_frame(&f, "BTC-USDT-SWAP", &mut book) {
            DataClientEvent::Trades(trades) => {
                let seen: Vec<(i64, f64, f64, bool)> = trades
                    .iter()
                    .map(|t| (t.timestamp, t.price, t.size, t.is_buyer_maker))
                    .collect();
                assert_eq!(
                    seen,
                    vec![
                        (1, 60000.0, 0.1, false),
                        (2, 60001.0, 0.2, true),
                        (3, 60002.0, 0.3, false)
                    ]
                );
            }
            other => panic!("expected Trades, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_trades_array_yields_no_event() {
        let mut book = L2Book::new(0.1);
        assert_eq!(
            route_frame(&trades_frame(serde_json::json!([])), "BTC-USDT-SWAP", &mut book),
            DataClientEvent::Ignored
        );
    }

    #[test]
    fn one_unreadable_trade_row_keeps_the_other_two() {
        let f = trades_frame(serde_json::json!([
            {"px":"60000","sz":"0.1","side":"buy","ts":"1"},
            {"px":"not-a-number","sz":"0.2","side":"sell","ts":"2"},
            {"px":"60002","sz":"0.3","side":"buy","ts":"3"},
        ]));
        let mut book = L2Book::new(0.1);
        match route_frame(&f, "BTC-USDT-SWAP", &mut book) {
            DataClientEvent::Trades(trades) => {
                let seen: Vec<(i64, f64)> = trades.iter().map(|t| (t.timestamp, t.price)).collect();
                assert_eq!(seen, vec![(1, 60000.0), (3, 60002.0)]);
            }
            other => panic!("expected Trades, got {other:?}"),
        }
    }

    /// The pump refuses the prints the recorded tape refuses ([`is_valid_trade`]): a zero price, a
    /// negative size and a `NaN` price each drop their row, and the readable row stays.
    #[test]
    fn a_print_the_trade_guard_refuses_is_dropped() {
        let f = trades_frame(serde_json::json!([
            {"px":"0","sz":"0.1","side":"buy","ts":"1"},
            {"px":"60001","sz":"-0.2","side":"sell","ts":"2"},
            {"px":"NaN","sz":"0.3","side":"buy","ts":"3"},
            {"px":"60002","sz":"0.3","side":"buy","ts":"4"},
        ]));
        match route_frame(&f, "BTC-USDT-SWAP", &mut L2Book::new(0.1)) {
            DataClientEvent::Trades(trades) => {
                assert_eq!(trades.iter().map(|t| t.timestamp).collect::<Vec<_>>(), [4]);
            }
            other => panic!("expected Trades, got {other:?}"),
        }
    }

    #[test]
    fn books5_rebuilds_the_top() {
        let f = frame(
            "books5",
            serde_json::json!({"bids":[["60000.0","5","0","1"],["59999.0","3","0","1"]],
                               "asks":[["60001.0","4","0","1"]],"ts":"222"}),
        );
        let mut book = L2Book::new(0.1);
        assert_eq!(
            route_frame(&f, "BTC-USDT-SWAP", &mut book),
            DataClientEvent::BookUpdated { timestamp_ms: 0 }
        );
        assert_eq!(book.best_bid(), Some(L2BookLevel::new(60000.0, 5.0)));
        assert_eq!(book.best_ask(), Some(L2BookLevel::new(60001.0, 4.0)));
        assert_eq!(book.bid_levels(), 2);
        // a fresh snapshot REPLACES (books5 is full state each push)
        let f2 = frame(
            "books5",
            serde_json::json!({"bids":[["60000.5","1","0","1"]],"asks":[],"ts":"223"}),
        );
        route_frame(&f2, "BTC-USDT-SWAP", &mut book);
        assert_eq!(book.best_bid(), Some(L2BookLevel::new(60000.5, 1.0)));
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn subscribe_ack_and_junk_ignored() {
        let mut book = L2Book::new(0.1);
        assert_eq!(
            route_frame(r#"{"event":"subscribe","arg":{"channel":"trades"}}"#, "X", &mut book),
            DataClientEvent::Ignored
        );
        assert_eq!(route_frame("pong", "X", &mut book), DataClientEvent::Ignored);
    }

    #[test]
    fn books_deep_snapshot_then_update() {
        let snap = frame_books(
            "snapshot",
            serde_json::json!({"bids":[["60000.0","5","0","1"],["59999.0","2","0","1"]],
                               "asks":[["60001.0","4","0","1"]],"ts":"1","seqId":10,"prevSeqId":-1}),
        );
        match parse_books_frame(&snap) {
            BooksFrame::Snapshot { seq, bids, asks, .. } => {
                assert_eq!(seq, 10);
                assert_eq!(bids.len(), 2);
                assert_eq!(asks[0], L2BookLevel::new(60001.0, 4.0));
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
        // update chains prevSeqId 10 → seqId 11: bump a bid, remove the ask (sz 0)
        let upd = frame_books(
            "update",
            serde_json::json!({"bids":[["60000.0","8","0","1"]],"asks":[["60001.0","0","0","0"]],
                               "ts":"2","seqId":11,"prevSeqId":10}),
        );
        match parse_books_frame(&upd) {
            BooksFrame::Update { seq, previous_sequence, bids, asks, .. } => {
                assert_eq!((seq, previous_sequence), (11, 10));
                assert_eq!(bids[0], L2BookLevel::new(60000.0, 8.0));
                assert_eq!(asks[0], L2BookLevel::new(60001.0, 0.0)); // sz 0 → the folder removes it
            }
            other => panic!("expected Update, got {other:?}"),
        }
        // an ack (no `action`) and the shallow books5 channel are both `Other`
        assert_eq!(
            parse_books_frame(r#"{"arg":{"channel":"books","instId":"X"},"event":"subscribe"}"#),
            BooksFrame::Other
        );
        assert_eq!(
            parse_books_frame(r#"{"arg":{"channel":"books5"},"data":[]}"#),
            BooksFrame::Other
        );
    }

    fn frame_books(action: &str, data: serde_json::Value) -> String {
        serde_json::json!({"arg":{"channel":"books","instId":"BTC-USDT"},"action":action,"data":[data]})
            .to_string()
    }

    /// The tier-less feeds dial the seed's LIVE public and business sockets, byte-for-byte the URLs the
    /// code held before the addresses became rows, and a box with no okx row spawns no pump at all.
    #[test]
    fn the_feed_sockets_are_the_seeds_live_rows_and_no_row_spawns_nothing() {
        let seed = VenueUrls::seed("okx");
        let public = feed_websocket(&seed, "", PATH_WEBSOCKET_PUBLIC);
        assert_eq!(public.as_deref(), Ok("wss://ws.okx.com:8443/ws/v5/public"));
        let business = feed_websocket(&seed, PRODUCT_BUSINESS, PATH_WEBSOCKET_BUSINESS);
        assert_eq!(business.as_deref(), Ok("wss://ws.okx.com/ws/v5/business"));
        let (events, _rx) = vike_execution::event_channel(4);
        let ticks = TickSender { ingest: events.ingest.clone() };
        let refused = spawn_okx_feed_tick(&VenueUrls::default(), ticks, "BTC-USDT-SWAP", 0.1);
        assert!(refused.err().expect("no row, no pump").to_string().contains("no address row"));
    }

    /// The stop-flag poll of the socket reads in this file and in `market_feed`'s depth lane is the
    /// 2 s each held as a private `READ_TIMEOUT` before `vike_bridge_core::websocket_session::spec::READ_POLL`
    /// (the venue-free mechanism default, decision 0160) replaced the copies.
    #[test]
    fn the_sockets_poll_their_stop_flag_every_two_seconds() {
        assert_eq!(vike_bridge_core::websocket_session::spec::READ_POLL, Duration::from_secs(2));
    }

    /// A `bbo-tbt` quote leaves the pump stamped with the machine receive time (`send_quote`); the pure
    /// decoder ([`decode_bbo`]) stamps nothing and keeps the venue's own `ts`.
    #[test]
    fn a_quote_leaves_the_pump_stamped_with_the_machine_receive_time() {
        let (events, mut rx) = vike_execution::event_channel(4);
        let ticks = TickSender { ingest: events.ingest.clone() };
        let datum = serde_json::json!({
            "bids": [["60000.1", "1.5", "0", "2"]], "asks": [["60000.2", "2.0", "0", "3"]], "ts": "111"
        });
        let quote = decode_bbo(&datum, "BTC-USDT-SWAP").expect("a bbo datum decodes");
        assert_eq!(
            (quote.timestamp, quote.local_timestamp),
            (111, 0),
            "the decoder keeps the venue ts, stamps nothing"
        );

        let before = vike_model::now_ms();
        send_quote(&ticks, VENUE, "BTC-USDT-SWAP", quote).expect("the channel is open");
        let after = vike_model::now_ms();

        let Ok(vike_execution::Ingest::Quote(update)) = rx.try_recv() else {
            panic!("a QuoteUpdate must be on the lane")
        };
        assert_eq!((update.venue.as_str(), update.symbol.as_str()), ("okx", "BTC-USDT-SWAP"));
        assert_eq!(update.quote.timestamp, 111, "the venue's own time is untouched");
        assert!(
            (before..=after).contains(&update.quote.local_timestamp),
            "local_ts {} must be the receive time, between {before} and {after}",
            update.quote.local_timestamp
        );
    }
}
