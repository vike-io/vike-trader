//! The fxcm price stream over a SCRIPTED session (docs-shaped Offers rows; nothing here has met a ForexConnect
//! session): nothing is asked of the venue at build, a quote is judged by the desk and labelled real-time once,
//! every refusal is in words, and a pair let go stops being read. The desk's own polling rules are
//! `crates/bridges/fxcm/src/quotes_tests.rs`'s; the real registry and the engine's money are
//! `crates/bridges/fxcm/tests/fxcm_price_lane_money.rs`'s.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use vike_data::RecordingSink;
use vike_fxcm::home_conversion::Offer;
use vike_fxcm::quotes::OffersSource;
use vike_fxcm::test_support::{PanicsFirst, offer};

use std::sync::{Arc, Mutex};

use vike_data::{DataClient, LiveDataError, LiveDataSink};

use vike_bridge_core::price_stream::PriceStream;
use vike_fxcm::quotes::PriceDesk;
use vike_gatewayhub_client::credentialed_limits::stream_limits_for;
use vike_log::poll::wait_for;
use vike_market_stream::market::CAP_MARK;
use vike_model::StreamStatus;

use crate::data::session_guard::SessionGuard;

/// How long a wait in this file allows before `vike_log::poll::wait_for` fails it.
const WAIT_FOR: Duration = Duration::from_secs(10);

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

fn stream(session: &Arc<Session>) -> (PriceStream<PriceDesk>, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    (PriceStream::on(&desk(session), Arc::clone(&sink) as Arc<dyn LiveDataSink>), sink)
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
    wait_for(WAIT_FOR, "two quotes", || sink.quotes().len() >= 2);
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
    wait_for(WAIT_FOR, "several reads", || session.asked.load(Ordering::SeqCst) >= 4);
    assert!(sink.recorded().is_empty(), "no price: {:?}", sink.calls());
    *session.table.lock().unwrap() = standard();
    wait_for(WAIT_FOR, "the market's return", || sink.quotes().len() >= 2);
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

    // every other verb, and a closed stream, in this venue's own words
    let only = |lane: &str| format!("fxcm's price stream carries quotes only (no {lane})");
    let words = |e: LiveDataError| match e {
        LiveDataError::Unsupported(w) => w.to_string(),
        other => panic!("{other:?}"),
    };
    assert_eq!(words(stream.subscribe_bars("EURUSD", "1m").expect_err("bars")), only("bars"));
    assert_eq!(words(stream.subscribe_trades("EURUSD").expect_err("trades")), only("trades"));
    assert_eq!(words(stream.subscribe_book("EURUSD").expect_err("book")), only("book"));
    stream.shutdown();
    let closed = stream.subscribe_quotes("EURUSD").expect_err("closed");
    assert!(
        matches!(&closed, LiveDataError::Subscribe(w) if w == "fxcm: the price stream is closed"),
        "{closed:?}"
    );
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
    wait_for(WAIT_FOR, "the first quote", || !sink.quotes().is_empty());
    stream.unsubscribe(id);
    stream.unsubscribe(id); // unknown now: a no-op
    std::thread::sleep(Duration::from_millis(100)); // a read already out may still land
    let (asked, quotes) = (session.asked.load(Ordering::SeqCst), sink.quotes().len());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(session.asked.load(Ordering::SeqCst), asked, "nothing wanted, nothing read");
    assert_eq!(sink.quotes().len(), quotes, "a stopped symbol is not emitted");

    stream.subscribe_quotes("EURUSD").expect("again");
    wait_for(WAIT_FOR, "the re-subscribed quote", || sink.quotes().len() > quotes);
    let labels = sink.calls().iter().filter(|c| c.starts_with("data_delay")).count();
    assert_eq!(labels, 2, "a fresh subscription is labelled afresh");
}

/// **A poller that died restarts itself once, and a repeat ask for a pair the stream ALREADY streams restarts it
/// when it died again inside the throttle** (the review's F6 and the cold re-review's F-4): the repeat ask used to
/// return the old id before it reached the desk, so nothing ever restarted a poller and the pair stayed silent for
/// the life of the stream. Two reads panic here: the first death is restarted by the poller's own guard, the
/// second (inside the ten-second throttle) is not, so the pair is silent until the repeat ask.
#[test]
fn a_repeat_subscription_of_a_streaming_pair_restarts_a_poller_that_died_again() {
    let session = Session::up(standard());
    let source = PanicsFirst::first(2, Arc::clone(&session));
    let desk = PriceDesk::paced(
        Arc::clone(&source) as Arc<dyn OffersSource>,
        Duration::from_millis(5),
        Duration::from_millis(20),
    );
    let sink = Arc::new(RecordingSink::default());
    let mut stream = PriceStream::on(&desk, Arc::clone(&sink) as Arc<dyn LiveDataSink>);
    let id = stream.subscribe_quotes("EURUSD").expect("subscribed");
    wait_for(WAIT_FOR, "both reads to panic", || source.spent());
    std::thread::sleep(Duration::from_millis(100));
    assert!(sink.quotes().is_empty(), "no poller, no price");
    assert_eq!(stream.subscribe_quotes("EURUSD").expect("a repeat ask"), id, "the same id");
    wait_for(WAIT_FOR, "the restarted poller's quote", || !sink.quotes().is_empty());
}

#[test]
fn a_shutdown_stops_every_pair_and_a_fresh_stream_starts_clean() {
    let session = Session::up(standard());
    let desk = desk(&session);
    let sink = Arc::new(RecordingSink::default());
    let mut stream = PriceStream::on(&desk, Arc::clone(&sink) as Arc<dyn LiveDataSink>);
    stream.subscribe_quotes("EURUSD").expect("a");
    stream.subscribe_quotes("GBPUSD").expect("b");
    stream.begin_shutdown();
    stream.shutdown();
    let why = stream.subscribe_quotes("EURUSD").expect_err("closed").to_string();
    assert!(why.contains("closed"), "{why}");
    drop(stream);
    let mut next = PriceStream::on(&desk, Arc::new(RecordingSink::default()));
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
    let mut old = PriceStream::on(&desk, Arc::clone(&old_sink) as Arc<dyn LiveDataSink>);
    old.subscribe_quotes("EURUSD").expect("streaming");
    assert!(old.subscribe_quotes("EURUSD").is_ok(), "idempotent while it is the consumer");

    let mut newer = PriceStream::on(&desk, Arc::clone(&newer_sink) as Arc<dyn LiveDataSink>);
    let why = old.subscribe_quotes("EURUSD").expect_err("displaced").to_string();
    assert!(why.contains("replaced"), "{why}");
    newer.subscribe_quotes("GBPUSD").expect("the newer one asks for itself");
    wait_for(WAIT_FOR, "the newer stream's quote", || !newer_sink.quotes().is_empty());
    drop(old); // the old one closing touches nothing of the new one
    assert!(newer.subscribe_quotes("GBPUSD").is_ok());
    assert!(
        old_sink.quotes().iter().all(|q| q.1 == "EURUSD"),
        "the displaced stream is not fed the newer's pair"
    );
}

// ---- behind the session guard (decision 0193 verdicts 4 and 6) ---------------------------------------------------

/// **The row's ceiling is the desk's own cap, one number**, so neither refuses first (the bridge's constant cannot be
/// named by the crate the table lives in), and it spaces nothing: a subscribe opens no connection.
#[test]
fn the_fxcm_row_is_the_desks_own_cap_and_makes_no_subscribe_wait() {
    let row = stream_limits_for("fxcm").expect("fxcm has a row");
    assert_eq!(row.max_streams, vike_fxcm::quotes::MAX_PRICED_SYMBOLS);
    assert_eq!(row.min_open_spacing, Duration::ZERO);
}

/// One session the way `CredentialedLane::open_in_session` makes it: the stream is BUILT first (which is what takes
/// the desk's consumer place), then admitted.
fn admitted(
    guard: &SessionGuard,
    desk: &Arc<PriceDesk>,
    id: u64,
) -> (Box<dyn DataClient + Send>, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let wired = Arc::clone(&sink) as Arc<dyn LiveDataSink>;
    let stream = Box::new(PriceStream::on(desk, Arc::clone(&wired)));
    (guard.admit("fxcm", id, wired, stream), sink)
}

/// **A newer session replaces the older, the older is told, and the desk never has two consumers.** The older
/// discloses a gap on every key it held and refuses naming the newer. ⚠ Where the guard's rule would free it once the
/// replacing session has ended (decision 0193 verdict 4), this venue's source keeps refusing: a replaced `Listening`
/// never comes back, so a stale session cannot take the desk from whoever opens next. A third session starts clean.
#[test]
fn a_replaced_session_is_told_and_stays_refused_after_its_replacement_ended() {
    let session = Session::up(standard());
    let desk = desk(&session);
    let guard = SessionGuard::new(stream_limits_for);
    let (mut older, older_sink) = admitted(&guard, &desk, 1);
    older.subscribe_quotes("EURUSD").expect("the first session");
    wait_for(WAIT_FOR, "the older session's quote", || !older_sink.quotes().is_empty());

    let (mut newer, newer_sink) = admitted(&guard, &desk, 2);
    wait_for(WAIT_FOR, "the older session's gap", || {
        older_sink
            .stream_statuses()
            .iter()
            .any(|s| s.1 == "EURUSD" && matches!(s.3, StreamStatus::GapStart { .. }))
    });
    let why = older.subscribe_quotes("GBPUSD").expect_err("replaced").to_string();
    assert!(why.contains("replaced by session 2"), "{why}");
    newer.subscribe_quotes("GBPUSD").expect("the newer session");
    wait_for(WAIT_FOR, "the newer session's quote", || !newer_sink.quotes().is_empty());
    assert!(newer_sink.quotes().iter().all(|q| q.1 == "GBPUSD"), "the older's wants went with it");

    newer.shutdown();
    drop(newer);
    let why = older.subscribe_quotes("GBPUSD").expect_err("the desk's own refusal").to_string();
    assert!(why.contains("replaced by a newer one"), "{why}");

    let (mut third, third_sink) = admitted(&guard, &desk, 3);
    third.subscribe_quotes("USDJPY").expect("a third session starts clean");
    wait_for(WAIT_FOR, "the third session's quote", || !third_sink.quotes().is_empty());
}

/// **At the ceiling the refusal is the guard's cap (the class the tradehub does not restart on), never the desk's own
/// words, and a released pair's place is free for the very next ask**, because the guard makes the queued release
/// before the open: the desk's cap and the row's ceiling are one number, so neither is met first.
#[test]
fn at_the_ceiling_the_refusal_is_a_cap_and_a_released_place_is_free_for_the_next_ask() {
    let session = Session::up(standard());
    let desk = desk(&session);
    let guard = SessionGuard::new(stream_limits_for);
    let (mut client, _sink) = admitted(&guard, &desk, 1);
    let pairs = ('A'..='Q').map(|c| format!("EUR{c}{c}{c}")).collect::<Vec<_>>();
    let cap = vike_fxcm::quotes::MAX_PRICED_SYMBOLS;
    let mut ids = Vec::new();
    for pair in &pairs[..cap] {
        ids.push(client.subscribe_quotes(pair).unwrap_or_else(|e| panic!("{pair}: {e}")));
    }

    let why = client.subscribe_quotes(&pairs[cap]).expect_err("past the ceiling").to_string();
    assert!(why.contains(CAP_MARK) && why.contains(&format!("({cap})")), "{why}");

    client.unsubscribe(ids[2]);
    client.subscribe_quotes(&pairs[cap]).expect("the release is made before the open");
}
