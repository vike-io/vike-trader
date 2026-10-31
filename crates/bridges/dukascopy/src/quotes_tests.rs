//! `crate::quotes`' tests: the judgement of a tick, the spelling, the wants and their replay across sidecars,
//! the refusals in words, the consumer's isolation (a panicking or stalled listener never reaches the reader), the
//! sidecar's answers to a subscribe, the silence warning, and the two constants held equal to the Java twin.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

use vike_log::capture::captured;

use super::*;

const NOW: i64 = 1_760_000_000_000;

/// A jar of the release before the answers: it ticks and does not answer a `subscribe`.
fn ticks_only() -> Vec<String> {
    vec![FEATURE_TICKS.to_string()]
}

/// A jar that ticks and answers every symbol of a `subscribe`.
fn ticks_and_acks() -> Vec<String> {
    vec![FEATURE_TICKS.to_string(), FEATURE_TICK_ACKS.to_string()]
}

/// Poll `ready` for up to ten seconds; the failure names what never happened. The consumer is a thread of the bus's
/// own, so a test that delivers a tick waits for it to land.
fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A bus whose clocks and queue are the test's.
fn tuned(queue: usize, ack_ms: u64, silent_ms: u64, check_ms: u64) -> Arc<QuoteBus> {
    QuoteBus::tuned(Tuning {
        queue,
        ack_wait: Duration::from_millis(ack_ms),
        silent_after: Duration::from_millis(silent_ms),
        check_every: Duration::from_millis(check_ms),
        ..Tuning::PRODUCTION
    })
}

/// A sidecar's command pipe that records what was written and can be closed.
#[derive(Default)]
struct Wire {
    sent: Mutex<Vec<String>>,
    closed: AtomicBool,
}

impl Wire {
    fn lines(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

impl LinePort for Wire {
    fn send(&self, line: &str) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        self.sent.lock().unwrap().push(line.to_string());
        true
    }
}

fn subscribe(symbols: &[&str]) -> String {
    encode_line(&Command::Subscribe { symbols: symbols.iter().map(|s| (*s).to_string()).collect() })
}

fn unsubscribe(symbols: &[&str]) -> String {
    encode_line(&Command::Unsubscribe {
        symbols: symbols.iter().map(|s| (*s).to_string()).collect(),
    })
}

/// A bus with a sidecar attached (ticks announced) and a consumer collecting what it is handed.
struct Rig {
    bus: Arc<QuoteBus>,
    wire: Arc<Wire>,
    sidecar: u64,
    got: Arc<Mutex<Vec<PriceTick>>>,
    listening: Listening,
}

fn rig() -> Rig {
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    let sidecar = bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &ticks_only());
    let got: Arc<Mutex<Vec<PriceTick>>> = Arc::default();
    let sink = Arc::clone(&got);
    let listening = bus.listen(Arc::new(move |t: &PriceTick| sink.lock().unwrap().push(t.clone())));
    Rig { bus, wire, sidecar, got, listening }
}

// ---- the judgement of a tick ---------------------------------------------------------------------

#[test]
fn a_current_two_sided_tick_is_a_quote_and_every_other_kind_is_named() {
    let ok = accept("EURUSD", 1.1, 1.1002, NOW - 40, NOW).expect("a quote");
    assert_eq!(ok, PriceTick { symbol: "EURUSD".into(), bid: 1.1, ask: 1.1002, ts: NOW - 40 });
    // a locked market (ask == bid) is still a quote
    assert!(accept("EURUSD", 1.1, 1.1, NOW, NOW).is_ok());

    for (bid, ask) in [
        (0.0, 1.1),
        (1.1, 0.0),
        (-1.1, 1.1),
        (f64::NAN, 1.1),
        (1.1, f64::NAN),
        (1.1, f64::INFINITY),
        (f64::NEG_INFINITY, 1.1),
    ] {
        assert_eq!(accept("EURUSD", bid, ask, NOW, NOW), Err(TickRefusal::OneSided), "{bid} {ask}");
    }
    assert_eq!(accept("EURUSD", 1.2, 1.1, NOW, NOW), Err(TickRefusal::Crossed));
    assert_eq!(accept("EURUSD", 1.1, 1.2, 0, NOW), Err(TickRefusal::NoTime));
    assert_eq!(accept("EURUSD", 1.1, 1.2, -5, NOW), Err(TickRefusal::NoTime));
}

/// The age bound is symmetric and inclusive: ten seconds either way is a quote, one millisecond more is not.
#[test]
fn the_age_bound_is_ten_seconds_either_way() {
    assert_eq!(MAX_TICK_AGE_MS, 10_000);
    assert!(accept("EURUSD", 1.1, 1.2, NOW - MAX_TICK_AGE_MS, NOW).is_ok());
    assert_eq!(accept("EURUSD", 1.1, 1.2, NOW - MAX_TICK_AGE_MS - 1, NOW), Err(TickRefusal::Stale));
    assert!(accept("EURUSD", 1.1, 1.2, NOW + MAX_TICK_AGE_MS, NOW).is_ok());
    assert_eq!(
        accept("EURUSD", 1.1, 1.2, NOW + MAX_TICK_AGE_MS + 1, NOW),
        Err(TickRefusal::FromTheFuture)
    );
    // extreme clocks do not overflow
    assert_eq!(accept("EURUSD", 1.1, 1.2, 1, i64::MAX), Err(TickRefusal::Stale));
    assert_eq!(accept("EURUSD", 1.1, 1.2, i64::MAX, i64::MIN), Err(TickRefusal::FromTheFuture));
    // every refusal has words
    for why in [
        TickRefusal::OneSided,
        TickRefusal::Crossed,
        TickRefusal::NoTime,
        TickRefusal::Stale,
        TickRefusal::FromTheFuture,
    ] {
        assert!(why.to_string().len() > 20, "{why:?}");
    }
}

/// **A stamp in another unit than milliseconds is refused, whichever way the unit is wrong** (`ITick.getTime()` has
/// no documented unit): seconds are behind this box's clock by decades, microseconds and nanoseconds ahead of it by
/// a factor of a thousand and a million, so none is a quote, none reaches the consumer and none is kept as a rate.
#[test]
fn a_stamp_in_another_unit_than_milliseconds_is_never_a_quote_or_a_rate() {
    let seconds = NOW / 1_000;
    let micros = NOW * 1_000;
    let nanos = NOW * 1_000_000;
    assert_eq!(accept("EURUSD", 1.1, 1.2, seconds, NOW), Err(TickRefusal::Stale));
    assert_eq!(accept("EURUSD", 1.1, 1.2, micros, NOW), Err(TickRefusal::FromTheFuture));
    assert_eq!(accept("EURUSD", 1.1, 1.2, nanos, NOW), Err(TickRefusal::FromTheFuture));
    // the words name the unit, so the operator reads the cause and not "a skewed clock"
    assert!(TickRefusal::Stale.to_string().contains("seconds instead of milliseconds"));
    assert!(TickRefusal::FromTheFuture.to_string().contains("microseconds or nanoseconds"));

    // through the bus: all three are counted as refused and none is forwarded or kept for the rate reader
    let r = rig();
    r.listening.want("EURUSD").expect("wanted");
    for stamp in [seconds, micros, nanos] {
        r.bus.deliver("EURUSD", 1.1, 1.1002, stamp, NOW);
    }
    assert_eq!(r.bus.state().dropped, 3, "the three mis-scaled ticks are counted");
    assert!(r.got.lock().unwrap().is_empty(), "none reached the consumer");
    assert!(!r.bus.state().last.contains_key("EURUSD"), "none is kept as the freshest tick");
    // a stamp in milliseconds right after them is a quote as ever
    r.bus.deliver("EURUSD", 1.1, 1.1002, NOW - 5, NOW);
    wait_for("the one quote", || !r.got.lock().unwrap().is_empty());
    assert_eq!(r.got.lock().unwrap()[0].ts, NOW - 5);
}

/// **A pair that could not be SUBSCRIBED is told from one that was subscribed and sent no tick**
/// ([`is_not_subscribed`], what the gatewayhub says once an hour): no free place (the consumer holds all eight), a
/// sidecar whose pipe closed, and a quiet market are three answers of `fresh_tick`, and only the first two are
/// "not subscribed".
#[test]
fn a_pair_that_could_not_be_subscribed_is_told_from_one_that_sent_no_tick() {
    let r = rig();
    for symbol in ["EURUSD", "GBPUSD", "AUDUSD", "NZDUSD", "USDCHF", "EURGBP", "EURJPY", "GBPJPY"] {
        r.listening.want(symbol).expect("a place");
    }
    let why = r.bus.fresh_tick("USDCAD", Duration::from_millis(5)).expect_err("no place");
    assert!(why.contains("at most 8 instruments") && is_not_subscribed(&why), "{why}");

    let quiet = rig();
    let why = quiet.bus.fresh_tick("USDCAD", Duration::from_millis(30)).expect_err("no tick");
    assert!(why.contains("no tick of `USDCAD` arrived") && !is_not_subscribed(&why), "{why}");

    let gone = rig();
    gone.wire.closed.store(true, Ordering::SeqCst);
    let why = gone.bus.fresh_tick("USDCAD", Duration::from_millis(5)).expect_err("a closed pipe");
    assert!(why.contains("command pipe is closed") && is_not_subscribed(&why), "{why}");
}

#[test]
fn the_one_spelling_is_six_letters_or_the_sdks_slashed_name() {
    assert_eq!(canonical("EURUSD").as_deref(), Ok("EURUSD"));
    assert_eq!(canonical(" xauusd ").as_deref(), Ok("XAUUSD"));
    // a slashed pair is the six letters (one pair is one symbol); a CFD keeps the SDK's slashed name, upper-cased
    assert_eq!(canonical("EUR/USD").as_deref(), Ok("EURUSD"));
    assert_eq!(canonical("xau/usd").as_deref(), Ok("XAUUSD"));
    for cfd in [
        "COPPER.CMD/USD",
        "AAPL.US/USD",
        "DEU.IDX/EUR",
        "A.US/USD",
        "USATECH.IDX/USD",
        "LSE.GB/GBX",
    ] {
        assert_eq!(canonical(cfd).as_deref(), Ok(cfd), "{cfd}");
        assert_eq!(canonical(&cfd.to_ascii_lowercase()).as_deref(), Ok(cfd), "{cfd}");
    }
    // never the enum-style name of a CFD (the sidecar cannot resolve it), and nothing that is not a name
    for bad in [
        "EURUSDX",
        "EURUS",
        "",
        "EUR USD",
        "EURUS1",
        "ÉURUSD",
        "COPPERCMDUSD",
        "AAPLUSUSD",
        "COPPER.CMD/US",
        "COPPER.CMD/USDX",
        "COPPER.CMD/12D",
        ".CMD/USD",
        "1ABC/USD",
        "COPPER CMD/USD",
        "COPPER.CMD/USD/EUR",
        "TOO.LONG.A.CODE.HERE/USD",
        "/USD",
    ] {
        let why = canonical(bad).expect_err(bad);
        assert!(why.contains("six-letter") && why.contains("COPPER.CMD/USD"), "{bad}: {why}");
    }
}

// ---- wants: the refusals in words, the cap, the wire ----------------------------------------------

#[test]
fn a_want_with_no_sidecar_is_refused_in_words_and_subscribes_nothing() {
    let bus = QuoteBus::new();
    let listening = bus.listen(Arc::new(|_: &PriceTick| {}));
    let why = listening.want("EURUSD").expect_err("nothing is attached");
    assert!(why.contains("no JForex sidecar is running"), "{why}");
    assert!(why.contains("never starts one"), "the lane must say it starts no login: {why}");
    assert!(bus.state().wanted.is_empty());
}

#[test]
fn a_jar_that_announced_no_ticks_is_refused_and_written_nothing() {
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &[]);
    let listening = bus.listen(Arc::new(|_: &PriceTick| {}));
    let why = listening.want("EURUSD").expect_err("an old jar");
    assert!(why.contains("predates the price lane"), "{why}");
    assert!(why.contains(FEATURE_TICKS), "{why}");
    assert!(wire.lines().is_empty(), "an old jar is never written a command it would not answer");
}

#[test]
fn a_want_writes_one_subscribe_and_is_idempotent_and_an_unwant_writes_one_unsubscribe() {
    let r = rig();
    r.listening.want("eurusd").expect("subscribed");
    r.listening.want("EURUSD").expect("idempotent");
    r.listening.want("XAUUSD").expect("a second pair");
    assert_eq!(r.wire.lines(), [subscribe(&["EURUSD"]), subscribe(&["XAUUSD"])]);
    r.listening.unwant("EURUSD");
    r.listening.unwant("EURUSD"); // already stopped: no second line
    r.listening.unwant("GBPUSD"); // never asked
    assert_eq!(r.wire.lines().last(), Some(&unsubscribe(&["EURUSD"])));
    assert_eq!(r.wire.lines().len(), 3);
    // the enum-style name of a CFD is no spelling the sidecar resolves: refused here, nothing written
    assert!(
        r.listening.want("COPPERCMDUSD").is_err(),
        "the sidecar cannot resolve the enum-style name"
    );
    assert_eq!(r.wire.lines().len(), 3);
    // a slashed pair is the six letters (one pair, one symbol), and a CFD is subscribed by the SDK's slashed name
    r.listening.want("EUR/USD").expect("read as EURUSD");
    r.listening.want("copper.cmd/usd").expect("a CFD");
    assert_eq!(
        r.wire.lines()[3..],
        [subscribe(&["EURUSD"]), subscribe(&["COPPER.CMD/USD"])],
        "the wire carries the canonical spelling"
    );
}

#[test]
fn the_cap_bounds_the_symbols_priced_at_once_and_a_freed_place_is_usable() {
    let r = rig();
    let pairs = ["EURUSD", "GBPUSD", "AUDUSD", "NZDUSD", "USDJPY", "USDCAD", "USDCHF", "EURGBP"];
    assert_eq!(pairs.len(), MAX_PRICED_SYMBOLS);
    for p in pairs {
        r.listening.want(p).expect(p);
    }
    let why = r.listening.want("EURJPY").expect_err("past the cap");
    assert!(why.contains("at most 8"), "{why}");
    assert_eq!(r.wire.lines().len(), 8, "the refused ninth was not written");
    r.listening.unwant("EURUSD");
    r.listening.want("EURJPY").expect("the freed place");
}

// ---- the wire dies, the sidecar changes -----------------------------------------------------------

#[test]
fn a_closed_pipe_refuses_the_want_detaches_the_sidecar_and_keeps_no_phantom_want() {
    let r = rig();
    r.wire.closed.store(true, Ordering::SeqCst);
    let why = r.listening.want("EURUSD").expect_err("the pipe is closed");
    assert!(why.contains("sidecar is gone"), "{why}");
    assert!(r.bus.state().wanted.is_empty(), "a symbol nothing serves is not held");
    let again = r.listening.want("EURUSD").expect_err("detached");
    assert!(again.contains("no JForex sidecar is running"), "{again}");
}

/// The wants outlive a sidecar: the next one that attaches is told to quote them, in one line.
#[test]
fn the_wants_are_replayed_to_the_next_sidecar() {
    let r = rig();
    r.listening.want("EURUSD").expect("a");
    r.listening.want("XAUUSD").expect("b");
    r.bus.detach(r.sidecar);
    assert!(r.listening.want("GBPUSD").is_err(), "between sidecars a want is refused");
    assert_eq!(r.bus.state().wanted.len(), 2, "...and the held wants stand");

    let next = Arc::new(Wire::default());
    r.bus.attach(Arc::clone(&next) as Arc<dyn LinePort>, &ticks_only());
    assert_eq!(next.lines(), [subscribe(&["EURUSD", "XAUUSD"])]);
    // a stale detach (the old reader's EOF arriving late) does not take the new sidecar away
    r.bus.detach(r.sidecar);
    r.listening.want("GBPUSD").expect("the new sidecar is still attached");
}

#[test]
fn a_replay_to_a_dead_sidecar_detaches_it() {
    let r = rig();
    r.listening.want("EURUSD").expect("a");
    r.bus.detach(r.sidecar);
    let dead = Arc::new(Wire::default());
    dead.closed.store(true, Ordering::SeqCst);
    r.bus.attach(Arc::clone(&dead) as Arc<dyn LinePort>, &ticks_only());
    assert!(r.listening.want("GBPUSD").is_err());
}

// ---- ticks ---------------------------------------------------------------------------------------

#[test]
fn only_an_accepted_tick_of_a_wanted_symbol_reaches_the_consumer() {
    let r = rig();
    r.listening.want("EURUSD").expect("wanted");
    r.bus.deliver("EURUSD", 1.1, 1.1002, NOW - 5, NOW);
    r.bus.deliver("GBPUSD", 1.3, 1.3002, NOW - 5, NOW); // not wanted
    r.bus.deliver("EURUSD", 0.0, 1.1002, NOW - 5, NOW); // one-sided
    r.bus.deliver("EURUSD", 1.1, 1.1002, NOW - 60_000, NOW); // a cached tick
    r.bus.deliver("EURUSD", 1.1, 1.1002, 0, NOW); // no time
    wait_for("the one quote", || !r.got.lock().unwrap().is_empty());
    let got = r.got.lock().unwrap().clone();
    assert_eq!(got, [PriceTick { symbol: "EURUSD".into(), bid: 1.1, ask: 1.1002, ts: NOW - 5 }]);
    assert_eq!(r.bus.state().dropped, 3, "the three refused ticks of a wanted symbol are counted");

    r.listening.unwant("EURUSD");
    r.bus.deliver("EURUSD", 1.1, 1.1002, NOW - 5, NOW); // the unsubscribe still in flight
    assert_eq!(r.got.lock().unwrap().len(), 1, "a symbol no longer wanted is not forwarded");
    assert_eq!(r.bus.state().dropped, 3, "...and is not counted as a fault");
}

#[test]
fn a_newer_consumer_replaces_the_older_without_the_older_touching_it() {
    let r = rig();
    r.listening.want("EURUSD").expect("old consumer's want");
    let newer = r.bus.listen(Arc::new(|_: &PriceTick| {}));
    assert_eq!(
        r.wire.lines().last(),
        Some(&unsubscribe(&["EURUSD"])),
        "the replaced consumer's wants are stopped at the sidecar"
    );
    assert!(r.bus.state().wanted.is_empty());
    let why = r.listening.want("XAUUSD").expect_err("replaced");
    assert!(why.contains("replaced"), "{why}");
    newer.want("XAUUSD").expect("the new consumer works");

    let old = r.listening;
    drop(old); // the old one going away must not clear the new one
    assert!(r.bus.state().listener.is_some());
    assert!(r.bus.state().wanted.contains("XAUUSD"));
    r.bus.deliver("XAUUSD", 2400.0, 2400.5, NOW, NOW);
    drop(newer);
    assert!(r.bus.state().listener.is_none());
    assert_eq!(r.wire.lines().last(), Some(&unsubscribe(&["XAUUSD"])), "closing stops the ticks");
}

#[test]
fn dropping_the_consumer_stops_every_want_and_the_next_consumer_starts_clean() {
    let r = rig();
    r.listening.want("EURUSD").expect("a");
    r.listening.want("GBPUSD").expect("b");
    let Rig { bus, wire, listening, .. } = r;
    drop(listening);
    assert_eq!(wire.lines().last(), Some(&unsubscribe(&["EURUSD", "GBPUSD"])));
    assert!(bus.state().wanted.is_empty());
    let next = bus.listen(Arc::new(|_: &PriceTick| {}));
    next.want("EURUSD").expect("starts clean");
}

// ---- a takeover is loud, and the displaced stream is never answered as if it still priced ---------------

#[test]
fn a_takeover_is_warned_naming_both_streams_and_the_displaced_one_is_refused() {
    let r = rig();
    r.listening.want("EURUSD").expect("held by the older stream");
    r.listening.ensure_current().expect("the only stream so far");

    let (newer, events) = captured(|| r.bus.listen(Arc::new(|_: &PriceTick| {})));
    let warn = events
        .iter()
        .find(|e| e.level == tracing::Level::WARN && e.target == TARGET)
        .unwrap_or_else(|| panic!("a takeover is said at warn: {events:?}"));
    // tokens are numbered from the sidecar's attach (1): the older stream is #2, the newer #3
    assert!(
        warn.message.contains("stream #3") && warn.message.contains("stream #2"),
        "{}",
        warn.message
    );
    assert_eq!(warn.field("held"), Some("[\"EURUSD\"]"), "what the older stream held is named");

    let why = r.listening.ensure_current().expect_err("displaced");
    assert!(why.contains("replaced"), "{why}");
    newer.ensure_current().expect("the newer one is the consumer");

    // the first consumer of a bus displaces nobody and says nothing
    let (_first, events) = captured(|| QuoteBus::new().listen(Arc::new(|_: &PriceTick| {})));
    assert!(events.iter().all(|e| e.level != tracing::Level::WARN), "{events:?}");
}

// ---- a consumer fault never reaches the reader -----------------------------------------------------

/// A listener that panics is caught on the bus's thread, logged and detached: the reader's `deliver` neither panics nor
/// blocks, the consumer's wants are stopped at the sidecar, and the stream is told on its next ask.
#[test]
fn a_panicking_consumer_is_detached_and_never_reaches_the_reader() {
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &ticks_only());
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let listening = bus.listen(Arc::new(move |_: &PriceTick| {
        counted.fetch_add(1, Ordering::SeqCst);
        panic!("a consumer bug");
    }));
    listening.want("EURUSD").expect("wanted");

    bus.deliver("EURUSD", 1.1, 1.1002, NOW, NOW); // the reader's call: returns whatever the consumer does
    wait_for("the faulty consumer's wants to be stopped", || {
        wire.lines().last() == Some(&unsubscribe(&["EURUSD"]))
    });
    assert!(bus.state().listener.is_none(), "the faulty consumer is detached");
    assert!(bus.state().wanted.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // the reader goes on: a later tick has no taker and is dropped quietly
    bus.deliver("EURUSD", 1.1, 1.1002, NOW, NOW);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let why = listening.want("GBPUSD").expect_err("detached");
    assert!(why.contains("replaced") && why.contains("fault"), "{why}");
    listening.ensure_current().expect_err("the stream is told on every ask");
    // a fresh consumer prices again
    let next = bus.listen(Arc::new(|_: &PriceTick| {}));
    next.want("EURUSD").expect("a new stream starts clean");
}

/// A listener that stalls fills its queue; the reader's `deliver` never waits, the surplus ticks are dropped and
/// counted, and when the consumer wakes it drains what was queued.
#[test]
fn a_stalled_consumer_never_blocks_the_reader_and_the_surplus_is_dropped_and_counted() {
    const QUEUE: usize = 4;
    const SENT: usize = 20;
    let bus = tuned(QUEUE, 5_000, 120_000, 60_000);
    let wire = Arc::new(Wire::default());
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &ticks_only());
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let handled = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&handled);
    let listening = bus.listen(Arc::new(move |_: &PriceTick| {
        // held until the test lets go (a closed gate answers at once, so the drain after it is quick)
        let _ = gate.lock().unwrap().recv();
        counted.fetch_add(1, Ordering::SeqCst);
    }));
    listening.want("EURUSD").expect("wanted");

    let reader_bus = Arc::clone(&bus);
    let (done, finished) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        for _ in 0..SENT {
            reader_bus.deliver("EURUSD", 1.1, 1.1002, NOW, NOW);
        }
        let _ = done.send(());
    });
    finished
        .recv_timeout(Duration::from_secs(10))
        .expect("the reader was held up by a stalled consumer");
    // one tick in the consumer's hands, QUEUE waiting: the rest were turned away and counted
    assert!(bus.state().overflowed >= (SENT - QUEUE - 1) as u64, "{}", bus.state().overflowed);
    assert!(
        bus.state().listener.is_some(),
        "a slow consumer is not a faulty one: it stays attached"
    );

    drop(release);
    wait_for("the consumer to drain its queue", || handled.load(Ordering::SeqCst) >= QUEUE);
    assert!(handled.load(Ordering::SeqCst) <= QUEUE + 1);
}

// ---- the sidecar's answers to a subscribe ----------------------------------------------------------

/// A bus with an ANSWERING sidecar attached (wire, attach id) and a consumer; `ack_ms` is the bus's patience.
fn answering(ack_ms: u64) -> (Arc<QuoteBus>, Arc<Wire>, u64, Arc<Listening>) {
    let bus = tuned(8, ack_ms, 120_000, 60_000);
    let wire = Arc::new(Wire::default());
    let sidecar = bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &ticks_and_acks());
    let listening = Arc::new(bus.listen(Arc::new(|_: &PriceTick| {})));
    (bus, wire, sidecar, listening)
}

/// `want(symbol)` on its own thread (it waits for the sidecar's answer); the result arrives on the channel.
fn ask(listening: &Arc<Listening>, symbol: &'static str) -> mpsc::Receiver<Result<(), String>> {
    let (tx, rx) = mpsc::channel();
    let listening = Arc::clone(listening);
    std::thread::spawn(move || {
        let _ = tx.send(listening.want(symbol));
    });
    rx
}

fn answer_of(asked: &mpsc::Receiver<Result<(), String>>) -> Result<(), String> {
    asked.recv_timeout(Duration::from_secs(10)).expect("the want never returned")
}

#[test]
fn a_decline_is_the_refusal_and_frees_the_place() {
    let (bus, wire, _, listening) = answering(10_000);
    let asked = ask(&listening, "GBPUSD");
    wait_for("the subscribe", || wire.lines() == [subscribe(&["GBPUSD"])]);
    let (_, events) = captured(|| bus.subscription("GBPUSD", false, "unknown instrument"));
    let why = answer_of(&asked).expect_err("declined");
    assert!(
        why.contains("declined") && why.contains("unknown instrument") && why.contains("GBPUSD"),
        "{why}"
    );
    let st = bus.state();
    assert!(st.wanted.is_empty(), "the place the want took is free");
    assert!(st.awaiting.is_empty() && st.declined.is_empty() && st.quiet.is_empty());
    drop(st);
    assert!(
        events
            .iter()
            .any(|e| e.level == tracing::Level::WARN && e.message.contains("unknown instrument")),
        "a decline is said at warn with the jar's reason: {events:?}"
    );
}

#[test]
fn an_ok_answer_ends_the_wait_and_keeps_the_want() {
    let (bus, wire, _, listening) = answering(10_000);
    let asked = ask(&listening, "EURUSD");
    wait_for("the subscribe", || wire.lines() == [subscribe(&["EURUSD"])]);
    bus.subscription("EURUSD", true, "");
    answer_of(&asked).expect("acknowledged");
    assert!(bus.state().wanted.contains("EURUSD"));
    assert!(bus.state().awaiting.is_empty());
}

/// No answer in time is what a jar that never answers always was (the want stands), plus one warning; a decline that
/// arrives after the wait gave up still frees the place, and a late `ok` changes nothing.
#[test]
fn no_answer_in_time_keeps_the_want_with_one_warning_and_a_late_decline_frees_it() {
    let (bus, _, _, listening) = answering(50);
    let (out, events) = captured(|| listening.want("EURUSD"));
    out.expect("no answer in time is not a refusal");
    assert!(bus.state().wanted.contains("EURUSD"));
    let warns: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::WARN).collect();
    assert_eq!(warns.len(), 1, "{events:?}");
    assert!(warns[0].message.contains("did not answer"), "{}", warns[0].message);

    bus.subscription("EURUSD", true, ""); // late ok: nothing
    assert!(bus.state().wanted.contains("EURUSD"));
    bus.subscription("EURUSD", false, "too late"); // late decline: the place is freed
    assert!(bus.state().wanted.is_empty());
    assert!(
        bus.state().declined.is_empty(),
        "nobody is waiting, so nothing is parked for a later want"
    );
}

/// A jar that announced no `tick-acks` is never waited for: its `subscribe` is fire-and-forget, as before.
#[test]
fn a_jar_without_tick_acks_is_not_waited_for() {
    let r = rig(); // production patience (five seconds) and a jar that only ticks
    let started = Instant::now();
    r.listening.want("EURUSD").expect("subscribed");
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert!(r.bus.state().awaiting.is_empty());
}

#[test]
fn a_sidecar_that_goes_away_mid_wait_ends_the_wait_and_the_want_stands_for_the_next_one() {
    let (bus, wire, sidecar, listening) = answering(10_000);
    let asked = ask(&listening, "EURUSD");
    wait_for("the subscribe", || wire.lines() == [subscribe(&["EURUSD"])]);
    bus.detach(sidecar);
    answer_of(&asked).expect("a gone sidecar is not a decline");
    assert!(bus.state().wanted.contains("EURUSD"), "the next sidecar is told to quote it");
}

#[test]
fn a_stream_replaced_while_waiting_is_told_so() {
    let (bus, wire, _, listening) = answering(10_000);
    let asked = ask(&listening, "EURUSD");
    wait_for("the subscribe", || wire.lines() == [subscribe(&["EURUSD"])]);
    let _newer = bus.listen(Arc::new(|_: &PriceTick| {}));
    let why = answer_of(&asked).expect_err("replaced");
    assert!(why.contains("replaced"), "{why}");
}

/// A replay to a new sidecar is not waited for, but a decline of one of its symbols frees that want too.
#[test]
fn a_decline_of_a_replayed_want_frees_it() {
    let (bus, wire, sidecar, listening) = answering(10_000);
    let asked = ask(&listening, "EURUSD");
    wait_for("the subscribe", || wire.lines() == [subscribe(&["EURUSD"])]);
    bus.subscription("EURUSD", true, "");
    answer_of(&asked).expect("acknowledged");
    bus.detach(sidecar);

    let next = Arc::new(Wire::default());
    bus.attach(Arc::clone(&next) as Arc<dyn LinePort>, &ticks_and_acks());
    assert_eq!(next.lines(), [subscribe(&["EURUSD"])], "replayed");
    assert!(bus.state().awaiting.is_empty(), "attach does not wait for the replay's answer");
    bus.subscription("EURUSD", false, "gone from the SDK");
    assert!(bus.state().wanted.is_empty());
    drop(listening);
}

// ---- silence ---------------------------------------------------------------------------------------

#[test]
fn a_silent_symbol_is_warned_once_and_a_tick_re_arms_it() {
    let bus = tuned(8, 5_000, 300, 60_000);
    let wire = Arc::new(Wire::default());
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &ticks_only());
    let listening = bus.listen(Arc::new(|_: &PriceTick| {}));
    listening.want("EURUSD").expect("a");
    listening.want("GBPUSD").expect("b");
    assert!(bus.warn_silent().is_empty(), "just asked for: not silent yet");

    std::thread::sleep(Duration::from_millis(350));
    let (said, events) = captured(|| bus.warn_silent());
    assert_eq!(said, ["EURUSD", "GBPUSD"]);
    let warns = events.iter().filter(|e| e.level == tracing::Level::WARN).count();
    assert_eq!(warns, 2, "one line per silent symbol: {events:?}");
    assert!(bus.warn_silent().is_empty(), "said once, not every check");

    bus.deliver("EURUSD", 1.1, 1.1002, NOW, NOW); // EURUSD is alive again
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(
        bus.warn_silent(),
        ["EURUSD"],
        "re-armed by its tick; GBPUSD's silence was already said"
    );
}

#[test]
fn silence_is_only_said_while_a_ticking_sidecar_is_attached() {
    let bus = tuned(8, 5_000, 300, 60_000);
    let wire = Arc::new(Wire::default());
    let sidecar = bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &ticks_only());
    let listening = bus.listen(Arc::new(|_: &PriceTick| {}));
    listening.want("EURUSD").expect("wanted");
    bus.detach(sidecar);
    std::thread::sleep(Duration::from_millis(350));
    assert!(bus.warn_silent().is_empty(), "no sidecar, no silence to blame it for");
    // a new sidecar starts the clock over
    bus.attach(Arc::new(Wire::default()) as Arc<dyn LinePort>, &ticks_only());
    assert!(bus.warn_silent().is_empty());
}

/// The check is the consumer thread's own: with no tick arriving at all, it still looks.
#[test]
fn the_consumers_thread_looks_at_the_silence_by_itself() {
    let bus = tuned(8, 5_000, 30, 10);
    let wire = Arc::new(Wire::default());
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &ticks_only());
    let listening = bus.listen(Arc::new(|_: &PriceTick| {}));
    listening.want("EURUSD").expect("wanted");
    wait_for("the silence to be said", || {
        bus.state().quiet.get("EURUSD").is_some_and(|q| q.warned)
    });
}

// ---- held equal to the Java twin ------------------------------------------------------------------

/// The sidecar's cap and its feature word are the Java's own. The Java is read at run time (the public mirror
/// ships no `jforex-bridge/` sources a test may `include_str!`), and a checkout without it says so.
#[test]
fn the_caps_and_the_feature_word_agree_with_the_java() {
    let java = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("jforex-bridge/src/main/java/vike/jforex");
    let (Ok(lane), Ok(proto)) = (
        std::fs::read_to_string(java.join("TickLane.java")),
        std::fs::read_to_string(java.join("Proto.java")),
    ) else {
        eprintln!("SKIPPED: {} is not in this checkout", java.display());
        return;
    };
    let cap = lane
        .lines()
        .find(|l| l.contains("int MAX_INSTRUMENTS"))
        .expect("TickLane.java declares MAX_INSTRUMENTS");
    let value = cap.split('=').nth(1).and_then(|v| v.split(';').next()).map(str::trim);
    assert_eq!(value.and_then(|v| v.parse::<usize>().ok()), Some(MAX_PRICED_SYMBOLS), "{cap}");
    let features = proto
        .lines()
        .find(|l| l.contains("String[] FEATURES"))
        .expect("Proto.java declares FEATURES");
    assert!(features.contains(&format!("\"{FEATURE_TICKS}\"")), "{features}");
    assert!(features.contains(&format!("\"{FEATURE_TERMS}\"")), "{features}");
}

#[test]
fn the_process_bus_is_one_bus() {
    assert!(Arc::ptr_eq(&process_bus(), &process_bus()));
}
