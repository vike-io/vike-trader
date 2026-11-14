//! The engine over a scripted source that carries every lane, with a cap and the price check on: each lane is a
//! key of its own, holders are counted, the cap counts symbols across lanes, a label goes out once per change,
//! an outage opens one gap per held key, and a bad price is dropped. The two production sources (dukascopy's bus,
//! fxcm's desk) are their bridges' `crates/bridges/dukascopy/src/feed_stream_tests.rs` and
//! `crates/bridges/fxcm/src/feed_stream_tests.rs`.

use std::sync::{Arc, Mutex};

use vike_data::{DataClient, DataClientError, DataClientSink, RecordingSink};
use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};
use vike_model::{StreamDelay, StreamStatus};

use super::*;

/// What the source was asked, and its consumer's deliver.
#[derive(Default)]
struct Script {
    asked: Mutex<Vec<String>>,
    deliver: Mutex<Option<DataClientBusSpecDeliver>>,
}

/// A source carrying every lane, at most two symbols, with the price check on.
#[derive(Default)]
struct Every(Arc<Script>);

struct ScriptHold(Arc<Script>);

impl DataClientBusSpec for Every {
    type Hold = ScriptHold;
    const VENUE: &'static str = "scripted";
    const CLOSED: &'static str = "scripted: the stream is closed";
    const BARS: Result<(), &'static str> = Ok(());
    const TRADES: Result<(), &'static str> = Ok(());
    const BOOK: Result<(), &'static str> = Ok(());
    const MAX_SYMBOLS: Option<(usize, &'static str)> = Some((2, "the venue's line limit"));
    const PRICE_CHECK: bool = true;

    fn consume(self: &Arc<Self>, deliver: DataClientBusSpecDeliver) -> ScriptHold {
        *self.0.deliver.lock().unwrap() = Some(deliver);
        ScriptHold(Arc::clone(&self.0))
    }
}

impl DataClientBusSpecHold for ScriptHold {
    /// A `1s` bar is a venue that never serves it; `REFUSED` a symbol the source refuses this time.
    fn ask(&self, key: &DataClientBusSpecKey, streaming: bool) -> Result<(), DataClientError> {
        self.0.asked.lock().unwrap().push(format!("ask {key:?} streaming={streaming}"));
        match key {
            DataClientBusSpecKey::Bars { interval, .. } if interval == "1s" => {
                Err(DataClientError::Unsupported("scripted: no 1s bars"))
            }
            key if key.symbol() == "REFUSED" => Err(DataClientError::Subscribe("not now".into())),
            _ => Ok(()),
        }
    }
    fn unwant(&self, key: &DataClientBusSpecKey) {
        self.0.asked.lock().unwrap().push(format!("unwant {key:?}"));
    }
    fn begin_shutdown(&self) {
        self.0.asked.lock().unwrap().push("begin_shutdown".to_string());
    }
}

/// [`Every`] with the four opt-in knobs turned: the caller's spelling, bar-close snapshots, keys let go at
/// `begin_shutdown`, a shutdown that leaves the teardown to the hold's drop.
#[derive(Default)]
struct Knobs(Arc<Script>);

impl DataClientBusSpec for Knobs {
    type Hold = ScriptHold;
    const VENUE: &'static str = "knobs";
    const CLOSED: &'static str = "knobs: the stream is closed";
    const BARS: Result<(), &'static str> = Ok(());
    const TRADES: Result<(), &'static str> = Ok(());
    const BOOK: Result<(), &'static str> = Ok(());
    const MAX_SYMBOLS: Option<(usize, &'static str)> = None;
    const PRICE_CHECK: bool = false;
    const SYMBOL_VERBATIM: bool = true;
    const BAR_CLOSE_TICK: bool = true;
    const RELEASE_AT_BEGIN_SHUTDOWN: bool = true;
    const UNWANT_AT_SHUTDOWN: bool = false;

    fn consume(self: &Arc<Self>, deliver: DataClientBusSpecDeliver) -> ScriptHold {
        *self.0.deliver.lock().unwrap() = Some(deliver);
        ScriptHold(Arc::clone(&self.0))
    }
}

fn knobs_client() -> (DataClientBus<Knobs>, Arc<Script>, Arc<RecordingSink>) {
    let source = Arc::new(Knobs::default());
    let sink = Arc::new(RecordingSink::default());
    let client = DataClientBus::on(&source, Arc::clone(&sink) as Arc<dyn DataClientSink>);
    (client, Arc::clone(&source.0), sink)
}

impl Script {
    fn send(&self, event: DataClientBusSpecEvent<'_>) {
        (self.deliver.lock().unwrap().as_ref().expect("a consumer"))(event);
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

fn client() -> (DataClientBus<Every>, Arc<Script>, Arc<RecordingSink>) {
    let source = Arc::new(Every::default());
    let sink = Arc::new(RecordingSink::default());
    let client = DataClientBus::on(&source, Arc::clone(&sink) as Arc<dyn DataClientSink>);
    (client, Arc::clone(&source.0), sink)
}

fn quote(bid: f64, ask: f64) -> TickQuote {
    TickQuote {
        timestamp: 1,
        local_timestamp: 0,
        bid,
        ask,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    }
}

fn trade(price: f64, size: f64) -> TickTrade {
    TickTrade {
        timestamp: 1,
        local_timestamp: 0,
        price,
        size,
        is_buyer_maker: false,
        symbol: String::new(),
    }
}

fn statuses(sink: &RecordingSink) -> Vec<(String, String, StreamStatus)> {
    sink.stream_statuses()
        .into_iter()
        .map(|(_, symbol, stream, status)| (symbol, stream, status))
        .collect()
}

#[test]
fn every_lane_is_a_key_of_its_own_and_its_data_reaches_the_sink() {
    let (mut client, source, sink) = client();
    client.subscribe_quotes(" eurusd ").expect("quotes");
    client.subscribe_trades("EURUSD").expect("trades");
    client.subscribe_book("EURUSD").expect("book");
    client.subscribe_bars("eurusd", "1m").expect("bars");
    assert_eq!(
        source.asked(),
        [
            "ask Quotes(\"EURUSD\") streaming=false",
            "ask Trades(\"EURUSD\") streaming=false",
            "ask Book(\"EURUSD\") streaming=false",
            "ask Bars { symbol: \"EURUSD\", interval: \"1m\" } streaming=false",
        ],
        "one key per lane, in the engine's spelling"
    );

    source.send(DataClientBusSpecEvent::Quote { symbol: "EURUSD", quote: quote(1.1, 1.2) });
    source.send(DataClientBusSpecEvent::Trade { symbol: "EURUSD", trade: trade(1.15, 2.0) });
    source
        .send(DataClientBusSpecEvent::Book { symbol: "EURUSD", book: Arc::new(L2Book::new(0.1)) });
    let (symbol, interval) = ("EURUSD", "1m");
    let bar = Bar::ohlcv(60_000, 1.0, 1.2, 0.9, 1.1, 5.0);
    source.send(DataClientBusSpecEvent::BarSeed {
        symbol,
        interval,
        bars: vec![bar.clone(), bar.clone()],
    });
    source.send(DataClientBusSpecEvent::Bar { symbol, interval, bar: bar.clone(), closed: true });
    source.send(DataClientBusSpecEvent::Bar { symbol, interval, bar, closed: false });

    let (q, t) = (sink.quotes().remove(0).2, sink.trades().remove(0).2);
    assert!(q.local_timestamp > 0 && t.local_timestamp > 0, "the receive time is stamped here");
    assert_eq!((q.symbol.as_str(), t.symbol.as_str()), ("EURUSD", "EURUSD"));
    assert_eq!(sink.books().len(), 1);
    assert_eq!(sink.seeded_bars()[0].3.len(), 2);
    assert_eq!((sink.closed_bars().len(), sink.forming_bars().len()), (1, 1));
    assert!(sink.calls().iter().all(|c| c.contains("scripted")), "{:?}", sink.calls());
}

/// **Holders are counted per key**: every subscribe asks the source, saying whether the key already streams; one
/// leaving holder never stops a key another still holds, and the source hears `unwant` once, from the last.
#[test]
fn holders_are_counted_and_only_the_last_one_leaving_stops_the_key() {
    let (mut client, source, _sink) = client();
    let first = client.subscribe_quotes("EURUSD").expect("a holder");
    let second = client.subscribe_quotes("EURUSD").expect("another");
    assert_ne!(first, second, "one id per holder");
    client.subscribe_trades("EURUSD").expect("a key of its own");
    let asks = [
        "ask Quotes(\"EURUSD\") streaming=false",
        "ask Quotes(\"EURUSD\") streaming=true",
        "ask Trades(\"EURUSD\") streaming=false",
    ];
    assert_eq!(source.asked(), asks);

    client.unsubscribe(first);
    client.unsubscribe(first); // gone already: a no-op
    assert_eq!(source.asked(), asks, "the other holder still holds it");
    client.unsubscribe(second);
    assert_eq!(source.asked().last().map(String::as_str), Some("unwant Quotes(\"EURUSD\")"));
}

/// **The cap counts symbols, across lanes**: another lane of a held symbol is no new symbol, a new one past the cap
/// is refused in the venue's words, and a symbol let go frees its place.
#[test]
fn the_cap_counts_symbols_across_lanes() {
    let (mut client, source, _sink) = client();
    let eur = client.subscribe_quotes("EURUSD").expect("a");
    client.subscribe_trades("EURUSD").expect("the same symbol");
    client.subscribe_quotes("GBPUSD").expect("b");
    let asked = source.asked().len();
    let why = client.subscribe_bars("USDJPY", "1m").expect_err("past the cap").to_string();
    assert!(
        why.contains(
            "scripted: at most 2 symbols are priced at once (the venue's line limit); USDJPY was not subscribed"
        ),
        "{why}"
    );
    assert_eq!(source.asked().len(), asked, "the source is not asked past the cap");
    client.subscribe_quotes("GBPUSD").expect("a held symbol is no new one");

    client.unsubscribe(eur);
    assert!(client.subscribe_quotes("USDJPY").is_err(), "EURUSD's trades still hold it");
}

/// **One label rule**: sent when first known and again on a change, never per message; a key whose last holder
/// leaves forgets it, so a fresh subscription is labelled afresh. A bar lane's label is its interval's.
#[test]
fn a_label_goes_out_when_first_known_and_on_a_change_never_per_message() {
    let (mut client, source, sink) = client();
    let id = client.subscribe_quotes("EURUSD").expect("quotes");
    let label = |stream: &'static str, delay| DataClientBusSpecEvent::Delay {
        symbol: "EURUSD",
        stream,
        delay,
    };
    source.send(label("quotes", StreamDelay::RealTime));
    source.send(label("quotes", StreamDelay::RealTime));
    source.send(label("quotes", StreamDelay::DELAYED));
    source.send(label("1m", StreamDelay::DELAYED));
    let words = |sink: &RecordingSink| -> Vec<String> {
        sink.data_delays()
            .into_iter()
            .map(|(_, _, stream, delay)| format!("{stream}:{delay:?}"))
            .collect()
    };
    assert_eq!(
        words(&sink),
        ["quotes:RealTime", "quotes:Delayed { lag_ms: None }", "1m:Delayed { lag_ms: None }"]
    );

    client.unsubscribe(id);
    client.subscribe_quotes("EURUSD").expect("again");
    source.send(label("quotes", StreamDelay::DELAYED));
    assert_eq!(words(&sink).len(), 4, "a fresh subscription is labelled afresh");
}

/// **One outage voice**: `Down` opens ONE gap per held key, `Up` closes each with a `Live` echoing it, and a key let
/// go or shut down while its gap is open closes it too.
#[test]
fn an_outage_opens_one_gap_per_held_key_and_live_closes_each() {
    let (mut client, source, sink) = client();
    let eur = client.subscribe_quotes("EURUSD").expect("quotes");
    client.subscribe_bars("EURUSD", "1m").expect("bars");

    let told = |stream: &str, status| ("EURUSD".to_string(), stream.to_string(), status);

    source.send(DataClientBusSpecEvent::Down { at_ms: 100 });
    source.send(DataClientBusSpecEvent::Down { at_ms: 200 }); // the same outage: told once
    let mut gaps = statuses(&sink);
    gaps.sort_by(|a, b| a.1.cmp(&b.1));
    let gap = StreamStatus::GapStart { at_timestamp_ms: 100 };
    assert_eq!(gaps, [told("1m", gap), told("quotes", gap)]);

    source.send(DataClientBusSpecEvent::Up);
    source.send(DataClientBusSpecEvent::Up); // nothing open
    let live = StreamStatus::Live { gap_started_timestamp_ms: Some(100) };
    assert_eq!(
        statuses(&sink)[2..].iter().filter(|s| s.2 == live).count(),
        2,
        "{:?}",
        statuses(&sink)
    );
    assert_eq!(statuses(&sink).len(), 4);

    source.send(DataClientBusSpecEvent::Down { at_ms: 300 });
    client.unsubscribe(eur);
    let reopened = StreamStatus::Live { gap_started_timestamp_ms: Some(300) };
    assert_eq!(statuses(&sink).last(), Some(&told("quotes", reopened)), "let go: closed");
    client.shutdown();
    assert_eq!(statuses(&sink).last(), Some(&told("1m", reopened)), "shut down: closed");
    assert_eq!(statuses(&sink).len(), 8);
}

/// **The price check** (`PRICE_CHECK`): a one-sided, crossed or non-finite quote and a print with no positive,
/// finite price or size are dropped; a good one passes.
#[test]
fn the_price_check_drops_a_bad_quote_or_print() {
    let (mut client, source, sink) = client();
    client.subscribe_quotes("EURUSD").expect("quotes");
    client.subscribe_trades("EURUSD").expect("trades");
    for (bid, ask) in [(0.0, 1.1), (1.2, 1.1), (f64::NAN, 1.1), (1.1, f64::INFINITY), (1.1, 1.2)] {
        source.send(DataClientBusSpecEvent::Quote { symbol: "EURUSD", quote: quote(bid, ask) });
    }
    for (price, size) in [(0.0, 1.0), (-1.0, 1.0), (1.0, 0.0), (f64::NAN, 1.0), (1.0, 1.0)] {
        source.send(DataClientBusSpecEvent::Trade { symbol: "EURUSD", trade: trade(price, size) });
    }
    assert_eq!(
        sink.calls(),
        ["quote:scripted:EURUSD:1.1/1.2:1x1", "trade:scripted:EURUSD:1/1:maker=false"]
    );
}

/// **A shutdown lets every key go once**, whatever its holder count, and a closed client refuses in its words.
#[test]
fn a_shutdown_lets_every_key_go_once_and_a_closed_client_refuses() {
    let (mut client, source, _sink) = client();
    client.subscribe_quotes("EURUSD").expect("a");
    client.subscribe_quotes("EURUSD").expect("b");
    client.subscribe_trades("EURUSD").expect("c");
    client.shutdown();
    let mut unwants: Vec<String> =
        source.asked().into_iter().filter(|a| a.starts_with("unwant")).collect();
    unwants.sort();
    assert_eq!(unwants, ["unwant Quotes(\"EURUSD\")", "unwant Trades(\"EURUSD\")"]);
    let closed = client.subscribe_quotes("EURUSD").expect_err("closed");
    assert!(
        matches!(&closed, DataClientError::Subscribe(w) if w == "scripted: the stream is closed"),
        "{closed:?}"
    );
    drop(client);
    assert_eq!(
        source.asked().iter().filter(|a| a.starts_with("unwant")).count(),
        2,
        "dropped: nothing more"
    );
}

/// **A source's own refusal keeps its class**: an `Unsupported` answer reaches the caller as one, a failed ask as a
/// `Subscribe`, and neither leaves a holder behind.
#[test]
fn an_ask_refusal_keeps_its_class_and_holds_nothing() {
    let (mut client, source, _sink) = client();
    let never = client.subscribe_bars("EURUSD", "1s").expect_err("never served");
    assert!(matches!(never, DataClientError::Unsupported("scripted: no 1s bars")), "{never:?}");
    let now = client.subscribe_quotes("REFUSED").expect_err("refused this time");
    assert!(matches!(&now, DataClientError::Subscribe(w) if w == "not now"), "{now:?}");
    client.subscribe_bars("EURUSD", "1s").expect_err("still never");
    assert!(
        source.asked().iter().all(|a| a.ends_with("streaming=false")),
        "a refused key is no holder: {:?}",
        source.asked()
    );
}

/// **The knobs**: the caller's spelling reaches the source, a bar files its close as the last-price snapshot ahead of
/// itself, and `begin_shutdown` lets every key go before it tells the hold, keeping the client open.
#[test]
fn the_knobs_keep_the_spelling_file_the_bar_close_and_release_at_begin_shutdown() {
    let (mut client, source, sink) = knobs_client();
    let held = client.subscribe_quotes(" eurusd.m ").expect("quotes");
    client.subscribe_bars("eurusd.m", "1m").expect("bars");
    assert_eq!(
        source.asked(),
        [
            "ask Quotes(\" eurusd.m \") streaming=false",
            "ask Bars { symbol: \"eurusd.m\", interval: \"1m\" } streaming=false",
        ]
    );
    let bar = Bar::ohlcv(60_000, 1.0, 1.2, 0.9, 1.1, 5.0);
    let (symbol, interval) = ("eurusd.m", "1m");
    source.send(DataClientBusSpecEvent::Bar { symbol, interval, bar: bar.clone(), closed: true });
    source.send(DataClientBusSpecEvent::Bar { symbol, interval, bar, closed: false });
    assert_eq!(
        sink.calls(),
        [
            "bar_close_tick(knobs,eurusd.m,1.1,60000)",
            "close_bar(knobs,eurusd.m,1m,1.1)",
            "bar_close_tick(knobs,eurusd.m,1.1,60000)",
            "forming_bar(knobs,eurusd.m,1m,1.1)",
        ]
    );

    client.begin_shutdown();
    let told: Vec<String> = source.asked().into_iter().skip(2).collect();
    assert_eq!(told.last().map(String::as_str), Some("begin_shutdown"), "{told:?}");
    assert_eq!(told.iter().filter(|a| a.starts_with("unwant")).count(), 2, "{told:?}");
    client.unsubscribe(held); // forgotten at begin_shutdown: the no-op of an unknown id
    assert_eq!(source.asked().len(), 5);
    client.subscribe_quotes(" eurusd.m ").expect("the client stays open");
    assert_eq!(
        source.asked().last().map(String::as_str),
        Some("ask Quotes(\" eurusd.m \") streaming=false")
    );
}

/// Without `UNWANT_AT_SHUTDOWN` a shutdown tells the source nothing per key (its hold's drop tears it down) and still
/// closes each key's open gap.
#[test]
fn a_shutdown_without_unwant_closes_the_gaps_and_tells_the_source_nothing() {
    let (mut client, source, sink) = knobs_client();
    client.subscribe_quotes("EURUSD").expect("quotes");
    source.send(DataClientBusSpecEvent::Down { at_ms: 5 });
    client.shutdown();
    assert!(!source.asked().iter().any(|a| a.starts_with("unwant")), "{:?}", source.asked());
    let live = StreamStatus::Live { gap_started_timestamp_ms: Some(5) };
    assert_eq!(statuses(&sink).last(), Some(&("EURUSD".to_string(), "quotes".to_string(), live)));
}

/// Without the knob, `begin_shutdown` only tells the hold: every key stays held and the client stays open.
#[test]
fn begin_shutdown_reaches_the_hold_and_keeps_the_keys() {
    let (mut client, source, _sink) = client();
    let id = client.subscribe_quotes("EURUSD").expect("quotes");
    client.begin_shutdown();
    assert_eq!(source.asked().last().map(String::as_str), Some("begin_shutdown"));
    client.unsubscribe(id);
    assert_eq!(source.asked().last().map(String::as_str), Some("unwant Quotes(\"EURUSD\")"));
}

/// **A source's own status and its raw book update reach the sink as they came**, under the word it named.
#[test]
fn a_status_and_a_book_update_pass_through() {
    let (mut client, source, sink) = client();
    client.subscribe_book("EURUSD").expect("book");
    let gap = StreamStatus::GapStart { at_timestamp_ms: 7 };
    source.send(DataClientBusSpecEvent::Status { symbol: "EURUSD", stream: "tape", status: gap });
    let update = L2BookUpdate {
        timestamp: 1,
        local_timestamp: 2,
        sequence: 3,
        kind: vike_marketdata::L2BookUpdateKind::Delta,
        tick_size: 0.01,
        bids: Vec::new(),
        asks: Vec::new(),
        symbol: String::new(),
    };
    source.send(DataClientBusSpecEvent::BookUpdate { symbol: "EURUSD", update });
    assert_eq!(
        sink.calls(),
        [
            "stream_status:scripted:EURUSD:tape:GapStart { at_timestamp_ms: 7 }",
            "book_update:scripted:EURUSD:Delta:seq=3:bids=0:asks=0:local_ts_pos=true:tick=0.01",
        ]
    );
}

/// **The fixed sink**: a source built with it speaks the sink's verbs, and each reaches the engine as its event, so
/// the price check and the receive time apply; nothing goes anywhere before a consumer attaches or after it detaches.
#[test]
fn the_fixed_sink_turns_each_verb_into_its_event() {
    let relay = DataClientBusSink::default();
    relay.quote("scripted", "EURUSD", quote(1.1, 1.2)); // no consumer yet: dropped
    let (mut client, source, sink) = client();
    client.subscribe_quotes("EURUSD").expect("quotes");
    relay.attach(source.deliver.lock().unwrap().take().expect("consumed"));
    relay.quote("ignored", "EURUSD", quote(1.2, 1.1)); // crossed: the engine's check drops it
    relay.quote("ignored", "EURUSD", quote(1.1, 1.2));
    relay.trade("ignored", "EURUSD", trade(1.1, 1.0));
    let bar = Bar::ohlcv(60_000, 1.0, 1.2, 0.9, 1.1, 5.0);
    relay.seed_bars("ignored", "EURUSD", "1m", vec![bar.clone()]);
    relay.close_bar("ignored", "EURUSD", "1m", bar.clone());
    relay.forming_bar("ignored", "EURUSD", "1m", bar);
    relay.data_delay("ignored", "EURUSD", "quotes", StreamDelay::RealTime);
    relay.stream_status(
        "ignored",
        "EURUSD",
        "quotes",
        StreamStatus::GapStart { at_timestamp_ms: 1 },
    );
    relay.mark_tick("ignored", "EURUSD", 1.0, 1); // no mark lane on the bus
    assert_eq!(
        sink.calls(),
        [
            "quote:scripted:EURUSD:1.1/1.2:1x1",
            "trade:scripted:EURUSD:1.1/1:maker=false",
            "seed_bars(scripted,EURUSD,1m,1)",
            "close_bar(scripted,EURUSD,1m,1.1)",
            "forming_bar(scripted,EURUSD,1m,1.1)",
            "data_delay:scripted:EURUSD:quotes:RealTime",
            "stream_status:scripted:EURUSD:quotes:GapStart { at_timestamp_ms: 1 }",
        ]
    );
    assert!(sink.quotes()[0].2.local_timestamp > 0, "the engine stamped the receive time");
    relay.detach();
    relay.quote("ignored", "EURUSD", quote(1.1, 1.2));
    assert_eq!(sink.quotes().len(), 1, "detached: dropped");
}
