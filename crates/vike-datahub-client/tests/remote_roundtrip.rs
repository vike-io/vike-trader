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

use vike_data::{
    CohortRow, ExecFillRow, HistStore, MemHistStore, PerpMetricRow, SeriesCoverage, SeriesId,
    TsRange,
};
use vike_datahub::serve;
use vike_datahub_client::{RemoteHistStore, Request, Response, read_frame, write_frame};
use vike_model::{Bar, EquitySample, SymbolProperties};

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

/// The GUI served-read verbs plumb end-to-end over the store double. `MemHistStore` stores no
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

/// The read-only contract: every write verb and every STILL-non-served read verb returns `Err`
/// naming the method, and NEVER touches the network (they short-circuit before dialling). A dummy
/// address the test never binds proves no connection is attempted.
///
/// ⚠ **This test used to cover six more reads, and they moved to
/// [`the_six_wired_reads_dial_instead_of_refusing`] when the wire grew verbs for them**
/// (`docs/decisions/0084-only-the-datahub-touches-the-store.md`): `scan_book_updates`, `scan_depth`,
/// `scan_cohort`, `scan_perp_metrics`, `scan_equity`, `scan_exec_fills`. They are not merely absent
/// from the list below — a verb that stops refusing must be PINNED as dialling, or "we wired it"
/// and "somebody deleted the assertion" look identical in this file forever.
///
/// The four reads that remain here are the whole of what `RemoteHistStore` still cannot ask for,
/// and they are what stops this file passing against a client that had simply stopped refusing
/// anything.
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

    // append_depth is covered by the TRAIT default already (it refuses — "a store that serves no
    // depth lane"); pinned here so a future override can never flip it to a silent Ok. ⚠ Its READ
    // twin is served now and lives in the dialling test below; the WRITE is not and stays here,
    // which is the asymmetry this line exists to make visible.
    assert!(store.append_depth(VENUE, SYMBOL, &[], None).is_err());

    // The four reads still NOT on the wire. Each refuses locally, naming itself.
    let e = store.scan_symbol_properties(VENUE, SYMBOL, TsRange::all()).unwrap_err();
    assert!(e.to_string().contains("scan_symbol_properties"), "error names the method: {e}");
    assert!(e.to_string().contains("not served"), "error is the read-only message: {e}");
    assert!(store.scan_exec_orders(VENUE, SYMBOL).is_err());
    assert!(store.scan_funding(VENUE, SYMBOL, TsRange::all()).is_err());
    assert!(store.scan_chain(VENUE, SYMBOL, TsRange::all()).is_err());
    assert!(store.chain_as_of(VENUE, SYMBOL, PROPS_TS).is_err());
    assert!(store.chain_as_of_within(VENUE, SYMBOL, PROPS_TS, 60_000).is_err());
}

/// An address nothing is listening on, established by THIS test rather than assumed.
///
/// ⚠ [`writes_and_unsupported_reads_are_read_only_errors`] uses the literal discard port `:9`, which
/// is sound THERE because that test proves nothing is ever dialled — the address is never used. A
/// test that DOES dial wants a port it watched close, so a connect gets an immediate loopback RST
/// instead of depending on what a box happens to run on `:9`.
fn closed_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    drop(listener);
    addr
}

/// ⚠ **THE MIRROR PIN, and it is the assertion this file was missing on the day the wire grew.**
///
/// The six reads `docs/decisions/0084-only-the-datahub-touches-the-store.md` added must now DIAL.
/// Against a closed address each therefore fails with a TRANSPORT error — and specifically NOT with
/// the local `not served` refusal, which is the failure mode being pinned against: the first
/// attempt at that change wired the protocol, the client, the server and both refusal mirrors and
/// left `RemoteHistStore` refusing all six, so the wire could answer and nothing asked it.
///
/// Asserting the ABSENCE of the refusal message is what makes this test catch that, and it is why
/// the test does not simply assert `is_err()` — a refusal is an `Err` too, and the broken state
/// would have passed.
#[test]
fn the_six_wired_reads_dial_instead_of_refusing() {
    let store = RemoteHistStore::new(closed_addr().to_string());
    let all = TsRange::all();

    let errs = [
        ("scan_book_updates", store.scan_book_updates(VENUE, SYMBOL, all).unwrap_err()),
        ("scan_depth", store.scan_depth(VENUE, SYMBOL, all).unwrap_err()),
        ("scan_cohort", store.scan_cohort(VENUE, SYMBOL, all).unwrap_err()),
        ("scan_perp_metrics", store.scan_perp_metrics(VENUE, SYMBOL, all).unwrap_err()),
        ("scan_equity", store.scan_equity(VENUE, SYMBOL, all).unwrap_err()),
        ("scan_exec_fills", store.scan_exec_fills(VENUE, SYMBOL).unwrap_err()),
    ];
    for (name, e) in errs {
        let msg = e.to_string();
        assert!(
            !msg.contains("not served"),
            "{name} is on the wire now and must DIAL, not refuse locally: {msg}"
        );
    }
}

/// FOUR of the six carry real rows over the store DOUBLE — in the fast roster lane, on every PR.
///
/// ⚠ This is deliberate coverage overlap with
/// `crates/vike-datahub/tests/wire_six_verbs_roundtrip.rs`, and the overlap is the point: that file
/// is behind `serve-datafusion` and runs in a FEATURE lane, which fires only when the plan says the
/// change touches it. These four seams are ones `MemHistStore` genuinely stores (see this file's
/// module doc), so they cost nothing to prove here and they are proven on every PR.
///
/// `scan_book_updates` and `scan_depth` are NOT here: the double stubs both to an empty answer, so
/// a round-trip over it could only ever assert `is_empty()` — which is exactly the vacuous shape
/// that made the composed suite necessary. Their real-row proof is that file's, and only that
/// file's.
#[test]
fn four_of_the_six_wired_reads_carry_real_rows_over_the_double() {
    let local = MemHistStore::new();

    let equity = vec![EquitySample {
        ts: 1_000,
        venue: VENUE.to_string(),
        equity: 10_125.5,
        realized: 100.25,
        unrealized: 25.25,
        missing_prices: 1,
    }];
    let fills = vec![ExecFillRow {
        ts: 1_000,
        trade_id: "t-1".to_string(),
        client_order_id: "coid-1".to_string(),
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        side: 1,
        qty: 0.5,
        px: 100.25,
        commission: 0.05,
        mark_price: None,
        liquidity_side: "maker".to_string(),
        commission_asset: "USDT".to_string(),
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
    // would leave every read below vacuously empty, and this test would then be gating the wire
    // against an empty store instead of against the wire.
    //
    // ⚠ **FOUR DISTINCT COMMIT KEYS, and reusing one is not a style point.** `MemHistStore`'s
    // `seen` is ONE `HashSet<String>` across every kind — deliberately, because a real
    // `commit_key` is a caller-chosen string with no kind scoping either — so a second append
    // under a key already spent is a silent batch-level no-op returning `Ok(0)`. Written with one
    // shared key this test failed on its second seed, which is the double behaving exactly as
    // documented and the caller getting the contract wrong.
    assert!(local.append_equity(VENUE, SYMBOL, &equity, Some("seed-equity")).unwrap() > 0);
    assert!(local.append_exec_fills(VENUE, SYMBOL, &fills, Some("seed-fills")).unwrap() > 0);
    assert!(local.append_cohort(VENUE, "BTC", &cohort, Some("seed-cohort")).unwrap() > 0);
    assert!(local.append_perp_metrics(VENUE, SYMBOL, &perp, Some("seed-perp")).unwrap() > 0);

    let local: Arc<dyn HistStore + Send + Sync> = Arc::new(local);
    let addr = spawn_server(local.clone());
    let remote = RemoteHistStore::new(addr.to_string());
    let all = TsRange::all();

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
    let mut buf: Vec<u8> = Vec::new();
    let req = Request::LoadBars {
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        interval: INTERVAL.to_string(),
        start: Some(0),
        end: Some(10_000),
        limit: None,
    };
    write_frame(&mut buf, &req).unwrap();
    write_frame(&mut buf, &Response::Bars(sample_bars())).unwrap();

    let mut cur = Cursor::new(buf);
    let got_req: Request = read_frame(&mut cur).unwrap();
    match got_req {
        Request::LoadBars { venue, symbol, interval, start, end, limit } => {
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
