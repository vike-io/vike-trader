//! **The execution-history reads through the REAL server**: the window and the soft `limit` on
//! `ScanExecFills`, the `ScanExecOrders` verb, and the `exec_materialize` advertisement — driven
//! over loopback by the real client against `vike_datahub::serve` / `serve_with_lanes` over the
//! in-memory store double.
//!
//! - a windowed, `limit`ed read cuts only on a WHOLE `ts` (the soft cap every range verb keeps),
//!   so a client paging from `last_timestamp + 1` loses no row of a tied timestamp;
//! - a paged read through `HistoryStoreDatahub` comes back whole;
//! - `exec_materialize` (with `execution_stream`) is advertised exactly when the execution stream is
//!   mounted, and the two exec reads are advertised either way (build facts).
//!
//! The ceiling and frame halves of the two verbs are `scan_ceilings.rs`'s (`Kind::ExecFills`,
//! `Kind::ExecOrders`).

use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use vike_data::{ExecFillRow, ExecOrderRow, HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_datahub::execution_stream::ExecutionStreamLane;
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub_client::proto::{
    FEATURE_EXECUTION_STREAM, FEATURE_EXECUTIONS_RECORDED, FEATURE_SCAN_EXECUTION_FILLS_RANGE,
    FEATURE_SCAN_EXECUTION_ORDERS, Request, Response,
};
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};

use vike_datahub::test_support::{exchange, spawn_server};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

fn fill(ts: i64, trade: &str) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: trade.to_string(),
        order_client_id: "c1".to_string(),
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 1.0,
        px: 100.0,
        commission: 0.0,
        mark_price: None,
        liquidity_side: "taker".to_string(),
        commission_asset: String::new(),
        venue_order_id: Some("v1".to_string()),
        account: None,
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

fn order(ts: i64, order_client_id: &str, status: &str) -> ExecOrderRow {
    ExecOrderRow {
        ts,
        order_client_id: order_client_id.to_string(),
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        side: -1,
        qty: 1.0,
        order_type: "limit".to_string(),
        status: status.to_string(),
        price: Some(100.0),
        trigger_price: None,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        account: Some("ALT".to_string()),
        mount_id: None,
        ownership: None,
        journal_sequence: None,
        journal_generation: None,
        reason: None,
    }
}

/// Fills at 10, 20 (×3, a tied timestamp), 30 and 40; orders at 10, 20 (×2) and 30.
fn seeded() -> Arc<HistoryStoreMemory> {
    let store = Arc::new(HistoryStoreMemory::new());
    let fills =
        [fill(10, "a"), fill(20, "b"), fill(20, "c"), fill(20, "d"), fill(30, "e"), fill(40, "f")];
    store.append_exec_fills(VENUE, SYMBOL, &fills, None).unwrap();
    let orders = [
        order(10, "o1", "SUBMITTED"),
        order(20, "o1", "ACCEPTED"),
        order(20, "o2", "SUBMITTED"),
        order(30, "o1", "FILLED"),
    ];
    store.append_exec_orders(VENUE, SYMBOL, &orders, None).unwrap();
    store
}

/// A `limit` that falls INSIDE the tied `ts = 20` group answers the whole group or none of it —
/// never a split, which a client continuing from `last_timestamp + 1` would silently lose.
#[test]
fn the_execution_range_cuts_only_on_a_whole_timestamp_boundary() {
    let addr = spawn_server(seeded());
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    let ask = |stream: &mut std::net::TcpStream, limit| {
        let request = Request::ScanExecFills {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            start: Some(10),
            end: Some(40),
            limit: Some(limit),
            columns: None,
        };
        match exchange(stream, &request) {
            Response::ExecFills(rows) => rows.iter().map(|r| r.ts).collect::<Vec<_>>(),
            other => panic!("expected ExecFills, got {other:?}"),
        }
    };
    assert_eq!(
        ask(&mut stream, 2),
        vec![10],
        "the straddled group is left whole for the next page"
    );
    assert_eq!(ask(&mut stream, 4), vec![10, 20, 20, 20], "the cap lands on the group's end");
    let request = Request::ScanExecOrders {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        start: Some(15),
        end: None,
        limit: Some(1),
        columns: None,
    };
    match exchange(&mut stream, &request) {
        Response::ExecOrders(rows) => {
            // a FIRST group larger than the cap is taken whole: progress outranks the cap
            assert_eq!(rows.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![20, 20]);
            assert!(rows.iter().all(|r| r.account.as_deref() == Some("ALT")));
        }
        other => panic!("expected ExecOrders, got {other:?}"),
    }
}

/// `HistoryStoreDatahub`'s paged heads come back WHOLE over a range, in order, ties included.
#[test]
fn a_paged_execution_scan_comes_back_whole() {
    let store = seeded();
    let remote = HistoryStoreDatahub::new(spawn_server(store.clone()).to_string());
    let window = TimestampRange { start: Some(20), end: Some(40) };
    let fills = remote.scan_execution_fills_head(VENUE, SYMBOL, window, usize::MAX).unwrap();
    let expected = store.scan_execution_fills_head(VENUE, SYMBOL, window, usize::MAX).unwrap();
    assert_eq!(fills, expected);
    assert_eq!(fills.len(), 5);
    let orders = remote
        .scan_execution_orders_head(VENUE, SYMBOL, TimestampRange::all(), usize::MAX)
        .unwrap();
    assert_eq!(orders, store.scan_exec_orders(VENUE, SYMBOL).unwrap());
}

/// The two exec READS are build facts — advertised by a server that mounts nothing — and
/// `exec_materialize` is a runtime fact: advertised, with `execution_stream`, exactly when the execution
/// stream is mounted (the stored word an older desktop reads, now meaning "this datahub records the live
/// executions a tradehub streams").
#[test]
fn executions_recorded_is_advertised_only_when_the_stream_is_mounted() {
    let plain = DatahubClient::connect(spawn_server(seeded())).expect("handshake");
    for f in [FEATURE_SCAN_EXECUTION_FILLS_RANGE, FEATURE_SCAN_EXECUTION_ORDERS] {
        assert!(plain.features().iter().any(|x| x == f), "{f} is a build fact");
    }
    assert!(!plain.serves_executions_recorded(), "{:?}", plain.features());

    for mounted in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let served: Arc<dyn HistoryStore + Send + Sync> = seeded();
        let settings = tempfile::tempdir().expect("settings dir");
        let lanes = ServerLanes {
            execution_stream: mounted
                .then(|| Arc::new(ExecutionStreamLane::new(settings.path().to_path_buf(), None))),
            ..ServerLanes::default()
        };
        thread::spawn(move || {
            let timings = ServerTimings::PRODUCTION;
            let _ =
                serve_with_lanes(listener, served, None, None, None, None, lanes, None, timings);
        });
        let client = DatahubClient::connect(addr).expect("handshake");
        assert_eq!(client.serves_executions_recorded(), mounted, "{:?}", client.features());
        for feature in [FEATURE_EXECUTIONS_RECORDED, FEATURE_EXECUTION_STREAM] {
            assert_eq!(
                client.features().iter().filter(|f| *f == feature).count(),
                usize::from(mounted),
                "{feature}"
            );
        }
    }
}

/// **The exec reads cross as COLUMNS equal to their JSON reply** (`FEATURE_SCAN_COLUMNS`): windowless, windowed and
/// paged by a soft `limit`, in every body codec and both column bodies, the same page as the JSON reply's; and the
/// client's reads (which ask for the columns of a server that answers them) read the same rows back.
#[test]
fn the_exec_reads_cross_as_columns_equal_to_their_json_reply() {
    use vike_datahub_client::proto::{
        BarColumnsBody, EXECUTION_FILL_COLUMNS, EXECUTION_ORDER_COLUMNS, FEATURE_SCAN_COLUMNS,
        decode_columns_into,
    };
    use vike_hub_protocol::frame::{BodyCodec, read_frame_raw, write_frame_as};
    let store = seeded();
    let addr = spawn_server(store.clone());
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    let windows = [(None, None, None), (Some(10), Some(40), Some(2)), (Some(15), None, Some(4))];
    for (start, end, limit) in windows {
        let fills = |columns| Request::ScanExecFills {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            start,
            end,
            limit,
            columns,
        };
        let orders = |columns| Request::ScanExecOrders {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            start,
            end,
            limit,
            columns,
        };
        let Response::ExecFills(json_fills) = exchange(&mut stream, &fills(None)) else {
            panic!("the JSON fills")
        };
        let Response::ExecOrders(json_orders) = exchange(&mut stream, &orders(None)) else {
            panic!("the JSON orders")
        };
        for codec in [BodyCodec::Json, BodyCodec::Msgpack, BodyCodec::MsgpackLz4] {
            for body in [BarColumnsBody::Plain, BarColumnsBody::Lz4] {
                let what = format!("{start:?}..{end:?} x{limit:?} {codec:?} {body:?}");
                write_frame_as(&mut stream, codec, &fills(Some(body))).expect("ask");
                let mut got: Vec<ExecFillRow> = Vec::new();
                let reply = read_frame_raw(&mut stream).expect("the reply");
                decode_columns_into(&reply, &EXECUTION_FILL_COLUMNS, &mut got).expect(&what);
                assert_eq!(got, json_fills, "{what}");
                write_frame_as(&mut stream, codec, &orders(Some(body))).expect("ask");
                let mut got: Vec<ExecOrderRow> = Vec::new();
                let reply = read_frame_raw(&mut stream).expect("the reply");
                decode_columns_into(&reply, &EXECUTION_ORDER_COLUMNS, &mut got).expect(&what);
                assert_eq!(got, json_orders, "{what}");
            }
        }
    }
    let mut client = DatahubClient::connect(addr).expect("handshake");
    assert!(client.features().iter().any(|f| f == FEATURE_SCAN_COLUMNS), "{:?}", client.features());
    assert_eq!(
        client.scan_exec_fills(VENUE, SYMBOL).unwrap(),
        store.scan_exec_fills(VENUE, SYMBOL).unwrap()
    );
    let window = TimestampRange { start: Some(15), end: None };
    let orders = client.scan_exec_orders(VENUE, SYMBOL, window, None).unwrap();
    assert_eq!(
        orders,
        store.scan_execution_orders_head(VENUE, SYMBOL, window, usize::MAX).unwrap()
    );
}
