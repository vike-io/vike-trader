//! Live Bybit-V5 public MARKET data → the vike-core tick/L2 lanes (R8 HFT track). Same shape as the
//! Binance/OKX market-data modules. Bybit has no standalone book-ticker, so the QUOTE is derived
//! from the L2 top: `orderbook.50` (snapshot then deltas) → `L2Book` → both `on_order_book` and a
//! top-of-book `QuoteTick`; `publicTrade` → `TradeTick`. Formats per the Bybit V5 WS docs; the live
//! smoke verifies them against the real stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;
use tungstenite::Message;

use vike_bridge_core::user_data::sleep_unless_stopped;
use vike_bridge_core::ws::{configure_ws_stream, is_timeout};
use vike_exec::{BookUpdate, QuoteUpdate, TickSender, TradeUpdate};
use vike_model::{DeltaDecision, L2Book, Level, QuoteTick, SeqPolicy, TradeTick};

const VENUE: &str = "bybit";
const READ_TIMEOUT: Duration = Duration::from_secs(2);
pub const PUBLIC_WS_LINEAR: &str = "wss://stream.bybit.com/v5/public/linear";

fn f(v: Option<&Value>) -> Option<f64> {
    v.and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
}

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

/// `publicTrade` datum (`{T,p,v,S}`) → TradeTick. `S` is the taker side, so a taker "Sell" means
/// the buyer was the maker.
pub fn decode_trade(d: &Value, symbol: &str) -> Option<TradeTick> {
    Some(TradeTick {
        ts: d.get("T").and_then(Value::as_i64).unwrap_or(0),
        // TODO(book-recording plan): stamp at WS receive
        local_ts: 0,
        price: f(d.get("p"))?,
        size: f(d.get("v"))?,
        is_buyer_maker: d.get("S").and_then(Value::as_str) == Some("Sell"),
        symbol: symbol.to_string(),
    })
}

/// Best bid/ask of the current book → a QuoteTick (Bybit has no book-ticker channel). None until
/// the book has a two-sided top.
pub fn quote_from_book(book: &L2Book, symbol: &str) -> Option<QuoteTick> {
    let (bid, bid_size) = book.best_bid()?;
    let (ask, ask_size) = book.best_ask()?;
    Some(QuoteTick {
        ts: 0,
        // TODO(book-recording plan): stamp at WS receive
        local_ts: 0,
        bid,
        ask,
        bid_size,
        ask_size,
        symbol: symbol.to_string(),
    })
}

/// What one decoded Bybit frame becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum MdEvent {
    Trade(TradeTick),
    /// the L2Book changed — the pump publishes the book AND a top-of-book quote
    BookUpdated,
    /// depth seq gap — frames were dropped (forward `u` jump) or the venue restarted its
    /// counter (`u` regression); the book is untrustworthy and the pump must re-seed a fresh
    /// snapshot (the delta was NOT folded)
    Resync,
    Ignored,
}

/// Route ONE Bybit WS text frame to a lane, folding `orderbook` snapshot/delta into `book`. Pure
/// over `book` — testable without a socket.
///
/// Bybit `orderbook.*` deltas increment `u` by exactly 1, so the delta arm consults the shared
/// [`vike_model::L2Book::delta_decision`] law under [`SeqPolicy::Strict`] (the SAME decision
/// replay runs over recorded books): in-sequence folds, a duplicate (`u == last_seq`) is
/// [`MdEvent::Ignored`], and anything else — a dropped frame OR a venue-restart `u` regression
/// (Bybit signals restarts by resetting `u`, e.g. a `u=1` delta) — is [`MdEvent::Resync`]
/// instead of silently corrupting the book until the timed reseed.
pub fn route_frame(text: &str, symbol: &str, book: &mut L2Book) -> MdEvent {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return MdEvent::Ignored;
    };
    let Some(topic) = v.get("topic").and_then(Value::as_str) else {
        return MdEvent::Ignored; // subscribe acks carry no topic
    };
    if topic.starts_with("orderbook.") {
        let msg_type = v.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(data) = v.get("data") else {
            return MdEvent::Ignored;
        };
        let u = data.get("u").and_then(Value::as_u64).unwrap_or(0);
        let (b, a) = (levels(data.get("b")), levels(data.get("a")));
        match msg_type {
            "snapshot" => {
                book.apply_snapshot(u, &b, &a);
                MdEvent::BookUpdated
            }
            "delta" => match book.delta_decision(u, SeqPolicy::Strict) {
                DeltaDecision::Apply => {
                    book.apply_delta(u, &b, &a);
                    MdEvent::BookUpdated
                }
                DeltaDecision::Stale => MdEvent::Ignored, // duplicate — book stays trustworthy
                DeltaDecision::Gap => MdEvent::Resync,    // dropped frames / venue restart
            },
            _ => MdEvent::Ignored, // unknown type
        }
    } else if topic.starts_with("publicTrade") {
        v.get("data")
            .and_then(Value::as_array)
            .and_then(|d| d.first())
            .and_then(|d| decode_trade(d, symbol))
            .map_or(MdEvent::Ignored, MdEvent::Trade)
    } else {
        MdEvent::Ignored
    }
}

/// Parse a Bybit `orderbook` SNAPSHOT frame's raw levels + sequence. `None` if the frame isn't an
/// orderbook snapshot (a delta, ack, or trade). The DOM depth feed uses this to infer the tick +
/// seed the book before handing subsequent frames to [`route_frame`] (which needs a pre-built book).
pub fn parse_orderbook_snapshot(text: &str) -> Option<(u64, Vec<Level>, Vec<Level>)> {
    let v: Value = serde_json::from_str(text).ok()?;
    if !v.get("topic").and_then(Value::as_str)?.starts_with("orderbook.") {
        return None;
    }
    if v.get("type").and_then(Value::as_str) != Some("snapshot") {
        return None;
    }
    let data = v.get("data")?;
    let u = data.get("u").and_then(Value::as_u64).unwrap_or(0);
    Some((u, levels(data.get("b")), levels(data.get("a"))))
}

/// The subscribe frame: L2-50 book + public trades for `symbol`.
pub fn subscribe_frame(symbol: &str) -> String {
    serde_json::json!({
        "op": "subscribe",
        "args": [format!("orderbook.50.{symbol}"), format!("publicTrade.{symbol}")]
    })
    .to_string()
}

/// Handle for the Bybit market-data thread; `shutdown` flags + joins it.
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

/// Spawn the Bybit public market-data feed for `symbol` (e.g. "BTCUSDT") into the tick/L2 lanes.
pub fn spawn_bybit_market_data(ticks: TickSender, symbol: &str, tick_size: f64) -> MarketDataFeed {
    let stop = Arc::new(AtomicBool::new(false));
    let st = Arc::clone(&stop);
    let sym = symbol.to_string();
    let join = std::thread::Builder::new()
        .name(format!("md-bybit-{sym}"))
        .spawn(move || {
            vike_exec::affinity::pin_current_thread(vike_exec::affinity::Role::MarketData, "bybit");
            while !st.load(Ordering::Relaxed) {
                if run_session(&sym, tick_size, &ticks, &st).is_ok() {
                    return;
                }
                // 1s stop-aware reconnect nap (the shared bridge-core helper — dedup F16); the
                // `while` above re-reads `stop`, so a stop during the nap exits the thread.
                sleep_unless_stopped(&st, Duration::from_secs(1));
            }
        })
        .expect("spawn bybit market-data thread");
    MarketDataFeed { stop, join: Some(join) }
}

fn run_session(
    symbol: &str,
    tick_size: f64,
    ticks: &TickSender,
    stop: &Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut socket, _resp) = tungstenite::connect(PUBLIC_WS_LINEAR)?;
    configure_ws_stream(&socket, READ_TIMEOUT);
    socket.send(Message::Text(subscribe_frame(symbol).into()))?;
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
                // `book` must end before the arms below read `book` again.
                let ev = route_frame(txt.as_str(), symbol, Arc::make_mut(&mut book));
                match ev {
                    MdEvent::Trade(trade) => {
                        if ticks
                            .trade(TradeUpdate {
                                venue: VENUE.into(),
                                symbol: symbol.into(),
                                trade,
                            })
                            .is_err()
                        {
                            return Ok(());
                        }
                    }
                    MdEvent::BookUpdated => {
                        // publish the book, then a derived top-of-book quote
                        if ticks
                            .book(BookUpdate {
                                venue: VENUE.into(),
                                symbol: symbol.into(),
                                book: Arc::clone(&book),
                            })
                            .is_err()
                        {
                            return Ok(());
                        }
                        if let Some(quote) = quote_from_book(&book, symbol)
                            && ticks
                                .quote(QuoteUpdate {
                                    venue: VENUE.into(),
                                    symbol: symbol.into(),
                                    quote,
                                })
                                .is_err()
                        {
                            return Ok(());
                        }
                    }
                    // Depth seq gap (dropped frame / venue counter restart): the book is
                    // untrustworthy and Bybit has no REST re-seed on this pump — end the
                    // session so the spawn loop's reconnect re-subscribes, and the fresh
                    // session's WS `snapshot` rebuilds the book (the same shape a
                    // binance-family Resync takes, minus the REST fetch that venue has).
                    MdEvent::Resync => return Err("bybit depth seq gap — resync".into()),
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
mod tests {
    use super::*;

    #[test]
    fn public_trade_decodes_taker_side() {
        let frame = serde_json::json!({
            "topic": "publicTrade.BTCUSDT",
            "data": [{"T": 1700, "s": "BTCUSDT", "S": "Sell", "v": "0.4", "p": "60000.7"}]
        })
        .to_string();
        let mut book = L2Book::new(0.1);
        match route_frame(&frame, "BTCUSDT", &mut book) {
            MdEvent::Trade(t) => {
                assert_eq!(t.ts, 1700);
                assert_eq!(t.price, 60000.7);
                assert_eq!(t.size, 0.4);
                assert!(t.is_buyer_maker, "taker Sell → buyer was the maker");
            }
            other => panic!("expected Trade, got {other:?}"),
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
        assert_eq!(route_frame(&snap, "BTCUSDT", &mut book), MdEvent::BookUpdated);
        assert_eq!(book.best_bid(), Some((60000.0, 5.0)));
        assert_eq!(book.best_ask(), Some((60001.0, 4.0)));

        // delta: update best bid qty, remove the ask (size 0)
        let delta = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "delta",
            "data": {"s":"BTCUSDT","b":[["60000.0","8"]],"a":[["60001.0","0"]],"u":11,"seq":2}
        })
        .to_string();
        assert_eq!(route_frame(&delta, "BTCUSDT", &mut book), MdEvent::BookUpdated);
        assert_eq!(book.best_bid(), Some((60000.0, 8.0)));
        assert_eq!(book.best_ask(), None);

        // a top-of-book quote can be derived once both sides exist again
        assert!(quote_from_book(&book, "BTCUSDT").is_none(), "one-sided → no quote");

        // stale delta (u <= last_seq) is dropped
        let stale = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "delta",
            "data": {"s":"BTCUSDT","b":[["60000.0","99"]],"a":[],"u":11,"seq":3}
        })
        .to_string();
        assert_eq!(route_frame(&stale, "BTCUSDT", &mut book), MdEvent::Ignored);
        assert_eq!(book.best_bid(), Some((60000.0, 8.0)), "stale must not apply");
    }

    /// A dropped frame (snapshot u=10, next delta u=12 — Bybit deltas increment `u` by 1) is a
    /// detectable gap: the frame answers `Resync` and the delta is NOT folded into the book —
    /// previously it silently applied (any `u > last_seq`), corrupting the book until the timed
    /// reseed.
    #[test]
    fn dropped_frame_is_a_resync_and_the_delta_is_not_folded() {
        let mut book = L2Book::new(0.1);
        let snap = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "snapshot",
            "data": {"s":"BTCUSDT","b":[["60000.0","5"]],"a":[["60001.0","4"]],"u":10,"seq":1}
        })
        .to_string();
        assert_eq!(route_frame(&snap, "BTCUSDT", &mut book), MdEvent::BookUpdated);

        // u jumps 10 → 12: the u=11 frame was dropped somewhere upstream
        let gap = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "delta",
            "data": {"s":"BTCUSDT","b":[["60000.0","99"]],"a":[],"u":12,"seq":3}
        })
        .to_string();
        assert_eq!(route_frame(&gap, "BTCUSDT", &mut book), MdEvent::Resync);
        assert_eq!(book.best_bid(), Some((60000.0, 5.0)), "a gapped delta must NOT fold");
        assert_eq!(book.last_seq, 10, "the book's seq anchor is untouched by a gapped delta");
    }

    /// A `u` REGRESSION (a venue restart resets the counter — Bybit signals restarts with a low
    /// `u`, e.g. `u=1`) is a gap → `Resync`, NOT a stale drop: previously every post-restart
    /// delta read as stale (`u <= last_seq`) and the book froze until the timed reseed.
    #[test]
    fn u_regression_is_a_resync_not_a_stale_drop() {
        let mut book = L2Book::new(0.1);
        let snap = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "snapshot",
            "data": {"s":"BTCUSDT","b":[["60000.0","5"]],"a":[["60001.0","4"]],"u":1000,"seq":1}
        })
        .to_string();
        assert_eq!(route_frame(&snap, "BTCUSDT", &mut book), MdEvent::BookUpdated);

        let restart = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "delta",
            "data": {"s":"BTCUSDT","b":[["59999.0","1"]],"a":[],"u":1,"seq":2}
        })
        .to_string();
        assert_eq!(route_frame(&restart, "BTCUSDT", &mut book), MdEvent::Resync);
        assert_eq!(book.best_bid(), Some((60000.0, 5.0)), "a regressed delta must NOT fold");
        // an exact duplicate of the last applied u stays a plain stale drop, not a resync storm
        let dup = serde_json::json!({
            "topic": "orderbook.50.BTCUSDT", "type": "delta",
            "data": {"s":"BTCUSDT","b":[["59999.0","1"]],"a":[],"u":1000,"seq":2}
        })
        .to_string();
        assert_eq!(route_frame(&dup, "BTCUSDT", &mut book), MdEvent::Ignored);
    }

    #[test]
    fn subscribe_ack_ignored() {
        let mut book = L2Book::new(0.1);
        assert_eq!(
            route_frame(r#"{"success":true,"op":"subscribe"}"#, "BTCUSDT", &mut book),
            MdEvent::Ignored
        );
    }
}
