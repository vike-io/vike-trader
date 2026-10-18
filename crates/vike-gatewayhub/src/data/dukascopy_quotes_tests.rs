//! The dukascopy price stream over a scripted sidecar: nothing is dialled at build, a subscription is one command
//! to the sidecar that is already up, a quote is judged by the bus and labelled real-time once, and every refusal
//! is in words. The sidecar itself is a recording pipe on the bus (the real exec client's reader is exercised in
//! `crates/bridges/dukascopy/tests/dukascopy_quotes.rs`).

use std::sync::atomic::{AtomicBool, Ordering};

use vike_data::RecordingSink;
use vike_dukascopy::quotes::LinePort;

use super::*;

/// A sidecar's command pipe that records what was written.
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

fn sub(symbols: &[&str]) -> String {
    format!(
        "{{\"cmd\":\"subscribe\",\"symbols\":[{}]}}",
        symbols.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(",")
    )
}

fn unsub(symbols: &[&str]) -> String {
    format!(
        "{{\"cmd\":\"unsubscribe\",\"symbols\":[{}]}}",
        symbols.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(",")
    )
}

/// A bus with a sidecar attached (`ticks` = whether its jar announced the price lane).
fn bus_with_sidecar(ticks: bool) -> (Arc<QuoteBus>, Arc<Wire>) {
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, ticks);
    (bus, wire)
}

fn stream(bus: &Arc<QuoteBus>) -> (DukascopyQuotes, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    (DukascopyQuotes::on(bus, Arc::clone(&sink) as Arc<dyn LiveDataSink>), sink)
}

fn tick(bus: &QuoteBus, symbol: &str, bid: f64, ask: f64) {
    let now = vike_model::now_ms();
    bus.deliver(symbol, bid, ask, now, now);
}

#[test]
fn building_the_stream_dials_and_subscribes_nothing() {
    let (bus, wire) = bus_with_sidecar(true);
    let (_stream, sink) = stream(&bus);
    assert!(wire.lines().is_empty(), "lazy: nothing is written until a symbol is asked for");
    assert!(sink.recorded().is_empty());
}

#[test]
fn a_subscription_is_one_command_and_a_quote_is_labelled_real_time_once() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, sink) = stream(&bus);

    let id = stream.subscribe_quotes("eurusd").expect("the sidecar is up");
    assert_eq!(
        stream.subscribe_quotes("EURUSD").expect("idempotent"),
        id,
        "the same symbol, the same id"
    );
    assert_eq!(wire.lines(), [sub(&["EURUSD"])], "one command, in the canonical spelling");

    tick(&bus, "EURUSD", 1.1, 1.1002);
    tick(&bus, "EURUSD", 1.1001, 1.1003);
    let calls = sink.calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(
        calls[0], "data_delay:dukascopy:EURUSD:quotes:RealTime",
        "the label goes out first, once"
    );
    assert_eq!(calls[1], "quote:dukascopy:EURUSD:1.1/1.1002:0x0");
    assert_eq!(calls[2], "quote:dukascopy:EURUSD:1.1001/1.1003:0x0");
    let quotes = sink.quotes();
    assert!(quotes[0].2.local_ts > 0 && quotes[0].2.ts > 0, "both stamps are carried");
    assert_eq!(quotes[0].2.symbol, "EURUSD");
}

#[test]
fn only_a_real_time_two_sided_quote_of_a_subscribed_symbol_is_emitted() {
    let (bus, _wire) = bus_with_sidecar(true);
    let (mut stream, sink) = stream(&bus);
    stream.subscribe_quotes("EURUSD").expect("subscribed");
    let now = vike_model::now_ms();
    bus.deliver("EURUSD", 0.0, 1.1, now, now); // one-sided
    bus.deliver("EURUSD", 1.2, 1.1, now, now); // crossed
    bus.deliver("EURUSD", 1.1, 1.2, now - 60_000, now); // a minute old
    bus.deliver("EURUSD", 1.1, 1.2, 0, now); // no time of its own
    bus.deliver("GBPUSD", 1.3, 1.31, now, now); // not asked for
    assert!(sink.recorded().is_empty(), "nothing of the above is a quote: {:?}", sink.calls());
}

#[test]
fn every_refusal_is_in_words() {
    // no sidecar at all
    let (mut stream, _sink) = stream(&QuoteBus::new());
    let why = stream.subscribe_quotes("EURUSD").expect_err("no sidecar").to_string();
    assert!(why.contains("no JForex sidecar is running"), "{why}");

    // an old jar
    let (bus, wire) = bus_with_sidecar(false);
    let (mut stream, _sink) = self::stream(&bus);
    let why = stream.subscribe_quotes("EURUSD").expect_err("old jar").to_string();
    assert!(why.contains("predates the price lane"), "{why}");
    assert!(wire.lines().is_empty());

    // a spelling the sidecar does not round-trip
    let (bus, _wire) = bus_with_sidecar(true);
    let (mut stream, _sink) = self::stream(&bus);
    let why = stream.subscribe_quotes("COPPER.CMD/USD").expect_err("not a pair").to_string();
    assert!(why.contains("six-letter"), "{why}");

    // every other verb
    assert!(matches!(stream.subscribe_bars("EURUSD", "1m"), Err(LiveDataError::Unsupported(_))));
    assert!(matches!(stream.subscribe_trades("EURUSD"), Err(LiveDataError::Unsupported(_))));
    assert!(matches!(stream.subscribe_book("EURUSD"), Err(LiveDataError::Unsupported(_))));
}

#[test]
fn the_cap_is_the_buses() {
    let (bus, _wire) = bus_with_sidecar(true);
    let (mut stream, _sink) = stream(&bus);
    let pairs = ["EURUSD", "GBPUSD", "AUDUSD", "NZDUSD", "USDJPY", "USDCAD", "USDCHF", "EURGBP"];
    for p in pairs {
        stream.subscribe_quotes(p).expect(p);
    }
    let why = stream.subscribe_quotes("EURJPY").expect_err("past the cap").to_string();
    assert!(why.contains("at most 8"), "{why}");
}

#[test]
fn an_unsubscribe_stops_the_command_and_the_quotes_and_a_resubscribe_labels_again() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, sink) = stream(&bus);
    let id = stream.subscribe_quotes("EURUSD").expect("subscribed");
    tick(&bus, "EURUSD", 1.1, 1.2);
    stream.unsubscribe(id);
    stream.unsubscribe(id); // unknown now: a no-op
    assert_eq!(wire.lines(), [sub(&["EURUSD"]), unsub(&["EURUSD"])]);
    tick(&bus, "EURUSD", 1.1, 1.2);
    assert_eq!(sink.quotes().len(), 1, "a stopped symbol is not emitted");

    stream.subscribe_quotes("EURUSD").expect("again");
    tick(&bus, "EURUSD", 1.1, 1.2);
    let labels = sink.calls().iter().filter(|c| c.starts_with("data_delay")).count();
    assert_eq!(labels, 2, "a fresh subscription is labelled afresh");
}

#[test]
fn a_shutdown_stops_every_symbol_and_releases_the_bus() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, sink) = stream(&bus);
    stream.subscribe_quotes("EURUSD").expect("a");
    stream.subscribe_quotes("XAUUSD").expect("b");
    stream.begin_shutdown();
    stream.shutdown();
    assert_eq!(wire.lines().len(), 4, "two subscribes, then each symbol stopped");
    tick(&bus, "EURUSD", 1.1, 1.2);
    assert!(sink.quotes().is_empty());
    let why = stream.subscribe_quotes("EURUSD").expect_err("closed").to_string();
    assert!(why.contains("closed"), "{why}");
    drop(stream);
    // a fresh stream on the same bus starts clean
    let (mut next, _sink) = self::stream(&bus);
    next.subscribe_quotes("EURUSD").expect("starts clean");
}

#[test]
fn a_newer_stream_replaces_the_older() {
    let (bus, _wire) = bus_with_sidecar(true);
    let (mut old, _old_sink) = stream(&bus);
    old.subscribe_quotes("EURUSD").expect("old stream");
    let (mut newer, newer_sink) = self::stream(&bus);
    let why = old.subscribe_quotes("XAUUSD").expect_err("replaced").to_string();
    assert!(why.contains("replaced"), "{why}");
    newer.subscribe_quotes("XAUUSD").expect("the newer one works");
    tick(&bus, "XAUUSD", 2400.0, 2400.5);
    assert_eq!(newer_sink.quotes().len(), 1);
    drop(old); // the old one closing touches nothing of the new one
    tick(&bus, "XAUUSD", 2400.0, 2400.5);
    assert_eq!(newer_sink.quotes().len(), 2);
}
