//! **The `ServerBacked` refresh route, driven end to end over a real loopback datahub double.**
//!
//! `crates/vike-app-core/tests/catalog_refresh.rs` gates the ROUTING decision, the budget and the
//! cache rule; this file gates what a press actually does once something answers. The double is a
//! `std::net::TcpListener` speaking the real frame codec
//! (`vike_datahub_client::proto::{read_frame, write_frame}`) — not a mock of the client — so a
//! change to the handshake or to `Response::VenueCatalog`'s shape reddens here rather than being
//! agreed with by a fake.
//!
//! Five properties, each with the defect it exists for:
//!
//! 1. **A listing reaches the picker's universe.** The whole point of the button for the eight
//!    venues this binary links no bridge for.
//! 2. **⚠ Every refusal is a REFUSAL, never a failure and never an empty list.** An unarmed lane, a
//!    build with no provider, a credentialed venue and a venue with no bulk list are four different
//!    facts about the world, all arriving as a SUCCESSFUL response. Folding any into
//!    `RefreshOutcome::Empty` renders *"the venue answered with nothing"* — the lie
//!    `docs/decisions/0062`'s decision 5 exists to prevent, and a lie for five roster venues.
//! 3. **A server that does not advertise the capability is refused CLIENT-side, with ZERO request
//!    frames sent.** Proved by counting the frames the double received.
//! 4. **A truncated listing is adopted AND says it is short.** A picker silently missing a venue's
//!    tail is a bug report nobody can reproduce.
//! 5. **No refusal ever replaces a cached list.** The same rule the local route has, over the wire.

use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use vike_app_core::catalog_refresh::{
    CatalogRefresh, RefreshBlock, RefreshOutcome, VenueRefreshState,
};
use vike_app_core::catalog_wire::{CatalogDial, CatalogFetchReport, fetch_venue_catalog};
use vike_catalog::{AssetClass, Catalog, Instrument};
use vike_datahub_client::FEATURE_VENUE_CATALOG;
use vike_datahub_client::catalog::{CatalogListing, CatalogOutcome, CatalogRefusal};
use vike_datahub_client::proto::{PROTO_VERSION, Request, Response, read_frame, write_frame};

/// Generous, for the same reason `catalog_refresh.rs`'s is: the box this must be non-flaky on is a
/// loaded the CI box runner, and the work itself is a `Vec` over loopback.
const WAIT: Duration = Duration::from_secs(10);

fn inst(venue: &str, sym: &str) -> Instrument {
    Instrument {
        venue: venue.into(),
        raw_symbol: sym.into(),
        asset_class: AssetClass::CryptoSpot,
        base: "BTC".into(),
        quote: "USDT".into(),
        description: String::new(),
        properties: Default::default(),
        contract_type: None,
        settle_asset: None,
    }
}

/// **A KEY-LESS datahub, for as many connections as the test makes.**
///
/// Key-less on purpose: `DatahubClient::connect` is a successful unauthenticated connect against a
/// server advertising no `auth` feature, which is the documented loopback-dev-server shape. The
/// AUTH leg is `crates/vike-datahub/tests/auth_roundtrip.rs`'s business and is not re-proved here.
struct FakeHub {
    addr: String,
    /// Every `Request` frame the double received after the handshake — the evidence behind
    /// property 3, which is a claim about frames NOT sent.
    verb_frames: Arc<AtomicUsize>,
    _thread: std::thread::JoinHandle<()>,
}

impl FakeHub {
    /// `features` is what the `Welcome` advertises; `answer` is the outcome every
    /// `Request::VenueCatalog` gets, echoed back under the venue the client asked for.
    fn spawn(features: Vec<String>, answer: CatalogOutcome) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr").to_string();
        let verb_frames = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&verb_frames);
        let thread = std::thread::Builder::new()
            .name("fake-datahub".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let features = features.clone();
                    let answer = answer.clone();
                    let counter = Arc::clone(&counter);
                    // One connection at a time is enough: the refresh engine opens one per press.
                    serve(stream, features, answer, &counter);
                }
            })
            .expect("spawn the fake datahub");
        Self { addr, verb_frames, _thread: thread }
    }

    fn dial(&self) -> CatalogDial {
        CatalogDial {
            addr: self.addr.clone(),
            key_name: "VIKE_DATAHUB_OBSERVE_KEY".to_string(),
            keys: None,
        }
    }

    fn verbs_received(&self) -> usize {
        self.verb_frames.load(Ordering::SeqCst)
    }
}

/// ⚠ No buffering wrapper on either side: `write_frame` writes the length prefix, the body and then
/// FLUSHES, so a `BufWriter` here would only add a second place a missing flush could hang the
/// client on a read that never arrives.
fn serve(stream: TcpStream, features: Vec<String>, answer: CatalogOutcome, counter: &AtomicUsize) {
    let mut r = stream.try_clone().expect("clone the accepted stream");
    let mut w = stream;
    // The handshake. A key-less `Welcome` carries no nonce, which is the byte-identical-to-pre-auth
    // shape `Response::Welcome::nonce`'s doc describes.
    match read_frame::<_, Request>(&mut r) {
        Ok(Request::Hello { .. }) => {}
        _ => return,
    }
    if write_frame(
        &mut w,
        &Response::Welcome { proto_version: PROTO_VERSION, features, nonce: None },
    )
    .is_err()
    {
        return;
    }
    // ...then answer verbs until the client hangs up.
    while let Ok(req) = read_frame::<_, Request>(&mut r) {
        counter.fetch_add(1, Ordering::SeqCst);
        let resp = match req {
            Request::VenueCatalog { venue } => {
                Response::VenueCatalog(CatalogListing { venue, outcome: answer.clone() })
            }
            other => Response::Error(format!("unexpected verb: {other:?}")),
        };
        if write_frame(&mut w, &resp).is_err() {
            return;
        }
    }
}

/// The real engine, with NO local provider at all — so every venue it can refresh is routed.
struct Rig {
    catalog: CatalogRefresh,
    picker: Receiver<Catalog>,
}

impl Rig {
    fn new(dial: CatalogDial) -> Self {
        let (tx, picker) = channel();
        let catalog = CatalogRefresh::new(Vec::new(), None, None, None, tx);
        catalog.set_dial(Some(dial));
        Self { catalog, picker }
    }

    fn refresh(&self, venue: &str) -> Result<(), RefreshBlock> {
        let (tx, rx) = channel();
        self.catalog.request(venue, vike_model::now_ms(), move || {
            let _ = tx.send(());
        })?;
        rx.recv_timeout(WAIT).expect("the refresh worker never finished");
        Ok(())
    }

    fn outcome(&self, venue: &str) -> RefreshOutcome {
        match self.catalog.rows().into_iter().find(|r| r.venue == venue).expect("row").state {
            VenueRefreshState::Done { outcome, .. } => outcome,
            other => panic!("expected a completed attempt, got {other:?}"),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// 1. A listing reaches the picker
// ------------------------------------------------------------------------------------------------

/// **The whole point of the route**: a venue this binary links no bridge for is listed by the
/// backend's datahub and its symbols become searchable in the picker's universe.
#[test]
fn a_routed_venue_is_listed_by_the_server_and_reaches_the_picker() {
    let hub = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string()],
        CatalogOutcome::Listed {
            instruments: vec![inst("binance", "BTCUSDT"), inst("binance", "PEPEUSDT")],
            truncated: false,
            cached: false,
        },
    );
    let rig = Rig::new(hub.dial());

    rig.refresh("binance").expect("a routed press with a dial is allowed");

    assert_eq!(
        rig.outcome("binance"),
        RefreshOutcome::Refreshed { count: 2, previous: 0, truncated: false }
    );
    assert_eq!(rig.catalog.total(), 2, "the merged universe holds them");
    let mut latest = None;
    while let Ok(c) = rig.picker.try_recv() {
        latest = Some(c);
    }
    let catalog = latest.expect("a successful routed refresh publishes to the picker");
    let hits = catalog.search("PEPE", &vike_catalog::SearchFilter::default(), 10);
    assert!(
        hits.iter().any(|i| i.raw_symbol == "PEPEUSDT"),
        "the server's listing must be searchable in the picker's universe"
    );
    assert_eq!(hub.verbs_received(), 1, "exactly one verb frame, for one press");
}

/// A listing the server answered from its own TTL memo is adopted identically — `cached` is
/// reported, not acted on. It is the difference between "seconds old" and "up to the TTL old",
/// which is the only thing an operator staring at a stale symbol wants to know.
#[test]
fn a_cached_listing_is_adopted_like_a_fresh_one() {
    let hub = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string()],
        CatalogOutcome::Listed {
            instruments: vec![inst("okx", "BTC-USDT")],
            truncated: false,
            cached: true,
        },
    );
    let rig = Rig::new(hub.dial());
    rig.refresh("okx").expect("press");
    assert!(rig.outcome("okx").adopted());
    assert_eq!(rig.catalog.total(), 1);
}

// ------------------------------------------------------------------------------------------------
// 2. Every refusal is a refusal
// ------------------------------------------------------------------------------------------------

/// **⚠ The four SUCCESSFUL non-listing answers each land as [`RefreshOutcome::Refused`] carrying
/// the SERVER's own sentence — never as `Failed`, never as `Empty`, and never as a count.**
///
/// `Empty` renders *"the venue answered with nothing — kept the N already cached"*, which for an
/// unarmed lane or a credentialed venue is false about the venue; `Failed` calls a venue property
/// an error and invites a retry that cannot change its answer. Decision 5 of 0062 is exactly this
/// distinction, and it is held here in the value rather than in the rendering.
#[test]
fn every_server_refusal_lands_as_a_refusal_with_the_servers_own_words() {
    let cases: Vec<(&str, CatalogOutcome, &str)> = vec![
        ("binance", CatalogOutcome::NotArmed, "venue_catalog_off"),
        (
            "dukascopy",
            CatalogOutcome::Refused(CatalogRefusal::NotServed {
                supported: vec!["binance".into(), "okx".into()],
            }),
            "binance, okx",
        ),
        (
            "alpaca",
            CatalogOutcome::Refused(CatalogRefusal::NeedsCredentials),
            "operator's own venue credentials",
        ),
        (
            "ig",
            CatalogOutcome::Refused(CatalogRefusal::NoBulkList {
                why: "IG publishes no bulk market list".into(),
            }),
            "publishes no bulk instrument list",
        ),
    ];
    for (venue, outcome, needle) in cases {
        let hub = FakeHub::spawn(vec![FEATURE_VENUE_CATALOG.to_string()], outcome);
        // Two of the four venues above are ones the routing table sends NOWHERE, so the wire leg is
        // driven directly here rather than through a press — a server answering them at all is the
        // belt this test holds, and the routing gate that stops the desktop asking is
        // `crates/vike-app-core/tests/catalog_refresh.rs`'s business.
        let report = fetch_venue_catalog(Some(&hub.dial()), venue);
        let why = match &report {
            CatalogFetchReport::Refused { why } => why.clone(),
            other => panic!("{venue}: expected a refusal, got {other:?}"),
        };
        assert!(why.contains(needle), "{venue}: the server's own words: {why}");
        assert!(!why.contains("0 instruments"), "{venue}: a refusal is not a count: {why}");
    }
}

/// …and the ENGINE folds one the same way: a refused routed press writes `Refused`, touches no
/// instrument, publishes nothing, and stamps nothing.
#[test]
fn a_refusal_never_replaces_a_cached_list_and_publishes_nothing() {
    // First: seed okx's slice from a server that answers.
    let good = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string()],
        CatalogOutcome::Listed {
            instruments: vec![inst("okx", "BTC-USDT"), inst("okx", "ETH-USDT")],
            truncated: false,
            cached: false,
        },
    );
    let rig = Rig::new(good.dial());
    rig.refresh("okx").expect("seed");
    let seeded_at = rig
        .catalog
        .rows()
        .into_iter()
        .find(|r| r.venue == "okx")
        .and_then(|r| r.stamp)
        .expect("a stamp")
        .last_refreshed_ms;
    while rig.picker.try_recv().is_ok() {}

    // Then re-point at a server whose lane is UNARMED and press again past the cooldown.
    let unarmed = FakeHub::spawn(vec![FEATURE_VENUE_CATALOG.to_string()], CatalogOutcome::NotArmed);
    rig.catalog.set_dial(Some(unarmed.dial()));
    let (tx, rx) = channel();
    rig.catalog
        .request(
            "okx",
            vike_model::now_ms() + vike_app_core::catalog_refresh::REFRESH_COOLDOWN_MS + 5_000,
            move || {
                let _ = tx.send(());
            },
        )
        .expect("the second press is allowed");
    rx.recv_timeout(WAIT).expect("the refresh worker never finished");

    match rig.outcome("okx") {
        RefreshOutcome::Refused { why, kept } => {
            assert_eq!(kept, 2, "the refusal names the list it left alone");
            assert!(why.contains("venue_catalog_off"), "{why}");
        }
        other => panic!("an unarmed lane must be a REFUSAL, got {other:?}"),
    }
    assert_eq!(rig.catalog.total(), 2, "the cached list survives a refusal");
    assert_eq!(
        rig.catalog
            .rows()
            .into_iter()
            .find(|r| r.venue == "okx")
            .and_then(|r| r.stamp)
            .expect("the stamp survives")
            .last_refreshed_ms,
        seeded_at,
        "the stamp must NOT move — a moved stamp claims a refresh that did not happen"
    );
    assert!(rig.picker.try_recv().is_err(), "a refusal publishes no new universe");
}

// ------------------------------------------------------------------------------------------------
// 3. An old or unarmed server is refused CLIENT-side, with zero frames sent
// ------------------------------------------------------------------------------------------------

/// **A server that does not advertise [`FEATURE_VENUE_CATALOG`] gets NO request frame**, and the
/// operator gets the sentence naming the switch and the build.
///
/// The frame count is the assertion, not the sentence: a client that sent the verb and translated
/// the server's error would produce a plausible-looking line while still spending a round trip on
/// every venue the operator pressed.
#[test]
fn an_unadvertising_server_is_refused_locally_and_is_sent_nothing() {
    let hub = FakeHub::spawn(
        vec!["load_bars".to_string(), "seed_series".to_string()],
        CatalogOutcome::NotArmed,
    );
    let report = fetch_venue_catalog(Some(&hub.dial()), "binance");
    match &report {
        CatalogFetchReport::ServerUnsupported { addr, features } => {
            assert_eq!(addr, &hub.addr);
            assert_eq!(features, &["load_bars".to_string(), "seed_series".to_string()]);
        }
        other => panic!("expected ServerUnsupported, got {other:?}"),
    }
    assert_eq!(hub.verbs_received(), 0, "not one request frame may be written");

    let line = vike_app_core::catalog_wire::render_catalog_fetch_status(&report);
    // ⚠ The named switch is the REFUSAL since `docs/decisions/0066`, and the `catalog-serve`
    // clause is GONE rather than renamed: a build without that feature now serves an empty table
    // and DOES advertise, so a missing advertisement can no longer be evidence of it.
    assert!(line.contains("venue_catalog_off"), "{line}");
    assert!(line.contains("ON by default"), "{line}");
    assert!(!line.contains("0 instruments"), "an old server is not an empty venue: {line}");
}

// ------------------------------------------------------------------------------------------------
// 4. Truncation is visible
// ------------------------------------------------------------------------------------------------

/// **A truncated listing is adopted — it is real data — and the row SAYS it is short.** The server
/// sets the flag when a listing hits `CATALOG_MAX_INSTRUMENTS`; dropping it here would leave the
/// picker quietly missing a venue's tail, which is a bug nobody can reproduce.
#[test]
fn a_truncated_listing_is_adopted_and_the_row_says_it_is_short() {
    let hub = FakeHub::spawn(
        vec![FEATURE_VENUE_CATALOG.to_string()],
        CatalogOutcome::Listed {
            instruments: vec![inst("polymarket", "A"), inst("polymarket", "B")],
            truncated: true,
            cached: false,
        },
    );
    let rig = Rig::new(hub.dial());
    rig.refresh("polymarket").expect("press");

    let outcome = rig.outcome("polymarket");
    assert_eq!(outcome, RefreshOutcome::Refreshed { count: 2, previous: 0, truncated: true });
    assert!(outcome.adopted(), "a short list is still a list");
    assert!(outcome.line().contains("TRUNCATED"), "{}", outcome.line());
    assert_eq!(rig.catalog.total(), 2);
}
