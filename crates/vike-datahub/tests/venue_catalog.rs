//! The COMPOSED-PATH gate for the VENUE-CATALOG verb
//! (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`): a server on
//! an ephemeral loopback port, driven through `DatahubClient::venue_catalog`, with a FAKE provider
//! table standing where `real_catalog_table` goes.
//!
//! # ⚠ It needs NO feature and NO DataFusion, and that is a property of the verb
//!
//! Its sibling `seed_series.rs` opens with `#![cfg(feature = "backfill-serve")]` and a real
//! `DataFusionHist` over a temp dir, because that verb WRITES the store and the write must be read
//! back to prove anything. This one writes nothing (0062's decision 1), so it runs over the
//! in-memory `MemHistStore` double on a DEFAULT build — which means it executes in the derived
//! roster lane on every PR, rather than only in a feature lane. The asymmetry in these two headers
//! is the clearest evidence that the two verbs are different classes.
//!
//! # What each test holds
//!
//! 1. [`a_refused_lane_succeeds_and_fetches_nothing`] — the REFUSAL, 0066 decision 3 (it was
//!    0062 decision 4's opt-in until the default flipped, and the two tests it names were called
//!    `an_unarmed_*` then);
//! 2. [`the_lane_is_built_by_default_and_only_the_written_refusal_stops_it`] — the FLIP itself,
//!    over the gate the daemon calls;
//! 3. [`a_listing_crosses_the_wire_and_the_client_adopts_it`] — the verb actually works;
//! 4. [`a_venue_with_no_bulk_list_is_refused_distinctly_from_one_that_listed_nothing`] — decision 5,
//!    driven end to end rather than asserted on the DTO;
//! 5. [`a_credentialed_venue_is_refused_and_no_provider_is_called`] — decision 3, observed at the
//!    PROVIDER rather than read off the refusal text;
//! 6. [`the_servers_bounds_hold_whatever_the_client_asks`] — the cost rule: a client that asks a
//!    two hundred times, across two connections, causes ONE fetch per venue and no more;
//! 7. [`an_old_server_makes_the_client_say_so_rather_than_show_an_empty_list`] — the capability
//!    negotiation, which is the leg that stops an unadvertised verb rendering as "no instruments".

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use vike_catalog::{AssetClass, Instrument};
use vike_data::{HistStore, MemHistStore};
use vike_datahub::catalog::{CatalogFn, CatalogLane, CatalogTable};
use vike_datahub::serve_authed;
use vike_datahub_client::catalog::{CatalogOutcome, CatalogRefusal};
use vike_datahub_client::{DatahubClient, FEATURE_VENUE_CATALOG};

/// Which venues each provider was asked for, and how many times — the evidence tests 4 and 5 turn
/// on, because "the venue was never called" is not something a refusal STRING can prove.
#[derive(Default)]
struct Spy {
    calls: Mutex<Vec<String>>,
    count: AtomicUsize,
}

impl Spy {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
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
        // ⚠ `None` is the ORDINARY state for both — see `vike_catalog::Instrument::contract_type`.
        contract_type: None,
        settle_asset: None,
    }
}

/// One spying provider entry yielding `n` instruments.
fn spying(venue: &str, n: usize, spy: Arc<Spy>) -> (String, CatalogFn) {
    let v = venue.to_string();
    let vc = v.clone();
    (
        v,
        Box::new(move || {
            spy.count.fetch_add(1, Ordering::SeqCst);
            spy.calls.lock().unwrap().push(vc.clone());
            Ok((0..n).map(|i| inst(&vc, &format!("SYM{i}USDT"))).collect())
        }),
    )
}

/// Serve with the catalog lane ARMED or not.
fn spawn(lane: Option<Arc<CatalogLane>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve_authed(listener, store, None, None, None, None, lane);
    });
    addr
}

/// An armed server whose table serves `binance` (3 instruments) and `okx` (0 — a LEGITIMATELY
/// empty listing, which test 3 needs in order to have something to contrast a refusal with).
fn armed() -> (Arc<Spy>, SocketAddr) {
    let spy = Arc::new(Spy::default());
    let table = CatalogTable::new(vec![
        spying("binance", 3, Arc::clone(&spy)),
        spying("okx", 0, Arc::clone(&spy)),
    ]);
    let addr = spawn(Some(Arc::new(CatalogLane::new(table))));
    (spy, addr)
}

fn client(addr: SocketAddr) -> DatahubClient {
    DatahubClient::connect(addr).expect("connect to the test server")
}

// -------------------------------------------------------------------------------------------
// 1. The opt-in
// -------------------------------------------------------------------------------------------

/// **With the operator's REFUSAL written, the verb SUCCEEDS and fetches nothing**, and the
/// capability is not advertised.
///
/// ⚠ This was "with the operator opt-in OFF" and cited 0062's decision 4;
/// `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`
/// flipped the default, so a server in this state has `venue_catalog_off = true` written rather
/// than an arming absent. The PROPERTY is unchanged and is the reason the test survives the flip:
/// the answer must be a SUCCESS rather than an error, because a lane-less server and a venue with
/// no instruments are otherwise indistinguishable from the picker's side — the same confusion
/// decision 5 exists to prevent one layer down.
#[test]
fn a_refused_lane_succeeds_and_fetches_nothing() {
    let addr = spawn(None);
    let mut c = client(addr);
    assert!(!c.serves_venue_catalog(), "an unarmed server must not advertise the capability");
    // The client refuses LOCALLY on the advertisement, naming the switch — an operator learns what
    // to set without a round trip.
    let err = c.venue_catalog("binance").expect_err("an unadvertised capability refuses locally");
    assert!(err.contains("venue_catalog_off"), "the switch is named: {err}");
    assert!(err.contains("nothing was sent"), "{err}");
}

/// ...and the SERVER's own half of the same property, reached past the client's local guard.
///
/// The client above never sends a frame, so on its own it proves nothing about the server. This
/// drives `Request::VenueCatalog` at an unarmed server directly and requires the positional answer
/// to be a successful `NotArmed` — if the server ever answered `Response::Error` here, the verb
/// would mean "do this" rather than "this is the venue I am looking at", and the scope argument
/// would need remaking on weaker ground.
#[test]
fn a_refused_server_answers_not_armed_positionally() {
    use vike_datahub_client::proto::{Request, Response, read_frame, write_frame};
    let addr = spawn(None);
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::VenueCatalog { venue: "binance".into() }).expect("write");
    match read_frame::<_, Response>(&mut s).expect("read") {
        Response::VenueCatalog(listing) => {
            assert_eq!(listing.outcome, CatalogOutcome::NotArmed);
            assert_eq!(listing.venue, "binance", "the venue is echoed verbatim");
            assert!(listing.describe().contains("venue_catalog_off"));
        }
        other => panic!("an unarmed server must answer NotArmed, got {other:?}"),
    }
}

/// **THE FLIP ITSELF**, over the gate `crates/vike-datahub/src/datahub_cli.rs` calls — a server
/// whose operator wrote nothing SERVES.
///
/// ⚠ This test exists because the two above cannot see the flip at all: both drive `spawn(None)`,
/// which is a server built WITHOUT a lane, and that is the same value whether the lane was never
/// armed or was refused. The thing `docs/decisions/0066` changed is which `flags.venue_catalog_off`
/// produces `None`, and the only site that decides it is `vike_catalog::venue_catalog_gate`. Drive
/// that, then build the lane the way the daemon does, and the composed property is real rather
/// than assumed.
///
/// ⚠ WHAT THIS TEST CANNOT SEE, said out loud: the WIRING. Make the daemon ignore the operator by
/// passing a literal `false` where `booted.settings.flags.venue_catalog_off` goes and every
/// assertion here still passes, because this file constructs the gate's inputs itself. The half
/// that catches that mutation is `crates/vike-config/tests/settings_are_consumed.rs`'s
/// `every_claimed_consumer_really_reads_it`, which opens `crates/vike-datahub/src/datahub_cli.rs`
/// and requires the `CONSUMPTION` needle for `flags.venue_catalog_off` to be present in it —
/// i.e. requires the key to reach the gate from the SETTINGS rather than from a constant. The two
/// tests are the pair; neither is the proof alone.
#[test]
fn the_lane_is_built_by_default_and_only_the_written_refusal_stops_it() {
    use vike_catalog::{VenueCatalogGate, venue_catalog_gate};

    let table = || CatalogTable::new(vec![spying("binance", 1, Arc::new(Spy::default()))]);

    // Nothing written: the lane is built, the server advertises, and a client gets a listing.
    let gate = venue_catalog_gate(false, table().supported().len());
    assert_eq!(gate, VenueCatalogGate::Armed);
    let lane = gate.serves().then(|| Arc::new(CatalogLane::new(table())));
    assert!(lane.is_some(), "the default must BUILD a lane");
    let mut c = client(spawn(lane));
    assert!(c.serves_venue_catalog(), "a default server must advertise the capability");
    let listing = c.venue_catalog("binance").expect("an advertised verb answers");
    assert!(
        matches!(listing.outcome, CatalogOutcome::Listed { .. }),
        "a default server LISTS: {:?}",
        listing.outcome
    );

    // The refusal written: no lane, no advertisement, and the SUCCESSFUL non-answer above.
    let refused = venue_catalog_gate(true, table().supported().len());
    assert_eq!(refused, VenueCatalogGate::RefusedByOperator);
    let none = refused.serves().then(|| Arc::new(CatalogLane::new(table())));
    assert!(none.is_none(), "a written refusal must build NO lane");
    let c = client(spawn(none));
    assert!(!c.serves_venue_catalog(), "a refused server must not advertise");
}

// -------------------------------------------------------------------------------------------
// 2. It works
// -------------------------------------------------------------------------------------------

/// **The verb returns a venue's instruments and the client adopts them** — the whole point.
#[test]
fn a_listing_crosses_the_wire_and_the_client_adopts_it() {
    let (spy, addr) = armed();
    let mut c = client(addr);
    assert!(c.serves_venue_catalog(), "an armed server advertises the capability");

    let listing = c.venue_catalog("binance").expect("a served venue lists");
    match &listing.outcome {
        CatalogOutcome::Listed { instruments, truncated, cached } => {
            assert_eq!(instruments.len(), 3);
            assert!(!truncated, "3 instruments is not a truncated listing");
            assert!(!cached, "the first ask is a fresh fetch");
            // ...and they survive the wire as real `vike_catalog::Instrument`s, which is what lets
            // the client fold them straight into a `Catalog` with no second DTO in between.
            assert_eq!(instruments[0].venue, "binance");
            assert_eq!(instruments[0].id(), "SYM0USDT.BINANCE");
        }
        other => panic!("expected a listing, got {other:?}"),
    }
    assert_eq!(spy.calls(), vec!["binance"], "exactly the venue asked for was fetched");

    // The CLIENT-SIDE ADOPTION: the instruments fold into a searchable catalog, which is what the
    // Data Manager's refresh actually does with them.
    let cat = vike_catalog::Catalog::from_instruments(listing.instruments().to_vec());
    assert!(
        !cat.search("SYM1", &vike_catalog::SearchFilter::default(), 10).is_empty(),
        "an adopted listing must be searchable"
    );
}

// -------------------------------------------------------------------------------------------
// 3. The distinction decision 5 exists for
// -------------------------------------------------------------------------------------------

/// **A venue with no bulk list is REFUSED, distinctly from one that returned nothing** — 0062's
/// decision 5, driven end to end.
///
/// `okx` is in the table and legitimately lists zero instruments; `ig` and `ibkr` cannot be
/// enumerated at all. Both produce "no instruments" for a caller that only looks at a vector, and
/// they must not be the same VALUE or the same SENTENCE.
#[test]
fn a_venue_with_no_bulk_list_is_refused_distinctly_from_one_that_listed_nothing() {
    let (_spy, addr) = armed();
    let mut c = client(addr);

    let empty = c.venue_catalog("okx").expect("okx is served");
    assert!(
        matches!(&empty.outcome, CatalogOutcome::Listed { instruments, .. } if instruments.is_empty()),
        "okx must be a LISTING that happens to be empty, got {:?}",
        empty.outcome
    );

    for venue in ["ig", "ibkr"] {
        let refused = c.venue_catalog(venue).expect("a refusal is a successful answer");
        match &refused.outcome {
            CatalogOutcome::Refused(CatalogRefusal::NoBulkList { why }) => {
                assert!(!why.is_empty(), "{venue}'s refusal must carry a reason");
            }
            other => panic!("{venue} must be NoBulkList, got {other:?}"),
        }
        // The two are DIFFERENT VALUES...
        assert_ne!(refused.outcome, empty.outcome, "{venue} must not equal an empty listing");
        // ...and DIFFERENT SENTENCES, which is what an operator actually sees.
        assert!(
            refused.describe().contains("publishes no bulk instrument list"),
            "{venue}: {}",
            refused.describe()
        );
        assert!(
            !refused.describe().contains("0 instruments"),
            "{venue} must not read as an empty venue: {}",
            refused.describe()
        );
    }
    assert!(empty.describe().contains("0 instruments"), "{}", empty.describe());
}

// -------------------------------------------------------------------------------------------
// 4. The credential fence
// -------------------------------------------------------------------------------------------

/// **A credentialed venue is refused and NO provider is called** — 0062's decision 3, observed at
/// the provider rather than read off the refusal text.
///
/// ⚠ The evidence that matters is `spy.count() == 0`. A refusal STRING would be produced by a
/// server that had already spent the operator's credentials and then thought better of it; the call
/// count is what proves nothing was spent.
#[test]
fn a_credentialed_venue_is_refused_and_no_provider_is_called() {
    let (spy, addr) = armed();
    let mut c = client(addr);
    for venue in ["alpaca", "oanda", "ctrader"] {
        let r = c.venue_catalog(venue).expect("a refusal is a successful answer");
        assert_eq!(
            r.outcome,
            CatalogOutcome::Refused(CatalogRefusal::NeedsCredentials),
            "{venue} must be refused as credentialed"
        );
        assert!(
            !r.describe().contains("VIKE_DATAHUB_VENUE_CATALOG"),
            "{venue}: no switch arms this, so the sentence must not suggest one: {}",
            r.describe()
        );
    }
    assert_eq!(spy.count(), 0, "no provider may be called for a credentialed venue");
}

/// ...and a venue this build does not serve names what it DOES serve, so a missing venue reads
/// differently from a misspelled one.
#[test]
fn an_unserved_venue_names_the_supported_set() {
    let (spy, addr) = armed();
    let mut c = client(addr);
    // `dukascopy` is publicly enumerable but is not in this table — a BUILD fact.
    let r = c.venue_catalog("dukascopy").expect("a refusal is a successful answer");
    match &r.outcome {
        CatalogOutcome::Refused(CatalogRefusal::NotServed { supported }) => {
            assert_eq!(supported, &vec!["binance".to_string(), "okx".to_string()]);
        }
        other => panic!("expected NotServed, got {other:?}"),
    }
    // A well-formed slug that is not a venue at all takes the same arm rather than inventing a fact
    // about a venue that does not exist.
    let unknown = c.venue_catalog("kraken").expect("a refusal is a successful answer");
    assert!(matches!(unknown.outcome, CatalogOutcome::Refused(CatalogRefusal::NotServed { .. })));
    // ...and a MALFORMED one is the exceptional case: an Error, refused by the CLIENT before a
    // frame is sent, and never echoing the offending string back.
    let err = c.venue_catalog("BINANCE").expect_err("an upper-case slug is malformed");
    assert!(err.contains("outside the permitted set"), "{err}");
    assert!(!err.contains("BINANCE"), "the refusal must not echo the venue: {err}");
    assert_eq!(spy.count(), 0, "none of these may reach a provider");
}

// -------------------------------------------------------------------------------------------
// 5. The bounds
// -------------------------------------------------------------------------------------------

/// **The server's bounds hold regardless of what the client asks** — the cost rule itself.
///
/// A client that asks a thousand times causes exactly ONE venue fetch (the TTL memo), and a client
/// that defeats the memo by naming fresh venues is stopped by the per-venue bucket with a refusal
/// that NAMES the budget it is protecting.
#[test]
fn the_servers_bounds_hold_whatever_the_client_asks() {
    // ⚠ 200 rather than the 1,000 this started at: each iteration is a real loopback round trip,
    // and the larger number cost 91 s on a loaded the CI box lane for a property that is already proved
    // at the first repeat. The lane's own unit tests drive the memo a thousand times in-process,
    // where it is free.
    const ASKS: usize = 200;
    let (spy, addr) = armed();
    let mut c = client(addr);
    for _ in 0..ASKS {
        let r = c.venue_catalog("binance").expect("served");
        assert!(matches!(r.outcome, CatalogOutcome::Listed { .. }));
    }
    assert_eq!(spy.count(), 1, "{ASKS} asks must cost exactly ONE venue fetch");
    // ...and the answers after the first say so, which is the only thing `cached` is for.
    let r = c.venue_catalog("binance").expect("served");
    assert!(
        matches!(r.outcome, CatalogOutcome::Listed { cached: true, .. }),
        "a memo hit must report itself"
    );

    // ...and the bound survives a NEW CONNECTION, which is what makes it the SERVER's rather than
    // one client's book-keeping. A caller that reconnects to defeat a client-side cache gets the
    // same memo, because the memo lives in the lane the process owns.
    let mut fresh = client(addr);
    for _ in 0..25 {
        assert!(matches!(
            fresh.venue_catalog("binance").expect("served").outcome,
            CatalogOutcome::Listed { cached: true, .. }
        ));
    }
    assert_eq!(spy.count(), 1, "reconnecting must not buy a second fetch");

    // The whole served set, hammered from both connections, still costs one fetch per VENUE — the
    // bound is per venue and is never per request.
    for _ in 0..25 {
        let _ = c.venue_catalog("okx").expect("served");
        let _ = fresh.venue_catalog("okx").expect("served");
    }
    assert_eq!(spy.count(), 2, "one fetch per venue, whatever the client does");
    assert_eq!(spy.calls(), vec!["binance", "okx"]);
    // ⚠ The BUCKET's own refusal is exercised in `crates/vike-datahub/src/catalog.rs`'s unit tests,
    // where `Instant` is injected and a 60 s refill can be walked without sleeping. What this test
    // holds is the property that matters at the WIRE and that a unit test cannot see: no sequence
    // of real client requests, across reconnections, raises the server's venue spend above the
    // bound — which is 0062's decision 2 stated as an experiment.
}

// -------------------------------------------------------------------------------------------
// 6. The negotiation
// -------------------------------------------------------------------------------------------

/// **An old datahub that does not advertise the capability makes the client SAY SO** rather than
/// show an empty list.
///
/// This is the leg without which decision 5 leaks one layer up: a client that showed "no
/// instruments" for an un-advertised verb would be making exactly the claim `ig` and `ibkr` make
/// truthfully, about a venue for which it is false.
#[test]
fn an_old_server_makes_the_client_say_so_rather_than_show_an_empty_list() {
    let addr = spawn(None);
    let mut c = client(addr);
    assert!(!c.serves_venue_catalog());
    let err = c.venue_catalog("binance").expect_err("an old/unarmed server refuses locally");
    assert!(err.contains(FEATURE_VENUE_CATALOG), "the missing capability is named: {err}");
    assert!(err.contains("predates"), "the two causes are distinguished: {err}");
    assert!(err.contains("venue_catalog_off"), "{err}");
    // ...and crucially it is an Err, so no caller can mistake it for an empty catalog.
}

/// ...and an ARMED server advertises it, so the same client renders a control rather than a
/// sentence. The pair is what makes `serves_venue_catalog` a decidable question.
#[test]
fn an_armed_server_advertises_the_capability() {
    let (_spy, addr) = armed();
    let c = client(addr);
    assert!(c.serves_venue_catalog());
    assert!(c.features().iter().any(|f| f == FEATURE_VENUE_CATALOG), "{:?}", c.features());
}
