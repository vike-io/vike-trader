//! The production feed constructor and the feed objects' release order (`feeds.rs`, `feeds/live.rs`).

use std::sync::{Arc, Mutex};

use vike_data::{DataClient, DataClientError, DataClientSubscriptionId, NoopSink};

use super::{DataClientTracking, StrategyLaneFactories, StrategyLaneFactoriesProduction};
use vike_model::VenueNetwork;

/// **No strategy-lane key, no dial — by name.** Absent credentials are the gate: the daemon takes its
/// prices from the datahub's strategy lane under its own `Scope::Trade` key, and a store holding none
/// refuses every dial before a socket exists, naming the key to mint.
#[test]
fn a_constructor_with_no_strategy_lane_key_refuses_by_name_and_dials_nothing() {
    let ctors = StrategyLaneFactoriesProduction::new("127.0.0.1:1", None);
    let err = match ctors.strat("binance", None, Arc::new(NoopSink)) {
        Ok(_) => panic!("no key must refuse"),
        Err(e) => e,
    };
    assert!(
        err.contains(vike_venues::credential_keys::datahub_trade_key())
            && err.contains("Nothing was dialled"),
        "{err}"
    );
    assert!(err.contains("binance"), "names the venue: {err}");
    // …and `client` (every non-pump arm's door) is the same refusal.
    let err = match ctors.client("deribit", None, Arc::new(NoopSink)) {
        Ok(_) => panic!("no key must refuse"),
        Err(e) => e,
    };
    assert!(err.contains(vike_venues::credential_keys::datahub_trade_key()), "{err}");
}

/// With a key the dial is built (and never dials on this thread): the feed serves the venue it was
/// asked for, on the test network when asked (Hyperliquid's alone), and shuts down cleanly with its link
/// still down.
#[test]
fn a_constructor_with_a_key_builds_the_venues_dial() {
    let ctors = StrategyLaneFactoriesProduction::new("127.0.0.1:1", Some(b"k".to_vec()));
    for (venue, network) in [("binance", None), ("hyperliquid", Some("testnet"))] {
        let mut feed = ctors
            .strat(venue, network, Arc::new(NoopSink))
            .unwrap_or_else(|e| panic!("{venue}: {e}"));
        assert_eq!(feed.venue(), venue);
        assert!(!feed.is_up(), "construction dials nothing on this thread");
        feed.shutdown();
    }
}

/// **Every dial made is watched, and a dial nobody holds is not a lost link**: a datahub that does not answer lists the
/// venue with the dial's own words, a venue with two dials is one row, and a feed an arm dropped (a refused arm's
/// partial feed) reads as nothing, or it would call a healthy datahub dead for ever.
#[test]
fn every_dial_is_watched_and_a_dropped_one_is_not_a_lost_link() {
    let ctors = StrategyLaneFactoriesProduction::new("127.0.0.1:1", Some(b"k".to_vec()));
    let links = ctors.price_links().expect("production watches its dials");
    assert!(links.lost().is_empty(), "no dial, nothing lost");

    let binance = ctors.strat("binance", None, Arc::new(NoopSink)).expect("a dial");
    let again = ctors.strat("binance", None, Arc::new(NoopSink)).expect("a second dial");
    let bybit = ctors.strat("bybit", None, Arc::new(NoopSink)).expect("a dial");
    let lost = links.lost();
    assert_eq!(
        lost.iter().map(|l| (l.venue.as_str(), l.of)).collect::<Vec<_>>(),
        [("binance", 2), ("bybit", 2)],
        "one row per venue, each knowing how many venues want a lane: {lost:?}"
    );
    assert!(lost.iter().all(|l| !l.why.is_empty()), "{lost:?}");

    drop(binance);
    drop(again);
    assert_eq!(
        links.lost().iter().map(|l| (l.venue.as_str(), l.of)).collect::<Vec<_>>(),
        [("bybit", 1)],
        "a dropped feed neither lists nor counts"
    );
    drop(bybit);
    assert!(links.lost().is_empty());
}

/// The word Hyperliquid's dial NAMES: the main network names none (the datahub's one main client),
/// the test network says `testnet`; and the tier maps `live` to the main network and every tier below it to
/// the test network.
#[test]
fn hyperliquids_network_is_named_only_off_the_main_network() {
    assert_eq!(super::hyperliquid::strategy_network(VenueNetwork::Mainnet), None);
    assert_eq!(super::hyperliquid::strategy_network(VenueNetwork::Testnet), Some("testnet"));
    use vike_model::AccountTier::{Demo, Live, Paper};
    assert_eq!(VenueNetwork::for_tier(Live), VenueNetwork::Mainnet);
    assert_eq!(VenueNetwork::for_tier(Demo), VenueNetwork::Testnet);
    assert_eq!(VenueNetwork::for_tier(Paper), VenueNetwork::Testnet);
}

/// A recording double: the order of every `unsubscribe` and the `shutdown` it sees.
struct Recording {
    log: Arc<Mutex<Vec<String>>>,
    next: u64,
}

impl Recording {
    fn id(&mut self) -> Result<DataClientSubscriptionId, DataClientError> {
        self.next += 1;
        Ok(DataClientSubscriptionId(self.next))
    }
}

impl DataClient for Recording {
    fn subscribe_bars(
        &mut self,
        _s: &str,
        _i: &str,
    ) -> Result<DataClientSubscriptionId, DataClientError> {
        self.id()
    }
    fn subscribe_quotes(&mut self, _s: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        self.id()
    }
    fn subscribe_trades(&mut self, _s: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        self.id()
    }
    fn subscribe_book(&mut self, _s: &str) -> Result<DataClientSubscriptionId, DataClientError> {
        self.id()
    }
    fn unsubscribe(&mut self, id: DataClientSubscriptionId) {
        self.log.lock().unwrap().push(format!("unsubscribe {}", id.0));
    }
    fn shutdown(&mut self) {
        self.log.lock().unwrap().push("shutdown".to_string());
    }
}

/// **A feed releases what it holds BEFORE it shuts down** (the strat-link gap-closure rule: a gap the
/// link disclosed is closed by the last unsubscribe of its symbol, not left for the dead-man). A
/// subscription the arm already released is not released twice.
#[test]
fn a_tracked_feed_unsubscribes_what_it_holds_and_then_shuts_down() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut feed = DataClientTracking::new(Box::new(Recording { log: Arc::clone(&log), next: 0 }));
    let a = feed.subscribe_bars("X", "1m").unwrap();
    let b = feed.subscribe_quotes("X").unwrap();
    let _c = feed.subscribe_trades("X").unwrap();
    feed.unsubscribe(b);
    log.lock().unwrap().clear();
    feed.shutdown();
    let seen = log.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![format!("unsubscribe {}", a.0), "unsubscribe 3".to_string(), "shutdown".to_string()]
    );
}

/// **A PARTIAL feed is released in order too** (review 17, finding 8b): an arm that meets a refusal after its first
/// subscription returns `Err` and drops the feed it built. Dropped without a `shutdown`, it unsubscribes what it holds
/// and then shuts down, exactly as the teardown does; one that WAS shut down is not shut down a second time.
#[test]
fn a_partial_feed_dropped_without_a_shutdown_is_released_in_order_and_a_closed_one_is_not_closed_twice()
 {
    let log = Arc::new(Mutex::new(Vec::new()));
    {
        let mut feed =
            DataClientTracking::new(Box::new(Recording { log: Arc::clone(&log), next: 0 }));
        let a = feed.subscribe_bars("X", "1m").unwrap();
        let b = feed.subscribe_quotes("X").unwrap();
        log.lock().unwrap().clear();
        drop(feed); // the arm's `?` on a third subscription that was refused
        let seen = log.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                format!("unsubscribe {}", a.0),
                format!("unsubscribe {}", b.0),
                "shutdown".to_string()
            ],
            "released, then shut down, on drop"
        );
    }
    log.lock().unwrap().clear();
    {
        let mut feed =
            DataClientTracking::new(Box::new(Recording { log: Arc::clone(&log), next: 0 }));
        feed.subscribe_bars("X", "1m").unwrap();
        feed.shutdown();
        log.lock().unwrap().clear();
    }
    assert!(log.lock().unwrap().is_empty(), "already shut down: the drop does nothing more");
}
