//! Proves `vike_alpaca::execution::spawn` wires the real `ExecutionClientActor` + `AlpacaTransport` path:
//! `submit` emits `OrderSubmitted` synchronously, and a transport failure (an unroutable host)
//! yields a terminal `OrderRejected` — the venue-adapter contract that an order intent never
//! silently vanishes. Mirrors OANDA's `execution_actor_wiring.rs` shape but drives the REAL network
//! path against a dead host rather than a fake command loop, since Alpaca's OAuth token
//! exchange + order POST are both exercised end-to-end here.

use crate::common::kept_rows;

use tokio::sync::mpsc;
use vike_alpaca::execution::spawn;
use vike_execution::ExecutionClient;
use vike_execution::{EventSender, Ingest};
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest, TimeInForce};
use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};

fn make_config() -> vike_alpaca::AlpacaConfig {
    // Point hosts at an unroutable base so the POST fails fast → terminal OrderRejected.
    let fields = CredentialFields::from_pairs([
        ("CLIENT_ID", "id"),
        ("CLIENT_SECRET", "sec"),
        ("ACCOUNT_ID", "acct"),
    ]);
    let creds = AccountCredentials::new(Some(1), Some(&fields), None);
    let seed = vike_venues::venue_urls::VenueUrls::seed("alpaca");
    let mut c = vike_alpaca::load_alpaca_config_from(AccountTier::Demo, &creds, &seed)
        .unwrap()
        .expect("the seed's sandbox rows");
    c.hosts.authx = "http://127.0.0.1:9".into();
    c.hosts.broker = "http://127.0.0.1:9".into();
    c
}

fn order() -> OrderRequest {
    OrderRequest {
        order_client_id: "c1".into(),
        venue: "alpaca".into(),
        symbol: "AAPL".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        price: None,
        trigger_price: None,
        time_in_force: TimeInForce::Day,
        timestamp: 1,
        ..Default::default()
    }
}

/// A loopback Alpaca: it mints a token, CLOSES THE SOCKET on an order POST with the request read and no answer (the order may
/// be live), and answers everything else 404. Its base url, and the body of every order POST it read.
///
/// Every request is read WHOLE (head plus its declared body) before the answer or the hang-up. One `read` could return the
/// head alone, and closing over the unread form body of the token mint sent an RST that flushed the mint's 200 out of the
/// client's buffer: "token mint unreachable: Connection reset by peer", a reject before any order was sent.
fn venue_that_hangs_up_on_the_order() -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let base = format!("http://{}", listener.local_addr().expect("resolve assigned port"));
    let (posted, bodies) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok((mut sock, _)) = listener.accept() {
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            let head_end = loop {
                let n = sock.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break None;
                }
                buffer.extend_from_slice(&chunk[..n]);
                if let Some(p) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(p + 4);
                }
            };
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
            let want = head
                .to_ascii_lowercase()
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            while buffer.len() < head_end + want {
                let n = sock.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..n]);
            }
            let line = head.lines().next().unwrap_or_default();
            if line.starts_with("POST") && line.contains("/orders") {
                let _ = posted.send(String::from_utf8_lossy(&buffer[head_end..]).into_owned());
                continue;
            }
            let body = if line.contains("/oauth2/token") {
                r#"{"access_token":"t","expires_in":900}"#
            } else {
                "{}"
            };
            let status = if line.contains("/oauth2/token") { "200 OK" } else { "404 X" };
            let _ = write!(
                sock,
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, bodies)
}

/// **A coin's USER name is ordered under its kept row's spelling** (decision 0290, audit flag ALP-1): `BTCUSD` posts
/// `BTC/USD`, and a share `AAPL` posts `AAPL`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_name_is_ordered_under_its_kept_rows_spelling() {
    kept_rows::hold_kept_rows();
    let (base, bodies) = venue_that_hangs_up_on_the_order();
    let mut config = make_config();
    config.hosts.authx = base.clone();
    config.hosts.broker = base;
    let (tx, _rx) = mpsc::channel::<Ingest>(64);
    let mut client = spawn(config, EventSender { ingest: tx, route_key: None });
    for (order_client_id, user, wire) in [("coin", "BTCUSD", "BTC/USD"), ("share", "AAPL", "AAPL")]
    {
        client.submit(&OrderRequest {
            order_client_id: order_client_id.into(),
            symbol: user.into(),
            ..order()
        });
        let body = tokio::task::block_in_place(|| {
            bodies.recv_timeout(std::time::Duration::from_secs(5)).expect("the order POST")
        });
        let body: serde_json::Value = serde_json::from_str(&body).expect("a JSON body");
        assert_eq!(body["symbol"], wire, "{user}");
    }
    client.detach();
}

/// Every POST failure used to be a terminal `OrderRejected`, because `send_body` never classified one: a reply lost after
/// the write rejected an order the venue may hold. It stays `Submitted` now, for the SSE lane.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_post_whose_reply_was_lost_stays_submitted() {
    kept_rows::hold_kept_rows();
    let (base, _bodies) = venue_that_hangs_up_on_the_order();
    let mut config = make_config();
    config.hosts.authx = base.clone();
    config.hosts.broker = base;
    let (tx, mut rx) = mpsc::channel::<Ingest>(64);
    let mut client = spawn(config, EventSender { ingest: tx, route_key: None });
    client.submit(&order());

    let mut saw_submitted = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
            Ok(Some(Ingest::Event(Event::OrderSubmitted(s)))) if s.order_client_id == "c1" => {
                saw_submitted = true
            }
            Ok(Some(Ingest::Event(Event::OrderRejected(r)))) if r.order_client_id == "c1" => {
                panic!("a terminal reject over an order the venue may hold: {}", r.reason)
            }
            _ => {}
        }
    }
    assert!(saw_submitted, "OrderSubmitted not observed");
    client.detach();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_emits_submitted_then_rejected_on_dead_host() {
    kept_rows::hold_kept_rows();
    let (tx, mut rx) = mpsc::channel::<Ingest>(64);
    let mut client = spawn(make_config(), EventSender { ingest: tx, route_key: None });
    client.submit(&order());

    let mut saw_submitted = false;
    let mut saw_rejected = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !(saw_submitted && saw_rejected) {
        match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
            Ok(Some(Ingest::Event(Event::OrderSubmitted(s)))) if s.order_client_id == "c1" => {
                saw_submitted = true
            }
            Ok(Some(Ingest::Event(Event::OrderRejected(r)))) if r.order_client_id == "c1" => {
                saw_rejected = true
            }
            Ok(Some(_)) => {}
            _ => {}
        }
    }
    assert!(saw_submitted, "OrderSubmitted not observed");
    assert!(saw_rejected, "OrderRejected (dead host) not observed");
    client.detach();
}
