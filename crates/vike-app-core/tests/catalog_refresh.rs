//! **The per-venue instrument-catalog refresh, driven end to end against a scripted provider.**
//!
//! `vike-desktop` is outside the derived CI roster
//! (`crates/vike-ops/tests/ci_excluded_gui_shell_ratchet.rs` exists to keep it that way), so the
//! refresh engine lives in `vike-app-core` and is gated here. Five properties, each with the defect
//! it exists for:
//!
//! 1. **A successful refresh moves the stamp AND the count**, which is the whole of how an operator
//!    tells a press that worked from one that did nothing.
//! 2. **⚠ A FAILED refresh keeps the previous cache.** This is the one that matters: the picker
//!    reads one universe, and replacing a venue's symbols with an empty list makes every one of
//!    that venue's instruments unreachable in the picker with nothing on screen to say why. The
//!    test is mutation-proved against production code — see its own doc.
//! 3. **An EMPTY answer is a failure, not a truth.** A venue that returns `Ok(vec![])` — a
//!    rate-limit page that parsed, a listing endpoint that answered with an empty array — must not
//!    be allowed to wipe a good list. This is the same property as 2 through a different door, and
//!    the door a real venue is far more likely to come through.
//! 4. **The fetch does NOT run on the caller's thread.** `request` returns while the socket is
//!    still open, which is what keeps a GUI frame off a venue REST call. Proved by having the
//!    scripted provider record the thread it ran on.
//! 5. **The picker sees the new list.** A refresh that rewrites a file the picker never re-reads is
//!    a button that appears to work; the engine publishes the whole merged universe over the SAME
//!    channel the symbol picker already drains, and this asserts the `Catalog` arrives with the new
//!    symbol in it.
//! 6. **The DIAL is late-bound and a blip does not take it away.** The handle is built once at
//!    `App::new` while the datahub address and the observe-key name are per-frame properties of the
//!    ACTIVE BACKEND RECORD, so a dial baked into the constructor goes stale SILENTLY — a stale
//!    address still connects, and signing backend B's datahub with backend A's key is `bad mac`.
//!    Section 7 gates the compare-and-hold rule `MdSession::set_addr` exists for.
//!
//! ⚠ The WIRE half — what a `ServerBacked` press does against a server that answers — is
//! `crates/vike-app-core/tests/catalog_wire.rs`, driven over a real loopback datahub double.
//! What is gated HERE is the routing decision, the budget, and the one wire path that needs no
//! server at all (an unreachable one).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_app_core::catalog_refresh::{
    CatalogRefresh, REFRESH_COOLDOWN_MS, RefreshAvailability, RefreshBlock, RefreshOutcome,
    VenueCatalogRow, VenueRefreshState, merge_refresh, refresh_block,
};
use vike_catalog::{
    AssetClass, Catalog, CatalogError, CatalogMode, CatalogProvider, CatalogSource, Instrument,
    load_cache,
};

/// How long a test waits for the worker thread's `wake`. Generous — a loaded the CI box runner is the
/// box this has to be non-flaky on, and the work itself is a scripted `Vec`, so a miss is a real
/// failure rather than a slow machine.
const WAIT: Duration = Duration::from_secs(10);

/// ⚠ **Every `now_ms` a test passes has to be on the REAL clock.** `refresh_block` compares the
/// caller's `now_ms` against the instant the worker stamped, and the worker stamps with
/// `vike_model::now_ms()` — so a synthetic `0` makes every elapsed span hugely negative and the
/// budget refuses a press the test meant to allow. The engine takes the instant as a PARAMETER so
/// the renderer has one instant per frame, not so a test can invent an epoch.
fn now() -> i64 {
    vike_model::now_ms()
}

/// An instant safely past [`REFRESH_COOLDOWN_MS`] from any press made during this test.
fn after_cooldown() -> i64 {
    now() + REFRESH_COOLDOWN_MS + 5_000
}

fn inst(venue: &str, sym: &str) -> Instrument {
    Instrument {
        venue: venue.into(),
        raw_symbol: sym.into(),
        asset_class: AssetClass::CryptoSpot,
        base: "BTC".into(),
        quote: "USDT".into(),
        description: String::new(),
        properties: Default::default(),
        // ⚠ `None` is the ORDINARY state for both, not a gap this fixture is papering over: most
        // venues publish no contract-type word at all, and this suite drives `CryptoSpot`, where
        // there is nothing to settle in. See `vike_catalog::Instrument::contract_type`.
        contract_type: None,
        settle_asset: None,
    }
}

/// A `CatalogProvider` whose answer the test writes, which records how many times it was asked and
/// on which thread — the double the whole suite is driven through, so no test needs a network.
struct Scripted {
    venue: &'static str,
    mode: CatalogMode,
    answer: Mutex<Result<Vec<Instrument>, String>>,
    calls: AtomicUsize,
    ran_on: Mutex<Option<std::thread::ThreadId>>,
}

impl Scripted {
    fn enumerable(venue: &'static str, symbols: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            venue,
            mode: CatalogMode::Enumerable,
            answer: Mutex::new(Ok(symbols.iter().map(|s| inst(venue, s)).collect())),
            calls: AtomicUsize::new(0),
            ran_on: Mutex::new(None),
        })
    }

    fn query_backed(venue: &'static str) -> Arc<Self> {
        Arc::new(Self {
            venue,
            mode: CatalogMode::QueryBacked,
            answer: Mutex::new(Ok(Vec::new())),
            calls: AtomicUsize::new(0),
            ran_on: Mutex::new(None),
        })
    }

    fn will(&self, answer: Result<Vec<Instrument>, String>) {
        *self.answer.lock().unwrap() = answer;
    }
}

impl CatalogProvider for Scripted {
    fn venue(&self) -> &str {
        self.venue
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::CryptoSpot]
    }
    fn mode(&self) -> CatalogMode {
        self.mode
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.ran_on.lock().unwrap() = Some(std::thread::current().id());
        self.answer.lock().unwrap().clone().map_err(CatalogError)
    }
}

/// The handle plus the picker's receiving end and a completion signal, so every test drives the
/// real `CatalogRefresh` rather than its parts.
struct Rig {
    catalog: CatalogRefresh,
    picker: Receiver<Catalog>,
}

impl Rig {
    fn new(providers: Vec<Arc<dyn CatalogProvider>>, cache: Option<std::path::PathBuf>) -> Self {
        let (pub_tx, picker) = channel();
        Rig { catalog: CatalogRefresh::new(providers, cache, None, None, pub_tx), picker }
    }

    /// Press Refresh on `venue` and block until the worker says it finished.
    fn refresh(&self, venue: &str, now_ms: i64) -> Result<(), RefreshBlock> {
        let (tx, rx) = channel();
        self.catalog.request(venue, now_ms, move || {
            let _ = tx.send(());
        })?;
        rx.recv_timeout(WAIT).expect("the refresh worker never finished");
        Ok(())
    }

    fn row(&self, venue: &str) -> VenueCatalogRow {
        self.catalog.rows().into_iter().find(|r| r.venue == venue).expect("roster venue")
    }
}

// ------------------------------------------------------------------------------------------------
// 1. A successful refresh
// ------------------------------------------------------------------------------------------------

/// **A press updates the stamp and the count.** A button with no "last refreshed" is a button you
/// press twice because you cannot tell whether it worked.
#[test]
fn a_successful_refresh_updates_the_stamp_and_the_count() {
    let p = Scripted::enumerable("binance", &["BTCUSDT", "ETHUSDT"]);
    let rig = Rig::new(vec![p.clone()], None);

    let before = rig.row("binance");
    assert!(before.stamp.is_none(), "nothing has been fetched yet");
    assert_eq!(before.count(), 0);
    assert_eq!(before.availability(), RefreshAvailability::Direct);
    assert_eq!(
        before.source,
        Some(CatalogSource::Direct),
        "a linked provider for a publicly-enumerable venue is a Direct source"
    );

    rig.refresh("binance", now()).expect("the press is allowed");

    let after = rig.row("binance");
    let stamp = after.stamp.as_ref().expect("a successful refresh writes a stamp");
    assert_eq!(stamp.count, 2, "the stamp carries the fetched count");
    assert!(stamp.last_refreshed_ms > 0, "the stamp carries a real instant");
    assert_eq!(after.count(), 2);
    assert_eq!(rig.catalog.total(), 2, "the whole cache holds the two");
    match after.state {
        VenueRefreshState::Done {
            outcome: RefreshOutcome::Refreshed { count, previous, truncated },
            ..
        } => {
            assert_eq!((count, previous), (2, 0));
            assert!(!truncated, "no cap sits between a LINKED provider and the fold");
        }
        other => panic!("expected a completed refresh, got {other:?}"),
    }
}

// ------------------------------------------------------------------------------------------------
// 2. The failure rule — the one this module exists for
// ------------------------------------------------------------------------------------------------

/// **⚠ A FAILED refresh keeps the previous cache**, down to the stamp.
///
/// MUTATION-PROVED against production code, not against the harness: with
/// `crates/vike-app-core/src/catalog_refresh.rs`'s `merge_refresh` altered so the `Err` arm falls
/// through to the `retain`/`extend` instead of returning —
///
/// ```text
/// let list = match fetched { Err(_) => Vec::new(), Ok(list) => list };
/// ```
///
/// — this test fails on its FIRST assertion, for its stated reason:
/// `the failed venue's instruments must survive: left 0, right 2`. Restored, it passes. The
/// mutation is in the fold, which is the only code that can discard a cached list.
#[test]
fn a_failed_refresh_keeps_the_previous_cache() {
    let p = Scripted::enumerable("binance", &["BTCUSDT", "ETHUSDT"]);
    let rig = Rig::new(vec![p.clone()], None);
    rig.refresh("binance", now()).expect("the seeding press is allowed");
    let seeded = rig.row("binance");
    let seeded_at = seeded.stamp.as_ref().unwrap().last_refreshed_ms;
    assert_eq!(seeded.count(), 2);

    // The venue now refuses. `REFRESH_COOLDOWN_MS` has to be cleared or the press is refused
    // before the provider is ever asked — which would make this test pass for the wrong reason.
    p.will(Err("connection reset by peer".into()));
    rig.refresh("binance", after_cooldown()).expect("the second press is allowed");

    let after = rig.row("binance");
    assert_eq!(
        after.count(),
        2,
        "the failed venue's instruments must survive: left {}, right 2",
        after.count()
    );
    assert_eq!(rig.catalog.total(), 2, "and nothing was dropped from the whole cache");
    assert_eq!(
        after.stamp.as_ref().unwrap().last_refreshed_ms,
        seeded_at,
        "the stamp must NOT move — a moved stamp claims a refresh that did not happen"
    );
    match after.state {
        VenueRefreshState::Done { outcome: RefreshOutcome::Failed { kept, ref error }, .. } => {
            assert_eq!(kept, 2, "the failure names what it kept");
            assert!(error.contains("connection reset"), "the venue's own words reach the row");
        }
        other => panic!("expected a reported failure, got {other:?}"),
    }
    assert_eq!(p.calls.load(Ordering::SeqCst), 2, "the provider really was asked a second time");
}

/// **An EMPTY answer is a failure too**, and it is the shape a real venue is likeliest to produce:
/// a rate-limited or empty listing that parses perfectly into zero rows. Adopting it would take
/// every symbol of that venue out of the picker with nothing reported.
#[test]
fn an_empty_answer_never_replaces_a_good_list() {
    let p = Scripted::enumerable("binance", &["BTCUSDT", "ETHUSDT"]);
    let rig = Rig::new(vec![p.clone()], None);
    rig.refresh("binance", now()).expect("seed");

    p.will(Ok(Vec::new()));
    rig.refresh("binance", after_cooldown()).expect("press");

    assert_eq!(rig.row("binance").count(), 2, "an empty answer must not wipe the cached list");
    match rig.row("binance").state {
        VenueRefreshState::Done { outcome: RefreshOutcome::Empty { kept }, .. } => {
            assert_eq!(kept, 2)
        }
        other => panic!("an empty answer must be REPORTED, not silently adopted: {other:?}"),
    }
}

/// A refresh of one venue leaves every other venue's rows and stamp exactly where they were —
/// including when it fails. The picker holds ONE universe, so a per-venue press that touched a
/// sibling would be a cross-venue bug wearing a per-venue button.
#[test]
fn a_refresh_touches_no_other_venue() {
    let binance = Scripted::enumerable("binance", &["BTCUSDT"]);
    let okx = Scripted::enumerable("okx", &["BTC-USDT", "ETH-USDT"]);
    let rig = Rig::new(vec![binance.clone(), okx.clone()], None);
    rig.refresh("binance", now()).expect("seed binance");
    rig.refresh("okx", now()).expect("seed okx");
    let okx_at = rig.row("okx").stamp.as_ref().unwrap().last_refreshed_ms;

    binance.will(Err("timeout".into()));
    rig.refresh("binance", after_cooldown()).expect("press binance");
    assert_eq!(rig.row("okx").count(), 2);
    assert_eq!(rig.row("okx").stamp.as_ref().unwrap().last_refreshed_ms, okx_at);

    binance.will(Ok(vec![inst("binance", "SOLUSDT"), inst("binance", "XRPUSDT")]));
    rig.refresh("binance", after_cooldown()).expect("press binance again");
    assert_eq!(rig.row("binance").count(), 2);
    assert_eq!(rig.row("okx").count(), 2, "okx is still whole after a successful binance refresh");
    assert_eq!(rig.catalog.total(), 4);
}

// ------------------------------------------------------------------------------------------------
// 3. Availability — what the button is offered for
// ------------------------------------------------------------------------------------------------

/// **A QueryBacked venue is never offered a wholesale refresh**, and a venue the routing table
/// sends nowhere is not either. Both are refused by the engine and not merely hidden by the
/// renderer, so a second caller cannot route around it.
///
/// ⚠ **The QueryBacked example is `deribit`, not `ig`, and the swap is the change this PR makes
/// visible.** `RefreshAvailability` is now computed from the ROUTE
/// (`vike_catalog::catalog_source_for`) rather than from the local provider alone, and `ig` routes
/// NOWHERE — it is `CatalogAvailability::NoBulkList`, a fact about the venue that outranks any
/// linked provider's self-description. So the mode arm is now reachable only for a venue that IS
/// routed `Direct` and whose provider reports `QueryBacked`.
#[test]
fn only_a_routed_venue_can_be_refreshed() {
    let enumerable = Scripted::enumerable("binance", &["BTCUSDT"]);
    let query = Scripted::query_backed("deribit");
    let unlisted = Scripted::query_backed("ig");
    let rig = Rig::new(vec![enumerable, query.clone(), unlisted.clone()], None);

    assert_eq!(rig.row("binance").availability(), RefreshAvailability::Direct);
    assert_eq!(rig.row("deribit").availability(), RefreshAvailability::QueryBacked);
    // ig is LINKED here and still routes nowhere: the venue publishes no bulk list at any price.
    assert!(matches!(rig.row("ig").availability(), RefreshAvailability::NoBulkList { .. }));
    assert_eq!(rig.row("ig").source, None, "a venue with no bulk list has no route for anybody");
    // okx is on the roster, has no provider here, and IS publicly enumerable — so it routes to the
    // backend's datahub rather than reading "not in this build".
    assert_eq!(rig.row("okx").availability(), RefreshAvailability::ServerBacked);
    assert_eq!(
        rig.row("okx").source,
        Some(CatalogSource::ServerBacked),
        "a publicly enumerable venue with no local provider routes to a server"
    );
    // ...and the three credentialed ones route nowhere either, for the OTHER reason.
    for v in ["alpaca", "oanda", "ctrader"] {
        assert_eq!(
            rig.row(v).availability(),
            RefreshAvailability::Credentialed { own_keys: false },
            "{v}"
        );
        assert_eq!(rig.row(v).source, None, "{v}");
    }

    assert_eq!(
        rig.catalog.request("deribit", 0, || {}),
        Err(RefreshBlock::Unavailable(RefreshAvailability::QueryBacked))
    );
    assert!(matches!(
        rig.catalog.request("ig", 0, || {}),
        Err(RefreshBlock::Unavailable(RefreshAvailability::NoBulkList { .. }))
    ));
    assert_eq!(
        rig.catalog.request("alpaca", 0, || {}),
        Err(RefreshBlock::Unavailable(RefreshAvailability::Credentialed { own_keys: false }))
    );
    // A ROUTED venue with no dial is refused for a different reason, and the difference is the
    // whole point: this one CAN be refreshed, once something is connected.
    assert_eq!(rig.catalog.request("okx", 0, || {}), Err(RefreshBlock::NoBackend));
    assert_eq!(
        query.calls.load(Ordering::SeqCst) + unlisted.calls.load(Ordering::SeqCst),
        0,
        "a QueryBacked provider is never asked for a bulk list"
    );

    // Every roster venue gets a row — the screen is complete by construction, because the roster
    // is `vike_model::VENUES` through `vike_catalog::BRIDGE_VENUES`.
    assert_eq!(rig.catalog.rows().len(), vike_catalog::BRIDGE_VENUES.len());
}

/// The rate budget: a press inside [`REFRESH_COOLDOWN_MS`] of a completed one is refused, so a
/// held-down button cannot repeat a venue crawl as fast as the network answers.
#[test]
fn a_second_press_inside_the_cooldown_is_refused() {
    let p = Scripted::enumerable("binance", &["BTCUSDT"]);
    let rig = Rig::new(vec![p.clone()], None);
    rig.refresh("binance", now()).expect("first press");
    let at = rig.row("binance").stamp.as_ref().unwrap().last_refreshed_ms;

    match rig.catalog.request("binance", at + 1_000, || {}) {
        Err(RefreshBlock::Cooldown { remaining_ms }) => {
            assert!(remaining_ms > 0 && remaining_ms <= REFRESH_COOLDOWN_MS)
        }
        other => panic!("a press one second later must be refused, got {other:?}"),
    }
    assert_eq!(p.calls.load(Ordering::SeqCst), 1, "the refused press asked the venue nothing");

    rig.refresh("binance", at + REFRESH_COOLDOWN_MS).expect("the cooldown ends, it does not latch");
    assert_eq!(p.calls.load(Ordering::SeqCst), 2);
}

// ------------------------------------------------------------------------------------------------
// 4. Off the frame thread
// ------------------------------------------------------------------------------------------------

/// **The fetch does not run on the caller's thread.** A venue instrument grid is network I/O; run
/// inline it would freeze a GUI frame for the whole round trip, with no repaint and no cancel.
///
/// The provider records the thread it was asked on, and `request` returns a value the test reads
/// BEFORE the wake arrives — so the assertion is on real concurrency, not on a promise.
#[test]
fn the_fetch_does_not_run_on_the_calling_thread() {
    let p = Scripted::enumerable("binance", &["BTCUSDT"]);
    let rig = Rig::new(vec![p.clone()], None);
    let here = std::thread::current().id();

    rig.refresh("binance", now()).expect("press");

    let ran_on = p.ran_on.lock().unwrap().expect("the provider was asked");
    assert_ne!(ran_on, here, "the venue fetch ran on the frame thread — a GUI frame owes 16ms");
}

/// …and the call RETURNS before the fetch finishes. Proved by a provider that blocks until the
/// test releases it: if `request` waited, this test would deadlock instead of failing, so the
/// assertion is that the in-flight state is observable while the provider is still parked.
#[test]
fn request_returns_while_the_fetch_is_still_running() {
    // ⚠ The `Sender` sits behind a `Mutex` because `CatalogProvider` is `Send + Sync` and
    // `std::sync::mpsc::Sender` is `Send` but NOT `Sync` — the same reason the engine keeps its
    // own publish channel inside its lock.
    struct Parked {
        gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
        entered: Mutex<Sender<()>>,
    }
    impl CatalogProvider for Parked {
        fn venue(&self) -> &str {
            "binance"
        }
        fn asset_classes(&self) -> &[AssetClass] {
            &[AssetClass::CryptoSpot]
        }
        fn mode(&self) -> CatalogMode {
            CatalogMode::Enumerable
        }
        fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
            let _ = self.entered.lock().unwrap().send(());
            let (lock, cv) = &*self.gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
            Ok(vec![inst("binance", "BTCUSDT")])
        }
    }

    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let (entered_tx, entered) = channel();
    let rig = Rig::new(
        vec![Arc::new(Parked { gate: Arc::clone(&gate), entered: Mutex::new(entered_tx) })],
        None,
    );
    let (done_tx, done) = channel();
    rig.catalog
        .request("binance", 0, move || {
            let _ = done_tx.send(());
        })
        .expect("press");

    entered.recv_timeout(WAIT).expect("the worker never reached the provider");
    // `request` has returned and the fetch is parked — so the row reads in-flight, which is what
    // the button renders as its busy state.
    assert!(
        matches!(rig.row("binance").state, VenueRefreshState::InFlight { .. }),
        "the row must read in-flight while the fetch is open"
    );
    assert_eq!(
        rig.catalog.request("binance", 0, || {}),
        Err(RefreshBlock::InFlight),
        "a second press while one is open is refused rather than doubling the venue call"
    );

    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    done.recv_timeout(WAIT).expect("the worker never finished");
    assert_eq!(rig.row("binance").count(), 1);
}

// ------------------------------------------------------------------------------------------------
// 5. The picker sees it
// ------------------------------------------------------------------------------------------------

/// **The whole point of the button**: after a refresh, the symbol picker's universe holds the new
/// instruments. A refresh that rewrote a file the picker never re-read would be a button that only
/// appears to work.
///
/// The picker's universe is `App::symbols_catalog`, an `Arc<Catalog>` replaced from a channel drain
/// in `crates/vike-desktop/src/app_ui.rs`'s `frame_begin`. Nothing has to be INVALIDATED: the
/// engine publishes a whole new `Catalog` over that same channel, so the next frame adopts it.
#[test]
fn the_picker_receives_the_refreshed_universe() {
    let binance = Scripted::enumerable("binance", &["BTCUSDT"]);
    let okx = Scripted::enumerable("okx", &["BTC-USDT"]);
    let rig = Rig::new(vec![binance.clone(), okx], None);
    rig.refresh("binance", now()).expect("seed binance");
    rig.refresh("okx", now()).expect("seed okx");

    binance.will(Ok(vec![inst("binance", "BTCUSDT"), inst("binance", "PEPEUSDT")]));
    rig.refresh("binance", after_cooldown()).expect("press");

    // Drain to the newest publish — the picker's own drain is a `try_recv` per frame, so the last
    // one sent is what it ends up holding.
    let mut latest = None;
    while let Ok(c) = rig.picker.try_recv() {
        latest = Some(c);
    }
    let catalog = latest.expect("a successful refresh publishes to the picker");
    let hits = catalog.search("PEPE", &vike_catalog::SearchFilter::default(), 10);
    assert!(
        hits.iter().any(|i| i.raw_symbol == "PEPEUSDT"),
        "the newly listed symbol must be searchable in the picker's universe"
    );
    let all = catalog.search("", &vike_catalog::SearchFilter::default(), 100);
    assert!(
        all.iter().any(|i| i.venue == "okx"),
        "…and the OTHER venue must still be in the universe the picker adopted"
    );
}

/// A failed refresh publishes NOTHING. Sending the unchanged universe would be harmless but
/// dishonest — a frame repaint that says "something changed" when nothing did.
#[test]
fn a_failed_refresh_publishes_nothing_to_the_picker() {
    let p = Scripted::enumerable("binance", &["BTCUSDT"]);
    let rig = Rig::new(vec![p.clone()], None);
    rig.refresh("binance", now()).expect("seed");
    while rig.picker.try_recv().is_ok() {}

    p.will(Err("503".into()));
    rig.refresh("binance", after_cooldown()).expect("press");
    assert!(rig.picker.try_recv().is_err(), "a failed refresh must publish no new universe");
}

// ------------------------------------------------------------------------------------------------
// 6. The disk cache — the policy `vike_catalog::persist` documented
// ------------------------------------------------------------------------------------------------

/// A successful refresh WRITES the cache, a startup READS it, and a venue already in the cache is
/// **not re-fetched** by the startup path — which is what makes the button the only re-fetch there
/// is (`crates/vike-catalog/src/persist.rs`: *"rewritten only on an explicit user refresh"*).
#[test]
fn the_cache_is_written_by_a_refresh_and_read_without_re_fetching() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("catalog.json");

    {
        let p = Scripted::enumerable("binance", &["BTCUSDT", "ETHUSDT"]);
        let rig = Rig::new(vec![p], Some(path.clone()));
        rig.refresh("binance", now()).expect("press");
    }
    let on_disk = load_cache(&path).expect("the refresh wrote the cache");
    assert_eq!(on_disk.instruments.len(), 2);
    assert_eq!(on_disk.fetched.iter().find(|s| s.venue == "binance").unwrap().count, 2);

    // A fresh process over the same cache: it loads, publishes, and asks the venue NOTHING.
    let p = Scripted::enumerable("binance", &["BTCUSDT", "ETHUSDT"]);
    let rig = Rig::new(vec![p.clone()], Some(path.clone()));
    let (tx, done) = channel();
    rig.catalog.spawn_initial(move || {
        let _ = tx.send(());
    });
    done.recv_timeout(WAIT).expect("the startup worker never finished");

    assert_eq!(rig.catalog.total(), 2, "the cache was adopted");
    assert_eq!(rig.row("binance").count(), 2);
    assert_eq!(
        p.calls.load(Ordering::SeqCst),
        0,
        "a cached venue must NOT be re-fetched at startup — the button is the only re-fetch"
    );
    assert!(rig.picker.try_recv().is_ok(), "the cached universe reaches the picker at startup");
}

/// …and a venue the cache has NEVER held is fetched once, so a fresh install does not open on an
/// empty picker with no hint that a button exists.
#[test]
fn a_venue_the_cache_never_held_is_fetched_once_at_startup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = Scripted::enumerable("binance", &["BTCUSDT"]);
    let query = Scripted::query_backed("ig");
    let rig = Rig::new(vec![p.clone(), query.clone()], Some(dir.path().join("catalog.json")));
    let (tx, done) = channel();
    rig.catalog.spawn_initial(move || {
        let _ = tx.send(());
    });
    done.recv_timeout(WAIT).expect("the startup worker never finished");

    assert_eq!(p.calls.load(Ordering::SeqCst), 1, "the cold Enumerable venue was fetched once");
    assert_eq!(
        query.calls.load(Ordering::SeqCst),
        0,
        "a QueryBacked venue is never cached, so it is never fetched wholesale"
    );
    assert_eq!(rig.catalog.total(), 1);
}

/// The pure fold, exercised through the same door the engine uses: a caller outside this crate can
/// reason about the cache without spawning anything.
#[test]
fn the_fold_is_reusable_and_keeps_its_contract_outside_the_engine() {
    let mut cache = vike_catalog::CatalogCache::default();
    assert_eq!(
        merge_refresh(&mut cache, "binance", Ok(vec![inst("binance", "BTCUSDT")]), 10, false),
        RefreshOutcome::Refreshed { count: 1, previous: 0, truncated: false }
    );
    assert_eq!(
        merge_refresh(&mut cache, "binance", Err("boom".into()), 20, false),
        RefreshOutcome::Failed { error: "boom".into(), kept: 1 }
    );
    assert_eq!(cache.instruments.len(), 1);
    assert_eq!(cache.fetched[0].last_refreshed_ms, 10);
}

// ------------------------------------------------------------------------------------------------
// 7. The DIAL — the late-bound half of the ServerBacked route
// ------------------------------------------------------------------------------------------------

/// **A `None` address does NOT clear the dial**, and that is the whole reason `dial_is_stale`
/// exists rather than a bare `set_dial` per frame.
///
/// `crate::datahub_resolve::resolve_datahub_addr`'s second rung is the ACTIVE backend's `Welcome`
/// advertisement, which the observe bridge clears on every link blip and restores only after the
/// next handshake. On the documented default box that advertisement IS the whole address ladder,
/// so a rule that cleared on `None` would turn every live button into a `NoBackend` refusal — for
/// a datahub that is a different process from the tradehub that blipped and did not go anywhere.
/// The verbatim rule of `MdSession::set_addr`, which exists for this precise bug.
#[test]
fn a_transient_advertisement_gap_does_not_take_the_dial_away() {
    let rig = Rig::new(vec![Scripted::enumerable("binance", &["BTCUSDT"])], None);
    assert!(
        rig.catalog.dial_is_stale(Some("<host>:7878"), "VIKE_DATAHUB_OBSERVE_KEY"),
        "an empty handle needs the first dial"
    );
    rig.catalog.set_dial(Some(dial("<host>:7878")));
    assert_eq!(rig.row("okx").server.as_deref(), Some("<host>:7878"));

    assert!(
        !rig.catalog.dial_is_stale(None, "VIKE_DATAHUB_OBSERVE_KEY"),
        "a blip is NOT a different server"
    );
    assert!(
        !rig.catalog.dial_is_stale(Some("<host>:7878"), "VIKE_DATAHUB_OBSERVE_KEY"),
        "the same pair resolves nothing — the credential store is opened only on a change"
    );
    assert!(
        rig.catalog.dial_is_stale(Some("<host>:7878"), "VIKE_DATAHUB_OBSERVE_KEY"),
        "a DIFFERENT address is a different server"
    );
    assert!(
        rig.catalog.dial_is_stale(Some("<host>:7878"), "BACKEND_B_OBSERVE_KEY"),
        "…and a different KEY NAME is a different backend record, which is `bad mac` if ignored"
    );
}

/// A dial reaches every `ServerBacked` row and NO `Direct` one cares — a local fetch needs no
/// datahub, so connecting or losing a backend cannot change whether deribit is refreshable.
#[test]
fn the_dial_gates_the_routed_rows_and_not_the_linked_one() {
    let rig = Rig::new(vec![Scripted::enumerable("deribit", &["BTC-PERPETUAL"])], None);
    assert_eq!(rig.catalog.request("okx", now(), || {}), Err(RefreshBlock::NoBackend));
    assert_eq!(refresh_block(&rig.row("deribit"), now()), None, "the linked venue is unaffected");

    rig.catalog.set_dial(Some(dial("127.0.0.1:7878")));
    assert_eq!(
        refresh_block(&rig.row("okx"), now()),
        None,
        "a dial makes the routed row pressable"
    );
    rig.catalog.set_dial(None);
    assert_eq!(refresh_block(&rig.row("okx"), now()), Some(RefreshBlock::NoBackend));
    assert_eq!(refresh_block(&rig.row("deribit"), now()), None);
}

/// **A `ServerBacked` fetch that cannot reach its datahub is a FAILURE, not a refusal, and it
/// keeps the cached list** — the same rule the local route has, proved over the real wire leg
/// against a port nothing is listening on.
///
/// This is the end-to-end plumbing test for the route: `request` → `Route::Server` →
/// `catalog_wire::fetch_venue_catalog` → `ConnectFailed` → `settle` → `merge_refresh`'s `Err` arm.
#[test]
fn an_unreachable_datahub_fails_the_routed_venue_and_keeps_its_cache() {
    let rig = Rig::new(vec![Scripted::enumerable("deribit", &["BTC-PERPETUAL"])], None);
    // Seed okx's slice through the pure fold, so there is something a bad fetch could destroy.
    rig.catalog.set_dial(Some(dial(&dead_addr())));
    rig.refresh("okx", now()).expect("a routed press with a dial is allowed");

    match rig.row("okx").state {
        VenueRefreshState::Done { outcome: RefreshOutcome::Failed { ref error, kept }, .. } => {
            assert_eq!(kept, 0);
            assert!(error.contains("could not be reached"), "the dial's own words reach the row");
        }
        other => panic!("expected a reported failure, got {other:?}"),
    }
    assert!(rig.picker.try_recv().is_err(), "a failed routed refresh publishes no new universe");
    assert!(rig.row("okx").stamp.is_none(), "…and stamps nothing");
}

/// A dial pointed at a dead port, for the tests above. `CatalogDial` carries no socket, so this
/// costs nothing until something actually dials it.
fn dial(addr: &str) -> vike_app_core::catalog_wire::CatalogDial {
    vike_app_core::catalog_wire::CatalogDial {
        addr: addr.to_string(),
        key_name: "VIKE_DATAHUB_OBSERVE_KEY".to_string(),
        keys: None,
    }
}

/// A loopback address with nothing on it — bound, read, and dropped, so the port is free and a
/// connect to it is refused rather than left hanging. (Binding port 0 and closing is the standard
/// way to name a port nothing holds; a hard-coded one could collide on a shared runner.)
fn dead_addr() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a throwaway port");
    let addr = l.local_addr().expect("local_addr").to_string();
    drop(l);
    addr
}
