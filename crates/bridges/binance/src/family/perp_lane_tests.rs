use super::*;
use crate::market_feed::BINANCE_URLS;

/// Real frames, captured live 2026-08-02 from `wss://fstream.binance.com/ws/btcusdt@trade`.
const WS_RAW: &str = r#"{"e":"trade","E":1785701708157,"T":1785701708156,"s":"BTCUSDT","t":7947574407,"p":"63437.30","q":"0.003","X":"MARKET","m":false,"st":1}"#;
/// Real body, captured live from `GET /fapi/v1/trades?symbol=BTCUSDT&limit=2`.
const REST_RAW: &str = r#"[{"id":7947579848,"price":"63440.80","qty":"0.001","quoteQty":"63.44","time":1785702045391,"isBuyerMaker":true,"isRPITrade":false}]"#;

#[test]
fn the_raw_ws_frame_decodes_with_its_trade_id() {
    let t = ws_raw_trade("BTCUSDT.P", WS_RAW).expect("decodes");
    assert_eq!(t.id, 7_947_574_407, "`t`, the RAW trade id");
    assert_eq!(t.tick.ts, 1_785_701_708_156, "`T` (trade time), not `E` (event time)");
    assert_eq!(t.tick.price, 63437.30);
    assert_eq!(t.tick.size, 0.003);
    assert!(!t.tick.is_buyer_maker);
    assert_eq!(t.tick.symbol, "BTCUSDT.P", "the SERIES label, never the wire `s`");
}

/// The REST array uses entirely different field NAMES for the same five values — one struct
/// covers both only because every field carries an alias.
#[test]
fn the_raw_rest_body_decodes_with_the_same_id_space() {
    let v = rest_raw_trades("BTCUSDT.P", REST_RAW);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].id, 7_947_579_848);
    assert_eq!(v[0].tick.ts, 1_785_702_045_391);
    assert_eq!(v[0].tick.price, 63440.80);
    assert!(v[0].tick.is_buyer_maker);
}

/// Each decoder rejects the OTHER lane's frame, so a lane/decoder mismatch fails loudly at the
/// first frame instead of silently ignoring every one of them — which is exactly how the dead
/// `@aggTrade` subscription looked from the inside.
#[test]
fn the_two_decoders_do_not_accept_each_others_frames() {
    let agg = r#"{"e":"aggTrade","E":1,"s":"BTCUSDT","a":5,"p":"1.0","q":"2.0","T":3,"m":false}"#;
    assert!(ws_raw_trade("S", agg).is_none(), "raw decoder must reject an aggTrade event");
    assert!(ws_agg_trade("S", WS_RAW).is_none(), "agg decoder must reject a trade event");
}

/// **The bug, pinned.** Binance perp subscribes `@trade`; binance spot and BOTH aster classes
/// stay on `@aggTrade`. Measured: binance futures `@aggTrade` = 0 frames/60s, `@trade` = 770;
/// aster futures `@aggTrade` = 10/15s.
#[test]
fn only_binance_perp_rides_the_raw_lane() {
    assert_eq!(trades_stream(&BINANCE_URLS, true), "@trade");
    assert_eq!(trades_stream(&BINANCE_URLS, false), "@aggTrade");
    assert_eq!(
        agg_trades_ws_url(&BINANCE_URLS, "BTCUSDT", true),
        "wss://fstream.binance.com/ws/btcusdt@trade"
    );
    assert_eq!(
        agg_trades_ws_url(&BINANCE_URLS, "BTCUSDT", false),
        "wss://stream.binance.com:9443/ws/btcusdt@aggTrade",
        "spot is byte-identical to before"
    );
}

/// The warmup URL must ride the SAME lane as the stream, or `handoff`'s id dedup compares two
/// unrelated sequences and nothing ever dedups.
#[test]
fn the_warmup_url_follows_the_lane() {
    assert_eq!(
        trades_rest_url(&BINANCE_URLS, "BTCUSDT", true, 1000),
        "https://fapi.binance.com/fapi/v1/trades?symbol=BTCUSDT&limit=1000"
    );
    assert_eq!(
        trades_rest_url(&BINANCE_URLS, "BTCUSDT", false, 1000),
        agg_trades_rest_url(&BINANCE_URLS, "BTCUSDT", false, 1000, None),
        "spot still goes through the aggTrades builder unchanged"
    );
}
