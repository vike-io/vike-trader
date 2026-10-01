//! The feed broker's contract, over a scripted client. See `crates/vike-datahub/src/feeds/mod.rs`.
use std::sync::{Arc, Mutex};

use vike_data::live::{DataClient, LiveDataError, LiveDataSink, StreamStatus, SubscriptionId};
use vike_datahub::feeds::{ClientBuilder, FeedBroker, Holder, Sharing};
use vike_model::{Bar, L2Book, QuoteTick, TradeTick};

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
        _: Vec<vike_model::BookLevel>,
        _: Vec<vike_model::BookLevel>,
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
    assert!(matches!(r.subscribe_bars("BTCUSDT", "1m"), Err(LiveDataError::Unsupported(_))));
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
