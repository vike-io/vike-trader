//! An idle price pump under an open venue gap, for the credentialed FX venues' price-only feeds
//! (`feeds/credentialed_price_only.rs`): the reaper closes it and ENDS the gap with it, so the core's link
//! dead-man window for the symbol closes instead of tripping.

use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};
use vike_model::StreamStatus;

use super::*;
use crate::feeds::price_tap::TapSink;
use crate::feeds::{PriceTap, QuoteFeed, Tracked};

/// A scripted dial: it records the quote lanes the feed opens and closes, and fails nothing.
struct Lanes(Arc<Mutex<Vec<String>>>, u64);

impl DataClient for Lanes {
    fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.1 += 1;
        self.0.lock().unwrap().push(format!("quotes {symbol}"));
        Ok(SubscriptionId(self.1))
    }
    fn subscribe_trades(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn subscribe_book(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        unreachable!("the door subscribes quotes only")
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.0.lock().unwrap().push(format!("unsubscribe {}", id.0));
    }
    fn shutdown(&mut self) {}
}

/// The core's side of the tap: every `stream_status` it was handed, as `(symbol, stream, status)`.
#[derive(Default)]
struct CoreStatuses(Mutex<Vec<(String, String, StreamStatus)>>);

impl LiveDataSink for CoreStatuses {
    fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<vike_marketdata::Bar>) {}
    fn close_bar(&self, _: &str, _: &str, _: &str, _: vike_marketdata::Bar) {}
    fn forming_bar(&self, _: &str, _: &str, _: &str, _: vike_marketdata::Bar) {}
    fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
    fn quote(&self, _: &str, _: &str, _: vike_marketdata::QuoteTick) {}
    fn trade(&self, _: &str, _: &str, _: vike_marketdata::TradeTick) {}
    fn book(&self, _: &str, _: &str, _: Arc<vike_marketdata::L2Book>) {}
    fn stream_status(&self, _: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.0.lock().unwrap().push((symbol.to_string(), stream.to_string(), status));
    }
}

fn key(venue: &str, symbol: &str) -> (String, String) {
    (venue.to_string(), symbol.to_string())
}

fn entry(facts: &LeaseFacts, venue: &str, symbol: &str) -> Option<Sub> {
    facts.watched.lock().unwrap().get(&key(venue, symbol)).copied()
}

/// What [`idle_pump_under_a_gap`] builds.
struct IdlePump {
    facts: LeaseFacts,
    /// Kept alive: the feed owns the dial the lane was opened on.
    feed: QuoteFeed,
    tap: Arc<PriceTap>,
    /// The dial's log: the lanes opened and closed.
    log: Arc<Mutex<Vec<String>>>,
    /// What the core was told: the `GapStart` that opened the gap, then whatever closed it.
    core: Arc<CoreStatuses>,
}

/// A registry whose `venue` has a price feed (at most `cap` extra lanes) with `symbol`'s quote lane open and a
/// VENUE-source gap open on it (a stream whose dial keeps failing: `GapStart` from every refused open, never a
/// `Live`) that the CORE was told about through the tap's sink, and the door's entry for it idle since long
/// enough to be reaped.
fn idle_pump_under_a_gap(venue: &str, symbol: &str, cap: usize) -> IdlePump {
    let log = Arc::new(Mutex::new(Vec::new()));
    let core = Arc::new(CoreStatuses::default());
    let tap = Arc::new(PriceTap::default());
    let sink = TapSink::new(Arc::clone(&core) as Arc<dyn LiveDataSink>, Arc::clone(&tap));
    let feed = QuoteFeed::new(Tracked::new(Box::new(Lanes(Arc::clone(&log), 0))), Arc::clone(&tap))
        .with_lane_cap(cap);
    let watcher = feed.watcher();
    watcher.subscribe_ticks(symbol, 0.00001).expect("the lane opens");
    sink.stream_status(venue, symbol, "quotes", StreamStatus::GapStart { at_ts_ms: 1 });
    assert!(watcher.gap_open(venue, symbol));
    let facts = LeaseFacts::default();
    facts.bind_prices(HashMap::from([(venue.to_string(), watcher)]));
    let long_ago = Instant::now().checked_sub(IDLE_TTL + Duration::from_secs(1)).unwrap();
    facts.watched.lock().unwrap().insert(key(venue, symbol), Sub::Idle(long_ago));
    IdlePump { facts, feed, tap, log, core }
}

/// **An epic whose Lightstreamer session cannot connect does not keep one of ig's four lanes for ever.** The
/// reaper closes the idle pump, the lane is unsubscribed (its redials stop and its IG login is given back), the
/// gap is ended and the tap forgets it, and the freed lane lets the next epic open. Held by the gap, a mistyped
/// epic would have starved the price-only feed of its capped lanes.
#[test]
fn an_idle_ig_lane_under_a_gap_is_reaped_and_its_place_is_given_back() {
    let IdlePump { facts, feed, tap, log, .. } =
        idle_pump_under_a_gap("ig", "CS.D.EURUSD.MINI.IP", 1);
    let watcher = feed.watcher();
    assert!(
        watcher.subscribe_ticks("CS.D.GBPUSD.MINI.IP", 0.00001).is_err(),
        "the only lane is held until the reaper frees it"
    );
    assert_eq!(facts.close_idle_pumps(), 1, "the place was freed");
    assert_eq!(entry(&facts, "ig", "CS.D.EURUSD.MINI.IP"), None);
    assert_eq!(
        *log.lock().unwrap(),
        ["quotes CS.D.EURUSD.MINI.IP".to_string(), "unsubscribe 1".to_string()],
        "the lane was closed, so the session stops redialling"
    );
    assert!(!tap.gap_open("ig", "CS.D.EURUSD.MINI.IP"), "a re-subscribe starts clean");
    watcher.subscribe_ticks("CS.D.GBPUSD.MINI.IP", 0.00001).expect("the freed lane is reusable");
}

/// **An ADMITTED epic's lane is the engine's, never the reaper's, and the cap's refusal says so**: the lane of
/// an epic an account admitted prices that engine's position, so `close_idle_pumps` leaves it open (the node
/// cannot see from here that the engine holds no order and no position) and it keeps its place under ig's four
/// until the account's engine stops (`engine_stopped` hands the epic to the reaper) or the node restarts.
#[test]
fn an_admitted_ig_lane_keeps_its_place_until_its_engine_stops() {
    let epic = "CS.D.EURUSD.MINI.IP";
    let IdlePump { facts, feed, log, core, .. } = idle_pump_under_a_gap("ig", epic, 1);
    facts.set_idle_ttl(Duration::ZERO);
    let watcher = feed.watcher();
    facts.admitted("ig", "ig", epic);
    assert_eq!(entry(&facts, "ig", epic), None, "an admitted epic is not the door's to count");
    assert_eq!(facts.close_idle_pumps(), 0, "nothing idle to reap");
    let err = watcher.subscribe_ticks("CS.D.GBPUSD.MINI.IP", 0.00001).expect_err("the cap");
    assert!(
        format!("{err:?}").contains("engine stops or the node restarts"),
        "the words promise only what happens: {err:?}"
    );
    assert_eq!(*log.lock().unwrap(), ["quotes CS.D.EURUSD.MINI.IP".to_string()]);
    assert_eq!(
        core.0.lock().unwrap().len(),
        1,
        "the admitted epic's gap is still the core's to hear"
    );

    facts.engine_stopped("ig");
    assert_eq!(facts.close_idle_pumps(), 1, "the stopped engine's epic is the reaper's again");
    watcher.subscribe_ticks("CS.D.GBPUSD.MINI.IP", 0.00001).expect("the lane was freed");
}

/// **A venue the dead-man ARMS gets its pump closed under a gap too, and the core's window closes with it**
/// (A55): ctrader's row is `Inert`, which `AdapterLinkDisclosure::StratLinkOnly` arms for the two hub links, so
/// the core holds a link dead-man window for an open gap, and a window that nobody closes trips after its grace
/// (cancel and a HALT of the venue). The reaper used to keep such a pump for as long as the gap stayed open, which
/// held one of the node's [`MAX_WATCHED`] places for ever; now it closes the pump and says `Live` for the symbol,
/// on the stream that opened the gap, so the window closes.
#[test]
fn an_idle_ctrader_pump_under_a_gap_is_closed_and_its_dead_man_window_with_it() {
    let IdlePump { facts, tap, log, core, .. } = idle_pump_under_a_gap("ctrader", "GBPUSD", 8);
    assert_eq!(
        *core.0.lock().unwrap(),
        [("GBPUSD".to_string(), "quotes".to_string(), StreamStatus::GapStart { at_ts_ms: 1 })],
        "the core holds the window the gap opened"
    );

    assert_eq!(facts.close_idle_pumps(), 1, "the place was freed");
    assert_eq!(entry(&facts, "ctrader", "GBPUSD"), None);
    assert_eq!(
        *log.lock().unwrap(),
        ["quotes GBPUSD".to_string(), "unsubscribe 1".to_string()],
        "the lane was closed"
    );
    assert!(!tap.gap_open("ctrader", "GBPUSD"));
    assert_eq!(
        core.0.lock().unwrap()[1..],
        [(
            "GBPUSD".to_string(),
            "quotes".to_string(),
            StreamStatus::Live { gap_started_ts_ms: Some(1) }
        )],
        "the core was told the stream is over: its window closes instead of tripping"
    );
}
