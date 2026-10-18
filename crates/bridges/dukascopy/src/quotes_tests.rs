//! `crate::quotes`' tests: the judgement of a tick, the spelling, the wants and their replay across sidecars,
//! the refusals in words, and the two constants held equal to the Java twin.

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

const NOW: i64 = 1_760_000_000_000;

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
    let sidecar = bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, true);
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

#[test]
fn the_one_spelling_is_six_letters() {
    assert_eq!(canonical("EURUSD").as_deref(), Ok("EURUSD"));
    assert_eq!(canonical(" xauusd ").as_deref(), Ok("XAUUSD"));
    for bad in ["EUR/USD", "EURUSDX", "EURUS", "", "COPPER.CMD/USD", "EUR USD", "EURUS1", "ÉURUSD"]
    {
        let why = canonical(bad).expect_err(bad);
        assert!(why.contains("six-letter"), "{bad}: {why}");
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
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, false);
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
    assert!(r.listening.want("EUR/USD").is_err(), "the spelling is the door's to canonicalise");
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
    r.bus.attach(Arc::clone(&next) as Arc<dyn LinePort>, true);
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
    r.bus.attach(Arc::clone(&dead) as Arc<dyn LinePort>, true);
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
}

#[test]
fn the_process_bus_is_one_bus() {
    assert!(Arc::ptr_eq(&process_bus(), &process_bus()));
}
