//! The production feed constructor and the feed objects' release order (`feeds.rs`, `feeds/live.rs`).

use std::sync::{Arc, Mutex};

use vike_data::{DataClient, LiveDataError, NoopSink, SubscriptionId};

use super::{FeedCtors, ProdFeedCtors, Tracked};
use crate::HlNetwork;

/// **No strategy-lane key, no dial — by name.** Absent credentials are the gate: the daemon takes its
/// prices from the datahub's strategy lane under its own `Scope::Trade` key, and a store holding none
/// refuses every dial before a socket exists, naming the key to mint.
#[test]
fn a_constructor_with_no_strategy_lane_key_refuses_by_name_and_dials_nothing() {
    let ctors = ProdFeedCtors::new("127.0.0.1:1", None);
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
    let ctors = ProdFeedCtors::new("127.0.0.1:1", Some(b"k".to_vec()));
    for (venue, network) in [("binance", None), ("hyperliquid", Some("testnet"))] {
        let mut feed = ctors
            .strat(venue, network, Arc::new(NoopSink))
            .unwrap_or_else(|e| panic!("{venue}: {e}"));
        assert_eq!(feed.venue(), venue);
        assert!(!feed.is_up(), "construction dials nothing on this thread");
        feed.shutdown();
    }
}

/// The word Hyperliquid's dial NAMES: the main network names none (the datahub's one main client),
/// the test network says `testnet`; and the tier maps `live` to the main network and every tier below it to
/// the test network.
#[test]
fn hyperliquids_network_is_named_only_off_the_main_network() {
    assert_eq!(HlNetwork::Mainnet.strat_network(), None);
    assert_eq!(HlNetwork::Testnet.strat_network(), Some("testnet"));
    use vike_model::AccountTier::{Demo, Live, Paper};
    assert_eq!(HlNetwork::for_tier(Live), HlNetwork::Mainnet);
    assert_eq!(HlNetwork::for_tier(Demo), HlNetwork::Testnet);
    assert_eq!(HlNetwork::for_tier(Paper), HlNetwork::Testnet);
}

/// A recording double: the order of every `unsubscribe` and the `shutdown` it sees.
struct Recording {
    log: Arc<Mutex<Vec<String>>>,
    next: u64,
}

impl Recording {
    fn id(&mut self) -> Result<SubscriptionId, LiveDataError> {
        self.next += 1;
        Ok(SubscriptionId(self.next))
    }
}

impl DataClient for Recording {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        self.id()
    }
    fn subscribe_quotes(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.id()
    }
    fn subscribe_trades(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.id()
    }
    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.id()
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
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
    let mut feed = Tracked::new(Box::new(Recording { log: Arc::clone(&log), next: 0 }));
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
        let mut feed = Tracked::new(Box::new(Recording { log: Arc::clone(&log), next: 0 }));
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
        let mut feed = Tracked::new(Box::new(Recording { log: Arc::clone(&log), next: 0 }));
        feed.subscribe_bars("X", "1m").unwrap();
        feed.shutdown();
        log.lock().unwrap().clear();
    }
    assert!(log.lock().unwrap().is_empty(), "already shut down: the drop does nothing more");
}
