//! Pure WS-message mapping tests for `vike_alpaca::feed::decode_websocket_message`.
//! No network — one Alpaca market-data WS frame (a JSON array) in, mapped `FeedMessage`s out.

use std::assert_matches;
use vike_alpaca::feed::{FeedMessage, decode_websocket_message};

#[test]
fn decodes_bar_quote_trade_array() {
    let frame = r#"[
        {"T":"b","S":"BTC/USD","o":62000.0,"h":62100.0,"l":61900.0,"c":62050.0,"v":1.5,"t":"2026-07-14T05:39:00Z"},
        {"T":"q","S":"BTC/USD","bp":62040.0,"ap":62060.0,"bs":0.5,"as":0.4,"t":"2026-07-14T05:39:01Z"},
        {"T":"t","S":"BTC/USD","p":62055.0,"s":0.01,"t":"2026-07-14T05:39:02Z"}
    ]"#;
    let messages = decode_websocket_message('c', frame);
    assert_eq!(messages.len(), 3);
    assert_matches!(
        &messages[0], FeedMessage::Bar { symbol, bar, .. } if symbol == "BTC/USD" && bar.close == 62050.0
    );
    assert_matches!(&messages[1], FeedMessage::Quote { q, .. } if q.bid == 62040.0 && q.ask == 62060.0);
    assert_matches!(&messages[2], FeedMessage::Trade { t, .. } if t.price == 62055.0 && t.size == 0.01);
}

#[test]
fn ignores_control_frames() {
    assert!(decode_websocket_message('c', r#"[{"T":"success","msg":"connected"}]"#).is_empty());
    assert!(
        decode_websocket_message('c', r#"[{"T":"subscription","trades":["BTC/USD"]}]"#).is_empty()
    );
}
