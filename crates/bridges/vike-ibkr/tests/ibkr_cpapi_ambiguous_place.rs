//! A cpapi `place` whose OUTCOME IS UNKNOWN must not synthesize a terminal `OrderRejected`: the venue
//! may hold the order (the send-failure bullet of `crates/vike-bridge-core/CLAUDE.md`'s "Local traps":
//! ambiguous unless it provably preceded the write). Only a DEFINITE failure (nothing left this host, or the venue
//! refused) rejects. Offline: a loopback listener plays the CP Gateway (the tickle that `connect`
//! needs answers 200; the order POST is hung up on / 503'd / 400'd / refused per case).
#![cfg(feature = "ibkr-cpapi")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
use vike_ibkr::IbkrConfig;
use vike_ibkr::config::MktDataType;
use vike_ibkr::contract::parse_simplified;
use vike_ibkr::order::IbOrderSpec;
use vike_ibkr::transport::{CpapiRest, CpapiTransport, IbInbound, IbkrTransport};
use vike_model::AccountTier;
use vike_venues::IbkrBackend;

/// What the fake gateway does with the order `POST` (everything else is answered `200 {}`).
#[derive(Clone, Copy)]
enum OnPost {
    HangUp,
    Status(u16, &'static str),
}

/// The open-orders list a gateway that DID take the order answers. ⚠ Hand-written: the row's field
/// name `order_ref` comes from secondary sources (IBKR's own pages were unreadable), not a capture.
const LISTED: &str = r#"{"orders":[{"order_ref":"coid-1"}]}"#;

/// The request line of every `GET .../iserver/account/orders` the fake gateway served, in order.
type OrdersLog = Arc<Mutex<Vec<String>>>;

/// A fake gateway on a loopback port, served until the process ends, and a transport connected to it.
fn gateway(on_post: OnPost) -> (CpapiTransport, u16) {
    let (t, port, _) = gateway_with(on_post, &[LISTED]);
    (t, port)
}

/// Like [`gateway`], with the open-orders replies scripted: the n-th `GET .../orders` is answered
/// `script[n]` (the last entry again once the script runs out), and each request line is logged.
fn gateway_with(
    on_post: OnPost,
    script: &'static [&'static str],
) -> (CpapiTransport, u16, OrdersLog) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log: OrdersLog = Arc::default();
    let served = Arc::clone(&log);
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            // Read the request head plus its declared body, so a hang-up is a clean FIN after a full
            // request (the order was SENT) and not an RST over unread data.
            let head_end = loop {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(p + 4);
                }
            };
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
            let want = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < head_end + want {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let is_order = head.starts_with("post ") && head.contains("/orders");
            let (status, body) = match (is_order, on_post) {
                (true, OnPost::HangUp) => continue, // drop `s`: EOF where the response should be
                (true, OnPost::Status(c, b)) => (c, b),
                _ if head.starts_with("get ") && head.contains("/iserver/account/orders") => {
                    let mut seen = served.lock().unwrap();
                    seen.push(head.lines().next().unwrap_or_default().to_string());
                    (200, script[(seen.len() - 1).min(script.len() - 1)])
                }
                _ => (200, "{}"),
            };
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(reply.as_bytes());
        }
    });
    let cfg = IbkrConfig {
        tier: AccountTier::Demo,
        backend: IbkrBackend::Cpapi,
        host: "127.0.0.1".into(),
        port: 7497,
        client_id: 1,
        account: "DU0000000".into(),
        cpapi_url: format!("http://127.0.0.1:{port}"),
        mktdata_type: MktDataType::Delayed,
        data_client_id: 2,
        recon_client_id: 3,
    };
    (CpapiTransport::connect(&cfg).expect("fake gateway answers the tickle"), port, log)
}

/// Place one order and collect every inbound the transport yields within `window`; returns whether
/// any was an `Error` (the transport's terminal-reject carrier, code 201) for order 101.
fn place_and_see_reject(t: &mut CpapiTransport) -> bool {
    let spec = IbOrderSpec {
        action: "BUY",
        total_qty: 1.0,
        order_type: "LMT",
        lmt_price: Some(1.0),
        aux_price: None,
        tif: "GTC",
        order_ref: "coid-1".into(),
    };
    let mut contract = parse_simplified("AAPL.SMART.USD").unwrap();
    contract.con_id = Some(265598); // skip the secdef lookup: this test is about the place alone
    t.place_order(101, &spec, &contract);
    let mut rejected = false;
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        if let Some(IbInbound::Error { code: 201, order_id: 101, .. }) =
            t.next_recv(Duration::from_millis(50))
        {
            rejected = true;
        }
    }
    rejected
}

#[test]
fn a_hang_up_after_the_post_is_unknown_not_rejected() {
    vike_log::test_init();
    let (mut t, _) = gateway(OnPost::HangUp);
    assert!(
        !place_and_see_reject(&mut t),
        "the POST left this host and no answer came back: the order may be live, so no terminal reject"
    );
}

#[test]
fn a_gateway_503_on_the_post_is_unknown_not_rejected() {
    vike_log::test_init();
    let (mut t, _) = gateway(OnPost::Status(503, "Service Unavailable"));
    assert!(!place_and_see_reject(&mut t), "a gateway 5xx says nothing about the venue's verdict");
}

/// The loopback agent follows no redirect, so a 3xx on the POST reaches the client as a status: it is
/// unknown (as the redirect-replayed GET it replaced always was), never a terminal reject.
#[test]
fn a_3xx_on_the_post_is_unknown_not_rejected() {
    vike_log::test_init();
    let (mut t, port) = gateway(OnPost::Status(302, "Found"));
    assert!(!place_and_see_reject(&mut t), "a redirect says nothing about the venue's verdict");
    let rest = CpapiRest::new(&format!("http://127.0.0.1:{port}"), "DU0000000");
    assert_eq!(rest.place(&serde_json::json!({})).unwrap_err().code, E_TIMEOUT_AMBIGUOUS);
}

/// Place order 101 / `coid-1` and count the `StreamResync`s the transport yields within `window`.
fn resyncs_after_place(t: &mut CpapiTransport, window: Duration) -> usize {
    let spec = IbOrderSpec {
        action: "BUY",
        total_qty: 1.0,
        order_type: "LMT",
        lmt_price: Some(1.0),
        aux_price: None,
        tif: "GTC",
        order_ref: "coid-1".into(),
    };
    let mut contract = parse_simplified("AAPL.SMART.USD").unwrap();
    contract.con_id = Some(265598);
    t.place_order(101, &spec, &contract);
    let mut n = 0;
    let end = Instant::now() + window;
    while Instant::now() < end {
        if let Some(IbInbound::StreamResync) = t.next_recv(Duration::from_millis(50)) {
            n += 1;
        }
    }
    n
}

/// An UNKNOWN place emits no terminal, and nothing else would ever look for the order: the venue
/// has no coid lookup, reconcile folds nothing, so without ONE resync the stuck-order watchdog
/// rejects a live order after about a minute. The resync is the exec loop's existing answer to
/// `StreamResync` (`request_open_orders`, then `rebind_open_order` accepts the coid).
#[test]
fn an_unknown_place_asks_for_one_resync_so_a_live_order_is_found() {
    vike_log::test_init();
    let (mut t, _) = gateway(OnPost::HangUp);
    assert_eq!(
        resyncs_after_place(&mut t, Duration::from_millis(3500)),
        1,
        "exactly one, not a loop"
    );
    t.request_open_orders();
    let mut found = false;
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        found |= matches!(
            t.next_recv(Duration::from_millis(50)),
            Some(IbInbound::OpenOrder { order_id: 101, ref order_ref }) if order_ref == "coid-1"
        );
    }
    assert!(found, "the open-orders row carries the coid back to order 101");
}

/// Ask the transport for its open orders and return the coids of the `OpenOrder` rows it yields.
fn open_order_refs(t: &mut CpapiTransport) -> Vec<String> {
    t.request_open_orders();
    let mut refs = Vec::new();
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        if let Some(IbInbound::OpenOrder { order_ref, .. }) = t.next_recv(Duration::from_millis(50))
        {
            refs.push(order_ref);
        }
    }
    refs
}

/// The documented cold-cache behaviour (secondary sources): the first `GET /iserver/account/orders`
/// of a session can answer an empty list, `?force=true` clears the cache (and itself answers an
/// empty array), and the call after it is the real one. So an EMPTY first reply earns exactly one
/// forced call and one more plain call, three in all, never a loop.
const NO_ROWS: &str = r#"{"orders":[],"snapshot":true}"#;

#[test]
fn an_empty_first_reply_is_followed_by_one_forced_call_and_one_plain_call() {
    vike_log::test_init();
    let (mut t, _, log) = gateway_with(OnPost::HangUp, &[NO_ROWS, "[]", LISTED]);
    assert_eq!(open_order_refs(&mut t), ["coid-1"], "the third call's row is decoded");
    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "plain, forced, plain: {seen:?}");
    assert!(!seen[0].contains("force"), "{seen:?}");
    assert!(seen[1].contains("force=true"), "{seen:?}");
    assert!(!seen[2].contains("force"), "{seen:?}");
}

#[test]
fn a_gateway_that_stays_empty_is_asked_three_times_and_no_more() {
    vike_log::test_init();
    let (mut t, _, log) = gateway_with(OnPost::HangUp, &[NO_ROWS]);
    assert!(open_order_refs(&mut t).is_empty());
    assert_eq!(log.lock().unwrap().len(), 3, "bounded: one plain, one forced, one plain");
}

#[test]
fn a_non_empty_first_reply_is_the_only_call() {
    vike_log::test_init();
    let (mut t, _, log) = gateway_with(OnPost::HangUp, &[LISTED]);
    assert_eq!(open_order_refs(&mut t), ["coid-1"]);
    assert_eq!(log.lock().unwrap().len(), 1, "no forced call when the list already has rows");
}

#[test]
fn a_definite_refusal_asks_for_no_resync() {
    vike_log::test_init();
    let (mut t, _) = gateway(OnPost::Status(400, r#"{"error":"Order rejected"}"#));
    assert_eq!(resyncs_after_place(&mut t, Duration::from_millis(3000)), 0);
}

/// The gateway's own words on a gateway-style 5xx travel in the error (the operator reads them in
/// the warning), bounded; the verdict stays "unknown": no repo evidence says what such a body means.
#[test]
fn a_gateway_5xx_on_a_write_keeps_a_bounded_head_of_its_body_and_stays_unknown() {
    vike_log::test_init();
    let long = format!("no brokerage session {}", "x".repeat(5000));
    let leaked: &'static str = Box::leak(long.into_boxed_str());
    let (_t, port) = gateway(OnPost::Status(503, leaked));
    let rest = CpapiRest::new(&format!("http://127.0.0.1:{port}"), "DU0000000");
    let e = rest.place(&serde_json::json!({})).unwrap_err();
    assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS);
    assert!(e.msg.contains("no brokerage session"), "{}", e.msg);
    assert!(e.msg.len() < 1000, "the head is bounded: {} bytes", e.msg.len());
}

#[test]
fn a_2xx_reply_with_an_unreadable_body_is_unknown_not_rejected() {
    vike_log::test_init();
    // The gateway took the request (2xx) but what came back is not JSON (an HTML error page, a
    // truncated body): the venue received the order, so its fate is unknown.
    for body in ["<html>Bad Gateway</html>", "", r#"[{"order_id":"#] {
        let (mut t, _) = gateway(OnPost::Status(200, body));
        assert!(
            !place_and_see_reject(&mut t),
            "a 2xx whose body cannot be read left the order possibly live: {body:?}"
        );
    }
}

#[test]
fn a_venue_4xx_refusal_still_rejects() {
    vike_log::test_init();
    let (mut t, _) = gateway(OnPost::Status(400, r#"{"error":"Order rejected"}"#));
    assert!(place_and_see_reject(&mut t), "a definite venue refusal keeps its terminal reject");
}

#[test]
fn a_refused_connection_still_rejects() {
    vike_log::test_init();
    // A gateway that answered the tickle and then went away: the order's connect is refused, a
    // definite failure that provably preceded the write.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        // The WS pump may dial first; answer everything until the tickle has been answered.
        loop {
            let (mut s, _) = listener.accept().unwrap();
            let mut b = [0u8; 4096];
            let n = s.read(&mut b).unwrap_or(0);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
            if String::from_utf8_lossy(&b[..n]).contains("/tickle") {
                break;
            }
        }
        // `listener` drops here: the port now refuses.
    });
    let cfg = IbkrConfig {
        tier: AccountTier::Demo,
        backend: IbkrBackend::Cpapi,
        host: "127.0.0.1".into(),
        port: 7497,
        client_id: 1,
        account: "DU0000000".into(),
        cpapi_url: format!("http://127.0.0.1:{port}"),
        mktdata_type: MktDataType::Delayed,
        data_client_id: 2,
        recon_client_id: 3,
    };
    let mut t = CpapiTransport::connect(&cfg).expect("tickle answered");
    server.join().unwrap();
    assert!(place_and_see_reject(&mut t), "connection refused is a definite failure: it rejects");
}
