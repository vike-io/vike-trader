//! The COMPOSED-PATH gate for the CHART-GAP SEED verb
//! (`docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`): a real `DataFusionHist` over a
//! temp dir, served on an ephemeral loopback port, driven through `DatahubClient::seed_series`.
//!
//! Same seam and same discipline as `backfill_roundtrip.rs`, so read that file's header for why the
//! collector is FAKE (CI is deterministic and network-free, and `BackfillTable` is the injection
//! point the real bin installs `real_backfill_table` into). What this file adds is the five
//! properties the verb's SCOPE classification rests on, each one of which would be a hole in
//! 0057's argument if it did not hold:
//!
//! 1. [`an_unarmed_lane_succeeds_and_writes_nothing`] — reach property 3, the leg that makes a
//!    WRITE verb's `VerbScope::Observe` classification honest rather than convenient;
//! 2. [`a_second_request_for_the_same_series_fetches_nothing`] — "exactly ONE fetch", proved on the
//!    SERVER, independently of any client-side ledger;
//! 3. [`the_window_is_the_servers_and_the_client_names_no_part_of_it`] — the cost rule itself: the
//!    request carries no range, and the collector is handed the server's;
//! 4. [`an_interval_outside_the_validated_set_never_reaches_a_bridge`] — the untrusted-path check
//!    that stands in front of `klines_url`'s unencoded interpolation, asserted by OBSERVING THE
//!    COLLECTOR rather than by reading the refusal text;
//! 5. [`the_venue_budget_refuses_by_name_once_the_burst_is_spent`] — the bound that keeps this lane
//!    a small share of the venue budget the order-signing daemon shares.
//!
//! Behind `backfill-serve` for the same reason its sibling is: the lane serves through the same
//! collector table, so a build with none has nothing to test.
#![cfg(feature = "backfill-serve")]

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use tempfile::TempDir;
use vike_data::{DataFusionHist, HistStore};
use vike_datahub::backfill::BackfillTable;
use vike_datahub::seed::{SEED_VENUE_BURST, SeedLane};
use vike_datahub::serve_authed;
use vike_datahub_client::seed::{SEED_BARS, SEED_INTERVALS};
use vike_datahub_client::{DatahubClient, FEATURE_SEED_SERIES};
use vike_model::Bar;

const VENUE: &str = "binance";
const SYMBOL: &str = "SEEDUSDT";
const INTERVAL: &str = "5m";
const FIVE_MIN_MS: i64 = 300_000;

/// What the fake collector recorded about ONE call — the arguments the SERVER chose, which is the
/// evidence tests 3 and 4 turn on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    symbol: String,
    interval: String,
    start: i64,
    end: i64,
}

/// Everything a test needs to observe the collector without a network.
#[derive(Default)]
struct Spy {
    calls: Mutex<Vec<Call>>,
    count: AtomicUsize,
}

impl Spy {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

fn empty_store() -> (TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    (dir, store)
}

/// Three bars inside any window the server can choose, so a successful seed lands something
/// `LoadBars` can find whatever "now" the server used.
fn fetched_bars(end_ms: i64) -> Vec<Bar> {
    (1..=3)
        .map(|i| Bar {
            ts: end_ms - i * FIVE_MIN_MS,
            open: 100.5,
            high: 101.25,
            low: 99.75,
            close: 100.875,
            volume: 4.5,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

/// A collector table whose `binance` entry RECORDS what it was asked for and appends bars through
/// the same handle the server serves. `bybit` records too and then fails, so a venue that errors is
/// still observable.
fn spying_table(store: Arc<DataFusionHist>, spy: Arc<Spy>) -> BackfillTable {
    let s1 = Arc::clone(&spy);
    let s2 = spy;
    let st = Arc::clone(&store);
    BackfillTable::new(vec![
        (
            "binance".to_string(),
            Box::new(move |symbol: &str, interval: &str, start: i64, end: i64| {
                s1.count.fetch_add(1, Ordering::SeqCst);
                s1.calls.lock().unwrap().push(Call {
                    symbol: symbol.to_string(),
                    interval: interval.to_string(),
                    start,
                    end,
                });
                let bars = fetched_bars(end);
                st.append_bars(VENUE, symbol, interval, &bars, Some("spy-collector"))
                    .map_err(|e| e.to_string())
            }),
        ),
        (
            "bybit".to_string(),
            Box::new(move |symbol: &str, interval: &str, start: i64, end: i64| {
                s2.count.fetch_add(1, Ordering::SeqCst);
                s2.calls.lock().unwrap().push(Call {
                    symbol: symbol.to_string(),
                    interval: interval.to_string(),
                    start,
                    end,
                });
                Err("bybit fake collector: simulated venue failure".to_string())
            }),
        ),
    ])
}

/// Serve `store` + `table`, with the seed lane ARMED or not.
fn spawn(
    store: Arc<dyn HistStore + Send + Sync>,
    table: BackfillTable,
    lane: Option<Arc<SeedLane>>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve_authed(listener, store, Some(table), None, None, lane, None);
    });
    addr
}

/// An armed server with a spying collector, ready to drive.
fn armed() -> (TempDir, Arc<DataFusionHist>, Arc<Spy>, SocketAddr) {
    let (dir, store) = empty_store();
    let spy = Arc::new(Spy::default());
    let table = spying_table(Arc::clone(&store), Arc::clone(&spy));
    let served = Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>;
    let addr = spawn(served, table, Some(Arc::new(SeedLane::new())));
    (dir, store, spy, addr)
}

// -------------------------------------------------------------------------------------------
// 1. The opt-in
// -------------------------------------------------------------------------------------------

/// **With the operator opt-in OFF the verb SUCCEEDS and collects nothing**, and the capability is
/// not advertised.
///
/// This is the single most load-bearing test in the file. `docs/decisions/0057`'s reach property 3
/// is the leg that separates this verb from "a write an Observe client can always cause", and its
/// reopen list names removing the switch. If an unarmed server ever REFUSED instead, the verb would
/// mean "do this" rather than "this is the series my chart is open on", and the whole scope argument
/// would have to be made again on weaker ground.
#[test]
fn an_unarmed_lane_succeeds_and_writes_nothing() {
    let (_dir, store) = empty_store();
    let spy = Arc::new(Spy::default());
    let table = spying_table(Arc::clone(&store), Arc::clone(&spy));
    let served = Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>;
    let addr = spawn(served, table, None);

    let mut c = DatahubClient::connect(addr).expect("connect");
    assert!(
        !c.features().iter().any(|f| f == FEATURE_SEED_SERIES),
        "an unarmed server must not advertise the capability: {:?}",
        c.features()
    );

    // The well-behaved client refuses LOCALLY off that advertisement, and its message names the
    // SWITCH rather than a rebuild — which is what an operator staring at an empty chart needs.
    let local = c.seed_series(VENUE, SYMBOL, INTERVAL).expect_err("the client refuses locally");
    assert!(local.contains("VIKE_DATAHUB_CHART_SEED"), "{local}");
    assert!(local.contains("nothing was sent"), "{local}");

    // ...and the SERVER's own answer, for a client that does not read the handshake: a SUCCESS.
    let done = raw_seed(addr, VENUE, SYMBOL, INTERVAL).expect("the server answers SeriesSeeded");
    assert!(!done.armed, "an unarmed lane reports armed=false");
    assert_eq!(done.rows_written, 0);
    assert_eq!(done.range, None, "no window was chosen because nothing was fetched");
    assert_eq!(spy.count(), 0, "NO collector ran");
    assert!(
        store.load_bars(VENUE, SYMBOL, INTERVAL, vike_data::TsRange::all()).unwrap().is_empty(),
        "and NOTHING was written"
    );
}

// -------------------------------------------------------------------------------------------
// 2. Exactly one fetch
// -------------------------------------------------------------------------------------------

/// **A chart key with no rows triggers exactly ONE fetch** — not one per frame and not one per
/// window — and the second request is answered off the store rather than off the venue.
///
/// Proved on the SERVER, deliberately, because the client's own once-per-session ledger is a
/// different leg: this asserts that a client which ignores its ledger entirely still costs the venue
/// one call. Twenty requests, one collector invocation.
#[test]
fn a_second_request_for_the_same_series_fetches_nothing() {
    let (_dir, _store, spy, addr) = armed();
    let mut c = DatahubClient::connect(addr).expect("connect");
    assert!(c.features().iter().any(|f| f == FEATURE_SEED_SERIES), "armed servers advertise");

    let first = c.seed_series(VENUE, SYMBOL, INTERVAL).expect("first seed");
    assert!(first.armed);
    assert!(!first.repeated, "the first is not a repeat");
    assert_eq!(first.rows_written, 3);

    for i in 0..19 {
        let again = c.seed_series(VENUE, SYMBOL, INTERVAL).expect("repeat seed");
        assert!(again.armed, "repeat {i}");
        assert!(again.repeated, "repeat {i} must report repeated=true");
        assert_eq!(again.rows_written, 0, "repeat {i} writes nothing");
        // It still reports what the store holds, so a client can act on the answer.
        assert!(again.first_ts.is_some(), "repeat {i} still reports the stored window");
    }
    assert_eq!(spy.count(), 1, "the venue was called EXACTLY once across 20 requests");
}

// -------------------------------------------------------------------------------------------
// 3. The server owns the window
// -------------------------------------------------------------------------------------------

/// **The fetch is bounded to the server's own limits regardless of what the client asks** — and the
/// strongest form of that is structural: the request carries no range FIELD at all, so this test
/// asserts the window the collector was handed is the one `SEED_BARS` and the interval imply.
#[test]
fn the_window_is_the_servers_and_the_client_names_no_part_of_it() {
    let (_dir, _store, spy, addr) = armed();
    let mut c = DatahubClient::connect(addr).expect("connect");
    let done = c.seed_series(VENUE, SYMBOL, INTERVAL).expect("seed");
    let (start, end) = done.range.expect("an armed fetch reports its window");
    assert_eq!(
        end - start,
        FIVE_MIN_MS * i64::from(SEED_BARS),
        "the window is exactly SEED_BARS wide"
    );
    let calls = spy.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].start, start, "the collector got the server's start");
    assert_eq!(calls[0].end, end, "the collector got the server's end");
    assert_eq!(calls[0].interval, INTERVAL);
    assert_eq!(calls[0].symbol, SYMBOL);

    // ...and the same holds at a different resolution, which is the property that makes opening a
    // 1m chart and a 1d chart cost one request each rather than wildly different work.
    let done_1d = c.seed_series(VENUE, SYMBOL, "1d").expect("seed 1d");
    let (s2, e2) = done_1d.range.expect("window");
    assert_eq!(e2 - s2, 86_400_000 * i64::from(SEED_BARS));
}

// -------------------------------------------------------------------------------------------
// 4. The untrusted path
// -------------------------------------------------------------------------------------------

/// **An interval outside the validated set is refused BEFORE reaching a bridge.**
///
/// ⚠ **The assertion is `spy.count() == 0`, not the refusal text**, and that choice is the whole
/// value of this test. `crates/bridges/binance/src/family/klines.rs`'s `klines_url` interpolates the
/// interval into a REST query string with no allowlist and no encoding, so what must be true is that
/// the string NEVER REACHES A COLLECTOR — a test asserting only that an error came back would still
/// pass against a server that dispatched first and refused afterwards.
///
/// ⚠ **MUTATION-PROVED against PRODUCTION code, not against the harness.** Deleting the
/// `validate_seed_interval` call from `crates/vike-datahub/src/server.rs`'s `seed_series_verb`
/// reddens this test on the `1s`/`1w`/`1M` rows (the collector is reached, `spy.count()` becomes 1);
/// deleting the `validate_seed_symbol` call beside it reddens the symbol rows below the same way.
/// Neither mutation touches this file.
#[test]
fn an_interval_outside_the_validated_set_never_reaches_a_bridge() {
    let (_dir, _store, spy, addr) = armed();
    let mut c = DatahubClient::connect(addr).expect("connect");

    // `1s` is the interesting one: binance genuinely SERVES it (MEASURED), so this row is refused
    // by the SET rather than by the venue — which is what makes it a stopgap the per-venue interval
    // table will own rather than a venue fact.
    for bad in ["1s", "1w", "1M", "1m; DROP", "1m&limit=99999", "../../etc", ""] {
        let err = raw_seed(addr, VENUE, SYMBOL, bad).expect_err("must be refused");
        assert!(err.contains("before any venue"), "{bad}: {err}");
        assert_eq!(spy.count(), 0, "interval {bad:?} reached a collector");
    }
    // ...and the symbol half of the same door, which is the field the SAME unencoded `format!`
    // interpolates and which this brief's trap notice did not name.
    for bad in ["BTC&interval=1s", "BTC USDT", "BTC#x", "BTC?a=b", ""] {
        let err = raw_seed(addr, VENUE, bad, INTERVAL).expect_err("must be refused");
        assert!(err.contains("seed"), "{bad}: {err}");
        assert!(!err.contains(bad) || bad.is_empty(), "the refusal echoed the symbol: {err}");
        assert_eq!(spy.count(), 0, "symbol {bad:?} reached a collector");
    }
    // The guard: a PERMITTED interval on the same server does reach one, so the zeros above are a
    // refusal rather than a dead harness.
    assert!(SEED_INTERVALS.contains(&INTERVAL));
    c.seed_series(VENUE, SYMBOL, INTERVAL).expect("the permitted interval is served");
    assert_eq!(spy.count(), 1, "guard: the collector IS reachable on this server");
}

/// An unsupported VENUE is refused naming the supported set, and — like every other door check —
/// costs no fetch.
#[test]
fn an_unsupported_venue_is_refused_by_name() {
    let (_dir, _store, spy, addr) = armed();
    let err = raw_seed(addr, "deribit", SYMBOL, INTERVAL).expect_err("must be refused");
    assert!(err.contains("deribit"), "{err}");
    assert!(err.contains("binance") && err.contains("bybit"), "the supported set is named: {err}");
    assert_eq!(spy.count(), 0);
}

// -------------------------------------------------------------------------------------------
// 5. The venue budget
// -------------------------------------------------------------------------------------------

/// The per-venue token bucket refuses by name once the burst is spent, and the refusal says WHY —
/// naming the order-signing daemon, because that is what the bound exists for.
///
/// Distinct series each time, so the ledger's free-repeat arm cannot be what stops it.
#[test]
fn the_venue_budget_refuses_by_name_once_the_burst_is_spent() {
    let (_dir, _store, spy, addr) = armed();
    let mut c = DatahubClient::connect(addr).expect("connect");
    for i in 0..SEED_VENUE_BURST {
        c.seed_series(VENUE, &format!("BURST{i}USDT"), INTERVAL)
            .unwrap_or_else(|e| panic!("burst {i} must be served: {e}"));
    }
    let err = c.seed_series(VENUE, "OVERUSDT", INTERVAL).expect_err("the burst is spent");
    assert!(err.contains("budget is spent"), "{err}");
    assert!(err.contains("order-signing daemon"), "the reason is named: {err}");
    assert_eq!(spy.count(), SEED_VENUE_BURST as usize, "the refused one cost no fetch");

    // A DIFFERENT venue is unaffected — the buckets are per venue, so one busy chart cannot strand
    // another venue's.
    let other = c.seed_series("bybit", "BYBITUSDT", INTERVAL);
    assert!(
        other.is_err_and(|e| e.contains("simulated venue failure")),
        "bybit's own bucket admitted the request and its collector ran"
    );
    assert_eq!(spy.count(), SEED_VENUE_BURST as usize + 1);
}

// -------------------------------------------------------------------------------------------
// The write-through contract
// -------------------------------------------------------------------------------------------

/// A successful seed's rows are in the store BEFORE the reply is written, so the `LoadBars` the
/// client sends next finds them — the same write-through proof `backfill_roundtrip.rs` makes for
/// its sibling verb, over the SAME connection.
#[test]
fn a_seeded_series_is_readable_over_the_same_wire_immediately() {
    let (_dir, _store, _spy, addr) = armed();
    let mut c = DatahubClient::connect(addr).expect("connect");
    let done = c.seed_series(VENUE, SYMBOL, INTERVAL).expect("seed");
    let (start, end) = done.range.expect("window");
    assert_eq!(done.rows_written, 3);
    assert_eq!(done.first_ts, Some(end - 3 * FIVE_MIN_MS));

    let bars = c
        .load_bars(VENUE, SYMBOL, INTERVAL, vike_data::TsRange::of(start, end))
        .expect("the follow-up read over the same connection");
    assert_eq!(bars.len(), 3, "the rows the seed reported are the rows the read finds");
    assert_eq!(bars[0].ts, done.first_ts.unwrap());
    assert_eq!(bars[2].ts, done.last_ts.unwrap());
}

// -------------------------------------------------------------------------------------------
// 6. The CLASS claim — gate 2b, the SERVER-side leg of `docs/decisions/0061` Phase 3
// -------------------------------------------------------------------------------------------
//
// Every test below drives `raw_seed_classed`, i.e. a client that read no handshake and ran no local
// check. That is deliberate and it is the only way to observe this gate: the two client-side legs
// (the advertisement and the local refusal) are proved in
// `crates/vike-datahub-client/tests/seed_class_negotiation.rs`, and neither of them holds for a
// client that skips them. This is the leg that does.

/// **The BUILD-fact advertisement, leg 1** — `seed_class` rides in `Welcome.features` whether or
/// not the lane is armed, where `seed_series` does not.
///
/// The two answer different questions and the test is the argument: gating the class capability on
/// the LANE would make an unarmed-but-modern daemon indistinguishable from one that predates the
/// field, and a client would then withhold a claim from a server that understands it — which is the
/// silent downgrade the field exists to prevent.
#[test]
fn the_class_capability_is_a_build_fact_and_the_lane_capability_is_not() {
    let (_dir, store) = empty_store();
    let spy = Arc::new(Spy::default());
    let table = spying_table(Arc::clone(&store), Arc::clone(&spy));
    let served = Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>;
    let unarmed = spawn(served, table, None);
    let f = DatahubClient::connect(unarmed).expect("connect").features().to_vec();
    assert!(
        f.iter().any(|s| s == vike_datahub_client::FEATURE_SEED_CLASS),
        "the class field is a property of the BUILD and is advertised unconditionally: {f:?}"
    );
    assert!(
        !f.iter().any(|s| s == FEATURE_SEED_SERIES),
        "...while the LANE is a property of the operator's switch: {f:?}"
    );

    let (_d2, _s2, _spy2, armed_addr) = armed();
    let g = DatahubClient::connect(armed_addr).expect("connect").features().to_vec();
    assert!(g.iter().any(|s| s == vike_datahub_client::FEATURE_SEED_CLASS), "{g:?}");
    assert!(g.iter().any(|s| s == FEATURE_SEED_SERIES), "{g:?}");
}

/// **Gate 2b(a)** — a class this venue's data path cannot address is refused BY NAME, before the
/// venue lookup and before the lane can spend a token. The table consulted is
/// `vike_catalog::addressing_for`, the same one the bridges' own `route_target` asks first.
#[test]
fn a_class_this_venue_cannot_address_is_refused_and_no_collector_runs() {
    let (_dir, store, spy, addr) = armed();
    let err = raw_seed_classed(addr, VENUE, SYMBOL, INTERVAL, Some(vike_model::AssetClass::Equity))
        .expect_err("binance's kline path addresses no equity");
    assert!(err.contains("Equity"), "the refusal names the claim: {err}");
    assert!(err.contains("CryptoSpot"), "...and what IS addressable, so it leaves an act: {err}");
    assert_eq!(spy.count(), 0, "refused before the collector, so no venue call was spent");
    assert!(
        store.load_bars(VENUE, SYMBOL, INTERVAL, vike_data::TsRange::all()).unwrap().is_empty(),
        "and nothing was written"
    );
    // An UNKNOWN venue reaches the same refusal through the addressing table's own fallback rather
    // than through the collector table's "no collector in this build" — which is the ORDER of the
    // gates being asserted, not just their content.
    let unknown = raw_seed_classed(
        addr,
        "no-such-venue",
        SYMBOL,
        INTERVAL,
        Some(vike_model::AssetClass::CryptoSpot),
    )
    .expect_err("an unknown venue addresses nothing");
    assert!(unknown.contains("addresses"), "{unknown}");
}

/// ⚠ **Gate 2b(b), and the sharpest test in this file.** A `CryptoPerp` claim on a BARE symbol at a
/// `Naming::PerpSuffix` venue is a request this server would answer FROM THE SPOT BOOK — the class
/// reaches no bridge from here — and would then report rows written for the series a chart is about
/// to read. That is `docs/decisions/0061`'s measured bug one layer below the one
/// `FEATURE_SEED_CLASS` closes, so it is REFUSED rather than honoured or dropped.
///
/// The refusal must name the spelling that works, because a caller who cannot act on it will simply
/// re-send without the claim and get the same wrong book.
#[test]
fn a_perpetual_claim_on_a_bare_symbol_is_refused_and_names_the_spelling_that_works() {
    let (_dir, store, spy, addr) = armed();
    let err =
        raw_seed_classed(addr, VENUE, SYMBOL, INTERVAL, Some(vike_model::AssetClass::CryptoPerp))
            .expect_err("a perpetual claim on a bare symbol must not be served from spot");
    assert!(err.contains(vike_catalog::PERP_SUFFIX), "the refusal names the suffix: {err}");
    assert!(err.contains("nothing was written"), "{err}");
    assert_eq!(spy.count(), 0, "NO collector ran — the refusal is before dispatch");
    assert!(
        store.load_bars(VENUE, SYMBOL, INTERVAL, vike_data::TsRange::all()).unwrap().is_empty(),
        "THE DEFECT: the spot tape must not land under this series because a perp was asked for"
    );

    // ...and the mirror image, which the bridges also refuse: a SPOT claim on a suffixed symbol.
    let suffixed = format!("{SYMBOL}{}", vike_catalog::PERP_SUFFIX);
    let back = raw_seed_classed(
        addr,
        VENUE,
        &suffixed,
        INTERVAL,
        Some(vike_model::AssetClass::CryptoSpot),
    )
    .expect_err("two claims that disagree");
    assert!(back.contains("disagree"), "{back}");
    assert_eq!(spy.count(), 0);
}

/// A claim that AGREES with the spelling is served, and served IDENTICALLY to the same request with
/// no claim — same symbol on the wire, same window, same rows. That is what makes gate 2b a gate
/// rather than a second routing input: the class this server can honour is exactly the one the
/// unclassed route would already have taken.
#[test]
fn a_claim_that_agrees_with_the_spelling_is_served_byte_identically() {
    let (_dir, _store, spy, addr) = armed();
    let done =
        raw_seed_classed(addr, VENUE, SYMBOL, INTERVAL, Some(vike_model::AssetClass::CryptoSpot))
            .expect("a spot claim on a bare symbol is exactly what the route already does");
    assert!(done.armed);
    assert!(done.rows_written > 0);
    let calls = spy.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].symbol, SYMBOL, "the claim changed nothing about what was asked for");
    assert_eq!(calls[0].interval, INTERVAL);
}

/// ⚠ **The ledger stays a TRIPLE, and gate 2b is what makes that safe.**
/// `SeedLane::admit` keys on `(venue, symbol, interval)` and the read-back below it keys on the
/// same, because 0061's store-key verdict keeps the key the SYMBOL. So a class must never be able
/// to make one triple mean two books — and it cannot, because the only claims this door honours are
/// the ones the spelling already made.
///
/// Asserted from the ledger's side: a classed seed and the class-less repeat of it are ONE series
/// and ONE fetch, not two.
#[test]
fn the_class_never_splits_the_lanes_ledger_so_one_series_is_still_one_fetch() {
    let (_dir, _store, spy, addr) = armed();
    let first =
        raw_seed_classed(addr, VENUE, SYMBOL, INTERVAL, Some(vike_model::AssetClass::CryptoSpot))
            .expect("first seed");
    assert!(!first.repeated);
    assert_eq!(spy.count(), 1);

    let again = raw_seed(addr, VENUE, SYMBOL, INTERVAL).expect("the class-less repeat");
    assert!(again.repeated, "the same series, whatever the claim said");
    assert_eq!(spy.count(), 1, "still exactly ONE fetch");
}

// -------------------------------------------------------------------------------------------
// Harness
// -------------------------------------------------------------------------------------------

/// Send `Request::SeedSeries` on a FRESH connection WITHOUT the client's capability or validation
/// guards — the "a client that does not read the handshake" path, which is the only way to observe
/// the SERVER's own door. Returns the server's `SeedDone` or its error text.
fn raw_seed(
    addr: SocketAddr,
    venue: &str,
    symbol: &str,
    interval: &str,
) -> Result<vike_datahub_client::proto::SeedDone, String> {
    raw_seed_classed(addr, venue, symbol, interval, None)
}

/// [`raw_seed`], plus the `docs/decisions/0061` Phase 3 class claim — the shape a client that
/// skipped BOTH client-side legs sends, which is the only way to observe the server's own gate 2b.
fn raw_seed_classed(
    addr: SocketAddr,
    venue: &str,
    symbol: &str,
    interval: &str,
    class: Option<vike_model::AssetClass>,
) -> Result<vike_datahub_client::proto::SeedDone, String> {
    use vike_datahub_client::proto::{Request, Response, read_frame, write_frame};
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    let req = Request::SeedSeries {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        interval: interval.to_string(),
        class,
    };
    write_frame(&mut s, &req).expect("write");
    match read_frame::<_, Response>(&mut s).expect("read") {
        Response::SeriesSeeded(done) => Ok(done),
        Response::Error(msg) => Err(msg),
        other => panic!("unexpected response: {other:?}"),
    }
}
