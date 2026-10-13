//! The CAPABILITY NEGOTIATION of the execution-history reads — the window on
//! [`Request::ScanExecFills`] ([`FEATURE_SCAN_EXEC_FILLS_RANGE`]) and the order-lifecycle verb
//! [`Request::ScanExecOrders`] ([`FEATURE_SCAN_EXEC_ORDERS`]), decision 0112's three legs:
//!
//! - **the advertisement is a BUILD fact** — the real `serve` advertises both and answers both;
//! - **new client, server without the capability** — the order read is refused CLIENT-SIDE with
//!   nothing sent, and a windowed fill read is never sent either: `RemoteHistStore` falls back to
//!   the windowless frame every server answers and cuts the window on its own side;
//! - **the runtime fact** [`FEATURE_EXEC_MATERIALIZE`] is absent from a server that mounts no
//!   materializer.
//!
//! Loopback + in-memory only.

// `common::spawn_server` is the real `serve` — no materializer, no lanes — over the store it is
// handed.
mod common;
#[path = "support/fake_peer.rs"]
mod fake_peer;

use std::sync::Arc;

use vike_data::{ExecFillRow, ExecOrderRow, HistStore, MemHistStore, TsRange};
use vike_datahub_client::proto::{
    FEATURE_EXEC_MATERIALIZE, FEATURE_SCAN_EXEC_FILLS_RANGE, FEATURE_SCAN_EXEC_ORDERS, Request,
    Response,
};
use vike_datahub_client::{DatahubClient, RemoteHistStore};

use common::spawn_server;
use fake_peer::spawn_fake_peer;

fn fill(ts: i64) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: format!("t{ts}"),
        client_order_id: "c1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        px: 100.0,
        commission: 0.0,
        mark_price: None,
        liquidity_side: String::new(),
        commission_asset: String::new(),
        venue_order_id: Some("v1".to_string()),
        account: Some("ALT".to_string()),
    }
}

fn order(ts: i64, status: &str) -> ExecOrderRow {
    ExecOrderRow {
        ts,
        client_order_id: "c1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        status: status.to_string(),
        price: Some(100.0),
        trigger_price: None,
        venue_order_id: Some("v1".to_string()),
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        account: None,
    }
}

/// Leg 1: the real server advertises both build facts and answers both reads over a window, the
/// rows whole — and does NOT advertise the runtime materializer fact, because it mounts none.
#[test]
fn the_server_advertises_and_answers_both_exec_reads() {
    let store = Arc::new(MemHistStore::new());
    store.append_exec_fills("binance", "BTCUSDT", &[fill(10), fill(20), fill(30)], None).unwrap();
    store
        .append_exec_orders(
            "binance",
            "BTCUSDT",
            &[order(10, "SUBMITTED"), order(20, "ACCEPTED"), order(30, "FILLED")],
            None,
        )
        .unwrap();
    let addr = spawn_server(store);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.serves_exec_fills_range(), "{:?}", client.features());
    assert!(client.serves_exec_orders(), "{:?}", client.features());
    assert!(!client.serves_exec_materialize(), "nothing mounted a materializer");

    let window = TsRange { start: Some(15), end: Some(30) };
    let fills = client.scan_exec_fills_range("binance", "BTCUSDT", window, None).unwrap();
    assert_eq!(fills.iter().map(|f| f.ts).collect::<Vec<_>>(), vec![20, 30]);
    assert_eq!(fills[0].account.as_deref(), Some("ALT"), "the additive column rides the wire");
    let orders = client.scan_exec_orders("binance", "BTCUSDT", window, Some(1)).unwrap();
    assert_eq!(orders.iter().map(|o| o.ts).collect::<Vec<_>>(), vec![20], "a soft page of one");

    // …and through the `HistStore` seam, paged and stopped at the head's count.
    let remote = RemoteHistStore::new(addr.to_string());
    let head = remote.scan_exec_orders_head("binance", "BTCUSDT", TsRange::all(), 2).unwrap();
    assert_eq!(head.iter().map(|o| o.ts).collect::<Vec<_>>(), vec![10, 20]);
    let all = remote.scan_exec_fills_head("binance", "BTCUSDT", TsRange::all(), 100).unwrap();
    assert_eq!(all.len(), 3);
    let features = remote.advertised_features().unwrap();
    assert!(features.iter().any(|f| f == FEATURE_SCAN_EXEC_ORDERS), "{features:?}");
    assert!(!features.iter().any(|f| f == FEATURE_EXEC_MATERIALIZE), "{features:?}");
}

/// Leg 2a: a server without `scan_exec_orders` is refused BEFORE a frame is written, naming the
/// capability.
#[test]
fn exec_orders_refuses_locally_when_unadvertised() {
    let (addr, rx, handle) =
        spawn_fake_peer(vec!["load_bars".to_string(), "scan_exec_fills".to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    assert!(!client.serves_exec_orders());
    let err = client
        .scan_exec_orders("binance", "BTCUSDT", TsRange::all(), Some(10))
        .expect_err("refused client-side");
    assert!(err.contains(FEATURE_SCAN_EXEC_ORDERS), "names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("fake joins");
}

/// Leg 2b: a server without `scan_exec_fills_range` never receives a window — it would IGNORE one
/// and answer the whole series as if it were the window. The direct read refuses locally, and the
/// seam's head falls back to the WINDOWLESS frame and cuts the window on the client's side.
#[test]
fn exec_fills_range_is_not_sent_to_a_server_that_does_not_advertise_it() {
    let (addr, rx, handle) =
        spawn_fake_peer(vec!["load_bars".to_string(), "scan_exec_fills".to_string()], |r| {
            matches!(r, Request::ScanExecFills { .. })
                .then(|| Response::ExecFills(vec![fill(10), fill(20), fill(30)]))
        });
    let remote = RemoteHistStore::new(addr.to_string());
    let window = TsRange { start: Some(15), end: Some(25) };
    let head = remote.scan_exec_fills_head("binance", "BTCUSDT", window, 100).unwrap();
    assert_eq!(head.iter().map(|f| f.ts).collect::<Vec<_>>(), vec![20], "cut client-side");
    drop(remote);
    let received = rx.recv().expect("fake reports");
    assert_eq!(received.len(), 1, "one windowless read: {received:?}");
    match &received[0] {
        Request::ScanExecFills { start, end, limit, .. } => {
            assert_eq!((*start, *end, *limit), (None, None, None), "no window was sent");
        }
        other => panic!("expected ScanExecFills, got {other:?}"),
    }
    handle.join().expect("fake joins");

    let (addr, rx, handle) = spawn_fake_peer(vec!["load_bars".to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let err = client
        .scan_exec_fills_range("binance", "BTCUSDT", window, None)
        .expect_err("refused client-side");
    assert!(err.contains(FEATURE_SCAN_EXEC_FILLS_RANGE), "names the capability: {err}");
    drop(client);
    assert!(rx.recv().expect("fake reports").is_empty(), "nothing was sent");
    handle.join().expect("fake joins");
}
