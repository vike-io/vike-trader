//! Hermetic loopback tests for [`RemoteHistStore`] — a real `vike-datahub::serve` bound on an
//! ephemeral `127.0.0.1:0` port over a seeded `MemHistStore`, read back through `RemoteHistStore`
//! (the `HistStore`-over-RPC client). No prod store, no external network.
//!
//! ⚠ On the choice of data checked: `MemHistStore` (vike-data's `test-support` double) carries REAL
//! storage ONLY for the properties / equity / exec-log / funding / chain seams — its `load_bars` /
//! `scan_quotes` / `scan_trades` are inert stubs that return empty and its `append_bars` is a no-op
//! (see the module doc on `vike_data::test_support`). So the meaningful DATA round-trip here goes
//! through `properties_as_of` (a real value crosses the wire); `load_bars`/`scan_quotes`/`scan_trades`
//! prove the request→server→store→response PLUMBING (they come back `Ok(empty)` over the stub). The
//! non-empty `Bar` wire path is proven separately by a direct `proto` frame round-trip below.

use std::io::Cursor;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use vike_data::{HistStore, MemHistStore, SeriesCoverage, SeriesId, TsRange};
use vike_datahub::serve;
use vike_datahub_client::{read_frame, write_frame, RemoteHistStore, Request, Response};
use vike_model::{Bar, SymbolProperties};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1d";
const PROPS_TS: i64 = 1_500;

/// The seeded properties value — the single source of truth for the seed and every expected assert.
fn sample_props() -> SymbolProperties {
    SymbolProperties {
        tick_size: 0.5,
        step_size: 0.001,
        min_qty: 0.001,
        min_notional: 5.0,
        ..Default::default()
    }
}

/// Two OHLCV bars — used only for the direct-`proto` frame round-trip (the store double stores no
/// bars, so these never go through `MemHistStore`).
fn sample_bars() -> Vec<Bar> {
    vec![
        Bar {
            ts: 1_000,
            open: 10.0,
            high: 12.0,
            low: 9.0,
            close: 11.0,
            volume: 100.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        },
        Bar {
            ts: 2_000,
            open: 11.0,
            high: 13.0,
            low: 10.5,
            close: 12.5,
            volume: 150.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        },
    ]
}

/// A `MemHistStore` seeded with one properties row (the seam the double stores for real).
fn seeded_store() -> Arc<dyn HistStore + Send + Sync> {
    let store = MemHistStore::new();
    let rows = [(PROPS_TS, sample_props())];
    let n = store.append_symbol_properties(VENUE, SYMBOL, &rows, Some("seed-props")).unwrap();
    assert_eq!(n, 1, "one properties row seeded");
    Arc::new(store)
}

/// Bind an ephemeral loopback listener, spawn `serve` over `store` on a detached thread, and return
/// the assigned address for a client to connect to.
fn spawn_server(store: Arc<dyn HistStore + Send + Sync>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        // The serve loop runs for the lifetime of the test process; its Result is only Err on an
        // impossible listener close, which we don't assert on here.
        let _ = serve(listener, store);
    });
    addr
}

/// The real data round-trip: a seeded `SymbolProperties` crosses the wire and comes back equal,
/// and an unseeded symbol round-trips as `None`.
#[test]
fn properties_as_of_round_trips_real_value() {
    let addr = spawn_server(seeded_store());
    let store = RemoteHistStore::new(addr.to_string());

    // seeded row is visible at/after its observation ts
    let got = store.properties_as_of(VENUE, SYMBOL, PROPS_TS + 500).expect("properties_as_of ok");
    assert_eq!(got, Some(sample_props()), "the seeded properties must round-trip byte-for-byte");

    // before the observation ts → nothing yet
    let before = store.properties_as_of(VENUE, SYMBOL, PROPS_TS - 1).expect("properties_as_of ok");
    assert_eq!(before, None, "no properties observed before the seed ts");

    // an unseeded symbol → None (the `Response::Properties(None)` path)
    let absent = store.properties_as_of(VENUE, "ETHUSDT", PROPS_TS + 500).expect("ok");
    assert_eq!(absent, None, "an unseeded symbol has no properties");
}

/// The three served-read verbs plumb end-to-end over the store double. `MemHistStore` stores no
/// bars/ticks, so each returns `Ok(empty)` — this asserts the request/response wiring (no error, no
/// hang), not data flow (which `properties_as_of` above covers).
#[test]
fn served_reads_plumb_end_to_end() {
    let addr = spawn_server(seeded_store());
    let store = RemoteHistStore::new(addr.to_string());

    let bars = store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).expect("load_bars ok");
    assert!(bars.is_empty(), "MemHistStore stores no bars — plumbing only");

    let quotes = store.scan_quotes(VENUE, SYMBOL, TsRange::all()).expect("scan_quotes ok");
    assert!(quotes.is_empty(), "MemHistStore stores no quotes — plumbing only");

    let trades = store.scan_trades(VENUE, SYMBOL, TsRange::of(0, 10_000)).expect("scan_trades ok");
    assert!(trades.is_empty(), "MemHistStore stores no trades — plumbing only");
}

/// The read-only contract: every write verb and every non-served read verb returns `Err` naming the
/// method, and NEVER touches the network (they short-circuit before dialling). A dummy address the
/// test never binds proves no connection is attempted.
#[test]
fn writes_and_unsupported_reads_are_read_only_errors() {
    let store = RemoteHistStore::new("127.0.0.1:9"); // discard port; must never be dialled

    // writes
    let e = store.append_bars(VENUE, SYMBOL, INTERVAL, &[], None).unwrap_err();
    assert!(e.to_string().contains("append_bars"), "error names the method: {e}");
    assert!(e.to_string().contains("not served"), "error is the read-only message: {e}");

    assert!(store.append_quotes(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.append_trades(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.append_book_updates(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.append_symbol_properties(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.append_equity(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.append_exec_fills(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.append_exec_orders(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.append_funding(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.append_chain_snapshot(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.append_cohort(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.resample_quotes_to_bars(VENUE, SYMBOL, INTERVAL, TsRange::all(), None).is_err());
    assert!(store.resample_trades_to_bars(VENUE, SYMBOL, INTERVAL, TsRange::all(), None).is_err());

    // non-served reads
    assert!(store.scan_book_updates(VENUE, SYMBOL, TsRange::all()).is_err());
    // scan_depth inherited the trait's `Ok(vec![])` default until split-plane B12 — an RPC store
    // silently fabricating "no depth data". It must refuse like its siblings, naming the method.
    // I5 has since made the TRAIT default refuse too, so the `not served` assertion below is what
    // keeps this override earning its place: the trait's refusal says "this store serves no depth
    // lane", which is false here — the server may hold depth, only the RPC lacks the verb.
    let e = store.scan_depth(VENUE, SYMBOL, TsRange::all()).unwrap_err();
    assert!(e.to_string().contains("scan_depth"), "error names the method: {e}");
    assert!(e.to_string().contains("not served"), "error is the read-only message: {e}");
    // append_depth is covered by the TRAIT default already (it refuses — "a store that serves no
    // depth lane"); pinned here so a future override can never flip it to a silent Ok.
    assert!(store.append_depth(VENUE, SYMBOL, &[], None).is_err());
    assert!(store.scan_symbol_properties(VENUE, SYMBOL, TsRange::all()).is_err());
    assert!(store.scan_equity(VENUE, SYMBOL, TsRange::all()).is_err());
    assert!(store.scan_exec_fills(VENUE, SYMBOL).is_err());
    assert!(store.scan_exec_orders(VENUE, SYMBOL).is_err());
    assert!(store.scan_funding(VENUE, SYMBOL, TsRange::all()).is_err());
    assert!(store.scan_chain(VENUE, SYMBOL, TsRange::all()).is_err());
    // `kind=cohort`'s two verbs default to a no-op append and an EMPTY scan on the trait, which is
    // right for a bars-only double and wrong here for the `scan_depth` reason above: the server may
    // hold cohort panels, only the RPC lacks the verb, so the empty would be this client's
    // fabrication rather than the store's answer.
    let e = store.scan_cohort(VENUE, SYMBOL, TsRange::all()).unwrap_err();
    assert!(e.to_string().contains("scan_cohort"), "error names the method: {e}");
    assert!(e.to_string().contains("not served"), "error is the read-only message: {e}");
    assert!(store.chain_as_of(VENUE, SYMBOL, PROPS_TS).is_err());
    assert!(store.chain_as_of_within(VENUE, SYMBOL, PROPS_TS, 60_000).is_err());
}

// --- direct proto framing (no network): prove the model types ride the wire with real data --------

/// A `Request::LoadBars` and a `Response::Bars` carrying NON-EMPTY `Bar`s survive
/// `write_frame` -> `read_frame` unchanged — the `Bar` wire path the store double cannot exercise.
#[test]
fn bars_request_and_response_frame_round_trip() {
    let mut buf: Vec<u8> = Vec::new();
    let req = Request::LoadBars {
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        interval: INTERVAL.to_string(),
        start: Some(0),
        end: Some(10_000),
    };
    write_frame(&mut buf, &req).unwrap();
    write_frame(&mut buf, &Response::Bars(sample_bars())).unwrap();

    let mut cur = Cursor::new(buf);
    let got_req: Request = read_frame(&mut cur).unwrap();
    match got_req {
        Request::LoadBars { venue, symbol, interval, start, end } => {
            assert_eq!(venue, VENUE);
            assert_eq!(symbol, SYMBOL);
            assert_eq!(interval, INTERVAL);
            assert_eq!(start, Some(0));
            assert_eq!(end, Some(10_000));
        }
        other => panic!("expected LoadBars, got {other:?}"),
    }
    let got_resp: Response = read_frame(&mut cur).unwrap();
    match got_resp {
        Response::Bars(bars) => assert_eq!(bars, sample_bars(), "bars survive the frame codec"),
        other => panic!("expected Bars, got {other:?}"),
    }
}

/// The three PR-6 store-metadata verbs plumb end-to-end over the store double — and since
/// `MemHistStore` grew a REAL catalog fold (its `list_series`/`inventory` answer from what it
/// holds, rather than inheriting a trait default), the seeded properties series now CROSSES THE
/// WIRE: this test used to be able to assert only `is_empty()` on every verb, which could not
/// tell served-and-empty from a fabricated empty. `series_gaps` still answers the trait's empty
/// default — the double records no `date=` partitions, so there is no span to find holes in. The
/// non-empty BAR catalog over the composed DataFusion path is
/// `crates/vike-datahub/tests/composed_store_roundtrip.rs`'s job.
#[test]
fn metadata_verbs_plumb_end_to_end() {
    let addr = spawn_server(seeded_store());
    let store = RemoteHistStore::new(addr.to_string());

    let series = store.list_series().expect("list_series ok");
    assert_eq!(
        series,
        vec![SeriesId::per_symbol("properties", VENUE, SYMBOL, None)],
        "the seeded double's real catalog crosses the wire"
    );

    let inv = store.inventory().expect("inventory ok");
    assert_eq!(inv.len(), 1, "the one seeded series, with coverage");
    assert_eq!(inv[0].0, series[0], "inventory names the listed series");
    assert_eq!(inv[0].1.rows, 1, "the seeded properties row is counted");
    assert_eq!(inv[0].1.first_ts, PROPS_TS);
    assert_eq!(inv[0].1.last_ts, PROPS_TS);

    let id = SeriesId::per_symbol("quote", VENUE, SYMBOL, None);
    let gaps = store.series_gaps(&id).expect("series_gaps ok");
    assert!(gaps.is_empty(), "MemHistStore records no date partitions — no span to hole");
}

/// The PR-6 metadata verbs carrying NON-EMPTY `SeriesId`/`SeriesCoverage` survive the frame codec —
/// the catalog wire path the store double cannot exercise (it enumerates nothing). Proves the added
/// `SeriesId` serde derive + the new Request/Response variants round-trip byte-for-byte.
#[test]
fn metadata_request_and_response_frame_round_trip() {
    let id = SeriesId::per_symbol("bar", VENUE, SYMBOL, Some(INTERVAL.into()));
    let cov = SeriesCoverage {
        first_ts: 1_000,
        last_ts: 2_000,
        rows: 2,
        bytes: 4096,
        parts: 1,
        dates: 1,
    };

    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Request::SeriesGaps { id: id.clone() }).unwrap();
    write_frame(&mut buf, &Response::SeriesList(vec![id.clone()])).unwrap();
    write_frame(&mut buf, &Response::Inventory(vec![(id.clone(), cov.clone())])).unwrap();
    write_frame(&mut buf, &Response::SeriesGaps(vec![(3_000, 5_000)])).unwrap();

    let mut cur = Cursor::new(buf);
    match read_frame::<_, Request>(&mut cur).unwrap() {
        Request::SeriesGaps { id: got } => assert_eq!(got, id, "SeriesId rides the request whole"),
        other => panic!("expected SeriesGaps, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::SeriesList(v) => assert_eq!(v, vec![id.clone()], "SeriesList round-trips"),
        other => panic!("expected SeriesList, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::Inventory(v) => {
            assert_eq!(v, vec![(id, cov)], "Inventory (SeriesId+coverage) round-trips")
        }
        other => panic!("expected Inventory, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::SeriesGaps(v) => assert_eq!(v, vec![(3_000, 5_000)], "gap ranges round-trip"),
        other => panic!("expected SeriesGaps, got {other:?}"),
    }
}

/// A `Response::Properties(Some(..))` (the BOXED `SymbolProperties`) survives the frame codec — the
/// wire shape is identical to an unboxed option, so the value round-trips byte-for-byte.
#[test]
fn properties_response_frame_round_trip() {
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Response::Properties(Some(Box::new(sample_props())))).unwrap();

    let mut cur = Cursor::new(buf);
    match read_frame::<_, Response>(&mut cur).unwrap() {
        Response::Properties(Some(props)) => assert_eq!(*props, sample_props()),
        other => panic!("expected Properties(Some), got {other:?}"),
    }
}
