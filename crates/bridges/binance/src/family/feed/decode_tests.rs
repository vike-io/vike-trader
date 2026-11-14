use super::*;
use std::assert_matches;

fn wire(stream: &str, data: &str) -> String {
    format!(r#"{{"stream":"{stream}","data":{data}}}"#)
}

const TICKER: &str =
    r#"{"u":400900217,"s":"BTCUSDT","b":"60000.10","B":"1.50","a":"60000.20","A":"2.25"}"#;
const FUTURES_TICKER: &str = r#"{"e":"bookTicker","u":400900217,"E":1700000000000,"T":1700000000001,"s":"BTCUSDT","b":"60000.10","B":"1.50","a":"60000.20","A":"2.25"}"#;
const TRADE: &str = r#"{"e":"trade","E":1700000000000,"s":"BTCUSDT","t":3000000001,"p":"60000.10","q":"0.001","T":1700000000001,"m":true,"M":true}"#;
const FUTURES_DEPTH: &str = r#"{"e":"depthUpdate","E":1700000000000,"T":1700000000001,"s":"BTCUSDT","U":6,"u":9,"pu":5,"b":[["100.00","1.5"],["99.50","0"]],"a":[["100.50","2"]]}"#;
const SPOT_DEPTH: &str = r#"{"e":"depthUpdate","E":1700000000000,"s":"BTCUSDT","U":6,"u":9,"b":[["100.00","1.5"]],"a":[]}"#;

/// The three consumed kinds, as the venue sends them (`stream` first), take the typed path and
/// carry the values the `Value` decoder would have read. If this fails the speed-up is gone
/// silently — every frame would fall back and still be correct.
#[test]
fn the_venues_own_frames_take_the_fast_path() {
    match scan_frame(&wire("btcusdt@bookTicker", TICKER), "BTCUSDT") {
        Ok(FeedScan::Quote(q)) => {
            assert_eq!((q.bid, q.ask, q.bid_size, q.ask_size), (60000.10, 60000.20, 1.50, 2.25));
            assert_eq!((q.timestamp, q.local_timestamp, q.symbol.as_str()), (0, 0, "BTCUSDT"));
        }
        _ => panic!("a bookTicker frame must take the fast path"),
    }
    match scan_frame(&wire("btcusdt@bookTicker", FUTURES_TICKER), "BTCUSDT.P") {
        Ok(FeedScan::Quote(q)) => {
            assert_eq!((q.timestamp, q.bid), (1_700_000_000_000, 60000.10), "E")
        }
        _ => panic!("a futures bookTicker frame must take the fast path"),
    }
    match scan_frame(&wire("btcusdt@trade", TRADE), "BTCUSDT") {
        Ok(FeedScan::Trade(t)) => {
            assert_eq!(
                (t.timestamp, t.price, t.size, t.is_buyer_maker),
                (1_700_000_000_001, 60000.10, 0.001, true)
            );
        }
        _ => panic!("a trade frame must take the fast path"),
    }
    match scan_frame(&wire("btcusdt@depth@100ms", FUTURES_DEPTH), "BTCUSDT") {
        Ok(FeedScan::Depth(d)) => {
            assert_eq!((d.first_u, d.final_u, d.previous_final_u), (6, 9, Some(5)));
            assert_eq!((d.bids.len(), d.asks.len()), (2, 1));
            assert_eq!(d.bids[1], L2BookLevel::new(99.5, 0.0));
        }
        _ => panic!("a futures depth frame must take the fast path"),
    }
    match scan_frame(&wire("btcusdt@depth@100ms", SPOT_DEPTH), "BTCUSDT") {
        Ok(FeedScan::Depth(d)) => {
            assert_eq!((d.first_u, d.final_u, d.previous_final_u), (6, 9, None))
        }
        _ => panic!("a spot depth frame must take the fast path"),
    }
    assert_matches!(
        scan_frame(&wire("btcusdt@kline_1m", r#"{"k":{"t":1}}"#), "BTCUSDT"),
        Ok(FeedScan::Ignored)
    );
}

/// Each of these is a frame the scan must DECLINE (so the `Value` decoder answers it). A scan that
/// accepted one would be reading a shape it does not fully understand.
#[test]
fn every_shape_it_does_not_fully_understand_declines() {
    let cases: Vec<(&str, String)> = vec![
        ("data before stream", format!(r#"{{"data":{TICKER},"stream":"x@bookTicker"}}"#)),
        ("repeated stream", format!(r#"{{"stream":"a","stream":"b@trade","data":{TRADE}}}"#)),
        ("a key after data", format!(r#"{{"stream":"x@bookTicker","data":{TICKER},"z":1}}"#)),
        ("a key before stream", format!(r#"{{"z":1,"stream":"x@bookTicker","data":{TICKER}}}"#)),
        ("an escaped stream", r#"{"stream":"x@tr\u0061de","data":{"p":"1","q":"1"}}"#.to_string()),
        ("a repeated data key", wire("x@trade", r#"{"p":"1","p":"2","q":"1"}"#)),
        ("a number where a string belongs", wire("x@bookTicker", r#"{"b":1.5,"a":"2"}"#)),
        ("an unparseable price", wire("x@bookTicker", r#"{"b":"abc","a":"2"}"#)),
        ("a missing price", wire("x@trade", r#"{"p":"1"}"#)),
        ("a level of three", wire("x@depth", r#"{"U":1,"u":2,"b":[["1","2","3"]]}"#)),
        ("a level of one", wire("x@depth", r#"{"U":1,"u":2,"b":[["1"]]}"#)),
        ("a negative id", wire("x@depth", r#"{"U":-1,"u":2}"#)),
        ("a float id", wire("x@depth", r#"{"U":1.0,"u":2}"#)),
        ("an array envelope", format!(r#"["x@trade",{TRADE}]"#)),
        ("a non-object data", wire("x@trade", "[1,2]")),
        ("a null data", wire("x@trade", "null")),
        ("trailing text", format!("{} x", wire("x@trade", TRADE))),
        ("two frames", format!("{0}{0}", wire("x@trade", TRADE))),
        // The two inputs `IgnoredAny` would skip without the check `Value` applies:
        (
            "a lone surrogate in a skipped field",
            wire("x@trade", r#"{"p":"1","q":"1","s":"\ud800"}"#),
        ),
        (
            "an out-of-range number in a skipped field",
            wire("x@trade", r#"{"p":"1","q":"1","s":1e999}"#),
        ),
        (
            "nesting past the recursion limit in a skipped field",
            wire(
                "x@trade",
                &format!(r#"{{"p":"1","q":"1","s":{}1{}}}"#, "[".repeat(130), "]".repeat(130)),
            ),
        ),
    ];
    for (name, text) in cases {
        assert!(scan_frame(&text, "BTCUSDT").is_err(), "{name}: the scan must decline {text}");
    }
}

/// A print the workspace's one trade guard refuses (`vike_bridge_core::is_valid_trade`) is no print on
/// the pump, on both paths: a zero price read by the scan, and a negative size on the `Value` path (a
/// repeated key the scan declines). The valid print beside each still passes.
#[test]
fn a_print_the_trade_guard_refuses_is_ignored_on_both_paths() {
    use crate::family::feed::tick::route_frame;
    use vike_bridge_core::data_client::event::DataClientEvent;
    let mut book = vike_marketdata::L2Book::new(0.01);
    let zero = TRADE.replace(r#""p":"60000.10""#, r#""p":"0""#);
    assert_matches!(scan_frame(&wire("btcusdt@trade", &zero), "BTCUSDT"), Ok(FeedScan::Trade(_)));
    assert_eq!(
        route_frame(&wire("btcusdt@trade", &zero), "BTCUSDT", &mut book),
        DataClientEvent::Ignored
    );
    assert_matches!(
        route_frame(&wire("btcusdt@trade", TRADE), "BTCUSDT", &mut book),
        DataClientEvent::Trade(_)
    );
    let negative = wire("btcusdt@trade", r#"{"p":"1","p":"2","q":"-1"}"#);
    assert!(scan_frame(&negative, "BTCUSDT").is_err(), "the scan declines a repeated key");
    assert_eq!(route_frame(&negative, "BTCUSDT", &mut book), DataClientEvent::Ignored);
    let valid = wire("btcusdt@trade", r#"{"p":"1","p":"2","q":"1"}"#);
    assert_matches!(route_frame(&valid, "BTCUSDT", &mut book), DataClientEvent::Trade(t) if t.price == 2.0);
}
