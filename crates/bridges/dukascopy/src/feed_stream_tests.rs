//! The dukascopy price stream (`vike_bridge_core::data_client::bus::DataClientBus` over this crate's bus) over a
//! scripted sidecar: nothing is dialled at build, a subscription is one command to the sidecar that is already up, a
//! quote is judged by the bus and labelled real-time once, and every refusal is in words. The sidecar itself is a
//! recording pipe on the bus (the real exec client's reader is exercised in
//! `crates/bridges/dukascopy/tests/sidecar/dukascopy_quotes.rs`). MOVED from the gatewayhub's
//! `history::dukascopy_quotes` with the stream's contract (`super`'s module doc); the session guard's half stays there
//! (`crates/vike-gatewayhub/src/data/session_guard_bus_tests.rs`).

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::data_client::bus::DataClientBus;
use vike_data::{DataClient, DataClientError, DataClientSink, RecordingSink};
use vike_log::poll::wait_for;

use super::{FEATURE_TICKS, LinePort, QuoteBus};
use crate::refusals::DukascopyPriceLaneRefusal;
use crate::test_support::{Wire, subscribe_line, tick_now, unsubscribe_line};

/// How long a wait in this file allows before `vike_log::poll::wait_for` fails it.
const WAIT_FOR: Duration = Duration::from_secs(10);

/// A bus with a sidecar attached (`ticks` = whether its jar announced the price lane).
fn bus_with_sidecar(ticks: bool) -> (Arc<QuoteBus>, Arc<Wire>) {
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    let features: Vec<String> = if ticks { vec![FEATURE_TICKS.to_string()] } else { Vec::new() };
    bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &features);
    (bus, wire)
}

/// A price stream on `bus`, and the kept list it names a subscription's wire by (decision 0290).
fn stream(bus: &Arc<QuoteBus>) -> (DataClientBus<QuoteBus>, Arc<RecordingSink>) {
    crate::test_support::hold_kept_list_for_tests();
    let sink = Arc::new(RecordingSink::default());
    (DataClientBus::on(bus, Arc::clone(&sink) as Arc<dyn DataClientSink>), sink)
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
    assert_ne!(
        stream.subscribe_quotes("EURUSD").expect("a second holder"),
        id,
        "the same symbol, a holder of its own"
    );
    assert_eq!(
        wire.lines(),
        [subscribe_line(&["EURUSD"])],
        "one command, in the canonical spelling"
    );

    tick_now(&bus, "EURUSD", 1.1, 1.1002);
    tick_now(&bus, "EURUSD", 1.1001, 1.1003);
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
    assert!(
        quotes[0].2.local_timestamp > 0 && quotes[0].2.timestamp > 0,
        "both stamps are carried"
    );
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

/// The stream's subscribe refusal is the price lane's `refusal`, in its words.
#[track_caller]
fn refused_as(e: DataClientError, refusal: DukascopyPriceLaneRefusal) {
    assert!(matches!(&e, DataClientError::Subscribe(w) if *w == refusal.to_string()), "{e:?}");
}

/// No sidecar runs to price `symbol`.
fn no_sidecar(symbol: &str) -> DukascopyPriceLaneRefusal {
    DukascopyPriceLaneRefusal::NoSidecar { symbol: symbol.to_string(), purpose: "priced" }
}

#[test]
fn every_refusal_is_in_words() {
    // no sidecar at all
    let (mut stream, _sink) = stream(&QuoteBus::new());
    refused_as(stream.subscribe_quotes("EURUSD").expect_err("no sidecar"), no_sidecar("EURUSD"));

    // an old jar
    let (bus, wire) = bus_with_sidecar(false);
    let (mut stream, _sink) = self::stream(&bus);
    let old_jar = DukascopyPriceLaneRefusal::JarPredatesTicks {
        symbol: "EURUSD".to_string(),
        purpose: "priced",
    };
    refused_as(stream.subscribe_quotes("EURUSD").expect_err("old jar"), old_jar);
    assert!(wire.lines().is_empty());

    // a name no kept row answers (the SDK's enum-style name of a CFD, an old spelling): refused, nothing written
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, _sink) = self::stream(&bus);
    for name in ["COPPERCMDUSD", "AAPL.US/USD", "EUR/USD"] {
        let why = crate::instrument::names::wire_symbol(name).expect_err(name);
        refused_as(
            stream.subscribe_quotes(name).expect_err(name),
            DukascopyPriceLaneRefusal::NotPriced { why },
        );
    }
    assert!(wire.lines().is_empty());

    // every other verb, and a closed stream, in this venue's own words
    let only = |lane: &str| format!("dukascopy's price stream carries quotes only (no {lane})");
    let words = |e: DataClientError| match e {
        DataClientError::Unsupported(w) => w.to_string(),
        other => panic!("{other:?}"),
    };
    assert_eq!(words(stream.subscribe_bars("EURUSD", "1m").expect_err("bars")), only("bars"));
    assert_eq!(words(stream.subscribe_trades("EURUSD").expect_err("trades")), only("trades"));
    assert_eq!(words(stream.subscribe_book("EURUSD").expect_err("book")), only("book"));
    stream.shutdown();
    let closed = stream.subscribe_quotes("EURUSD").expect_err("closed");
    assert!(
        matches!(&closed, DataClientError::Subscribe(w) if w == "dukascopy: the price stream is closed"),
        "{closed:?}"
    );
}

/// **A subscription by the USER name subscribes the kept row's WIRE name, and its ticks are keyed by the user name
/// again** (decision 0290): the share CFD `AAPL.CFD` is `AAPL.US/USD` to the sidecar, which stamps its ticks with it, and
/// a pair is its six letters both ways.
#[test]
fn a_subscription_by_the_user_name_subscribes_the_wire_and_its_ticks_carry_the_user_name() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, sink) = stream(&bus);
    stream.subscribe_quotes("aapl.cfd").expect("a share CFD");
    stream.subscribe_quotes("EURUSD").expect("a pair");
    assert_eq!(wire.lines(), [subscribe_line(&["AAPL.US/USD"]), subscribe_line(&["EURUSD"])]);
    tick_now(&bus, "AAPL.US/USD", 190.1, 190.2);
    wait_for(WAIT_FOR, "the CFD's quote", || sink.quotes().len() == 1);
    tick_now(&bus, "EURUSD", 1.1, 1.1002);
    wait_for(WAIT_FOR, "the pair's quote", || sink.quotes().len() == 2);
    let keyed: Vec<(String, String)> = sink
        .quotes()
        .into_iter()
        .map(|(_venue, symbol, quote)| (symbol, quote.symbol.to_string()))
        .collect();
    assert_eq!(
        keyed,
        [("AAPL.CFD".to_string(), "AAPL.CFD".to_string()), ("EURUSD".into(), "EURUSD".into())]
    );
}

#[test]
fn the_cap_is_the_buses() {
    let (bus, _wire) = bus_with_sidecar(true);
    let (mut stream, _sink) = stream(&bus);
    let pairs = ["EURUSD", "GBPUSD", "AUDUSD", "NZDUSD", "USDJPY", "USDCAD", "USDCHF", "EURGBP"];
    for p in pairs {
        stream.subscribe_quotes(p).expect(p);
    }
    refused_as(
        stream.subscribe_quotes("EURJPY").expect_err("past the cap"),
        DukascopyPriceLaneRefusal::AtMostPriced { max: 8, symbol: "EURJPY".to_string() },
    );
}

#[test]
fn an_unsubscribe_stops_the_command_and_the_quotes_and_a_resubscribe_labels_again() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, sink) = stream(&bus);
    let id = stream.subscribe_quotes("EURUSD").expect("subscribed");
    tick_now(&bus, "EURUSD", 1.1, 1.2);
    wait_for(WAIT_FOR, "the first quote", || sink.quotes().len() == 1);
    stream.unsubscribe(id);
    stream.unsubscribe(id); // unknown now: a no-op
    assert_eq!(wire.lines(), [subscribe_line(&["EURUSD"]), unsubscribe_line(&["EURUSD"])]);
    tick_now(&bus, "EURUSD", 1.1, 1.2);
    assert_eq!(sink.quotes().len(), 1, "a stopped symbol is not emitted");

    stream.subscribe_quotes("EURUSD").expect("again");
    tick_now(&bus, "EURUSD", 1.1, 1.2);
    wait_for(WAIT_FOR, "the re-subscribed quote", || sink.quotes().len() == 2);
    let labels = sink.calls().iter().filter(|c| c.starts_with("data_delay")).count();
    assert_eq!(labels, 2, "a fresh subscription is labelled afresh");
}

/// **Each symbol has an id of its own, and an unsubscribe stops that symbol alone** (`DataClientBus`'s id counter): two
/// streaming symbols never share an id, so letting one go leaves the other priced.
#[test]
fn each_symbol_has_its_own_id_and_an_unsubscribe_stops_that_symbol_alone() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, sink) = stream(&bus);
    let eur = stream.subscribe_quotes("EURUSD").expect("a");
    let gbp = stream.subscribe_quotes("GBPUSD").expect("b");
    assert_ne!(eur, gbp, "one id per symbol");
    stream.unsubscribe(eur);
    assert_eq!(
        wire.lines(),
        [subscribe_line(&["EURUSD"]), subscribe_line(&["GBPUSD"]), unsubscribe_line(&["EURUSD"])]
    );
    tick_now(&bus, "GBPUSD", 1.3, 1.31);
    wait_for(WAIT_FOR, "the other symbol's quote", || sink.quotes().len() == 1);
    assert_eq!(sink.quotes()[0].2.symbol, "GBPUSD");
}

/// **Holders are counted: one leaving holder never stops a symbol another still holds.** Each subscribe is a holder
/// with an id of its own; the sidecar hears the `unsubscribe` only when the LAST holder of the symbol leaves.
#[test]
fn a_symbol_stops_only_when_its_last_holder_leaves() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut stream, sink) = stream(&bus);
    let first = stream.subscribe_quotes("EURUSD").expect("a holder");
    let second = stream.subscribe_quotes("EURUSD").expect("another holder");
    assert_ne!(first, second, "one id per holder");

    stream.unsubscribe(first);
    assert_eq!(wire.lines(), [subscribe_line(&["EURUSD"])], "the other holder still holds it");
    tick_now(&bus, "EURUSD", 1.1, 1.2);
    wait_for(WAIT_FOR, "the remaining holder's quote", || sink.quotes().len() == 1);

    stream.unsubscribe(first); // gone already: a no-op
    assert_eq!(wire.lines(), [subscribe_line(&["EURUSD"])]);
    stream.unsubscribe(second);
    assert_eq!(
        wire.lines(),
        [subscribe_line(&["EURUSD"]), unsubscribe_line(&["EURUSD"])],
        "the last holder left"
    );
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
    tick_now(&bus, "EURUSD", 1.1, 1.2);
    assert!(sink.quotes().is_empty());
    let closed = stream.subscribe_quotes("EURUSD").expect_err("closed");
    assert!(
        matches!(&closed, DataClientError::Subscribe(w) if w == "dukascopy: the price stream is closed"),
        "{closed:?}"
    );
    drop(stream);
    // a fresh stream on the same bus starts clean
    let (mut next, _sink) = self::stream(&bus);
    next.subscribe_quotes("EURUSD").expect("starts clean");
}

/// **Dropping a stream stops each of its symbols, as a shutdown does** (`DataClientBus`'s `Drop`): one `unsubscribe` per
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
        lines.contains(&unsubscribe_line(&["EURUSD"]))
            && lines.contains(&unsubscribe_line(&["XAUUSD"])),
        "{lines:?}"
    );
}

#[test]
fn a_newer_stream_replaces_the_older() {
    let (bus, _wire) = bus_with_sidecar(true);
    let (mut old, _old_sink) = stream(&bus);
    old.subscribe_quotes("EURUSD").expect("old stream");
    let (mut newer, newer_sink) = self::stream(&bus);
    refused_as(
        old.subscribe_quotes("XAUUSD").expect_err("replaced"),
        DukascopyPriceLaneRefusal::NotCurrent,
    );
    newer.subscribe_quotes("XAUUSD").expect("the newer one works");
    tick_now(&bus, "XAUUSD", 2400.0, 2400.5);
    wait_for(WAIT_FOR, "the newer stream's quote", || newer_sink.quotes().len() == 1);
    drop(old); // the old one closing touches nothing of the new one
    tick_now(&bus, "XAUUSD", 2400.0, 2400.5);
    wait_for(WAIT_FOR, "the second quote", || newer_sink.quotes().len() == 2);
}

/// A displaced stream is never answered as if it still priced: a symbol it was already streaming is refused too (its
/// own books used to answer `Ok` for it), so the tradehub behind it learns the feed is gone on its next ask.
#[test]
fn a_displaced_stream_is_refused_even_for_a_symbol_it_already_streams() {
    let (bus, _wire) = bus_with_sidecar(true);
    let (mut old, old_sink) = stream(&bus);
    old.subscribe_quotes("EURUSD").expect("streaming");
    assert!(old.subscribe_quotes("EURUSD").is_ok(), "a second holder while it is the consumer");

    let (mut newer, newer_sink) = self::stream(&bus); // a second tradehub's session, or a redial
    refused_as(
        old.subscribe_quotes("EURUSD").expect_err("displaced"),
        DukascopyPriceLaneRefusal::NotCurrent,
    );

    // the bus now quotes for the newer stream only
    newer.subscribe_quotes("EURUSD").expect("the newer one asks for itself");
    tick_now(&bus, "EURUSD", 1.1, 1.2);
    wait_for(WAIT_FOR, "the newer stream's quote", || newer_sink.quotes().len() == 1);
    assert!(old_sink.quotes().is_empty(), "the displaced stream is not fed");
}

/// **A repeat ask of a symbol this stream already streams is answered by the stream alone, the bus is not asked
/// again** (dukascopy's `DataClientBusSpecHold::ask`, `crates/bridges/dukascopy/src/feed.rs`): with the sidecar gone, the
/// streaming symbol still admits another holder while a new one is refused in the bus's words. fxcm's desk is asked
/// every time instead (`crates/bridges/fxcm/src/feed_stream_tests.rs`'s
/// `a_repeat_subscription_of_a_streaming_pair_restarts_a_poller_that_died_again`).
#[test]
fn a_repeat_ask_of_a_streaming_symbol_is_answered_without_asking_the_bus() {
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    let sidecar = bus.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &[FEATURE_TICKS.to_string()]);
    let (mut stream, _sink) = stream(&bus);
    let id = stream.subscribe_quotes("EURUSD").expect("the sidecar is up");
    bus.detach(sidecar);
    assert_ne!(stream.subscribe_quotes("eurusd").expect("answered by the stream"), id);
    refused_as(stream.subscribe_quotes("GBPUSD").expect_err("no sidecar"), no_sidecar("GBPUSD"));
    assert_eq!(wire.lines(), [subscribe_line(&["EURUSD"])], "one command, and no second ask");
}

/// **A stream let back in is never told a stopped symbol still streams** (`DataClientBus::subscribe_quotes`'s
/// promise, with no guard in front of it): the takeover stopped its wants, so its repeat ask goes back to the
/// bus, as a second holder of the symbol, and the symbol is priced.
#[test]
fn a_stream_let_back_in_asks_the_bus_again_for_a_symbol_it_held() {
    let (bus, wire) = bus_with_sidecar(true);
    let (mut old, old_sink) = stream(&bus);
    let id = old.subscribe_quotes("EURUSD").expect("streaming");
    let (newer, _newer_sink) = self::stream(&bus);
    drop(newer);

    assert_ne!(old.subscribe_quotes("EURUSD").expect("let back in"), id);
    assert_eq!(wire.lines().last(), Some(&subscribe_line(&["EURUSD"])));
    tick_now(&bus, "EURUSD", 1.1, 1.2);
    wait_for(WAIT_FOR, "the quote", || old_sink.quotes().len() == 1);
}
