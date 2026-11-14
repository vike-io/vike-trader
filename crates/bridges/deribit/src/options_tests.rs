use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::options_fixture::{FakeDeribit, spawn_fake_deribit};

/// A provider on the release's mainnet rows (`VENUE_URL_SEED`), before a test points it elsewhere.
fn seeded_provider() -> DeribitOptionsProvider {
    DeribitOptionsProvider::from_urls(&VenueUrls::seed("deribit")).expect("the seed's live rows")
}

fn lane_over(fake: &FakeDeribit) -> OptionsSpecDeribit {
    OptionsSpecDeribit::with_provider(over(fake))
}

/// A provider whose book summary and option listing are both the stand-in's.
fn over(fake: &FakeDeribit) -> DeribitOptionsProvider {
    seeded_provider()
        .with_book_summary_url(fake.url.clone())
        .with_instruments_url(fake.instruments_url.clone())
}

fn no_marks() -> OptionsMarksDeliver {
    Box::new(|_| {})
}

fn no_ticker() -> OptionsTickerDeliver {
    Box::new(|_| {})
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

/// A lane whose rows lack the live row answers every request with the refusal, naming the row, asks
/// the venue nothing, and logs the refusal ONCE, at `error`, when it is built.
#[test]
fn a_lane_without_its_live_rows_refuses_every_request() {
    let (lane, events) = vike_log::capture::captured(|| {
        OptionsSpecDeribit::mainnet(&VenueUrls::from_rows("deribit", std::iter::empty()))
    });
    let text = lane.expiries("BTC").expect_err("no row");
    assert!(text.contains("no address row for live data rest"), "{text}");
    assert!(
        text.starts_with("deribit options: ") && text.ends_with("; nothing was asked of the venue"),
        "{text}"
    );
    assert_eq!(lane.listing(false).err(), Some(text.clone()));
    assert_eq!(lane.chain("BTC", "2037-06-25", None, 0.03).err(), Some(text.clone()));
    assert_eq!(lane.instruments("option").err(), Some(text.clone()));
    assert_eq!(lane.open_marks(names(&["btc_usd"]), no_marks()).err(), Some(text.clone()));
    assert_eq!(
        lane.open_ticker(names(&["BTC-25JUN37-100000-C"]), no_ticker()).err(),
        Some(text.clone())
    );
    let logged: Vec<&str> = events
        .iter()
        .filter(|e| e.level == tracing::Level::ERROR)
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(logged, [format!("{text} (fix the row and restart the daemon)")]);
}

/// **One provider per daemon**: an `expiries` and a `chain` call (and a second `chain`) share ONE
/// book-summary request, because the provider's 5 s cache survives across calls. This is the cost the
/// datahub's refresh pass paid in-process; a provider per request would pay it per call.
#[test]
fn the_persistent_provider_shares_one_book_summary_across_calls() {
    let venue = spawn_fake_deribit();
    let lane = lane_over(&venue);
    lane.expiries("BTC").expect("expiries");
    lane.chain("BTC", "2037-06-25", None, 0.03).expect("the first chain");
    lane.chain("BTC", "2037-06-25", Some(2), 0.03).expect("the second chain");
    assert_eq!(venue.hits.load(Ordering::SeqCst), 1, "the cache did not survive across calls");
}

/// …and the contrast, so the test above can fail: two lanes (a provider per request) cost two requests.
#[test]
fn a_provider_per_request_would_pay_the_venue_each_time() {
    let venue = spawn_fake_deribit();
    lane_over(&venue).expiries("BTC").expect("first");
    lane_over(&venue).expiries("BTC").expect("second");
    assert_eq!(venue.hits.load(Ordering::SeqCst), 2);
}

/// `strikes: None` is the FULL chain (what the datahub records before it applies its own window);
/// `Some(n)` windows it. The answer is the provider's own chain, greeks and all.
#[test]
fn the_full_chain_is_all_strikes_and_a_window_narrows_it() {
    let venue = spawn_fake_deribit();
    let lane = lane_over(&venue);
    let full = lane.chain("BTC", "2037-06-25", None, 0.03).expect("full");
    let windowed = lane.chain("BTC", "2037-06-25", Some(1), 0.03).expect("windowed");
    assert_eq!(full.rows.len(), 5, "the full chain holds every strike the venue listed");
    assert!(windowed.rows.len() < full.rows.len(), "a ±1 window must narrow five strikes");
    assert_eq!((full.underlying.as_str(), full.source.as_str()), ("BTC", "deribit"));
}

/// **The listing is the VENUE's, in canonical order, with each underlying's index read from the venue**:
/// the indexes the datahub's markprice feed subscribes are the venue's `price_index`, never a table. One
/// listing read, held across calls; a refresh asked within the gap answers the listing held and reads
/// nothing; and no book is read.
#[test]
fn the_listing_is_the_venues_with_its_indexes_in_canonical_order() {
    let venue = spawn_fake_deribit();
    let lane = lane_over(&venue);
    let want: Vec<OptionsListedUnderlying> =
        [("BTC", "btc_usd"), ("ETH", "eth_usd"), ("SOL", "sol_usdc")]
            .iter()
            .map(|(name, index)| OptionsListedUnderlying {
                name: (*name).into(),
                index: (*index).into(),
            })
            .collect();
    for refresh in [false, true, false] {
        assert_eq!(lane.listing(refresh).expect("listed"), want);
    }
    assert_eq!(lane.venue(), "deribit");
    assert_eq!(venue.listing_hits.load(Ordering::SeqCst), 1, "the listing is held across calls");
    assert_eq!(venue.hits.load(Ordering::SeqCst), 0, "listing the set reads no book");
}

/// A listing that cannot be read is the read's own words on every call, never an empty set, and a feed
/// open is refused with them.
#[test]
fn an_unreadable_listing_is_an_error_never_an_empty_set() {
    let venue = spawn_fake_deribit();
    let lane = OptionsSpecDeribit::with_provider(
        seeded_provider().with_book_summary_url(venue.url.clone()),
    );
    let text = lane.listing(false).expect_err("no listing read");
    assert!(text.contains("no listing read"), "{text}");
    let text = lane.expiries("BTC").expect_err("no listing, so no underlying");
    assert!(text.contains("no listing read"), "{text}");
    let text = lane.open_marks(names(&["btc_usd"]), no_marks()).err().expect("refused");
    assert_eq!(
        text,
        "deribit options: the venue's option listing could not be read (deribit option \
         instruments: this test provider has no listing read (`with_instruments_url`)); nothing \
         was subscribed"
    );
    assert_eq!(venue.hits.load(Ordering::SeqCst), 0, "no book was read");
}

/// A name outside the served set is refused before anything is sent: the client names a term of the
/// URL, and the 0052 cost rule is that it may not.
#[test]
fn an_underlying_the_provider_does_not_serve_is_refused_before_the_venue() {
    let venue = spawn_fake_deribit();
    let lane = lane_over(&venue);
    let text = lane.expiries("DOGE").expect_err("not a served underlying");
    assert_eq!(
        text,
        "deribit options: `DOGE` is not an underlying this daemon serves (served: [BTC, ETH, \
         SOL]); nothing was asked of the venue"
    );
    let text = lane
        .chain("BTC&kind=future", "2037-06-25", None, 0.0)
        .expect_err("not a served underlying");
    assert!(text.contains("served: [BTC, ETH, SOL]"), "{text}");
    assert_eq!(venue.hits.load(Ordering::SeqCst), 0);
}

/// A venue failure is the provider's own words, unchanged — never an empty answer.
#[test]
fn a_provider_failure_is_its_own_words() {
    // Nothing listens at this address: port 1, which no test's listener can be handed (a port bound
    // and released can be).
    let addr = "127.0.0.1:1";
    let listing = spawn_fake_deribit();
    let lane = OptionsSpecDeribit::with_provider(
        seeded_provider()
            .with_book_summary_url(format!("http://{addr}/book"))
            .with_instruments_url(listing.instruments_url.clone()),
    );
    let text = lane.expiries("BTC").expect_err("the venue is unreachable");
    assert!(text.starts_with("deribit book summary:"), "{text}");
}

fn instruments(n: usize) -> Vec<Instrument> {
    (0..n)
        .map(|i| {
            Instrument::new(
                "deribit",
                format!("BTC-25JUN37-{i}-C"),
                "BTC",
                "USD",
                vike_model::Contract::CryptoSpot,
            )
        })
        .collect()
}

/// The index fetch asks for `currency=any` and the kind's venue word, and its failure carries the
/// datahub's former prefix and the `CatalogError`'s own `Display`, so the log line is unchanged.
#[test]
fn the_instrument_fetch_asks_for_any_and_keeps_the_former_failure_text() {
    let asked: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&asked);
    let lane = OptionsSpecDeribit::mainnet(&VenueUrls::seed("deribit")).with_instruments(Box::new(
        move |currency: &str, kind: &str| {
            log.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((currency.to_string(), kind.to_string()));
            if kind == "future" {
                Err(CatalogError("HTTP 503".to_string()))
            } else {
                Ok(instruments(2))
            }
        },
    ));
    assert_eq!(lane.instruments("option").expect("options"), instruments(2));
    assert_eq!(
        lane.instruments("future"),
        Err("deribit future instruments: catalog error: HTTP 503".to_string())
    );
    assert_eq!(
        *asked.lock().unwrap_or_else(PoisonError::into_inner),
        vec![("any".to_string(), "option".to_string()), ("any".to_string(), "future".to_string())]
    );
}

type Opened = Arc<Mutex<Vec<(&'static str, Vec<String>)>>>;

/// A lane whose two feeds record the focus set they were opened over and count their stops; its
/// listing (BTC, ETH, SOL) is the stand-in's.
fn recording_lane() -> (OptionsSpecDeribit, Opened, Arc<AtomicUsize>) {
    let opened: Opened = Arc::new(Mutex::new(Vec::new()));
    let stops = Arc::new(AtomicUsize::new(0));
    let stop = |stops: &Arc<AtomicUsize>| -> OptionsFeedStop {
        let stops = Arc::clone(stops);
        Box::new(move || {
            stops.fetch_add(1, Ordering::SeqCst);
        })
    };
    let (marks_opened, marks_stops) = (Arc::clone(&opened), Arc::clone(&stops));
    let markprice: FeedSpawn<OptionsMarksDeliver> = Box::new(move |set, _deliver| {
        marks_opened.lock().unwrap_or_else(PoisonError::into_inner).push(("markprice", set));
        stop(&marks_stops)
    });
    let (ticker_opened, ticker_stops) = (Arc::clone(&opened), Arc::clone(&stops));
    let ticker: FeedSpawn<OptionsTickerDeliver> = Box::new(move |set, _deliver| {
        ticker_opened.lock().unwrap_or_else(PoisonError::into_inner).push(("ticker", set));
        stop(&ticker_stops)
    });
    // The stand-in outlives the lane: its listener thread runs until the test process ends.
    let lane = OptionsSpecDeribit::with_provider(over(&spawn_fake_deribit()))
        .with_feeds(markprice, ticker);
    (lane, opened, stops)
}

/// Each open reaches ITS feed with its own focus set, unchanged, and the stop it answers stops it.
#[test]
fn each_feed_opens_over_its_focus_set_and_answers_its_stop() {
    let (lane, opened, stops) = recording_lane();
    let marks = lane.open_marks(names(&["btc_usd", "eth_usd"]), no_marks()).expect("marks");
    let ticker = lane.open_ticker(names(&["BTC-25JUN37-100000-C"]), no_ticker()).expect("ticker");
    assert_eq!(
        *opened.lock().unwrap_or_else(PoisonError::into_inner),
        vec![
            ("markprice", names(&["btc_usd", "eth_usd"])),
            ("ticker", names(&["BTC-25JUN37-100000-C"])),
        ]
    );
    marks();
    ticker();
    assert_eq!(stops.load(Ordering::SeqCst), 2);
}

/// **A focus-set name must be a real one.** A well-formed word that is no index or instrument of a
/// served underlying would be subscribed, refused by Deribit and retried by the bridge feed's
/// reconnect loop for the life of the socket, so it is refused here and no feed is started. The real
/// shapes the datahub names (`btc_usd`, `sol_usdc`, `SOL_USDC-…`) open.
#[test]
fn a_well_formed_but_unreal_name_is_refused_before_a_feed_is_started() {
    let (lane, opened, _) = recording_lane();
    let unreal_indexes: [&[&str]; 5] =
        [&["xyz_usd"], &["btcusd"], &["_usd"], &["btc_"], &["btc_usd", "junk"]];
    for indexes in unreal_indexes {
        let err = lane.open_marks(names(indexes), no_marks()).err().unwrap_or_else(|| {
            panic!("{indexes:?} was opened");
        });
        assert!(err.contains("nothing was subscribed"), "{err}");
    }
    let unreal_instruments: [&[&str]; 5] = [
        &["BTC-foo"],
        &["XYZ-25JUN37-100000-C"],
        &["BTC-31FEB37-100000-C"],
        &["BTC-25JUN37-100000-X"],
        &["BTC-25JUN37-100000-C", "ETH"],
    ];
    for instruments in unreal_instruments {
        let err = lane.open_ticker(names(instruments), no_ticker()).err().unwrap_or_else(|| {
            panic!("{instruments:?} was opened");
        });
        assert!(err.contains("nothing was subscribed"), "{err}");
    }
    assert!(opened.lock().unwrap_or_else(PoisonError::into_inner).is_empty(), "a feed was started");
    // Byte for byte, one of each kind.
    assert_eq!(
        lane.open_marks(names(&["xyz_usd"]), no_marks()).err(),
        Some(
            "deribit options: `xyz_usd` is not an index of an underlying this daemon serves (an \
             index is `<underlying>_<settlement>`, e.g. `btc_usd`; served: [BTC, ETH, SOL]); \
             nothing was subscribed"
                .to_string()
        )
    );
    assert_eq!(
        lane.open_ticker(names(&["BTC-foo"]), no_ticker()).err(),
        Some(
            "deribit options: `BTC-foo` is not an option instrument of an underlying this daemon \
             serves (served: [BTC, ETH, SOL]); nothing was subscribed"
                .to_string()
        )
    );
    assert!(lane.open_marks(names(&["btc_usd", "eth_usd", "sol_usdc"]), no_marks()).is_ok());
    assert!(lane.open_marks(names(&["BTC_USD"]), no_marks()).is_ok());
    assert!(
        lane.open_ticker(names(&["SOL_USDC-25JUN37-150-C", "ETH-25JUN37-4000-P"]), no_ticker())
            .is_ok()
    );
}
