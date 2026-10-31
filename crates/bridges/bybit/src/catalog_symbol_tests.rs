//! `perp_symbol_at` against a scripted loopback HTTP double (no venue network): the per-symbol
//! request, the inverse fallback, and that a half-read is an error and never "no such symbol".

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::{parse_dated, parse_inverse, parse_perp, perp_symbol_at, spot_symbol_at};

/// A loopback server that answers its `replies` to successive connections, in order, and records each
/// request line. Returns the endpoint URL and the request log.
fn serve(replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base =
        format!("http://{}/v5/market/instruments-info", listener.local_addr().expect("addr"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for (status, body) in replies {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            log.lock().expect("log").push(head.lines().next().unwrap_or("").to_string());
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (base, seen)
}

fn page(rows: serde_json::Value) -> String {
    serde_json::json!({ "retCode": 0, "retMsg": "OK", "result": { "list": rows } }).to_string()
}

fn linear_row() -> serde_json::Value {
    serde_json::json!({
        "symbol": "BTCUSDT", "baseCoin": "BTC", "quoteCoin": "USDT", "status": "Trading",
        "contractType": "LinearPerpetual", "settleCoin": "USDT",
        "priceFilter": { "tickSize": "0.10" },
        "lotSizeFilter": { "qtyStep": "0.001", "minOrderQty": "0.001", "maxOrderQty": "1190" }
    })
}

fn inverse_row() -> serde_json::Value {
    serde_json::json!({
        "symbol": "BTCUSD", "baseCoin": "BTC", "quoteCoin": "USD", "status": "Trading",
        "contractType": "InversePerpetual", "settleCoin": "BTC",
        "priceFilter": { "tickSize": "0.50" },
        "lotSizeFilter": { "qtyStep": "1", "minOrderQty": "1", "maxOrderQty": "1000000" }
    })
}

#[test]
fn a_linear_symbol_is_one_request_with_the_symbol_in_the_query() {
    let (base, seen) = serve(vec![(200, page(serde_json::json!([linear_row()])))]);
    let rows = perp_symbol_at(&base, "BTCUSDT").expect("row");
    assert_eq!(
        seen.lock().expect("log").as_slice(),
        ["GET /v5/market/instruments-info?category=linear&symbol=BTCUSDT HTTP/1.1"]
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].raw_symbol, "BTCUSDT.P");
    assert_eq!(rows[0].properties.tick_size, 0.1);
}

#[test]
fn an_inverse_symbol_the_linear_read_returns_is_still_one_request() {
    // the linear category answers for an inverse symbol (measured 2026-09-16)
    let (base, seen) = serve(vec![(200, page(serde_json::json!([inverse_row()])))]);
    let rows = perp_symbol_at(&base, "BTCUSD").expect("row");
    assert_eq!(seen.lock().expect("log").len(), 1);
    assert_eq!(rows[0].raw_symbol, "BTCUSD.P");
    assert_eq!(rows[0].contract_type.as_deref(), Some("InversePerpetual"));
}

#[test]
fn a_symbol_linear_does_not_carry_is_asked_of_inverse() {
    let (base, seen) = serve(vec![
        (200, page(serde_json::json!([]))),
        (200, page(serde_json::json!([inverse_row()]))),
    ]);
    let rows = perp_symbol_at(&base, "BTCUSD").expect("row");
    assert_eq!(
        seen.lock().expect("log").as_slice(),
        [
            "GET /v5/market/instruments-info?category=linear&symbol=BTCUSD HTTP/1.1",
            "GET /v5/market/instruments-info?category=inverse&symbol=BTCUSD HTTP/1.1",
        ]
    );
    assert_eq!(rows.len(), 1);
}

#[test]
fn neither_category_carrying_it_is_an_empty_answer() {
    let (base, _) =
        serve(vec![(200, page(serde_json::json!([]))), (200, page(serde_json::json!([])))]);
    assert!(perp_symbol_at(&base, "NOPEUSDT").expect("an answer").is_empty());
}

/// The half-read: linear came back empty and the inverse read FAILED. That is not "no such symbol".
#[test]
fn a_half_read_is_an_error_never_no_such_symbol() {
    for second in [
        (500, "down".to_string()),
        (200, r#"{"retCode":10006,"retMsg":"Too many visits"}"#.to_string()),
        (200, "not json".to_string()),
    ] {
        let (base, _) = serve(vec![(200, page(serde_json::json!([]))), second.clone()]);
        assert!(perp_symbol_at(&base, "BTCUSD").is_err(), "{second:?}");
    }
    // and a failing FIRST read never reaches the second
    let (base, seen) = serve(vec![(500, "down".to_string())]);
    assert!(perp_symbol_at(&base, "BTCUSDT").is_err());
    assert_eq!(seen.lock().expect("log").len(), 1);
}

#[test]
fn a_symbol_that_could_alter_the_query_is_refused_before_any_request() {
    let (base, seen) = serve(vec![(200, page(serde_json::json!([])))]);
    for bad in ["", "BTC&limit=1", "BTC USDT", "BTC/USDT", "BTCUSDT.P"] {
        let err = perp_symbol_at(&base, bad).expect_err(bad);
        assert!(err.0.contains("not a perpetual symbol spelling"), "{err}");
    }
    assert!(seen.lock().expect("log").is_empty());
}

// ---- decision 0231: a dated future and a spot pair, over the rows the demo host served on 2026-10-30 ------------

/// A dated linear future is one request, and its row is keyed by the venue's OWN name (no `.P`, which says
/// PERPETUAL), typed as a dated `Future` with its delivery as the expiry.
#[test]
fn a_dated_linear_future_is_one_request_and_a_row_under_its_own_name() {
    let body =
        include_str!("../tests/fixtures/classes/instruments_linear_future_ethusdt_06nov26.json");
    let (base, seen) = serve(vec![(200, body.to_string())]);
    let rows = perp_symbol_at(&base, "ETHUSDT-06NOV26").expect("row");
    assert_eq!(
        seen.lock().expect("log").as_slice(),
        ["GET /v5/market/instruments-info?category=linear&symbol=ETHUSDT-06NOV26 HTTP/1.1"]
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].raw_symbol, "ETHUSDT-06NOV26");
    assert_eq!(rows[0].contract_type.as_deref(), Some("LinearFutures"));
    assert_eq!((rows[0].properties.tick_size, rows[0].properties.step_size), (0.01, 0.01));
    let vike_model::Contract::Future(spec) = &rows[0].contract else { panic!("{:?}", rows[0]) };
    assert_eq!(spec.expiry_ms, 1_793_952_000_000);
    assert_eq!(spec.margining, vike_model::Margining::Linear);
}

/// A dated coin-margined future: asked of `linear` (empty) and then of `inverse`, which carries it; the row is
/// typed `Inverse` and keyed by its own name.
#[test]
fn a_dated_inverse_future_is_found_in_the_inverse_book_under_its_own_name() {
    let body = include_str!("../tests/fixtures/classes/instruments_inverse_future_btcusdz26.json");
    let (base, seen) = serve(vec![(200, page(serde_json::json!([]))), (200, body.to_string())]);
    let rows = perp_symbol_at(&base, "BTCUSDZ26").expect("row");
    assert_eq!(seen.lock().expect("log").len(), 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].raw_symbol, "BTCUSDZ26");
    assert_eq!(rows[0].contract.margining(), Some(vike_model::Margining::Inverse));
}

/// The picker still mints no dated row (nothing in the data plane routes one); only the symbol read does.
#[test]
fn the_pickers_listing_still_mints_no_dated_row() {
    let payload: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/classes/instruments_linear_future_ethusdt_06nov26.json"
    ))
    .unwrap();
    assert!(parse_perp(&payload).is_empty() && parse_inverse(&payload).is_empty());
    assert_eq!(parse_dated(&payload).len(), 1);
}

/// A spot pair is `category=spot&symbol=`, one request, the picker's own row (keyed by the bare pair).
#[test]
fn a_spot_pair_is_one_request_and_the_pickers_row() {
    let body = include_str!("../tests/fixtures/classes/instruments_spot_dogeusdt.json");
    let (base, seen) = serve(vec![(200, body.to_string())]);
    let rows = spot_symbol_at(&base, "DOGEUSDT").expect("row");
    assert_eq!(
        seen.lock().expect("log").as_slice(),
        ["GET /v5/market/instruments-info?category=spot&symbol=DOGEUSDT HTTP/1.1"]
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].raw_symbol, "DOGEUSDT");
    assert_eq!(rows[0].contract, vike_model::Contract::CryptoSpot);
    assert_eq!((rows[0].properties.tick_size, rows[0].properties.step_size), (0.00001, 0.1));
    let err = spot_symbol_at(&base, "DOGEUSDT.S")
        .expect_err("the suffix is the engine's, not the venue's");
    assert!(err.0.contains("spot pair spelling"), "{err}");
}
