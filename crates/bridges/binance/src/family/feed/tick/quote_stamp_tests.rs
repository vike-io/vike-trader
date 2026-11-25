//! A quote leaves the binance-family tick pump stamped with its receive time (`send_quote`).
//!
//! Same shape as `crates/bridges/oanda/src/feed/feed_property_tests.rs`'s receive-stamp pins: the decoder is
//! pure and states `local_ts: 0` ("not stamped"); what reaches the core's tick lane is the pump's.

use serde_json::json;
use vike_bridge_core::depth::send_quote;
use vike_execution::{Ingest, event_channel};

use super::*;

#[test]
fn a_quote_leaves_the_pump_stamped_with_the_machine_receive_time() {
    let (events, mut rx) = event_channel(4);
    let ticks = TickSender { ingest: events.ingest.clone() };

    let quote = decode_book_ticker(
        &json!({"b": "60000.1", "a": "60000.2", "B": "1.5", "A": "2"}),
        "BTCUSDT",
    )
    .expect("a bookTicker payload decodes");
    assert_eq!(quote.local_timestamp, 0, "the pure decoder stamps nothing");

    let before = vike_model::now_ms();
    send_quote(&ticks, "binance", "BTCUSDT", quote).expect("the channel is open");
    let after = vike_model::now_ms();

    let Ok(Ingest::Quote(update)) = rx.try_recv() else {
        panic!("a QuoteUpdate must be on the lane")
    };
    assert_eq!((update.venue.as_str(), update.symbol.as_str()), ("binance", "BTCUSDT"));
    assert_eq!((update.quote.bid, update.quote.ask), (60000.1, 60000.2));
    assert!(
        (before..=after).contains(&update.quote.local_timestamp),
        "local_ts {} must be the receive time, between {before} and {after}",
        update.quote.local_timestamp
    );
}

/// What a quote leaves the pump as: through [`send_quote`], onto a tick lane, read back.
fn sent(quote: TickQuote) -> TickQuote {
    let (events, mut rx) = event_channel(4);
    let ticks = TickSender { ingest: events.ingest.clone() };
    send_quote(&ticks, "binance", "BTCUSDT", quote).expect("the channel is open");
    let Ok(Ingest::Quote(update)) = rx.try_recv() else {
        panic!("a QuoteUpdate must be on the lane")
    };
    update.quote
}

/// A futures bookTicker frame (written out, so it takes the typed fast path) keeps the venue's `E`, one with `T`
/// alone keeps `T`; a spot frame states neither and leaves the pump stamped with its arrival.
#[test]
fn a_quote_leaves_the_pump_with_the_venues_time_else_its_arrival() {
    let frame = |data: &str| format!(r#"{{"stream":"btcusdt@bookTicker","data":{data}}}"#);
    let ts_of = |text: &str| match route_frame(text, "BTCUSDT.P", &mut L2Book::new(0.1)) {
        DataClientEvent::Quote(q) => sent(q).timestamp,
        other => panic!("expected a quote, got {other:?}"),
    };
    let futures = frame(
        r#"{"e":"bookTicker","u":400900217,"E":1568014460893,"T":1568014460891,"s":"BTCUSDT","b":"60000.1","B":"1.5","a":"60000.2","A":"2.0"}"#,
    );
    assert_eq!(ts_of(&futures), 1_568_014_460_893, "the event time E");
    let t_only = frame(r#"{"T":1568014460891,"b":"60000.1","a":"60000.2"}"#);
    assert_eq!(ts_of(&t_only), 1_568_014_460_891, "the transaction time T when E is absent");
    let before = vike_model::now_ms();
    let spot =
        ts_of(&frame(r#"{"u":1,"s":"BTCUSDT","b":"60000.1","B":"1.5","a":"60000.2","A":"2"}"#));
    assert!(
        (before..=vike_model::now_ms()).contains(&spot),
        "a spot quote: its arrival, got {spot}"
    );
}

/// The point of the stamp: on the core's price board a quote stamped more than the streamed-mark window after a
/// mark takes over from it (`crates/vike-execution/src/price_board.rs`'s `PriceBoard::classify`). Stamped 0, as the
/// spot stream's quotes were, it never could, and a mark nothing refreshed priced the symbol for ever.
#[test]
fn a_stamped_quote_takes_over_from_a_ten_second_older_mark() {
    use vike_execution::price_board::{
        PriceBoard, PriceBoardConfig, PriceBoardResolution, PriceSource,
    };
    let quote = sent(
        decode_book_ticker(&json!({"b": "60000.1", "a": "60000.2"}), "BTCUSDT").expect("decodes"),
    );
    let mut board = PriceBoard::default();
    board.set_mark(
        "binance",
        "BTCUSDT",
        55_000.0,
        quote.timestamp - vike_execution::DEFAULT_MARK_STALENESS_MS - 1,
    );
    board.set_quote("binance", "BTCUSDT", quote.bid, quote.ask, quote.timestamp);
    assert_eq!(
        board.resolve("binance", "BTCUSDT", true, quote.timestamp, &PriceBoardConfig::default()),
        PriceBoardResolution::Priced {
            price: 60000.1,
            source: PriceSource::Bid,
            timestamp: quote.timestamp
        }
    );
}
