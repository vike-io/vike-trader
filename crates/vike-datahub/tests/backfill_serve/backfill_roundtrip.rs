//! The COMPOSED-PATH gate for the backfill-on-demand verb (split-plane REQ-9, §3): a real
//! `HistoryStoreDataFusion` over a temp dir, served by `vike_datahub::serve_with_backfill` on an ephemeral
//! loopback port, driven through `DatahubClient::backfill` — request in, rows in the REAL store,
//! `BackfillDone` counts out, and a follow-up `LoadBars` over the SAME wire returning the bars
//! bit-exact.
//!
//! The collector seam is FAKE here, deliberately: the real collectors (the rows
//! `vike_datahub::backfill::real_backfill_table` folds out of the gatewayhub client's rosters) ask
//! the gatewayhub, which does the venue REST I/O, and CI is deterministic and network-free.
//! `BackfillTable` is the injection point — the server dispatches
//! venue → collector through it, the bin installs the real table
//! (`vike_datahub::backfill::real_backfill_table`), and this test installs closures that write
//! KNOWN bars through the SAME `Arc<HistoryStoreDataFusion>` the server serves — which is exactly the
//! write-through-before-reply contract ("writers live next to the data") the verb exists to keep.
//! The REAL table is driven in the last section of this file, over `vike_gatewayhub::testkit`'s
//! scripted gateway: the sources it builds are the production ones, the daemon's answers are
//! a test's, and what it was asked is read back.
//!
//! The whole file is behind `backfill-serve` (the feature that adds the collector tree to the
//! server build); CI runs it in the hist job's `cargo test -p vike-datahub --features
//! backfill-serve`. A default or `datafusion-serve`-only build compiles this file to nothing.
#![cfg(feature = "backfill-serve")]

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use tempfile::TempDir;
use vike_catalog::HistoryChannelLane;
use vike_data::source::FUNDING_INTERVAL;
use vike_data::{HistoryStore, TimestampRange};
use vike_datahub::backfill::{BackfillTable, real_backfill_table};
use vike_datahub::serve_with_backfill;
use vike_datahub_client::{DatahubClient, FEATURE_BACKFILL, FEATURE_BACKFILL_FUNDING};
use vike_gatewayhub::testkit::data::DataLaneScript;
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::{Answer, FetchCall, RestLaneScript};
use vike_gatewayhub_client::history::KLINE_VENUES;
use vike_gatewayhub_client::proto::FetchWhat;
use vike_marketdata::Bar;
use vike_marketdata::FundingRatePoint;
use vike_marketdata::TickQuote;
use vike_model::test_support::bars::assert_bars_bit_eq;
use vike_store::HistoryStoreDataFusion;

use vike_datahub::test_support::no_gateway;

const VENUE: &str = "binance";
const SYMBOL: &str = "BACKFILLUSDT";
const INTERVAL: &str = "1h";
const HOUR_MS: i64 = 3_600_000;

/// The bars the FAKE collector "fetches" — the single source of truth for every assert below.
/// Distinctive fractional values so bit-exactness is meaningful (the composed sibling's
/// discipline).
fn fetched_bars() -> Vec<Bar> {
    let bar = |ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64, funding| Bar {
        timestamp: ts,
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

/// A fresh `HistoryStoreDataFusion` over a temp dir. Unlike the composed sibling it starts EMPTY — the
/// verb under test is the thing that writes.
fn empty_store() -> (TempDir, Arc<HistoryStoreDataFusion>) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    (dir, Arc::new(store))
}

/// A fake collector table whose `binance` entry appends [`fetched_bars`] through `store` — the
/// SAME handle the server serves — mimicking exactly what the real collectors do (fetch, then
/// `append_bars`), minus the network.
fn fake_table(store: Arc<HistoryStoreDataFusion>) -> BackfillTable {
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
                        .map_err(|e| e.to_string().into())
                },
            ),
        ),
        (
            "bybit".to_string(),
            Box::new(|_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| {
                Err("bybit fake collector: simulated venue failure".into())
            }),
        ),
    ])
}

/// Spawn `serve_with_backfill` over `store` + `table` on an ephemeral loopback port.
fn spawn_backfill_server(
    store: Arc<dyn HistoryStore + Send + Sync>,
    table: BackfillTable,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve_with_backfill(listener, store, Some(table));
    });
    addr
}

/// The whole verb, composed: a server WITH a table advertises `backfill`; the request runs the
/// collector; the rows land in the REAL `HistoryStoreDataFusion`; `BackfillDone` reports the true count
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
    assert_eq!(
        done.first_timestamp,
        Some(expected[0].timestamp),
        "first_timestamp is the range's first stored bar"
    );
    assert_eq!(
        done.last_timestamp,
        Some(expected[expected.len() - 1].timestamp),
        "last_timestamp is the range's last stored bar"
    );

    // The write went through the SAME store handle the server serves: the direct local read...
    let local = store.load_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all()).unwrap();
    assert_bars_bit_eq(&expected, &local);

    // ...and the follow-up read over the SAME wire both see it.
    let over_wire = client
        .load_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all(), None)
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

    let bars = store.load_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all()).unwrap();
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

    let bars = store.load_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all()).unwrap();
    assert!(bars.is_empty(), "a refused request writes nothing");
}

/// The REAL table names EVERY venue whose kline collector exists in `vike-backfill` — built, never
/// CALLED (the real collectors do venue REST I/O; only the `#[ignore]`d live paths may drive them).
///
/// ⚠ It named three until 0059 Phase 2, while six collectors were written. This assertion is a PIN,
/// not the gate: `crates/vike-ops/tests/venues/collector_dispatch_gate.rs` derives the population from the
/// real `crates/vike-backfill/src/` tree and is what fails when a seventh collector is written and
/// never dispatched. The two are deliberately both present — this one runs in every test
/// build and catches a reordering or a dropped entry at the table's own
/// type, which a text scan cannot see.
///
/// ⚠ **oanda and ig are in `supported()` and NOT in `seed_supported()`, and both halves are the
/// point** (docs/decisions/0097, verdict 3, and decision 0194 for ig's login). Each one's row is the
/// CREDENTIALED lane: a `Backfill` (Control scope) reaches it, and the chart seed — an Observe-scope
/// verb — must never, or an Observe client could make the daemon spend the operator's OANDA token or
/// log in to IG.
#[test]
fn the_real_table_names_every_kline_venue() {
    let (_dir, store) = empty_store();
    let table = real_backfill_table(store, no_gateway());
    assert_eq!(
        table.supported(),
        vec![
            "binance",
            "bybit",
            "okx",
            "aster",
            "deribit",
            "hyperliquid",
            "fxcm",
            "dukascopy",
            "oanda",
            "ig",
            "ctrader"
        ]
    );
    assert_eq!(
        table.seed_supported(),
        vec!["binance", "bybit", "okx", "aster", "deribit", "hyperliquid", "fxcm"]
    );
    assert!(table.get("oanda", "5s").is_some(), "a Backfill reaches the credentialed row");
    assert!(table.get_for_seed("oanda").is_none(), "the chart seed never reaches it");
    assert!(table.get("ig", "1h").is_some(), "a Backfill reaches IG's credentialed row");
    assert!(table.get_for_seed("ig").is_none(), "the chart seed never reaches it");
    assert!(table.get("ctrader", "1m").is_some(), "a Backfill reaches cTrader's credentialed row");
    assert!(table.get_for_seed("ctrader").is_none(), "the chart seed never reaches it");
    assert!(
        table.get("oanda", FUNDING_INTERVAL).is_none(),
        "the credentialed row is a BAR lane: the funding label does not reach it"
    );
    assert!(
        table.get("ctrader", FUNDING_INTERVAL).is_none(),
        "cTrader's credentialed row is a BAR lane too"
    );
}

/// The real table's KLINE lane names the roster's own venues, read at run time — a table that
/// grows a hand-written `Klines` entry would add a venue this list does not have.
///
/// ⚠ Compared against [`BackfillTable::seed_supported`], not `supported()`: `supported()` is
/// documented as "klines AND tick-resampled bars", and since dukascopy's hand-written `TickBars`
/// row joined `real_backfill_table` it deliberately outgrows the kline roster by exactly that one
/// venue — see `the_real_table_names_every_kline_venue`, which pins the wider set.
/// `seed_supported()` is Klines-only by construction, which is the property this test is actually
/// checking. The roster is the gatewayhub CLIENT's (`KLINE_VENUES`): the daemon's registry equals
/// it in order, which `crates/vike-gatewayhub`'s own suite pins.
#[test]
fn the_real_table_is_the_roster_and_not_a_second_list() {
    let (_dir, store) = empty_store();
    let table = real_backfill_table(store, no_gateway());
    assert_eq!(table.seed_supported(), KLINE_VENUES.to_vec());
}

/// The kline registry's declared order — what the unknown-venue refusal prints. The order is the
/// gatewayhub client's roster (`KLINE_VENUES`), which the daemon's own registry equals; the test keeps
/// the name `crates/vike-ops/tests/venues/collector_dispatch_gate.rs` cites.
#[test]
fn the_kline_registry_keeps_its_declared_order() {
    assert_eq!(
        KLINE_VENUES,
        ["binance", "bybit", "okx", "aster", "deribit", "hyperliquid", "fxcm"]
    );
}

/// An interval the STORE cannot measure is refused BEFORE the venue is looked up and before any
/// collector runs — the forming-bar refusal `backfill_verb`'s doc argues.
///
/// The three spellings below are the ones that reach it: `vike_marketdata::interval::interval_ms` splits on
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

        let bars = store.load_bars(VENUE, SYMBOL, interval, TimestampRange::all()).unwrap();
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
    assert!(
        err.contains("Refused HERE, before any venue is dispatched to"),
        "the venue lookup never ran: {err}"
    );
}

/// A ZERO-WIDTH interval is refused at the verb, for every lane. `0m` is the case the forming-bar
/// refusal above cannot see: `vike_marketdata::interval::interval_ms` MEASURES it, as `Some(0)` — but a
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
    let bars = store.load_bars(VENUE, SYMBOL, "0m", TimestampRange::all()).unwrap();
    assert!(bars.is_empty(), "the collector never ran, so nothing was written");

    let unknown = client.backfill("not-a-venue", SYMBOL, "0m", 0, HOUR_MS).expect_err("refused");
    assert!(unknown.contains("zero"), "the interval answers first: {unknown}");
    assert!(!unknown.contains("has no collector"), "the venue lookup never ran: {unknown}");
}

/// A second spelling of one width is refused at the verb too: the string becomes the series'
/// `interval=` partition, so `01m` would be a second series a reader of `1m` never sees. Before the
/// venue lookup, like the two refusals above.
#[test]
fn a_second_spelling_of_one_width_is_refused_before_the_collector_runs() {
    let (_dir, store) = empty_store();
    let addr = spawn_backfill_server(store.clone(), fake_table(store.clone()));
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");

    let err = client
        .backfill(VENUE, SYMBOL, "01m", 0, HOUR_MS)
        .expect_err("a second spelling must be refused");
    assert!(err.contains("canonical"), "names what is wrong: {err}");
    assert!(err.contains("\"01m\""), "names the interval: {err}");
    let bars = store.load_bars(VENUE, SYMBOL, "01m", TimestampRange::all()).unwrap();
    assert!(bars.is_empty(), "the collector never ran, so nothing was written");

    let unknown = client.backfill("not-a-venue", SYMBOL, "01m", 0, HOUR_MS).expect_err("refused");
    assert!(unknown.contains("canonical"), "the interval answers first: {unknown}");
}

fn funding_bar(ts: i64, rate: f64) -> Bar {
    Bar {
        timestamp: ts,
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
fn fake_table_with_funding(store: Arc<HistoryStoreDataFusion>) -> BackfillTable {
    let funding_store = Arc::clone(&store);
    fake_table(store).with(
        "binance",
        HistoryChannelLane::Funding,
        Box::new(
            move |symbol: &str, interval: &str, _start: i64, _end: i64, _: &dyn Fn() -> bool| {
                assert_eq!(
                    interval, FUNDING_INTERVAL,
                    "the funding lane is reached only for `funding`"
                );
                let bars = vec![funding_bar(HOUR_MS, 0.0001), funding_bar(9 * HOUR_MS, -0.00005)];
                funding_store
                    .append_bars(VENUE, symbol, interval, &bars, Some("fake-funding"))
                    .map_err(|e| e.to_string().into())
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
    assert_eq!(done.first_timestamp, Some(HOUR_MS));
    assert_eq!(done.last_timestamp, Some(9 * HOUR_MS));
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
    let table = real_backfill_table(store, no_gateway());
    assert_eq!(
        table.funding_supported(),
        vec!["binance", "hyperliquid", "bybit", "okx", "deribit", "aster"]
    );
    assert!(table.has_funding());
}

/// Five ticks across two 1-minute buckets — the shape `crates/vike-backfill/tests/dukascopy_backfill.rs` uses.
fn synthetic_quotes(symbol: &str) -> Vec<TickQuote> {
    [
        (0, 1.10001, 1.10003),
        (1_000, 1.10010, 1.10012),
        (2_000, 1.09990, 1.09992),
        (60_000, 1.10100, 1.10103),
        (61_500, 1.10080, 1.10082),
    ]
    .into_iter()
    .map(|(ts, bid, ask)| TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid,
        ask,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: symbol.into(),
    })
    .collect()
}

/// The `TickBars` lane, driven through the same server/client/store seam every other lane in this
/// file is: fetch (fake here — the real table's fetch is the gatewayhub's tick fetch) →
/// store the ticks as quotes → resample into the requested interval → report bars written. Proves
/// both halves of `backfill_quotes_then_bars` land in the SAME store the server serves.
#[test]
fn the_tick_lane_stores_the_quotes_and_answers_with_the_resampled_bars() {
    let (_dir, store) = empty_store();
    let lane_store = Arc::clone(&store);
    let table = BackfillTable::new(Vec::new()).with(
        "dukascopy",
        HistoryChannelLane::TickBars,
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
                    |symbol_name, _, _| Ok(synthetic_quotes(symbol_name)),
                )
                .map_err(|e| e.carried())
            },
        ),
    );
    let addr = spawn_backfill_server(store.clone(), table);
    let mut client = DatahubClient::connect(addr).expect("handshake on connect");
    let done = client
        .backfill("dukascopy", "EURUSD", "1m", 0, 119_999)
        .expect("the tick lane answers a bar interval");
    assert_eq!(done.rows_written, 2, "two 1m buckets resampled from five ticks");
    let quotes = store.scan_quotes("dukascopy", "EURUSD", TimestampRange::all()).unwrap();
    assert_eq!(quotes.len(), 5, "the ticks themselves were stored as quotes");
}

/// [`BackfillTable::get_for_seed`] restricts the chart seed to [`HistoryChannelLane::Klines`] — a
/// `TickBars` entry is reachable through [`BackfillTable::get`] (the `Backfill` verb) but not
/// through the seed lookup, so opening a chart on dukascopy can never start a tick download.
#[test]
fn a_chart_seed_never_reaches_the_tick_lane() {
    let table = BackfillTable::new(Vec::new()).with(
        "dukascopy",
        HistoryChannelLane::TickBars,
        Box::new(|_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| Ok(0)),
    );
    assert!(table.get("dukascopy", "1m").is_some());
    assert!(
        table.get_for_seed("dukascopy").is_none(),
        "a chart open must not start a tick download"
    );
    assert!(table.seed_supported().is_empty());
}

// ==================================================================================================
// The REAL table, over a scripted gatewayhub (decision 0164)
// ==================================================================================================
//
// `real_backfill_table` builds its sources from a link to the gatewayhub and fetches nothing
// itself, so these tests give it the testkit's loopback gateway: the daemon's answers are a test's,
// the sources are the production ones, and `rest.fetches()` reads back EXACTLY what the table asked
// (venue, symbol, window, and whether the credentialed verb was the one used). The gateway is
// declared first so it is dropped last.

/// Serve the REAL table over `store` on a fresh loopback port and connect a client to it.
fn serve_real_table(
    store: &Arc<HistoryStoreDataFusion>,
    link: vike_gatewayhub_client::client::GatewayLink,
) -> DatahubClient {
    // The kept rows the history doors find dukascopy's and oanda's `EURUSD` by (decision 0290).
    vike_datahub::test_support::hold_history_lists();
    let addr = spawn_backfill_server(store.clone(), real_backfill_table(Arc::clone(store), link));
    DatahubClient::connect(addr).expect("handshake on connect")
}

/// A kline `Backfill` through the real table is ONE keyless fetch of the request's own window from
/// the gatewayhub, and the rows it answers land in the store bit-exact — the same bytes the
/// in-process path stored, since the wire carries them as they came.
#[test]
fn a_kline_backfill_is_one_keyless_gateway_fetch_and_lands_in_the_store() {
    let rest = RestLaneScript::new().with_klines(VENUE, Answer::rows(fetched_bars()));
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, gateway.link());

    let done = client.backfill(VENUE, SYMBOL, INTERVAL, 0, 4 * HOUR_MS).expect("served");
    assert_eq!(done.rows_written, 3);
    assert_eq!(
        rest.fetches(),
        vec![FetchCall {
            what: FetchWhat::Klines {
                venue: VENUE.into(),
                symbol: SYMBOL.into(),
                interval: INTERVAL.into(),
                start_ms: 0,
                end_ms: 4 * HOUR_MS,
            },
            credentialed: false,
        }],
        "one fetch, the request's own window, never the credentialed verb"
    );
    let stored = store.load_bars(VENUE, SYMBOL, INTERVAL, TimestampRange::all()).unwrap();
    assert_bars_bit_eq(&stored, &fetched_bars());

    let again = client.backfill(VENUE, SYMBOL, INTERVAL, 0, 4 * HOUR_MS).expect("served");
    assert_eq!(again.rows_written, 0, "the window's commit key is spent: a re-run writes nothing");
}

/// Every venue of the KLINE roster is reachable through the real table, each asking the gateway for
/// ITS OWN venue — a fold that handed every row the first venue's source would store one venue's
/// bars under another's name.
#[test]
fn each_roster_venue_asks_the_gateway_for_itself() {
    let mut rest = RestLaneScript::new();
    for venue in KLINE_VENUES {
        rest = rest.with_klines(venue, Answer::rows(fetched_bars()));
    }
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, gateway.link());

    for venue in KLINE_VENUES {
        let done = client.backfill(venue, SYMBOL, INTERVAL, 0, 4 * HOUR_MS).expect(venue);
        assert_eq!(done.rows_written, 3, "{venue}");
    }
    let asked: Vec<String> = rest
        .fetches()
        .into_iter()
        .map(|call| match call.what {
            FetchWhat::Klines { venue, .. } => venue,
            other => panic!("not a kline fetch: {other:?}"),
        })
        .collect();
    assert_eq!(asked, KLINE_VENUES.to_vec(), "one fetch per venue, in the roster's order");
}

/// The FUNDING lane: the request's reserved `funding` label reaches the gateway's funding fetch,
/// and the datahub stores the points it answers as the `funding` series' rows (the rate half; the
/// premium half is `vike-backfill`'s own test) — the daemon only fetched.
#[test]
fn a_funding_backfill_is_one_gateway_funding_fetch() {
    let points = vec![
        FundingRatePoint { timestamp_ms: HOUR_MS, rate: 0.0001, premium: None, mark: None },
        FundingRatePoint {
            timestamp_ms: 9 * HOUR_MS,
            rate: -0.00005,
            premium: Some(0.00125),
            mark: None,
        },
    ];
    let rest = RestLaneScript::new().with_funding(VENUE, Answer::rows(points));
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, gateway.link());

    let done = client.backfill(VENUE, SYMBOL, FUNDING_INTERVAL, 0, 10 * HOUR_MS).expect("served");
    assert_eq!(done.rows_written, 2, "two rate rows");
    assert_eq!((done.first_timestamp, done.last_timestamp), (Some(HOUR_MS), Some(9 * HOUR_MS)));
    assert_eq!(
        rest.fetches(),
        vec![FetchCall {
            what: FetchWhat::Funding {
                venue: VENUE.into(),
                symbol: SYMBOL.into(),
                start_ms: 0,
                end_ms: 10 * HOUR_MS,
            },
            credentialed: false,
        }]
    );
}

/// The TICK lane: dukascopy's ticks come from the gateway's tick fetch, are stored as quotes by
/// the datahub, and the answer is the resampled bar count — `backfill_quotes_then_bars` is the
/// store-and-resample half, and the real table hands it the remote fetch.
#[test]
fn a_dukascopy_backfill_fetches_ticks_from_the_gateway_and_resamples_them() {
    let rest =
        RestLaneScript::new().with_ticks("dukascopy", Answer::rows(synthetic_quotes("EURUSD")));
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, gateway.link());

    let done = client.backfill("dukascopy", "EURUSD", "1m", 0, 119_999).expect("served");
    assert_eq!(done.rows_written, 2, "two 1m buckets resampled from five ticks");
    assert_eq!(store.scan_quotes("dukascopy", "EURUSD", TimestampRange::all()).unwrap().len(), 5);
    let fetches = rest.fetches();
    assert_eq!(fetches.len(), 1, "one chunk, one fetch: {fetches:?}");
    assert!(
        matches!(&fetches[0].what, FetchWhat::Ticks { venue, symbol, .. }
            if venue == "dukascopy" && symbol == "EURUSD"),
        "{:?}",
        fetches[0]
    );
    assert!(!fetches[0].credentialed);
}

/// **A dukascopy request names the instrument's USER name, and one the kept list has no row of is refused**
/// (decision 0290, audit flag DUK-5): the datafeed's own folder name and a second spelling would each be stored
/// as a SECOND series of one instrument. Refused in words before the gatewayhub is asked anything.
#[test]
fn a_dukascopy_request_naming_no_kept_row_is_refused_before_any_fetch() {
    let rest =
        RestLaneScript::new().with_ticks("dukascopy", Answer::rows(synthetic_quotes("EURUSD")));
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, gateway.link());
    for symbol in ["eurusd", "EUR/USD", "AAPLUSUSD"] {
        let err = client.backfill("dukascopy", symbol, "1m", 0, 119_999).expect_err(symbol);
        assert!(err.contains("refused: "), "{symbol}: {err}");
        assert!(err.contains("Nothing was fetched or written"), "{symbol}: {err}");
    }
    assert!(rest.fetches().is_empty(), "the gatewayhub was asked nothing");
    assert!(store.list_series().expect("list").is_empty(), "nothing was written");
}

/// A link to the gatewayhub's address with REAL keys and nothing listening there: the dial is refused
/// by the OS, which is what a gatewayhub that is down looks like (`no_gateway()` has no keys, so it
/// never gets as far as a dial). Port 1, which no test's listener can be handed (a port bound and
/// released can be).
fn dead_gateway() -> vike_gatewayhub_client::client::GatewayLink {
    vike_gatewayhub_client::client::GatewayLink::new(
        "127.0.0.1:1",
        vike_gatewayhub::testkit::gateway::keys(),
    )
}

/// What a link that cannot fetch must do on every lane, whatever the reason: an ERROR naming the
/// gatewayhub (`want` is the reason's own words), a store untouched, and the SAME window still open
/// once a live gateway serves it, because an empty success would have been stored as "the venue had no
/// rows" and spent the window's commit key for good.
fn assert_a_failed_link_errors_every_lane_and_spends_no_commit_key(
    link: vike_gatewayhub_client::client::GatewayLink,
    want: &str,
) {
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, link);
    let day0 = 1_704_153_600_000;
    for (venue, symbol, interval, start, end) in [
        (VENUE, SYMBOL, INTERVAL, 0, 4 * HOUR_MS),
        (VENUE, SYMBOL, FUNDING_INTERVAL, 0, 4 * HOUR_MS),
        ("dukascopy", "EURUSD", "1m", 0, 119_999),
        ("oanda", "EURUSD", "5s", day0, day0 + HOUR_MS),
    ] {
        let err = client
            .backfill(venue, symbol, interval, start, end)
            .expect_err(&format!("{venue}/{interval}: no gateway, no rows"));
        assert!(err.contains("gatewayhub"), "{venue}/{interval}: names the daemon: {err}");
        assert!(err.contains(want), "{venue}/{interval}: says why ({want:?}): {err}");
    }
    assert!(store.list_series().expect("list").is_empty(), "a failed request stores nothing");

    // ...and the SAME window is still open: a live gateway now serves it, so no key was spent.
    let rest = RestLaneScript::new().with_klines(VENUE, Answer::rows(fetched_bars()));
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest);
    let mut client = serve_real_table(&store, gateway.link());
    let done = client.backfill(VENUE, SYMBOL, INTERVAL, 0, 4 * HOUR_MS).expect("served");
    assert_eq!(done.rows_written, 3, "the failed request spent no commit key");
}

/// **A gatewayhub that cannot be reached fails the request — it never stores an empty success.** A
/// transport failure must be an ERROR naming the gatewayhub, on every lane. The link has real keys
/// and a socket nothing listens on, so the failure is the refused DIAL.
#[test]
fn an_unreachable_gateway_is_an_error_on_every_lane_and_spends_no_commit_key() {
    assert_a_failed_link_errors_every_lane_and_spends_no_commit_key(
        dead_gateway(),
        "cannot connect",
    );
}

/// The production link BEFORE the hub keys are provisioned (`no_gateway()`: empty keys): the failure
/// is local, nothing is dialled, and the error names the missing key - which is not the dead-socket
/// error above, and spends no commit key either.
#[test]
fn a_link_without_hub_keys_is_an_error_on_every_lane_and_spends_no_commit_key() {
    assert_a_failed_link_errors_every_lane_and_spends_no_commit_key(no_gateway(), "hub key");
}

/// **A gateway failure keeps its words and its variant.** The daemon refusing a request (the venue
/// was never asked) and the daemon failing it are two different answers the operator reads; both
/// cross the hop with their text intact, `venue fetch: ` prefixed by the ingest exactly as an
/// in-process source's would be.
#[test]
fn the_daemons_refusal_and_failure_texts_reach_the_client_unchanged() {
    let rest = RestLaneScript::new()
        .with_klines("bybit", Answer::refused("bybit refuses this spelling, nothing was asked"))
        .with_klines("okx", Answer::fetch_error("okx answered 429 after three tries"));
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest);
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, gateway.link());

    let refused = client.backfill("bybit", SYMBOL, INTERVAL, 0, HOUR_MS).expect_err("refused");
    assert!(refused.contains("bybit refuses this spelling, nothing was asked"), "{refused}");
    let failed = client.backfill("okx", SYMBOL, INTERVAL, 0, HOUR_MS).expect_err("failed");
    assert!(failed.contains("okx answered 429 after three tries"), "{failed}");
    assert!(failed.contains("venue fetch"), "an ingest-level fetch failure: {failed}");
    assert!(store.list_series().expect("list").is_empty());
}

/// One option trade of 2024-06-03 for the option-trades lane's tests.
fn option_trade(ts: i64, id: &str) -> vike_data::option_trade_log::OptionTradeRow {
    vike_data::option_trade_log::OptionTradeRow {
        ts,
        underlying: "BTC".into(),
        instrument: "BTC-21JUN24-94000-C".into(),
        expiry_ms: 1_718_956_800_000,
        strike: 94_000.0,
        kind: vike_marketdata::OptionKind::Call,
        trade_id: id.into(),
        price: 0.0007,
        amount: 2.7,
        is_buyer_maker: true,
        iv: Some(0.6337),
        mark_price: Some(0.000_784_82),
        index_price: 67_735.8,
        premium_in_coin: true,
    }
}

/// The OPTION-TRADES lane: the reserved `option-trades` label reaches the gatewayhub's option-trades fetch,
/// ONE fetch per whole settled UTC day (the request's window rounded outward), and the datahub stores the
/// trades AND the hourly chain derived from them; the read-back is the trade series' own edges, and a
/// repeat asks the gatewayhub nothing.
#[test]
fn an_option_trades_backfill_stores_the_days_trades_and_their_hourly_chain() {
    use vike_data::source::OPTION_TRADES_INTERVAL;
    const DAY0: i64 = 1_717_372_800_000;
    let trades = vec![option_trade(DAY0 + 10, "t1"), option_trade(DAY0 + HOUR_MS + 5, "t2")];
    let rest = RestLaneScript::new().with_option_trades("BTC", Answer::rows(trades));
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, gateway.link());
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_BACKFILL_OPTION_TRADES)
    );

    let window = (DAY0, DAY0 + 6 * HOUR_MS);
    let done = client
        .backfill("deribit", "BTC", OPTION_TRADES_INTERVAL, window.0, window.1)
        .expect("served");
    assert_eq!(done.rows_written, 2);
    assert_eq!(
        (done.first_timestamp, done.last_timestamp),
        (Some(DAY0 + 10), Some(DAY0 + HOUR_MS + 5))
    );
    assert_eq!(
        rest.fetches(),
        vec![FetchCall {
            what: FetchWhat::OptionTrades {
                currency: "BTC".into(),
                start_ms: DAY0,
                end_ms: DAY0 + 24 * HOUR_MS - 1,
            },
            credentialed: false,
        }],
        "one fetch for the whole day, on the keyless verb"
    );
    let chain = store.scan_chain("deribit", "BTC", TimestampRange::all()).unwrap();
    assert_eq!(
        chain.iter().map(|r| r.ts).collect::<Vec<_>>(),
        [DAY0 + HOUR_MS - 1, DAY0 + 2 * HOUR_MS - 1]
    );
    assert_eq!(chain[0].mark.map(f64::to_bits), Some((0.000_784_82_f64 * 67_735.8).to_bits()));

    let again = client
        .backfill("deribit", "BTC", OPTION_TRADES_INTERVAL, window.0, window.1)
        .expect("served");
    assert_eq!(again.rows_written, 0, "the day's key is spent");
    assert_eq!(rest.fetches().len(), 1, "a stored day is not asked again");
}

/// A venue with no option-trades source is refused naming the set, and a BAR request for deribit still
/// reaches its kline lane, never the option-trades one.
#[test]
fn the_option_trades_label_reaches_only_its_lane() {
    use vike_data::source::OPTION_TRADES_INTERVAL;
    let rest = RestLaneScript::new().with_klines("deribit", Answer::rows(fetched_bars()));
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, gateway.link());
    let err = client.backfill("binance", "BTC", OPTION_TRADES_INTERVAL, 0, HOUR_MS).unwrap_err();
    assert!(err.contains("no option-trades collector") && err.contains("[deribit]"), "{err}");
    client.backfill("deribit", "BTC-PERPETUAL", INTERVAL, 0, 4 * HOUR_MS).expect("the kline lane");
    assert!(matches!(&rest.fetches()[0].what, FetchWhat::Klines { .. }), "{:?}", rest.fetches());
}

/// The MARKET-RULES lane: the reserved `rules` label reaches the gatewayhub's closed-markets fetch, ONE fetch
/// per settled UTC day of market ends (the window rounded outward), and the datahub stores the texts into the
/// archive series each at its OWN received time (a market's close: before the day for one that resolved
/// early, after it for one that resolved late); the reply states no bar edges, a repeat asks the gatewayhub
/// nothing, and another collector is refused.
#[test]
fn a_market_rules_backfill_stores_the_days_texts_once() {
    use vike_data::source::RULES_INTERVAL;
    use vike_data::text_log::{
        POLYMARKET_RULES_ARCHIVE_COLLECTOR, TEXT_KIND_POLYMARKET_RULES, TextRow,
    };
    // 2024-11-05 00:00:00 UTC: settled long ago, whatever the wall clock reads.
    const DAY0: i64 = 1_730_764_800_000;
    let row = |received_ms: i64, market: &str| TextRow {
        source_id: market.into(),
        body: "Resolves Yes if it rains.".into(),
        actual: "Yes".into(),
        ..TextRow::new(TEXT_KIND_POLYMARKET_RULES, "Polymarket", received_ms)
    };
    let rows = vec![row(DAY0 - 3_000, "0xa"), row(DAY0 + 5_000, "0xb")];
    let rest = RestLaneScript::new().with_market_rules("polymarket", Answer::rows(rows));
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, gateway.link());
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_BACKFILL_MARKET_RULES)
    );

    let archive = POLYMARKET_RULES_ARCHIVE_COLLECTOR;
    let window = (DAY0 + HOUR_MS, DAY0 + 6 * HOUR_MS);
    let done =
        client.backfill("polymarket", archive, RULES_INTERVAL, window.0, window.1).expect("served");
    assert_eq!(done.rows_written, 2);
    assert_eq!(
        (done.first_timestamp, done.last_timestamp),
        (None, None),
        "texts have no bar edges"
    );
    assert_eq!(
        rest.fetches(),
        vec![FetchCall {
            what: FetchWhat::ClosedMarketRules {
                venue: "polymarket".into(),
                start_ms: DAY0,
                end_ms: DAY0 + 24 * HOUR_MS - 1,
            },
            credentialed: false,
        }],
        "one fetch for the whole day, on the keyless verb"
    );
    let stored =
        store.scan_texts(TEXT_KIND_POLYMARKET_RULES, archive, TimestampRange::all()).unwrap();
    assert_eq!(
        stored.iter().map(|r| r.received_ms).collect::<Vec<_>>(),
        [DAY0 - 3_000, DAY0 + 5_000]
    );
    assert_eq!(stored[1].actual, "Yes");

    let again =
        client.backfill("polymarket", archive, RULES_INTERVAL, window.0, window.1).expect("served");
    assert_eq!(again.rows_written, 0, "the day's key is spent");
    assert_eq!(rest.fetches().len(), 1, "a stored day is not asked again");

    let other = client.backfill("polymarket", "gamma", RULES_INTERVAL, DAY0, DAY0).unwrap_err();
    assert!(
        other.contains("polymarket:archive:rules")
            && other.contains("polymarket:btc-updown-5m:rules"),
        "{other}"
    );
    let elsewhere = client.backfill("binance", archive, RULES_INTERVAL, DAY0, DAY0).unwrap_err();
    assert!(
        elsewhere.contains("no market-rules collector") && elsewhere.contains("[polymarket]"),
        "{elsewhere}"
    );
    assert_eq!(rest.fetches().len(), 1, "a refusal asks the gatewayhub nothing");
}

/// The SERIES form of the market-rules lane: `polymarket:btc-updown-5m:rules` reaches the gatewayhub's series
/// fetch, ONE per settled UTC day, and stores the texts under the series' own collector; a name that states no
/// window length is refused before the gatewayhub is asked.
#[test]
fn a_market_rules_series_backfill_asks_its_series_and_stores_it_under_its_name() {
    use vike_data::source::RULES_INTERVAL;
    use vike_data::text_log::{TEXT_KIND_POLYMARKET_RULES, TextRow};
    // 2024-11-05 00:00:00 UTC: settled long ago, whatever the wall clock reads.
    const DAY0: i64 = 1_730_764_800_000;
    let rows = vec![TextRow {
        source_id: "0xa".into(),
        actual: "Up".into(),
        ..TextRow::new(TEXT_KIND_POLYMARKET_RULES, "Polymarket", DAY0 + 360_000)
    }];
    let rest = RestLaneScript::new().with_market_rules("polymarket", Answer::rows(rows));
    let gateway = LoopbackGateway::start(DataLaneScript::new(&[]), rest.clone());
    let (_dir, store) = empty_store();
    let mut client = serve_real_table(&store, gateway.link());

    let series = "btc-updown-5m";
    let done = client
        .backfill("polymarket", series, RULES_INTERVAL, DAY0 + HOUR_MS, DAY0 + 2 * HOUR_MS)
        .expect("served");
    assert_eq!(done.rows_written, 1);
    assert_eq!(
        rest.fetches(),
        vec![FetchCall {
            what: FetchWhat::SeriesMarketRules {
                venue: "polymarket".into(),
                series: series.into(),
                start_ms: DAY0,
                end_ms: DAY0 + 24 * HOUR_MS - 1,
            },
            credentialed: false,
        }],
        "one series fetch for the whole day"
    );
    let stored =
        store.scan_texts(TEXT_KIND_POLYMARKET_RULES, series, TimestampRange::all()).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].actual, "Up");

    for junk in ["gamma", "btc-updown-1h", "btc-updown"] {
        let refused = client.backfill("polymarket", junk, RULES_INTERVAL, DAY0, DAY0).unwrap_err();
        assert!(refused.contains("polymarket:btc-updown-5m:rules"), "{junk}: {refused}");
    }
    assert_eq!(rest.fetches().len(), 1, "a refused name asks the gatewayhub nothing");
}
