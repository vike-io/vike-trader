//! Hyperliquid's price handle for a symbol the control door admitted (decision 0174): the venue's
//! feed subscribes the symbol's QUOTE lane (it has no CEX tick pump), the price it delivers is read
//! back through the same handle, and the teardown still releases everything in order.

use std::sync::{Arc, Mutex};

use vike_data::{DataClient, LiveDataError, LiveDataSink, NoopSink, SubscriptionId};
use vike_marketdata::QuoteTick;

use super::price_tap::{PriceTap, TapSink};
use super::{LiveFeeds, QuoteFeed, Tracked};

/// A recording double: every verb the feed sees, in order.
struct Recording {
    log: Arc<Mutex<Vec<String>>>,
    next: u64,
}

impl Recording {
    fn id(&mut self, what: String) -> Result<SubscriptionId, LiveDataError> {
        self.next += 1;
        self.log.lock().unwrap().push(format!("{what} -> {}", self.next));
        Ok(SubscriptionId(self.next))
    }
}

impl DataClient for Recording {
    fn subscribe_bars(&mut self, s: &str, i: &str) -> Result<SubscriptionId, LiveDataError> {
        self.id(format!("bars {s}@{i}"))
    }
    fn subscribe_quotes(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.id(format!("quotes {s}"))
    }
    fn subscribe_trades(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.id(format!("trades {s}"))
    }
    fn subscribe_book(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.id(format!("book {s}"))
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.log.lock().unwrap().push(format!("unsubscribe {}", id.0));
    }
    fn shutdown(&mut self) {
        self.log.lock().unwrap().push("shutdown".to_string());
    }
}

fn feed() -> (QuoteFeed, Arc<Mutex<Vec<String>>>, Arc<PriceTap>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let tap = Arc::new(PriceTap::default());
    let tracked = Tracked::new(Box::new(Recording { log: Arc::clone(&log), next: 0 }));
    (QuoteFeed::new(tracked, Arc::clone(&tap)), log, tap)
}

fn seen(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    log.lock().unwrap().clone()
}

/// **The admitted symbol's price lane is its QUOTE subscription**, opened once however often the
/// door asks, with the case the venue spells (`kPEPE`), and the quote it delivers is the price.
#[test]
fn an_admitted_symbol_subscribes_its_quotes_once_and_reads_their_mid() {
    let (feed, log, tap) = feed();
    let watcher = feed.watcher();
    watcher.subscribe_ticks("kPEPE", 0.000001).expect("subscribed");
    watcher.subscribe_ticks("kPEPE", 0.000001).expect("idempotent");
    assert_eq!(seen(&log), vec!["quotes kPEPE -> 1".to_string()], "one subscription, exact case");

    assert_eq!(watcher.price("hyperliquid", "kPEPE"), None, "nothing delivered yet");
    let sink = TapSink::new(Arc::new(NoopSink) as Arc<dyn LiveDataSink>, tap);
    let quote = QuoteTick {
        ts: 1,
        local_ts: 0,
        bid: 0.25,
        ask: 0.75,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    sink.quote("hyperliquid", "kPEPE", quote);
    assert_eq!(watcher.price("hyperliquid", "kPEPE"), Some(0.5));
    assert_eq!(watcher.price("hyperliquid", "KPEPE"), None, "another case is another coin");
}

/// The teardown releases what the door opened BEFORE the dial closes (the strat-link gap rule), and
/// a handle outliving the feed refuses instead of subscribing on a dead dial.
#[test]
fn the_teardown_releases_admitted_symbols_in_order_and_a_late_handle_refuses() {
    let (mut feed, log, _) = feed();
    let watcher = feed.watcher();
    watcher.subscribe_ticks("kPEPE", 1.0).unwrap();
    watcher.subscribe_ticks("BTC", 0.1).unwrap();
    log.lock().unwrap().clear();
    feed.shutdown();
    assert_eq!(seen(&log), vec!["unsubscribe 1", "unsubscribe 2", "shutdown"]);
    let err = watcher.subscribe_ticks("ETH", 0.1).expect_err("the feed is shut down");
    assert!(format!("{err:?}").contains("shut down"), "{err:?}");
    assert_eq!(seen(&log).len(), 3, "nothing subscribed after the shutdown");
}

/// The feed variant hands the door its handle, so `bind_prices` binds hyperliquid like a CEX venue.
#[test]
fn the_hyperliquid_variant_hands_out_a_price_watcher() {
    let (feed, _, _) = feed();
    assert!(LiveFeeds::Hyperliquid(feed).price_watcher().is_some());
}

/// The same for deribit's variant, which the price-only wiring (`wire_deribit_price_feed`) relies on.
#[test]
fn the_deribit_variant_hands_out_a_price_watcher() {
    let (feed, _, _) = feed();
    assert!(LiveFeeds::Deribit(feed).price_watcher().is_some());
}

/// alpaca's variant hands the door its handle too (decision 0174's price lane for an admitted symbol).
#[test]
fn the_alpaca_variant_hands_out_a_price_watcher() {
    let (feed, _, _) = feed();
    assert!(LiveFeeds::Alpaca(feed).price_watcher().is_some());
}

/// ctrader's variant hands the door its handle too (decision 0174's price lane for an admitted symbol).
#[test]
fn the_ctrader_variant_hands_out_a_price_watcher() {
    let (feed, _, _) = feed();
    assert!(LiveFeeds::Ctrader(feed).price_watcher().is_some());
}

/// ig's variant hands the door its handle too: a foreign IG epic has a price only through it.
#[test]
fn the_ig_variant_hands_out_a_price_watcher() {
    let (feed, _, _) = feed();
    assert!(LiveFeeds::Ig(feed).price_watcher().is_some());
}

/// **ig's lane cap**: each extra quote lane is a logged-in IG session plus a socket, so the feed refuses a
/// subscription past the cap NAMING it. A lane already open is idempotent (it costs no place), the refusal
/// subscribes nothing, a lane the idle reaper closes frees its place, and an uncapped feed (every other
/// venue) is unbounded as before.
#[test]
fn a_lane_cap_refuses_the_lane_past_it_and_the_reaper_frees_a_place() {
    let (uncapped, log, _) = feed();
    let capped = uncapped.with_lane_cap(2);
    let watcher = capped.watcher();
    watcher.subscribe_ticks("CS.D.AAA.MINI.IP", 1.0).expect("first");
    watcher.subscribe_ticks("CS.D.BBB.MINI.IP", 1.0).expect("second");
    watcher.subscribe_ticks("CS.D.AAA.MINI.IP", 1.0).expect("an open lane costs no place");
    let err = watcher.subscribe_ticks("CS.D.CCC.MINI.IP", 1.0).expect_err("past the cap");
    assert!(format!("{err:?}").contains("at most 2 extra quote lanes"), "{err:?}");
    assert_eq!(seen(&log).len(), 2, "the refusal subscribed nothing: {:?}", seen(&log));

    assert!(watcher.close_ticks("ig", "CS.D.AAA.MINI.IP"));
    watcher.subscribe_ticks("CS.D.CCC.MINI.IP", 1.0).expect("a freed place");

    let (open, _, _) = feed();
    let watcher = open.watcher();
    for n in 0..20 {
        watcher.subscribe_ticks(&format!("S{n}"), 1.0).expect("no cap");
    }
}

/// The cap ig's arm applies is a ceiling UNDER the engine's own cap on admitted symbols (otherwise it
/// bounds nothing) and leaves room for at least one extra epic.
#[test]
fn igs_lane_cap_is_a_real_ceiling_under_the_engines_cap() {
    let cap = super::ig::IG_MAX_QUOTE_LANES;
    assert!((1..vike_exec::MAX_EXTRA_SYMBOLS).contains(&cap), "{cap}");
}

/// A handle that outlives the feed keeps the dial alive, so the feed's own `Drop` must still release
/// in order (the `Tracked` order), not leave it to the handle's.
#[test]
fn dropping_the_feed_while_a_handle_lives_still_releases_in_order() {
    let (feed, log, _) = feed();
    let watcher = feed.watcher();
    watcher.subscribe_ticks("kPEPE", 1.0).unwrap();
    log.lock().unwrap().clear();
    drop(feed);
    assert_eq!(seen(&log), vec!["unsubscribe 1", "shutdown"]);
    assert!(watcher.subscribe_ticks("ETH", 1.0).is_err(), "the dial is closed");
}

/// **The idle reaper closes a quote lane like a tick pump** (`LeaseFacts::close_idle_pumps` calls
/// `CexWatcher::close_ticks`): the lane is unsubscribed once, its price is forgotten, the teardown does
/// not release it a second time, and a later ask for the same coin opens a fresh lane. The mount's own
/// quotes (not opened through the watcher) are never in the watcher's book.
#[test]
fn the_idle_reaper_closes_a_quote_lane_once_and_a_later_ask_reopens_it() {
    let (mut feed, log, tap) = feed();
    let watcher = feed.watcher();
    watcher.subscribe_ticks("kPEPE", 1.0).unwrap();
    let sink = TapSink::new(Arc::new(NoopSink) as Arc<dyn LiveDataSink>, tap);
    let quote = QuoteTick {
        ts: 1,
        local_ts: 0,
        bid: 1.0,
        ask: 3.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    sink.quote("hyperliquid", "kPEPE", quote);
    assert_eq!(watcher.price("hyperliquid", "kPEPE"), Some(2.0));

    assert!(watcher.close_ticks("hyperliquid", "kPEPE"), "one lane was closed");
    assert!(!watcher.close_ticks("hyperliquid", "kPEPE"), "nothing left to close");
    assert_eq!(watcher.price("hyperliquid", "kPEPE"), None, "its price is forgotten");
    assert_eq!(seen(&log), vec!["quotes kPEPE -> 1", "unsubscribe 1"]);

    watcher.subscribe_ticks("kPEPE", 1.0).unwrap();
    assert_eq!(seen(&log).last().map(String::as_str), Some("quotes kPEPE -> 2"));
    log.lock().unwrap().clear();
    feed.shutdown();
    assert_eq!(
        seen(&log),
        vec!["unsubscribe 2", "shutdown"],
        "the closed lane is not released twice"
    );
}
