//! The COMPOSED-PATH gate for the backfill-on-demand verb (split-plane REQ-9, §3): a real
//! `DataFusionHist` over a temp dir, served by `vike_datahub::serve_with_backfill` on an ephemeral
//! loopback port, driven through `DatahubClient::backfill` — request in, rows in the REAL store,
//! `BackfillDone` counts out, and a follow-up `LoadBars` over the SAME wire returning the bars
//! bit-exact.
//!
//! The collector seam is FAKE here, deliberately: the real collectors (the rows of
//! `vike_datahub::backfill::KLINE_SOURCES`) do venue REST I/O, and CI is deterministic and
//! network-free. `BackfillTable` is the injection point — the server dispatches
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
use vike_data::source::FUNDING_INTERVAL;
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_datahub::backfill::{BackfillLane, BackfillTable, real_backfill_table};
use vike_datahub::serve_with_backfill;
use vike_datahub_client::{DatahubClient, FEATURE_BACKFILL, FEATURE_BACKFILL_FUNDING};
use vike_model::Bar;
use vike_model::QuoteTick;

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

/// The OANDA token provider every REAL table in this file is built with. It answers
/// `NotConfigured`, so the credentialed row — built here and never called — could not reach
/// OANDA's practice host even if a test did call it.
fn no_oanda_token() -> vike_oanda::HistoryTokenProvider {
    Arc::new(|| Err(vike_oanda::HistoryTokenError::NotConfigured))
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
            Box::new(
                move |symbol: &str,
                      interval: &str,
                      _start: i64,
                      _end: i64,
                      _: &dyn Fn() -> bool| {
                    let bars = fetched_bars();
                    store
                        .append_bars(VENUE, symbol, interval, &bars, Some("fake-collector"))
                        .map_err(|e| e.to_string())
                },
            ),
        ),
        (
            "bybit".to_string(),
            Box::new(|_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| {
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
        .load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all(), None)
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

/// The REAL table names EVERY venue whose kline collector exists in `vike-backfill` — built, never
/// CALLED (the real collectors do venue REST I/O; only the `#[ignore]`d live paths may drive them).
///
/// ⚠ It named three until 0059 Phase 2, while six collectors were written. This assertion is a PIN,
/// not the gate: `crates/vike-ops/tests/collector_dispatch_gate.rs` derives the population from the
/// real `crates/vike-backfill/src/` tree and is what fails when a seventh collector is written and
/// never dispatched. The two are deliberately both present — this one runs in the
/// `backfill-serve` feature lane and catches a reordering or a dropped entry at the table's own
/// type, which a text scan cannot see.
///
/// ⚠ **oanda is in `supported()` and NOT in `seed_supported()`, and both halves are the point**
/// (docs/decisions/0097, verdict 3). Its one row is the CREDENTIALED lane: a `Backfill` (Control
/// scope) reaches it, and the chart seed — an Observe-scope verb — must never, or an Observe
/// client could make the daemon spend the operator's OANDA token.
#[test]
fn the_real_table_names_every_kline_venue() {
    let (_dir, store) = empty_store();
    let table = real_backfill_table(store, no_oanda_token());
    assert_eq!(
        table.supported(),
        vec!["binance", "bybit", "okx", "aster", "deribit", "hyperliquid", "dukascopy", "oanda"]
    );
    assert_eq!(
        table.seed_supported(),
        vec!["binance", "bybit", "okx", "aster", "deribit", "hyperliquid"]
    );
    assert!(table.get("oanda", "5s").is_some(), "a Backfill reaches the credentialed row");
    assert!(table.get_for_seed("oanda").is_none(), "the chart seed never reaches it");
    assert!(
        table.get("oanda", FUNDING_INTERVAL).is_none(),
        "the credentialed row is a BAR lane: the funding label does not reach it"
    );
}

/// The real table's KLINE lane names the registry's own venues, read at run time — a table that
/// grows a hand-written `Klines` entry would add a venue this list does not have.
///
/// ⚠ Compared against [`BackfillTable::seed_supported`], not `supported()`: `supported()` is
/// documented as "klines AND tick-resampled bars", and since dukascopy's hand-written `TickBars`
/// row joined `real_backfill_table` it deliberately outgrows the kline registry by exactly that one
/// venue — see `the_real_table_names_every_kline_venue`, which pins the wider set.
/// `seed_supported()` is Klines-only by construction, which is the property this test is actually
/// checking.
#[test]
fn the_real_table_is_the_registry_and_not_a_second_list() {
    let (_dir, store) = empty_store();
    let table = real_backfill_table(store, no_oanda_token());
    let registry: Vec<&str> =
        vike_datahub::backfill::KLINE_SOURCES.iter().map(|s| s.venue()).collect();
    assert_eq!(table.seed_supported(), registry);
}

/// The registry's declared order — what the unknown-venue refusal prints.
#[test]
fn the_kline_registry_keeps_its_declared_order() {
    let venues: Vec<&str> =
        vike_datahub::backfill::KLINE_SOURCES.iter().map(|s| s.venue()).collect();
    assert_eq!(venues, vec!["binance", "bybit", "okx", "aster", "deribit", "hyperliquid"]);
}

/// An interval the STORE cannot measure is refused BEFORE the venue is looked up and before any
/// collector runs — the forming-bar refusal `backfill_verb`'s doc argues.
///
/// The three spellings below are the ones that reach it: `vike_model::time::interval_ms` splits on
/// a single trailing `s`/`m`/`h`/`d`, so a week, a month and `1mo` all have no width. Without this
/// gate `drop_forming_tail` declines silently, the venue's OPEN candle is stored as closed, and the
/// window's commit key is spent — which makes the corrective re-fetch a zero-row success.
#[test]
fn an_unmeasurable_interval_is_refused_before_the_collector_runs() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table(store.clone()));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");

    for interval in ["1w", "1M", "1mo"] {
        let err = client
            .backfill(VENUE, SYMBOL, interval, 0, 4 * HOUR_MS)
            .expect_err("an unmeasurable interval must be refused");
        assert!(err.contains("no bar width"), "names what is wrong: {err}");
        assert!(err.contains("commit key"), "names what it prevents: {err}");

        let bars = store.load_bars(VENUE, SYMBOL, interval, TsRange::all()).unwrap();
        assert!(bars.is_empty(), "a refused request writes nothing for {interval}");
    }
}

/// ...and the refusal is about the INTERVAL, not about the venue: a venue this build has no
/// collector for is refused identically, so an operator cannot conclude from the message that the
/// step would have worked somewhere else.
#[test]
fn the_interval_refusal_precedes_the_venue_lookup() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table(store));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");

    let err = client.backfill("not-a-venue", SYMBOL, "1w", 0, 4 * HOUR_MS).expect_err("refused");
    assert!(err.contains("no bar width"), "the interval answers first: {err}");
    assert!(!err.contains("has no collector"), "the venue lookup never ran: {err}");
}

/// A ZERO-WIDTH interval is refused at the verb, for every lane. `0m` is the case the forming-bar
/// refusal above cannot see: `vike_model::time::interval_ms` MEASURES it, as `Some(0)` — but a
/// bucket of no width is no bar, and the tick lane's resample would divide by it.
///
/// The fake `binance` entry appends [`fetched_bars`] under whatever interval it is handed, so an
/// empty `0m` series afterwards is the proof it never ran; and a venue this build has no collector
/// for gets the same answer, so the refusal precedes the venue lookup too.
#[test]
fn a_zero_width_interval_is_refused_before_the_collector_runs() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table(store.clone()));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");

    let err = client
        .backfill(VENUE, SYMBOL, "0m", 0, HOUR_MS)
        .expect_err("a zero-width interval must be refused");
    assert!(err.contains("zero"), "names what is wrong: {err}");
    assert!(err.contains("\"0m\""), "names the interval: {err}");
    let bars = store.load_bars(VENUE, SYMBOL, "0m", TsRange::all()).unwrap();
    assert!(bars.is_empty(), "the collector never ran, so nothing was written");

    let unknown = client.backfill("not-a-venue", SYMBOL, "0m", 0, HOUR_MS).expect_err("refused");
    assert!(unknown.contains("zero"), "the interval answers first: {unknown}");
    assert!(!unknown.contains("has no collector"), "the venue lookup never ran: {unknown}");
}

fn funding_bar(ts: i64, rate: f64) -> Bar {
    Bar {
        ts,
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: 0.0,
        volume: 0.0,
        funding: Some(rate),
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// [`fake_table`] plus a FUNDING entry for binance that writes two funding points through the
/// same store handle, as the real lane's `backfill_funding_rate` does.
fn fake_table_with_funding(store: Arc<DataFusionHist>) -> BackfillTable {
    let funding_store = Arc::clone(&store);
    fake_table(store).with(
        "binance",
        BackfillLane::Funding,
        Box::new(
            move |symbol: &str, interval: &str, _start: i64, _end: i64, _: &dyn Fn() -> bool| {
                assert_eq!(
                    interval, FUNDING_INTERVAL,
                    "the funding lane is reached only for `funding`"
                );
                let bars = vec![funding_bar(HOUR_MS, 0.0001), funding_bar(9 * HOUR_MS, -0.00005)];
                funding_store
                    .append_bars(VENUE, symbol, interval, &bars, Some("fake-funding"))
                    .map_err(|e| e.to_string())
            },
        ),
    )
}

#[test]
fn a_funding_request_reaches_the_funding_lane_and_reads_back_its_bars() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table_with_funding(Arc::clone(&store)));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");
    assert!(
        client.features().iter().any(|f| f == FEATURE_BACKFILL_FUNDING),
        "a table with a funding lane advertises it: {:?}",
        client.features()
    );
    let done = client
        .backfill(VENUE, SYMBOL, FUNDING_INTERVAL, 0, 10 * HOUR_MS)
        .expect("`funding` is a reserved label, not an unmeasurable step");
    assert_eq!(done.rows_written, 2);
    assert_eq!(done.first_ts, Some(HOUR_MS));
    assert_eq!(done.last_ts, Some(9 * HOUR_MS));
}

#[test]
fn a_funding_request_for_a_venue_without_a_funding_source_names_the_funding_set() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table_with_funding(Arc::clone(&store)));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");
    let err = client
        .backfill("bybit", SYMBOL, FUNDING_INTERVAL, 0, HOUR_MS)
        .expect_err("bybit has a kline entry but no funding one");
    assert!(err.contains("no funding-rate collector"), "{err}");
    assert!(err.contains("[binance]"), "names the FUNDING venues, not the kline ones: {err}");
}

#[test]
fn a_server_without_a_funding_lane_is_refused_by_the_client_before_sending() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table(Arc::clone(&store)));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");
    assert!(!client.features().iter().any(|f| f == FEATURE_BACKFILL_FUNDING));
    let err = client
        .backfill(VENUE, SYMBOL, FUNDING_INTERVAL, 0, HOUR_MS)
        .expect_err("no funding lane advertised");
    assert!(err.contains("nothing was sent"), "{err}");
    assert!(err.contains(FEATURE_BACKFILL_FUNDING), "{err}");
}

#[test]
fn the_real_table_serves_funding_for_exactly_its_funding_sources() {
    let (_dir, store) = empty_store();
    let table = real_backfill_table(store, no_oanda_token());
    assert_eq!(table.funding_supported(), vec!["binance", "hyperliquid"]);
    assert!(table.has_funding());
}

/// Five ticks across two 1-minute buckets — the shape `crates/vike-backfill/tests/dukascopy_backfill.rs` uses.
fn synthetic_quotes(symbol: &str) -> Vec<QuoteTick> {
    [
        (0, 1.10001, 1.10003),
        (1_000, 1.10010, 1.10012),
        (2_000, 1.09990, 1.09992),
        (60_000, 1.10100, 1.10103),
        (61_500, 1.10080, 1.10082),
    ]
    .into_iter()
    .map(|(ts, bid, ask)| QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: symbol.to_string(),
    })
    .collect()
}

/// The `TickBars` lane, driven through the same server/client/store seam every other lane in this
/// file is: fetch (fake here — a network fetch would be `vike_dukascopy::fetch_quotes_range`) →
/// store the ticks as quotes → resample into the requested interval → report bars written. Proves
/// both halves of `backfill_quotes_then_bars` land in the SAME store the server serves.
#[test]
fn the_tick_lane_stores_the_quotes_and_answers_with_the_resampled_bars() {
    let (_dir, store) = empty_store();
    let lane_store = Arc::clone(&store);
    let table = BackfillTable::new(Vec::new()).with(
        "dukascopy",
        BackfillLane::TickBars,
        Box::new(
            move |symbol: &str,
                  interval: &str,
                  start: i64,
                  end: i64,
                  should_stop: &dyn Fn() -> bool| {
                vike_backfill::venues::dukascopy::backfill_quotes_then_bars(
                    &lane_store,
                    symbol,
                    interval,
                    start,
                    end,
                    should_stop,
                    |sym, _, _| Ok(synthetic_quotes(sym)),
                )
                .map_err(|e| e.to_string())
            },
        ),
    );
    let addr = spawn_backfill_server(store.clone(), table);
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");
    let done = client
        .backfill("dukascopy", "EURUSD", "1m", 0, 119_999)
        .expect("the tick lane answers a bar interval");
    assert_eq!(done.rows_written, 2, "two 1m buckets resampled from five ticks");
    let quotes = store.scan_quotes("dukascopy", "EURUSD", TsRange::all()).unwrap();
    assert_eq!(quotes.len(), 5, "the ticks themselves were stored as quotes");
}

/// [`BackfillTable::get_for_seed`] restricts the chart seed to [`BackfillLane::Klines`] — a
/// `TickBars` entry is reachable through [`BackfillTable::get`] (the `Backfill` verb) but not
/// through the seed lookup, so opening a chart on dukascopy can never start a tick download.
#[test]
fn a_chart_seed_never_reaches_the_tick_lane() {
    let table = BackfillTable::new(Vec::new()).with(
        "dukascopy",
        BackfillLane::TickBars,
        Box::new(|_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| Ok(0)),
    );
    assert!(table.get("dukascopy", "1m").is_some());
    assert!(
        table.get_for_seed("dukascopy").is_none(),
        "a chart open must not start a tick download"
    );
    assert!(table.seed_supported().is_empty());
}
