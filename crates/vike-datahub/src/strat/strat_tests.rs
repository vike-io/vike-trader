//! The hub's own contracts, with no socket: the door (venue, network, slot), the key caps, and the
//! sink that fans a venue client's frames to the sessions holding them. The lane over a real socket is
//! `crates/vike-datahub/tests/strat_lane.rs`.

use std::sync::Arc;

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataSink, RecordingSink, SinkCall, StreamLabel};
use vike_marketdata::{QuoteTick, TradeTick};
use vike_model::StreamStatus;

use super::*;
use crate::feeds::routing::StratKeys;
use crate::feeds::{BrokerLane, ClientBuilder, Sharing};

fn broker() -> Arc<FeedBroker> {
    let builder: ClientBuilder = Box::new(
        |_venue: &str,
         _sink: Arc<dyn LiveDataSink>|
         -> Result<Box<dyn DataClient + Send>, String> {
            Err("no venue client in this test".into())
        },
    );
    FeedBroker::new(builder, |_| Sharing::Shared)
}

fn hub_with(limits: StratLimits, served: &[&str]) -> Arc<StratHub> {
    StratHub::new(broker(), served.iter().map(|v| (*v).to_string()).collect(), None, limits)
}

fn hub() -> Arc<StratHub> {
    hub_with(StratLimits::PRODUCTION, &["binance", "hyperliquid"])
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

fn lane(label: StreamLabel) -> BrokerLane {
    BrokerLane::Tick(label)
}

fn keys_of(lanes: &[BrokerLane]) -> StratKeys {
    let mut keys = StratKeys::default();
    for l in lanes {
        keys.set(l, true);
    }
    keys
}

// ---- the door ---------------------------------------------------------------------------------------

/// **An unserved venue is refused BY NAME**, naming what IS served, with nothing reserved.
#[test]
fn a_venue_the_lane_does_not_serve_is_refused_naming_the_served_set() {
    let hub = hub();
    let err = hub.open_session("kalshi", None).expect_err("refused");
    assert!(err.contains("kalshi") && err.contains("binance, hyperliquid"), "{err}");
    assert_eq!(hub.sessions(), 0, "a refusal reserves nothing");
}

/// **Only Hyperliquid takes a network**: a network on any other venue is refused rather than ignored (a
/// silently ignored network would put a testnet strategy on a mainnet feed), an unknown word is refused,
/// and Hyperliquid's test network is a session of its own - named by the broker as `hyperliquid@testnet`,
/// where `mainnet` and no network are the ONE main-network client.
#[test]
fn a_network_is_taken_by_hyperliquid_alone_and_its_test_network_is_a_client_of_its_own() {
    let hub = hub();
    assert!(hub.open_session("binance", None).is_ok());
    let err = hub.open_session("binance", Some("mainnet")).expect_err("binance has no network");
    assert!(err.contains("takes no network"), "{err}");
    let err = hub.open_session("binance", Some("testnet")).expect_err("binance has no network");
    assert!(err.contains("takes no network"), "{err}");
    let plain = hub.open_session("hyperliquid", None).expect("no network");
    let main = hub.open_session("hyperliquid", Some("mainnet")).expect("main network");
    let test = hub.open_session("hyperliquid", Some("testnet")).expect("test network is served");
    assert_eq!((plain.label().as_str(), main.label().as_str()), ("hyperliquid", "hyperliquid"));
    assert_eq!(test.label(), "hyperliquid@testnet");
    assert_eq!((plain.network(), main.network(), test.network()), (None, None, Some("testnet")));
    assert_eq!(test.venue(), "hyperliquid", "the venue is still the venue");
    let err = hub.open_session("hyperliquid", Some("moonnet")).expect_err("unknown");
    assert!(err.contains("unknown network") && err.contains("moonnet"), "{err}");
}

/// **The test network's keys are not the main network's**: the same symbol on `hyperliquid` and on
/// `hyperliquid@testnet` is two keys against the lane's caps (two clients feed them).
#[test]
fn the_same_symbol_on_two_networks_is_two_keys() {
    let hub = hub();
    hub.reserve_key("hyperliquid", "BTC").expect("main");
    hub.reserve_key("hyperliquid@testnet", "BTC").expect("test");
    assert_eq!(hub.keys_held(), 2);
}

/// **The lane's connection cap is a slot taken at the door and given back by the guard's drop**, however
/// the session ended.
#[test]
fn the_connection_cap_is_a_slot_the_guard_gives_back() {
    let hub =
        hub_with(StratLimits { max_stream_conns: 2, ..StratLimits::PRODUCTION }, &["binance"]);
    let a = hub.open_session("binance", None).expect("one");
    let b = hub.open_session("binance", None).expect("two");
    assert_ne!(a.id(), b.id(), "each session has its own id");
    assert_eq!(a.venue(), "binance");
    let err = hub.open_session("binance", None).expect_err("full");
    assert!(err.contains("already serving 2"), "{err}");
    assert!(
        !vike_market_stream::market::is_transient_refusal(&err),
        "a full lane is a cap: a caller that restarts itself on a transient refusal must not restart on it: {err}"
    );
    assert_eq!(hub.sessions(), 2);
    drop(a);
    assert_eq!(hub.sessions(), 1);
    assert!(hub.open_session("binance", None).is_ok(), "the freed slot is reusable");
}

// ---- the lane's own key caps --------------------------------------------------------------------------

/// **The caps count DISTINCT keys**: a key two sessions hold is one key, a refusal names the cap that is
/// full, and a released key frees its place - never against the market-data plane's caps.
#[test]
fn the_key_caps_count_distinct_keys_and_name_the_cap_that_is_full() {
    let hub = hub_with(
        StratLimits { max_keys_total: 3, max_keys_per_venue: 2, ..StratLimits::PRODUCTION },
        &["binance", "bybit"],
    );
    hub.reserve_key("binance", "A").expect("first");
    hub.reserve_key("binance", "A").expect("the same key held by a second session is one key");
    hub.reserve_key("binance", "B").expect("second distinct key on the venue");
    assert_eq!(hub.keys_held(), 2);
    match hub.reserve_key("binance", "C").expect_err("the venue cap") {
        SubErrKind::Budget { scope, held: 2, cap: 2 } => {
            assert!(scope.contains("on this venue"), "{scope}");
        }
        other => panic!("{other:?}"),
    }
    hub.reserve_key("bybit", "A").expect("another venue has its own count");
    match hub.reserve_key("bybit", "B").expect_err("the total cap") {
        SubErrKind::Budget { scope, held: 3, cap: 3 } => assert_eq!(scope, "strategy keys"),
        other => panic!("{other:?}"),
    }
    // Two holds of binance/A: one release leaves the key, the second frees it.
    hub.release_key("binance", "A");
    assert_eq!(hub.keys_held(), 3, "one session still holds binance/A");
    hub.release_key("binance", "A");
    assert_eq!(hub.keys_held(), 2);
    hub.reserve_key("binance", "C").expect("a freed place is usable");
}

// ---- the key sets and the sink that fans out ---------------------------------------------------------------

/// **The admission rules**: a quote is admitted under `Quotes` OR `Book`, a trade under `Trades`, the
/// folded book and the raw delta under `Book`, a depth snapshot under `Depth`, a bar verb by its
/// interval, a kline tick by any bar lane; and a label word or a held interval is a status word while
/// the pump's link word is not the broker's.
#[test]
fn the_key_set_admits_by_lane_interval_and_status_word() {
    let none = StratKeys::default();
    assert!(none.is_empty());
    assert!(!none.admits_quote() && !none.admits_trade() && !none.admits_book());
    assert!(!none.admits_depth() && !none.admits_bar_tick() && !none.admits_stream_word("quotes"));

    let quotes = keys_of(&[lane(StreamLabel::Quotes)]);
    assert!(quotes.admits_quote() && !quotes.admits_trade() && !quotes.admits_book());
    let book = keys_of(&[lane(StreamLabel::Book)]);
    assert!(book.admits_quote(), "a book pump may derive L1: a book holder is admitted its quotes");
    assert!(book.admits_book() && !book.admits_depth() && !book.admits_trade());
    assert!(keys_of(&[lane(StreamLabel::Trades)]).admits_trade());
    assert!(keys_of(&[lane(StreamLabel::Depth)]).admits_depth());

    let bars = keys_of(&[BrokerLane::Bars("1m".into())]);
    assert!(bars.admits_interval("1m") && !bars.admits_interval("5m"));
    assert!(bars.admits_bar_tick() && !bars.admits_quote());
    assert!(bars.admits_stream_word("1m") && !bars.admits_stream_word("5m"));
    assert!(!bars.admits_stream_word("ticks"), "the pump's link word is not the broker's");

    let mut both = keys_of(&[lane(StreamLabel::Quotes), lane(StreamLabel::Trades)]);
    both.set(&lane(StreamLabel::Quotes), false);
    assert!(!both.admits_quote() && both.admits_trade(), "a released lane's bit goes");
    let mut bars = keys_of(&[BrokerLane::Bars("1m".into()), BrokerLane::Bars("5m".into())]);
    bars.set(&BrokerLane::Bars("1m".into()), false);
    assert!(!bars.admits_interval("1m") && bars.admits_interval("5m"));
    bars.set(&BrokerLane::Bars("5m".into()), false);
    assert!(bars.is_empty());
}

fn recording() -> (Arc<RecordingSink>, Arc<dyn LiveDataSink>) {
    let rec = Arc::new(RecordingSink::default());
    let dyn_sink: Arc<dyn LiveDataSink> = rec.clone();
    (rec, dyn_sink)
}

fn verbs(rec: &RecordingSink) -> Vec<String> {
    rec.recorded()
        .into_iter()
        .map(|c| match c {
            SinkCall::Quote { symbol, .. } => format!("quote {symbol}"),
            SinkCall::Trade { symbol, .. } => format!("trade {symbol}"),
            SinkCall::StreamStatus { symbol, stream, .. } => format!("status:{stream} {symbol}"),
            other => format!("{other:?}"),
        })
        .collect()
}

/// **A frame reaches exactly the sessions that hold its key and lane.** Two sessions on one symbol with
/// different lanes are each handed only theirs; a symbol nobody holds reaches nobody; a session that
/// leaves is forgotten and a key nobody holds is dropped from the table.
#[test]
fn the_sink_fans_a_frame_to_the_sessions_that_hold_it_and_to_no_others() {
    let sink = StratSink::default();
    let (a, a_dyn) = recording();
    let (b, b_dyn) = recording();
    sink.set("binance", "X", 1, &a_dyn, keys_of(&[lane(StreamLabel::Quotes)]));
    sink.set("binance", "X", 2, &b_dyn, keys_of(&[lane(StreamLabel::Trades)]));
    sink.set("binance", "Y", 2, &b_dyn, keys_of(&[lane(StreamLabel::Quotes)]));
    assert_eq!(sink.key_count(), 2);

    sink.quote("binance", "X", quote());
    sink.trade("binance", "X", trade());
    sink.quote("binance", "Y", quote());
    sink.quote("binance", "Z", quote()); // nobody holds it
    sink.quote("bybit", "X", quote()); // another venue
    sink.stream_status("binance", "X", "quotes", StreamStatus::Live { gap_started_ts_ms: None });
    sink.stream_status("binance", "X", "trades", StreamStatus::Live { gap_started_ts_ms: None });
    sink.stream_status("binance", "X", "book", StreamStatus::Live { gap_started_ts_ms: None });

    assert_eq!(verbs(&a), vec!["quote X", "status:quotes X"], "session 1 holds X/Quotes only");
    assert_eq!(
        verbs(&b),
        vec!["trade X", "quote Y", "status:trades X"],
        "session 2 holds X/Trades and Y/Quotes"
    );

    // Narrowing a claim narrows what is delivered; an empty claim withdraws it.
    sink.set("binance", "X", 2, &b_dyn, StratKeys::default());
    sink.trade("binance", "X", trade());
    assert_eq!(verbs(&b).len(), 3, "session 2 no longer holds X");
    sink.forget(2);
    sink.quote("binance", "Y", quote());
    assert_eq!(verbs(&b).len(), 3, "a forgotten session is handed nothing");
    assert_eq!(sink.key_count(), 1, "only session 1's X is left");
    sink.forget(1);
    assert_eq!(sink.key_count(), 0, "a key nobody holds is dropped");
}

/// **A venue's pump may never reach a session through the broker's sink** - the pump's frames go
/// straight to their own session's wire sink; the broker-side sink only ever carries the lanes it was
/// asked for, and an interval is a status word only for a session holding it.
#[test]
fn a_bar_interval_status_reaches_only_the_session_holding_that_interval() {
    let sink = StratSink::default();
    let (a, a_dyn) = recording();
    let (b, b_dyn) = recording();
    sink.set("binance", "X", 1, &a_dyn, keys_of(&[BrokerLane::Bars("1m".into())]));
    sink.set("binance", "X", 2, &b_dyn, keys_of(&[BrokerLane::Bars("5m".into())]));
    sink.stream_status("binance", "X", "1m", StreamStatus::GapStart { at_ts_ms: 5 });
    sink.mark_tick("binance", "X", 1.0, 1);
    assert_eq!(a.recorded().len(), 2, "status and mark tick");
    assert_eq!(b.recorded().len(), 1, "the mark tick only: it carries no interval");
}

/// **Only the strategy lane's handle states a delivery floor through the broker**: the market-data hub's
/// and the recorder's classes are their own (the lane's default, `lane_delivery` for the recorder), so
/// their handles decline; the strategy handle remembers a floor stated before any subscribe.
#[test]
fn only_the_strategy_handle_states_a_delivery_floor() {
    let broker = broker();
    for holder in [Holder::Md, Holder::Rec] {
        let mut handle = broker.handle(holder, "binance");
        assert!(
            matches!(
                handle.set_delivery("X", StreamLabel::Depth, Delivery::Lossless),
                Err(vike_data::LiveDataError::Unsupported(_))
            ),
            "{holder:?}"
        );
    }
    let mut strat = broker.handle(Holder::Strat, "binance");
    assert!(strat.set_delivery("X", StreamLabel::Depth, Delivery::Lossless).is_ok());
    assert!(strat.set_delivery("X", StreamLabel::Depth, Delivery::Conflatable).is_ok());
}

// ---- W12b: a joiner is told where a lane stands, and a whole book is capped to the lane's ceiling -------

fn statuses(rec: &RecordingSink) -> Vec<(String, StreamStatus)> {
    rec.recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::StreamStatus { stream, status, .. } => Some((stream, status)),
            _ => None,
        })
        .collect()
}

/// **A session that joins a lane is told where it stands.** The venue's client discloses transitions only,
/// so a joiner (a tradehub back after a datahub restart whose old session ended on a `GapStart` whose
/// `Live` fell into the dead socket) hears nothing about a lane that is up: it is told `Live` when the
/// lane has disclosed nothing, the standing `GapStart` while an outage is open, and the last `Live` after
/// it. A word nobody holds is never remembered, and a key's memory goes with its last holder.
#[test]
fn a_joiner_is_told_where_the_lane_stands_and_a_key_nobody_holds_is_forgotten() {
    let sink = StratSink::default();
    let (_first, first_dyn) = recording();
    sink.set("binance", "X", 1, &first_dyn, keys_of(&[lane(StreamLabel::Book)]));

    // Nothing disclosed yet: the joiner is told the lane is up.
    let (b, b_dyn) = recording();
    sink.restate("binance", "X", "book", b_dyn.as_ref());
    assert_eq!(
        statuses(&b),
        [("book".to_string(), StreamStatus::Live { gap_started_ts_ms: None })]
    );

    // An outage the first session heard: a joiner during it is told the gap, with its stamp.
    sink.stream_status("binance", "X", "book", StreamStatus::GapStart { at_ts_ms: 40 });
    let (c, c_dyn) = recording();
    sink.restate("binance", "X", "book", c_dyn.as_ref());
    assert_eq!(statuses(&c), [("book".to_string(), StreamStatus::GapStart { at_ts_ms: 40 })]);

    // …and after the recovery, the recovery (the joiner's window, if it has one, closes).
    sink.stream_status("binance", "X", "book", StreamStatus::Live { gap_started_ts_ms: Some(40) });
    let (d, d_dyn) = recording();
    sink.restate("binance", "X", "book", d_dyn.as_ref());
    assert_eq!(
        statuses(&d),
        [("book".to_string(), StreamStatus::Live { gap_started_ts_ms: Some(40) })]
    );

    // A word that nobody holds is not remembered: its status reached no session.
    sink.stream_status("binance", "X", "trades", StreamStatus::GapStart { at_ts_ms: 7 });
    let (e, e_dyn) = recording();
    sink.restate("binance", "X", "trades", e_dyn.as_ref());
    assert_eq!(
        statuses(&e),
        [("trades".to_string(), StreamStatus::Live { gap_started_ts_ms: None })]
    );

    // The last holder leaving forgets the key's memory: a later joiner starts from `Live` again.
    sink.forget(1);
    sink.set("binance", "X", 2, &b_dyn, keys_of(&[lane(StreamLabel::Book)]));
    let (f, f_dyn) = recording();
    sink.restate("binance", "X", "book", f_dyn.as_ref());
    assert_eq!(
        statuses(&f),
        [("book".to_string(), StreamStatus::Live { gap_started_ts_ms: None })]
    );
}

/// A route sink that parks the FIRST status it is handed until the test releases it, so the test holds a
/// venue client's fan-out open at a known point: `entered` is signalled from inside the call.
struct ParkedOnFirstStatus {
    entered: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
    release: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl LiveDataSink for ParkedOnFirstStatus {
    fn seed_bars(&self, _: &str, _: &str, _: &str, _: Vec<vike_marketdata::Bar>) {}
    fn close_bar(&self, _: &str, _: &str, _: &str, _: vike_marketdata::Bar) {}
    fn forming_bar(&self, _: &str, _: &str, _: &str, _: vike_marketdata::Bar) {}
    fn mark_tick(&self, _: &str, _: &str, _: f64, _: i64) {}
    fn quote(&self, _: &str, _: &str, _: QuoteTick) {}
    fn trade(&self, _: &str, _: &str, _: TradeTick) {}
    fn book(&self, _: &str, _: &str, _: Arc<vike_marketdata::L2Book>) {}
    fn stream_status(&self, _venue: &str, _symbol: &str, _stream: &str, _status: StreamStatus) {
        if let Some(entered) = self.entered.lock().unwrap().take() {
            let _ = entered.send(());
        }
        let release = self.release.lock().unwrap().take();
        if let Some(release) = release {
            let _ = release.recv();
        }
    }
}

/// **A joiner is never restated across a status the venue client is in the middle of saying.** `stream_status`
/// fans out to the sessions and THEN records the status; `restate` used to read the record under the lock and
/// send after letting go of it, so a joiner could be sent the OLD `Live` after a session had already been told the
/// new `GapStart` (its dead-man window for a real outage closed at once: the outage missed), or a stale
/// `GapStart` after a real `Live` (a window opened on a healthy lane that trips `CancelAllAndHalt`). The fan-out
/// and the record are one step under one lock now, and `restate` reads and sends under it.
///
/// Deterministic, no sleeps: a route sink parks the venue client's fan-out half way; the restatement is asked
/// for meanwhile and must NOT complete (it waits its turn), and when the fan-out is released the joiner is told
/// the status the lane really stands at. The bound on the "does not complete" wait can only make a broken
/// build pass slowly, never a correct one fail: the restatement cannot finish while the fan-out holds the lock.
#[test]
fn a_joiner_waits_for_a_status_in_flight_and_is_told_where_the_lane_ends_up() {
    let sink = Arc::new(StratSink::default());
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let parked: Arc<dyn LiveDataSink> = Arc::new(ParkedOnFirstStatus {
        entered: std::sync::Mutex::new(Some(entered_tx)),
        release: std::sync::Mutex::new(Some(release_rx)),
    });
    sink.set("binance", "X", 1, &parked, keys_of(&[lane(StreamLabel::Book)]));

    // The venue client says `GapStart`; the fan-out is parked inside the first session's sink.
    let venue_thread = {
        let sink = Arc::clone(&sink);
        std::thread::spawn(move || {
            sink.stream_status("binance", "X", "book", StreamStatus::GapStart { at_ts_ms: 40 });
        })
    };
    entered_rx.recv().expect("the fan-out reached the first session");

    // A second session joins and is restated meanwhile.
    let (joiner, joiner_dyn) = recording();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let restate_thread = {
        let sink = Arc::clone(&sink);
        std::thread::spawn(move || {
            sink.restate("binance", "X", "book", joiner_dyn.as_ref());
            let _ = done_tx.send(());
        })
    };
    assert!(
        done_rx.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
        "the restatement completed while a status was half way through the fan-out: it read the OLD status \
         (`Live`) and told it to the joiner after the sessions were told `GapStart`"
    );

    release_tx.send(()).expect("release the fan-out");
    venue_thread.join().expect("the venue thread");
    restate_thread.join().expect("the restate thread");
    assert_eq!(
        statuses(&joiner),
        [("book".to_string(), StreamStatus::GapStart { at_ts_ms: 40 })],
        "the joiner ends where the lane stands: in the outage"
    );
}

fn book_of(levels_a_side: usize) -> Arc<vike_marketdata::L2Book> {
    use vike_marketdata::{BookLevel, L2Book};
    let bids: Vec<_> =
        (0..levels_a_side).map(|i| BookLevel::new(100_000.0 - i as f64, 1.0 + i as f64)).collect();
    let asks: Vec<_> =
        (0..levels_a_side).map(|i| BookLevel::new(100_001.0 + i as f64, 1.0 + i as f64)).collect();
    let mut book = L2Book::new(1.0);
    book.apply_snapshot(7, &bids, &asks);
    Arc::new(book)
}

/// **A whole book past the lane's level ceiling is capped to the BEST levels, a book within it is passed as
/// it is.** The frame ceiling is sized for 1000 levels a side and `WireSink::book` truncates nothing, so an
/// uncapped whole book (deribit's `book.{inst}.100ms` is the whole ladder) would be refused as
/// unsendable on every update.
#[test]
fn a_whole_book_past_the_lane_ceiling_is_capped_to_the_best_levels() {
    use vike_datahub_client::proto::SL_BOOK_LEVELS_CEILING;
    let sink = StratSink::default();
    let (a, a_dyn) = recording();
    sink.set("deribit", "BTC-PERPETUAL", 1, &a_dyn, keys_of(&[lane(StreamLabel::Book)]));

    let deep = book_of(SL_BOOK_LEVELS_CEILING + 500);
    sink.book("deribit", "BTC-PERPETUAL", Arc::clone(&deep));
    let shallow = book_of(SL_BOOK_LEVELS_CEILING);
    sink.book("deribit", "BTC-PERPETUAL", Arc::clone(&shallow));

    let books = a.books();
    assert_eq!(books.len(), 2);
    let capped = &books[0].2;
    assert_eq!(
        (capped.bid_levels(), capped.ask_levels()),
        (SL_BOOK_LEVELS_CEILING, SL_BOOK_LEVELS_CEILING)
    );
    assert_eq!(capped.best_bid(), deep.best_bid(), "the BEST levels are kept");
    assert_eq!(capped.best_ask(), deep.best_ask());
    assert_eq!(capped.top_n(SL_BOOK_LEVELS_CEILING), deep.top_n(SL_BOOK_LEVELS_CEILING));
    assert_eq!((capped.last_seq, capped.tick_size), (deep.last_seq, deep.tick_size));
    let whole = &books[1].2;
    assert_eq!(
        whole.top_n(usize::MAX),
        shallow.top_n(usize::MAX),
        "a book at the ceiling is whole"
    );
}
