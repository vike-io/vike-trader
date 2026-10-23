//! The feed broker's contract, over a scripted client. See `crates/vike-datahub/src/feeds/mod.rs`.
use std::assert_matches;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_datahub::feeds::{ClientBuilder, FeedBroker, Holder, Sharing};
use vike_gatewayhub_client::credentialed_limits;
use vike_market_stream::market::CAP_MARK;
use vike_marketdata::{Bar, L2Book, QuoteTick, TradeTick};
use vike_model::{DataDelay, StreamStatus};

/// Every call the scripted clients received, in order, across every client the broker built.
type Log = Arc<Mutex<Vec<String>>>;
/// The sink each built client was handed, so a test can emit on the venue side.
type Sinks = Arc<Mutex<Vec<Arc<dyn LiveDataSink>>>>;

struct Scripted {
    venue: String,
    sink: Arc<dyn LiveDataSink>,
    log: Log,
    next: u64,
    /// How many times each `FLAKY<n>` symbol was asked for (see [`Scripted::start`]).
    asked: std::collections::HashMap<String, u32>,
}

impl Scripted {
    fn start(&mut self, verb: &str, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        if symbol == "REFUSED" {
            return Err(LiveDataError::Subscribe("scripted refusal".into()));
        }
        // `FLAKY<n>`: the first ask succeeds, the next `n` are refused as a link that is down, the rest succeed.
        if let Some(n) = symbol.strip_prefix("FLAKY").and_then(|n| n.parse::<u32>().ok()) {
            let asked = self.asked.entry(symbol.to_string()).or_default();
            *asked += 1;
            if (2..=n + 1).contains(asked) {
                return Err(LiveDataError::Subscribe(
                    "scripted: the link is down; the subscription was not started and is retried on the caller's next pass"
                        .into(),
                ));
            }
        }
        self.next += 1;
        self.log.lock().unwrap().push(format!("{} {verb} {symbol} -> {}", self.venue, self.next));
        Ok(SubscriptionId(self.next))
    }
}

impl DataClient for Scripted {
    /// A bar lane, logged as `"<venue> bars:<interval> <symbol> -> <id>"`. Only the strategy lane's
    /// handle ever reaches here: md's and the recorder's answer `Unsupported` before the client.
    fn subscribe_bars(&mut self, s: &str, interval: &str) -> Result<SubscriptionId, LiveDataError> {
        self.start(&format!("bars:{interval}"), s)
    }
    fn subscribe_quotes(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.start("quotes", s)
    }
    /// Emits SYNCHRONOUSLY inside the subscribe, the way a fast venue can — which is what
    /// proves the recorder's filter bit was published BEFORE the real call.
    fn subscribe_trades(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        let id = self.start("trades", s)?;
        self.sink.stream_status(
            &self.venue,
            s,
            "trades",
            StreamStatus::Live { gap_started_ts_ms: None },
        );
        Ok(id)
    }
    fn subscribe_book(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.start("book", s)
    }
    fn subscribe_depth(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.start("depth", s)
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.log.lock().unwrap().push(format!("{} unsubscribe {}", self.venue, id.0));
    }
    fn begin_shutdown(&mut self) {
        self.log.lock().unwrap().push(format!("{} begin_shutdown", self.venue));
    }
    /// Logged as `"<venue> set_delivery <symbol> <lane> <class>"`. `NOCLASS` declines the way a
    /// client with no class to change would; `CLASSFAIL` fails the way a lost link would.
    fn set_delivery(
        &mut self,
        symbol: &str,
        lane: StreamLabel,
        class: Delivery,
    ) -> Result<(), LiveDataError> {
        match symbol {
            "NOCLASS" => return Err(LiveDataError::Unsupported("scripted: no class")),
            "CLASSFAIL" => return Err(LiveDataError::Subscribe("scripted class failure".into())),
            _ => {}
        }
        self.log.lock().unwrap().push(format!(
            "{} set_delivery {symbol} {} {class:?}",
            self.venue,
            lane.as_str()
        ));
        Ok(())
    }
    /// A subscription is dead once a test writes `"dead <venue> <id>"` into the shared log.
    fn is_dead(&self, id: SubscriptionId) -> bool {
        let want = format!("dead {} {}", self.venue, id.0);
        self.log.lock().unwrap().contains(&want)
    }
    fn shutdown(&mut self) {
        self.log.lock().unwrap().push(format!("{} shutdown", self.venue));
    }
}

/// Counts what reached one holder's sink: `"verb venue symbol"` per call.
#[derive(Default)]
struct Seen(Mutex<Vec<String>>);
impl Seen {
    fn got(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
    fn push(&self, s: String) {
        self.0.lock().unwrap().push(s);
    }
}
impl LiveDataSink for Seen {
    fn seed_bars(&self, v: &str, s: &str, _: &str, _: Vec<Bar>) {
        self.push(format!("seed_bars {v} {s}"))
    }
    fn close_bar(&self, v: &str, s: &str, _: &str, _: Bar) {
        self.push(format!("close_bar {v} {s}"))
    }
    fn forming_bar(&self, v: &str, s: &str, _: &str, _: Bar) {
        self.push(format!("forming_bar {v} {s}"))
    }
    fn mark_tick(&self, v: &str, s: &str, _: f64, _: i64) {
        self.push(format!("mark_tick {v} {s}"))
    }
    fn quote(&self, v: &str, s: &str, _: QuoteTick) {
        self.push(format!("quote {v} {s}"))
    }
    fn trade(&self, v: &str, s: &str, _: TradeTick) {
        self.push(format!("trade {v} {s}"))
    }
    fn book(&self, v: &str, s: &str, _: Arc<L2Book>) {
        self.push(format!("book {v} {s}"))
    }
    fn l2_snapshot(
        &self,
        v: &str,
        s: &str,
        _: f64,
        _: Vec<vike_marketdata::BookLevel>,
        _: Vec<vike_marketdata::BookLevel>,
        _: i64,
    ) {
        self.push(format!("l2_snapshot {v} {s}"))
    }
    fn stream_status(&self, v: &str, s: &str, stream: &str, _: StreamStatus) {
        self.push(format!("status:{stream} {v} {s}"))
    }
    fn data_delay(&self, v: &str, s: &str, stream: &str, _: DataDelay) {
        self.push(format!("delay:{stream} {v} {s}"))
    }
}

fn quote() -> QuoteTick {
    QuoteTick {
        ts: 1,
        local_ts: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    }
}
fn trade() -> TradeTick {
    TradeTick {
        ts: 1,
        local_ts: 0,
        price: 1.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: String::new(),
    }
}

/// A broker over scripted clients. `polymarket` is PerHolder, every other venue Shared — the
/// production policy (D4).
fn broker() -> (Arc<FeedBroker>, Log, Sinks, Arc<Seen>, Arc<Seen>) {
    let (b, log, sinks) = bare_broker();
    let md = Arc::new(Seen::default());
    let rec = Arc::new(Seen::default());
    b.attach_sink(Holder::Md, Arc::clone(&md) as Arc<dyn LiveDataSink>);
    b.attach_sink(Holder::Rec, Arc::clone(&rec) as Arc<dyn LiveDataSink>);
    (b, log, sinks, md, rec)
}

/// The same broker with NO holder's sink attached yet (a holder attaches once: a test that wants its own
/// sinks starts here).
fn bare_broker() -> (Arc<FeedBroker>, Log, Sinks) {
    let log: Log = Arc::default();
    let sinks: Sinks = Arc::default();
    let (l, s) = (Arc::clone(&log), Arc::clone(&sinks));
    // The return type is spelled: `Box::new` is generic, so a closure passed through it gets no
    // expected signature and would infer `Box<Scripted>`, which does not coerce to the builder.
    let builder: ClientBuilder = Box::new(
        move |venue: &str,
              sink: Arc<dyn LiveDataSink>|
              -> Result<Box<dyn DataClient + Send>, String> {
            s.lock().unwrap().push(Arc::clone(&sink));
            Ok(Box::new(Scripted {
                venue: venue.into(),
                sink,
                log: Arc::clone(&l),
                next: 0,
                asked: Default::default(),
            }))
        },
    );
    let b = FeedBroker::new(builder, |venue| {
        if venue == "polymarket" { Sharing::PerHolder } else { Sharing::Shared }
    });
    (b, log, sinks)
}

fn count(log: &Log, needle: &str) -> usize {
    log.lock().unwrap().iter().filter(|l| l.contains(needle)).count()
}

#[test]
fn two_holders_of_one_key_share_one_real_subscription() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "binance"));
    let a = md.subscribe_depth("BTCUSDT.P").unwrap();
    let c = rec.subscribe_depth("BTCUSDT.P").unwrap();
    assert_ne!(a, c, "each holder gets its own id");
    assert_eq!(count(&log, "depth BTCUSDT.P"), 1, "ONE real subscribe");
    md.unsubscribe(a);
    assert_eq!(count(&log, "unsubscribe"), 0, "the recorder still holds it");
    rec.unsubscribe(c);
    assert_eq!(count(&log, "unsubscribe"), 1, "the last release is the real unsubscribe");
    assert_eq!(b.real_subscriptions(), 0);
}

#[test]
fn one_holder_twice_is_one_real_subscription_and_one_row() {
    let (b, log, sinks, _md, rec) = broker();
    let (mut r1, mut r2) = (b.handle(Holder::Rec, "binance"), b.handle(Holder::Rec, "binance"));
    let a = r1.subscribe_depth("BTCUSDT.P").unwrap();
    let _c = r2.subscribe_depth("BTCUSDT.P").unwrap();
    assert_eq!(count(&log, "depth BTCUSDT.P"), 1);
    sinks.lock().unwrap()[0].l2_snapshot("binance", "BTCUSDT.P", 0.1, vec![], vec![], 1);
    assert_eq!(
        rec.got().iter().filter(|g| g.starts_with("l2_snapshot")).count(),
        1,
        "written ONCE"
    );
    r1.unsubscribe(a);
    assert_eq!(count(&log, "unsubscribe"), 0, "the second profile entry still holds it");
}

#[test]
fn the_recorder_never_sees_a_key_only_md_holds() {
    let (b, _log, sinks, md, rec) = broker();
    let mut m = b.handle(Holder::Md, "binance");
    m.subscribe_depth("ETHUSDT.P").unwrap();
    let sink = Arc::clone(&sinks.lock().unwrap()[0]);
    sink.l2_snapshot("binance", "ETHUSDT.P", 0.1, vec![], vec![], 1);
    sink.trade("binance", "ETHUSDT.P", trade());
    assert!(md.got().iter().any(|g| g == "l2_snapshot binance ETHUSDT.P"), "md always sees it");
    assert!(rec.got().is_empty(), "no recorder row for a key the recorder never asked for");
}

#[test]
fn the_filter_bit_is_published_before_the_real_subscribe() {
    let (b, _log, _sinks, _md, rec) = broker();
    let mut r = b.handle(Holder::Rec, "binance");
    r.subscribe_trades("BTCUSDT.P").unwrap();
    assert!(
        rec.got().iter().any(|g| g == "status:trades binance BTCUSDT.P"),
        "the event emitted INSIDE the subscribe reached the recorder: {:?}",
        rec.got()
    );
}

/// **Every `StreamLabel` is routed by the recorder lane it names — to a recorder that HOLDS that
/// lane, and to nobody else.** The broker keeps one filter bit per lane and decides a disclosure's
/// bit from the feed's `stream` word, so a word it fails to recognise silently drops that lane's gap
/// and stale disclosures at the recorder (the forty-day `depth` blind spot's shape). This walks the
/// whole shared vocabulary instead of spelling the words: `subscribe_lane` is an exhaustive `match`
/// (a new label fails to compile here until the broker's routing is decided for it), the emitted
/// words come from `StreamLabel::as_str`, and a bar interval is routed to md only.
#[test]
fn a_status_reaches_the_recorder_only_for_the_lane_it_names() {
    fn subscribe_lane(
        handle: &mut dyn DataClient,
        label: StreamLabel,
        symbol: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        match label {
            StreamLabel::Quotes => handle.subscribe_quotes(symbol),
            StreamLabel::Trades => handle.subscribe_trades(symbol),
            StreamLabel::Book => handle.subscribe_book(symbol),
            StreamLabel::Depth => handle.subscribe_depth(symbol),
        }
    }
    fn statuses(seen: &Seen) -> BTreeSet<String> {
        seen.got().into_iter().filter(|g| g.starts_with("status:")).collect()
    }

    for &held in StreamLabel::ALL {
        let (b, _log, sinks, md, rec) = broker();
        let mut r = b.handle(Holder::Rec, "binance");
        subscribe_lane(&mut r, held, "S").unwrap();
        let sink = Arc::clone(&sinks.lock().unwrap()[0]);
        let live = StreamStatus::Live { gap_started_ts_ms: None };
        for &emitted in StreamLabel::ALL {
            sink.stream_status("binance", "S", emitted.as_str(), live);
        }
        sink.stream_status("binance", "S", "1m", live);

        let only_held: BTreeSet<String> =
            [format!("status:{} binance S", held.as_str())].into_iter().collect();
        assert_eq!(statuses(&rec), only_held, "a recorder holding {held:?}");
        let md_got = statuses(&md);
        for &emitted in StreamLabel::ALL {
            assert!(
                md_got.contains(&format!("status:{} binance S", emitted.as_str())),
                "md hears every label, {emitted:?} included: {md_got:?}"
            );
        }
    }
}

#[test]
fn a_quote_reaches_the_recorder_under_a_held_book_and_not_otherwise() {
    let (b, _log, sinks, _md, rec) = broker();
    let mut r = b.handle(Holder::Rec, "deribit");
    r.subscribe_book("BTC-PERPETUAL").unwrap();
    let sink = Arc::clone(&sinks.lock().unwrap()[0]);
    sink.quote("deribit", "BTC-PERPETUAL", quote());
    sink.quote("deribit", "ETH-PERPETUAL", quote());
    assert_eq!(rec.got(), vec!["quote deribit BTC-PERPETUAL".to_string()]);
}

#[test]
fn bar_and_mark_verbs_never_reach_the_recorder() {
    let (b, _log, sinks, md, rec) = broker();
    let mut r = b.handle(Holder::Rec, "binance");
    r.subscribe_trades("BTCUSDT.P").unwrap();
    let sink = Arc::clone(&sinks.lock().unwrap()[0]);
    sink.mark_tick("binance", "BTCUSDT.P", 1.0, 1);
    assert!(!rec.got().iter().any(|g| g.starts_with("mark_tick")));
    assert!(md.got().iter().any(|g| g.starts_with("mark_tick")), "md gets every verb");
}

#[test]
fn begin_shutdown_raises_nothing_while_the_other_holder_holds_a_live_key() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "binance"));
    md.subscribe_depth("BTCUSDT.P").unwrap();
    rec.subscribe_trades("ETHUSDT.P").unwrap();
    md.begin_shutdown();
    assert_eq!(count(&log, "begin_shutdown"), 0, "raise_stops would kill the recorder's key");
    md.shutdown();
    assert_eq!(count(&log, "unsubscribe"), 1, "md's own key only");
}

#[test]
fn begin_shutdown_raises_the_real_flags_for_a_sole_holder() {
    let (b, log, ..) = broker();
    let mut md = b.handle(Holder::Md, "binance");
    md.subscribe_depth("BTCUSDT.P").unwrap();
    md.begin_shutdown();
    assert_eq!(
        count(&log, "begin_shutdown"),
        1,
        "the two-phase wind-down is kept where it is safe"
    );
}

#[test]
fn an_acquire_during_a_drain_is_a_transient_refusal_that_clears() {
    let (b, _log, ..) = broker();
    let (mut md, mut rec) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "binance"));
    md.subscribe_depth("BTCUSDT.P").unwrap();
    md.begin_shutdown();
    match rec.subscribe_depth("BTCUSDT.P") {
        Err(LiveDataError::Subscribe(msg)) => assert!(msg.contains("draining"), "{msg}"),
        other => panic!("expected a TRANSIENT refusal, got {other:?}"),
    }
    md.shutdown();
    assert!(rec.subscribe_depth("BTCUSDT.P").is_ok(), "the drain ends with md's last release");
}

#[test]
fn a_failed_real_subscribe_leaves_no_phantom_hold() {
    let (b, _log, sinks, _md, rec) = broker();
    let mut r = b.handle(Holder::Rec, "binance");
    assert!(r.subscribe_trades("REFUSED").is_err());
    assert_eq!(b.real_subscriptions(), 0);
    sinks.lock().unwrap()[0].trade("binance", "REFUSED", trade());
    assert!(rec.got().is_empty(), "the filter bit was withdrawn with the failure");
}

#[test]
fn a_per_holder_venue_builds_one_client_per_holder_with_that_holders_sink() {
    let (b, log, sinks, md, rec) = broker();
    let (mut m, mut r) = (b.handle(Holder::Md, "polymarket"), b.handle(Holder::Rec, "polymarket"));
    m.subscribe_book("TOK").unwrap();
    r.subscribe_book("TOK").unwrap();
    assert_eq!(
        count(&log, "book TOK"),
        2,
        "polymarket shards resubscribe co-tenants — never shared"
    );
    assert_eq!(sinks.lock().unwrap().len(), 2);
    sinks.lock().unwrap()[1].trade("polymarket", "TOK", trade());
    assert_eq!(rec.got().len() + md.got().len(), 1, "each per-holder client feeds ONE sink");
}

#[test]
fn begin_shutdown_all_raises_every_client_before_anything_joins() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "polymarket"));
    md.subscribe_depth("BTCUSDT.P").unwrap();
    rec.subscribe_book("TOK").unwrap();
    b.begin_shutdown_all();
    assert_eq!(count(&log, "begin_shutdown"), 2);
    assert_eq!(count(&log, "unsubscribe"), 0, "phase one joins nothing");
}

/// The PROCESS teardown (Task 7 calls `begin_shutdown_all` before the recorder's `stop_all`):
/// every flag is raised before anything joins, and a key md still holds is not released for real.
#[test]
fn process_teardown_raises_every_flag_before_the_recorder_releases_anything() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "binance"));
    md.subscribe_depth("BTCUSDT.P").unwrap();
    rec.subscribe_depth("BTCUSDT.P").unwrap();
    rec.subscribe_trades("ETHUSDT.P").unwrap();
    b.begin_shutdown_all();
    rec.shutdown();
    let lines = log.lock().unwrap().clone();
    let raise = lines.iter().position(|l| l.contains("begin_shutdown")).expect("raised");
    let first_unsub =
        lines.iter().position(|l| l.contains("unsubscribe")).expect("a sole key released");
    assert!(raise < first_unsub, "phase one before any join: {lines:?}");
    assert_eq!(
        count(&log, "unsubscribe"),
        1,
        "BTCUSDT.P is still md's; only ETHUSDT.P is released for real"
    );
}

#[test]
fn bars_are_not_brokered() {
    let (b, ..) = broker();
    let mut r = b.handle(Holder::Rec, "binance");
    assert_matches!(r.subscribe_bars("BTCUSDT", "1m"), Err(LiveDataError::Unsupported(_)));
}

/// M3 — the filter's cost on a feed thread, measured rather than assumed. Ignored: run once in a
/// lane with `-- --ignored --nocapture` and paste the figure into the plan's `## Measured`.
#[test]
#[ignore]
fn m3_filter_cost_per_event() {
    let (b, _log, sinks, _md, _rec) = broker();
    let mut r = b.handle(Holder::Rec, "binance");
    for i in 0..64 {
        r.subscribe_trades(&format!("S{i}")).unwrap();
    }
    let sink = Arc::clone(&sinks.lock().unwrap()[0]);
    let n = 200_000u32;
    let t = std::time::Instant::now();
    for _ in 0..n {
        sink.stream_status(
            "binance",
            "S7",
            "trades",
            StreamStatus::Live { gap_started_ts_ms: None },
        );
    }
    println!(
        "routing: {} ns/event (incl. the Seen sink's push)",
        t.elapsed().as_nanos() / n as u128
    );
}

// ---- rule 4: a key's delivery class is the strongest need of its holders ----

fn lines(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

/// **The recorder joins a depth key the hub already holds: no second subscribe, and the class on the
/// EXISTING subscription is strengthened.** This is the case a per-subscribe parameter cannot reach
/// (the broker sends no `subscribe_*` on a join), and the one the tube's conflation would turn into a
/// thinner `kind=depth` tape with no marker.
#[test]
fn a_recorder_joining_a_depth_key_the_hub_holds_strengthens_it_without_a_second_subscribe() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "binance"));
    md.subscribe_depth("BTCUSDT.P").unwrap();
    assert_eq!(count(&log, "set_delivery"), 0, "the hub alone runs at the lane's default");
    rec.subscribe_depth("BTCUSDT.P").unwrap();
    assert_eq!(count(&log, "depth BTCUSDT.P ->"), 1, "ONE real subscribe");
    assert_eq!(
        count(&log, "binance set_delivery BTCUSDT.P depth Lossless"),
        1,
        "strengthened once: {:?}",
        lines(&log)
    );
    assert_eq!(count(&log, "set_delivery"), 1);
    // A second recorder entry on the same key changes nothing further.
    let mut rec2 = b.handle(Holder::Rec, "binance");
    rec2.subscribe_depth("BTCUSDT.P").unwrap();
    assert_eq!(count(&log, "set_delivery"), 1, "already lossless");
}

/// **The recorder leaves a key the hub still holds: the class relaxes; the hub leaving a key the
/// recorder holds does not.**
#[test]
fn a_recorder_leaving_relaxes_the_delivery_and_the_hub_leaving_does_not() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "binance"));
    let m = md.subscribe_depth("BTCUSDT.P").unwrap();
    let r = rec.subscribe_depth("BTCUSDT.P").unwrap();
    rec.unsubscribe(r);
    assert_eq!(
        count(&log, "binance set_delivery BTCUSDT.P depth Conflatable"),
        1,
        "{:?}",
        lines(&log)
    );
    assert_eq!(count(&log, "unsubscribe"), 0, "the hub still holds the real subscription");

    // The other way round: the hub leaves, the recorder stays: lossless stays, no call.
    let r = rec.subscribe_depth("BTCUSDT.P").unwrap();
    let before = count(&log, "set_delivery");
    md.unsubscribe(m);
    assert_eq!(count(&log, "set_delivery"), before, "the recorder's need is unchanged");
    // The last release is the real unsubscribe and calls nothing else.
    rec.unsubscribe(r);
    assert_eq!(count(&log, "unsubscribe"), 1);
    assert_eq!(count(&log, "set_delivery"), before);
}

/// A recorder that opens the key FIRST carries its class before the subscribe, so the very first
/// frames are already lossless; the hub joining afterwards changes nothing.
#[test]
fn a_recorder_first_key_is_lossless_from_the_subscribe() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "binance"));
    rec.subscribe_depth("BTCUSDT.P").unwrap();
    let l = lines(&log);
    let at = |needle: &str| l.iter().position(|x| x.contains(needle)).unwrap();
    assert!(
        at("set_delivery BTCUSDT.P depth Lossless") < at("depth BTCUSDT.P ->"),
        "the class precedes the real subscribe: {l:?}"
    );
    md.subscribe_depth("BTCUSDT.P").unwrap();
    assert_eq!(count(&log, "set_delivery"), 1, "the hub needs less than the key already runs at");
}

/// Only the depth lane has a class to change: every other lane is lossless for both holders, so the
/// client is never called for them (the broker's hot path adds nothing there).
#[test]
fn lanes_that_are_lossless_for_everyone_never_see_a_class_call() {
    let (b, log, ..) = broker();
    let mut rec = b.handle(Holder::Rec, "binance");
    rec.subscribe_quotes("S").unwrap();
    rec.subscribe_trades("S").unwrap();
    rec.subscribe_book("S").unwrap();
    let mut md = b.handle(Holder::Md, "binance");
    md.subscribe_trades("S").unwrap();
    assert_eq!(count(&log, "set_delivery"), 0, "{:?}", lines(&log));
}

/// A `PerHolder` venue's client is the holder's own: the recorder's client is told lossless at its
/// subscribe, the hub's is never touched.
#[test]
fn a_per_holder_client_takes_its_own_holders_class() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) =
        (b.handle(Holder::Md, "polymarket"), b.handle(Holder::Rec, "polymarket"));
    md.subscribe_depth("TOK").unwrap();
    rec.subscribe_depth("TOK").unwrap();
    assert_eq!(count(&log, "depth TOK ->"), 2, "one client per holder");
    assert_eq!(count(&log, "polymarket set_delivery TOK depth Lossless"), 1);
    assert_eq!(count(&log, "set_delivery"), 1, "the hub's client runs at its default");
}

/// A client that DECLINES a class (`Unsupported`) does not fail the join: the key keeps running as it
/// always did, and the recorder holds it. A client that FAILS the change (a lost link) is a transient
/// refusal that leaves no phantom hold and no recorder filter bit.
#[test]
fn a_declined_class_keeps_the_join_and_a_failed_one_refuses_it_cleanly() {
    let (b, _log, sinks, _md, rec) = broker();
    let (mut md, mut r) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "binance"));
    md.subscribe_depth("NOCLASS").unwrap();
    assert!(r.subscribe_depth("NOCLASS").is_ok(), "declined: the join stands");

    md.subscribe_depth("CLASSFAIL").unwrap();
    assert!(r.subscribe_depth("CLASSFAIL").is_err(), "failed: the join is refused");
    sinks.lock().unwrap()[0].l2_snapshot("binance", "CLASSFAIL", 0.1, vec![], vec![], 1);
    assert!(
        !rec.got().iter().any(|g| g == "l2_snapshot binance CLASSFAIL"),
        "the filter bit was withdrawn with the refusal: {:?}",
        rec.got()
    );
}

// ---- rule 5: a dead subscription is replaced ----

/// **A client that reports a held subscription dead is dropped and asked for afresh; one that does
/// not is left alone.** Without the question `slot.real` stays set for ever and nothing re-asks.
#[test]
fn a_dead_subscription_is_unsubscribed_and_resubscribed_once_and_a_live_one_is_untouched() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "binance"));
    md.subscribe_trades("BTCUSDT.P").unwrap(); // real id 1
    rec.subscribe_trades("BTCUSDT.P").unwrap();
    assert_eq!(count(&log, "trades BTCUSDT.P ->"), 1, "alive: shared, not re-asked");
    assert_eq!(count(&log, "unsubscribe"), 0);

    log.lock().unwrap().push("dead binance 1".to_string());
    let mut rec2 = b.handle(Holder::Rec, "binance");
    rec2.subscribe_trades("BTCUSDT.P").unwrap();
    assert_eq!(
        count(&log, "binance unsubscribe 1"),
        1,
        "the dead id is released: {:?}",
        lines(&log)
    );
    assert_eq!(count(&log, "trades BTCUSDT.P ->"), 2, "and asked for afresh, once");
    assert_eq!(b.real_subscriptions(), 1);

    // The new id (2) is alive: the next join is untouched.
    let mut rec3 = b.handle(Holder::Rec, "binance");
    rec3.subscribe_trades("BTCUSDT.P").unwrap();
    assert_eq!(count(&log, "trades BTCUSDT.P ->"), 2);
    assert_eq!(count(&log, "unsubscribe"), 1);
}

/// The replacement re-sends the class: a dead DEPTH subscription the recorder holds comes back
/// lossless, after the drop and before its subscribe. And the holders' ids survive: releasing them
/// all ends the REPLACEMENT subscription, once.
#[test]
fn a_replaced_depth_subscription_comes_back_at_the_holders_class() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Rec, "binance"));
    let m = md.subscribe_depth("BTCUSDT.P").unwrap(); // real id 1
    let r = rec.subscribe_depth("BTCUSDT.P").unwrap(); // joins: Lossless
    log.lock().unwrap().push("dead binance 1".to_string());
    let mut md2 = b.handle(Holder::Md, "binance");
    let m2 = md2.subscribe_depth("BTCUSDT.P").unwrap();
    assert_eq!(count(&log, "depth BTCUSDT.P ->"), 2);
    let l = lines(&log);
    let at = |needle: &str| l.iter().rposition(|x| x.contains(needle)).unwrap();
    let (dropped, class, subscribed) = (
        at("binance unsubscribe 1"),
        at("set_delivery BTCUSDT.P depth Lossless"),
        at("depth BTCUSDT.P -> 2"),
    );
    assert!(
        dropped < class && class < subscribed,
        "the class is re-sent after the drop and before the new subscribe: {l:?}"
    );
    md.unsubscribe(m);
    rec.unsubscribe(r);
    md2.unsubscribe(m2);
    assert_eq!(
        count(&log, "binance unsubscribe 2"),
        1,
        "the replacement ends with the last release"
    );
    assert_eq!(b.real_subscriptions(), 0);
}

/// A `PerHolder` venue: one holder's client dying replaces ITS subscription and not the other's.
#[test]
fn a_dead_per_holder_client_is_replaced_without_touching_the_other_holders() {
    let (b, log, ..) = broker();
    let (mut md, mut rec) =
        (b.handle(Holder::Md, "polymarket"), b.handle(Holder::Rec, "polymarket"));
    md.subscribe_book("TOK").unwrap(); // md's client: id 1
    rec.subscribe_book("TOK").unwrap(); // rec's client: id 1
    log.lock().unwrap().push("dead polymarket 1".to_string()); // both clients name their id 1
    let mut rec2 = b.handle(Holder::Rec, "polymarket");
    rec2.subscribe_book("TOK").unwrap();
    assert_eq!(count(&log, "polymarket unsubscribe 1"), 1, "only the recorder's client was asked");
    assert_eq!(count(&log, "book TOK ->"), 3, "md once, the recorder's twice");
}

// ---- The strategy lane: the broker's third holder (wave 2, W5) ------------------------------------------
//
// Rules 1 to 3 above pass UNCHANGED. What follows are the rules the third holder adds: its keys are its
// own, it is the one holder that may subscribe a bar lane, it reaches only the keys and lanes it holds,
// it is called FIRST, its depth floor is a class need while the hold stands, and a strategy-only release
// leaves md and the recorder exactly as they were.

/// The broker above with the strategy lane's sink attached as `Holder::Strat`.
fn with_strat() -> (Arc<FeedBroker>, Log, Sinks, Arc<Seen>, Arc<Seen>, Arc<Seen>) {
    let (b, log, sinks, md, rec) = broker();
    let strat = Arc::new(Seen::default());
    b.attach_sink(Holder::Strat, Arc::clone(&strat) as Arc<dyn LiveDataSink>);
    (b, log, sinks, md, rec, strat)
}

fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 3.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// **Md, the recorder and the strategy lane share ONE real subscription**, and a strategy-only release
/// leaves the others' key exactly as it was: no unsubscribe, no class change, no begin_shutdown.
#[test]
fn a_strategy_only_release_leaves_the_key_md_and_the_recorder_hold_untouched() {
    let (b, log, _sinks, ..) = with_strat();
    let (mut md, mut rec, mut strat) = (
        b.handle(Holder::Md, "binance"),
        b.handle(Holder::Rec, "binance"),
        b.handle(Holder::Strat, "binance"),
    );
    md.subscribe_trades("BTCUSDT.P").unwrap();
    rec.subscribe_trades("BTCUSDT.P").unwrap();
    let s = strat.subscribe_trades("BTCUSDT.P").unwrap();
    assert_eq!(count(&log, "trades BTCUSDT.P"), 1, "ONE real subscription for three holders");
    let before = log.lock().unwrap().clone();
    strat.unsubscribe(s);
    assert_eq!(*log.lock().unwrap(), before, "the strategy's release touched the client at all");
    assert_eq!(b.real_subscriptions(), 1, "md and the recorder still hold it");
    // ...and the strategy lane alone holding a key is released for real when it lets go.
    let t = strat.subscribe_trades("ETHUSDT.P").unwrap();
    assert_eq!(b.real_subscriptions(), 2);
    strat.unsubscribe(t);
    assert_eq!(count(&log, "unsubscribe"), 1, "the strategy's own key is the real unsubscribe");
    assert_eq!(b.real_subscriptions(), 1);
}

/// **A bar lane is the strategy lane's alone.** md's and the recorder's handles still answer
/// `Unsupported` without asking the client, the strategy's reaches it, the bar verbs of a held interval
/// reach the strategy lane and no other interval does, the kline lane's per-symbol ticks reach it, and
/// the recorder hears none of them.
#[test]
fn only_the_strategy_lane_holds_bars_and_its_frames_follow_the_interval() {
    let (b, log, sinks, md, rec, strat) = with_strat();
    for holder in [Holder::Md, Holder::Rec] {
        let mut h = b.handle(holder, "binance");
        assert_matches!(h.subscribe_bars("BTCUSDT.P", "1m"), Err(LiveDataError::Unsupported(_)));
    }
    assert_eq!(count(&log, "bars:"), 0, "the client was never asked on their behalf");

    let mut s = b.handle(Holder::Strat, "binance");
    s.subscribe_bars("BTCUSDT.P", "1m").unwrap();
    assert_eq!(count(&log, "binance bars:1m BTCUSDT.P"), 1);
    let sink = Arc::clone(&sinks.lock().unwrap()[0]);
    sink.seed_bars("binance", "BTCUSDT.P", "1m", vec![bar(1)]);
    sink.close_bar("binance", "BTCUSDT.P", "1m", bar(2));
    sink.forming_bar("binance", "BTCUSDT.P", "1m", bar(3));
    sink.forming_bar("binance", "BTCUSDT.P", "5m", bar(3)); // an interval nobody here holds
    sink.mark_tick("binance", "BTCUSDT.P", 1.0, 1);
    sink.mark_tick("binance", "OTHER", 1.0, 1); // a symbol nobody here holds
    sink.stream_status(
        "binance",
        "BTCUSDT.P",
        "1m",
        StreamStatus::Live { gap_started_ts_ms: None },
    );
    sink.stream_status(
        "binance",
        "BTCUSDT.P",
        "5m",
        StreamStatus::Live { gap_started_ts_ms: None },
    );
    assert_eq!(
        strat.got(),
        vec![
            "seed_bars binance BTCUSDT.P".to_string(),
            "close_bar binance BTCUSDT.P".to_string(),
            "forming_bar binance BTCUSDT.P".to_string(),
            "mark_tick binance BTCUSDT.P".to_string(),
            "status:1m binance BTCUSDT.P".to_string(),
        ],
        "exactly the held interval's verbs, the symbol's mark tick and the held interval's status"
    );
    assert!(rec.got().is_empty(), "the recorder hears no bar verb: {:?}", rec.got());
    assert!(
        md.got().iter().any(|g| g == "forming_bar binance BTCUSDT.P"),
        "md still gets every verb it always got"
    );
}

/// **The strategy lane is admitted by the lanes it holds**, as the recorder is: a quote under `Quotes`
/// OR `Book`, a trade under `Trades`, a status for a lane's own word; and a symbol it does not hold
/// reaches it never. A release withdraws the lane.
#[test]
fn the_strategy_lane_reaches_only_the_keys_and_lanes_it_holds() {
    let (b, _log, sinks, _md, rec, strat) = with_strat();
    let mut s = b.handle(Holder::Strat, "binance");
    let q = s.subscribe_quotes("A").unwrap();
    s.subscribe_book("B").unwrap();
    let sink = Arc::clone(&sinks.lock().unwrap()[0]);
    let live = StreamStatus::Live { gap_started_ts_ms: None };
    sink.quote("binance", "A", quote());
    sink.trade("binance", "A", trade()); // A is held under Quotes only
    sink.quote("binance", "B", quote()); // a book pump may derive L1
    sink.quote("binance", "C", quote()); // not held
    sink.stream_status("binance", "A", "quotes", live);
    sink.stream_status("binance", "A", "trades", live);
    assert_eq!(
        strat.got(),
        vec![
            "quote binance A".to_string(),
            "quote binance B".to_string(),
            "status:quotes binance A".to_string(),
        ]
    );
    assert!(rec.got().is_empty(), "a strategy hold publishes nothing to the recorder's key set");
    s.unsubscribe(q);
    sink.quote("binance", "A", quote());
    assert_eq!(strat.got().len(), 3, "the released lane no longer reaches the strategy lane");
}

/// Records the order in which the three holders' sinks are called for one frame.
struct Tagged(&'static str, Arc<Mutex<Vec<&'static str>>>);
impl LiveDataSink for Tagged {
    fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<Bar>) {}
    fn close_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
    fn forming_bar(&self, _: &str, _: &str, _: &str, _: Bar) {}
    fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
    fn trade(&self, _: &str, _: &str, _: TradeTick) {
        self.1.lock().unwrap().push(self.0);
    }
    fn book(&self, _: &str, _: &str, _: Arc<L2Book>) {}
    fn quote(&self, _: &str, _: &str, _: QuoteTick) {
        self.1.lock().unwrap().push(self.0);
    }
}

/// **The strategy lane is called FIRST.** The routing sink calls its holders serially from the venue
/// client's one thread, so whatever sits ahead of a holder delays it; the tradehub's prices are the
/// latency-critical consumer, and its enqueue is lock-and-enqueue, so it goes before a slow md fold or a
/// recorder write.
#[test]
fn the_strategy_enqueue_is_first_in_the_sink_order() {
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let (b, _log, sinks) = bare_broker();
    b.attach_sink(Holder::Md, Arc::new(Tagged("md", Arc::clone(&order))));
    b.attach_sink(Holder::Rec, Arc::new(Tagged("rec", Arc::clone(&order))));
    b.attach_sink(Holder::Strat, Arc::new(Tagged("strat", Arc::clone(&order))));
    let mut handles = Vec::new();
    for holder in [Holder::Md, Holder::Rec, Holder::Strat] {
        let mut h = b.handle(holder, "binance");
        h.subscribe_trades("BTCUSDT.P").unwrap();
        handles.push(h); // keep the hold for the whole test
    }
    let sink = Arc::clone(&sinks.lock().unwrap()[0]);
    sink.trade("binance", "BTCUSDT.P", trade());
    assert_eq!(*order.lock().unwrap(), vec!["strat", "rec", "md"]);
    order.lock().unwrap().clear();
    // ...and for a quote, where the recorder and the strategy lane are admitted by Quotes OR Book.
    let mut s = b.handle(Holder::Strat, "binance");
    s.subscribe_quotes("ETHUSDT.P").unwrap();
    sink.quote("binance", "ETHUSDT.P", quote());
    assert_eq!(*order.lock().unwrap(), vec!["strat", "md"]);
}

/// **A strategy's depth floor is a class need while the hold stands** (rule 4, the other holder that
/// states one): stated before the subscribe it rides it - the client is told BEFORE it is asked to
/// subscribe -, stated after it raises the live key, and cleared or released it relaxes - and md and the
/// recorder, whose class is their own, decline to state one.
#[test]
fn a_strategy_depth_floor_raises_the_class_while_the_hold_stands_and_relaxes_with_it() {
    let (b, log, ..) = with_strat();
    let mut s = b.handle(Holder::Strat, "binance");
    // Stated BEFORE the subscribe.
    s.set_delivery("BTCUSDT.P", StreamLabel::Depth, Delivery::Lossless).unwrap();
    let id = s.subscribe_depth("BTCUSDT.P").unwrap();
    let lines = log.lock().unwrap().clone();
    let raised = lines.iter().position(|l| l.contains("set_delivery BTCUSDT.P depth Lossless"));
    let subscribed = lines.iter().position(|l| l.contains("depth BTCUSDT.P ->"));
    assert!(
        raised.is_some_and(|r| subscribed.is_some_and(|sub| r < sub)),
        "the floor reaches the client before the subscribe: {lines:?}"
    );
    // Cleared AFTER: the live key relaxes.
    s.set_delivery("BTCUSDT.P", StreamLabel::Depth, Delivery::Conflatable).unwrap();
    assert_eq!(count(&log, "set_delivery BTCUSDT.P depth Conflatable"), 1, "relaxed live");
    // Stated AFTER the subscribe: the live key is raised, and releasing the hold relaxes it again.
    s.set_delivery("BTCUSDT.P", StreamLabel::Depth, Delivery::Lossless).unwrap();
    assert_eq!(count(&log, "set_delivery BTCUSDT.P depth Lossless"), 2, "raised live");
    let mut md = b.handle(Holder::Md, "binance");
    md.subscribe_depth("BTCUSDT.P").unwrap(); // md joins the strategy's already-lossless key
    s.unsubscribe(id);
    assert_eq!(
        count(&log, "set_delivery BTCUSDT.P depth Conflatable"),
        2,
        "the strategy left, md's need is the lane's default: the key relaxes"
    );
    for holder in [Holder::Md, Holder::Rec] {
        let mut h = b.handle(holder, "binance");
        assert_matches!(
            h.set_delivery("BTCUSDT.P", StreamLabel::Depth, Delivery::Lossless),
            Err(LiveDataError::Unsupported(_))
        );
    }
}

/// The two-phase wind-down is kept where it is safe: a strategy handle that is the sole holder of every
/// live key raises the real flags, and one that shares a key with md does not.
#[test]
fn a_strategy_begin_shutdown_raises_nothing_while_md_holds_a_live_key() {
    let (b, log, ..) = with_strat();
    let (mut md, mut s) = (b.handle(Holder::Md, "binance"), b.handle(Holder::Strat, "binance"));
    md.subscribe_trades("BTCUSDT.P").unwrap();
    s.subscribe_trades("BTCUSDT.P").unwrap();
    s.begin_shutdown();
    assert_eq!(count(&log, "begin_shutdown"), 0, "raise_stops would kill md's key");
    s.shutdown();
    md.shutdown();
    let mut sole = b.handle(Holder::Strat, "binance");
    sole.subscribe_trades("ETHUSDT.P").unwrap();
    sole.begin_shutdown();
    assert_eq!(count(&log, "begin_shutdown"), 1, "sole holder: the wind-down is raised");
}

/// A `PerHolder` venue gives the strategy lane its OWN client with its own sink, and md and the
/// recorder theirs: three clients, three sinks, nothing crossing.
#[test]
fn a_per_holder_venue_gives_the_strategy_lane_its_own_client_and_sink() {
    let (b, log, sinks, md, rec, strat) = with_strat();
    let mut handles = Vec::new();
    for holder in [Holder::Md, Holder::Rec, Holder::Strat] {
        let mut h = b.handle(holder, "polymarket");
        h.subscribe_book("TOK").unwrap();
        handles.push(h);
    }
    assert_eq!(count(&log, "book TOK"), 3, "one client per holder");
    assert_eq!(sinks.lock().unwrap().len(), 3);
    // The strategy lane's client is the third built; its sink is the lane's, handed to it directly.
    sinks.lock().unwrap()[2].trade("polymarket", "TOK", trade());
    assert_eq!(
        rec.got().len() + md.got().len(),
        0,
        "nothing the strategy's client emits reaches them"
    );
    assert_eq!(strat.got(), vec!["trade polymarket TOK".to_string()]);
}

/// A failed subscribe leaves no phantom hold and withdraws the strategy lane's key set, like the
/// recorder's bit.
#[test]
fn a_failed_strategy_subscribe_withdraws_its_key_set() {
    let (b, _log, sinks, _md, _rec, strat) = with_strat();
    let mut s = b.handle(Holder::Strat, "binance");
    assert!(s.subscribe_trades("REFUSED").is_err());
    assert!(s.subscribe_bars("REFUSED", "1m").is_err());
    assert_eq!(b.real_subscriptions(), 0);
    let sink = Arc::clone(&sinks.lock().unwrap()[0]);
    sink.trade("binance", "REFUSED", trade());
    sink.close_bar("binance", "REFUSED", "1m", bar(1));
    assert!(strat.got().is_empty(), "{:?}", strat.got());
}

// ---- decision 0193: the stream ceiling and the withheld recorder --------------------------------------------------

/// **A venue with a stream ceiling counts every holder's streams TOGETHER** (rule 7): the strategy lane and
/// the recorder fill OANDA's row between them, the next stream is refused for either of them with the
/// number and the cap mark (the class the tradehub does not restart on), a bar lane is REST and not
/// counted, a join of a held key opens nothing and is never refused, and a released key makes room.
#[test]
fn a_venue_with_a_stream_ceiling_counts_every_holders_streams_together() {
    let (b, log, ..) = with_strat();
    let cap = credentialed_limits::OANDA.max_streams;
    let mut strat = b.handle(Holder::Strat, "oanda");
    let mut rec = b.handle(Holder::Rec, "oanda");
    let mut first = None;
    for k in 0..cap {
        let symbol = format!("P{k}");
        let asked = if k % 2 == 0 {
            strat.subscribe_quotes(&symbol)
        } else {
            rec.subscribe_quotes(&symbol)
        };
        let id = asked.unwrap_or_else(|e| panic!("stream {k} of {cap} must fit: {e}"));
        if k == 0 {
            first = Some(id);
        }
    }
    assert_eq!(b.real_subscriptions(), cap);
    for refused in [strat.subscribe_quotes("EXTRA"), rec.subscribe_quotes("EXTRA")] {
        let text = refused.expect_err("the stream past the ceiling").to_string();
        assert!(text.contains(CAP_MARK), "a cap refusal, not a link: {text}");
        assert!(text.contains(&cap.to_string()), "it names the number: {text}");
    }
    assert_eq!(count(&log, "EXTRA"), 0, "the venue client was never asked");
    assert_eq!(b.real_subscriptions(), cap, "a refused stream leaves no hold");

    // A join of a key the other holder holds opens no stream.
    rec.subscribe_quotes("P0").expect("a join is never refused by the ceiling");
    // A bar lane is a REST poll: not a stream.
    strat.subscribe_bars("EXTRA", "1m").expect("a bar lane is not counted");
    // The strategy lane leaves P0, but the recorder still holds it: no room is made.
    strat.unsubscribe(first.expect("the first stream's id"));
    assert!(rec.subscribe_quotes("EXTRA").is_err(), "P0 is still held by the recorder's join");
    // Only the last holder out frees the stream.
    drop(rec); // the handle's drop releases everything it held
    strat.subscribe_quotes("EXTRA").expect("the recorder's streams were released: there is room");
}

/// **The recorder's OANDA feed records what the strategy lane holds, and nothing only the recorder holds**
/// (decision 0193 verdict 7, acceptance A2/A3/A4): the strategy lane's QUOTE keys (a bar-only symbol is
/// not a quote key), re-resolved on each ask; the recorder's own hold of a key never keeps it looking
/// strategy-held; OANDA is a followed venue and NOT a profile venue (`supported`, so
/// `series_cadence_gate` asks no cadence row of `vike-data`); and a venue not followed has no such feed.
#[cfg(feature = "record-oanda")]
#[test]
fn the_recorders_oanda_feed_follows_the_strategy_lanes_quote_keys() {
    use vike_datahub::recording::{build_strat_followed_feed, strat_followed, supported};
    use vike_recorder::runtime::VenueFeed;

    assert!(strat_followed() == ["oanda"] && !supported().contains(&"oanda"));
    let (b, _log, _sinks, _md, _rec, _strat) = with_strat();
    let mut strat = b.handle(Holder::Strat, "oanda");
    strat.subscribe_quotes("EURUSD").unwrap();
    let usdjpy = strat.subscribe_quotes("USDJPY").unwrap();
    strat.subscribe_bars("AUDUSD", "1m").unwrap();
    let names = |set: BTreeSet<String>| set.into_iter().collect::<Vec<_>>();

    let mut feed = build_strat_followed_feed("oanda", &b).unwrap();
    assert_eq!((feed.venue(), feed.family()), ("oanda", None));
    assert_eq!(names(feed.desired(0).unwrap()), ["EURUSD", "USDJPY"]);

    // The recorder joins USDJPY, then the strategy lane leaves it: the key is nobody's but the recorder's.
    feed.client().subscribe_quotes("USDJPY").unwrap();
    strat.unsubscribe(usdjpy);
    assert_eq!(b.real_subscriptions(), 3, "EURUSD, the recorder's USDJPY and AUDUSD's bar lane");
    // ...but it stays WANTED for two ticks (a restarting tradehub finds its stream open, and nothing is
    // released at the venue just to be asked for again a few seconds later) and is dropped on the third ask.
    assert_eq!(names(feed.desired(30_000).unwrap()), ["EURUSD", "USDJPY"]);
    assert_eq!(names(feed.desired(59_999).unwrap()), ["EURUSD", "USDJPY"]);
    assert_eq!(names(feed.desired(60_000).unwrap()), ["EURUSD"]);
    // A symbol the strategy lane takes up later joins on the next ask.
    strat.subscribe_quotes("GBPUSD").unwrap();
    assert_eq!(names(feed.desired(90_000).unwrap()), ["EURUSD", "GBPUSD"]);

    // The followed feed asks for the one stream the venue serves, whatever the profile requests.
    assert_eq!(
        feed.narrow(&vike_recorder::session::Stream::ALL),
        [vike_recorder::session::Stream::Quotes]
    );

    assert!(build_strat_followed_feed("binance", &b).is_err());
}

/// **The recorder leaving a key the strategy lane holds disturbs nothing** (acceptance A3), and the
/// strategy lane leaving a key the recorder holds closes nothing either (A4): one real stream per key
/// through both, no unsubscribe at the venue while a holder remains, no status or label to the strategy
/// lane, and the frames keep flowing to whoever stays.
#[test]
fn either_holder_leaving_a_shared_oanda_key_closes_nothing_while_the_other_stays() {
    let (b, log, sinks, _md, rec, strat) = with_strat();
    let mut s = b.handle(Holder::Strat, "oanda");
    let mut r = b.handle(Holder::Rec, "oanda");
    let held_by_strat = s.subscribe_quotes("EURUSD").unwrap();
    let held_by_rec = r.subscribe_quotes("EURUSD").unwrap();
    assert_eq!(count(&log, "quotes EURUSD"), 1, "one stream at the venue for the two holders");
    let sink = Arc::clone(&sinks.lock().unwrap()[0]);

    // A3: the recorder leaves.
    r.unsubscribe(held_by_rec);
    assert_eq!(
        count(&log, "unsubscribe"),
        0,
        "the venue is not told: the strategy lane still holds it"
    );
    sink.quote("oanda", "EURUSD", quote());
    assert_eq!(strat.got(), vec!["quote oanda EURUSD".to_string()], "and its prices keep coming");
    assert_eq!(rec.got().len(), 0, "the recorder takes nothing it no longer holds");

    // A4: the recorder is back, the strategy lane leaves.
    let held_by_rec = r.subscribe_quotes("EURUSD").unwrap();
    s.unsubscribe(held_by_strat);
    assert_eq!(count(&log, "unsubscribe"), 0, "the recorder still holds it");
    sink.quote("oanda", "EURUSD", quote());
    assert_eq!(rec.got(), vec!["quote oanda EURUSD".to_string()]);
    assert_eq!(strat.got().len(), 1, "the strategy lane left: it takes nothing more");
    // The last holder out closes the stream, once.
    r.unsubscribe(held_by_rec);
    assert_eq!(count(&log, "unsubscribe"), 1);
    assert_eq!(b.real_subscriptions(), 0);
}

/// How many streams a venue's client had open at its busiest, read from the log: a `-> <id>` subscribe opens
/// one, an `unsubscribe` closes one. The swap of rule eight must never have a key's old and new stream open
/// together.
fn most_streams_open(log: &Log, venue: &str) -> usize {
    let (mut open, mut most) = (0_usize, 0_usize);
    for line in log.lock().unwrap().iter().filter(|l| l.starts_with(venue)) {
        if line.contains("->") {
            open += 1;
            most = most.max(open);
        } else if line.contains("unsubscribe") {
            open = open.saturating_sub(1);
        }
    }
    most
}

/// **A strategy that joins an OANDA quote key only the recorder holds, and that has had no quote, is given a
/// FRESH stream** (decision 0193, review of wave A): an open stream gives a joiner no first price (OANDA's
/// on-connect snapshot and the bridge's first-price read belong to a new stream's start), so the broker
/// releases the quiet stream and asks for it afresh, in that order. The recorder keeps its hold through the
/// swap and gets the new stream's first frame too.
#[test]
fn a_strategy_joining_a_quiet_recorder_held_oanda_key_gets_a_fresh_stream() {
    let (b, log, sinks, _md, rec, strat) = with_strat();
    let mut r = b.handle(Holder::Rec, "oanda");
    let held_by_rec = r.subscribe_quotes("EURUSD").unwrap();
    assert_eq!(count(&log, "quotes EURUSD"), 1);

    let mut s = b.handle(Holder::Strat, "oanda");
    let held_by_strat = s.subscribe_quotes("EURUSD").unwrap();

    let lines = log.lock().unwrap().clone();
    let opened: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| l.contains("quotes EURUSD ->").then_some(i))
        .collect();
    let closed = lines.iter().position(|l| l == "oanda unsubscribe 1");
    assert_eq!(opened.len(), 2, "the venue is asked for the key again: {lines:?}");
    assert!(
        closed.is_some_and(|c| opened[0] < c && c < opened[1]),
        "the quiet stream is released BEFORE the fresh one is asked for: {lines:?}"
    );
    assert_eq!(b.real_subscriptions(), 1, "one stream for the key, as before");
    assert_eq!(most_streams_open(&log, "oanda"), 1, "never the old and the new together");

    // The fresh stream's first frame reaches the strategy AND the recorder, which kept its hold.
    sinks.lock().unwrap()[0].quote("oanda", "EURUSD", quote());
    assert_eq!(strat.got(), vec!["quote oanda EURUSD".to_string()]);
    assert_eq!(rec.got(), vec!["quote oanda EURUSD".to_string()]);

    // Both holders out closes the fresh stream, once.
    s.unsubscribe(held_by_strat);
    assert_eq!(count(&log, "unsubscribe 2"), 0, "the recorder still holds it");
    r.unsubscribe(held_by_rec);
    assert_eq!(count(&log, "unsubscribe 2"), 1);
}

/// **A strategy that joins a key with recent quotes joins it with no venue traffic at all**, and so does one
/// that is back right after it let the key go: the quote clock is written by a quote only the recorder took,
/// and seeded at the hand-over from the strategy lane to the recorder. Another venue's key is never swapped
/// (rule eight is for the venue whose first price belongs to a new stream).
#[test]
fn a_strategy_joining_a_key_with_recent_quotes_opens_and_closes_nothing_at_the_venue() {
    let (b, log, sinks, ..) = with_strat();
    let mut r = b.handle(Holder::Rec, "oanda");
    r.subscribe_quotes("EURUSD").unwrap();
    // A quote reaches the recorder alone: the key is active.
    sinks.lock().unwrap()[0].quote("oanda", "EURUSD", quote());
    let mut s = b.handle(Holder::Strat, "oanda");
    let first = s.subscribe_quotes("EURUSD").unwrap();
    assert_eq!(
        count(&log, "quotes EURUSD"),
        1,
        "joined the open stream: {:?}",
        log.lock().unwrap()
    );
    assert_eq!(count(&log, "unsubscribe"), 0);

    // The strategy lets the key go to the recorder and is back at once, with no quote in between: the
    // hand-over seeded the clock.
    s.unsubscribe(first);
    s.subscribe_quotes("EURUSD").unwrap();
    assert_eq!(count(&log, "quotes EURUSD"), 1, "still the one stream: {:?}", log.lock().unwrap());
    assert_eq!(count(&log, "unsubscribe"), 0);

    // Control: a venue whose first price does not wait for a new stream is never swapped, quiet or not.
    let mut rb = b.handle(Holder::Rec, "binance");
    rb.subscribe_quotes("BTCUSDT").unwrap();
    let mut sb = b.handle(Holder::Strat, "binance");
    sb.subscribe_quotes("BTCUSDT").unwrap();
    assert_eq!(count(&log, "binance quotes BTCUSDT"), 1);
    assert_eq!(count(&log, "binance unsubscribe"), 0);
}

/// **The swap never needs a stream more than the key had**: with the venue's ceiling full of quiet keys the
/// recorder holds, the strategy's join of one of them is not refused for the ceiling, the venue never has more
/// streams open than the ceiling, and a key nobody holds is still refused.
#[test]
fn the_swap_of_a_quiet_key_never_exceeds_the_ceiling() {
    let (b, log, ..) = with_strat();
    let cap = credentialed_limits::OANDA.max_streams;
    let mut r = b.handle(Holder::Rec, "oanda");
    for k in 0..cap {
        r.subscribe_quotes(&format!("P{k}")).unwrap();
    }
    assert_eq!(b.real_subscriptions(), cap);

    let mut s = b.handle(Holder::Strat, "oanda");
    s.subscribe_quotes("P0").expect("a join is never refused by the ceiling, a swap included");
    assert_eq!(b.real_subscriptions(), cap, "the same number of streams as before");
    assert!(
        most_streams_open(&log, "oanda") <= cap,
        "the venue was over its ceiling during the swap"
    );
    assert_eq!(count(&log, "quotes P0 ->"), 2, "and the key WAS swapped");
    let text = s.subscribe_quotes("EXTRA").expect_err("a new key past the ceiling").to_string();
    assert!(text.contains(CAP_MARK), "{text}");
}

/// **A swap whose fresh subscribe fails once is tried again, and the joiner is served** (second review of
/// wave A): the swap released the recorder's stream, so a failed fresh subscribe (a link that is down, the
/// gatewayhub's transient refusal behind a slow join) would leave the recorder holding a key with no stream
/// until the strategy happened to retry. One retry inside the same `acquire` carries the common case.
#[test]
fn a_swap_whose_fresh_subscribe_fails_once_is_tried_again_and_the_joiner_is_served() {
    let (b, log, ..) = with_strat();
    let mut r = b.handle(Holder::Rec, "oanda");
    r.subscribe_quotes("FLAKY1").unwrap();
    let mut s = b.handle(Holder::Strat, "oanda");
    s.subscribe_quotes("FLAKY1").expect("the fresh subscribe fails once and is asked again");
    assert_eq!(b.real_subscriptions(), 1, "the key has its stream again, for both holders");
    assert_eq!(count(&log, "quotes FLAKY1 ->"), 2, "the first stream and the one that took");
}

/// A swap whose fresh subscribe fails twice is the joiner's refusal (the retry is ONE, so a venue that is
/// really down is not asked in a loop with the venue's mutex held), the recorder's key is left without a
/// stream, and the joiner's next ask revives it.
#[test]
fn a_swap_whose_fresh_subscribe_fails_twice_is_the_joiners_refusal_and_the_next_ask_revives_the_key()
 {
    let (b, ..) = with_strat();
    let mut r = b.handle(Holder::Rec, "oanda");
    r.subscribe_quotes("FLAKY2").unwrap();
    let mut s = b.handle(Holder::Strat, "oanda");
    s.subscribe_quotes("FLAKY2").expect_err("both attempts of the swap are refused");
    assert_eq!(b.real_subscriptions(), 0, "the recorder's key has no stream until the next ask");
    s.subscribe_quotes("FLAKY2").expect("the next ask subscribes the key afresh");
    assert_eq!(b.real_subscriptions(), 1);
}

/// **A venue with no ceiling row has no ceiling**: every venue before OANDA keeps the behaviour it had.
#[test]
fn a_venue_with_no_ceiling_row_is_never_refused_by_one() {
    let (b, ..) = with_strat();
    let mut strat = b.handle(Holder::Strat, "ibkr");
    for k in 0..64 {
        strat.subscribe_quotes(&format!("S{k}")).expect("no row, no ceiling");
    }
}

/// **A stream whose last delay label is `Delayed` or `Frozen` is withheld from the recorder**, from the
/// label on and until a `RealTime` one, whether or not the recorder held the key when the label came
/// (a label is sent when first known and when it changes, never to a late joiner). The strategy lane
/// is told the label and keeps every frame: it is information for it, never a gate.
#[test]
fn a_stream_labelled_delayed_is_withheld_from_the_recorder_and_only_from_the_recorder() {
    let (b, _log, sinks, _md, rec, strat) = with_strat();
    let mut s = b.handle(Holder::Strat, "ibkr");
    let mut r = b.handle(Holder::Rec, "ibkr");
    s.subscribe_quotes("AAPL").unwrap();
    r.subscribe_quotes("AAPL").unwrap();
    let sink = Arc::clone(&sinks.lock().unwrap()[0]);
    sink.quote("ibkr", "AAPL", quote());
    assert_eq!((rec.got().len(), strat.got().len()), (1, 1));

    sink.data_delay("ibkr", "AAPL", "quotes", DataDelay::DELAYED);
    sink.quote("ibkr", "AAPL", quote());
    assert_eq!(rec.got().len(), 1, "a delayed stream is not recorded as if it were real time");
    assert_eq!(
        strat.got(),
        vec![
            "quote ibkr AAPL".to_string(),
            "delay:quotes ibkr AAPL".to_string(),
            "quote ibkr AAPL".to_string()
        ],
        "the strategy lane is told the label and keeps every frame"
    );
    sink.data_delay("ibkr", "AAPL", "quotes", DataDelay::Frozen);
    sink.quote("ibkr", "AAPL", quote());
    assert_eq!(rec.got().len(), 1, "frozen is withheld too");

    sink.data_delay("ibkr", "AAPL", "quotes", DataDelay::RealTime);
    sink.quote("ibkr", "AAPL", quote());
    assert_eq!(rec.got().len(), 2, "real time again: recorded");

    // A bar word is the strategy lane's and touches nothing of the recorder's.
    sink.data_delay("ibkr", "AAPL", "1m", DataDelay::DELAYED);
    sink.quote("ibkr", "AAPL", quote());
    assert_eq!(rec.got().len(), 3);

    // The label came BEFORE the recorder held the key: it is withheld from its first frame.
    sink.data_delay("ibkr", "TSLA", "quotes", DataDelay::DELAYED);
    s.subscribe_quotes("TSLA").unwrap();
    r.subscribe_quotes("TSLA").unwrap();
    sink.quote("ibkr", "TSLA", quote());
    assert_eq!(
        rec.got().len(),
        3,
        "a late joiner is not handed a stream it was never told is delayed"
    );
}
