//! The CAPABILITY NEGOTIATION of the execution-history reads — the window on
//! [`Request::ScanExecFills`] ([`FEATURE_SCAN_EXECUTION_FILLS_RANGE`]) and the order-lifecycle verb
//! [`Request::ScanExecOrders`] ([`FEATURE_SCAN_EXECUTION_ORDERS`]), decision 0112's three legs:
//!
//! - **the advertisement is a BUILD fact** — the real `serve` advertises both and answers both;
//! - **new client, server without the capability** — the order read is refused CLIENT-SIDE with
//!   nothing sent, and a windowed fill read is never sent either: `HistoryStoreDatahub` falls back to
//!   the windowless frame every server answers and cuts the window on its own side;
//! - **the runtime fact** [`FEATURE_EXECUTIONS_RECORDED`] is absent from a server that mounts no
//!   materializer;
//! - **the catalog of one window** ([`Request::ExecutionSeries`], [`FEATURE_EXECUTION_SERIES`]): the real
//!   server answers exactly the exec series the window reaches, and a server without the string is sent
//!   nothing new — the seam asks it the whole inventory and folds it. Its frames are pinned by
//!   `fixtures/datahub_wire/v1_execution_series_*.jsonl`.
//!
//! Loopback + in-memory only.

use std::path::PathBuf;
use std::sync::Arc;

use vike_data::{
    ExecFillRow, ExecOrderRow, ExecutionSeries, HistoryStore, HistoryStoreMemory, TimestampRange,
};
use vike_datahub_client::proto::{
    FEATURE_EXECUTION_SERIES, FEATURE_EXECUTIONS_RECORDED, FEATURE_SCAN_EXECUTION_FILLS_RANGE,
    FEATURE_SCAN_EXECUTION_ORDERS, Request, Response, request_kind,
};
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};
use vike_hub_protocol::test_support::Goldens;

// `spawn_server` is the real `serve` — no materializer, no lanes — over the store it is
// handed.
use vike_datahub::test_support::spawn_server;
use vike_datahub_client::test_support::spawn_fake_peer;

fn fill(ts: i64) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: format!("t{ts}"),
        order_client_id: "c1".to_string(),
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
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
        mount_id: None,
        received_ms: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
    }
}

fn order(ts: i64, status: &str) -> ExecOrderRow {
    ExecOrderRow {
        ts,
        order_client_id: "c1".to_string(),
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
        mount_id: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        reason: None,
    }
}

/// Leg 1: the real server advertises both build facts and answers both reads over a window, the
/// rows whole — and does NOT advertise the runtime materializer fact, because it mounts none.
#[test]
fn the_server_advertises_and_answers_both_execution_reads() {
    let store = Arc::new(HistoryStoreMemory::new());
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
    assert!(client.serves_execution_fills_range(), "{:?}", client.features());
    assert!(client.serves_execution_orders(), "{:?}", client.features());
    assert!(!client.serves_executions_recorded(), "nothing mounted a materializer");

    let window = TimestampRange { start: Some(15), end: Some(30) };
    let fills = client.scan_execution_fills_range("binance", "BTCUSDT", window, None).unwrap();
    assert_eq!(fills.iter().map(|f| f.ts).collect::<Vec<_>>(), vec![20, 30]);
    assert_eq!(fills[0].account.as_deref(), Some("ALT"), "the additive column rides the wire");
    let orders = client.scan_exec_orders("binance", "BTCUSDT", window, Some(1)).unwrap();
    assert_eq!(orders.iter().map(|o| o.ts).collect::<Vec<_>>(), vec![20], "a soft page of one");

    // …and through the `HistoryStore` seam, paged and stopped at the head's count.
    let remote = HistoryStoreDatahub::new(addr.to_string());
    let head =
        remote.scan_execution_orders_head("binance", "BTCUSDT", TimestampRange::all(), 2).unwrap();
    assert_eq!(head.iter().map(|o| o.ts).collect::<Vec<_>>(), vec![10, 20]);
    let all =
        remote.scan_execution_fills_head("binance", "BTCUSDT", TimestampRange::all(), 100).unwrap();
    assert_eq!(all.len(), 3);
    let features = remote.advertised_features().unwrap();
    assert!(features.iter().any(|f| f == FEATURE_SCAN_EXECUTION_ORDERS), "{features:?}");
    assert!(!features.iter().any(|f| f == FEATURE_EXECUTIONS_RECORDED), "{features:?}");
}

/// Leg 2a: a server without `scan_exec_orders` is refused BEFORE a frame is written, naming the
/// capability.
#[test]
fn execution_orders_refuses_locally_when_unadvertised() {
    let (addr, rx, handle) =
        spawn_fake_peer(vec!["load_bars".to_string(), "scan_exec_fills".to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake succeeds — same version");
    assert!(!client.serves_execution_orders());
    let err = client
        .scan_exec_orders("binance", "BTCUSDT", TimestampRange::all(), Some(10))
        .expect_err("refused client-side");
    assert!(err.contains(FEATURE_SCAN_EXECUTION_ORDERS), "names the capability: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
    drop(client);
    let received = rx.recv().expect("fake reports");
    assert!(received.is_empty(), "the client SENT {received:?} after a local refusal");
    handle.join().expect("fake joins");
}

/// Leg 2b: a server without `scan_execution_fills_range` never receives a window — it would IGNORE one
/// and answer the whole series as if it were the window. The direct read refuses locally, and the
/// seam's head falls back to the WINDOWLESS frame and cuts the window on the client's side.
#[test]
fn execution_fills_range_is_not_sent_to_a_server_that_does_not_advertise_it() {
    let (addr, rx, handle) =
        spawn_fake_peer(vec!["load_bars".to_string(), "scan_exec_fills".to_string()], |r| {
            matches!(r, Request::ScanExecFills { .. })
                .then(|| Response::ExecFills(vec![fill(10), fill(20), fill(30)]))
        });
    let remote = HistoryStoreDatahub::new(addr.to_string());
    let window = TimestampRange { start: Some(15), end: Some(25) };
    let head = remote.scan_execution_fills_head("binance", "BTCUSDT", window, 100).unwrap();
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
        .scan_execution_fills_range("binance", "BTCUSDT", window, None)
        .expect_err("refused client-side");
    assert!(err.contains(FEATURE_SCAN_EXECUTION_FILLS_RANGE), "names the capability: {err}");
    drop(client);
    assert!(rx.recv().expect("fake reports").is_empty(), "nothing was sent");
    handle.join().expect("fake joins");
}

/// The `kind/venue/symbol` of each series an answer names, in its order.
fn keys(answer: &ExecutionSeries) -> Vec<String> {
    answer.series.iter().map(|(id, _)| format!("{}/{}/{}", id.kind, id.venue, id.symbol)).collect()
}

/// Leg 1 of the catalog: the REAL server advertises `execution_series` and answers exactly the exec series
/// with rows in the window — a bar series and an exec series whose rows end before the window are not named
/// — with the venue and symbol filters applied and every venue's exec symbols still the choices. The seam
/// answers what the store's own fold answers.
#[test]
fn the_server_answers_the_exec_series_a_window_reaches() {
    let store = Arc::new(HistoryStoreMemory::new());
    store.append_exec_fills("binance", "BTCUSDT", &[fill(10), fill(20)], None).unwrap();
    store.append_exec_orders("binance", "BTCUSDT", &[order(25, "FILLED")], None).unwrap();
    store.append_exec_fills("binance", "ETHUSDT", &[fill(1), fill(2)], None).unwrap();
    store.append_exec_fills("okx", "BTC-USDT", &[fill(30)], None).unwrap();
    let bar = vike_marketdata::Bar {
        timestamp: 20,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    store.append_bars("binance", "BTCUSDT", "1m", &[bar], None).unwrap();
    let addr = spawn_server(Arc::clone(&store) as Arc<dyn HistoryStore + Send + Sync>);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.serves_execution_series(), "{:?}", client.features());

    let window = TimestampRange { start: Some(15), end: Some(40) };
    let all = client.execution_series(None, None, window).unwrap();
    let mut got = keys(&all);
    got.sort();
    assert_eq!(
        got,
        ["exec_fill/binance/BTCUSDT", "exec_fill/okx/BTC-USDT", "exec_order/binance/BTCUSDT"],
        "the exec series the window reaches, and nothing else"
    );
    let choices: Vec<String> = all
        .venues
        .iter()
        .flat_map(|(v, symbols)| symbols.iter().map(move |s| format!("{v}/{s}")))
        .collect();
    assert_eq!(choices, ["binance/BTCUSDT", "binance/ETHUSDT", "okx/BTC-USDT"], "every choice");

    let okx = client.execution_series(Some("okx"), None, window).unwrap();
    assert_eq!(keys(&okx), ["exec_fill/okx/BTC-USDT"], "the venue filter");
    assert_eq!(okx.venues, all.venues, "a filter narrows the series, never the choices");
    let one = client.execution_series(Some("binance"), Some("BTCUSDT"), window).unwrap();
    assert_eq!(one.series.len(), 2, "the symbol filter keeps both kinds of that symbol");
    let none = client.execution_series(Some("binance"), Some("ETHUSDT"), window).unwrap();
    assert!(none.series.is_empty(), "its rows end before the window");

    let remote = HistoryStoreDatahub::new(addr.to_string());
    assert_eq!(
        remote.execution_series(None, None, window).unwrap(),
        store.execution_series(None, None, window).unwrap(),
        "the seam answers what the store's own fold answers"
    );
}

/// Leg 2c: a server without `execution_series` is sent NOTHING new — the direct read refuses locally, naming
/// the string, and the seam asks the whole `Inventory` (one frame) and folds it on this side.
#[test]
fn an_older_server_is_asked_its_inventory_and_folded_here() {
    let inventory = {
        let store = HistoryStoreMemory::new();
        store.append_exec_fills("binance", "BTCUSDT", &[fill(10), fill(20)], None).unwrap();
        store.append_exec_fills("okx", "BTC-USDT", &[fill(1)], None).unwrap();
        store.inventory().unwrap()
    };
    let window = TimestampRange { start: Some(15), end: Some(40) };
    let expected = ExecutionSeries::of(inventory.clone(), None, None, window);
    assert_eq!(keys(&expected), ["exec_fill/binance/BTCUSDT"], "the fold this test expects");

    let (addr, rx, handle) =
        spawn_fake_peer(vec![FEATURE_SCAN_EXECUTION_ORDERS.to_string()], |_| None);
    let mut client = DatahubClient::connect(addr).expect("handshake");
    let err = client.execution_series(None, None, window).expect_err("refused client-side");
    assert!(err.contains(FEATURE_EXECUTION_SERIES) && err.contains("nothing was sent"), "{err}");
    drop(client);
    assert!(rx.recv().expect("fake reports").is_empty(), "nothing was sent");
    handle.join().expect("fake joins");

    let (addr, rx, handle) = spawn_fake_peer(vec![FEATURE_SCAN_EXECUTION_ORDERS.to_string()], {
        let inventory = inventory.clone();
        move |r| matches!(r, Request::Inventory).then(|| Response::Inventory(inventory.clone()))
    });
    let remote = HistoryStoreDatahub::new(addr.to_string());
    assert_eq!(remote.execution_series(None, None, window).unwrap(), expected);
    drop(remote);
    let asked: Vec<&str> = rx.recv().expect("fake reports").iter().map(request_kind).collect();
    assert_eq!(asked, ["Inventory"], "one whole-inventory read, no new verb");
    handle.join().expect("fake joins");
}

/// The catalog's frames, pinned outside `crates/`: every golden decodes into this build's types and this
/// build still produces every key of it, so a renamed tag or field fails here. A new frame is a line
/// APPENDED to its file.
#[test]
fn the_golden_execution_series_frames_decode_and_re_encode() {
    let goldens = Goldens::at(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let src = "crates/vike-datahub-client/src/proto.rs";
    goldens.assert_round_trip::<Request>(
        "fixtures/datahub_wire/v1_execution_series_requests.jsonl",
        2,
        src,
    );
    goldens.assert_round_trip::<Response>(
        "fixtures/datahub_wire/v1_execution_series_responses.jsonl",
        2,
        src,
    );
}
