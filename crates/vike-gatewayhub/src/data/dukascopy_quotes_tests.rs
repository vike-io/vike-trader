//! The dukascopy price stream over a scripted sidecar: nothing is dialled at build, a subscription is one command
//! to the sidecar that is already up, a quote is judged by the bus and labelled real-time once, and every refusal
//! is in words. The sidecar itself is a recording pipe on the bus (the real exec client's reader is exercised in
//! `crates/bridges/dukascopy/tests/sidecar/dukascopy_quotes.rs`).

use std::time::Duration;

use vike_data::RecordingSink;
use vike_dukascopy::quotes::{FEATURE_TICKS, LinePort, MAX_PRICED_SYMBOLS};
use vike_dukascopy::test_support::Wire;

use std::sync::Arc;

use vike_data::{DataClient, LiveDataError, LiveDataSink};

use vike_bridge_core::price_stream::PriceStream;
use vike_dukascopy::quotes::QuoteBus;
use vike_gatewayhub_client::credentialed_limits::stream_limits_for;
use vike_log::poll::wait_for;

use crate::data::session_guard::SessionGuard;

/// How long a wait in this file allows before `vike_log::poll::wait_for` fails it.
const WAIT_FOR: Duration = Duration::from_secs(10);

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
    let features: Vec<String> = if ticks { vec![FEATURE_TICKS.to_string()] } else { Vec::new() };
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &features);
    (bus, wire)
}

fn stream(bus: &Arc<QuoteBus>) -> (PriceStream<QuoteBus>, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    (PriceStream::on(bus, Arc::clone(&sink) as Arc<dyn LiveDataSink>), sink)
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
    wait_for(WAIT_FOR, "both quotes", || sink.quotes().len() == 2);
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
    let why = stream.subscribe_quotes("COPPERCMDUSD").expect_err("enum-style name").to_string();
    assert!(why.contains("six-letter") && why.contains("COPPER.CMD/USD"), "{why}");

    // every other verb, and a closed stream, in this venue's own words
    let only = |lane: &str| format!("dukascopy's price stream carries quotes only (no {lane})");
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
        matches!(&closed, LiveDataError::Subscribe(w) if w == "dukascopy: the price stream is closed"),
        "{closed:?}"
    );
}

/// **A CFD streams under the SDK's slashed name**, the spelling the sidecar resolves and stamps its ticks with, so the
/// quote the stream emits is keyed by the very symbol the node asked a price of.
#[test]
fn a_cfd_is_subscribed_and_quoted_under_its_slashed_name() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, sink) = stream(&bus);
    stream.subscribe_quotes("copper.cmd/usd").expect("a CFD");
    assert_eq!(wire.lines(), [sub(&["COPPER.CMD/USD"])]);
    tick(&bus, "COPPER.CMD/USD", 3.8, 3.81);
    wait_for(WAIT_FOR, "the CFD's quote", || sink.quotes().len() == 1);
    let (_venue, symbol, quote) = sink.quotes().remove(0);
    assert_eq!((symbol.as_str(), quote.symbol.as_str()), ("COPPER.CMD/USD", "COPPER.CMD/USD"));
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
    wait_for(WAIT_FOR, "the first quote", || sink.quotes().len() == 1);
    stream.unsubscribe(id);
    stream.unsubscribe(id); // unknown now: a no-op
    assert_eq!(wire.lines(), [sub(&["EURUSD"]), unsub(&["EURUSD"])]);
    tick(&bus, "EURUSD", 1.1, 1.2);
    assert_eq!(sink.quotes().len(), 1, "a stopped symbol is not emitted");

    stream.subscribe_quotes("EURUSD").expect("again");
    tick(&bus, "EURUSD", 1.1, 1.2);
    wait_for(WAIT_FOR, "the re-subscribed quote", || sink.quotes().len() == 2);
    let labels = sink.calls().iter().filter(|c| c.starts_with("data_delay")).count();
    assert_eq!(labels, 2, "a fresh subscription is labelled afresh");
}

/// **Each symbol has an id of its own, and an unsubscribe stops that symbol alone** (`PriceStream`'s id counter): two
/// streaming symbols never share an id, so letting one go leaves the other priced.
#[test]
fn each_symbol_has_its_own_id_and_an_unsubscribe_stops_that_symbol_alone() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, sink) = stream(&bus);
    let eur = stream.subscribe_quotes("EURUSD").expect("a");
    let gbp = stream.subscribe_quotes("GBPUSD").expect("b");
    assert_ne!(eur, gbp, "one id per symbol");
    stream.unsubscribe(eur);
    assert_eq!(wire.lines(), [sub(&["EURUSD"]), sub(&["GBPUSD"]), unsub(&["EURUSD"])]);
    tick(&bus, "GBPUSD", 1.3, 1.31);
    wait_for(WAIT_FOR, "the other symbol's quote", || sink.quotes().len() == 1);
    assert_eq!(sink.quotes()[0].2.symbol, "GBPUSD");
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

/// **Dropping a stream stops each of its symbols, as a shutdown does** (`PriceStream`'s `Drop`): one `unsubscribe` per
/// symbol, not the bus's one combined release of a consumer that went away.
#[test]
fn dropping_the_stream_stops_each_symbol_as_a_shutdown_does() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, _sink) = stream(&bus);
    stream.subscribe_quotes("EURUSD").expect("a");
    stream.subscribe_quotes("XAUUSD").expect("b");
    drop(stream);
    let lines = wire.lines();
    assert_eq!(lines.len(), 4, "two subscribes, then each symbol stopped: {lines:?}");
    assert!(
        lines.contains(&unsub(&["EURUSD"])) && lines.contains(&unsub(&["XAUUSD"])),
        "{lines:?}"
    );
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
    wait_for(WAIT_FOR, "the newer stream's quote", || newer_sink.quotes().len() == 1);
    drop(old); // the old one closing touches nothing of the new one
    tick(&bus, "XAUUSD", 2400.0, 2400.5);
    wait_for(WAIT_FOR, "the second quote", || newer_sink.quotes().len() == 2);
}

/// A displaced stream is never answered as if it still priced: a symbol it was already streaming is refused too (its
/// own books used to answer `Ok` for it), so the tradehub behind it learns the feed is gone on its next ask.
#[test]
fn a_displaced_stream_is_refused_even_for_a_symbol_it_already_streams() {
    let (bus, _wire) = bus_with_sidecar(true);
    let (mut old, old_sink) = stream(&bus);
    old.subscribe_quotes("EURUSD").expect("streaming");
    assert!(old.subscribe_quotes("EURUSD").is_ok(), "idempotent while it is the consumer");

    let (mut newer, newer_sink) = self::stream(&bus); // a second tradehub's session, or a redial
    let why = old.subscribe_quotes("EURUSD").expect_err("displaced").to_string();
    assert!(why.contains("replaced"), "{why}");

    // the bus now quotes for the newer stream only
    newer.subscribe_quotes("EURUSD").expect("the newer one asks for itself");
    tick(&bus, "EURUSD", 1.1, 1.2);
    wait_for(WAIT_FOR, "the newer stream's quote", || newer_sink.quotes().len() == 1);
    assert!(old_sink.quotes().is_empty(), "the displaced stream is not fed");
}

/// **A repeat ask of a symbol this stream already streams is answered by the stream alone, the bus is not asked
/// again** (dukascopy's `BusHold::ask`, `crates/bridges/dukascopy/src/quotes.rs`): with the sidecar gone, the
/// streaming symbol still answers its id while a new one is refused in the bus's words. fxcm's desk is asked every
/// time instead (`fxcm_quotes_tests.rs`'s `a_repeat_subscription_of_a_streaming_pair_restarts_a_poller_that_died_again`).
#[test]
fn a_repeat_ask_of_a_streaming_symbol_is_answered_without_asking_the_bus() {
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    let sidecar = bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &[FEATURE_TICKS.to_string()]);
    let (mut stream, _sink) = stream(&bus);
    let id = stream.subscribe_quotes("EURUSD").expect("the sidecar is up");
    bus.detach(sidecar);
    assert_eq!(stream.subscribe_quotes("eurusd").expect("answered by the stream"), id);
    let why = stream.subscribe_quotes("GBPUSD").expect_err("no sidecar").to_string();
    assert!(why.contains("no JForex sidecar is running"), "{why}");
    assert_eq!(wire.lines(), [sub(&["EURUSD"])], "one command, and no second ask");
}

/// The ceiling for all holders is the bus's own cap: the row in `vike-gatewayhub-client` cannot name the
/// bridge, so this crate, which links both, holds them equal. No spacing, since a stream open opens nothing.
#[test]
fn the_ceiling_row_is_the_bridges_own_cap() {
    let row = stream_limits_for("dukascopy").expect("dukascopy's row");
    assert_eq!(row.max_streams, MAX_PRICED_SYMBOLS);
    assert_eq!(row.min_open_spacing, Duration::ZERO);
}

/// One session of the venue under the production guard, built in the order `CredentialedLane::open_in_session`
/// builds it: the stream, which takes the bus, and then the admission.
fn guarded(
    guard: &SessionGuard,
    bus: &Arc<QuoteBus>,
    session: u64,
) -> (Box<dyn DataClient + Send>, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let stream = PriceStream::on(bus, Arc::clone(&sink) as Arc<dyn LiveDataSink>);
    (guard.admit("dukascopy", session, Arc::clone(&sink) as _, Box::new(stream)), sink)
}

/// **A replaced session's wind-down never stops the newer session's symbol** (decision 0193 verdict 4): the
/// older session discloses a gap on its key and is refused by the guard, and its stale hold on the bus reaches
/// the sidecar with nothing, though both sessions asked for the same symbol.
#[test]
fn a_replaced_session_discloses_its_gap_and_never_stops_the_newer_sessions_symbol() {
    let guard = SessionGuard::new(stream_limits_for);
    let (bus, wire) = bus_with_sidecar(true);
    let (mut older, older_sink) = guarded(&guard, &bus, 1);
    older.subscribe_quotes("EURUSD").expect("the first session");

    let (mut newer, newer_sink) = guarded(&guard, &bus, 2);
    wait_for(WAIT_FOR, "the older session's gap", || {
        !older_sink.stream_statuses_by_symbol().is_empty()
    });
    newer.subscribe_quotes("EURUSD").expect("the newer session");
    let stopped_by_the_takeover = [sub(&["EURUSD"]), unsub(&["EURUSD"]), sub(&["EURUSD"])];
    assert_eq!(wire.lines(), stopped_by_the_takeover);

    tick(&bus, "EURUSD", 1.1, 1.2);
    wait_for(WAIT_FOR, "the newer session's quote", || newer_sink.quotes().len() == 1);
    assert!(older_sink.quotes().is_empty());
    let why = older.subscribe_quotes("GBPUSD").expect_err("replaced").to_string();
    assert!(why.contains("was replaced by session 2"), "{why}");
    // The older session's queued release has had its reaper by now: still no line of its own.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(wire.lines(), stopped_by_the_takeover);
}

/// ⚠ **The one place this venue departs from verdict 4's last sentence, pinned so it stays a known fact.**
/// The guard lets an older session back in once its replacement has ended, and on this venue the bus still
/// refuses it: its hold was replaced for good, so the session prices nothing until it reopens. It is reachable
/// only with two live datahubs, it fails closed, and it says so in words.
#[test]
fn a_replaced_session_stays_refused_by_the_bus_after_its_replacement_ended() {
    let guard = SessionGuard::new(stream_limits_for);
    let (bus, _wire) = bus_with_sidecar(true);
    let (mut older, _older_sink) = guarded(&guard, &bus, 1);
    older.subscribe_quotes("EURUSD").expect("the first session");
    let (mut newer, _newer_sink) = guarded(&guard, &bus, 2);

    newer.shutdown();
    drop(newer);

    let why = older.subscribe_quotes("GBPUSD").expect_err("the bus's word").to_string();
    assert!(why.contains("replaced by a newer one"), "the guard let it through: {why}");
}
