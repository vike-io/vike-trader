//! On the CLIENT PORTAL backend a cancel or amend addresses the gateway's OWN order id, never ours: a restored order
//! (taught by the host after a restart) and a resynced one get it from the gateway's order list by our order client id
//! (`IbkrTransport::locate_restored`, `request_open_orders`), and an order whose id no list named is NOT SENT, in
//! words. Until that change the cancel of a resynced order went to `DELETE …/order/-1` (the resync's synthetic id) with
//! a `warn!` only. A cancel the gateway REFUSES (a 4xx, or an `error` in a 2xx body) is an `OrderCancelRejected` in the
//! gateway's words. The gateway's ACK takes the cancel and is not the order's end (IB-2): the order list read back
//! after it ends the order when its row says `Cancelled`. An outcome the gateway did not state (a lost answer, a 5xx)
//! is OUTCOME UNKNOWN, read back the same way (IB-3), and an amend of unknown outcome is read back by the asked terms
//! (IB-4); nothing is sent again (decision 0303). Offline: a loopback listener plays the CP Gateway and logs every
//! request line; the client tests run the real exec loop over the real `IbkrTransportCpapi`.
#![cfg(all(feature = "ibkr-cpapi", feature = "test-support"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vike_bridge_core::execution_drive::drain;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_execution::{ExecutionClient, RestoredOrder, event_channel};
use vike_ibkr::IbkrConfig;
use vike_ibkr::client_for_test;
use vike_ibkr::config::MarketDataType;
use vike_ibkr::execution::order::IbkrOrderSpec;
use vike_ibkr::instrument::contract::parse_simplified;
use vike_ibkr::transport::{AmendOutcome, IbkrInbound, IbkrTransport, IbkrTransportCpapi};
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRefusalId};
use vike_venues::IbkrBackend;

/// The order list naming our order the way the live row of 2026-10-17 does (`order_ref`, a numeric `orderId`).
const LISTED: &str = r#"{"orders":[{"order_ref":"order-client-id-r","orderId":1911375756,"order_ccp_status":"Submitted"}]}"#;
/// [`LISTED`]'s row once IB enacted the cancel. ⚠ Hand-written: no cancelled row was captured; the word is IB's
/// documented `Cancelled`, in the live row's `order_ccp_status` field.
const LISTED_CANCELLED: &str = r#"{"orders":[{"order_ref":"order-client-id-r","orderId":1911375756,"order_ccp_status":"Cancelled"}]}"#;
/// The same row without its `orderId`.
const NO_ID: &str =
    r#"{"orders":[{"order_ref":"order-client-id-r","order_ccp_status":"Submitted"}]}"#;
/// [`LISTED`]'s order as a LIMIT row with its terms, in the live row's fields (`conid`, `orderType`, `price`,
/// `sizeAndFills`, `remainingQuantity`, `filledQuantity`): buy 1 at 1.00.
const LIMIT_1_AT_1: &str = r#"{"orders":[{"acct":"DU0000000","conid":265598,"order_ref":"order-client-id-r","orderId":1911375756,"orderType":"Limit","price":1.0,"sizeAndFills":"0/1","remainingQuantity":1.0,"filledQuantity":0.0,"order_ccp_status":"Submitted"}]}"#;
/// The same order holding the amended terms: 2 at 1.50.
const LIMIT_2_AT_1_5: &str = r#"{"orders":[{"acct":"DU0000000","conid":265598,"order_ref":"order-client-id-r","orderId":1911375756,"orderType":"Limit","price":1.5,"sizeAndFills":"0/2","remainingQuantity":2.0,"filledQuantity":0.0,"order_ccp_status":"Submitted"}]}"#;
/// The cancel the gateway is owed for [`LISTED`]'s order.
const CANCEL_BY_ITS_ID: &str = "DELETE /v1/api/iserver/account/DU0000000/order/1911375756 HTTP/1.1";

/// The request line of every request the fake gateway served, in order.
type Requests = Arc<Mutex<Vec<String>>>;

/// What the fake gateway does with an order write (a cancel's `DELETE`, an amend's `POST …/order/{id}`).
#[derive(Clone, Copy)]
enum OnWrite {
    /// Answer with this status and body.
    Answer(u16, &'static str),
    /// Read the request and close the connection with no answer.
    HangUp,
}

/// The live cancel ack (`transport/cpapi/endpoints.rs`'s module doc table, 2026-08-23).
const CANCEL_ACK: OnWrite =
    OnWrite::Answer(200, r#"{"msg":"Request was submitted","order_id":1911375756}"#);

/// [`gateway_with`] whose cancels are acknowledged and whose list shows the order cancelled after one.
fn gateway(orders: &'static str) -> (IbkrTransportCpapi, Requests) {
    gateway_with(orders, CANCEL_ACK, LISTED_CANCELLED)
}

/// A fake gateway on a loopback port, served until the process ends: `GET …/iserver/account/orders` answers
/// `orders`, and `after` once an order write was served; an order write does `on_write`; everything else answers
/// `200 {}`. And a transport connected to it.
fn gateway_with(
    orders: &'static str,
    on_write: OnWrite,
    after: &'static str,
) -> (IbkrTransportCpapi, Requests) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log: Requests = Arc::default();
    let served = Arc::clone(&log);
    let wrote = AtomicBool::new(false);
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 1024];
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
            let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
            let want = head
                .to_ascii_lowercase()
                .lines()
                .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().to_string()))
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            while buffer.len() < head_end + want {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..n]);
            }
            let line = head.lines().next().unwrap_or_default().to_string();
            let order_write = line.starts_with("DELETE ")
                || (line.starts_with("POST ") && line.contains("/order/"));
            let (status, body) = match on_write {
                _ if line.starts_with("GET /v1/api/iserver/account/orders") => {
                    (200, if wrote.load(Ordering::SeqCst) { after } else { orders })
                }
                OnWrite::HangUp if order_write => {
                    wrote.store(true, Ordering::SeqCst);
                    served.lock().unwrap().push(line);
                    continue; // drop `s`: EOF where the answer should be
                }
                OnWrite::Answer(status, body) if order_write => {
                    wrote.store(true, Ordering::SeqCst);
                    (status, body)
                }
                _ => (200, "{}"),
            };
            served.lock().unwrap().push(line);
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
    (IbkrTransportCpapi::connect(&config, None).expect("fake gateway answers the tickle"), log)
}

/// The cancel and amend requests the gateway saw (a `DELETE`, or a `POST` to one order).
fn order_writes(log: &Requests) -> Vec<String> {
    let seen = log.lock().unwrap();
    seen.iter()
        .filter(|l| l.starts_with("DELETE ") || (l.starts_with("POST ") && l.contains("/order/")))
        .cloned()
        .collect()
}

/// How many times the gateway's order list was read.
fn list_reads(log: &Requests) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|l| l.starts_with("GET /v1/api/iserver/account/orders"))
        .count()
}

/// Ask for the open orders and return the numeric id the row of `order-client-id-r` came back under.
fn resynced_id(t: &mut IbkrTransportCpapi) -> i32 {
    t.request_open_orders();
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        if let Some(IbkrInbound::OpenOrder { order_id, order_ref }) =
            t.next_recv(Duration::from_millis(50))
            && order_ref == "order-client-id-r"
        {
            return order_id;
        }
    }
    panic!("the open-orders row of order-client-id-r never came back");
}

/// The order id the transport reports cancelled within `within`, if any (the read back of a cancel's end).
fn reported_cancelled(t: &mut IbkrTransportCpapi, within: Duration) -> Option<i32> {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if let Some(IbkrInbound::OrderStatus(s)) = t.next_recv(Duration::from_millis(50))
            && s.status == "Cancelled"
        {
            return Some(s.order_id);
        }
    }
    None
}

/// Whether the transport asks for a resync (the read back it waits for) within `within`.
fn asks_for_a_resync(t: &mut IbkrTransportCpapi, within: Duration) -> bool {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if let Some(IbkrInbound::StreamResync) = t.next_recv(Duration::from_millis(50)) {
            return true;
        }
    }
    false
}

/// A restored order taught and found in the list, the platform's cancel of it run through the real exec loop (the
/// gateway answers it with `on_write`, and lists the order `after` it), and up to `n` events the platform heard within
/// `wait`.
fn cancel_a_restored_order(
    on_write: OnWrite,
    after: &'static str,
    n: usize,
    wait: Duration,
) -> (Vec<Event>, Requests) {
    let (t, log) = gateway_with(LISTED, on_write, after);
    let (events, mut rx) = event_channel(64);
    let mut client = client_for_test(events, Box::new(t));
    client.adopt_restored(&[RestoredOrder::new("order-client-id-r", "AAPL")]);
    client.cancel("order-client-id-r");
    let got = drain(|| rx.try_recv().ok(), n, wait);
    client.detach();
    (got, log)
}

/// **A restored order is cancelled by the id the gateway's list names**: the host teaches it after a restart, the
/// exec loop asks the transport (`locate_restored`), which finds it in the order list by our order client id; the cancel is a
/// `DELETE` of that id, and the platform hears `OrderCanceled` once the list read back after the gateway's ack says
/// `Cancelled` (IB-2: the ack is not the end).
#[test]
fn a_restored_order_is_cancelled_by_the_gateways_own_id() {
    vike_log::test_init();
    let (got, log) =
        cancel_a_restored_order(CANCEL_ACK, LISTED_CANCELLED, 1, Duration::from_secs(5));
    assert!(
        matches!(got.as_slice(), [Event::OrderCanceled(c)] if c.order_client_id == "order-client-id-r"),
        "{got:?}"
    );
    assert_eq!(order_writes(&log), [CANCEL_BY_ITS_ID]);
}

/// **IB-2: the gateway's ack is not the order's end.** While the list read back still shows the order working, the
/// platform hears nothing: no `OrderCanceled` (the order may still fill), no refusal, and the cancel is not sent again.
#[test]
fn the_gateways_ack_ends_nothing_while_the_list_shows_the_order_working() {
    vike_log::test_init();
    let (got, log) = cancel_a_restored_order(CANCEL_ACK, LISTED, 1, Duration::from_secs(3));
    assert!(got.is_empty(), "{got:?}");
    assert_eq!(order_writes(&log), [CANCEL_BY_ITS_ID], "sent once");
    assert!(list_reads(&log) >= 2, "the locate, then the read back: {:?}", log.lock().unwrap());
}

/// **A cancel the gateway REFUSES is an `OrderCancelRejected` in the gateway's words**, never a `warn!` alone and
/// never an `OrderCanceled`: the order stays live. The case is a restored order the list still names that has
/// finished at the venue. ⚠ Hand-written answer: the status and words a gateway gives for a finished order are
/// not captured.
#[test]
fn a_gateway_refusal_of_a_listed_finished_orders_cancel_is_a_cancel_rejected_in_its_words() {
    vike_log::test_init();
    let refusal = OnWrite::Answer(400, r#"{"error":"Order 1911375756 is already filled"}"#);
    let (got, log) = cancel_a_restored_order(refusal, LISTED, 2, Duration::from_millis(1500));
    assert!(
        matches!(got.as_slice(), [Event::OrderCancelRejected(r)]
            if r.order_client_id == "order-client-id-r"
                && r.reason == "cpapi cancel refused by the gateway (400): Order 1911375756 is already filled"
                && r.refusal_id.as_deref() == Some(OrderRefusalId::VenueRefused.word())),
        "{got:?}"
    );
    assert_eq!(order_writes(&log), [CANCEL_BY_ITS_ID]);
}

/// A refusal the gateway states in a 2xx body (the shape it answered a cancel by our numeric with, 2026-07-15) is a
/// refusal too: it used to be taken for the ack and reported the order cancelled.
#[test]
fn a_refusal_stated_in_a_2xx_body_is_refused_not_taken_for_an_ack() {
    vike_log::test_init();
    let (mut t, _) = gateway_with(
        LISTED,
        OnWrite::Answer(200, r#"{"error":"OrderID 1911375756 doesn't exist"}"#),
        LISTED_CANCELLED,
    );
    let id = resynced_id(&mut t);
    let why = t.cancel_order(id).expect_err("a stated refusal");
    assert_eq!(
        why,
        VenueApiError {
            code: 200,
            msg: "cpapi cancel refused by the gateway: OrderID 1911375756 doesn't exist"
                .to_string()
        }
    );
    assert!(
        !asks_for_a_resync(&mut t, Duration::from_millis(2500)),
        "a refused cancel is not read back"
    );
}

/// **IB-3: an outcome the gateway did not state is OUTCOME UNKNOWN, read back, never sent again**: a lost answer (the
/// gateway read the cancel and hung up) and a gateway 5xx may have cancelled the order or not, so the platform hears
/// the non-terminal advisory that says so (`order.outcome_unknown`, never a refusal), and then the order's end from
/// the list read back.
#[test]
fn an_ambiguous_cancel_outcome_is_unknown_and_read_back() {
    vike_log::test_init();
    for unknown in [OnWrite::HangUp, OnWrite::Answer(503, "Service Unavailable")] {
        let (got, log) =
            cancel_a_restored_order(unknown, LISTED_CANCELLED, 2, Duration::from_secs(5));
        assert!(
            matches!(got.as_slice(), [Event::OrderCancelRejected(r), Event::OrderCanceled(c)]
                if r.order_client_id == "order-client-id-r"
                    && r.refusal_id.as_deref() == Some(OrderRefusalId::OutcomeUnknown.word())
                    && c.order_client_id == "order-client-id-r"),
            "{got:?}"
        );
        assert_eq!(order_writes(&log), [CANCEL_BY_ITS_ID], "the cancel was sent once");
    }
}

/// The defect's own path: a RESYNC numbers an order it never placed `-1`, and its cancel now addresses the id its
/// row names, not `-1`. The ack reports nothing at once; the resync it asks for reads the order's end off the list.
#[test]
fn a_resynced_order_is_cancelled_by_the_id_its_row_names() {
    vike_log::test_init();
    let (mut t, log) = gateway(LISTED);
    let id = resynced_id(&mut t);
    assert_eq!(
        id, -1,
        "an order client id this transport never placed is numbered by a synthetic id"
    );
    assert_eq!(t.cancel_order(id), Ok(()));
    assert_eq!(order_writes(&log), [CANCEL_BY_ITS_ID]);
    assert_eq!(
        reported_cancelled(&mut t, Duration::from_millis(300)),
        None,
        "the ack is not the end"
    );
    assert!(asks_for_a_resync(&mut t, Duration::from_secs(3)), "the read back is asked for");
    t.request_open_orders();
    assert_eq!(reported_cancelled(&mut t, Duration::from_millis(400)), Some(id), "the list's end");
}

/// **No gateway id, no request**: a resynced order whose row names no `orderId`, and an order of ours whose accept
/// was never seen, are NOT SENT, in words, for a cancel and for an amend, and the gateway sees neither.
#[test]
fn with_no_gateway_id_a_cancel_and_an_amend_are_refused_and_nothing_is_sent() {
    vike_log::test_init();
    let (mut t, log) = gateway(NO_ID);
    let id = resynced_id(&mut t);
    assert_eq!(id, -1);
    for order_id in [id, 101] {
        let why = t.cancel_order(order_id).expect_err("no gateway id: refused");
        assert_eq!(why.code, 0, "NOT sent: {why:?}");
        assert!(
            why.msg.starts_with("cpapi cancel NOT sent") && why.msg.contains("client portal"),
            "{why:?}"
        );
    }
    let spec = IbkrOrderSpec {
        action: "BUY",
        total_qty: 1.0,
        order_type: "LMT",
        lmt_price: Some(1.0),
        tif: "GTC",
        order_ref: "order-client-id-r".into(),
        ..Default::default()
    };
    let mut contract = parse_simplified("AAPL.SMART.USD").unwrap();
    contract.con_id = Some(265598);
    match t.modify_order(id, &spec, &contract) {
        AmendOutcome::Refused(why, refusal) => {
            assert!(why.starts_with("cpapi amend NOT sent"), "{why}");
            assert_eq!(refusal, vike_model::OrderRefusalId::NotSent);
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(
        order_writes(&log).is_empty(),
        "nothing addressed to an order: {:?}",
        order_writes(&log)
    );
}

/// **IB-4: an amend whose answer was lost is read back, never called failed.** The gateway read the amend and hung
/// up: when the order list then shows the order holding the asked terms (2 at 1.50) the amend took
/// (`Acknowledged`, `OrderModified`); when it still shows the old ones the outcome stays UNKNOWN (the shared answer's
/// sentinel), never a refusal. The amend is sent once either way.
#[test]
fn an_amend_whose_answer_was_lost_is_read_back_by_the_asked_terms() {
    vike_log::test_init();
    let spec = IbkrOrderSpec {
        action: "BUY",
        total_qty: 2.0,
        order_type: "LMT",
        lmt_price: Some(1.5),
        tif: "GTC",
        order_ref: "order-client-id-r".into(),
        ..Default::default()
    };
    let mut contract = parse_simplified("AAPL.SMART.USD").unwrap();
    contract.con_id = Some(265598);
    for (after, took) in [(LIMIT_2_AT_1_5, true), (LIMIT_1_AT_1, false)] {
        let (mut t, log) = gateway_with(LIMIT_1_AT_1, OnWrite::HangUp, after);
        let id = resynced_id(&mut t);
        match t.modify_order(id, &spec, &contract) {
            AmendOutcome::Acknowledged => assert!(took, "the old terms prove no amend"),
            AmendOutcome::Failed(e) => {
                assert!(!took, "the asked terms are listed: {e:?}");
                assert_eq!(e.code, E_TIMEOUT_AMBIGUOUS, "OUTCOME UNKNOWN, never a refusal: {e:?}");
            }
            other => panic!("expected the read back's answer, got {other:?}"),
        }
        assert_eq!(order_writes(&log).len(), 1, "sent once: {:?}", order_writes(&log));
    }
}

/// A restored order the list does not name with an id is left unbound: its cancel is the exec loop's unknown-order
/// refusal, in words, and nothing reaches the gateway.
#[test]
fn a_restored_order_the_list_gives_no_id_is_refused_and_nothing_is_sent() {
    vike_log::test_init();
    let (t, log) = gateway(NO_ID);
    let (events, mut rx) = event_channel(64);
    let mut client = client_for_test(events, Box::new(t));
    client.adopt_restored(&[RestoredOrder::new("order-client-id-r", "AAPL")]);
    client.cancel("order-client-id-r");
    let got = drain(|| rx.try_recv().ok(), 1, Duration::from_secs(3));
    client.detach();
    assert!(
        matches!(got.as_slice(), [Event::OrderCancelRejected(r)]
            if r.order_client_id == "order-client-id-r" && r.reason.contains("NOT sent")),
        "{got:?}"
    );
    assert!(order_writes(&log).is_empty(), "{:?}", order_writes(&log));
}
