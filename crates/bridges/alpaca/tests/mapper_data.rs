//! Pure WS-message mapping tests for `vike_alpaca::data::decode_ws_message` (Task 11 Step 1).
//! No network — one Alpaca market-data WS frame (a JSON array) in, mapped `DataMsg`s out.

use vike_alpaca::data::{decode_ws_message, DataMsg};

#[test]
fn decodes_bar_quote_trade_array() {
    let frame = r#"[
        {"T":"b","S":"BTC/USD","o":62000.0,"h":62100.0,"l":61900.0,"c":62050.0,"v":1.5,"t":"2026-07-14T05:39:00Z"},
        {"T":"q","S":"BTC/USD","bp":62040.0,"ap":62060.0,"bs":0.5,"as":0.4,"t":"2026-07-14T05:39:01Z"},
        {"T":"t","S":"BTC/USD","p":62055.0,"s":0.01,"t":"2026-07-14T05:39:02Z"}
    ]"#;
    let msgs = decode_ws_message('c', frame);
    assert_eq!(msgs.len(), 3);
    assert!(
        matches!(&msgs[0], DataMsg::Bar { symbol, bar, .. } if symbol == "BTC/USD" && bar.close == 62050.0)
    );
    assert!(matches!(&msgs[1], DataMsg::Quote { q, .. } if q.bid == 62040.0 && q.ask == 62060.0));
    assert!(matches!(&msgs[2], DataMsg::Trade { t, .. } if t.price == 62055.0 && t.size == 0.01));
}

#[test]
fn ignores_control_frames() {
    assert!(decode_ws_message('c', r#"[{"T":"success","msg":"connected"}]"#).is_empty());
    assert!(decode_ws_message('c', r#"[{"T":"subscription","trades":["BTC/USD"]}]"#).is_empty());
}
