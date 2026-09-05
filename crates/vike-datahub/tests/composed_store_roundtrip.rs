//! The COMPOSED-PATH gate for the datahub wire: a real `DataFusionHist` (seeded over a temp dir)
//! served by `vike_datahub::serve` on an ephemeral loopback port, read back through
//! `vike_datahub_client::RemoteHistStore` — the `HistStore`-over-RPC client — with REAL data
//! crossing the wire and coming back equal.
//!
//! Why this file exists when `crates/vike-datahub-client/tests/remote_roundtrip.rs` already runs
//! the same server↔client pair: that suite drives the server over `MemHistStore` (vike-data's
//! `test-support` double), which stores no bars — so its `served_reads_plumb_end_to_end` can only
//! ever assert `is_empty()`, and while its `metadata_verbs_plumb_end_to_end` now carries the
//! double's real (properties-seam) catalog, a BAR series with real manifest-fold coverage can
//! only cross the wire here. The frame codec is proven there over in-memory buffers, and the DataFusion
//! delegation is proven in `crates/vike-data/tests/hist_datafusion.rs` — but the COMPOSITION
//! (DataFusion-backed server ↔ `RemoteHistStore` over a real TCP socket) had never carried a
//! non-empty answer in any test. The split-plane design (Phase 3, "Studio & data remote") mandates
//! closing exactly that hole BEFORE any Studio-remote work builds on the path, because
//! `RemoteHistStore` is complete yet has zero production callers — its first consumer must not also
//! be its first integration test.
//!
//! The whole file is behind `serve-datafusion` (the feature that makes `DataFusionHist` nameable
//! here), exactly like its sibling `run_slice_roundtrip.rs`; CI runs it in the hist job's
//! `cargo test -p vike-datahub --features serve-datafusion`. A default `cargo test -p vike-datahub`
//! compiles this file to nothing.
#![cfg(feature = "serve-datafusion")]

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use tempfile::TempDir;
use vike_data::{DataFusionHist, HistStore, SeriesId, TsRange};
use vike_datahub::serve;
use vike_datahub_client::RemoteHistStore;
use vike_model::{Bar, SymbolProperties};

const VENUE: &str = "binance";
const SYMBOL: &str = "COMPOSEDUSDT";
const INTERVAL: &str = "1h";

/// Epoch-ms per UTC day — the store's `date=` partition granularity (matches
/// `vike_model::time::epoch_ms_to_utc_date`'s day-floor, and the `DAY_MS` the store's own gap
/// helpers use).
const DAY_MS: i64 = 86_400_000;

/// Observation ts of the one seeded properties row.
const PROPS_TS: i64 = 3_600_000;

/// The seeded bars — the single source of truth for the seed and every expected assert. Three bars
/// on UTC day 0 and two on UTC day 2, DELIBERATELY skipping day 1: that is what makes
/// `series_gaps` answer non-empty (a gap exists only between two recorded days), so the verb is
/// proven to carry a real hole across the wire rather than a vacuous `[]`.
fn seeded_bars() -> Vec<Bar> {
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
        // UTC day 0
        bar(3_600_000, 101.25, 103.5, 100.125, 102.75, 11.5, Some(0.0001)),
        bar(7_200_000, 102.75, 104.0, 101.5, 103.25, 7.25, None),
        bar(10_800_000, 103.25, 105.125, 102.0, 104.5, 3.75, Some(-0.0002)),
        // UTC day 2 (day 1 is the seeded gap)
        bar(2 * DAY_MS + 3_600_000, 104.5, 106.0, 103.75, 105.5, 9.0, None),
        bar(2 * DAY_MS + 7_200_000, 105.5, 107.25, 104.25, 106.125, 4.5, Some(0.0003)),
    ]
}

/// The seeded properties value — distinctive, non-default fields so equality is meaningful.
fn sample_props() -> SymbolProperties {
    SymbolProperties {
        tick_size: 0.25,
        step_size: 0.01,
        min_qty: 0.01,
        min_notional: 12.5,
        ..Default::default()
    }
}

/// The `SeriesId` the seeded bars land under (the store's `kind=bar` per-symbol layout).
fn bar_series_id() -> SeriesId {
    SeriesId::per_symbol("bar", VENUE, SYMBOL, Some(INTERVAL.to_string()))
}

/// A `DataFusionHist` over a temp dir, seeded with [`seeded_bars`] + one properties row. The
/// `TempDir` is returned so the caller keeps it alive for the store's lifetime; the concrete
/// `Arc<DataFusionHist>` is returned (not the trait object) so a test can also read the store
/// LOCALLY and assert the wire answer equals the direct in-process answer.
fn seeded_store() -> (TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let bars = seeded_bars();
    let n = store.append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some("seed-bars")).unwrap();
    assert_eq!(n, bars.len(), "every seeded bar lands");
    let rows = [(PROPS_TS, sample_props())];
    let n = store.append_symbol_properties(VENUE, SYMBOL, &rows, Some("seed-props")).unwrap();
    assert_eq!(n, 1, "one properties row seeded");
    (dir, Arc::new(store))
}

/// Bind an ephemeral loopback listener, spawn `serve` over `store` on a detached thread, and return
/// the assigned address for a client to connect to (the sibling suites' convention).
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

/// Bit-exact bar comparison (ts + OHLCV + funding via `f64::to_bits`) — the same discipline
/// `crates/vike-data/tests/hist_datafusion.rs`'s `assert_bars_bit_eq` applies to the local store,
/// now applied across the wire: JSON frames must not perturb a single bit of any float.
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

/// The seeded bars cross the wire bit-exactly, and the `TsRange` bounds are honoured server-side:
/// an unbounded read returns every bar, a `[0, day0_end]` read returns exactly the day-0 bars, and
/// a start-only read returns exactly the day-2 bars.
#[test]
fn bars_round_trip_bit_exact_over_the_composed_path() {
    let (_dir, store) = seeded_store();
    let addr = spawn_server(store);
    let remote = RemoteHistStore::new(addr.to_string());

    let all = remote.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).expect("load_bars ok");
    assert_bars_bit_eq(&seeded_bars(), &all);

    let day0 = remote
        .load_bars(VENUE, SYMBOL, INTERVAL, TsRange::of(0, DAY_MS - 1))
        .expect("bounded load_bars ok");
    assert_bars_bit_eq(&seeded_bars()[..3], &day0);

    let from_day2 = remote
        .load_bars(VENUE, SYMBOL, INTERVAL, TsRange { start: Some(2 * DAY_MS), end: None })
        .expect("start-only load_bars ok");
    assert_bars_bit_eq(&seeded_bars()[3..], &from_day2);
}

/// The three store-metadata verbs answer NON-EMPTY over the composed path, equal to the direct
/// in-process `DataFusionHist` answer, and semantically correct for the seed: the catalog names the
/// seeded series, the inventory row carries its true coverage, and `series_gaps` reports the one
/// deliberately-seeded missing day as a whole-day ms range.
#[test]
fn metadata_verbs_carry_real_answers_over_the_composed_path() {
    let (_dir, store) = seeded_store();
    let addr = spawn_server(store.clone());
    let remote = RemoteHistStore::new(addr.to_string());

    // list_series: the wire catalog equals the local one, and names both seeded series.
    let series = remote.list_series().expect("list_series ok");
    assert_eq!(series, store.list_series().unwrap(), "wire catalog == local catalog");
    assert!(series.contains(&bar_series_id()), "the seeded bar series is named: {series:?}");
    assert!(
        series.contains(&SeriesId::per_symbol("properties", VENUE, SYMBOL, None)),
        "the seeded properties series is named: {series:?}"
    );

    // inventory: wire == local, and the bar row's coverage is the seed's truth.
    let inv = remote.inventory().expect("inventory ok");
    assert_eq!(inv, store.inventory().unwrap(), "wire inventory == local inventory");
    let (_, cov) = inv
        .iter()
        .find(|(id, _)| *id == bar_series_id())
        .expect("inventory carries the seeded bar series");
    assert_eq!(cov.rows, seeded_bars().len() as u64, "rows == seeded bar count");
    assert_eq!(cov.first_ts, 3_600_000, "first_ts is the first seeded bar");
    assert_eq!(cov.last_ts, 2 * DAY_MS + 7_200_000, "last_ts is the last seeded bar");
    assert_eq!(cov.dates, 2, "two distinct date= partitions (day 0 and day 2)");
    assert!(cov.parts >= 2, "at least one sealed part per date: {}", cov.parts);
    assert!(cov.bytes > 0, "real Parquet bytes on disk");

    // series_gaps: the seeded missing day 1 comes back as its whole-day inclusive ms range.
    let gaps = remote.series_gaps(&bar_series_id()).expect("series_gaps ok");
    assert_eq!(gaps, vec![(DAY_MS, 2 * DAY_MS - 1)], "day 1 is the one seeded gap");
    assert_eq!(gaps, store.series_gaps(&bar_series_id()).unwrap(), "wire gaps == local gaps");
}

// ---- the cross-kind coverage verb (spec §6-Q2) -------------------------------------------------

/// The symbol the cross-kind seed uses — its OWN store, deliberately not [`SYMBOL`]'s: the
/// coverage report is whole-store, so seeding tick kinds into the shared fixture would change what
/// every other test in this file sees for no reason.
const CROSS_SYMBOL: &str = "CROSSKINDUSDT";

/// A `DataFusionHist` seeded so its cross-kind report is genuinely PARTIAL: trades on UTC days 0
/// and 2, quotes on day 0 ONLY. Both kinds are therefore RECORDED for the instrument (so
/// `partial_days` compares them rather than short-circuiting on "fewer than two recorded kinds"),
/// and day 2 has trades with no quotes — one partial day, naming `quote` as missing.
///
/// Bars are deliberately NOT seeded here: `TICK_KINDS` excludes them, so a bars-only store reports
/// NOTHING and every assertion below would pass vacuously.
fn cross_kind_store() -> (TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let trade = |ts: i64, price: f64| vike_model::TradeTick {
        ts,
        local_ts: 0,
        price,
        size: 1.5,
        is_buyer_maker: false,
        symbol: CROSS_SYMBOL.to_string(),
    };
    let quote = |ts: i64, bid: f64| vike_model::QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask: bid + 0.5,
        bid_size: 2.0,
        ask_size: 3.0,
        symbol: CROSS_SYMBOL.to_string(),
    };
    let trades = [trade(3_600_000, 100.0), trade(2 * DAY_MS + 3_600_000, 101.0)];
    let n = store.append_trades(VENUE, CROSS_SYMBOL, &trades, Some("seed-trades")).unwrap();
    assert_eq!(n, trades.len(), "every seeded trade lands");
    let quotes = [quote(3_600_000, 99.5)];
    let n = store.append_quotes(VENUE, CROSS_SYMBOL, &quotes, Some("seed-quotes")).unwrap();
    assert_eq!(n, quotes.len(), "every seeded quote lands");
    (dir, Arc::new(store))
}

/// THE §6-Q2 gate: the coverage report a client folds from the WIRE equals, entry for entry, the
/// report the server's own `DataFusionHist` produces IN PROCESS for the same store — the wire adds
/// nothing and loses nothing.
///
/// Equality alone could pass on two empty vectors, so the test also pins the seed's SEMANTICS: the
/// report names the seeded instrument, both kinds read as recorded, and `partial_days` — the fold
/// the Data-Manager's Partial column actually renders — finds day 2 missing `quote`. If the verb
/// ever degraded to the trait's empty default (server-side or in `RemoteHistStore`), the equality
/// would still hold and these three assertions are what would catch it.
#[test]
fn the_coverage_report_over_the_wire_equals_the_in_process_report() {
    let (_dir, store) = cross_kind_store();
    let addr = spawn_server(store.clone());
    let remote = RemoteHistStore::new(addr.to_string());

    let wire = remote.coverage_report().expect("coverage_report over the wire");
    let local = store.coverage_report().expect("coverage_report in process");
    assert_eq!(wire, local, "the wire report must equal the in-process report exactly");

    let entry = wire
        .iter()
        .find(|c| c.key.venue == VENUE && c.key.label == CROSS_SYMBOL)
        .expect("the seeded instrument is in the report");
    assert_eq!(
        entry.recorded_kinds(),
        vec!["trade", "quote"],
        "both seeded kinds read as recorded (in TICK_KINDS order)"
    );
    let partial = entry.partial_days();
    assert_eq!(partial.len(), 1, "exactly the one seeded partial day: {partial:?}");
    assert_eq!(partial[0].day, 2, "day 2 is the day with trades and no quotes");
    assert_eq!(partial[0].missing_kinds, vec!["quote".to_string()], "quote is what day 2 lacks");
}

/// The Data-Manager's actual render input, end to end: the per-instrument `partial_days()` fold —
/// the step `vike_data_manager::partial_days_from_coverage` performs over each entry — is identical
/// on both sides of the wire. Asserted through `InstrumentCoverage`'s own method rather than
/// through the data-manager helper so this suite stays free of the GUI crate's dependency tree;
/// `vike_app_core::stored_load`'s tests pin the helper itself.
#[test]
fn the_partial_day_fold_is_identical_on_both_sides_of_the_wire() {
    let (_dir, store) = cross_kind_store();
    let addr = spawn_server(store.clone());
    let remote = RemoteHistStore::new(addr.to_string());

    let fold = |report: &[vike_data::InstrumentCoverage]| {
        report.iter().map(|c| (c.key.clone(), c.partial_days())).collect::<Vec<_>>()
    };
    let from_wire = fold(&remote.coverage_report().unwrap());
    let from_local = fold(&store.coverage_report().unwrap());
    assert_eq!(from_wire, from_local, "one fold, two stores — the column cannot differ");
    assert!(
        from_wire.iter().any(|(_, days)| !days.is_empty()),
        "the seed must actually produce a partial day"
    );
}

/// A store with NOTHING partial answers an `Ok` report over the wire — not an error, and not the
/// negotiation refusal. "Nothing is partial" and "I cannot ask" must stay distinguishable, since
/// the GUI renders the first as a blank column and the second as a note.
#[test]
fn a_store_with_no_tick_kinds_answers_an_ok_empty_report() {
    let (_dir, store) = seeded_store(); // bars + properties only — no TICK_KINDS series at all
    let addr = spawn_server(store.clone());
    let remote = RemoteHistStore::new(addr.to_string());

    let wire = remote.coverage_report().expect("an empty report is still an answer");
    assert_eq!(wire, store.coverage_report().unwrap(), "wire report == local report");
    assert!(wire.is_empty(), "a bars-only store has no cross-kind instruments to report");
}

/// The server ADVERTISES the verb, which is the whole of its negotiation (there is no table to
/// mount — it is a plain trait verb, so every build that serves at all serves it). The client-side
/// refusal against a server that does NOT advertise is pinned in
/// `crates/vike-datahub-client/tests/coverage_negotiation.rs`, the fast lane.
#[test]
fn the_server_advertises_the_coverage_capability() {
    let (_dir, store) = cross_kind_store();
    let addr = spawn_server(store);
    let client = vike_datahub_client::DatahubClient::connect(addr).expect("handshake on connect");
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_COVERAGE),
        "every serving build advertises `{}`: {:?}",
        vike_datahub_client::FEATURE_COVERAGE,
        client.features()
    );
}

/// The seeded `SymbolProperties` value crosses the wire and comes back equal; before its
/// observation ts the point-in-time lookup answers `None`.
#[test]
fn properties_as_of_round_trips_real_value_over_the_composed_path() {
    let (_dir, store) = seeded_store();
    let addr = spawn_server(store);
    let remote = RemoteHistStore::new(addr.to_string());

    let got = remote.properties_as_of(VENUE, SYMBOL, PROPS_TS + 500).expect("properties_as_of ok");
    assert_eq!(got, Some(sample_props()), "the seeded properties round-trip whole");

    let before = remote.properties_as_of(VENUE, SYMBOL, PROPS_TS - 1).expect("properties_as_of ok");
    assert_eq!(before, None, "nothing observed before the seed ts");
}

/// The negative contract: a series that was never seeded answers EMPTY (`Ok(vec![])` /
/// `Ok(None)` — the trait's documented no-data shape) over the composed path, not an error and not
/// a hang. `DataFusionHist` treats an absent series dir as an empty manifest, and that answer must
/// survive the wire unchanged.
#[test]
fn an_unknown_series_answers_empty_over_the_composed_path() {
    let (_dir, store) = seeded_store();
    let addr = spawn_server(store);
    let remote = RemoteHistStore::new(addr.to_string());

    let bars = remote.load_bars(VENUE, "NOSUCHUSDT", INTERVAL, TsRange::all()).expect("ok");
    assert!(bars.is_empty(), "an unseeded symbol has no bars");

    let unknown = SeriesId::per_symbol("bar", VENUE, "NOSUCHUSDT", Some(INTERVAL.to_string()));
    let gaps = remote.series_gaps(&unknown).expect("ok");
    assert!(gaps.is_empty(), "an unseeded series has no gaps to report");

    let props = remote.properties_as_of(VENUE, "NOSUCHUSDT", PROPS_TS + 500).expect("ok");
    assert_eq!(props, None, "an unseeded symbol has no properties");
}
