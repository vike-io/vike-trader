//! The fxcm price stream over a SCRIPTED session (docs-shaped Offers rows; nothing here has met a ForexConnect
//! session): nothing is asked of the venue at build, a quote is judged by the desk and labelled real-time once,
//! every refusal is in words, and a pair let go stops being read. The desk's own polling rules are
//! `crates/bridges/fxcm/src/quotes_tests.rs`'s; the real registry and the engine's money are
//! `crates/bridges/fxcm/tests/fxcm_price_lane_money.rs`'s.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_data::RecordingSink;
use vike_fxcm::home_conversion::Offer;
use vike_fxcm::quotes::OffersSource;

use super::*;

fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn offer(name: &str, bid: f64, ask: f64) -> Offer {
    Offer {
        instrument: name.to_string(),
        bid,
        ask,
        digits: 5,
        instrument_type: 1,
        contract_currency: name.split('/').next().unwrap_or_default().to_string(),
        contract_multiplier: 1.0,
        subscription: "T".to_string(),
        trading_status: "O".to_string(),
        bid_tradable: "T".to_string(),
        ask_tradable: "T".to_string(),
        base_unit: 1000,
        quote_age_ms: Some(0),
        terms: None,
    }
}

/// The session: a table that can change, whether it exists, and how often it was asked.
struct Session {
    table: Mutex<Vec<Offer>>,
    up: Mutex<bool>,
    asked: AtomicUsize,
}

impl Session {
    fn up(table: Vec<Offer>) -> Arc<Self> {
        Arc::new(Self {
            table: Mutex::new(table),
            up: Mutex::new(true),
            asked: AtomicUsize::new(0),
        })
    }
}

impl OffersSource for Session {
    fn ready(&self) -> Result<(), String> {
        if *self.up.lock().unwrap() {
            Ok(())
        } else {
            Err("fxcm: no reconcile session is running for the default account".to_string())
        }
    }

    fn offers(&self) -> Result<Vec<Offer>, String> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        Ok(self.table.lock().unwrap().clone())
    }
}

fn standard() -> Vec<Offer> {
    vec![
        offer("EUR/USD", 1.0998, 1.1002),
        offer("USD/JPY", 150.08, 150.12),
        offer("GBP/USD", 1.3498, 1.3502),
    ]
}

fn desk(session: &Arc<Session>) -> Arc<PriceDesk> {
    let source: Arc<dyn OffersSource> = Arc::clone(session) as Arc<dyn OffersSource>;
    PriceDesk::paced(source, Duration::from_millis(5), Duration::from_millis(20))
}

fn stream(session: &Arc<Session>) -> (FxcmQuotes, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    (FxcmQuotes::on(&desk(session), Arc::clone(&sink) as Arc<dyn LiveDataSink>), sink)
}

#[test]
fn building_the_stream_asks_the_venue_nothing() {
    let session = Session::up(standard());
    let (_stream, sink) = stream(&session);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        session.asked.load(Ordering::SeqCst),
        0,
        "lazy: nothing is read until a symbol is asked for"
    );
    assert!(sink.recorded().is_empty());
}

/// **The first price**: a pair asked for is read off the table and reaches the sink two-sided, labelled
/// real-time once, in the engine's spelling and with both stamps.
#[test]
fn a_subscription_prices_the_pair_and_the_quote_is_labelled_real_time_once() {
    let session = Session::up(standard());
    let (mut stream, sink) = stream(&session);
    let id = stream.subscribe_quotes("usdjpy").expect("a session is up");
    assert_eq!(stream.subscribe_quotes("USDJPY").expect("idempotent"), id);
    wait_for("two quotes", || sink.quotes().len() >= 2);
    let calls = sink.calls();
    assert_eq!(
        calls[0], "data_delay:fxcm:USDJPY:quotes:RealTime",
        "the label goes out first, once"
    );
    assert_eq!(calls.iter().filter(|c| c.starts_with("data_delay")).count(), 1, "{calls:?}");
    assert_eq!(calls[1], "quote:fxcm:USDJPY:150.08/150.12:0x0");
    let quotes = sink.quotes();
    assert!(quotes[0].2.local_ts > 0 && quotes[0].2.ts > 0, "both stamps are carried");
    assert!(quotes[0].2.ts <= quotes[0].2.local_ts, "a quote is never stamped after its receipt");
    assert_eq!(quotes[0].2.symbol, "USDJPY");
    assert!(sink.quotes().iter().all(|q| q.1 == "USDJPY"), "only the pair asked for is priced");
}

/// A closed market and a frozen quote are no quote, the price returns with the market, and a pair the table
/// does not list is never priced.
#[test]
fn a_closed_stale_or_missing_pair_emits_nothing_and_the_price_returns() {
    let mut closed = offer("EUR/USD", 1.0998, 1.1002);
    closed.trading_status = "C".to_string();
    let mut frozen = offer("GBP/USD", 1.3498, 1.3502);
    frozen.quote_age_ms = Some(121_000);
    let session = Session::up(vec![closed, frozen]);
    let (mut stream, sink) = stream(&session);
    for pair in ["EURUSD", "GBPUSD", "AUDNZD"] {
        stream.subscribe_quotes(pair).expect("a subscription is not a price");
    }
    wait_for("several reads", || session.asked.load(Ordering::SeqCst) >= 4);
    assert!(sink.recorded().is_empty(), "no price: {:?}", sink.calls());
    *session.table.lock().unwrap() = standard();
    wait_for("the market's return", || sink.quotes().len() >= 2);
    assert!(sink.quotes().iter().all(|q| q.1 == "EURUSD" || q.1 == "GBPUSD"));
}

#[test]
fn every_refusal_is_in_words() {
    // no session at all
    let session = Session::up(standard());
    *session.up.lock().unwrap() = false;
    let (mut stream, _sink) = stream(&session);
    let why = stream.subscribe_quotes("EURUSD").expect_err("no session").to_string();
    assert!(why.contains("no reconcile session"), "{why}");
    assert_eq!(session.asked.load(Ordering::SeqCst), 0, "a second login or read is never tried");

    // a spelling no FXCM instrument has (a space): refused before anything is asked. (A name like `US30` is an
    // instrument now, and a row the table lacks is the poll's to say, not the subscription's.)
    *session.up.lock().unwrap() = true;
    let why = stream.subscribe_quotes("EUR USD").expect_err("not a name").to_string();
    assert!(why.contains("not an instrument name"), "{why}");

    // every other verb
    assert!(matches!(stream.subscribe_bars("EURUSD", "1m"), Err(LiveDataError::Unsupported(_))));
    assert!(matches!(stream.subscribe_trades("EURUSD"), Err(LiveDataError::Unsupported(_))));
    assert!(matches!(stream.subscribe_book("EURUSD"), Err(LiveDataError::Unsupported(_))));
}

/// **The cap is the desk's**, and a pair let go gives its place back at once.
#[test]
fn the_cap_is_the_desks_and_an_unsubscribe_frees_a_place() {
    let session = Session::up(standard());
    let (mut stream, _sink) = stream(&session);
    let letters = ('A'..='Q').map(|c| format!("EUR{c}{c}{c}")).collect::<Vec<_>>();
    let mut ids = Vec::new();
    for pair in &letters[..vike_fxcm::quotes::MAX_PRICED_SYMBOLS] {
        ids.push(stream.subscribe_quotes(pair).unwrap_or_else(|e| panic!("{pair}: {e}")));
    }
    let why = stream.subscribe_quotes(&letters[16]).expect_err("past the cap").to_string();
    assert!(why.contains("16 pairs"), "{why}");
    stream.unsubscribe(ids[2]);
    stream.subscribe_quotes(&letters[16]).expect("the released place is free");
}

/// **Idle release**: once the last pair is let go the venue is not read again; a fresh subscription labels afresh.
#[test]
fn an_unsubscribe_stops_the_reads_and_the_quotes_and_a_resubscribe_labels_again() {
    let session = Session::up(standard());
    let (mut stream, sink) = stream(&session);
    let id = stream.subscribe_quotes("EURUSD").expect("subscribed");
    wait_for("the first quote", || !sink.quotes().is_empty());
    stream.unsubscribe(id);
    stream.unsubscribe(id); // unknown now: a no-op
    std::thread::sleep(Duration::from_millis(100)); // a read already out may still land
    let (asked, quotes) = (session.asked.load(Ordering::SeqCst), sink.quotes().len());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(session.asked.load(Ordering::SeqCst), asked, "nothing wanted, nothing read");
    assert_eq!(sink.quotes().len(), quotes, "a stopped symbol is not emitted");

    stream.subscribe_quotes("EURUSD").expect("again");
    wait_for("the re-subscribed quote", || sink.quotes().len() > quotes);
    let labels = sink.calls().iter().filter(|c| c.starts_with("data_delay")).count();
    assert_eq!(labels, 2, "a fresh subscription is labelled afresh");
}

/// A session whose first `left` reads panic (a bug in the read), then answers as `Session` does.
struct PanicsFirst {
    inner: Arc<Session>,
    left: AtomicUsize,
}

impl OffersSource for PanicsFirst {
    fn ready(&self) -> Result<(), String> {
        self.inner.ready()
    }

    fn offers(&self) -> Result<Vec<Offer>, String> {
        assert!(
            self.left
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_err(),
            "a bug in the session read"
        );
        self.inner.offers()
    }
}

/// **A poller that died restarts itself once, and a repeat ask for a pair the stream ALREADY streams restarts it
/// when it died again inside the throttle** (the review's F6 and the cold re-review's F-4): the repeat ask used to
/// return the old id before it reached the desk, so nothing ever restarted a poller and the pair stayed silent for
/// the life of the stream. Two reads panic here: the first death is restarted by the poller's own guard, the
/// second (inside the ten-second throttle) is not, so the pair is silent until the repeat ask.
#[test]
fn a_repeat_subscription_of_a_streaming_pair_restarts_a_poller_that_died_again() {
    let session = Session::up(standard());
    let source = Arc::new(PanicsFirst { inner: Arc::clone(&session), left: AtomicUsize::new(2) });
    let desk = PriceDesk::paced(
        Arc::clone(&source) as Arc<dyn OffersSource>,
        Duration::from_millis(5),
        Duration::from_millis(20),
    );
    let sink = Arc::new(RecordingSink::default());
    let mut stream = FxcmQuotes::on(&desk, Arc::clone(&sink) as Arc<dyn LiveDataSink>);
    let id = stream.subscribe_quotes("EURUSD").expect("subscribed");
    wait_for("both reads to panic", || source.left.load(Ordering::SeqCst) == 0);
    std::thread::sleep(Duration::from_millis(100));
    assert!(sink.quotes().is_empty(), "no poller, no price");
    assert_eq!(stream.subscribe_quotes("EURUSD").expect("a repeat ask"), id, "the same id");
    wait_for("the restarted poller's quote", || !sink.quotes().is_empty());
}

#[test]
fn a_shutdown_stops_every_pair_and_a_fresh_stream_starts_clean() {
    let session = Session::up(standard());
    let desk = desk(&session);
    let sink = Arc::new(RecordingSink::default());
    let mut stream = FxcmQuotes::on(&desk, Arc::clone(&sink) as Arc<dyn LiveDataSink>);
    stream.subscribe_quotes("EURUSD").expect("a");
    stream.subscribe_quotes("GBPUSD").expect("b");
    stream.begin_shutdown();
    stream.shutdown();
    let why = stream.subscribe_quotes("EURUSD").expect_err("closed").to_string();
    assert!(why.contains("closed"), "{why}");
    drop(stream);
    let mut next = FxcmQuotes::on(&desk, Arc::new(RecordingSink::default()));
    next.subscribe_quotes("EURUSD").expect("starts clean");
}

/// A displaced stream is never answered as if it still priced: a pair it was already streaming is refused too, so
/// the tradehub behind it learns the feed is gone on its next ask.
#[test]
fn a_displaced_stream_is_refused_even_for_a_pair_it_already_streams() {
    let session = Session::up(standard());
    let desk = desk(&session);
    let (old_sink, newer_sink) =
        (Arc::new(RecordingSink::default()), Arc::new(RecordingSink::default()));
    let mut old = FxcmQuotes::on(&desk, Arc::clone(&old_sink) as Arc<dyn LiveDataSink>);
    old.subscribe_quotes("EURUSD").expect("streaming");
    assert!(old.subscribe_quotes("EURUSD").is_ok(), "idempotent while it is the consumer");

    let mut newer = FxcmQuotes::on(&desk, Arc::clone(&newer_sink) as Arc<dyn LiveDataSink>);
    let why = old.subscribe_quotes("EURUSD").expect_err("displaced").to_string();
    assert!(why.contains("replaced"), "{why}");
    newer.subscribe_quotes("GBPUSD").expect("the newer one asks for itself");
    wait_for("the newer stream's quote", || !newer_sink.quotes().is_empty());
    drop(old); // the old one closing touches nothing of the new one
    assert!(newer.subscribe_quotes("GBPUSD").is_ok());
    assert!(
        old_sink.quotes().iter().all(|q| q.1 == "EURUSD"),
        "the displaced stream is not fed the newer's pair"
    );
}
