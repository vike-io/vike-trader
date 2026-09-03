//! Live OKX-V5 public MARKET data → the vike-core tick/L2 lanes (R8 HFT track). Mirrors the
//! Binance market-data module's shape (pure `route_frame` decoders + a stoppable WS pump), but for
//! OKX's public channels: `bbo-tbt` → `QuoteTick`, `trades` → `TradeTick`, `books5` (a FULL top-5
//! snapshot each push, so no delta-sync) → `L2Book`. Message formats per the OKX V5 WS docs; the
//! live smoke verifies them against the real stream.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;
use tungstenite::Message;

use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_bridge_core::ws::{configure_ws_stream, is_timeout};
use vike_exec::{BookUpdate, QuoteUpdate, TickSender, TradeUpdate};
use vike_model::{L2Book, Level, QuoteTick, TradeTick};

const VENUE: &str = "okx";
const READ_TIMEOUT: Duration = Duration::from_secs(2);
pub const PUBLIC_WS: &str = "wss://ws.okx.com:8443/ws/v5/public";

fn f(v: Option<&Value>) -> Option<f64> {
    v.and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
}

/// First level `[px, sz, …]` of an OKX bids/asks array → (price, size).
fn top(side: Option<&Value>) -> Option<(f64, f64)> {
    let lvl = side?.as_array()?.first()?.as_array()?;
    Some((f(lvl.first())?, f(lvl.get(1))?))
}

/// All levels of an OKX bids/asks array → `Level`s.
fn levels(side: Option<&Value>) -> Vec<Level> {
    side.and_then(Value::as_array)
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

/// `bbo-tbt` datum (`{bids:[[px,sz,..]],asks:[[px,sz,..]],ts}`) → QuoteTick.
pub fn decode_bbo(d: &Value, symbol: &str) -> Option<QuoteTick> {
    let (bid, bid_size) = top(d.get("bids"))?;
    let (ask, ask_size) = top(d.get("asks"))?;
    Some(QuoteTick {
        ts: d.get("ts").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or(0),
        // TODO(book-recording plan): stamp at WS receive
        local_ts: 0,
        bid,
        ask,
        bid_size,
        ask_size,
        symbol: symbol.to_string(),
    })
}

/// `trades` datum (`{px,sz,side,ts}`) → TradeTick. OKX `side` is the taker side, so a taker "sell"
/// means the buyer was the maker.
pub fn decode_trade(d: &Value, symbol: &str) -> Option<TradeTick> {
    Some(TradeTick {
        ts: d.get("ts").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or(0),
        // TODO(book-recording plan): stamp at WS receive
        local_ts: 0,
        price: f(d.get("px"))?,
        size: f(d.get("sz"))?,
        is_buyer_maker: d.get("side").and_then(Value::as_str) == Some("sell"),
        symbol: symbol.to_string(),
    })
}

/// What one decoded OKX frame becomes (mirrors the Binance module).
#[derive(Debug, Clone, PartialEq)]
pub enum MdEvent {
    Quote(QuoteTick),
    Trade(TradeTick),
    BookUpdated,
    Ignored,
}

/// Route ONE OKX WS text frame to a lane, rebuilding `book` from a `books5` snapshot. Pure over
/// `book` — testable without a socket. Non-data frames (subscribe acks, `pong`) → Ignored.
pub fn route_frame(text: &str, symbol: &str, book: &mut L2Book) -> MdEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return MdEvent::Ignored;
    };
    let channel = v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str);
    let first = v.get("data").and_then(Value::as_array).and_then(|d| d.first());
    match (channel, first) {
        (Some("bbo-tbt"), Some(d)) => {
            decode_bbo(d, symbol).map_or(MdEvent::Ignored, MdEvent::Quote)
        }
        (Some("trades"), Some(d)) => {
            decode_trade(d, symbol).map_or(MdEvent::Ignored, MdEvent::Trade)
        }
        (Some("books5"), Some(d)) => {
            // full top-5 snapshot each push — clear + rebuild (ts as the monotonic seq)
            let seq = d.get("ts").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or(0);
            book.apply_snapshot(seq, &levels(d.get("bids")), &levels(d.get("asks")));
            MdEvent::BookUpdated
        }
        _ => MdEvent::Ignored,
    }
}

/// A decoded OKX `books` (400-level, snapshot+update) frame — the DEEP book for the DOM, distinct
/// from the shallow `books5` handled by [`route_frame`]. `Other` = a non-`books` frame / ack / pong.
#[derive(Debug, Clone, PartialEq)]
pub enum BooksFrame {
    /// full 400-level state (`action:"snapshot"`) — clear + rebuild
    Snapshot {
        seq: u64,
        bids: Vec<Level>,
        asks: Vec<Level>,
    },
    /// incremental changes (`action:"update"`; `sz` 0 removes). `prev_seq` chains to the previously
    /// applied `seqId` — a break signals a gap (the caller resyncs).
    Update {
        seq: u64,
        prev_seq: u64,
        bids: Vec<Level>,
        asks: Vec<Level>,
    },
    Other,
}

/// Decode one OKX `books` channel frame. Uses `seqId` as the book sequence and `prevSeqId` for the
/// gap-chain check. Carries only f64 levels; the CRC32 `checksum` is validated on the side by
/// [`parse_raw_books`] + [`crate::book_checksum`] (which need OKX's RAW price/size strings, unlike the
/// f64 [`vike_model::L2Book`]) — wired into `market_feed::okx_depth_decode`.
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
    let bids = levels(d.get("bids"));
    let asks = levels(d.get("asks"));
    match action {
        Some("snapshot") => BooksFrame::Snapshot { seq, bids, asks },
        Some("update") => {
            let prev_seq = d.get("prevSeqId").and_then(Value::as_i64).unwrap_or(-1).max(0) as u64;
            BooksFrame::Update { seq, prev_seq, bids, asks }
        }
        _ => BooksFrame::Other,
    }
}

/// The RAW (unparsed) OKX `books` levels + the frame `checksum` — re-parsed straight from the wire
/// text so the CRC32 can be computed over the EXACT `price`/`size` strings OKX sent. The folded
/// [`vike_model::L2Book`] and the f64 [`BooksFrame`] cannot reproduce those strings (see
/// [`crate::book_checksum`]). Mirrors `market_feed`'s `frame_ts_ms` targeted re-parse so
/// [`BooksFrame`]/[`parse_books_frame`] and their fixtures stay untouched. `bids`/`asks` are in OKX's
/// wire order (bids high→low, asks low→high) as `(price, size)`.
#[derive(Debug, Clone, PartialEq)]
pub struct RawBooks {
    pub bids: Vec<(String, String)>,
    pub asks: Vec<(String, String)>,
    /// OKX's `checksum` as the signed i32 it transmits. `None` if the field is absent. NOTE: OKX
    /// FIXED this to `0` on 2026-06-23 (deprecated) — the checksum validator treats `0` as
    /// "not provided" and skips it (see [`crate::book_checksum`]).
    pub checksum: Option<i32>,
}

/// Re-parse a `books` frame's raw levels + checksum for CRC32 validation. `None` for a non-`books`
/// frame / ack / pong (nothing to validate). Reads element `[0]`/`[1]` of each `[px, sz, …]` level
/// verbatim; a level missing a string px/sz is skipped.
pub fn parse_raw_books(text: &str) -> Option<RawBooks> {
    let v = serde_json::from_str::<Value>(text).ok()?;
    if v.get("arg").and_then(|a| a.get("channel")).and_then(Value::as_str) != Some("books") {
        return None;
    }
    let d = v.get("data").and_then(Value::as_array).and_then(|a| a.first())?;
    let raw_side = |key: &str| -> Vec<(String, String)> {
        d.get(key)
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|lvl| {
                        let l = lvl.as_array()?;
                        Some((l.first()?.as_str()?.to_string(), l.get(1)?.as_str()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let checksum = d.get("checksum").and_then(Value::as_i64).map(|c| c as i32);
    Some(RawBooks { bids: raw_side("bids"), asks: raw_side("asks"), checksum })
}

/// The subscribe frame for the three channels on `inst`.
pub fn subscribe_frame(inst: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [
            {"channel": "bbo-tbt", "instId": inst},
            {"channel": "trades", "instId": inst},
            {"channel": "books5", "instId": inst},
        ]
    })
    .to_string()
}

/// Handle for the OKX market-data thread; `shutdown` flags + joins it.
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

/// Spawn the OKX public market-data feed for `inst` (e.g. "BTC-USDT-SWAP") into the tick/L2 lanes.
pub fn spawn_okx_market_data(ticks: TickSender, inst: &str, tick_size: f64) -> MarketDataFeed {
    let stop = Arc::new(AtomicBool::new(false));
    let st = Arc::clone(&stop);
    let inst = inst.to_string();
    let join = std::thread::Builder::new()
        .name(format!("md-okx-{inst}"))
        .spawn(move || {
            vike_exec::affinity::pin_current_thread(vike_exec::affinity::Role::MarketData, "okx");
            while !st.load(Ordering::Relaxed) {
                if run_session(&inst, tick_size, &ticks, &st).is_ok() {
                    return;
                }
                // 1s stop-aware reconnect nap (the shared bridge-core helper — dedup F16); the
                // `while` above re-reads `stop`, so a stop during the nap exits the thread.
                sleep_unless_stopped(&st, Duration::from_secs(1));
            }
        })
        .expect("spawn okx market-data thread");
    MarketDataFeed { stop, join: Some(join) }
}

fn run_session(
    inst: &str,
    tick_size: f64,
    ticks: &TickSender,
    stop: &Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut socket, _resp) = tungstenite::connect(PUBLIC_WS)?;
    configure_ws_stream(&socket, READ_TIMEOUT);
    socket.send(Message::Text(subscribe_frame(inst).into()))?;
    // ONE standing book behind an `Arc`: a `BookUpdated` emission is a refcount bump, not a deep
    // clone of both `BTreeMap`s, and folds go through `Arc::make_mut` (copy-on-write, and any copy
    // lands on THIS thread, never on the single-writer core). See `vike_exec::BookUpdate`.
    let mut book = Arc::new(L2Book::new(tick_size));
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) if is_timeout(&e) => continue,
            Err(e) => return Err(e.into()),
        };
        match msg {
            Message::Text(txt) => {
                // Bound OUT of the `match` scrutinee on purpose: `Arc::make_mut`'s `&mut` borrow of
                // `book` must end before the `BookUpdated` arm reads `book` again.
                let ev = route_frame(txt.as_str(), inst, Arc::make_mut(&mut book));
                let push = match ev {
                    MdEvent::Quote(quote) => {
                        ticks.quote(QuoteUpdate { venue: VENUE.into(), symbol: inst.into(), quote })
                    }
                    MdEvent::Trade(trade) => {
                        ticks.trade(TradeUpdate { venue: VENUE.into(), symbol: inst.into(), trade })
                    }
                    MdEvent::BookUpdated => ticks.book(BookUpdate {
                        venue: VENUE.into(),
                        symbol: inst.into(),
                        book: Arc::clone(&book),
                    }),
                    MdEvent::Ignored => Ok(()),
                };
                if push.is_err() {
                    return Ok(()); // core gone
                }
            }
            Message::Ping(p) => socket.send(Message::Pong(p))?,
            // OKX sends a text "pong" (handled as Ignored above); handle protocol close
            Message::Close(_) => return Err("server closed".into()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            MdEvent::Quote(q) => {
                assert_eq!(q.bid, 60000.1);
                assert_eq!(q.ask, 60000.2);
                assert_eq!(q.bid_size, 1.5);
                assert_eq!(q.ts, 111);
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
            MdEvent::Trade(t) => {
                assert_eq!(t.price, 60000.0);
                assert_eq!(t.size, 0.3);
                assert!(t.is_buyer_maker, "taker sell → buyer was the maker");
            }
            other => panic!("expected Trade, got {other:?}"),
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
        assert_eq!(route_frame(&f, "BTC-USDT-SWAP", &mut book), MdEvent::BookUpdated);
        assert_eq!(book.best_bid(), Some((60000.0, 5.0)));
        assert_eq!(book.best_ask(), Some((60001.0, 4.0)));
        assert_eq!(book.bid_levels(), 2);
        // a fresh snapshot REPLACES (books5 is full state each push)
        let f2 = frame(
            "books5",
            serde_json::json!({"bids":[["60000.5","1","0","1"]],"asks":[],"ts":"223"}),
        );
        route_frame(&f2, "BTC-USDT-SWAP", &mut book);
        assert_eq!(book.best_bid(), Some((60000.5, 1.0)));
        assert_eq!(book.best_ask(), None);
    }

    #[test]
    fn subscribe_ack_and_junk_ignored() {
        let mut book = L2Book::new(0.1);
        assert_eq!(
            route_frame(r#"{"event":"subscribe","arg":{"channel":"trades"}}"#, "X", &mut book),
            MdEvent::Ignored
        );
        assert_eq!(route_frame("pong", "X", &mut book), MdEvent::Ignored);
    }

    #[test]
    fn books_deep_snapshot_then_update() {
        let snap = frame_books(
            "snapshot",
            serde_json::json!({"bids":[["60000.0","5","0","1"],["59999.0","2","0","1"]],
                               "asks":[["60001.0","4","0","1"]],"ts":"1","seqId":10,"prevSeqId":-1}),
        );
        match parse_books_frame(&snap) {
            BooksFrame::Snapshot { seq, bids, asks } => {
                assert_eq!(seq, 10);
                assert_eq!(bids.len(), 2);
                assert_eq!(asks[0], (60001.0, 4.0));
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
            BooksFrame::Update { seq, prev_seq, bids, asks } => {
                assert_eq!((seq, prev_seq), (11, 10));
                assert_eq!(bids[0], (60000.0, 8.0));
                assert_eq!(asks[0], (60001.0, 0.0)); // sz 0 → the folder removes it
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
}
