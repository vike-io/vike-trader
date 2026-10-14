//! The feed broker's contract, over a scripted client. See `crates/vike-datahub/src/feeds/mod.rs`.
use std::assert_matches;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_datahub::feeds::{ClientBuilder, FeedBroker, Holder, Sharing};
use vike_marketdata::{Bar, L2Book, QuoteTick, TradeTick};
use vike_model::StreamStatus;

/// Every call the scripted clients received, in order, across every client the broker built.
type Log = Arc<Mutex<Vec<String>>>;
/// The sink each built client was handed, so a test can emit on the venue side.
type Sinks = Arc<Mutex<Vec<Arc<dyn LiveDataSink>>>>;

struct Scripted {
    venue: String,
    sink: Arc<dyn LiveDataSink>,
    log: Log,
    next: u64,
}

impl Scripted {
    fn start(&mut self, verb: &str, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        if symbol == "REFUSED" {
            return Err(LiveDataError::Subscribe("scripted refusal".into()));
        }
        self.next += 1;
        self.log.lock().unwrap().push(format!("{} {verb} {symbol} -> {}", self.venue, self.next));
        Ok(SubscriptionId(self.next))
    }
}

impl DataClient for Scripted {
    fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("bars"))
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
            Ok(Box::new(Scripted { venue: venue.into(), sink, log: Arc::clone(&l), next: 0 }))
        },
    );
    let b = FeedBroker::new(builder, |venue| {
        if venue == "polymarket" { Sharing::PerHolder } else { Sharing::Shared }
    });
    let md = Arc::new(Seen::default());
    let rec = Arc::new(Seen::default());
    b.attach_sink(Holder::Md, Arc::clone(&md) as Arc<dyn LiveDataSink>);
    b.attach_sink(Holder::Rec, Arc::clone(&rec) as Arc<dyn LiveDataSink>);
    (b, log, sinks, md, rec)
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
