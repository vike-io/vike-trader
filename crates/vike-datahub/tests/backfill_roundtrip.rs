//! The COMPOSED-PATH gate for the backfill-on-demand verb (split-plane REQ-9, §3): a real
//! `DataFusionHist` over a temp dir, served by `vike_datahub::serve_with_backfill` on an ephemeral
//! loopback port, driven through `DatahubClient::backfill` — request in, rows in the REAL store,
//! `BackfillDone` counts out, and a follow-up `LoadBars` over the SAME wire returning the bars
//! bit-exact.
//!
//! The collector seam is FAKE here, deliberately: the real collectors
//! (`vike_backfill::backfill_binance_klines` and siblings) do venue REST I/O, and CI is
//! deterministic and network-free. `BackfillTable` is the injection point — the server dispatches
//! venue → collector through it, the bin installs the real table
//! (`vike_datahub::backfill::real_backfill_table`), and this test installs closures that write
//! KNOWN bars through the SAME `Arc<DataFusionHist>` the server serves — which is exactly the
//! write-through-before-reply contract ("writers live next to the data") the verb exists to keep.
//!
//! The whole file is behind `backfill-serve` (the feature that adds the collector tree to the
//! server build); CI runs it in the hist job's `cargo test -p vike-datahub --features
//! backfill-serve`. A default or `serve-datafusion`-only build compiles this file to nothing.
#![cfg(feature = "backfill-serve")]

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use tempfile::TempDir;
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_datahub::backfill::{real_backfill_table, BackfillTable};
use vike_datahub::serve_with_backfill;
use vike_datahub_client::{DatahubClient, FEATURE_BACKFILL};
use vike_model::Bar;

const VENUE: &str = "binance";
const SYMBOL: &str = "BACKFILLUSDT";
const INTERVAL: &str = "1h";
const HOUR_MS: i64 = 3_600_000;

/// The bars the FAKE collector "fetches" — the single source of truth for every assert below.
/// Distinctive fractional values so bit-exactness is meaningful (the composed sibling's
/// discipline).
fn fetched_bars() -> Vec<Bar> {
    let bar = |ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64, funding| Bar {
        ts,
        open,
        high,
        low,
        close,
        volume,
        funding,
        bid: None,
        ask: None,
        symbol: None,
    };
    vec![
        bar(HOUR_MS, 201.25, 203.5, 200.125, 202.75, 11.5, Some(0.0001)),
        bar(2 * HOUR_MS, 202.75, 204.0, 201.5, 203.25, 7.25, None),
        bar(3 * HOUR_MS, 203.25, 205.125, 202.0, 204.5, 3.75, Some(-0.0002)),
    ]
}

/// A fresh `DataFusionHist` over a temp dir. Unlike the composed sibling it starts EMPTY — the
/// verb under test is the thing that writes.
fn empty_store() -> (TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    (dir, Arc::new(store))
}

/// A fake collector table whose `binance` entry appends [`fetched_bars`] through `store` — the
/// SAME handle the server serves — mimicking exactly what the real collectors do (fetch, then
/// `append_bars`), minus the network.
fn fake_table(store: Arc<DataFusionHist>) -> BackfillTable {
    BackfillTable::new(vec![
        (
            "binance".to_string(),
            Box::new(move |symbol: &str, interval: &str, _start: i64, _end: i64| {
                let bars = fetched_bars();
                store
                    .append_bars(VENUE, symbol, interval, &bars, Some("fake-collector"))
                    .map_err(|e| e.to_string())
            }),
        ),
        (
            "bybit".to_string(),
            Box::new(|_: &str, _: &str, _: i64, _: i64| {
                Err("bybit fake collector: simulated venue failure".to_string())
            }),
        ),
    ])
}

/// Spawn `serve_with_backfill` over `store` + `table` on an ephemeral loopback port.
fn spawn_backfill_server(
    store: Arc<dyn HistStore + Send + Sync>,
    table: BackfillTable,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve_with_backfill(listener, store, Some(table));
    });
    addr
}

/// Bit-exact bar comparison — the composed sibling's discipline, applied to the verb's output.
fn assert_bars_bit_eq(expected: &[Bar], got: &[Bar]) {
    assert_eq!(expected.len(), got.len(), "bar count");
    for (i, (x, y)) in expected.iter().zip(got).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        for (name, xv, yv) in [
            ("open", x.open, y.open),
            ("high", x.high, y.high),
            ("low", x.low, y.low),
            ("close", x.close, y.close),
            ("volume", x.volume, y.volume),
        ] {
            assert_eq!(xv.to_bits(), yv.to_bits(), "{name}[{i}] ({xv} vs {yv})");
        }
        assert_eq!(x.funding.map(f64::to_bits), y.funding.map(f64::to_bits), "funding[{i}]");
    }
}

/// The whole verb, composed: a server WITH a table advertises `backfill`; the request runs the
/// collector; the rows land in the REAL `DataFusionHist`; `BackfillDone` reports the true count
/// and the true first/last ts of the range READ BACK from the store (the write-through proof);
/// and a follow-up `LoadBars` over the SAME connection returns the bars bit-exact.
#[test]
fn backfill_writes_through_the_served_store_and_load_bars_returns_it() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table(store.clone()));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");

    assert!(
        client.features().iter().any(|f| f == FEATURE_BACKFILL),
        "a server WITH a table advertises `{FEATURE_BACKFILL}`: {:?}",
        client.features()
    );

    let done = client
        .backfill(VENUE, SYMBOL, INTERVAL, 0, 4 * HOUR_MS)
        .expect("the advertised verb serves the request");
    let expected = fetched_bars();
    assert_eq!(done.rows_written, expected.len() as u64, "every fetched bar was written");
    assert_eq!(done.first_ts, Some(expected[0].ts), "first_ts is the range's first stored bar");
    assert_eq!(
        done.last_ts,
        Some(expected[expected.len() - 1].ts),
        "last_ts is the range's last stored bar"
    );

    // The write went through the SAME store handle the server serves: the direct local read...
    let local = store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_bars_bit_eq(&expected, &local);

    // ...and the follow-up read over the SAME wire both see it.
    let over_wire = client
        .load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all())
        .expect("follow-up LoadBars on the same connection");
    assert_bars_bit_eq(&expected, &over_wire);
}

/// An unknown venue is a clean `Response::Error` NAMING the supported set (the recorder's
/// error-naming idiom), and the collector never runs — the store stays empty.
#[test]
fn an_unknown_venue_is_refused_naming_the_supported_set() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table(store.clone()));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");

    let err = client
        .backfill("kraken", SYMBOL, INTERVAL, 0, 4 * HOUR_MS)
        .expect_err("an unsupported venue must be refused");
    assert!(err.contains("kraken"), "names the offending venue: {err}");
    assert!(err.contains("binance") && err.contains("bybit"), "names the supported set: {err}");

    let bars = store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert!(bars.is_empty(), "a refused request writes nothing");
}

/// A collector FAILURE (venue REST down, geo-block — the spec's honest-error arm) surfaces as a
/// clean `Err` carrying the venue context, never a hang and never a partial `BackfillDone`.
#[test]
fn a_collector_failure_is_a_clean_error_with_context() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table(store.clone()));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");

    let err = client
        .backfill("bybit", SYMBOL, INTERVAL, 0, 4 * HOUR_MS)
        .expect_err("the failing collector must surface its error");
    assert!(err.contains("bybit"), "carries the venue context: {err}");
    assert!(err.contains("simulated venue failure"), "carries the collector's message: {err}");
}

/// An INVERTED range (start > end) is refused before any collector runs — the v1 contract is a
/// bounded, well-formed range per request.
#[test]
fn an_inverted_range_is_refused_before_the_collector_runs() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table(store.clone()));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");

    let err = client
        .backfill(VENUE, SYMBOL, INTERVAL, 4 * HOUR_MS, 0)
        .expect_err("an inverted range must be refused");
    assert!(err.contains("start") && err.contains("end"), "names the malformed bounds: {err}");

    let bars = store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert!(bars.is_empty(), "a refused request writes nothing");
}

/// The REAL table names exactly the three venues whose kline collectors exist
/// (`vike_backfill::backfill_{binance,bybit,okx}_klines`) — built, never CALLED (the real
/// collectors do venue REST I/O; only the `#[ignore]`d live paths may drive them).
#[test]
fn the_real_table_names_the_three_kline_venues() {
    let (_dir, store) = empty_store();
    let table = real_backfill_table(store);
    assert_eq!(table.supported(), vec!["binance", "bybit", "okx"]);
}
