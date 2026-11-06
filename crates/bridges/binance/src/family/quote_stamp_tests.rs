//! A quote leaves the binance-family tick pump stamped with its receive time (`send_quote`).
//!
//! Same shape as `crates/bridges/oanda/src/market_feed_props.rs`'s receive-stamp pins: the decoder is
//! pure and states `local_ts: 0` ("not stamped"); what reaches the core's tick lane is the pump's.

use serde_json::json;
use vike_exec::{Ingest, event_channel};

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
    assert_eq!(quote.local_ts, 0, "the pure decoder stamps nothing");

    let before = vike_model::now_ms();
    send_quote(&ticks, "binance", "BTCUSDT", quote).expect("the channel is open");
    let after = vike_model::now_ms();

    let Ok(Ingest::Quote(update)) = rx.try_recv() else {
        panic!("a QuoteUpdate must be on the lane")
    };
    assert_eq!((update.venue.as_str(), update.symbol.as_str()), ("binance", "BTCUSDT"));
    assert_eq!((update.quote.bid, update.quote.ask), (60000.1, 60000.2));
    assert!(
        (before..=after).contains(&update.quote.local_ts),
        "local_ts {} must be the receive time, between {before} and {after}",
        update.quote.local_ts
    );
}
