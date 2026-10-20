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

/// oanda's variant hands the door its handle too: a foreign OANDA position has a mark only through it.
#[test]
fn the_oanda_variant_hands_out_a_price_watcher() {
    let (feed, _, _) = feed();
    assert!(LiveFeeds::Oanda(feed).price_watcher().is_some());
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

/// **ibkr's price-only feed is lazy and prices only what is real-time** (any-symbol price lane): nothing is
/// subscribed when the feed is built, the door's watcher opens ONE quote lane per stock however often it asks,
/// a delayed stream is refused by name through the door's handle, and only a two-sided quote of a stream
/// labelled real-time is a price.
#[test]
fn the_ibkr_variant_is_lazy_and_its_watcher_prices_only_a_real_time_quote() {
    const AAPL: &str = "AAPL.SMART.USD";
    // By the arm's own const, not a literal: this file is folded into `feeds.rs` by `new_venue_gate`, which
    // reads a file naming the whole roster as a per-venue table.
    const IBKR: &str = super::ibkr::VENUE;
    let log = Arc::new(Mutex::new(Vec::new()));
    let tap = Arc::new(PriceTap::real_time_only());
    let tracked = Tracked::new(Box::new(Recording { log: Arc::clone(&log), next: 0 }));
    let feed = LiveFeeds::Ibkr(QuoteFeed::new(tracked, Arc::clone(&tap)));
    let watcher = feed.price_watcher().expect("ibkr's feed hands out a price watcher");
    assert!(seen(&log).is_empty(), "nothing is subscribed until the door asks");

    watcher.subscribe_ticks(AAPL, 0.01).expect("subscribed");
    watcher.subscribe_ticks(AAPL, 0.01).expect("idempotent");
    assert_eq!(seen(&log), vec![format!("quotes {AAPL} -> 1")], "one quote lane, nothing else");
    assert_eq!(watcher.door_price(IBKR, AAPL), Ok(None), "no label, no quote yet");

    let sink = TapSink::new(Arc::new(NoopSink) as Arc<dyn LiveDataSink>, tap);
    let quote = QuoteTick {
        ts: 1,
        local_ts: 0,
        bid: 149.0,
        ask: 151.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    sink.data_delay(IBKR, AAPL, "quotes", vike_model::DataDelay::DELAYED);
    sink.quote(IBKR, AAPL, quote.clone());
    let why = watcher.door_price(IBKR, AAPL).expect_err("delayed is refused by name");
    assert!(why.contains("delayed"), "{why}");

    sink.data_delay(IBKR, AAPL, "quotes", vike_model::DataDelay::RealTime);
    sink.quote(IBKR, AAPL, quote);
    assert_eq!(watcher.door_price(IBKR, AAPL), Ok(Some(150.0)));
}

/// **dukascopy's price-only feed is lazy and prices a pair from its two-sided quote** (any-symbol price lane):
/// nothing is subscribed when the feed is built, the door's watcher opens ONE quote lane per pair however often it
/// asks, and the quote it delivers is the price (the gatewayhub's stream has already judged it real-time).
#[test]
fn the_dukascopy_variant_is_lazy_and_its_watcher_prices_a_pairs_quote() {
    const EURUSD: &str = "EURUSD";
    // By the arm's own const, not a literal: this file is folded into `feeds.rs` by `new_venue_gate`, which reads a
    // file naming the whole roster as a per-venue table.
    const DUKASCOPY: &str = super::dukascopy::VENUE;
    let (feed, log, tap) = feed();
    let feed = LiveFeeds::Dukascopy(feed);
    let watcher = feed.price_watcher().expect("dukascopy's feed hands out a price watcher");
    assert!(seen(&log).is_empty(), "nothing is subscribed until the door asks");

    watcher.subscribe_ticks(EURUSD, 0.00001).expect("subscribed");
    watcher.subscribe_ticks(EURUSD, 0.00001).expect("idempotent");
    assert_eq!(seen(&log), vec![format!("quotes {EURUSD} -> 1")], "one quote lane, nothing else");
    assert_eq!(watcher.price(DUKASCOPY, EURUSD), None, "no quote yet");

    let sink = TapSink::new(Arc::new(NoopSink) as Arc<dyn LiveDataSink>, tap);
    let quote = QuoteTick {
        ts: 1,
        local_ts: 0,
        bid: 1.1,
        ask: 1.3,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    sink.quote(DUKASCOPY, EURUSD, quote);
    assert_eq!(watcher.price(DUKASCOPY, EURUSD), Some((1.1 + 1.3) / 2.0));
    assert_eq!(watcher.price(DUKASCOPY, "GBPUSD"), None, "another pair is another price");
}

/// **fxcm's price-only feed is lazy and prices a pair from its two-sided quote** (any-symbol price lane, the twin
/// of dukascopy's): nothing is subscribed when the feed is built, the door's watcher opens ONE quote lane per pair
/// however often it asks, and the quote it delivers is the price (the gatewayhub's stream has already judged it a
/// live quote of an open market). A pair nobody asked for has no price.
#[test]
fn the_fxcm_variant_is_lazy_and_its_watcher_prices_a_pairs_quote() {
    const USDJPY: &str = "USDJPY";
    // By the arm's own const, not a literal (see the dukascopy test above).
    const FXCM: &str = super::fxcm::VENUE;
    let (feed, log, tap) = feed();
    let feed = LiveFeeds::Fxcm(feed);
    let watcher = feed.price_watcher().expect("fxcm's feed hands out a price watcher");
    assert!(seen(&log).is_empty(), "nothing is subscribed until the door asks");

    watcher.subscribe_ticks(USDJPY, 0.001).expect("subscribed");
    watcher.subscribe_ticks(USDJPY, 0.001).expect("idempotent");
    assert_eq!(seen(&log), vec![format!("quotes {USDJPY} -> 1")], "one quote lane, nothing else");
    assert_eq!(watcher.price(FXCM, USDJPY), None, "no quote yet");

    let sink = TapSink::new(Arc::new(NoopSink) as Arc<dyn LiveDataSink>, tap);
    let quote = QuoteTick {
        ts: 1,
        local_ts: 0,
        bid: 150.08,
        ask: 150.12,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    sink.quote(FXCM, USDJPY, quote);
    assert_eq!(watcher.price(FXCM, USDJPY), Some((150.08 + 150.12) / 2.0));
    assert_eq!(watcher.price(FXCM, "EURUSD"), None, "another pair is another price");
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

// ---------------------------------------------------------------------------------------------
// The dial is lent out for a subscribe round trip, never locked across it; a capped feed remembers
// a failed subscribe
// ---------------------------------------------------------------------------------------------

/// How long a test waits for something that should be immediate before it calls it blocked.
const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A dial whose `SLOW` quote subscription is a round trip the test holds open: it says it began (`entered`)
/// and answers only when told (`go`: `true` = subscribed, `false` = refused). `BAD` is always refused. Every
/// other symbol answers at once.
struct Scripted {
    log: Arc<Mutex<Vec<String>>>,
    next: u64,
    entered: std::sync::mpsc::Sender<()>,
    go: std::sync::mpsc::Receiver<bool>,
}

impl DataClient for Scripted {
    fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn subscribe_quotes(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.log.lock().unwrap().push(format!("quotes {s}"));
        let refused = || Err(LiveDataError::Subscribe("login refused".to_string()));
        if s == "BAD" {
            return refused();
        }
        if s == "SLOW" {
            self.entered.send(()).unwrap();
            if !self.go.recv().unwrap_or(false) {
                return refused();
            }
        }
        self.next += 1;
        Ok(SubscriptionId(self.next))
    }
    fn subscribe_trades(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn subscribe_book(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.log.lock().unwrap().push(format!("unsubscribe {}", id.0));
    }
    fn shutdown(&mut self) {
        self.log.lock().unwrap().push("shutdown".to_string());
    }
}

/// What [`scripted`] hands the test: the feed, the dial's log, the "round trip began" signal and the answer.
type ScriptedFeed = (
    QuoteFeed,
    Arc<Mutex<Vec<String>>>,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<bool>,
);

/// A feed over a [`Scripted`] dial; `memory` = the failure memory of a capped feed (`None`: an uncapped one).
fn scripted(memory: Option<std::time::Duration>) -> ScriptedFeed {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (entered, entered_rx) = std::sync::mpsc::channel();
    let (go_tx, go) = std::sync::mpsc::channel();
    let dial = Scripted { log: Arc::clone(&log), next: 0, entered, go };
    let feed = QuoteFeed::new(Tracked::new(Box::new(dial)), Arc::new(PriceTap::default()));
    let feed = match memory {
        Some(ttl) => feed.with_lane_limits(4, ttl),
        None => feed,
    };
    (feed, log, entered_rx, go_tx)
}

/// Run `f` on another thread and fail, rather than hang, when it does not come back: it was queued behind
/// a round trip it must not wait for.
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(WAIT).expect("blocked behind a subscribe round trip")
}

fn subscribe_on_a_thread(
    watcher: &super::CexWatcher,
    symbol: &'static str,
) -> std::thread::JoinHandle<Result<(), LiveDataError>> {
    let watcher = watcher.clone();
    std::thread::spawn(move || watcher.subscribe_ticks(symbol, 1.0))
}

/// **A close and a shutdown never wait for a subscribe round trip** (a first ctrader handshake is ~12 s, an
/// IG subscribe is a login). While `SLOW` is out at the venue the reaper's close of `FAST` and the feed's
/// shutdown both return at once; the close is applied, and the dial closed, when the round trip gives the
/// dial back, in the order a shutdown keeps (lanes first, then the dial), and the lane the late subscribe
/// returned is released too.
#[test]
fn a_close_and_a_shutdown_never_wait_for_a_subscribe_round_trip() {
    let (feed, log, entered, go) = scripted(None);
    let watcher = feed.watcher();
    watcher.subscribe_ticks("FAST", 1.0).unwrap();
    let slow = subscribe_on_a_thread(&watcher, "SLOW");
    entered.recv_timeout(WAIT).expect("the round trip began");

    let w = watcher.clone();
    assert!(within(move || w.close_ticks("ig", "FAST")), "the lane was closed");
    let feed = within(move || {
        let mut feed = feed;
        feed.shutdown();
        feed
    });
    assert_eq!(seen(&log), vec!["quotes FAST", "quotes SLOW"], "the dial was out: nothing yet");

    go.send(true).unwrap();
    slow.join().unwrap().expect("a subscribe that outlived the shutdown is not an error");
    assert_eq!(
        seen(&log),
        vec!["quotes FAST", "quotes SLOW", "unsubscribe 1", "unsubscribe 2", "shutdown"],
        "the deferred close, the late lane, then the dial"
    );
    drop(feed);
    assert_eq!(seen(&log).len(), 5, "nothing is released twice");
}

/// **Another subscriber still queues for the dial** (the feed has ONE `&mut` dial) and lands after the first:
/// the round trip is out of the lock, not parallel.
#[test]
fn a_second_subscriber_waits_for_the_dial_and_then_lands() {
    let (mut feed, log, entered, go) = scripted(None);
    let watcher = feed.watcher();
    let first = subscribe_on_a_thread(&watcher, "SLOW");
    entered.recv_timeout(WAIT).expect("the round trip began");
    let second = subscribe_on_a_thread(&watcher, "OTHER");
    go.send(true).unwrap();
    first.join().unwrap().expect("first");
    second.join().unwrap().expect("second");
    assert_eq!(seen(&log), vec!["quotes SLOW", "quotes OTHER"]);
    feed.shutdown();
    assert_eq!(seen(&log)[2..], ["unsubscribe 1", "unsubscribe 2", "shutdown"]);
}

/// **A failed subscribe on a capped feed is remembered**, per venue: while the memory lasts a retry is
/// refused with the failure's words and reaches no dial (each attempt is an IG login, and a failing one is
/// no lane, so the cap never counted it); an UNCAPPED feed and an expired memory ask the dial each time, as
/// before.
#[test]
fn a_capped_feed_remembers_a_failed_subscribe_so_a_retry_spends_no_login() {
    let (feed, log, _, _) = scripted(Some(std::time::Duration::from_secs(3600)));
    let watcher = feed.watcher();
    watcher.subscribe_ticks("BAD", 1.0).expect_err("the login is refused");
    let again = watcher.subscribe_ticks("BAD", 1.0).expect_err("remembered");
    assert!(format!("{again:?}").contains("login refused"), "{again:?}");
    assert!(format!("{again:?}").contains("not retried"), "{again:?}");
    watcher.subscribe_ticks("GOOD", 1.0).expect_err("the memory is the venue's, not the symbol's");
    assert_eq!(seen(&log), vec!["quotes BAD"], "only the first attempt reached the venue");

    for memory in [Some(std::time::Duration::ZERO), None] {
        let (feed, log, _, _) = scripted(memory);
        let watcher = feed.watcher();
        watcher.subscribe_ticks("BAD", 1.0).expect_err("refused");
        watcher.subscribe_ticks("BAD", 1.0).expect_err("refused again");
        assert_eq!(seen(&log).len(), 2, "{memory:?}: every attempt asks the venue");
    }
}

/// **A subscriber queued behind a round trip that fails does not spend a login of its own**: it re-reads
/// the memory once it has the dial.
#[test]
fn a_subscriber_queued_behind_a_failing_round_trip_is_refused_by_its_memory() {
    let (feed, log, entered, go) = scripted(Some(std::time::Duration::from_secs(3600)));
    let watcher = feed.watcher();
    let first = subscribe_on_a_thread(&watcher, "SLOW");
    entered.recv_timeout(WAIT).expect("the round trip began");
    let second = subscribe_on_a_thread(&watcher, "OTHER");
    go.send(false).unwrap();
    first.join().unwrap().expect_err("the login is refused");
    let refused = second.join().unwrap().expect_err("refused by the memory");
    assert!(format!("{refused:?}").contains("not retried"), "{refused:?}");
    assert_eq!(seen(&log), vec!["quotes SLOW"], "the queued subscriber reached no dial");
}

/// **The cap's refusal says plainly which lanes the reaper frees**: a fifth epic is refused while the cap's
/// lanes are open, and the words name that only a symbol no account admitted is reaped, an admitted one
/// staying until its engine stops or the node restarts.
#[test]
fn the_cap_refusal_says_an_admitted_lane_stays_until_the_engine_stops_or_the_node_restarts() {
    let (uncapped, _, _) = feed();
    let capped = uncapped.with_lane_cap(1);
    let watcher = capped.watcher();
    watcher.subscribe_ticks("CS.D.AAA.MINI.IP", 1.0).expect("first");
    let err = watcher.subscribe_ticks("CS.D.BBB.MINI.IP", 1.0).expect_err("past the cap");
    let words = format!("{err:?}");
    assert!(words.contains("at most 1 extra quote lanes"), "{words}");
    assert!(words.contains("no account admitted"), "{words}");
    assert!(words.contains("engine stops or the node restarts"), "{words}");
}

// ---------------------------------------------------------------------------------------------
// A NEW OANDA pair whose pricing stream is silent: the REST first price (the bridge's
// `market_feed/seed.rs`, served by the gatewayhub's feed) is what the door's price wait reads
// ---------------------------------------------------------------------------------------------

/// The gatewayhub side of a new OANDA pair as the door sees it through the datahub: opening the quote
/// lane starts a pricing stream that NEVER delivers (the hung open of the evidence) and, beside it, the
/// REST first price, which lands `after` later carrying the VENUE's own time (seconds in the past).
/// Nothing else ticks. The bridge cannot be linked here (the tradehub links no bridge), so the seed is
/// what the bridge's loopback tests pin, delivered through the same `LiveDataSink` seam.
struct SilentStreamWithSeed {
    sink: Arc<dyn LiveDataSink>,
    seed: QuoteTick,
    after: std::time::Duration,
    next: u64,
}

impl DataClient for SilentStreamWithSeed {
    fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn subscribe_quotes(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        let (sink, seed, after) = (Arc::clone(&self.sink), self.seed.clone(), self.after);
        let symbol = s.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            sink.quote(SEEDED_VENUE, &symbol, seed);
        });
        self.next += 1;
        Ok(SubscriptionId(self.next))
    }
    fn subscribe_trades(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn subscribe_book(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn unsubscribe(&mut self, _: SubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// A venue label of the test's own (the tap keys by it; nothing here depends on the real venue's name).
const SEEDED_VENUE: &str = "seeded-venue";

/// The door's price source over a feed's watcher: what `LeaseFacts::price` does for a venue's feed,
/// subscribe on the ask and read the tap, with nothing else around it.
struct WatcherDoor(super::CexWatcher);

impl crate::server::symbol_admission::SymbolFacts for WatcherDoor {
    fn admission(&self, _: &str, _: &str, _: &str) -> Result<vike_exec::SymbolAdmission, String> {
        unreachable!("the price wait asks for a price only")
    }
    fn price(
        &self,
        _: &str,
        venue: &str,
        symbol: &str,
        subscribe: bool,
    ) -> Result<Option<f64>, String> {
        if subscribe {
            self.0.subscribe_ticks(symbol, 0.00001).map_err(|e| format!("{e:?}"))?;
        }
        self.0.door_price(venue, symbol)
    }
    fn generation(&self, _: &str) -> u64 {
        0
    }
}

/// **An order on a NEW OANDA pair, its stream silent and its REST first price landing, is priced
/// without hitting the door's hold.** The tick carries the venue's time five seconds old, and the door
/// accepts it (the tap ages a price from its ARRIVAL, which is why the bridge bounds the seed's own age).
/// The stream's tick, when it comes, replaces it.
#[test]
fn an_order_on_a_new_oanda_pair_with_a_silent_stream_is_priced_by_the_rest_seed_inside_the_hold() {
    use crate::server::symbol_admission::{PRICE_WAIT, PriceWait, await_price};
    let quote = |bid: f64, ask: f64, age_ms: i64| QuoteTick {
        ts: vike_model::now_ms() - age_ms,
        local_ts: 0,
        bid,
        ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: String::new(),
    };
    let seed = quote(146.50, 146.52, 5_000);
    let tap = Arc::new(PriceTap::default());
    let idle = Arc::new(NoopSink) as Arc<dyn LiveDataSink>;
    let sink = Arc::new(TapSink::new(idle, Arc::clone(&tap))) as Arc<dyn LiveDataSink>;
    let dial = SilentStreamWithSeed {
        sink: Arc::clone(&sink),
        seed: seed.clone(),
        after: std::time::Duration::from_millis(300),
        next: 0,
    };
    let feed = LiveFeeds::Oanda(QuoteFeed::new(Tracked::new(Box::new(dial)), tap));
    let watcher = feed.price_watcher().expect("oanda's feed hands out a price watcher");
    let wait = PriceWait { route_key: "seeded#1".into(), venue: SEEDED_VENUE.into() };

    let started = std::time::Instant::now();
    let px = await_price(&WatcherDoor(watcher.clone()), &wait, "USDJPY")
        .expect("the REST first price prices the order");
    assert_eq!(px, seed.mid());
    assert!(
        started.elapsed() < PRICE_WAIT / 2,
        "the order was held {:?} of the {PRICE_WAIT:?} hold for a silent stream",
        started.elapsed()
    );

    let stream = quote(146.60, 146.62, 0);
    sink.quote(SEEDED_VENUE, "USDJPY", stream.clone());
    assert_eq!(watcher.price(SEEDED_VENUE, "USDJPY"), Some(stream.mid()), "the stream takes over");
}
