//! **Every tap-fronted arm's dial, pinned per arm** (`feeds.rs`'s `dial`, which ten copies became): which
//! venue and network the arm asks the [`FeedCtors`] seam for, that `wrap` runs once and BEFORE the seam is
//! asked and its sink is the one handed over, and which [`PriceTap`] the arm put in front of the core (the
//! plain tap, the venue-clocked one or the labelled-quotes one), told apart by how each prices the same
//! three deliveries.

use std::sync::{Arc, Mutex};

use vike_data::{DataClient, LiveDataError, LiveDataSink, SubscriptionId};
use vike_marketdata::QuoteTick;
use vike_model::DataDelay;
use vike_tradehub_core::MakerConfig;

use super::{CexWatcher, FeedCtors, LiveFeeds, PriceTap};
use crate::HlNetwork;

/// The symbol every probe prices: no venue's lane twin, so the door never waits for a twin mark.
const SYM: &str = "PIN-SYM";

/// A client that hands out ids and does nothing else.
struct Quiet(u64);

impl Quiet {
    fn id(&mut self) -> Result<SubscriptionId, LiveDataError> {
        self.0 += 1;
        Ok(SubscriptionId(self.0))
    }
}

impl DataClient for Quiet {
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
    fn unsubscribe(&mut self, _id: SubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// The seam double: logs every ask, keeps the sink it was handed.
#[derive(Default)]
struct Asked {
    log: Arc<Mutex<Vec<String>>>,
    sink: Mutex<Option<Arc<dyn LiveDataSink>>>,
}

impl FeedCtors for Asked {
    crate::testutil::scripted_strat!();
    fn client(
        &self,
        venue: &str,
        network: Option<&str>,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        self.log.lock().unwrap().push(format!("client {venue} {network:?}"));
        *self.sink.lock().unwrap() = Some(sink);
        Ok(Box::new(Quiet(0)))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Tap {
    Plain,
    VenueClocked,
    Labelled,
}

/// Which tap stands between `sink` and `watcher`: a quote the venue stamped long ago, a fresh one with no
/// stream label, then a fresh one after a real-time label. The plain tap prices all three (it ages from
/// arrival), the venue-clocked one refuses the first, the labelled-quotes one prices only the third.
fn tap_kind(sink: &Arc<dyn LiveDataSink>, watcher: &CexWatcher, venue: &str) -> Tap {
    let now = vike_model::now_ms();
    let quote = |ts: i64| QuoteTick {
        ts,
        local_ts: now,
        bid: 1.0,
        ask: 3.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: SYM.to_string(),
    };
    let priced = || watcher.door_price(venue, SYM) == Ok(Some(2.0));
    sink.quote(venue, SYM, quote(1));
    let old = priced();
    sink.quote(venue, SYM, quote(now));
    let fresh = priced();
    sink.data_delay(venue, SYM, "quotes", DataDelay::RealTime);
    sink.quote(venue, SYM, quote(now));
    let labelled = priced();
    match (old, fresh, labelled) {
        (true, true, true) => Tap::Plain,
        (false, true, true) => Tap::VenueClocked,
        (false, false, true) => Tap::Labelled,
        other => panic!("{venue}: no tap prices like {other:?}"),
    }
}

/// A live core to hand the arms (its three lanes are what the dial's `CoreLaneSink` sends on).
fn core() -> vike_core::CoreHandle {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
    use vike_model::RiskLimits;
    let engine = ExecutionEngine::new(
        Account::new(1.0, "paper", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "paper",
        "X",
    );
    vike_core::spawn_core(engine, vike_core::CoreConfig::default())
}

/// An arm under test: build its feed on the core, through `wrap` and the seam, and hand back its price handle.
type Build<'a> = dyn Fn(
        &vike_core::CoreHandle,
        &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
        &dyn FeedCtors,
    ) -> Result<CexWatcher, String>
    + 'a;

/// Build one arm's feed through `build` and assert what its dial asked (`wrap` once, then the seam for
/// `venue` on `network`, handed the sink `wrap` returned) and which tap it chose.
fn check(
    handle: &vike_core::CoreHandle,
    venue: &str,
    network: Option<&str>,
    tap: Tap,
    build: &Build<'_>,
) {
    let asked = Asked::default();
    let wrapped: Mutex<Option<Arc<dyn LiveDataSink>>> = Mutex::new(None);
    let log = Arc::clone(&asked.log);
    let wrap = |sink: Arc<dyn LiveDataSink>| {
        log.lock().unwrap().push("wrap".to_string());
        *wrapped.lock().unwrap() = Some(Arc::clone(&sink));
        sink
    };
    let watcher = build(handle, &wrap, &asked).unwrap_or_else(|e| panic!("{venue}: {e}"));
    let handed = asked.sink.lock().unwrap().clone().expect("the seam was asked");
    let wrapped = wrapped.lock().unwrap().clone().expect("wrap ran");
    assert!(Arc::ptr_eq(&handed, &wrapped), "{venue}: the seam gets the sink `wrap` returned");
    assert_eq!(
        asked.log.lock().unwrap().clone(),
        vec!["wrap".to_string(), format!("client {venue} {network:?}")],
        "{venue}: what the dial asked, in order"
    );
    assert_eq!(tap_kind(&handed, &watcher, venue), tap, "{venue}: the tap in front of the core");
}

fn watcher_of(feed: Result<LiveFeeds, String>) -> Result<CexWatcher, String> {
    feed.map(|f| f.price_watcher().expect("a price-only feed hands out a watcher"))
}

/// **The price-only feeds**: each venue's dial, network and tap.
#[test]
fn every_price_only_feed_dials_its_own_venue_with_its_own_tap() {
    let handle = core();
    check(&handle, "ctrader", None, Tap::Plain, &|h, w, m| {
        watcher_of(super::wire_ctrader_price_feed(h, w, m))
    });
    check(&handle, "ig", None, Tap::Plain, &|h, w, m| {
        watcher_of(super::wire_ig_price_feed(h, w, m))
    });
    check(&handle, "deribit", None, Tap::Plain, &|h, w, m| {
        watcher_of(super::wire_deribit_price_feed(h, w, m))
    });
    check(&handle, "oanda", None, Tap::VenueClocked, &|h, w, m| {
        watcher_of(super::wire_oanda_price_feed(h, w, m))
    });
    check(&handle, "hyperliquid", Some("testnet"), Tap::Plain, &|h, w, m| {
        watcher_of(super::wire_hl_price_feed(&HlNetwork::Testnet, h, w, m))
    });
    check(&handle, "hyperliquid", None, Tap::Plain, &|h, w, m| {
        watcher_of(super::wire_hl_price_feed(&HlNetwork::Mainnet, h, w, m))
    });
    check(&handle, super::ibkr::VENUE, None, Tap::Labelled, &|h, w, m| {
        watcher_of(super::wire_ibkr_price_feed(h, w, m))
    });
    check(&handle, super::dukascopy::VENUE, None, Tap::Plain, &|h, w, m| {
        watcher_of(super::wire_dukascopy_price_feed(h, w, m))
    });
    check(&handle, super::fxcm::VENUE, None, Tap::VenueClocked, &|h, w, m| {
        watcher_of(super::wire_fxcm_price_feed(h, w, m))
    });
    check(&handle, super::polymarket::VENUE, None, Tap::Plain, &|h, w, m| {
        watcher_of(super::wire_polymarket_price_feed(h, w, m))
    });
    handle.shutdown_and_join();
}

/// **The mounted arms that start from the same dial**: alpaca, ig, deribit, oanda and Hyperliquid ask for
/// their own venue (Hyperliquid on its network) with the tap their price-only twin uses.
#[test]
fn every_mounted_arm_on_the_dial_asks_for_its_venue_with_its_tap() {
    let handle = core();
    let cfg = |venue: &str| MakerConfig::crypto(venue, SYM, 0.01, 1.0);
    let (alpaca, ig, deribit, oanda, hl) =
        (cfg("alpaca"), cfg("ig"), cfg("deribit"), cfg("oanda"), cfg("hyperliquid"));
    let live = std::collections::HashSet::new();
    check(&handle, "alpaca", None, Tap::Plain, &|h, w, m| {
        super::wire_alpaca_feed(&[&alpaca], h, w, m).map(|f| f.watcher())
    });
    check(&handle, "ig", None, Tap::Plain, &|h, w, m| {
        super::wire_ig_feed(&[&ig], h, w, m).map(|f| f.watcher())
    });
    check(&handle, "deribit", None, Tap::Plain, &|h, w, m| {
        super::wire_deribit_feed(&[&deribit], h, w, m).map(|f| f.watcher())
    });
    check(&handle, "oanda", None, Tap::VenueClocked, &|h, w, m| {
        super::wire_oanda_feed(&[&oanda], h, w, m).map(|f| f.watcher())
    });
    check(&handle, "hyperliquid", Some("testnet"), Tap::Plain, &|h, w, m| {
        super::wire_hyperliquid_feed(&HlNetwork::Testnet, &hl, &[&hl], h, &live, w, m)
            .map(|f| f.watcher())
    });
    handle.shutdown_and_join();
}

/// A client whose quote lane refuses every ask with its text, counting the asks.
struct RefusesQuotes(String, Arc<Mutex<u32>>);

impl DataClient for RefusesQuotes {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(1))
    }
    fn subscribe_quotes(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        *self.1.lock().unwrap() += 1;
        Err(LiveDataError::Subscribe(self.0.clone()))
    }
    fn subscribe_trades(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(1))
    }
    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(1))
    }
    fn unsubscribe(&mut self, _id: SubscriptionId) {}
    fn shutdown(&mut self) {}
}

/// A seam that waits for nothing, so three tries take no time.
struct NoWait;

impl FeedCtors for NoWait {
    crate::testutil::scripted_strat!();
    fn client(
        &self,
        venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        Err(format!("{venue}: this seam dials nothing"))
    }
    fn pause(&self, _d: std::time::Duration) -> bool {
        false
    }
}

/// **The two quote verbs of an arm, for any venue**: the refusal names the venue the arm passed, a TRANSIENT one
/// is asked three times, a CAP is asked once and only `subscribe_quotes_surviving` carries it, and any other
/// refusal is returned at once by both. OANDA's own arm is pinned through the real start in `feed_outage.rs`.
#[test]
fn the_quote_verbs_name_their_venue_and_only_the_surviving_one_carries_a_cap() {
    use vike_market_stream::market::{CAP_MARK, TRANSIENT_MARK};
    let asked_of = |refusal: &str, verb: &dyn Fn(&mut super::Tracked) -> Result<(), String>| {
        let asked = Arc::new(Mutex::new(0));
        let inner = RefusesQuotes(refusal.to_string(), Arc::clone(&asked));
        let mut feed = super::Tracked::new(Box::new(inner));
        let answer = verb(&mut feed);
        let asks = *asked.lock().unwrap();
        (answer, asks)
    };
    let surviving: &dyn Fn(&mut super::Tracked) -> Result<(), String> =
        &|feed| super::subscribe_quotes_surviving("ig", feed, "E", &NoWait);
    let retrying: &dyn Fn(&mut super::Tracked) -> Result<(), String> =
        &|feed| super::subscribe_quotes_retrying("ig", feed, "E", &NoWait);

    let (cap, transient) =
        (format!("the ceiling; {CAP_MARK}"), format!("a join; {TRANSIENT_MARK}"));
    assert_eq!(asked_of(&cap, surviving), (Ok(()), 1));
    let (answer, asks) = asked_of(&cap, retrying);
    assert!(answer.is_err_and(|e| e.starts_with("ig subscribe_quotes E: ")));
    assert_eq!(asks, 1, "a retry would meet the same ceiling");

    for verb in [surviving, retrying] {
        let (answer, asks) = asked_of(&transient, verb);
        assert!(
            answer.is_err_and(|e| e.contains(TRANSIENT_MARK)),
            "every try refused ends the start"
        );
        assert_eq!(asks, super::QUOTE_TRIES);
        let (answer, asks) = asked_of("no such epic", verb);
        assert!(answer.is_err_and(|e| e.contains("no such epic")));
        assert_eq!(asks, 1, "only a transient is asked again");
    }
}

/// A seam whose client refuses every quote ask with its text, counting the asks.
struct Refusing(String, Arc<Mutex<u32>>);

impl FeedCtors for Refusing {
    crate::testutil::scripted_strat!();
    fn client(
        &self,
        _venue: &str,
        _network: Option<&str>,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        Ok(Box::new(RefusesQuotes(self.0.clone(), Arc::clone(&self.1))))
    }
    fn pause(&self, _d: std::time::Duration) -> bool {
        false
    }
}

/// **IG's arm survives its ceiling** (decision 0193 verdict 6): the ceiling counts all holders together now, so a
/// mounted epic can meet it, and that costs the epic's quote lane and not the start of every venue's engines. Any
/// other refusal still refuses the mount.
#[test]
fn igs_arm_survives_a_cap_on_a_mounted_epics_quote_lane() {
    use vike_market_stream::market::CAP_MARK;

    let handle = core();
    let ig = MakerConfig::crypto("ig", SYM, 0.01, 1.0);
    let wrap = |sink: Arc<dyn LiveDataSink>| sink;
    let wired = |refusal: String| {
        let asked = Arc::new(Mutex::new(0));
        let seam = Refusing(refusal, Arc::clone(&asked));
        let feed = super::wire_ig_feed(&[&ig], &handle, &wrap, &seam).map(|_| ());
        (feed, *asked.lock().unwrap())
    };
    assert_eq!(wired(format!("the ceiling; {CAP_MARK}")), (Ok(()), 1));
    let (refused, asks) = wired("no usable credentials".to_string());
    assert!(refused.is_err_and(|e| e.contains("no usable credentials")));
    assert_eq!(asks, 1);
    handle.shutdown_and_join();
}

/// **cTrader's mount has no candle lane to survive on**: its one quote stream drives `on_quote_tick` AND the bar
/// synth, so a cap on the mount's symbol refuses the start in the arm's words, and a transient is asked again.
#[test]
fn a_ctrader_mount_is_asked_again_on_a_transient_and_refused_on_a_cap() {
    use vike_market_stream::market::{CAP_MARK, TRANSIENT_MARK};

    let handle = core();
    let cfg = MakerConfig::crypto("ctrader", SYM, 0.01, 1.0);
    let refused_by = |refusal: String| {
        let asked = Arc::new(Mutex::new(0));
        let seam = Refusing(refusal, Arc::clone(&asked));
        let answer = super::wire_ctrader_feed(&cfg, &[&cfg], &handle, &|sink| sink, &seam);
        let asks = *asked.lock().unwrap();
        (answer.err().expect("every ask was refused"), asks)
    };

    let (cap, asks) = refused_by(format!("the ceiling; {CAP_MARK}"));
    assert_eq!(asks, 1, "a retry would meet the same ceiling");
    assert!(cap.contains(CAP_MARK) && cap.contains("the daemon refuses to start"), "{cap}");

    let (transient, asks) = refused_by(format!("a join; {TRANSIENT_MARK}"));
    assert_eq!(asks, super::QUOTE_TRIES);
    assert!(transient.contains("the daemon refuses to start"), "{transient}");
    handle.shutdown_and_join();
}

/// The one dial on its own: the tap it hands back IS the one the sink feeds (the arms read prices through it).
#[test]
fn the_dial_hands_back_the_tap_its_sink_feeds() {
    let handle = core();
    let asked = Asked::default();
    let (_feed, tap) =
        super::dial("ig", None, PriceTap::default, &handle, &|s| s, &asked).expect("dialled");
    let sink = asked.sink.lock().unwrap().clone().expect("the seam was asked");
    let quote = QuoteTick {
        ts: 1,
        local_ts: 0,
        bid: 4.0,
        ask: 6.0,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: SYM.to_string(),
    };
    sink.quote("ig", SYM, quote);
    assert_eq!(tap.price("ig", SYM), Some(5.0));
    handle.shutdown_and_join();
}
