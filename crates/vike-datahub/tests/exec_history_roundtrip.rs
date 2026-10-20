//! **The execution-history reads through the REAL server**: the window and the soft `limit` on
//! `ScanExecFills`, the `ScanExecOrders` verb, and the `exec_materialize` advertisement — driven
//! over loopback by the real client against `vike_datahub::serve` / `serve_with_import` over the
//! in-memory store double.
//!
//! - a windowed, `limit`ed read cuts only on a WHOLE `ts` (the soft cap every range verb keeps),
//!   so a client paging from `last_ts + 1` loses no row of a tied timestamp;
//! - a paged read through `RemoteHistStore` comes back whole;
//! - `exec_materialize` is advertised exactly when the boot says the materializer is mounted, and
//!   the two exec reads are advertised either way (build facts).
//!
//! The ceiling and frame halves of the two verbs are `scan_ceilings.rs`'s (`Kind::ExecFills`,
//! `Kind::ExecOrders`).

mod common;

use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use vike_data::{ExecFillRow, ExecOrderRow, HistStore, MemHistStore, TsRange};
use vike_datahub::server::serve_with_import;
use vike_datahub_client::proto::{
    FEATURE_EXEC_MATERIALIZE, FEATURE_SCAN_EXEC_FILLS_RANGE, FEATURE_SCAN_EXEC_ORDERS, Request,
    Response,
};
use vike_datahub_client::{DatahubClient, RemoteHistStore};

use common::{exchange, spawn_server};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

fn fill(ts: i64, trade: &str) -> ExecFillRow {
    ExecFillRow {
        ts,
        trade_id: trade.to_string(),
        client_order_id: "c1".to_string(),
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
    }
}

fn order(ts: i64, coid: &str, status: &str) -> ExecOrderRow {
    ExecOrderRow {
        ts,
        client_order_id: coid.to_string(),
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
    }
}

/// Fills at 10, 20 (×3, a tied timestamp), 30 and 40; orders at 10, 20 (×2) and 30.
fn seeded() -> Arc<MemHistStore> {
    let store = Arc::new(MemHistStore::new());
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
/// never a split, which a client continuing from `last_ts + 1` would silently lose.
#[test]
fn the_exec_range_cuts_only_on_a_whole_ts_boundary() {
    let addr = spawn_server(seeded());
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    let ask = |stream: &mut std::net::TcpStream, limit| {
        let request = Request::ScanExecFills {
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            start: Some(10),
            end: Some(40),
            limit: Some(limit),
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

/// `RemoteHistStore`'s paged heads come back WHOLE over a range, in order, ties included.
#[test]
fn a_paged_exec_scan_comes_back_whole() {
    let store = seeded();
    let remote = RemoteHistStore::new(spawn_server(store.clone()).to_string());
    let window = TsRange { start: Some(20), end: Some(40) };
    let fills = remote.scan_exec_fills_head(VENUE, SYMBOL, window, usize::MAX).unwrap();
    let expected = store.scan_exec_fills_head(VENUE, SYMBOL, window, usize::MAX).unwrap();
    assert_eq!(fills, expected);
    assert_eq!(fills.len(), 5);
    let orders = remote.scan_exec_orders_head(VENUE, SYMBOL, TsRange::all(), usize::MAX).unwrap();
    assert_eq!(orders, store.scan_exec_orders(VENUE, SYMBOL).unwrap());
}

/// The two exec READS are build facts — advertised by a server that mounts nothing — and
/// `exec_materialize` is the boot's runtime fact, advertised exactly when it says the
/// materializer is mounted.
#[test]
fn exec_materialize_is_advertised_only_when_mounted() {
    let plain = DatahubClient::connect(spawn_server(seeded())).expect("handshake");
    for f in [FEATURE_SCAN_EXEC_FILLS_RANGE, FEATURE_SCAN_EXEC_ORDERS] {
        assert!(plain.features().iter().any(|x| x == f), "{f} is a build fact");
    }
    assert!(!plain.serves_exec_materialize(), "{:?}", plain.features());

    for mounted in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("resolve assigned port");
        let served: Arc<dyn HistStore + Send + Sync> = seeded();
        thread::spawn(move || {
            let _ =
                serve_with_import(listener, served, None, None, None, None, None, None, mounted);
        });
        let client = DatahubClient::connect(addr).expect("handshake");
        assert_eq!(client.serves_exec_materialize(), mounted, "{:?}", client.features());
        assert_eq!(
            client.features().iter().filter(|f| *f == FEATURE_EXEC_MATERIALIZE).count(),
            usize::from(mounted)
        );
    }
}
