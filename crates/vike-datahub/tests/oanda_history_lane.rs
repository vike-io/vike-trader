//! **The CREDENTIALED history lane over the wire** — OANDA's one `BackfillLane::CredentialedKlines`
//! row (docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md),
//! driven through a real `DataFusionHist` served on an ephemeral loopback port.
//!
//! What each section proves, in the record's own terms:
//!
//! 1. **An absent key is a request-time refusal that names the fix; an unreadable store says so
//!    instead** — over the REAL table, and nothing is written (verdict 6).
//! 2. **A present key lands rows through the DAY-CHUNKED ingest**, ONE read of the key for the whole
//!    request, and a repeat reads nothing and fetches nothing — and the row hands that ingest the
//!    request's stop probe, so a request whose client has gone stops at the next day boundary.
//! 3. **The row's own refusals come first**: a second spelling of one instrument, and a sub-minute
//!    window that starts before OANDA's dense series — both before the key is asked for.
//! 4. **No Observe verb reaches the lane** — the chart seed refuses oanda on the real table, and on a
//!    KEYED server an Observe connection sweeps every Observe verb naming oanda (and the Control verb
//!    it is refused) without the key being asked for once, while a Control connection reaches it
//!    (verdict 3; 0062's decision 3).
//!
//! ⚠ **No test here may hand the REAL table a provider that answers a token.** The real
//! `vike_oanda::OandaKlines` would then reach OANDA's practice host from CI. [`refusing`] is the only
//! provider these tests give the real table, and section 2 — the one that needs rows to land — builds
//! its row through `vike_datahub::backfill::credentialed_klines_row`, the body production runs, over a
//! fake source.
//!
//! Behind `backfill-serve` for the reason `backfill_roundtrip.rs` is: the real table, and the lane,
//! exist only in that build.
#![cfg(feature = "backfill-serve")]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use tempfile::TempDir;
use vike_data::removal::SeriesSelector;
use vike_data::source::{KlineSource, SourceError};
use vike_data::{DataFusionHist, HistStore, SeriesId, TsRange};
use vike_datahub::backfill::{
    BackfillLane, BackfillTable, credentialed_klines_row, real_backfill_table,
};
use vike_datahub::catalog::{CatalogLane, CatalogTable};
use vike_datahub::seed::SeedLane;
use vike_datahub::{serve_authed, serve_with_backfill};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{
    PROTO_VERSION, Request, Response, VerbScope, read_frame, required_scope, write_frame,
};
use vike_model::Bar;
use vike_node_proto::auth::{self, DATAHUB_DOMAIN, NodeKeys, Scope};
use vike_oanda::{HistoryTokenError, HistoryTokenProvider, oanda_history_token_names};

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;
/// Tuesday 2024-01-02T00:00:00Z — a settled weekday inside OANDA's dense series.
const DAY0: i64 = 1_704_153_600_000;
/// 2005-01-03T00:00:00Z — the first day the lane takes a sub-minute window from.
const DENSE_FROM: i64 = 1_104_710_400_000;
/// A token no real account holds, for the fake-source rows only.
const PLANTED: &str = "fake-oanda-practice-token-3k8w";

const OBSERVE_KEY: &[u8] = b"datahub-observe-key";
const CONTROL_KEY: &[u8] = b"datahub-control-key";

fn empty_store() -> (TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(DataFusionHist::open(dir.path()).expect("a store"));
    (dir, store)
}

fn practice_key() -> String {
    oanda_history_token_names().into_iter().next().expect("the lane declares one name")
}

/// The ONLY provider the REAL table is given here: it COUNTS its reads and answers `why` every time,
/// so the real source refuses before it could send anything anywhere.
fn refusing(why: HistoryTokenError) -> (HistoryTokenProvider, Arc<AtomicUsize>) {
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reads);
    let provider: HistoryTokenProvider = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Err(why)
    });
    (provider, reads)
}

fn read_count(reads: &AtomicUsize) -> usize {
    reads.load(Ordering::SeqCst)
}

fn spawn_keyless(store: Arc<DataFusionHist>, table: BackfillTable) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve_with_backfill(listener, store, Some(table));
    });
    addr
}

fn stored_bars(store: &DataFusionHist, symbol: &str, interval: &str) -> Vec<Bar> {
    store.load_bars("oanda", symbol, interval, TsRange::all()).expect("a readable store")
}

// -------------------------------------------------------------------------------------------------
// 1. An absent key, an unreadable store
// -------------------------------------------------------------------------------------------------

/// **Absent: the teaching refusal. Unreadable: its own text.** Two answers that must never look the
/// same — an operator told to store a key they already stored is chasing the wrong fault. Each is
/// asked for ONCE (the first day's fetch, where the request ends), and nothing is written.
#[test]
fn an_absent_key_is_the_teaching_refusal_and_an_unreadable_store_is_its_own() {
    let key = practice_key();
    let cases = [
        (
            HistoryTokenError::NotConfigured,
            format!("vike-cli secrets set {key}"),
            "could not be read",
        ),
        (HistoryTokenError::StoreUnreadable, "could not be read".to_string(), "secrets set"),
    ];
    for (why, must, must_not) in cases {
        let (_dir, store) = empty_store();
        let (token, reads) = refusing(why);
        let addr =
            spawn_keyless(Arc::clone(&store), real_backfill_table(Arc::clone(&store), token));
        let mut client = DatahubClient::connect(addr).expect("connect");

        let err = client
            .backfill("oanda", "EUR_USD", "5s", DAY0 + HOUR, DAY0 + DAY + HOUR)
            .expect_err("no token, no history");
        assert!(err.contains(&must), "{why:?}: {err}");
        assert!(!err.contains(must_not), "{why:?} reads like the other refusal: {err}");
        assert_eq!(read_count(&reads), 1, "{why:?}: asked once, at the first day's fetch");
        assert!(stored_bars(&store, "EUR_USD", "5s").is_empty(), "{why:?}: nothing written");
    }
}

/// The absent-key refusal says the three things an operator needs: which key, that it is the
/// PRACTICE account's, and that storing it needs no restart.
#[test]
fn the_absent_key_refusal_teaches_the_whole_fix() {
    let (_dir, store) = empty_store();
    let (token, _) = refusing(HistoryTokenError::NotConfigured);
    let addr = spawn_keyless(Arc::clone(&store), real_backfill_table(Arc::clone(&store), token));
    let mut client = DatahubClient::connect(addr).expect("connect");
    let err = client.backfill("oanda", "EUR_USD", "1m", DAY0, DAY0 + HOUR).expect_err("refused");
    assert!(err.contains(&format!("`vike-cli secrets set {}`", practice_key())), "{err}");
    assert!(err.contains("practice account"), "{err}");
    assert!(err.contains("no restart"), "{err}");
    assert!(err.contains("data server's own box"), "{err}");
}

// -------------------------------------------------------------------------------------------------
// 2. A present key — through the row production runs, over a fake source
// -------------------------------------------------------------------------------------------------

/// A source that asks its provider on every fetch — the way `vike_oanda::OandaKlines` does — and
/// answers three bars at the start of whatever window it is handed, recording the window.
struct FakeKlines {
    token: HistoryTokenProvider,
    asked: Arc<Mutex<Vec<(i64, i64)>>>,
}

impl KlineSource for FakeKlines {
    fn venue(&self) -> &str {
        "oanda"
    }

    fn fetch(
        &self,
        _symbol: &str,
        _interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        let token = (self.token)().map_err(|e| SourceError::Refused(e.to_string()))?;
        assert_eq!(token, PLANTED, "the source is handed the provider's token");
        self.asked.lock().expect("asked lock").push((start_ms, end_ms));
        Ok((0..3).map(|i| bar(start_ms + i * 5_000)).collect())
    }
}

fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.08125,
        high: 1.0815,
        low: 1.081,
        close: 1.08137,
        volume: 12.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// **A present key lands rows through the DAY-CHUNKED ingest.** A ragged two-day window is asked
/// of the source as two whole UTC days on the grid — the chunked path, not the per-window one — each
/// stored under its own day key; the key is read ONCE for the request and handed to both days. The
/// repeat is the point of the grid: every day's key is spent, so nothing is fetched and the key is
/// not read at all.
#[test]
fn a_present_key_lands_rows_through_the_day_chunked_ingest() {
    let (_dir, store) = empty_store();
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reads);
    let token: HistoryTokenProvider = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(PLANTED.to_string())
    });
    let asked = Arc::new(Mutex::new(Vec::new()));
    let asked_by_source = Arc::clone(&asked);
    let (venue, collect) = credentialed_klines_row(
        Arc::clone(&store),
        token,
        move |token| FakeKlines { token, asked: Arc::clone(&asked_by_source) },
        |_, _, _, _| None,
    );
    assert_eq!(venue, "oanda", "the table key is the source's own venue");
    let table =
        BackfillTable::new(Vec::new()).with(&venue, BackfillLane::CredentialedKlines, collect);
    assert!(table.get_for_seed("oanda").is_none(), "the chart seed never reaches it");
    let addr = spawn_keyless(Arc::clone(&store), table);
    let mut client = DatahubClient::connect(addr).expect("connect");

    let done = client
        .backfill("oanda", "EUR_USD", "5s", DAY0 + HOUR, DAY0 + DAY + HOUR)
        .expect("a present key serves the request");
    assert_eq!(done.rows_written, 6, "three bars from each of two day chunks");
    let days = [(DAY0, DAY0 + DAY - 1), (DAY0 + DAY, DAY0 + 2 * DAY - 1)];
    assert_eq!(*asked.lock().expect("asked lock"), days, "whole UTC days, never the ragged window");
    assert_eq!(read_count(&reads), 1, "ONE read for the request, however many days it fetched");
    let series = SeriesId::per_symbol("bar", "oanda", "EUR_USD", Some("5s".to_string()));
    for (c0, c1) in days {
        assert!(
            store.series_has_commit(&series, &format!("oanda:EUR_USD:5s:{c0}-{c1}")).expect("read"),
            "day [{c0}, {c1}] is stored under the day grid's own key"
        );
    }
    assert_eq!(stored_bars(&store, "EUR_USD", "5s").len(), 6);

    let again = client
        .backfill("oanda", "EUR_USD", "5s", DAY0 + HOUR, DAY0 + DAY + HOUR)
        .expect("a repeat is served");
    assert_eq!(again.rows_written, 0, "every day is already stored");
    assert_eq!(asked.lock().expect("asked lock").len(), 2, "…so nothing was fetched");
    assert_eq!(read_count(&reads), 1, "…and the key was not read: a stored day needs no token");
}

/// **The row hands the REQUEST'S stop probe to its chunked ingest.** This lane is the one the
/// incident behind docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md ran
/// on — a client that left, and a datahub that fetched on for years of days — so the row production
/// runs is called directly, with a probe that fires after the first day: that day is fetched and
/// stored, the second never is, and the answer is the ingest's `stopped:` text, never a row count. A
/// row that handed its ingest a probe that never fires would fetch both days and answer `Ok(6)`.
#[test]
fn the_credentialed_row_hands_the_requests_stop_probe_to_its_chunked_ingest() {
    let (_dir, store) = empty_store();
    let token: HistoryTokenProvider = Arc::new(|| Ok(PLANTED.to_string()));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let asked_by_source = Arc::clone(&asked);
    let (_, collect) = credentialed_klines_row(
        Arc::clone(&store),
        token,
        move |token| FakeKlines { token, asked: Arc::clone(&asked_by_source) },
        |_, _, _, _| None,
    );
    let asks = std::cell::Cell::new(0);
    let stop_after_one_day = || {
        asks.set(asks.get() + 1);
        asks.get() > 1
    };

    let outcome = collect("EUR_USD", "5s", DAY0 + HOUR, DAY0 + DAY + HOUR, &stop_after_one_day);

    let err = outcome.expect_err("a stopped request is never a row count");
    assert!(err.starts_with("stopped: oanda EUR_USD 5s "), "{err}");
    assert!(err.contains("stopped before day chunk 2 of 2"), "{err}");
    assert_eq!(
        *asked.lock().expect("asked lock"),
        [(DAY0, DAY0 + DAY - 1)],
        "the first day is fetched, the second never is"
    );
    assert_eq!(stored_bars(&store, "EUR_USD", "5s").len(), 3, "the first day stays stored");
}

// -------------------------------------------------------------------------------------------------
// 3. The row's own refusals — before the key is asked for
// -------------------------------------------------------------------------------------------------

/// **A second spelling of one instrument is refused, naming OANDA's own.** The store keys a series
/// on the symbol the request carried, so `eurusd` beside `EUR_USD` would be two series of one
/// instrument. Refused before the key is read, and nothing is written under either spelling.
#[test]
fn a_second_spelling_of_one_instrument_is_refused_naming_the_canonical_one() {
    let (_dir, store) = empty_store();
    let (token, reads) = refusing(HistoryTokenError::NotConfigured);
    let addr = spawn_keyless(Arc::clone(&store), real_backfill_table(Arc::clone(&store), token));
    let mut client = DatahubClient::connect(addr).expect("connect");

    for (symbol, canonical) in [
        ("eurusd", Some("EUR_USD")),
        ("EURUSD", Some("EUR_USD")),
        ("eur_usd", Some("EUR_USD")),
        ("EUR/USD", None),
    ] {
        let err = client.backfill("oanda", symbol, "5s", DAY0, DAY0 + HOUR).expect_err(symbol);
        assert!(err.contains("SECOND series"), "{symbol}: {err}");
        match canonical {
            Some(c) => assert!(err.contains(&format!("ask for `{c}`")), "{symbol}: {err}"),
            None => assert!(err.contains("OANDA's own name"), "{symbol}: {err}"),
        }
    }
    assert_eq!(read_count(&reads), 0, "every refusal came before the key was asked for");
    assert!(store.list_series().expect("list").is_empty(), "nothing was written anywhere");
}

/// **A sub-minute window that starts before the dense series is refused, naming the date** — and
/// only a SUB-minute one, and only when it STARTS before it: a `1m` window over the same days, and a
/// `5s` window starting on the date itself, both pass the row and reach the source (which then
/// refuses for want of a key, the proof they got that far).
#[test]
fn a_sub_minute_window_before_the_dense_series_is_refused_by_date() {
    let (_dir, store) = empty_store();
    let (token, reads) = refusing(HistoryTokenError::NotConfigured);
    let addr = spawn_keyless(Arc::clone(&store), real_backfill_table(Arc::clone(&store), token));
    let mut client = DatahubClient::connect(addr).expect("connect");

    for interval in ["5s", "10s", "15s", "30s"] {
        let err = client
            .backfill("oanda", "EUR_USD", interval, DENSE_FROM - DAY, DENSE_FROM + DAY)
            .expect_err(interval);
        assert!(err.contains("2005-01-03"), "{interval}: names the date: {err}");
        assert!(err.contains("one candle a DAY"), "{interval}: names the reason: {err}");
        assert!(err.contains("rather than clamped"), "{interval}: a refusal, not a clamp: {err}");
    }
    assert_eq!(read_count(&reads), 0, "refused before the key was asked for");

    let minute = client
        .backfill("oanda", "EUR_USD", "1m", DENSE_FROM - DAY, DENSE_FROM + DAY)
        .expect_err("no key");
    assert!(minute.contains("secrets set"), "a 1m window passes the date rule: {minute}");
    let on_the_date = client
        .backfill("oanda", "EUR_USD", "5s", DENSE_FROM, DENSE_FROM + HOUR)
        .expect_err("no key");
    assert!(on_the_date.contains("secrets set"), "the date itself is allowed: {on_the_date}");
    assert_eq!(read_count(&reads), 2, "both reached the source");
    assert!(store.list_series().expect("list").is_empty(), "nothing was written");
}

// -------------------------------------------------------------------------------------------------
// 4. No Observe verb reaches the lane
// -------------------------------------------------------------------------------------------------

/// **The chart seed refuses oanda on the REAL table.** An armed seed lane, a raw seed request the
/// way a client that skips the handshake's guards sends it, and the key never asked for.
#[test]
fn the_chart_seed_never_reaches_the_credentialed_row() {
    let (_dir, store) = empty_store();
    let (token, reads) = refusing(HistoryTokenError::NotConfigured);
    let table = real_backfill_table(Arc::clone(&store), token);
    assert!(table.get_for_seed("oanda").is_none(), "the table's own seed lookup");
    assert!(!table.seed_supported().contains(&"oanda"), "{:?}", table.seed_supported());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let served = Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>;
    thread::spawn(move || {
        let _ = serve_authed(
            listener,
            served,
            Some(table),
            None,
            None,
            Some(Arc::new(SeedLane::new())),
            None,
        );
    });

    let mut stream = TcpStream::connect(addr).expect("connect");
    let seed = Request::SeedSeries {
        venue: "oanda".into(),
        symbol: "EUR_USD".into(),
        interval: "5m".into(),
        class: None,
    };
    match exchange(&mut stream, &seed) {
        Response::Error(err) => {
            assert!(err.contains("has no collector"), "{err}");
            let supported = err.split("Supported:").nth(1).unwrap_or_default();
            assert!(!supported.contains("oanda"), "the seed's set must not name oanda: {err}");
        }
        other => panic!("a chart seed for oanda was SERVED: {other:?}"),
    }
    assert_eq!(read_count(&reads), 0, "the seed never asked for the key");
}

fn keys() -> NodeKeys {
    NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec())
}

fn exchange(stream: &mut TcpStream, request: &Request) -> Response {
    write_frame(stream, request).expect("write request");
    read_frame::<_, Response>(stream).expect("read response")
}

/// Open a socket and complete the handshake as `scope` — `auth_roundtrip.rs`'s helper.
fn authed_stream(addr: SocketAddr, scope: Scope) -> TcpStream {
    let keys = keys();
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut s).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce.expect("a keyed server's Welcome carries a nonce"),
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(DATAHUB_DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope);
    write_frame(&mut s, &Request::Auth { scope, mac }).expect("auth");
    match read_frame::<_, Response>(&mut s).expect("auth answer") {
        Response::AuthOk { scope: granted } => assert_eq!(granted, scope),
        other => panic!("expected AuthOk, got {other:?}"),
    }
    s
}

/// Every OBSERVE-scope verb this daemon serves, each naming oanda where it names a venue — the
/// reads, the store metadata, the chart seed and the venue catalog.
fn observe_requests_naming_oanda() -> Vec<Request> {
    let series = SeriesId::per_symbol("bar", "oanda", "EUR_USD", Some("5s".to_string()));
    let venue = || "oanda".to_string();
    let symbol = || "EUR_USD".to_string();
    vec![
        Request::Ping,
        Request::LoadBars {
            venue: venue(),
            symbol: symbol(),
            interval: "5s".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanQuotes {
            venue: venue(),
            symbol: symbol(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanTrades {
            venue: venue(),
            symbol: symbol(),
            start: None,
            end: None,
            limit: None,
        },
        Request::PropertiesAsOf { venue: venue(), symbol: symbol(), ts: 0 },
        Request::ScanBookUpdates {
            venue: venue(),
            symbol: symbol(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanDepth {
            venue: venue(),
            symbol: symbol(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanCohort {
            venue: venue(),
            asset: "EUR".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanPerpMetrics {
            venue: venue(),
            symbol: symbol(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanEquity {
            venue: venue(),
            symbol: symbol(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanExecFills { venue: venue(), symbol: symbol() },
        Request::ListSeries,
        Request::Inventory,
        Request::Coverage,
        Request::SeriesFacts { id: series.clone() },
        Request::SeriesGaps { id: series },
        Request::SeedSeries {
            venue: venue(),
            symbol: symbol(),
            interval: "5m".into(),
            class: None,
        },
        Request::VenueCatalog { venue: venue() },
    ]
}

/// **On a KEYED server no Observe verb reaches the lane, and the Control scope does.** An Observe
/// connection sweeps every Observe verb naming oanda — the chart seed and the venue catalog
/// included, both armed — and is refused the `Backfill` by SCOPE; the key is never asked for. Then
/// the guard that makes the zero mean something: a Control connection's `Backfill` reaches the
/// row, and the key is asked for exactly once.
#[test]
fn no_observe_verb_reaches_the_credentialed_lane_and_control_does() {
    let (_dir, store) = empty_store();
    let (token, reads) = refusing(HistoryTokenError::NotConfigured);
    let table = real_backfill_table(Arc::clone(&store), token);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let served = Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>;
    thread::spawn(move || {
        let _ = serve_authed(
            listener,
            served,
            Some(table),
            Some(keys()),
            None,
            Some(Arc::new(SeedLane::new())),
            Some(Arc::new(CatalogLane::new(CatalogTable::new(Vec::new())))),
        );
    });

    let mut observe = authed_stream(addr, Scope::Read);
    for request in observe_requests_naming_oanda() {
        assert_eq!(required_scope(&request), VerbScope::Read, "not an Observe verb: {request:?}");
        let _ = exchange(&mut observe, &request);
        assert_eq!(read_count(&reads), 0, "{request:?} reached the credential");
    }
    let backfill = Request::Backfill {
        venue: "oanda".into(),
        symbol: "EUR_USD".into(),
        interval: "5s".into(),
        start: DAY0,
        end: DAY0 + HOUR,
    };
    match exchange(&mut observe, &backfill) {
        Response::Error(msg) => assert!(msg.contains("Control scope"), "refused by SCOPE: {msg}"),
        other => panic!("an Observe connection was served the Backfill: {other:?}"),
    }
    let delete = Request::DeleteSeries {
        selector: SeriesSelector::new("bar", "oanda"),
        produced_by: None,
        dry_run: true,
    };
    let _ = exchange(&mut observe, &delete);
    assert_eq!(read_count(&reads), 0, "no Observe request asked for the key");

    let mut control = authed_stream(addr, Scope::Write);
    match exchange(&mut control, &backfill) {
        Response::Error(msg) => {
            assert!(msg.contains("secrets set"), "the Control Backfill reached the row: {msg}")
        }
        other => panic!("expected the absent-key refusal, got {other:?}"),
    }
    assert_eq!(read_count(&reads), 1, "the guard: Control reaches the lane, once");
}

/// The table prints VENUES and nothing that leads to a credential — and printing it asks for none.
#[test]
fn the_table_prints_venues_and_never_asks_for_the_token() {
    let (_dir, store) = empty_store();
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reads);
    let token: HistoryTokenProvider = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(PLANTED.to_string())
    });
    // Built and printed, NEVER called: this provider answers a token, so calling the real row
    // would reach OANDA — which is also why the check below is on `reads` rather than on a fetch.
    let table = real_backfill_table(store, token);
    let printed = format!("{table:?}");
    assert!(printed.contains("oanda"), "{printed}");
    assert!(!printed.contains(PLANTED), "{printed}");
    assert_eq!(read_count(&reads), 0, "building and printing the table read nothing");
    for why in [HistoryTokenError::NotConfigured, HistoryTokenError::StoreUnreadable] {
        assert!(!format!("{why} {why:?}").contains(PLANTED), "the error carries no payload");
    }
}
