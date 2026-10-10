use super::*;

const BOOK_DELTA: &str = r#"{"topic":"orderbook.50.BTCUSDT","type":"delta","ts":1687940967466,"data":{"s":"BTCUSDT","b":[["100.00","1.5"],["99.50","0"]],"a":[["100.50","2"]],"u":177400507,"seq":66544703342},"cts":1687940967464}"#;
const BOOK_SNAPSHOT: &str = r#"{"topic":"orderbook.50.BTCUSDT","type":"snapshot","ts":1687940967466,"data":{"s":"BTCUSDT","b":[["100.00","1.5"]],"a":[],"u":1,"seq":7}}"#;
const TRADES: &str = r#"{"topic":"publicTrade.BTCUSDT","type":"snapshot","ts":1672304486868,"data":[{"T":1672304486865,"s":"BTCUSDT","S":"Sell","v":"0.001","p":"16578.50","L":"PlusTick","i":"20f43950","BT":false},{"T":1672304486866,"s":"BTCUSDT","S":"Buy","v":"0.002","p":"16578.60","i":"20f43951","BT":false}]}"#;

/// The consumed kinds, as the venue sends them (`topic` first), take the typed path and carry the
/// values the `Value` decoder would have read. If this fails the speed-up is gone silently — every
/// frame would fall back and still be correct.
#[test]
fn the_venues_own_frames_take_the_fast_path() {
    match scan_frame(BOOK_DELTA, "BTCUSDT") {
        Ok(Scanned::Book(f)) => {
            assert!(f.kind == BookKind::Delta);
            assert_eq!((f.u, f.ts_ms), (177_400_507, 1_687_940_967_466));
            assert_eq!((f.bids.len(), f.asks.len()), (2, 1));
            assert_eq!(f.bids[1], BookLevel::new(99.5, 0.0));
        }
        _ => panic!("an orderbook delta must take the fast path"),
    }
    match scan_frame(BOOK_SNAPSHOT, "BTCUSDT") {
        Ok(Scanned::Book(f)) => assert!(f.kind == BookKind::Snapshot && f.u == 1),
        _ => panic!("an orderbook snapshot must take the fast path"),
    }
    match scan_frame(TRADES, "BTCUSDT") {
        Ok(Scanned::Trade(t)) => {
            // the FIRST trade only, as the `Value` decoder reads it
            assert_eq!(
                (t.ts, t.price, t.size, t.is_buyer_maker),
                (1_672_304_486_865, 16578.50, 0.001, true)
            );
            assert_eq!(t.symbol, "BTCUSDT");
        }
        _ => panic!("a publicTrade frame must take the fast path"),
    }
    assert!(matches!(
        scan_frame(r#"{"topic":"tickers.BTCUSDT","data":{"symbol":"BTCUSDT"}}"#, "BTCUSDT"),
        Ok(Scanned::Ignored)
    ));
}

/// Each of these is a frame the scan must DECLINE (so the `Value` decoder answers it). A scan that
/// accepted one would be reading a shape it does not fully understand.
#[test]
fn every_shape_it_does_not_fully_understand_declines() {
    let cases: Vec<(&str, String)> = vec![
        (
            "topic not first",
            BOOK_DELTA.replace(
                r#"{"topic":"orderbook.50.BTCUSDT","#,
                r#"{"ts":1,"topic":"orderbook.50.BTCUSDT","#,
            ),
        ),
        (
            "repeated topic",
            BOOK_DELTA.replace(r#""type":"delta","#, r#""type":"delta","topic":"x","#),
        ),
        ("repeated data", BOOK_SNAPSHOT.replace(r#""seq":7}"#, r#""seq":7},"data":null"#)),
        ("repeated type", BOOK_DELTA.replace(r#""ts":"#, r#""type":"snapshot","ts":"#)),
        ("an orderbook without a type", BOOK_DELTA.replace(r#""type":"delta","#, "")),
        ("an unknown type", BOOK_DELTA.replace("delta", "other")),
        ("an orderbook without data", r#"{"topic":"orderbook.50.X","type":"delta"}"#.to_string()),
        ("an orderbook without u", BOOK_DELTA.replace(r#""u":177400507,"#, "")),
        ("a float u", BOOK_DELTA.replace("177400507", "1.5")),
        ("a negative u", BOOK_DELTA.replace("177400507", "-1")),
        ("a string ts", BOOK_DELTA.replace("1687940967466", r#""1""#)),
        ("a level of three", BOOK_DELTA.replace(r#"["100.50","2"]"#, r#"["100.50","2","3"]"#)),
        ("an unparseable qty", BOOK_DELTA.replace(r#""1.5""#, r#""x""#)),
        ("an escaped topic", BOOK_DELTA.replace("orderbook", r"order\u0062ook")),
        (
            "a non-object book data",
            r#"{"topic":"orderbook.50.X","type":"delta","data":[1]}"#.to_string(),
        ),
        ("an empty trade array", r#"{"topic":"publicTrade.X","data":[]}"#.to_string()),
        ("a trade without a price", TRADES.replace(r#""p":"16578.50","#, "")),
        ("a numeric side", TRADES.replacen(r#""S":"Sell""#, r#""S":1"#, 1)),
        ("a non-object first trade", r#"{"topic":"publicTrade.X","data":[1]}"#.to_string()),
        ("trailing text", format!("{TRADES} x")),
        ("two frames", format!("{TRADES}{TRADES}")),
        ("an array envelope", format!(r#"["publicTrade.X",{TRADES}]"#)),
        // The two inputs `IgnoredAny` would skip without the check `Value` applies:
        (
            "a lone surrogate in a skipped field",
            TRADES.replace(r#""BT":false"#, r#""BT":"\ud800""#),
        ),
        (
            "an out-of-range number in a skipped field",
            TRADES.replace(r#""BT":false"#, r#""BT":1e999"#),
        ),
        (
            "nesting past the recursion limit in a skipped field",
            BOOK_DELTA.replace(
                r#""cts":"#,
                &format!(r#""z":{}1{},"cts":"#, "[".repeat(130), "]".repeat(130)),
            ),
        ),
    ];
    for (name, text) in cases {
        assert!(scan_frame(&text, "BTCUSDT").is_err(), "{name}: the scan must decline {text}");
    }
}

/// `frame_scan.rs` is a COPY of `crates/bridges/binance/src/family/frame_scan.rs` (a venue bridge
/// may not name another, and the shared home would cost `vike-bridge-core` a manifest edit). Two
/// copies of the code that decides what the typed decode accepts must not drift apart.
#[test]
fn frame_scan_is_the_binance_copy() {
    assert!(
        include_str!("frame_scan.rs") == include_str!("../../binance/src/family/frame_scan.rs"),
        "crates/bridges/bybit/src/frame_scan.rs and crates/bridges/binance/src/family/frame_scan.rs \
         must stay byte-identical: edit both"
    );
}
