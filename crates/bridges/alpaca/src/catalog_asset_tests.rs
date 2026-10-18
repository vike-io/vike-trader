//! The per-asset read (`fetch_asset`, `AlpacaCatalog::asset`) against a scripted loopback HTTP double
//! (no venue network): the path and its encoding, the `404`, and that no credential reaches a message.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::*;
use crate::auth::AuthError;

const TOKEN: &str = "tok-SECRET-bearer";

/// A loopback server answering `replies` to successive connections, in order, recording each request's
/// full head. Returns the base URL and the log.
fn serve(replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for (status, body) in replies {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            log.lock().expect("log").push(String::from_utf8_lossy(&buf[..n]).into_owned());
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

fn rest() -> AlpacaRest {
    AlpacaRest::new(Arc::new(TokenSource::with_clock_and_exchanger(
        Box::new(|| 0),
        Box::new(|_| Ok((TOKEN.to_string(), 900))),
    )))
}

fn request_line(head: &str) -> &str {
    head.lines().next().unwrap_or("")
}

#[test]
fn an_equity_is_one_request_on_its_own_path_with_the_bearer() {
    let body = r#"{"symbol":"AAPL","class":"us_equity","exchange":"NASDAQ","tradable":true,
                   "price_increment":"0.01","min_trade_increment":"1"}"#;
    let (base, seen) = serve(vec![(200, body.to_string())]);
    let rows = fetch_asset(&rest(), &base, "AAPL").expect("row");
    let log = seen.lock().expect("log");
    assert_eq!(log.len(), 1, "one request, not the list");
    assert_eq!(request_line(&log[0]), "GET /v1/assets/AAPL HTTP/1.1");
    assert!(
        log[0]
            .to_ascii_lowercase()
            .contains(&format!("authorization: bearer {TOKEN}").to_ascii_lowercase())
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].raw_symbol, "AAPL");
    assert_eq!(rows[0].asset_class(), AssetClass::Equity);
}

#[test]
fn a_crypto_pairs_slash_is_percent_encoded_in_the_path() {
    let body = r#"{"symbol":"BTC/USD","class":"crypto","tradable":true,"price_increment":"1",
                   "min_trade_increment":"0.0001","min_order_size":"0.0001"}"#;
    let (base, seen) = serve(vec![(200, body.to_string())]);
    let rows = fetch_asset(&rest(), &base, "BTC/USD").expect("row");
    assert_eq!(request_line(&seen.lock().expect("log")[0]), "GET /v1/assets/BTC%2FUSD HTTP/1.1");
    assert_eq!(rows[0].raw_symbol, "BTC/USD");
    assert_eq!(rows[0].asset_class(), AssetClass::CryptoSpot);
}

#[test]
fn nothing_but_unreserved_bytes_stays_literal() {
    assert_eq!(path_segment("BRK.B"), "BRK.B");
    assert_eq!(path_segment("BTC/USD"), "BTC%2FUSD");
    assert_eq!(path_segment("A?b#c d"), "A%3Fb%23c%20d");
    assert_eq!(path_segment("../x"), "..%2Fx");
    assert_eq!(path_segment("é"), "%C3%A9");
}

#[test]
fn a_404_is_no_such_asset_and_not_a_failure() {
    let (base, _) =
        serve(vec![(404, r#"{"code":40410000,"message":"asset not found"}"#.to_string())]);
    assert!(fetch_asset(&rest(), &base, "NOPE").expect("an answer").is_empty());
}

#[test]
fn an_untradable_asset_is_an_empty_answer() {
    let body = r#"{"symbol":"HALT","class":"us_equity","tradable":false}"#;
    let (base, _) = serve(vec![(200, body.to_string())]);
    assert!(fetch_asset(&rest(), &base, "HALT").expect("an answer").is_empty());
}

#[test]
fn any_other_status_is_an_error_naming_it_and_no_credential() {
    for (status, replies) in [
        (500, vec![(500, r#"{"message":"boom"}"#.to_string())]),
        (429, vec![(429, "slow down".to_string())]),
        // a 401 is retried once with a fresh token, so it takes two replies
        (401, vec![(401, r#"{"message":"unauthorized."}"#.to_string()); 2]),
    ] {
        let (base, _) = serve(replies);
        let err = fetch_asset(&rest(), &base, "AAPL").expect_err(&status.to_string());
        let text = err.to_string();
        assert!(text.contains(&format!("HTTP {status}")), "{text}");
        assert!(!text.contains(TOKEN) && !text.contains("Bearer"), "{text}");
    }
}

#[test]
fn an_unreachable_token_mint_fails_without_a_request_or_a_secret() {
    let rest = AlpacaRest::new(Arc::new(TokenSource::with_clock_and_exchanger(
        Box::new(|| 0),
        Box::new(|_| Err(AuthError::Transport("io: Connection refused".into()))),
    )));
    let (base, seen) = serve(vec![(200, "{}".to_string())]);
    let err = fetch_asset(&rest, &base, "AAPL").expect_err("no token");
    assert!(err.to_string().contains("Connection refused"), "{err}");
    assert!(seen.lock().expect("log").is_empty());
}

#[test]
fn an_account_without_credentials_is_refused_by_field_name_not_as_an_empty_listing() {
    let catalog = AlpacaCatalog { config: None };
    let err = catalog.asset("AAPL").expect_err("no credentials");
    assert!(err.0.contains("CLIENT_ID") && err.0.contains("ACCOUNT_ID"), "{err}");
}

#[test]
fn the_config_the_catalog_holds_never_prints_its_secret() {
    let fields = vike_secrets::account_credentials::CredentialFields::from_pairs([
        ("CLIENT_ID", "the-client-id"),
        ("CLIENT_SECRET", "the-client-secret"),
        ("ACCOUNT_ID", "acct-1"),
    ]);
    let creds = AccountCredentials::new(Some(1), Some(&fields), None);
    let config = AlpacaCatalog::new(&creds).config.expect("config");
    let shown = format!("{config:?}");
    assert!(!shown.contains("the-client-secret") && !shown.contains("the-client-id"), "{shown}");
}
