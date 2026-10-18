//! The PRICE LANE end to end against the scripted fake bridge (`src/bin/fake_jforex_bridge.rs`) - no Java, no
//! network. The exec client's reader attaches the sidecar to a `QuoteBus` on `ready`; a consumer's `want` writes a
//! `subscribe` line the fake answers with a `tick`; the reader hands the tick to the bus, which judges it and gives
//! it to the listener. What this proves is the Rust half over the real stdio protocol; the Java half is
//! `jforex-bridge/src/test/java/vike/jforex/TickLaneTest.java`, and a real tick cadence needs a live login.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_dukascopy::quotes::{Listening, PriceTick, QuoteBus};
use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient};
use vike_exec::{ExecutionClient, event_channel};

/// Path of the fake bridge binary (cargo builds crate bins for integration tests).
const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
    }
}

fn spawn(mode: &str, bus: &Arc<QuoteBus>) -> DukascopyExecutionClient {
    // Nothing here reads the ingest lane: a tick is not an `Event`, and the fake sends no other.
    let (events, _rx) = event_channel(64);
    let args: Vec<String> = if mode.is_empty() { vec![] } else { vec![mode.to_string()] };
    DukascopyExecutionClient::spawn_with_program_quoting(
        BRIDGE,
        &args,
        &config(),
        events,
        Arc::clone(bus),
    )
    .expect("the fake sidecar says ready")
}

/// A consumer collecting what the bus hands it.
fn consumer(bus: &Arc<QuoteBus>) -> (Listening, Arc<Mutex<Vec<PriceTick>>>) {
    let got: Arc<Mutex<Vec<PriceTick>>> = Arc::default();
    let sink = Arc::clone(&got);
    let listening = bus.listen(Arc::new(move |t: &PriceTick| sink.lock().unwrap().push(t.clone())));
    (listening, got)
}

/// Poll `ready` for up to ten seconds; the failure names what never happened.
fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn count(got: &Arc<Mutex<Vec<PriceTick>>>) -> usize {
    got.lock().unwrap().len()
}

#[test]
fn a_wanted_symbol_is_quoted_by_the_sidecar_and_handed_to_the_listener() {
    let bus = QuoteBus::new();
    let (listening, got) = consumer(&bus);
    let mut client = spawn("", &bus);

    listening.want("EURUSD").expect("a sidecar that announced ticks takes the subscription");
    wait_for("the first tick", || count(&got) == 1);
    let tick = got.lock().unwrap()[0].clone();
    assert_eq!(tick.symbol, "EURUSD");
    assert!((tick.bid - 1.1).abs() < 1e-9 && tick.ask > tick.bid, "{tick:?}");
    let age = vike_model::now_ms() - tick.ts;
    assert!((0..10_000).contains(&age), "the tick carries its own, current time: {age} ms old");

    client.detach();
}

#[test]
fn a_bad_tick_never_reaches_the_listener() {
    // The fake spoils EURUSD only; XAUUSD is asked second and answered second, so when its tick has arrived the
    // spoiled one has already been judged.
    for mode in ["ticks-stale", "ticks-one-sided", "ticks-ghost"] {
        let bus = QuoteBus::new();
        let (listening, got) = consumer(&bus);
        let mut client = spawn(mode, &bus);
        listening.want("EURUSD").expect("accepted");
        listening.want("XAUUSD").expect("accepted");
        wait_for("the good tick", || count(&got) >= 1);
        // a short settle: a tick wrongly let through would be right behind
        std::thread::sleep(Duration::from_millis(100));
        let symbols: Vec<String> = got.lock().unwrap().iter().map(|t| t.symbol.clone()).collect();
        assert_eq!(
            symbols,
            ["XAUUSD"],
            "{mode}: only the good tick of the asked symbol is a quote"
        );
        client.detach();
    }
}

#[test]
fn a_jar_that_announced_no_ticks_is_refused_in_words() {
    let bus = QuoteBus::new();
    let (listening, got) = consumer(&bus);
    let mut client = spawn("old-jar", &bus);
    let why = listening.want("EURUSD").expect_err("an old jar");
    assert!(why.contains("predates the price lane"), "{why}");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(count(&got), 0);
    client.detach();
}

/// A sidecar that goes away takes the lane with it (a `want` is refused in words), and the wants outlive it: the
/// next sidecar is told to quote them without anybody asking again.
#[test]
fn a_dead_sidecar_is_detached_and_its_wants_are_replayed_to_the_next_one() {
    let bus = QuoteBus::new();
    let (listening, got) = consumer(&bus);

    let mut first = spawn("", &bus);
    listening.want("EURUSD").expect("accepted");
    wait_for("the first sidecar's tick", || count(&got) == 1);
    // shutdown + reap: the reader hits EOF, detaches the lane, and is joined before this returns
    first.detach();
    let why = listening.want("XAUUSD").expect_err("the sidecar is gone");
    assert!(why.contains("no JForex sidecar is running"), "{why}");

    let mut second = spawn("", &bus);
    wait_for("the replayed subscription's tick", || count(&got) == 2);
    assert!(got.lock().unwrap().iter().all(|t| t.symbol == "EURUSD"));
    second.detach();
}

#[test]
fn closing_the_consumer_stops_the_listener() {
    let bus = QuoteBus::new();
    let (listening, got) = consumer(&bus);
    let mut client = spawn("", &bus);
    listening.want("EURUSD").expect("accepted");
    wait_for("the tick", || count(&got) == 1);
    drop(listening);
    // a new consumer starts with no wants: the previous one's EURUSD is not quoted to it
    let (next, next_got) = consumer(&bus);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(count(&next_got), 0);
    next.want("XAUUSD").expect("a fresh consumer asks for itself");
    wait_for("its own tick", || count(&next_got) == 1);
    assert_eq!(next_got.lock().unwrap()[0].symbol, "XAUUSD");
    client.detach();
}
