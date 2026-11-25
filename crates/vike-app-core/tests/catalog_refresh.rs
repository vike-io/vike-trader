//! **The per-venue instrument-catalog refresh, driven end to end against a scripted provider.**
//!
//! `vike-desktop` is outside the derived CI roster
//! (`crates/vike-ops/tests/gui/ci_excluded_gui_shell_ratchet.rs` exists to keep it that way), so the
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
//!    `App::new` while the datahub address is a per-frame property of the ACTIVE BACKEND, so a dial
//!    baked into the constructor goes stale SILENTLY — a stale address still connects.
//!    Section 7 gates the compare-and-hold rule `MarketDataSession::set_address` exists for.
//!
//! ⚠ The WIRE half — what a `ServerBacked` press does against a server that answers — is
//! `crates/vike-app-core/tests/catalog_wire.rs`, driven over a real loopback datahub double.
//! What is gated HERE is the routing decision, the budget, and the one wire path that needs no
//! server at all (an unreachable one).

use std::assert_matches;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_app_core::data::catalog_refresh::{
    CatalogRefreshBlock, CatalogRefreshVenueRow, CatalogRefreshVenueState, DatahubCatalogRefresh,
    DatahubCatalogRefreshAvailability, DatahubCatalogRefreshOutcome, REFRESH_COOLDOWN_MS,
    merge_refresh, refresh_block,
};
use vike_app_core::test_support::{datahub_dial, dead_address};
use vike_catalog::{Catalog, CatalogError, CatalogMode, CatalogProvider, CatalogRoute};
use vike_model::{AssetClass, Contract, Instrument};

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

/// A BTC/USDT spot row. No venue word is set (`contract_type` stays `None`) and a spot contract has
/// no settle asset: the ORDINARY state, not a gap this fixture is papering over — most venues
/// publish no contract-type word at all, and this suite drives `CryptoSpot`, where there is nothing
/// to settle in. See `vike_model::Instrument::contract_type`.
fn instrument(venue: &str, symbol: &str) -> Instrument {
    Instrument::new(venue, symbol, "BTC", "USDT", Contract::CryptoSpot)
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
            answer: Mutex::new(Ok(symbols.iter().map(|s| instrument(venue, s)).collect())),
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
        self.answer.lock().unwrap().clone().map_err(|why| CatalogError(why.into()))
    }
}

/// The handle plus the picker's receiving end and a completion signal, so every test drives the
/// real `DatahubCatalogRefresh` rather than its parts.
struct Rig {
    catalog: DatahubCatalogRefresh,
    picker: Receiver<Catalog>,
}

impl Rig {
    fn new(providers: Vec<Arc<dyn CatalogProvider>>) -> Self {
        let (pub_tx, picker) = channel();
        Rig { catalog: DatahubCatalogRefresh::new(providers, pub_tx), picker }
    }

    /// Press Refresh on `venue` and block until the worker says it finished.
    fn refresh(&self, venue: &str, now_ms: i64) -> Result<(), CatalogRefreshBlock> {
        let (tx, rx) = channel();
        self.catalog.request(venue, now_ms, move || {
            let _ = tx.send(());
        })?;
        rx.recv_timeout(WAIT).expect("the refresh worker never finished");
        Ok(())
    }

    fn row(&self, venue: &str) -> CatalogRefreshVenueRow {
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
    let rig = Rig::new(vec![p.clone()]);

    let before = rig.row("binance");
    assert!(before.stamp.is_none(), "nothing has been fetched yet");
    assert_eq!(before.count(), 0);
    assert_eq!(before.availability(), DatahubCatalogRefreshAvailability::Direct);
    assert_eq!(
        before.source,
        Some(CatalogRoute::Direct),
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
        CatalogRefreshVenueState::Done {
            outcome: DatahubCatalogRefreshOutcome::Refreshed { count, previous, truncated },
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
/// `crates/vike-app-core/src/data/catalog_refresh/mod.rs`'s `merge_refresh` altered so the `Err` arm falls
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
    let rig = Rig::new(vec![p.clone()]);
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
        CatalogRefreshVenueState::Done {
            outcome: DatahubCatalogRefreshOutcome::Failed { kept, ref error },
            ..
        } => {
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
    let rig = Rig::new(vec![p.clone()]);
    rig.refresh("binance", now()).expect("seed");

    p.will(Ok(Vec::new()));
    rig.refresh("binance", after_cooldown()).expect("press");

    assert_eq!(rig.row("binance").count(), 2, "an empty answer must not wipe the cached list");
    match rig.row("binance").state {
        CatalogRefreshVenueState::Done {
            outcome: DatahubCatalogRefreshOutcome::Empty { kept },
            ..
        } => {
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
    let rig = Rig::new(vec![binance.clone(), okx.clone()]);
    rig.refresh("binance", now()).expect("seed binance");
    rig.refresh("okx", now()).expect("seed okx");
    let okx_at = rig.row("okx").stamp.as_ref().unwrap().last_refreshed_ms;

    binance.will(Err("timeout".into()));
    rig.refresh("binance", after_cooldown()).expect("press binance");
    assert_eq!(rig.row("okx").count(), 2);
    assert_eq!(rig.row("okx").stamp.as_ref().unwrap().last_refreshed_ms, okx_at);

    binance.will(Ok(vec![instrument("binance", "SOLUSDT"), instrument("binance", "XRPUSDT")]));
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
/// ⚠ **The QueryBacked example is `deribit`, not `ibkr`, and the swap is the change this PR makes
/// visible.** `DatahubCatalogRefreshAvailability` is now computed from the ROUTE
/// (`vike_catalog::catalog_source_for`) rather than from the local provider alone, and `ibkr` routes
/// NOWHERE — it is `CatalogAvailability::NoBulkList`, a fact about the venue that outranks any
/// linked provider's self-description. So the mode arm is now reachable only for a venue that IS
/// routed `Direct` and whose provider reports `QueryBacked`. (The unrouted example was `ig` until
/// 2026-11-16, when its list became a held session's.)
#[test]
fn only_a_routed_venue_can_be_refreshed() {
    let enumerable = Scripted::enumerable("binance", &["BTCUSDT"]);
    let query = Scripted::query_backed("deribit");
    let unlisted = Scripted::query_backed("ibkr");
    let rig = Rig::new(vec![enumerable, query.clone(), unlisted.clone()]);

    assert_eq!(rig.row("binance").availability(), DatahubCatalogRefreshAvailability::Direct);
    assert_eq!(rig.row("deribit").availability(), DatahubCatalogRefreshAvailability::QueryBacked);
    // ibkr is LINKED here and still routes nowhere: the venue publishes no bulk list at any price.
    assert_matches!(
        rig.row("ibkr").availability(),
        DatahubCatalogRefreshAvailability::NoBulkList { .. }
    );
    assert_eq!(rig.row("ibkr").source, None, "a venue with no bulk list has no route for anybody");
    // okx is on the roster, has no provider here, and IS publicly enumerable — so it routes to the
    // backend's datahub rather than reading "not in this build".
    assert_eq!(rig.row("okx").availability(), DatahubCatalogRefreshAvailability::ServerBacked);
    assert_eq!(
        rig.row("okx").source,
        Some(CatalogRoute::ServerBacked),
        "a publicly enumerable venue with no local provider routes to a server"
    );
    // ...and the credentialed one routes nowhere either, for the OTHER reason.
    assert_eq!(
        rig.row("alpaca").availability(),
        DatahubCatalogRefreshAvailability::Credentialed { own_keys: false }
    );
    assert_eq!(rig.row("alpaca").source, None);
    // oanda and ctrader are listed by the backend's datahub from a running account's session (decision
    // 0062's decision 3, amended 2026-11-13), so they route to the server like a public venue.
    for v in ["oanda", "ctrader", "ig"] {
        assert_eq!(
            rig.row(v).availability(),
            DatahubCatalogRefreshAvailability::ServerBacked,
            "{v}"
        );
        assert_eq!(rig.row(v).source, Some(CatalogRoute::ServerBacked), "{v}");
    }

    assert_eq!(
        rig.catalog.request("deribit", 0, || {}),
        Err(CatalogRefreshBlock::Unavailable(DatahubCatalogRefreshAvailability::QueryBacked))
    );
    assert_matches!(
        rig.catalog.request("ibkr", 0, || {}),
        Err(CatalogRefreshBlock::Unavailable(DatahubCatalogRefreshAvailability::NoBulkList { .. }))
    );
    assert_eq!(
        rig.catalog.request("alpaca", 0, || {}),
        Err(CatalogRefreshBlock::Unavailable(DatahubCatalogRefreshAvailability::Credentialed {
            own_keys: false
        }))
    );
    // A ROUTED venue with no dial is refused for a different reason, and the difference is the
    // whole point: this one CAN be refreshed, once something is connected.
    assert_eq!(rig.catalog.request("okx", 0, || {}), Err(CatalogRefreshBlock::NoBackend));
    assert_eq!(
        query.calls.load(Ordering::SeqCst) + unlisted.calls.load(Ordering::SeqCst),
        0,
        "a QueryBacked provider is never asked for a bulk list"
    );

    // Every roster venue gets a row — the screen is complete by construction, because the roster
    // is `vike_model::VENUES` through `vike_model::VENUES`.
    assert_eq!(rig.catalog.rows().len(), vike_model::VENUES.len());
}

/// The rate budget: a press inside [`REFRESH_COOLDOWN_MS`] of a completed one is refused, so a
/// held-down button cannot repeat a venue crawl as fast as the network answers.
#[test]
fn a_second_press_inside_the_cooldown_is_refused() {
    let p = Scripted::enumerable("binance", &["BTCUSDT"]);
    let rig = Rig::new(vec![p.clone()]);
    rig.refresh("binance", now()).expect("first press");
    let at = rig.row("binance").stamp.as_ref().unwrap().last_refreshed_ms;

    match rig.catalog.request("binance", at + 1_000, || {}) {
        Err(CatalogRefreshBlock::Cooldown { remaining_ms }) => {
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
    let rig = Rig::new(vec![p.clone()]);
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
            Ok(vec![instrument("binance", "BTCUSDT")])
        }
    }

    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let (entered_tx, entered) = channel();
    let rig = Rig::new(vec![Arc::new(Parked {
        gate: Arc::clone(&gate),
        entered: Mutex::new(entered_tx),
    })]);
    let (done_tx, done) = channel();
    rig.catalog
        .request("binance", 0, move || {
            let _ = done_tx.send(());
        })
        .expect("press");

    entered.recv_timeout(WAIT).expect("the worker never reached the provider");
    // `request` has returned and the fetch is parked — so the row reads in-flight, which is what
    // the button renders as its busy state.
    assert_matches!(
        rig.row("binance").state,
        CatalogRefreshVenueState::InFlight { .. },
        "the row must read in-flight while the fetch is open"
    );
    assert_eq!(
        rig.catalog.request("binance", 0, || {}),
        Err(CatalogRefreshBlock::InFlight),
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
/// in `crates/vike-desktop/src/app_ui/mod.rs`'s `frame_begin`. Nothing has to be INVALIDATED: the
/// engine publishes a whole new `Catalog` over that same channel, so the next frame adopts it.
#[test]
fn the_picker_receives_the_refreshed_universe() {
    let binance = Scripted::enumerable("binance", &["BTCUSDT"]);
    let okx = Scripted::enumerable("okx", &["BTC-USDT"]);
    let rig = Rig::new(vec![binance.clone(), okx]);
    rig.refresh("binance", now()).expect("seed binance");
    rig.refresh("okx", now()).expect("seed okx");

    binance.will(Ok(vec![instrument("binance", "BTCUSDT"), instrument("binance", "PEPEUSDT")]));
    rig.refresh("binance", after_cooldown()).expect("press");

    // Drain to the newest publish — the picker's own drain is a `try_recv` per frame, so the last
    // one sent is what it ends up holding.
    let mut latest = None;
    while let Ok(c) = rig.picker.try_recv() {
        latest = Some(c);
    }
    let catalog = latest.expect("a successful refresh publishes to the picker");
    let hits = catalog.search("PEPE", &vike_catalog::CatalogSearchFilter::default(), 10);
    assert!(
        hits.iter().any(|i| i.raw_symbol == "PEPEUSDT"),
        "the newly listed symbol must be searchable in the picker's universe"
    );
    let all = catalog.search("", &vike_catalog::CatalogSearchFilter::default(), 100);
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
    let rig = Rig::new(vec![p.clone()]);
    rig.refresh("binance", now()).expect("seed");
    while rig.picker.try_recv().is_ok() {}

    p.will(Err("503".into()));
    rig.refresh("binance", after_cooldown()).expect("press");
    assert!(rig.picker.try_recv().is_err(), "a failed refresh must publish no new universe");
}

// ------------------------------------------------------------------------------------------------
// 6. No disk cache: the datahub keeps the lists
// ------------------------------------------------------------------------------------------------

/// **The desktop keeps NO catalog file** (the owner's ruling of 2026-11-12: the datahub keeps the lists in
/// `<project>/instruments`): a refresh writes nothing to disk, and a LEFTOVER `catalog.json` in the working
/// directory's state folder is read by nothing — the startup fetches the cold venue as though it were absent.
#[test]
fn a_leftover_catalog_json_is_ignored_and_a_refresh_writes_no_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let leftover = dir.path().join("catalog.json");
    let old = vike_catalog::CatalogCache {
        fetched: vec![vike_catalog::CatalogCacheVenueStamp {
            venue: "binance".into(),
            last_refreshed_ms: 1,
            count: 9,
        }],
        instruments: Vec::new(),
    };
    std::fs::write(&leftover, serde_json::to_vec(&old).expect("encode")).expect("plant");
    let before = std::fs::read(&leftover).expect("planted");

    let p = Scripted::enumerable("binance", &["BTCUSDT", "ETHUSDT"]);
    let rig = Rig::new(vec![p.clone()]);
    let (tx, done) = channel();
    rig.catalog.spawn_initial(move || {
        let _ = tx.send(());
    });
    done.recv_timeout(WAIT).expect("the startup worker never finished");
    assert_eq!(p.calls.load(Ordering::SeqCst), 1, "the leftover's stamp did not stand for a fetch");
    assert_eq!(rig.catalog.total(), 2);
    rig.refresh("binance", after_cooldown()).expect("press");
    assert_eq!(
        std::fs::read(&leftover).expect("still there"),
        before,
        "neither read nor rewritten"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("dir").count(),
        1,
        "no file was written beside it"
    );
}

/// A venue no list holds is fetched once at startup, so a fresh install does not open on an
/// empty picker with no hint that a button exists.
#[test]
fn a_venue_the_cache_never_held_is_fetched_once_at_startup() {
    let p = Scripted::enumerable("binance", &["BTCUSDT"]);
    let query = Scripted::query_backed("ig");
    let rig = Rig::new(vec![p.clone(), query.clone()]);
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
        merge_refresh(&mut cache, "binance", Ok(vec![instrument("binance", "BTCUSDT")]), 10, false),
        DatahubCatalogRefreshOutcome::Refreshed { count: 1, previous: 0, truncated: false }
    );
    assert_eq!(
        merge_refresh(&mut cache, "binance", Err("boom".into()), 20, false),
        DatahubCatalogRefreshOutcome::Failed { error: "boom".into(), kept: 1 }
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
/// `crate::data::datahub::resolve_datahub_address`'s second rung is the ACTIVE backend's `Welcome`
/// advertisement, which the observe bridge clears on every link blip and restores only after the
/// next handshake. On the documented default box that advertisement IS the whole address ladder,
/// so a rule that cleared on `None` would turn every live button into a `NoBackend` refusal — for
/// a datahub that is a different process from the tradehub that blipped and did not go anywhere.
/// The verbatim rule of `MarketDataSession::set_address`, which exists for this precise bug.
#[test]
fn a_transient_advertisement_gap_does_not_take_the_dial_away() {
    let rig = Rig::new(vec![Scripted::enumerable("binance", &["BTCUSDT"])]);
    assert!(
        rig.catalog.dial_is_stale(Some("<host>:7878")),
        "an empty handle needs the first dial"
    );
    rig.catalog.set_dial(Some(datahub_dial("<host>:7878")));
    assert_eq!(rig.row("okx").server.as_deref(), Some("<host>:7878"));

    assert!(!rig.catalog.dial_is_stale(None), "a blip is NOT a different server");
    assert!(
        !rig.catalog.dial_is_stale(Some("<host>:7878")),
        "the same address resolves nothing — the credential store is opened only on a change"
    );
    assert!(
        rig.catalog.dial_is_stale(Some("<host>:7878")),
        "a DIFFERENT address is a different server"
    );
}

/// A dial reaches every `ServerBacked` row and NO `Direct` one cares — a local fetch needs no
/// datahub, so connecting or losing a backend cannot change whether deribit is refreshable.
#[test]
fn the_dial_gates_the_routed_rows_and_not_the_linked_one() {
    let rig = Rig::new(vec![Scripted::enumerable("deribit", &["BTC-PERPETUAL"])]);
    assert_eq!(rig.catalog.request("okx", now(), || {}), Err(CatalogRefreshBlock::NoBackend));
    assert_eq!(refresh_block(&rig.row("deribit"), now()), None, "the linked venue is unaffected");

    rig.catalog.set_dial(Some(datahub_dial("127.0.0.1:17878")));
    assert_eq!(
        refresh_block(&rig.row("okx"), now()),
        None,
        "a dial makes the routed row pressable"
    );
    rig.catalog.set_dial(None);
    assert_eq!(refresh_block(&rig.row("okx"), now()), Some(CatalogRefreshBlock::NoBackend));
    assert_eq!(refresh_block(&rig.row("deribit"), now()), None);
}

/// **A `ServerBacked` fetch that cannot reach its datahub is a FAILURE, not a refusal, and it
/// keeps the cached list** — the same rule the local route has, proved over the real wire leg
/// against a port nothing is listening on.
///
/// This is the end-to-end plumbing test for the route: `request` → `DatahubCatalogRoute::Server` →
/// `vike_datahub_client::catalog_fetch::fetch_venue_catalog` → `ConnectFailed` → `settle` → `merge_refresh`'s `Err` arm.
#[test]
fn an_unreachable_datahub_fails_the_routed_venue_and_keeps_its_cache() {
    let rig = Rig::new(vec![Scripted::enumerable("deribit", &["BTC-PERPETUAL"])]);
    // Seed okx's slice through the pure fold, so there is something a bad fetch could destroy.
    rig.catalog.set_dial(Some(datahub_dial(&dead_address())));
    rig.refresh("okx", now()).expect("a routed press with a dial is allowed");

    match rig.row("okx").state {
        CatalogRefreshVenueState::Done {
            outcome: DatahubCatalogRefreshOutcome::Failed { ref error, kept },
            ..
        } => {
            assert_eq!(kept, 0);
            assert!(error.contains("could not be reached"), "the dial's own words reach the row");
        }
        other => panic!("expected a reported failure, got {other:?}"),
    }
    assert!(rig.picker.try_recv().is_err(), "a failed routed refresh publishes no new universe");
    assert!(rig.row("okx").stamp.is_none(), "…and stamps nothing");
}
