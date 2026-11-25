//! A cpapi `place` whose OUTCOME IS UNKNOWN must not synthesize a terminal `OrderRejected`: the venue
//! may hold the order (the send-failure bullet of `crates/vike-bridge-core/CLAUDE.md`'s "Local traps":
//! ambiguous unless it provably preceded the write). Only a DEFINITE failure (nothing left this host, or the venue
//! refused) rejects, and every answer of the reply chain is read, the last one too. Offline: a loopback listener
//! plays the CP Gateway (the tickle that `connect` needs answers 200; the order POST is hung up on / 500'd / 503'd /
//! 400'd / refused / answered with a chain of confirmation questions per case).
#![cfg(feature = "ibkr-cpapi")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
use vike_ibkr::IbkrConfig;
use vike_ibkr::config::MarketDataType;
use vike_ibkr::execution::order::IbkrOrderSpec;
use vike_ibkr::instrument::contract::parse_simplified;
use vike_ibkr::transport::{CpapiRest, IbkrInbound, IbkrTransport, IbkrTransportCpapi};
use vike_model::AccountTier;
use vike_venues::IbkrBackend;

/// What the fake gateway does with the order `POST` (everything else is answered `200 {}`).
#[derive(Clone, Copy)]
enum OnPost {
    HangUp,
    Status(u16, &'static str),
    /// The order `POST` and every `/iserver/reply/{id}` answer a confirmation question ([`QUESTION`])
    /// until this many replies were sent; that reply's answer is the accept ([`ACCEPTED`]).
    AcceptAtReply(usize),
}

/// A confirmation question and an acceptance, in the shapes LIVE-VERIFIED against a paper CP Gateway
/// (2026-07-15, `crates/bridges/vike-ibkr/src/transport/cpapi/reply.rs`'s module doc).
const QUESTION: &str = r#"[{"id":"q-1","message":["Are you sure you want to submit this order?"],"messageIds":["o163"]}]"#;
const ACCEPTED: &str = r#"[{"order_id":"1794719002","order_status":"PreSubmitted"}]"#;

/// The open-orders list a gateway that DID take the order answers. ⚠ Hand-written: the row's field
/// name `order_ref` is the live-proven one (paper, 2026-10-17); the other row fields are omitted.
const LISTED: &str = r#"{"orders":[{"order_ref":"order-client-id-1"}]}"#;

/// The request line of every `GET .../iserver/account/orders` and `POST .../iserver/reply/{id}` the fake
/// gateway served, in order.
type OrdersLog = Arc<Mutex<Vec<String>>>;

/// A fake gateway on a loopback port, served until the process ends, and a transport connected to it.
fn gateway(on_post: OnPost) -> (IbkrTransportCpapi, u16) {
    let (t, port, _) = gateway_with(on_post, &[LISTED]);
    (t, port)
}

/// Like [`gateway`], with the open-orders replies scripted: the n-th `GET .../orders` is answered
/// `script[n]` (the last entry again once the script runs out), and each request line is logged.
fn gateway_with(
    on_post: OnPost,
    script: &'static [&'static str],
) -> (IbkrTransportCpapi, u16, OrdersLog) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log: OrdersLog = Arc::default();
    let served = Arc::clone(&log);
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 1024];
            // Read the request head plus its declared body, so a hang-up is a clean FIN after a full
            // request (the order was SENT) and not an RST over unread data.
            let head_end = loop {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break None;
                }
                buffer.extend_from_slice(&chunk[..n]);
                if let Some(p) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(p + 4);
                }
            };
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&buffer[..head_end]).to_ascii_lowercase();
            let want = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buffer.len() < head_end + want {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..n]);
            }
            let is_order = head.starts_with("post ") && head.contains("/orders");
            let is_reply = head.starts_with("post ") && head.contains("/iserver/reply/");
            let (status, body) = match (is_order, on_post) {
                (true, OnPost::HangUp) => continue, // drop `s`: EOF where the response should be
                (true, OnPost::Status(c, b)) => (c, b),
                (true, OnPost::AcceptAtReply(_)) => (200, QUESTION),
                (false, OnPost::AcceptAtReply(at)) if is_reply => {
                    let mut seen = served.lock().unwrap();
                    seen.push(head.lines().next().unwrap_or_default().to_string());
                    let replies =
                        seen.iter().filter(|line| line.contains("/iserver/reply/")).count();
                    (200, if replies < at { QUESTION } else { ACCEPTED })
                }
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
    let config = IbkrConfig {
        tier: AccountTier::Demo,
        backend: IbkrBackend::Cpapi,
        host: "127.0.0.1".into(),
        port: 7497,
        client_id: 1,
        account: "DU0000000".into(),
        cpapi_url: format!("http://127.0.0.1:{port}"),
        market_data_type: MarketDataType::Delayed,
        data_client_id: 2,
        reconcile_client_id: 3,
        extended_hours: false,
    };
    (
        IbkrTransportCpapi::connect(&config, None).expect("fake gateway answers the tickle"),
        port,
        log,
    )
}

/// Place order 101 / `order-client-id-1` and collect every inbound the transport yields within `window`.
fn place_and_collect(t: &mut IbkrTransportCpapi, window: Duration) -> Vec<IbkrInbound> {
    let spec = IbkrOrderSpec {
        action: "BUY",
        total_qty: 1.0,
        order_type: "LMT",
        lmt_price: Some(1.0),
        aux_price: None,
        tif: "GTC",
        order_ref: "order-client-id-1".into(),
        display_size: None,
        lmt_price_offset: None,
        activation: None,
        ..Default::default()
    };
    let mut contract = parse_simplified("AAPL.SMART.USD").unwrap();
    contract.con_id = Some(265598); // skip the secdef lookup: this test is about the place alone
    t.place_order(101, &spec, &contract);
    let mut seen = Vec::new();
    let end = Instant::now() + window;
    while Instant::now() < end {
        seen.extend(t.next_recv(Duration::from_millis(50)));
    }
    seen
}

/// Place one order and return whether any inbound within 400 ms was an `Error` (the transport's
/// terminal-reject carrier, code 201) for order 101.
fn place_and_see_reject(t: &mut IbkrTransportCpapi) -> bool {
    place_and_collect(t, Duration::from_millis(400))
        .iter()
        .any(|inbound| matches!(inbound, IbkrInbound::Error { code: 201, order_id: 101, .. }))
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

/// **A bare 500 on the POST is unknown too** (owner ruling B-1): the gateway's error body carries no code of its own,
/// so nothing says IBKR did not take the order before it failed.
#[test]
fn a_bare_500_on_the_post_is_unknown_not_rejected() {
    vike_log::test_init();
    let (mut t, port) = gateway(OnPost::Status(500, r#"{"error":"internal error"}"#));
    assert!(!place_and_see_reject(&mut t), "a 5xx says nothing about the venue's verdict");
    let rest = CpapiRest::new(&format!("http://127.0.0.1:{port}"), "DU0000000");
    assert_eq!(rest.place(&serde_json::json!({})).unwrap_err().code, E_TIMEOUT_AMBIGUOUS);
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

/// Place order 101 / `order-client-id-1` and count the `StreamResync`s the transport yields within `window`.
fn resyncs_after_place(t: &mut IbkrTransportCpapi, window: Duration) -> usize {
    place_and_collect(t, window)
        .iter()
        .filter(|inbound| matches!(inbound, IbkrInbound::StreamResync))
        .count()
}

/// **The answer to the LAST reply is read.** A chain of eight questions is accepted at the eighth reply; that
/// answer used to be dropped and the live order rejected ("exceeded MAX_REPLY_HOPS").
#[test]
fn an_accept_at_the_last_allowed_reply_is_accepted_not_rejected() {
    vike_log::test_init();
    let (mut t, _, log) = gateway_with(OnPost::AcceptAtReply(8), &[LISTED]);
    let seen = place_and_collect(&mut t, Duration::from_millis(400));
    assert!(
        seen.iter().any(|inbound| matches!(
            inbound,
            IbkrInbound::OrderStatus(s) if s.status == "PreSubmitted" && s.order_ref == "order-client-id-1"
        )),
        "the gateway's accept is the order's accept"
    );
    assert!(!seen.iter().any(|inbound| matches!(inbound, IbkrInbound::Error { .. })), "no reject");
    assert_eq!(log.lock().unwrap().iter().filter(|l| l.contains("/iserver/reply/")).count(), 8);
}

/// A chain longer than the bound leaves its last question unanswered, so the gateway placed nothing: that is a
/// definite reject, after exactly `MAX_REPLY_HOPS` replies.
#[test]
fn a_question_left_unanswered_past_the_bound_rejects_after_eight_replies() {
    vike_log::test_init();
    let (mut t, _, log) = gateway_with(OnPost::AcceptAtReply(9), &[LISTED]);
    assert!(place_and_see_reject(&mut t), "the order was never placed");
    assert_eq!(log.lock().unwrap().iter().filter(|l| l.contains("/iserver/reply/")).count(), 8);
}

/// A 2xx order answer in a shape the bridge cannot read (no question, no acceptance, no stated `error`) says nothing
/// about the order: unknown, so no terminal reject and the one resync that finds a received order.
#[test]
fn a_2xx_answer_of_no_known_shape_is_unknown_not_rejected() {
    vike_log::test_init();
    for body in ["{}", "[]", r#"[{"order_status":"Submitted"}]"#] {
        let (mut t, _) = gateway(OnPost::Status(200, body));
        let seen = place_and_collect(&mut t, Duration::from_millis(2600));
        assert!(!seen.iter().any(|i| matches!(i, IbkrInbound::Error { .. })), "{body}: a reject");
        assert_eq!(
            seen.iter().filter(|i| matches!(i, IbkrInbound::StreamResync)).count(),
            1,
            "{body}: one resync"
        );
    }
}

/// **IB-7: the accept row is read by its own status word.** A row that numbers the order with a WORKING word is the accept,
/// in that word; one with any other word (an end, a fill, a word IB documents for no working order) or none is neither an
/// accept nor a reject: the order's state is unknown and the one resync reads it (its row accepts it, its execution
/// books the fill). Every accept used to go out as `PreSubmitted` whatever the row said.
#[test]
fn an_accept_row_is_read_by_its_status_word() {
    vike_log::test_init();
    let working = r#"[{"order_id":"1794719002","order_status":"Submitted"}]"#;
    let (mut t, _) = gateway(OnPost::Status(200, working));
    let seen = place_and_collect(&mut t, Duration::from_millis(400));
    assert!(
        seen.iter().any(|i| matches!(i, IbkrInbound::OrderStatus(s) if s.status == "Submitted")),
        "a working word is the accept, in that word"
    );
    for body in [
        r#"[{"order_id":"1794719002","order_status":"Inactive"}]"#,
        r#"[{"order_id":"1794719002","order_status":"Filled"}]"#,
        r#"[{"order_id":"1794719002"}]"#,
    ] {
        let (mut t, _) = gateway(OnPost::Status(200, body));
        let seen = place_and_collect(&mut t, Duration::from_millis(2600));
        assert!(
            !seen
                .iter()
                .any(|i| matches!(i, IbkrInbound::OrderStatus(_) | IbkrInbound::Error { .. })),
            "{body}: neither an accept nor a reject"
        );
        assert_eq!(
            seen.iter().filter(|i| matches!(i, IbkrInbound::StreamResync)).count(),
            1,
            "{body}: one resync"
        );
    }
}

/// An UNKNOWN place emits no terminal, and nothing else would ever look for the order: the venue
/// has no order client id lookup, reconcile folds nothing, so without ONE resync the stuck-order watchdog
/// rejects a live order after about a minute. The resync is the exec loop's existing answer to
/// `StreamResync` (`request_open_orders`, then `rebind_open_order` accepts the order client id).
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
            Some(IbkrInbound::OpenOrder { order_id: 101, ref order_ref }) if order_ref == "order-client-id-1"
        );
    }
    assert!(found, "the open-orders row carries the order client id back to order 101");
}

/// Ask the transport for its open orders and return the order client ids of the `OpenOrder` rows it yields.
fn open_order_refs(t: &mut IbkrTransportCpapi) -> Vec<String> {
    t.request_open_orders();
    let mut refs = Vec::new();
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        if let Some(IbkrInbound::OpenOrder { order_ref, .. }) =
            t.next_recv(Duration::from_millis(50))
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
    assert_eq!(open_order_refs(&mut t), ["order-client-id-1"], "the third call's row is decoded");
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
    assert_eq!(open_order_refs(&mut t), ["order-client-id-1"]);
    assert_eq!(log.lock().unwrap().len(), 1, "no forced call when the list already has rows");
}

#[test]
fn a_definite_refusal_asks_for_no_resync() {
    vike_log::test_init();
    let (mut t, _) = gateway(OnPost::Status(400, r#"{"error":"Order rejected"}"#));
    assert_eq!(resyncs_after_place(&mut t, Duration::from_millis(3000)), 0);
}

/// The gateway's own `{"error": …}` sentence on a gateway-style 5xx travels in the error (the operator
/// reads it in the warning); any other body is only its size, since a page can echo the request; the
/// verdict stays "unknown": no repo evidence says what such a body means.
#[test]
fn a_gateway_5xx_on_a_write_keeps_its_sentence_not_its_body_and_stays_unknown() {
    vike_log::test_init();
    let page: &'static str =
        Box::leak(format!("no brokerage session {}", "x".repeat(5000)).into_boxed_str());
    for (body, words) in [
        (r#"{"error":"no brokerage session"}"#, "no brokerage session"),
        (page, "512-byte body not shown"),
    ] {
        let (_t, port) = gateway(OnPost::Status(503, body));
        let rest = CpapiRest::new(&format!("http://127.0.0.1:{port}"), "DU0000000");
        let e = rest.place(&serde_json::json!({})).unwrap_err();
        assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS);
        assert_eq!(
            e.msg,
            format!("cpapi network error: gateway http 503, outcome unknown: {words}")
        );
    }
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
    let config = IbkrConfig {
        tier: AccountTier::Demo,
        backend: IbkrBackend::Cpapi,
        host: "127.0.0.1".into(),
        port: 7497,
        client_id: 1,
        account: "DU0000000".into(),
        cpapi_url: format!("http://127.0.0.1:{port}"),
        market_data_type: MarketDataType::Delayed,
        data_client_id: 2,
        reconcile_client_id: 3,
        extended_hours: false,
    };
    let mut t = IbkrTransportCpapi::connect(&config, None).expect("tickle answered");
    server.join().unwrap();
    assert!(place_and_see_reject(&mut t), "connection refused is a definite failure: it rejects");
}
