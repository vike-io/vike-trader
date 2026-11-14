use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_catalog::{CatalogAvailability, CatalogError, PickerTab};
use vike_gatewayhub_client::catalog::CATALOG_VENUES;
use vike_model::{AssetClass, Contract};
use vike_secrets::venue_setting::seeded_venue_settings;

use super::*;
use crate::testkit::fixtures::{CatalogSearches, ScriptedCatalog};

fn instrument(venue: &str, symbol: &str) -> Instrument {
    Instrument::new(venue, symbol, "BTC", "USDT", Contract::CryptoSpot)
}

/// A scripted entry and the counters its provider writes to.
struct Scripted {
    entry: (String, CatalogProviderFactory),
    built: Arc<AtomicUsize>,
    searched: CatalogSearches,
}

fn fake(venue: &str, listing: Result<Vec<Instrument>, CatalogError>) -> Scripted {
    let built = Arc::new(AtomicUsize::new(0));
    let searched = Arc::new(Mutex::new(Vec::new()));
    let (name, counter, log) = (venue.to_string(), Arc::clone(&built), Arc::clone(&searched));
    let factory: CatalogProviderFactory = Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::new(ScriptedCatalog {
            venue: name.clone(),
            classes: &[AssetClass::CryptoSpot],
            listing: listing.clone(),
            searched: Arc::clone(&log),
        }) as Box<dyn CatalogProvider>
    });
    Scripted { entry: (venue.to_string(), factory), built, searched }
}

fn table(venue: &str, listing: Result<Vec<Instrument>, CatalogError>) -> CatalogProviders {
    CatalogProviders::new(vec![fake(venue, listing).entry])
}

/// **The production table is the client's `CATALOG_VENUES`** (what the datahub folds, and the registry's
/// row: what the daemon advertises), IN ORDER. The order is what a `NotServed` refusal prints.
#[test]
fn the_real_table_is_the_clients_roster_in_order() {
    let venues = CatalogProviders::real(false, &seeded_venue_settings()).venues();
    assert_eq!(venues, CATALOG_VENUES);
}

/// **Both ways**: a venue is in the table exactly when `catalog_availability` lets a server hold it
/// (public, or a held session: fxcm, oanda, ctrader), with ONE named exception — the finding wave 1 does not touch: a
/// table entry is a decision about what a Read client may make this daemon fetch, and nothing asks
/// for the Deribit one.
#[test]
fn the_real_table_is_the_served_set_without_deribit_both_ways() {
    let served = CatalogProviders::real(true, &seeded_venue_settings()).venues();
    let mut servable_but_absent = Vec::new();
    for &venue in vike_model::VENUES {
        let servable = catalog_availability(venue).is_served();
        let listed = served.iter().any(|v| v == venue);
        match (servable, listed) {
            (true, true) | (false, false) => {}
            (true, false) => servable_but_absent.push(venue),
            (false, true) => panic!("`{venue}` is in the table and is not served"),
        }
    }
    assert_eq!(
        servable_but_absent,
        ["deribit"],
        "the one venue public and deliberately not served"
    );
    assert_eq!(catalog_availability("fxcm"), CatalogAvailability::HeldSession);
}

/// **oanda and ctrader are served from the list a running account's facts already read** (decision 0062's
/// decision 3, amended 2026-11-13): the production entry describes the broker's two classes offline and lists
/// what the trade lane held, with no request. The slot is process-wide, so this test holds before it lists and
/// asserts nothing about the not-yet-held state (`vike_catalog::held_list`'s own test does, on its own key).
#[test]
fn a_held_broker_list_is_served_from_what_the_trade_lane_read() {
    let t = CatalogProviders::real(false, &seeded_venue_settings());
    for venue in ["oanda", "ctrader"] {
        let traits = t.describe(venue).expect("described offline");
        assert_eq!(traits.asset_classes, [AssetClass::Fx, AssetClass::Cfd], "{venue}");
    }
    let row = Instrument::new(
        "ctrader",
        "EURUSD",
        "EUR",
        "USD",
        Contract::Fx(vike_model::ContractFx::default()),
    );
    vike_catalog::held_list::hold("ctrader", vec![row.clone()]);
    assert_eq!(t.list("ctrader", &FetchContext::new()), Ok(vec![row]));
}

/// A provider that lists from a session this process does not hold: it says so, and lists nothing.
struct NoSession;

impl CatalogProvider for NoSession {
    fn venue(&self) -> &str {
        "fxcm"
    }
    fn asset_classes(&self) -> &[AssetClass] {
        &[AssetClass::Fx]
    }
    fn mode(&self) -> vike_catalog::CatalogMode {
        vike_catalog::CatalogMode::Enumerable
    }
    fn no_session(&self) -> Option<String> {
        Some("no fxcm account holds a session".to_string())
    }
    fn list_instruments(&self) -> Result<Vec<Instrument>, CatalogError> {
        panic!("a provider with no session must never be asked to list")
    }
}

/// **No held session is a ROUTINE refusal, never a failure and never an empty list**: the listing is
/// `Refused` in the provider's words with the provider never asked, while `CatalogDescribe` still
/// answers (its traits are static), so the datahub's `CatalogProviderGateway::open` succeeds and its
/// listing folds as no change. The production fxcm entry answers the same with no account running.
#[test]
fn a_venue_listed_from_a_session_not_held_is_refused_in_its_words_and_still_describes() {
    let factory: CatalogProviderFactory = Box::new(|| Box::new(NoSession));
    let t = CatalogProviders::new(vec![("fxcm".to_string(), factory)]);
    assert_eq!(
        t.list("fxcm", &FetchContext::new()),
        Err(SourceError::Refused("no fxcm account holds a session".to_string()))
    );
    assert_eq!(t.describe("fxcm").expect("described").asset_classes, [AssetClass::Fx]);

    let real = CatalogProviders::real(false, &seeded_venue_settings());
    let Err(SourceError::Refused(why)) = real.list("fxcm", &FetchContext::new()) else {
        panic!("no fxcm account runs in this test, so the real entry refuses");
    };
    assert!(why.contains("no fxcm account"), "{why}");
}

/// 0062's decision 3, driven through the PRODUCTION constructor: every roster venue that is not
/// served — credentialed (alpaca) or with no bulk list (ig, ibkr) — is
/// dropped when a caller hands it over, and an unknown venue with them.
#[test]
fn a_venue_that_is_not_served_cannot_be_mounted_even_when_a_caller_asks_for_it() {
    let mut entries = vec![fake("okx", Ok(Vec::new())).entry];
    let mut expected_refusals = Vec::new();
    for &venue in vike_model::VENUES {
        if !catalog_availability(venue).is_served() {
            entries.push(fake(venue, Ok(vec![instrument(venue, "X")])).entry);
            expected_refusals.push(venue.to_string());
        }
    }
    entries.push(fake("kraken", Ok(Vec::new())).entry);
    expected_refusals.push("kraken".to_string());
    assert!(expected_refusals.iter().any(|v| v == "alpaca"), "guard: alpaca is not served");
    assert_eq!(CatalogProviders::refused_entries(&entries), expected_refusals);
    let t = CatalogProviders::new(entries);
    assert_eq!(t.venues(), ["okx"]);
    for venue in &expected_refusals {
        assert!(t.describe(venue).is_err(), "{venue} must not be describable");
        let out = t.list(venue, &FetchContext::new());
        assert!(matches!(out, Err(SourceError::Refused(_))), "{venue}: {out:?}");
    }
}

/// Every production provider describes itself with no network: the traits are static facts of the
/// provider, which is why `CatalogDescribe` is cheap and the client caches it.
#[test]
fn every_real_provider_describes_itself_offline() {
    let t = CatalogProviders::real(false, &seeded_venue_settings());
    for venue in t.venues() {
        let traits = t.describe(&venue).unwrap_or_else(|e| panic!("{venue}: {e}"));
        assert!(!traits.asset_classes.is_empty(), "{venue} declares no asset class");
    }
}

/// A venue the table does not hold is a refusal naming the served set in order, and nothing is built.
#[test]
fn an_unserved_venue_is_refused_naming_the_served_set() {
    let Scripted { entry, built, .. } = fake("okx", Ok(Vec::new()));
    let t = CatalogProviders::new(vec![entry]);
    let text = t.describe("kraken").expect_err("not served");
    assert!(text.contains("`kraken`") && text.contains("(served: [okx])"), "{text}");
    let Err(SourceError::Refused(text)) = t.list("kraken", &FetchContext::new()) else {
        panic!("an unserved venue is a Refused");
    };
    assert!(text.contains("nothing was asked of any venue"), "{text}");
    assert_eq!(built.load(Ordering::SeqCst), 0, "a refusal builds no provider");
}

/// **The provider's FULL answer**: past the datahub's 25,000-row cap, which stays in the datahub.
#[test]
fn a_listing_is_sent_whole_and_the_cap_is_the_datahubs() {
    let big: Vec<Instrument> = (0..25_001).map(|i| instrument("okx", &format!("SYM{i}"))).collect();
    let t = table("okx", Ok(big.clone()));
    assert_eq!(t.list("okx", &FetchContext::new()).expect("listed"), big);
}

/// A provider's failure crosses as `Fetch` with its INNER text — the `catalog error: ` prefix is the
/// `CatalogError`'s `Display`, which the datahub's own wrapper adds back — and is never an empty list.
#[test]
fn a_provider_failure_keeps_its_inner_words_and_is_never_an_empty_list() {
    let t = table("okx", Err(CatalogError("okx instruments HTTP 503".to_string())));
    assert_eq!(
        t.list("okx", &FetchContext::new()),
        Err(SourceError::Fetch("okx instruments HTTP 503".to_string()))
    );
    assert_eq!(
        t.search("okx", "btc", None, None, &FetchContext::new()),
        Err(SourceError::Fetch("okx instruments HTTP 503".to_string()))
    );
}

/// A provider is built for EACH call, as the datahub built one: `CatalogProviderHyperliquid` holds state, and
/// one kept across calls could remember what the in-process one forgot.
#[test]
fn a_provider_is_built_per_call() {
    let Scripted { entry, built, .. } = fake("okx", Ok(vec![instrument("okx", "BTC-USDT")]));
    let t = CatalogProviders::new(vec![entry]);
    let ctx = FetchContext::new();
    t.list("okx", &ctx).expect("first");
    t.list("okx", &ctx).expect("second");
    t.describe("okx").expect("describe");
    assert_eq!(built.load(Ordering::SeqCst), 3);
}

/// The picker's tab and venue filter reach the provider's search as the types it takes.
#[test]
fn a_search_carries_the_query_and_the_filter() {
    let Scripted { entry, searched, .. } = fake("okx", Ok(vec![instrument("okx", "BTC-USDT")]));
    let t = CatalogProviders::new(vec![entry]);
    let ctx = FetchContext::new();
    t.search("okx", "btc", Some(PickerTab::Perps), Some("bybit".to_string()), &ctx)
        .expect("search");
    t.search("okx", "eth", None, None, &ctx).expect("search");
    let log = searched.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(
        log,
        vec![
            ("btc".to_string(), Some(PickerTab::Perps), Some("bybit".to_string())),
            ("eth".to_string(), None, None),
        ]
    );
}

/// A fetch cancelled while it waited never builds a provider's request.
#[test]
fn a_cancelled_fetch_never_reaches_the_provider() {
    let Scripted { entry, searched, .. } = fake("okx", Ok(Vec::new()));
    let t = CatalogProviders::new(vec![entry]);
    let ctx = FetchContext::new();
    ctx.cancel();
    assert!(t.list("okx", &ctx).is_err());
    assert!(t.search("okx", "x", None, None, &ctx).is_err());
    assert!(searched.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_empty());
}

/// `hyperliquid_hip3` is a PARAMETER the entry hands to its provider, not a literal. A provider's
/// configuration is not readable through the table, and listing hyperliquid reaches the exchange; what
/// the provider DOES with the flag is held in the bridge
/// (`crates/bridges/hyperliquid/src/catalog.rs`'s `the_hip3_universe_is_the_callers_choice`).
/// This holds the other link, from the source text, which the default build runs too.
#[test]
fn the_hyperliquid_entry_hands_the_flag_to_its_provider() {
    let src = include_str!("catalog.rs");
    let body = &src[src.find("pub fn real(").expect("the real table")..];
    let entry = &body[body.find("entry(\"hyperliquid\"").expect("the hyperliquid entry")..];
    let entry = &entry[..entry.find("entry(\"polymarket\"").expect("the next entry")];
    assert!(
        entry.contains("CatalogProviderHyperliquid::new(&hyperliquid)")
            && entry.contains(".with_hip3(hyperliquid_hip3)"),
        "the entry must build its catalog with the table's `hyperliquid_hip3` argument:\n{entry}"
    );
    assert!(entry.contains("move ||"), "the closure must own the flag it was built with:\n{entry}");
}
