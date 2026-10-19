use super::*;
use vike_binance::family::depth::{MdEvent, route_frame};
use vike_marketdata::BookLevel;
use vike_marketdata::L2Book;

fn combined(stream: &str, data: serde_json::Value) -> String {
    serde_json::json!({ "stream": stream, "data": data }).to_string()
}

#[test]
fn book_ticker_decodes_to_quote() {
    let frame = combined(
        "btcusdt@bookTicker",
        serde_json::json!({"u":1,"s":"BTCUSDT","b":"60000.1","B":"1.5","a":"60000.2","A":"2.0"}),
    );
    let mut book = L2Book::new(0.1);
    match route_frame(&frame, "BTCUSDT", &mut book) {
        MdEvent::Quote(q) => {
            assert_eq!(q.bid, 60000.1);
            assert_eq!(q.ask, 60000.2);
            assert_eq!(q.bid_size, 1.5);
            assert_eq!(q.ask_size, 2.0);
            assert_eq!(q.symbol, "BTCUSDT");
        }
        other => panic!("expected Quote, got {other:?}"),
    }
}

#[test]
fn trade_decodes_to_trade_tick() {
    let frame = combined(
        "btcusdt@trade",
        serde_json::json!({"e":"trade","T":1234,"s":"BTCUSDT","p":"60000.5","q":"0.3","m":true}),
    );
    let mut book = L2Book::new(0.1);
    match route_frame(&frame, "BTCUSDT", &mut book) {
        MdEvent::Trade(t) => {
            assert_eq!(t.ts, 1234);
            assert_eq!(t.price, 60000.5);
            assert_eq!(t.size, 0.3);
            assert!(t.is_buyer_maker);
        }
        other => panic!("expected Trade, got {other:?}"),
    }
}

#[test]
fn depth_sync_snapshot_delta_stale_and_gap() {
    let mut book = L2Book::new(0.1);
    // snapshot seeds last_seq = 100
    book.apply_snapshot(100, &[BookLevel::new(60000.0, 5.0)], &[BookLevel::new(60001.0, 5.0)]);
    assert_eq!(book.last_seq, 100);

    // stale: u <= 100 -> dropped, no change
    let stale =
        combined("btcusdt@depth", serde_json::json!({"U":90,"u":100,"b":[["60000.0","9"]],"a":[]}));
    assert_eq!(route_frame(&stale, "BTCUSDT", &mut book), MdEvent::Ignored);
    assert_eq!(book.best_bid(), Some(BookLevel::new(60000.0, 5.0)), "stale must not apply");

    // contiguous: U(101) == last_seq+1 -> applied, best bid qty updated
    let ok = combined(
        "btcusdt@depth",
        serde_json::json!({"U":101,"u":105,"b":[["60000.0","8"]],"a":[["60001.0","0"]]}),
    );
    assert_eq!(route_frame(&ok, "BTCUSDT", &mut book), MdEvent::BookUpdated);
    assert_eq!(book.best_bid(), Some(BookLevel::new(60000.0, 8.0)));
    assert_eq!(book.best_ask(), None, "qty 0 removed the ask");
    assert_eq!(book.last_seq, 105);

    // gap: U(200) > last_seq+1(106) -> Resync
    let gap = combined(
        "btcusdt@depth",
        serde_json::json!({"U":200,"u":210,"b":[["60000.0","1"]],"a":[]}),
    );
    assert_eq!(route_frame(&gap, "BTCUSDT", &mut book), MdEvent::Resync);
    assert_eq!(book.last_seq, 105, "a gap must not advance the book");
}

#[test]
fn unknown_stream_is_ignored() {
    let mut book = L2Book::new(0.1);
    assert_eq!(
        route_frame(&combined("btcusdt@kline_1m", serde_json::json!({})), "BTCUSDT", &mut book),
        MdEvent::Ignored
    );
    assert_eq!(route_frame("not json", "BTCUSDT", &mut book), MdEvent::Ignored);
}

#[test]
fn combined_stream_url_uses_futures_ws_and_lowercased_symbol() {
    let demo = urls::urls_for(AccountTier::Demo);
    assert_eq!(
        combined_stream_url(demo.fapi_ws, "BTCUSDT"),
        "wss://fstream.asterdex-testnet.com/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
    );
    let live = urls::urls_for(AccountTier::Live);
    assert_eq!(
        combined_stream_url(live.fapi_ws, "ETHUSDT"),
        "wss://fstream.asterdex.com/stream?streams=ethusdt@bookTicker/ethusdt@trade/ethusdt@depth@100ms"
    );
}

/// The `.P` START spelling (`crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` pins `BTCUSDT.P` for this venue) must
/// reach the wire as the exchange's own `btcusdt`: a raw `.P` in the stream name dials a
/// stream nothing serves, so the pump connects, reports healthy and delivers nothing forever.
/// A bare symbol splits to itself — spot-spelled callers are byte-identical to before.
#[test]
fn pump_wire_strips_the_perp_suffix_for_the_stream_and_snapshot() {
    let (url, snap) = pump_wire(AccountTier::Live, "BTCUSDT.P");
    assert_eq!(
        url,
        "wss://fstream.asterdex.com/stream?streams=btcusdt@bookTicker/btcusdt@trade/btcusdt@depth@100ms"
    );
    assert_eq!(snap, "BTCUSDT", "the REST depth seed must ask for the exchange symbol");
    // The identity half: a suffix-less symbol changes nothing.
    assert_eq!(pump_wire(AccountTier::Live, "BTCUSDT"), (url, snap));
}

#[test]
fn depth_snapshot_url_perp_is_futures_v3_spot_is_api_v3() {
    let demo = urls::urls_for(AccountTier::Demo);
    assert_eq!(
        depth_snapshot_url(demo.fapi_rest, "BTCUSDT", true),
        "https://fapi.asterdex-testnet.com/fapi/v3/depth?symbol=BTCUSDT&limit=1000"
    );
    assert_eq!(
        depth_snapshot_url(demo.sapi_rest, "BTCUSDT", false),
        "https://sapi.asterdex-testnet.com/api/v3/depth?symbol=BTCUSDT&limit=1000"
    );
    let live = urls::urls_for(AccountTier::Live);
    assert_eq!(
        depth_snapshot_url(live.fapi_rest, "BTCUSDT", true),
        "https://fapi.asterdex.com/fapi/v3/depth?symbol=BTCUSDT&limit=1000"
    );
}
