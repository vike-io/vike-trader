//! `spot_symbol` and `perp_listing` against a scripted loopback HTTP double (no venue network): the
//! request each one makes, and that a failure is never read as "no such symbol".

use std::sync::{Arc, Mutex};

use super::{perp_listing, spot_symbol};

/// The scripted origin, at the base URL the lookups are handed.
fn serve(replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
    vike_log::loopback::serve_replies("", replies)
}

fn spot_body() -> String {
    serde_json::json!({ "symbols": [{
        "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT", "status": "TRADING",
        "filters": [
            { "filterType": "PRICE_FILTER", "tickSize": "0.01000000" },
            { "filterType": "LOT_SIZE", "minQty": "0.00001000", "maxQty": "9000.0", "stepSize": "0.00001000" },
            { "filterType": "NOTIONAL", "minNotional": "5.00000000" }
        ]
    }]})
    .to_string()
}

#[test]
fn a_spot_ask_names_the_one_symbol_and_reads_its_row() {
    let (base, seen) = serve(vec![(200, spot_body())]);
    let rows =
        spot_symbol("binance", &format!("{base}/api/v3/exchangeInfo"), "BTCUSDT").expect("one row");
    assert_eq!(
        seen.lock().expect("log").as_slice(),
        ["GET /api/v3/exchangeInfo?symbol=BTCUSDT HTTP/1.1"],
        "exactly one request, filtered at the venue"
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].raw_symbol, "BTCUSDT");
    assert_eq!(rows[0].properties.tick_size, 0.01);
    assert_eq!(rows[0].properties.step_size, 0.00001);
}

#[test]
fn the_venues_invalid_symbol_answer_is_an_empty_row_set_not_a_failure() {
    let (base, _) = serve(vec![(400, r#"{"code":-1121,"msg":"Invalid symbol."}"#.to_string())]);
    let rows = spot_symbol("binance", &format!("{base}/api/v3/exchangeInfo"), "NOPEUSDT")
        .expect("an unknown symbol is an answer");
    assert!(rows.is_empty());
}

#[test]
fn a_halted_symbol_is_an_empty_row_set() {
    let body = spot_body().replace("TRADING", "BREAK");
    let (base, _) = serve(vec![(200, body)]);
    let rows = spot_symbol("binance", &format!("{base}/api/v3/exchangeInfo"), "BTCUSDT")
        .expect("a halted row is an answer");
    assert!(rows.is_empty());
}

#[test]
fn every_other_failure_is_an_error_never_the_empty_answer() {
    for (status, body) in [
        (500, "boom".to_string()),
        (418, r#"{"code":-1003,"msg":"banned"}"#.to_string()),
        (400, r#"{"code":-1100,"msg":"Illegal characters"}"#.to_string()),
        (200, "not json".to_string()),
    ] {
        let (base, _) = serve(vec![(status, body.clone())]);
        let err = spot_symbol("binance", &format!("{base}/api/v3/exchangeInfo"), "BTCUSDT")
            .expect_err(&format!("HTTP {status} {body}"));
        assert!(err.0.starts_with("binance "), "{err}");
    }
    // a refused connection is a failure too
    assert!(spot_symbol("binance", "http://127.0.0.1:1/api/v3/exchangeInfo", "BTCUSDT").is_err());
}

#[test]
fn a_symbol_that_could_alter_the_query_is_refused_before_any_request() {
    let (base, seen) = serve(vec![(200, spot_body())]);
    for bad in ["", "BTC&limit=1", "BTC USDT", "BTC/USDT", "BTC%26x"] {
        let err =
            spot_symbol("binance", &format!("{base}/api/v3/exchangeInfo"), bad).expect_err(bad);
        assert!(err.0.contains("not a spot symbol spelling"), "{err}");
    }
    assert!(seen.lock().expect("log").is_empty(), "no request left the process");
}

#[test]
fn the_perp_listing_is_one_unfiltered_request_and_strict() {
    let perp = serde_json::json!({ "symbols": [{
        "symbol": "BTCUSDT", "baseAsset": "BTC", "quoteAsset": "USDT", "marginAsset": "USDT",
        "status": "TRADING", "contractType": "PERPETUAL", "filters": []
    }]})
    .to_string();
    let (base, seen) = serve(vec![(200, perp)]);
    let rows = perp_listing("binance", &format!("{base}/fapi/v1/exchangeInfo")).expect("list");
    assert_eq!(seen.lock().expect("log").as_slice(), ["GET /fapi/v1/exchangeInfo HTTP/1.1"]);
    assert_eq!(rows.iter().map(|r| r.raw_symbol.as_str()).collect::<Vec<_>>(), ["BTCUSDT.P"]);

    let (base, _) = serve(vec![(500, "down".to_string())]);
    assert!(
        perp_listing("binance", &format!("{base}/fapi/v1/exchangeInfo")).is_err(),
        "a failed fetch is an error, not an empty list"
    );
}
