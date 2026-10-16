//! **The CREDENTIALED history lane over the wire** — OANDA's one `HistoryLane::CredentialedKlines`
//! row (docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md),
//! driven through a real `DataFusionHist` served on an ephemeral loopback port, with a SCRIPTED
//! gatewayhub (`vike_gatewayhub::testkit`) standing where the venue and the credential are.
//!
//! Since decision 0164 this process reads no credential: the practice token is the gatewayhub's, read
//! once per CONNECTION and dropped with it (the daemon's own suite, `crates/vike-gatewayhub/tests/
//! oanda_credential.rs`, holds the read, the three states and the words of each refusal). What is
//! left to prove HERE is the datahub's half of the lane, in the record's own terms:
//!
//! 1. **An absent key and an unreadable store are the DAEMON's words, passed through unchanged** —
//!    over the REAL table, asked once, and nothing is written (verdict 6).
//! 2. **A present key lands rows through the DAY-CHUNKED ingest**, the whole request on ONE
//!    connection (the daemon's read-once memo is per connection, so one connection is one read), a
//!    repeat fetches nothing, and the NEXT request dials afresh — which is what makes a key stored,
//!    rotated or removed between requests the thing the next one sees. The row hands that ingest the
//!    request's stop probe, so a request whose client has gone stops at the next day boundary.
//! 3. **The row's own refusals come first**: a second spelling of one instrument, and a sub-minute
//!    window that starts before OANDA's dense series — both before the gatewayhub is asked anything.
//! 4. **No Observe verb reaches the lane** — the chart seed refuses oanda on the real table, and on a
//!    KEYED server an Observe connection sweeps every Observe verb naming oanda (and the Control verb
//!    it is refused) without the credentialed verb being sent once, while a Control connection reaches
//!    it (verdict 3; 0062's decision 3).
//!
//! ⚠ **No test here reaches OANDA, and none can**: the real table is handed a link to a scripted
//! gatewayhub whose credentialed lane answers what a test wrote, so "what the daemon was asked" is
//! read back from the script (`ScriptedRest::fetches`).
//!
//! Behind `backfill-serve` for the reason `backfill_roundtrip.rs` is: the real table, and the lane,
//! exist only in that build.
#![cfg(feature = "backfill-serve")]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use tempfile::TempDir;
use vike_data::StoreKindId;
use vike_data::source::{KlineSource, SourceError};
use vike_data::store::removal::SeriesSelector;
use vike_data::{HistStore, SeriesId, TsRange};
use vike_datahub::backfill::{BackfillTable, credentialed_klines_row, real_backfill_table};
use vike_datahub::catalog::{CATALOG_TTL, CatalogLane, CatalogTable};
use vike_datahub::seed::SeedLane;
use vike_datahub::{serve_authed, serve_with_backfill};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::{Request, Response, VerbScope, required_scope};
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::{Answer, FetchCall, ScriptedRest};
use vike_gatewayhub_client::proto::{FetchFailure, FetchRows, FetchWhat};
use vike_hub_proto::auth::Scope;
use vike_marketdata::Bar;
use vike_store::DataFusionHist;

mod common;
use common::{authed_stream, exchange, keys};

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;
/// Tuesday 2024-01-02T00:00:00Z — a settled weekday inside OANDA's dense series.
const DAY0: i64 = 1_704_153_600_000;
/// 2005-01-03T00:00:00Z — the first day the lane takes a sub-minute window from.
const DENSE_FROM: i64 = 1_104_710_400_000;

/// What the daemon says when no practice token is stored: its OWN words (the bridge's
/// `HistoryTokenError`, carried by the daemon as a `SourceError::Refused`), here a stand-in the
/// datahub must pass through unchanged. It names the fix, the way the real one does.
const ABSENT_WORDS: &str = "scripted gatewayhub: no practice token is stored for the oanda history \
                            lane; `vike-cli secrets set` arms it, with no restart";
/// …and when the store exists and will not read — which must never read like the one above.
const UNREADABLE_WORDS: &str = "scripted gatewayhub: the credential store could not be read, so \
                                whether the oanda history lane's token is stored cannot be told";

fn empty_store() -> (TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(DataFusionHist::open(dir.path()).expect("a store"));
    (dir, store)
}

/// A scripted gatewayhub whose CREDENTIALED lane refuses with `words` (and which serves keyless
/// klines beside it, as the real daemon always does: the advertisement is derived from the scripts).
/// Declared before every client of it (drop order: the clients go first).
fn refusing(words: &str) -> (LoopbackGateway, ScriptedRest) {
    let rest = ScriptedRest::new()
        .with_klines("binance", Answer::rows(Vec::new()))
        .with_credentialed_klines("oanda", Answer::refused(words));
    (LoopbackGateway::start(ScriptedData::new(&[]), rest.clone()), rest)
}

/// How many fetches reached the CREDENTIALED verb.
fn credentialed_fetches(rest: &ScriptedRest) -> usize {
    rest.fetches().iter().filter(|call| call.credentialed).count()
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
/// same — an operator told to store a key they already stored is chasing the wrong fault. Each
/// crosses the hop with the daemon's words intact, is asked for ONCE (the first day's fetch, where
/// the request ends) and nothing is written.
#[test]
fn an_absent_key_is_the_teaching_refusal_and_an_unreadable_store_is_its_own() {
    for (words, must_not) in
        [(ABSENT_WORDS, "could not be read"), (UNREADABLE_WORDS, "secrets set")]
    {
        let (gateway, rest) = refusing(words);
        let (_dir, store) = empty_store();
        let addr = spawn_keyless(
            Arc::clone(&store),
            real_backfill_table(Arc::clone(&store), gateway.link()),
        );
        let mut client = DatahubClient::connect(addr).expect("connect");

        let err = client
            .backfill("oanda", "EUR_USD", "5s", DAY0 + HOUR, DAY0 + DAY + HOUR)
            .expect_err("no token, no history");
        assert!(err.contains(words), "{words}: {err}");
        assert!(!err.contains(must_not), "{words} reads like the other refusal: {err}");
        assert_eq!(
            rest.fetches().len(),
            1,
            "{words}: asked once, at the first day's fetch: {:?}",
            rest.fetches()
        );
        assert_eq!(credentialed_fetches(&rest), 1, "…and on the credentialed verb alone");
        assert!(stored_bars(&store, "EUR_USD", "5s").is_empty(), "{words}: nothing written");
    }
}

/// The text the bridge gives for an absent practice token, copied: `vike_oanda::HistoryTokenError::
/// NotConfigured`'s `Display` (this crate links no bridge since decision 0164). The daemon's own suite
/// (`crates/vike-gatewayhub/tests/oanda_credential.rs`) pins its refusal to that `Display` itself; what
/// THIS crate owns, and pins below, is that whatever the daemon refuses with reaches the caller whole -
/// this text included, with the fix words an operator acts on.
fn bridge_absent_words() -> String {
    let field = vike_venues::credential_keys::API_KEY;
    format!(
        "oanda history needs the practice account's API token, and this data server holds none: \
         on the data server's own box, give an oanda account at tier `demo` its {field} \
         (`vike-cli secrets accounts` names the accounts, `vike-cli secrets set --account <ID> \
         {field}` stores the key). It is read when a Backfill arrives, so storing it needs no \
         restart. Only the practice account's key is read for history; a live account's key is \
         never used"
    )
}

/// **The absent-key refusal still teaches the whole fix on the reply a datahub client receives.** The
/// bridge's words say which key, that it is the PRACTICE account's, that storing it needs no restart and
/// where to store it; the daemon carries them as a `Refused`, and the datahub hands them to the caller
/// verbatim (the stand-in above proves pass-through of ANY text; this pins the words an operator reads).
#[test]
fn the_absent_key_refusal_teaches_the_whole_fix() {
    let words = bridge_absent_words();
    let (gateway, _rest) = refusing(&words);
    let (_dir, store) = empty_store();
    let addr =
        spawn_keyless(Arc::clone(&store), real_backfill_table(Arc::clone(&store), gateway.link()));
    let mut client = DatahubClient::connect(addr).expect("connect");
    let err = client.backfill("oanda", "EUR_USD", "1m", DAY0, DAY0 + HOUR).expect_err("refused");
    assert!(err.contains(&words), "the daemon's words arrive whole: {err}");
    for fix in ["secrets set", "practice account", "no restart", "data server's own box"] {
        assert!(err.contains(fix), "the fix words lost `{fix}`: {err}");
    }
}

/// **A gatewayhub that cannot be reached is a refusal naming the gatewayhub — and it is NOT the
/// absent-key text.** A dead link says nothing about whether a key is stored, so it must not send an
/// operator to store one; and it writes nothing and spends no commit key (the same window is
/// fetchable once the daemon is back).
#[test]
fn an_unreachable_gatewayhub_is_not_the_absent_key_refusal() {
    // Real keys, and a socket nothing listens on: the OS refuses the DIAL.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
    let dead = listener.local_addr().expect("its address");
    drop(listener);
    let link = vike_gatewayhub_client::client::GatewayLink::new(
        dead.to_string(),
        vike_gatewayhub::testkit::gateway::keys(),
    );
    assert_a_failed_link_is_not_the_absent_key_refusal(link, "cannot connect");
}

/// The same for the production link before the hub keys are provisioned (`no_gateway()`: empty
/// keys): refused locally, nothing dialled, naming the missing GATEWAYHUB hub key - not an OANDA
/// practice token.
#[test]
fn a_link_without_hub_keys_is_not_the_absent_key_refusal_either() {
    assert_a_failed_link_is_not_the_absent_key_refusal(common::no_gateway(), "hub key");
}

fn assert_a_failed_link_is_not_the_absent_key_refusal(
    link: vike_gatewayhub_client::client::GatewayLink,
    want: &str,
) {
    let (_dir, store) = empty_store();
    let addr = spawn_keyless(Arc::clone(&store), real_backfill_table(Arc::clone(&store), link));
    let mut client = DatahubClient::connect(addr).expect("connect");
    let err = client.backfill("oanda", "EUR_USD", "1m", DAY0, DAY0 + HOUR).expect_err("refused");
    assert!(err.contains("gatewayhub"), "names the daemon that could not be asked: {err}");
    assert!(err.contains(want), "says why ({want:?}): {err}");
    assert!(!err.contains("secrets set"), "a dead link is not a missing key: {err}");
    assert!(stored_bars(&store, "EUR_USD", "1m").is_empty());
    assert!(store.list_series().expect("list").is_empty(), "nothing was written");
}

// -------------------------------------------------------------------------------------------------
// 2. A present key — through the real table, over a scripted daemon
// -------------------------------------------------------------------------------------------------

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

/// A connection's identity as the DAEMON sees it: a per-connection slot (`ConnectionScope::slot`)
/// that takes the next number when a connection first asks for it. The OANDA token's read-once memo
/// is the same kind of slot, so "these fetches shared a slot" is "these fetches shared one read".
struct ConnectionId(usize);

static NEXT_CONNECTION: AtomicUsize = AtomicUsize::new(1);

impl Default for ConnectionId {
    fn default() -> Self {
        Self(NEXT_CONNECTION.fetch_add(1, Ordering::SeqCst))
    }
}

/// A scripted daemon whose credentialed lane answers three bars at the start of whatever window it
/// is handed (as a real source would), and records the connection each fetch arrived on.
fn answering() -> (LoopbackGateway, ScriptedRest, Arc<Mutex<Vec<usize>>>) {
    let connections = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&connections);
    let rest = ScriptedRest::new()
        // The scripts that advertise the lanes (the kit derives the advertisement from the scripts,
        // and the real daemon always serves keyless klines beside its credentialed lane, which is
        // what the client's `gw_fetch` check looks for); `on_fetch` answers every fetch itself.
        .with_klines("binance", Answer::rows(Vec::new()))
        .with_credentialed_klines("oanda", Answer::rows(Vec::new()))
        .on_fetch(move |scope, what, _credentialed, _ctx| {
            seen.lock().expect("connections lock").push(scope.slot::<ConnectionId>().0);
            match what {
                FetchWhat::Klines { start_ms, .. } => {
                    Ok(FetchRows::Bars((0..3).map(|i| bar(start_ms + i * 5_000)).collect()))
                }
                other => Err(FetchFailure::Fetch(format!("unscripted fetch: {other:?}"))),
            }
        });
    (LoopbackGateway::start(ScriptedData::new(&[]), rest.clone()), rest, connections)
}

fn window_of(call: &FetchCall) -> (i64, i64) {
    match &call.what {
        FetchWhat::Klines { start_ms, end_ms, .. } => (*start_ms, *end_ms),
        other => panic!("not a kline fetch: {other:?}"),
    }
}

/// **A present key lands rows through the DAY-CHUNKED ingest.** A ragged two-day window is asked of
/// the daemon as two whole UTC days on the grid — the chunked path, not the per-window one — each
/// stored under its own day key; both days are fetched on the CREDENTIALED verb over ONE connection
/// (so the daemon reads its key once for the request). The repeat is the point of the grid: every
/// day's key is spent, so nothing is fetched at all.
#[test]
fn a_present_key_lands_rows_through_the_day_chunked_ingest() {
    let (gateway, rest, connections) = answering();
    let (_dir, store) = empty_store();
    let table = real_backfill_table(Arc::clone(&store), gateway.link());
    assert!(table.get_for_seed("oanda").is_none(), "the chart seed never reaches it");
    let addr = spawn_keyless(Arc::clone(&store), table);
    let mut client = DatahubClient::connect(addr).expect("connect");

    let done = client
        .backfill("oanda", "EUR_USD", "5s", DAY0 + HOUR, DAY0 + DAY + HOUR)
        .expect("a present key serves the request");
    assert_eq!(done.rows_written, 6, "three bars from each of two day chunks");
    let days = [(DAY0, DAY0 + DAY - 1), (DAY0 + DAY, DAY0 + 2 * DAY - 1)];
    let fetches = rest.fetches();
    assert_eq!(
        fetches.iter().map(window_of).collect::<Vec<_>>(),
        days,
        "whole UTC days, never the ragged window"
    );
    assert!(fetches.iter().all(|call| call.credentialed), "the credentialed verb alone");
    assert!(
        fetches.iter().all(|call| matches!(&call.what,
            FetchWhat::Klines { venue, symbol, interval, .. }
                if venue == "oanda" && symbol == "EUR_USD" && interval == "5s")),
        "{fetches:?}"
    );
    let ids = connections.lock().expect("connections lock").clone();
    assert_eq!(ids.len(), 2, "two day fetches");
    assert_eq!(
        ids[0], ids[1],
        "ONE connection for the whole request: the daemon reads its key once"
    );
    let series = SeriesId::per_symbol(StoreKindId::Bar, "oanda", "EUR_USD", Some("5s".to_string()));
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
    assert_eq!(rest.fetches().len(), 2, "…so nothing was fetched: a stored day needs no token");
}

/// **A second request reads again — it does not inherit the first's connection.** The daemon keeps
/// the practice token in a per-CONNECTION memo and drops it when the connection closes, so a request
/// that dialled the daemon afresh is a request the daemon re-reads its store for: a key stored,
/// rotated or removed between two requests is what the second one sees, with no restart on either
/// side (0097's "removing the token disarms the lane"). This process cannot see the store, so the
/// proof it owes is the other half: each request builds its own source, on its own connection, and
/// the first request's connection is closed when it returns.
#[test]
fn a_second_request_rereads_the_store() {
    let (gateway, rest, connections) = answering();
    let (_dir, store) = empty_store();
    let addr =
        spawn_keyless(Arc::clone(&store), real_backfill_table(Arc::clone(&store), gateway.link()));
    let mut client = DatahubClient::connect(addr).expect("connect");

    client.backfill("oanda", "EUR_USD", "5s", DAY0, DAY0 + HOUR).expect("the first request");
    client
        .backfill("oanda", "EUR_USD", "5s", DAY0 + 2 * DAY, DAY0 + 2 * DAY + HOUR)
        .expect("second");
    client
        .backfill("oanda", "EUR_USD", "5s", DAY0 + 4 * DAY, DAY0 + 4 * DAY + HOUR)
        .expect("third");

    let ids = connections.lock().expect("connections lock").clone();
    assert_eq!(ids.len(), 3, "one day each: {:?}", rest.fetches());
    assert!(
        ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2],
        "three requests, three connections, so three reads at the daemon: {ids:?}"
    );
    assert_eq!(credentialed_fetches(&rest), 3);
}

/// A source the stop-probe test fetches from: three bars at the start of whatever window it is
/// handed, recording the window.
struct FakeKlines {
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
        self.asked.lock().expect("asked lock").push((start_ms, end_ms));
        Ok((0..3).map(|i| bar(start_ms + i * 5_000)).collect())
    }
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
    let asked = Arc::new(Mutex::new(Vec::new()));
    let asked_by_source = Arc::clone(&asked);
    let (_, collect) = credentialed_klines_row(
        Arc::clone(&store),
        move || FakeKlines { asked: Arc::clone(&asked_by_source) },
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
// 3. The row's own refusals — before the gatewayhub is asked anything
// -------------------------------------------------------------------------------------------------

/// **A second spelling of one instrument is refused, naming OANDA's own.** The store keys a series
/// on the symbol the request carried, so `eurusd` beside `EUR_USD` would be two series of one
/// instrument. Refused before the daemon is asked for anything, and nothing is written under either
/// spelling.
#[test]
fn a_second_spelling_of_one_instrument_is_refused_naming_the_canonical_one() {
    let (gateway, rest) = refusing(ABSENT_WORDS);
    let (_dir, store) = empty_store();
    let addr =
        spawn_keyless(Arc::clone(&store), real_backfill_table(Arc::clone(&store), gateway.link()));
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
    assert!(rest.calls().is_empty(), "every refusal came before the daemon was asked anything");
    assert!(store.list_series().expect("list").is_empty(), "nothing was written anywhere");
}

/// **A sub-minute window that starts before the dense series is refused, naming the date** — and
/// only a SUB-minute one, and only when it STARTS before it: a `1m` window over the same days, and a
/// `5s` window starting on the date itself, both pass the row and reach the daemon (which then
/// refuses for want of a key, the proof they got that far).
#[test]
fn a_sub_minute_window_before_the_dense_series_is_refused_by_date() {
    let (gateway, rest) = refusing(ABSENT_WORDS);
    let (_dir, store) = empty_store();
    let addr =
        spawn_keyless(Arc::clone(&store), real_backfill_table(Arc::clone(&store), gateway.link()));
    let mut client = DatahubClient::connect(addr).expect("connect");

    for interval in ["5s", "10s", "15s", "30s"] {
        let err = client
            .backfill("oanda", "EUR_USD", interval, DENSE_FROM - DAY, DENSE_FROM + DAY)
            .expect_err(interval);
        assert!(err.contains("2005-01-03"), "{interval}: names the date: {err}");
        assert!(err.contains("one candle a DAY"), "{interval}: names the reason: {err}");
        assert!(err.contains("rather than clamped"), "{interval}: a refusal, not a clamp: {err}");
    }
    assert!(rest.calls().is_empty(), "refused before the daemon was asked anything");

    let minute = client
        .backfill("oanda", "EUR_USD", "1m", DENSE_FROM - DAY, DENSE_FROM + DAY)
        .expect_err("no key");
    assert!(minute.contains("secrets set"), "a 1m window passes the date rule: {minute}");
    let on_the_date = client
        .backfill("oanda", "EUR_USD", "5s", DENSE_FROM, DENSE_FROM + HOUR)
        .expect_err("no key");
    assert!(on_the_date.contains("secrets set"), "the date itself is allowed: {on_the_date}");
    assert_eq!(rest.fetches().len(), 2, "both reached the daemon");
    assert!(store.list_series().expect("list").is_empty(), "nothing was written");
}

// -------------------------------------------------------------------------------------------------
// 4. No Observe verb reaches the lane
// -------------------------------------------------------------------------------------------------

/// **The chart seed refuses oanda on the REAL table.** An armed seed lane, a raw seed request the
/// way a client that skips the handshake's guards sends it, and the daemon never asked.
#[test]
fn the_chart_seed_never_reaches_the_credentialed_row() {
    let (gateway, rest) = refusing(ABSENT_WORDS);
    let (_dir, store) = empty_store();
    let table = real_backfill_table(Arc::clone(&store), gateway.link());
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
    assert!(rest.calls().is_empty(), "the seed never asked the daemon for anything");
}

/// Every OBSERVE-scope verb this daemon serves, each naming oanda where it names a venue — the
/// reads, the store metadata, the chart seed and the venue catalog.
fn observe_requests_naming_oanda() -> Vec<Request> {
    let series = SeriesId::per_symbol(StoreKindId::Bar, "oanda", "EUR_USD", Some("5s".to_string()));
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
        Request::ScanExecFills {
            venue: venue(),
            symbol: symbol(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanExecOrders {
            venue: venue(),
            symbol: symbol(),
            start: None,
            end: None,
            limit: None,
        },
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
/// included, both armed — and is refused the `Backfill` by SCOPE; the daemon is never sent the
/// credentialed verb. Then the guard that makes the zero mean something: a Control connection's
/// `Backfill` reaches the row, and the credentialed verb is sent exactly once.
#[test]
fn no_observe_verb_reaches_the_credentialed_lane_and_control_does() {
    let (gateway, rest) = refusing(ABSENT_WORDS);
    let (_dir, store) = empty_store();
    let table = real_backfill_table(Arc::clone(&store), gateway.link());
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
            Some(Arc::new(CatalogLane::new(CatalogTable::new(Vec::new()), CATALOG_TTL))),
        );
    });

    let mut observe = authed_stream(addr, &keys(), Scope::Read);
    for request in observe_requests_naming_oanda() {
        assert_eq!(required_scope(&request), VerbScope::Read, "not an Observe verb: {request:?}");
        let _ = exchange(&mut observe, &request);
        assert_eq!(credentialed_fetches(&rest), 0, "{request:?} reached the credential");
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
        selector: SeriesSelector::new(StoreKindId::Bar, "oanda"),
        produced_by: None,
        dry_run: true,
    };
    let _ = exchange(&mut observe, &delete);
    assert!(rest.calls().is_empty(), "no Observe request asked the daemon for anything");

    let mut control = authed_stream(addr, &keys(), Scope::Write);
    match exchange(&mut control, &backfill) {
        Response::Error(msg) => {
            assert!(msg.contains("secrets set"), "the Control Backfill reached the row: {msg}")
        }
        other => panic!("expected the absent-key refusal, got {other:?}"),
    }
    assert_eq!(credentialed_fetches(&rest), 1, "the guard: Control reaches the lane, once");
}

/// The table prints VENUES and nothing that leads to a credential — and building and printing it
/// asks the daemon for nothing (a remote source dials lazily, so the venue name the credentialed
/// row reads off an unused source costs no connection).
#[test]
fn the_table_prints_venues_and_never_asks_the_daemon() {
    let (gateway, rest) = refusing(ABSENT_WORDS);
    let (_dir, store) = empty_store();
    let table = real_backfill_table(store, gateway.link());
    let printed = format!("{table:?}");
    assert!(printed.contains("oanda"), "{printed}");
    assert!(!printed.contains(ABSENT_WORDS), "{printed}");
    assert!(rest.calls().is_empty(), "building and printing the table asked for nothing");
}
