//! The price handle over a SCRIPTED data client: one dial for N symbols, a failed dial remembered, the cap,
//! and what counts as a price (a real-time two-sided quote and nothing else).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use vike_data::{LiveDataError, SubscriptionId};

use super::*;

const AAPL: &str = "AAPL.SMART.USD";

/// A data client that records the quote streams it was asked for.
struct Scripted {
    asked: Arc<Mutex<Vec<String>>>,
    closed: Arc<AtomicBool>,
    refuse: bool,
}

impl DataClient for Scripted {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("bars"))
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        if self.refuse {
            return Err(LiveDataError::Subscribe("no such contract".into()));
        }
        let mut asked = self.asked.lock().expect("asked");
        asked.push(symbol.to_string());
        Ok(SubscriptionId(asked.len() as u64))
    }
    fn subscribe_trades(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("trades"))
    }
    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("book"))
    }
    fn unsubscribe(&mut self, _id: SubscriptionId) {}
    fn shutdown(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// A handle over a scripted dial: how many dials were made, what the client was asked, whether the client
/// was closed, and a switch that makes the next dial fail.
struct Rig {
    handle: QuoteHandle,
    dials: Arc<AtomicUsize>,
    asked: Arc<Mutex<Vec<String>>>,
    closed: Arc<AtomicBool>,
    dial_fails: Arc<AtomicBool>,
}

fn rig(refuse: bool) -> Rig {
    let dials = Arc::new(AtomicUsize::new(0));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(AtomicBool::new(false));
    let dial_fails = Arc::new(AtomicBool::new(false));
    let (d, a, c, f) = (dials.clone(), asked.clone(), closed.clone(), dial_fails.clone());
    let dial: Dial = Box::new(move |_sink| {
        d.fetch_add(1, Ordering::SeqCst);
        if f.load(Ordering::SeqCst) {
            return Err(IbkrError::Connect("gateway took the socket and never answered".into()));
        }
        Ok(Box::new(Scripted { asked: a.clone(), closed: c.clone(), refuse }))
    });
    Rig { handle: QuoteHandle::new(dial), dials, asked, closed, dial_fails }
}

fn quote(bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts: 1, local_ts: 1, bid, ask, bid_size: 0.0, ask_size: 0.0, symbol: String::new() }
}

fn feed(board: &QuoteBoard, symbol: &str, delay: Option<DataDelay>, bid: f64, ask: f64) {
    if let Some(delay) = delay {
        board.data_delay("ibkr", symbol, "quotes", delay);
    }
    board.quote("ibkr", symbol, quote(bid, ask));
}

/// N symbols are ONE dial and N subscriptions, however many times each is asked: the second ask of a symbol
/// subscribes nothing.
#[test]
fn many_symbols_are_one_dial() {
    let rig = rig(false);
    for symbol in [AAPL, "TSLA.SMART.USD", "MSFT.SMART.USD", AAPL, "tsla.smart.usd"] {
        assert_eq!(rig.handle.price(symbol, true), Ok(None), "nothing has arrived yet");
    }
    assert_eq!(rig.dials.load(Ordering::SeqCst), 1);
    assert_eq!(*rig.asked.lock().expect("asked"), [AAPL, "TSLA.SMART.USD", "MSFT.SMART.USD"]);
    assert_eq!(rig.handle.priced().len(), 3);
}

/// A read starts nothing: no dial, no subscription, no entry for a symbol nobody subscribed.
#[test]
fn a_read_dials_and_subscribes_nothing() {
    let rig = rig(false);
    assert_eq!(rig.handle.price(AAPL, false), Ok(None));
    assert_eq!(rig.dials.load(Ordering::SeqCst), 0);
    assert!(rig.handle.priced().is_empty());
    assert!(rig.handle.board.symbols.lock().expect("board").is_empty());
}

/// A failed dial is remembered: the asks inside the window answer the same sentence and make no dial, so a
/// burst of orders on a dead gateway is not a burst of dials; the symbol is not recorded as subscribed.
#[test]
fn a_failed_dial_is_remembered_and_not_repeated() {
    let rig = rig(false);
    rig.dial_fails.store(true, Ordering::SeqCst);
    let first = rig.handle.price(AAPL, true).expect_err("the dial failed");
    assert!(
        first.contains("no market-data connection") && first.contains("never answered"),
        "{first}"
    );
    for _ in 0..5 {
        assert_eq!(rig.handle.price("TSLA.SMART.USD", true), Err(first.clone()));
    }
    assert_eq!(rig.dials.load(Ordering::SeqCst), 1, "one dial for six asks");
    assert!(rig.handle.priced().is_empty());
}

/// Past the window the dial is tried again, and a client that came up serves the symbol.
#[test]
fn the_dial_is_tried_again_after_the_window() {
    let rig = rig(false);
    rig.dial_fails.store(true, Ordering::SeqCst);
    assert!(rig.handle.price(AAPL, true).is_err());
    rig.dial_fails.store(false, Ordering::SeqCst);
    // Age the remembered failure past the window without sleeping.
    let mut state = rig.handle.state.lock().expect("state");
    let aged =
        Instant::now().checked_sub(REDIAL_AFTER + Duration::from_secs(1)).expect("an old instant");
    state.failed = state.failed.take().map(|(_, why)| (aged, why));
    drop(state);
    assert_eq!(rig.handle.price(AAPL, true), Ok(None));
    assert_eq!(rig.dials.load(Ordering::SeqCst), 2);
}

/// A subscription the client refuses is an error naming the symbol and is not recorded.
#[test]
fn a_refused_subscription_is_not_recorded() {
    let rig = rig(true);
    let why = rig.handle.price(AAPL, true).expect_err("refused");
    assert!(why.contains(AAPL) && why.contains("no such contract"), "{why}");
    assert!(rig.handle.priced().is_empty());
}

/// The engine's own cap on admitted symbols bounds the market-data lines the handle opens; one past it is
/// refused and the client is not asked.
#[test]
fn the_symbols_priced_are_capped_at_the_engines_cap() {
    let rig = rig(false);
    let cap = vike_exec::MAX_EXTRA_SYMBOLS;
    for n in 0..cap {
        assert_eq!(rig.handle.price(&format!("S{n}.SMART.USD"), true), Ok(None));
    }
    let why = rig.handle.price("ONEMORE.SMART.USD", true).expect_err("past the cap");
    assert!(why.contains(&format!("at most {cap} symbols")), "{why}");
    assert_eq!(rig.asked.lock().expect("asked").len(), cap);
    // A symbol already priced is still answered.
    assert_eq!(rig.handle.price("S0.SMART.USD", true), Ok(None));
}

/// The price is the MID of a two-sided quote of a stream labelled REAL-TIME.
#[test]
fn a_real_time_two_sided_quote_is_a_price() {
    let rig = rig(false);
    assert_eq!(rig.handle.price(AAPL, true), Ok(None));
    feed(&rig.handle.board, AAPL, Some(DataDelay::RealTime), 149.98, 150.02);
    let px = rig.handle.price(AAPL, false).expect("a price").expect("one arrived");
    assert!((px - 150.0).abs() < 1e-9, "{px}");
}

/// Delayed, frozen and unlabelled streams are no price, each named; a one-sided or crossed quote is ignored.
#[test]
fn a_delayed_frozen_or_unlabelled_stream_is_no_mark() {
    let rig = rig(false);
    feed(&rig.handle.board, AAPL, Some(DataDelay::DELAYED), 149.0, 151.0);
    let why = rig.handle.price(AAPL, false).expect_err("delayed");
    assert!(why.contains("delayed") && why.contains("entitlement"), "{why}");

    feed(&rig.handle.board, AAPL, Some(DataDelay::Frozen), 149.0, 151.0);
    assert!(rig.handle.price(AAPL, false).expect_err("frozen").contains("frozen"));

    feed(&rig.handle.board, "TSLA.SMART.USD", None, 249.0, 251.0);
    let why = rig.handle.price("TSLA.SMART.USD", false).expect_err("unlabelled");
    assert!(why.contains("no real-time label"), "{why}");

    for (bid, ask) in [(0.0, 151.0), (149.0, 0.0), (151.0, 149.0), (f64::NAN, 151.0)] {
        feed(&rig.handle.board, "MSFT.SMART.USD", Some(DataDelay::RealTime), bid, ask);
        assert_eq!(rig.handle.price("MSFT.SMART.USD", false), Ok(None), "{bid}/{ask}");
    }
}

/// A quote older than the age bound is "none yet", not a stale mark.
#[test]
fn an_old_quote_is_none_yet() {
    let rig = rig(false);
    feed(&rig.handle.board, AAPL, Some(DataDelay::RealTime), 149.0, 151.0);
    let old =
        Instant::now().checked_sub(MAX_QUOTE_AGE + Duration::from_secs(1)).expect("an old instant");
    rig.handle.board.write(AAPL, |q| q.mid = q.mid.map(|(px, _)| (px, old)));
    assert_eq!(rig.handle.price(AAPL, false), Ok(None));
}

/// Dropping the handle closes its client.
#[test]
fn dropping_the_handle_closes_the_client() {
    let rig = rig(false);
    assert_eq!(rig.handle.price(AAPL, true), Ok(None));
    let closed = Arc::clone(&rig.closed);
    drop(rig);
    assert!(closed.load(Ordering::SeqCst));
}
