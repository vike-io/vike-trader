use super::*;
use crate::market_data::MdEvent;
use std::assert_matches;
use vike_model::L2Book;

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
        Ok(Scanned::Trades(ts)) => {
            // BOTH rows, in frame order
            let seen: Vec<_> =
                ts.iter().map(|t| (t.ts, t.price, t.size, t.is_buyer_maker)).collect();
            assert_eq!(
                seen,
                [
                    (1_672_304_486_865, 16578.50, 0.001, true),
                    (1_672_304_486_866, 16578.60, 0.002, false)
                ]
            );
            assert!(ts.iter().all(|t| t.symbol == "BTCUSDT"));
        }
        _ => panic!("a publicTrade frame must take the fast path"),
    }
    assert_matches!(
        scan_frame(r#"{"topic":"tickers.BTCUSDT","data":{"symbol":"BTCUSDT"}}"#, "BTCUSDT"),
        Ok(Scanned::Ignored)
    );
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
        ("a trade without a price", TRADES.replace(r#""p":"16578.50","#, "")),
        ("a numeric side", TRADES.replacen(r#""S":"Sell""#, r#""S":1"#, 1)),
        ("a non-object trade", r#"{"topic":"publicTrade.X","data":[1]}"#.to_string()),
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

const THREE_TRADES: &str = r#"{"topic":"publicTrade.BTCUSDT","type":"snapshot","ts":1672304486868,"data":[{"T":1672304486865,"s":"BTCUSDT","S":"Sell","v":"0.001","p":"16578.50","i":"20f43950"},{"T":1672304486866,"s":"BTCUSDT","S":"Buy","v":"0.002","p":"16578.60","i":"20f43951"},{"T":1672304486867,"s":"BTCUSDT","S":"Sell","v":"0.003","p":"16578.70","i":"20f43952"}]}"#;

/// The ticks of a `Trades` event, or a panic naming what the router returned instead.
fn trades_of(text: &str) -> Vec<TradeTick> {
    let mut book = L2Book::new(0.1);
    match crate::market_data::route_frame(text, "BTCUSDT", &mut book) {
        MdEvent::Trades(t) => t,
        other => panic!("expected Trades, got {other:?} for {text}"),
    }
}

/// A frame with N trades delivers N ticks, in frame order: the venue batches every fill of one
/// matching pass into one `publicTrade` frame, and each is a tick the strategy must see.
#[test]
fn a_publictrade_frame_with_three_rows_yields_three_ticks() {
    assert_matches!(scan_frame(THREE_TRADES, "BTCUSDT"), Ok(Scanned::Trades(t)) if t.len() == 3);
    let ticks = trades_of(THREE_TRADES);
    let seen: Vec<(i64, f64, f64, bool)> =
        ticks.iter().map(|t| (t.ts, t.price, t.size, t.is_buyer_maker)).collect();
    assert_eq!(
        seen,
        [
            (1_672_304_486_865, 16578.50, 0.001, true),
            (1_672_304_486_866, 16578.60, 0.002, false),
            (1_672_304_486_867, 16578.70, 0.003, true),
        ]
    );
    assert!(ticks.iter().all(|t| t.symbol == "BTCUSDT"));
}

/// An empty `data` array is a frame the scan understands and that carries nothing: no event.
#[test]
fn an_empty_data_array_yields_no_event() {
    let empty = r#"{"topic":"publicTrade.BTCUSDT","type":"snapshot","ts":1,"data":[]}"#;
    assert_matches!(scan_frame(empty, "BTCUSDT"), Ok(Scanned::Trades(t)) if t.is_empty());
    let mut book = L2Book::new(0.1);
    assert_eq!(
        crate::market_data::route_frame(empty, "BTCUSDT", &mut book),
        MdEvent::Ignored,
        "an empty trade array must produce no event (and never an empty Trades)"
    );
}

/// One off-shape row (row 2 of 3 carries a NUMBER where the price is a decimal string) makes the
/// scan decline the WHOLE frame, so the old `Value` decoder answers it: it drops only the row it
/// cannot read. The scan never hands the pump a partial list of its own.
#[test]
fn a_bad_row_declines_to_the_old_decoder_whole() {
    let bad = THREE_TRADES.replace(r#""p":"16578.60""#, r#""p":16578.60"#);
    assert_ne!(bad, THREE_TRADES, "the fixture edit must change the frame");
    assert!(scan_frame(&bad, "BTCUSDT").is_err(), "the scan must decline a frame with a bad row");
    let prices: Vec<f64> = trades_of(&bad).iter().map(|t| t.price).collect();
    assert_eq!(prices, [16578.50, 16578.70], "the old decoder keeps the two rows it can read");
}
