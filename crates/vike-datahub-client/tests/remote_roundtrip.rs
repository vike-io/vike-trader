//! Hermetic loopback tests for [`HistoryStoreDatahub`] — a real `vike-datahub::serve` bound on an
//! ephemeral `127.0.0.1:0` port over a seeded `HistoryStoreMemory`, read back through `HistoryStoreDatahub`
//! (the `HistoryStore`-over-RPC client). No prod store, no external network.
//!
//! ⚠ On the choice of data checked: `HistoryStoreMemory` (vike-data's `test-support` double) carries REAL
//! storage ONLY for the properties / equity / exec-log / funding / chain seams — its `load_bars` /
//! `scan_quotes` / `scan_trades` are inert stubs that return empty and its `append_bars` is a no-op
//! (see the module doc on `vike_data::test_support`). So the meaningful DATA round-trip here goes
//! through `properties_as_of` (a real value crosses the wire); `load_bars`/`scan_quotes`/`scan_trades`
//! prove the request→server→store→response PLUMBING (they come back `Ok(empty)` over the stub). The
//! non-empty `Bar` wire path is proven separately by a direct `proto` frame round-trip below.

use std::io::Cursor;
use std::sync::Arc;

use vike_data::StoreKindId;
use vike_data::{
    CohortRow, ExecFillRow, HistoryStore, HistoryStoreMemory, PerpMetricRow, SeriesCoverage,
    SeriesId, TimestampRange,
};
use vike_datahub_client::{
    HistoryStoreDatahub,
    proto::{Request, Response, read_frame, write_frame},
};
use vike_marketdata::Bar;
use vike_model::{EquitySample, SymbolProperties};

use vike_datahub::test_support::spawn_server;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "1d";
const PROPERTIES_TIMESTAMP: i64 = 1_500;

/// The seeded properties value — the single source of truth for the seed and every expected assert.
fn sample_properties() -> SymbolProperties {
    SymbolProperties {
        tick_size: 0.5,
        step_size: 0.001,
        min_qty: 0.001,
        min_notional: 5.0,
        ..Default::default()
    }
}

/// Two OHLCV bars — used only for the direct-`proto` frame round-trip (the store double stores no
/// bars, so these never go through `HistoryStoreMemory`).
fn sample_bars() -> Vec<Bar> {
    vec![
        Bar {
            timestamp: 1_000,
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
            timestamp: 2_000,
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

/// A `HistoryStoreMemory` seeded with one properties row (the seam the double stores for real).
fn seeded_store() -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    let rows = [(PROPERTIES_TIMESTAMP, sample_properties())];
    let n = store.append_symbol_properties(VENUE, SYMBOL, &rows, Some("seed-props")).unwrap();
    assert_eq!(n, 1, "one properties row seeded");
    Arc::new(store)
}

/// The real data round-trip: a seeded `SymbolProperties` crosses the wire and comes back equal,
/// and an unseeded symbol round-trips as `None`.
#[test]
fn properties_as_of_round_trips_real_value() {
    let addr = spawn_server(seeded_store());
    let store = HistoryStoreDatahub::new(addr.to_string());

    // seeded row is visible at/after its observation ts
    let got = store
        .properties_as_of(VENUE, SYMBOL, PROPERTIES_TIMESTAMP + 500)
        .expect("properties_as_of ok");
    assert_eq!(
        got,
        Some(sample_properties()),
        "the seeded properties must round-trip byte-for-byte"
    );

    // before the observation ts → nothing yet
    let before = store
        .properties_as_of(VENUE, SYMBOL, PROPERTIES_TIMESTAMP - 1)
        .expect("properties_as_of ok");
    assert_eq!(before, None, "no properties observed before the seed ts");

    // an unseeded symbol → None (the `Response::Properties(None)` path)
    let absent = store.properties_as_of(VENUE, "ETHUSDT", PROPERTIES_TIMESTAMP + 500).expect("ok");
    assert_eq!(absent, None, "an unseeded symbol has no properties");
}

/// The GUI served-read verbs plumb end-to-end over the store double. `HistoryStoreMemory` stores no
/// bars/ticks, so each returns `Ok(empty)` — this asserts the request/response wiring (no error, no
/// hang), not data flow (which `properties_as_of` above covers).
#[test]
fn served_reads_plumb_end_to_end() {
    let addr = spawn_server(seeded_store());
    let store = HistoryStoreDatahub::new(addr.to_string());

    let bars =
        store.load_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all()).expect("load_bars ok");
    assert!(bars.is_empty(), "HistoryStoreMemory stores no bars — plumbing only");

    let quotes = store.scan_quotes(VENUE, SYMBOL, TimestampRange::all()).expect("scan_quotes ok");
    assert!(quotes.is_empty(), "HistoryStoreMemory stores no quotes — plumbing only");

    let trades =
        store.scan_trades(VENUE, SYMBOL, TimestampRange::of(0, 10_000)).expect("scan_trades ok");
    assert!(trades.is_empty(), "HistoryStoreMemory stores no trades — plumbing only");
}

/// The read-only contract: every write verb and every STILL-non-served read verb returns `Err`
/// naming the method, and NEVER touches the network (they short-circuit before dialling). A dummy
/// address the test never binds proves no connection is attempted.
///
/// ⚠ The six reads `docs/decisions/0084-only-the-datahub-touches-the-store.md` put on the wire are
/// PINNED as dialling in [`the_six_wired_reads_dial_instead_of_refusing`]: a verb that stops
/// refusing moves there, or "we wired it" and "somebody deleted the assertion" look identical. The
/// four reads left here are the whole of what `HistoryStoreDatahub` still cannot ask for, and they stop
/// this file passing against a client that had simply stopped refusing anything.
#[test]
fn writes_and_unsupported_reads_are_read_only_errors() {
    let store = HistoryStoreDatahub::new("127.0.0.1:9"); // discard port; must never be dialled

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
    assert!(
        store
            .resample_quotes_to_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all(), None)
            .is_err()
    );
    assert!(
        store
            .resample_trades_to_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all(), None)
            .is_err()
    );

    // append_depth refuses through the TRAIT default ("a store that serves no depth lane"); pinned
    // so a future override can never flip it to a silent Ok. ⚠ Its READ twin is served and lives in
    // the dialling test below — the asymmetry this line makes visible.
    assert!(store.append_depth(VENUE, SYMBOL, &[], None).is_err());

    // The four reads still NOT on the wire. Each refuses locally, naming itself.
    let e = store.scan_symbol_properties(VENUE, SYMBOL, TimestampRange::all()).unwrap_err();
    assert!(e.to_string().contains("scan_symbol_properties"), "error names the method: {e}");
    assert!(e.to_string().contains("not served"), "error is the read-only message: {e}");
    assert!(store.scan_exec_orders(VENUE, SYMBOL).is_err());
    assert!(store.scan_funding(VENUE, SYMBOL, TimestampRange::all()).is_err());
    assert!(store.scan_chain(VENUE, SYMBOL, TimestampRange::all()).is_err());
    assert!(store.chain_as_of(VENUE, SYMBOL, PROPERTIES_TIMESTAMP).is_err());
    assert!(store.chain_as_of_within(VENUE, SYMBOL, PROPERTIES_TIMESTAMP, 60_000).is_err());
}

/// An address nothing is listening on, so a connect gets an immediate loopback RST.
///
/// ⚠ Port 1, never a port this test bound and released: port 1 is below every ephemeral range, so
/// no test's listener can be handed it, and a released port can be (a dial then reaches another
/// test's server). The discard port `:9` in [`writes_and_unsupported_reads_are_read_only_errors`]
/// is never dialled at all.
const CLOSED: &str = "127.0.0.1:1";

/// ⚠ **THE MIRROR PIN.** The six reads `docs/decisions/0084-only-the-datahub-touches-the-store.md`
/// added must DIAL: against a closed address each fails with a TRANSPORT error, NOT the local
/// `not served` refusal — the failure mode where the wire can answer and `HistoryStoreDatahub` never
/// asks. Hence the ABSENCE of the refusal message rather than `is_err()`: a refusal is an `Err` too.
#[test]
fn the_six_wired_reads_dial_instead_of_refusing() {
    let store = HistoryStoreDatahub::new(CLOSED);
    let all = TimestampRange::all();

    let errs = [
        ("scan_book_updates", store.scan_book_updates(VENUE, SYMBOL, all).unwrap_err()),
        ("scan_depth", store.scan_depth(VENUE, SYMBOL, all).unwrap_err()),
        ("scan_cohort", store.scan_cohort(VENUE, SYMBOL, all).unwrap_err()),
        ("scan_perp_metrics", store.scan_perp_metrics(VENUE, SYMBOL, all).unwrap_err()),
        ("scan_equity", store.scan_equity(VENUE, SYMBOL, all).unwrap_err()),
        ("scan_exec_fills", store.scan_exec_fills(VENUE, SYMBOL).unwrap_err()),
        // …and the two bounded execution-history heads, which dial for the Welcome first.
        (
            "scan_exec_fills_head",
            store.scan_execution_fills_head(VENUE, SYMBOL, all, 10).unwrap_err(),
        ),
        (
            "scan_exec_orders_head",
            store.scan_execution_orders_head(VENUE, SYMBOL, all, 10).unwrap_err(),
        ),
        // …and a lane's bars, whose trait default refuses as `cannot read a lane`.
        (
            "load_bars_sourced_capped",
            store
                .load_bars_sourced_capped(VENUE, "user-mine", SYMBOL, INTERVAL, all, None)
                .unwrap_err(),
        ),
    ];
    for (name, e) in errs {
        let msg = e.to_string();
        assert!(
            !msg.contains("not served") && !msg.contains("cannot read a lane"),
            "{name} is on the wire now and must DIAL, not refuse locally: {msg}"
        );
    }
}

/// FOUR of the six carry real rows over the store DOUBLE, in the roster lane.
///
/// ⚠ Deliberate coverage overlap with `crates/vike-datahub/tests/datafusion_serve/wire_six_verbs_roundtrip.rs`: that
/// file is behind `datafusion-serve` and runs in a FEATURE lane, which fires only when the plan says
/// the change touches it. These four seams are ones `HistoryStoreMemory` genuinely stores (this file's
/// module doc), so they cost nothing to prove here.
///
/// `scan_book_updates` and `scan_depth` are NOT here: the double stubs both to an empty answer, so
/// a round-trip over it could only assert `is_empty()` — the vacuous shape. Their real-row proof is
/// that file's alone.
#[test]
fn four_of_the_six_wired_reads_carry_real_rows_over_the_double() {
    let local = HistoryStoreMemory::new();

    let equity = vec![EquitySample {
        timestamp: 1_000,
        venue: VENUE.to_string(),
        equity: 10_125.5,
        realized: 100.25,
        unrealized: 25.25,
        missing_prices: 1,
    }];
    let fills = vec![ExecFillRow {
        ts: 1_000,
        trade_id: "t-1".to_string(),
        order_client_id: "order-client-id-1".to_string(),
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 0.5,
        px: 100.25,
        commission: 0.05,
        mark_price: None,
        liquidity_side: "maker".to_string(),
        commission_asset: "USDT".to_string(),
        venue_order_id: None,
        account: None,
        position_side: None,
        corrects_trade_id: None,
        correction_seq: None,
    }];
    let cohort = vec![CohortRow {
        ts: 1_000,
        asset: "BTC".to_string(),
        axis: "size".to_string(),
        cohort: "whale".to_string(),
        grading: "decile".to_string(),
        label_basis: "notional".to_string(),
        long_usd: 1_250_000.0,
        total_usd: 3_000_000.0,
    }];
    let perp = vec![PerpMetricRow { ts: 1_000, premium: 0.000_125, open_interest: Some(4_200.5) }];

    // Each seed is asserted on its row count: a double that accepted the call and stored nothing
    // would leave every read below vacuously empty.
    //
    // ⚠ **FOUR DISTINCT COMMIT KEYS.** `HistoryStoreMemory`'s `seen` is ONE `HashSet<String>` across every
    // kind (a real `commit_key` has no kind scoping either), so a second append under a key already
    // spent is a silent batch-level no-op returning `Ok(0)`.
    assert!(local.append_equity(VENUE, SYMBOL, &equity, Some("seed-equity")).unwrap() > 0);
    assert!(local.append_exec_fills(VENUE, SYMBOL, &fills, Some("seed-fills")).unwrap() > 0);
    assert!(local.append_cohort(VENUE, "BTC", &cohort, Some("seed-cohort")).unwrap() > 0);
    assert!(local.append_perp_metrics(VENUE, SYMBOL, &perp, Some("seed-perp")).unwrap() > 0);

    let local: Arc<dyn HistoryStore + Send + Sync> = Arc::new(local);
    let addr = spawn_server(local.clone());
    let remote = HistoryStoreDatahub::new(addr.to_string());
    let all = TimestampRange::all();

    let wire = remote.scan_equity(VENUE, SYMBOL, all).expect("scan_equity is served");
    assert!(!wire.is_empty(), "equity answers non-empty");
    assert_eq!(wire, local.scan_equity(VENUE, SYMBOL, all).unwrap(), "equity: wire == local");

    let wire = remote.scan_exec_fills(VENUE, SYMBOL).expect("scan_exec_fills is served");
    assert!(!wire.is_empty(), "exec fills answer non-empty");
    assert_eq!(wire, local.scan_exec_fills(VENUE, SYMBOL).unwrap(), "fills: wire == local");

    let wire = remote.scan_cohort(VENUE, "BTC", all).expect("scan_cohort is served");
    assert!(!wire.is_empty(), "cohort answers non-empty");
    assert_eq!(wire, local.scan_cohort(VENUE, "BTC", all).unwrap(), "cohort: wire == local");

    let wire = remote.scan_perp_metrics(VENUE, SYMBOL, all).expect("scan_perp_metrics is served");
    assert!(!wire.is_empty(), "perp metrics answer non-empty");
    assert_eq!(wire, local.scan_perp_metrics(VENUE, SYMBOL, all).unwrap(), "perp: wire == local");
}

// --- direct proto framing (no network): prove the model types ride the wire with real data --------

/// A `Request::LoadBars` and a `Response::Bars` carrying NON-EMPTY `Bar`s survive
/// `write_frame` -> `read_frame` unchanged — the `Bar` wire path the store double cannot exercise.
#[test]
fn bars_request_and_response_frame_round_trip() {
    let mut buffer: Vec<u8> = Vec::new();
    let req = Request::LoadBars {
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        interval: INTERVAL.to_string(),
        start: Some(0),
        end: Some(10_000),
        limit: None,
        source: None,
    };
    write_frame(&mut buffer, &req).unwrap();
    write_frame(&mut buffer, &Response::Bars(sample_bars())).unwrap();

    let mut cursor = Cursor::new(buffer);
    let got_request: Request = read_frame(&mut cursor).unwrap();
    match got_request {
        Request::LoadBars { venue, symbol, interval, start, end, limit, source: None } => {
            assert_eq!(venue, VENUE);
            assert_eq!(symbol, SYMBOL);
            assert_eq!(interval, INTERVAL);
            assert_eq!(start, Some(0));
            assert_eq!(end, Some(10_000));
            // ⚠ The ABSENT `limit` decodes as `None` rather than failing — the `#[serde(default)]`
            // half of what keeps the row cap additive. Its twin, that an absent `limit` is also
            // not SERIALIZED, is `a_request_without_a_limit_is_byte_identical_to_the_old_frame`.
            assert_eq!(limit, None, "an absent limit decodes as None");
        }
        other => panic!("expected LoadBars, got {other:?}"),
    }
    let got_response: Response = read_frame(&mut cursor).unwrap();
    match got_response {
        Response::Bars(bars) => assert_eq!(bars, sample_bars(), "bars survive the frame codec"),
        other => panic!("expected Bars, got {other:?}"),
    }
}

/// The three store-metadata verbs plumb end-to-end over the store double. `HistoryStoreMemory`'s
/// `list_series`/`inventory` answer from what it holds, so the seeded properties series CROSSES THE
/// WIRE — an `is_empty()` alone could not tell served-and-empty from a fabricated empty.
/// `series_gaps` answers the trait's empty default: the double records no `date=` partitions, so
/// there is no span to find holes in. The non-empty BAR catalog over the composed DataFusion path
/// is `crates/vike-datahub/tests/datafusion_serve/composed_store_roundtrip.rs`'s job.
#[test]
fn metadata_verbs_plumb_end_to_end() {
    let addr = spawn_server(seeded_store());
    let store = HistoryStoreDatahub::new(addr.to_string());

    let series = store.list_series().expect("list_series ok");
    assert_eq!(
        series,
        vec![SeriesId::per_symbol(StoreKindId::Properties, VENUE, SYMBOL, None)],
        "the seeded double's real catalog crosses the wire"
    );

    let inventory = store.inventory().expect("inventory ok");
    assert_eq!(inventory.len(), 1, "the one seeded series, with coverage");
    assert_eq!(inventory[0].0, series[0], "inventory names the listed series");
    assert_eq!(inventory[0].1.rows, 1, "the seeded properties row is counted");
    assert_eq!(inventory[0].1.first_timestamp, PROPERTIES_TIMESTAMP);
    assert_eq!(inventory[0].1.last_timestamp, PROPERTIES_TIMESTAMP);

    let id = SeriesId::per_symbol(StoreKindId::Quote, VENUE, SYMBOL, None);
    let gaps = store.series_gaps(&id).expect("series_gaps ok");
    assert!(gaps.is_empty(), "HistoryStoreMemory records no date partitions — no span to hole");
}

/// The metadata verbs carrying NON-EMPTY `SeriesId`/`SeriesCoverage` survive the frame codec — the
/// `SeriesId` serde derive and the Request/Response variants round-trip byte-for-byte.
#[test]
fn metadata_request_and_response_frame_round_trip() {
    let id = SeriesId::per_symbol(StoreKindId::Bar, VENUE, SYMBOL, Some(INTERVAL.into()));
    let cov = SeriesCoverage {
        first_timestamp: 1_000,
        last_timestamp: 2_000,
        rows: 2,
        bytes: 4096,
        parts: 1,
        dates: 1,
    };

    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &Request::SeriesGaps { id: id.clone() }).unwrap();
    write_frame(&mut buffer, &Response::SeriesList(vec![id.clone()])).unwrap();
    write_frame(&mut buffer, &Response::Inventory(vec![(id.clone(), cov.clone())])).unwrap();
    write_frame(&mut buffer, &Response::SeriesGaps(vec![(3_000, 5_000)])).unwrap();

    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Request>(&mut cursor).unwrap() {
        Request::SeriesGaps { id: got } => assert_eq!(got, id, "SeriesId rides the request whole"),
        other => panic!("expected SeriesGaps, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::SeriesList(v) => assert_eq!(v, vec![id.clone()], "SeriesList round-trips"),
        other => panic!("expected SeriesList, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::Inventory(v) => {
            assert_eq!(v, vec![(id, cov)], "Inventory (SeriesId+coverage) round-trips")
        }
        other => panic!("expected Inventory, got {other:?}"),
    }
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::SeriesGaps(v) => assert_eq!(v, vec![(3_000, 5_000)], "gap ranges round-trip"),
        other => panic!("expected SeriesGaps, got {other:?}"),
    }
}

/// A `Response::Properties(Some(..))` (the BOXED `SymbolProperties`) survives the frame codec — the
/// wire shape is identical to an unboxed option, so the value round-trips byte-for-byte.
#[test]
fn properties_response_frame_round_trip() {
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &Response::Properties(Some(Box::new(sample_properties())))).unwrap();

    let mut cursor = Cursor::new(buffer);
    match read_frame::<_, Response>(&mut cursor).unwrap() {
        Response::Properties(Some(properties)) => assert_eq!(*properties, sample_properties()),
        other => panic!("expected Properties(Some), got {other:?}"),
    }
}
