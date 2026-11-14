//! The engine's own suite: the lifecycle every venue's `Feeds` pinned over its own copy (once, over a
//! test spec whose lanes idle), the refusals, the thread rule, the status words of a `Session` lane
//! over a scripted socket, and the one emission.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_data::{DataClient, DataClientError, RecordingSink, SinkCall};
use vike_marketdata::{Bar, FundingRate, L2Book, L2BookLevel, TickQuote, TickTrade};

use super::*;
use crate::klines::kline_to_bar;
use crate::market_feed_pins::{self, DataClientPins, MarkPins};
use crate::scripted::{LaneStatus, one_session};

/// A spec over binance's declared rows (its caps serve bars, trades and depth and refuse quotes and
/// the lossless book; its pump row has no subscribe frame and no keepalive). Every lane idles until
/// stopped, noting its thread's name; `UNROUTED` has no plan, and a `7m` bar is refused.
#[derive(Default)]
struct Spec {
    names: Arc<Mutex<Vec<String>>>,
    ended: Arc<AtomicUsize>,
    /// The spec's teardown hooks, in order, each with how many lanes had ended when it ran.
    teardown: Arc<Mutex<Vec<String>>>,
}

impl DataClientSpec for Spec {
    const VENUE: &'static str = "binance";
    const DISPLAY: &'static str = "Test";
    const MARK_STREAMS_DEFAULT_ON: bool = true;

    fn pairs_mark(&self, symbol: &str) -> bool {
        symbol.ends_with(".P")
    }

    fn refuse_bars(&self, interval: &str) -> Result<(), String> {
        if interval == "7m" { Err("no 7m series".into()) } else { Ok(()) }
    }

    fn plan(
        &self,
        _lane: DataClientLane,
        symbol: &str,
        _interval: &str,
    ) -> Result<DataClientLanePlan, String> {
        if symbol == "UNROUTED" {
            return Err("no route".into());
        }
        let (names, ended) = (Arc::clone(&self.names), Arc::clone(&self.ended));
        Ok(DataClientLanePlan::Own(Box::new(move |ctx: &DataClientContext| {
            let name = std::thread::current().name().unwrap_or_default().to_string();
            names.lock().unwrap().push(name);
            market_feed_pins::idle_until_stopped(&ctx.stop);
            ended.fetch_add(1, Ordering::SeqCst);
        })))
    }

    fn begin_shutdown(&self) {
        self.teardown.lock().unwrap().push("begin_shutdown".into());
    }

    fn shutdown(&self) {
        let ended = self.ended.load(Ordering::SeqCst);
        self.teardown.lock().unwrap().push(format!("shutdown after {ended} lanes ended"));
    }
}

/// [`Spec`] whose REST seed holds closed bars only (okx's `history-candles`).
struct ClosedSeedSpec;

impl DataClientSpec for ClosedSeedSpec {
    const VENUE: &'static str = "binance";
    const DISPLAY: &'static str = "Test";
    const MARK_STREAMS_DEFAULT_ON: bool = true;
    const SEED_LAST_BAR_FORMING: bool = false;

    fn plan(&self, _: DataClientLane, _: &str, _: &str) -> Result<DataClientLanePlan, String> {
        Err("plans are handed in".into())
    }
}

fn fresh() -> DataClientWebsocket<Spec> {
    DataClientWebsocket::new(Spec::default(), Arc::new(RecordingSink::default()), || {})
}

/// Wait (bounded) until `done` holds.
fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

const PINS: DataClientPins<DataClientWebsocket<Spec>> = DataClientPins {
    spawn: |ws, symbol, interval| {
        ws.spawn(DataClientLane::Bars, symbol, interval).expect("spawn ok")
    },
    registry: |ws| &ws.registry,
    intervals: ["1m", "5m"],
    mark: Some(MarkPins {
        pair: |ws, bars_id, symbol| ws.pair_mark(bars_id, symbol),
        pairings: |ws| &ws.mark_pairings,
    }),
};

#[test]
fn subscribe_returns_distinct_ids() {
    PINS.distinct_ids(fresh(), ["BTCUSDT", "ETHUSDT"]);
}

#[test]
fn unsubscribe_stops_only_that_one_feed_others_keep_running() {
    PINS.unsubscribe_keeps_the_others(fresh(), ["BTCUSDT", "ETHUSDT"]);
}

#[test]
fn unsubscribe_of_an_unknown_id_is_a_no_op() {
    PINS.unknown_unsubscribe_is_a_no_op(fresh(), "BTCUSDT");
}

#[test]
fn shutdown_joins_every_feed() {
    PINS.shutdown_joins_every_feed(fresh(), &["BTCUSDT", "ETHUSDT", "SOLUSDT"]);
}

#[test]
fn a_perp_bars_subscription_spawns_and_then_stops_its_mark_stream() {
    PINS.bars_spawn_and_stop_their_mark(fresh(), ["BTCUSDT.P", "ETHUSDT"]);
}

#[test]
fn a_spot_bars_subscription_spawns_no_mark_stream() {
    PINS.pairs_no_mark(fresh(), "BTCUSDT", "no companion stream for spot");
}

#[test]
fn the_mark_streams_knob_off_suppresses_the_perp_spawn() {
    let mut ws = fresh();
    ws.mark_streams = false;
    PINS.pairs_no_mark(ws, "BTCUSDT.P", "knob off -> no mark stream even for a perp");
}

#[test]
fn two_intervals_on_one_perp_share_a_single_mark_stream() {
    PINS.two_intervals_share_one_mark(fresh(), "BTCUSDT.P");
}

/// Decision 0095: the `mark_streams` row decides, and no row keeps the spec's default.
#[test]
fn the_mark_streams_row_decides_and_no_row_keeps_the_spec_default() {
    market_feed_pins::mark_streams_row_decides(
        fresh,
        DataClientWebsocket::with_mark_streams,
        |ws| ws.mark_streams,
        true,
    );
}

/// `subscribe_bars` itself pairs a perp's mark stream (the spec's naming), and a spot one's not.
#[test]
fn subscribe_bars_pairs_the_mark_of_a_perp_only() {
    let mut ws = fresh();
    ws.subscribe_bars("BTCUSDT.P", "1m").expect("subscribed");
    assert_eq!(ws.status_writer_lanes(), 2, "the bars lane and its mark");
    ws.subscribe_bars("ETHUSDT", "1m").expect("subscribed");
    assert_eq!(ws.status_writer_lanes(), 3, "a spot symbol pairs nothing");
    ws.shutdown();
    assert_eq!(ws.status_writer_lanes(), 0);
}

/// The caps row answers first (B-21): a lane the venue does not serve is the table's refusal, and
/// no thread is spawned for it.
#[test]
fn a_lane_the_capabilities_row_refuses_spawns_nothing() {
    let mut ws = fresh();
    assert!(matches!(ws.subscribe_quotes("BTCUSDT"), Err(DataClientError::Unsupported(_))));
    assert!(matches!(ws.subscribe_book("BTCUSDT"), Err(DataClientError::Unsupported(_))));
    assert_eq!(ws.status_writer_lanes(), 0);
    market_feed_pins::refuses_quotes_and_book(fresh(), "BTCUSDT");
}

/// The spec's bar refusal (B-22) answers on the caller's thread, in its own words, before any spawn.
#[test]
fn a_bar_interval_the_spec_refuses_spawns_nothing() {
    let mut ws = fresh();
    let refused = ws.subscribe_bars("BTCUSDT", "7m");
    assert!(
        matches!(&refused, Err(DataClientError::Subscribe(why)) if why == "no 7m series"),
        "{refused:?}"
    );
    assert_eq!(ws.status_writer_lanes(), 0);
}

/// The thread rule (B-20): every lane thread carries the one name, which fits the 15 bytes Linux keeps.
#[test]
fn every_lane_thread_is_named_by_the_thread_rule() {
    assert!(DATA_CLIENT_THREAD.len() <= 15, "{DATA_CLIENT_THREAD}");
    let mut ws = fresh();
    let names = Arc::clone(&ws.spec.names);
    ws.subscribe_bars("BTCUSDT.P", "1m").expect("subscribed");
    ws.subscribe_trades("BTCUSDT").expect("subscribed");
    wait_until("three lanes", || names.lock().unwrap().len() == 3);
    assert!(names.lock().unwrap().iter().all(|n| n == DATA_CLIENT_THREAD), "{names:?}");
    ws.shutdown();
}

/// `begin_shutdown` raises every stop and joins nothing (B-20): the lanes end on their own while
/// the registry still holds them, and `shutdown` then joins.
#[test]
fn begin_shutdown_raises_every_stop_and_joins_nothing() {
    let mut ws = fresh();
    let ended = Arc::clone(&ws.spec.ended);
    ws.subscribe_bars("BTCUSDT", "1m").expect("subscribed");
    ws.subscribe_depth("ETHUSDT").expect("subscribed");
    ws.begin_shutdown();
    wait_until("both lanes to end", || ended.load(Ordering::SeqCst) == 2);
    assert_eq!(ws.status_writer_lanes(), 2, "nothing was joined yet");
    ws.shutdown();
    assert_eq!(ws.status_writer_lanes(), 0);
}

/// The spec's own teardown follows the lanes': its `begin_shutdown` runs once every lane's stop is
/// raised and joins nothing, its `shutdown` once every lane is joined (so what it winds down, ig's
/// link, outlives none of them).
#[test]
fn the_specs_teardown_hooks_follow_the_lanes() {
    let mut ws = fresh();
    let teardown = Arc::clone(&ws.spec.teardown);
    ws.subscribe_bars("BTCUSDT", "1m").expect("subscribed");
    ws.subscribe_depth("ETHUSDT").expect("subscribed");
    ws.begin_shutdown();
    assert_eq!(*teardown.lock().unwrap(), ["begin_shutdown"]);
    ws.shutdown();
    assert_eq!(*teardown.lock().unwrap(), ["begin_shutdown", "shutdown after 2 lanes ended"]);
}

/// A plan the venue refuses writes `"{key}: {why}"` and the lane ends; the client starts on its
/// connecting text.
#[test]
fn a_refused_plan_says_so_on_the_status_line() {
    let mut ws = fresh();
    assert_eq!(ws.status(), "connecting to Test…");
    ws.subscribe_bars("UNROUTED", "1m").expect("the spawn itself succeeds");
    wait_until("the refusal", || ws.status() == "UNROUTED@1m: no route");
    ws.shutdown();
}

#[test]
fn the_lane_key_names_the_interval_or_the_lane() {
    assert_eq!(DataClientLane::Bars.key("BTCUSDT", "1m"), "BTCUSDT@1m");
    assert_eq!(DataClientLane::Mark.key("BTCUSDT.P", "mark"), "BTCUSDT.P mark");
    assert_eq!(DataClientLane::Trades.key("BTCUSDT", "trades"), "BTCUSDT trades");
    assert_eq!(DataClientLane::Depth.key("BTCUSDT", "depth"), "BTCUSDT depth");
}

/// The healthy text is the prefix and the display name, which every reader classifies as connected.
#[test]
fn the_healthy_text_reads_as_connected() {
    use vike_model::feed_status::{DataClientConnectionState, parse_feed_status};
    let s = live_status("Binance");
    assert_eq!(s, "LIVE · Binance");
    assert_eq!(parse_feed_status(&s), DataClientConnectionState::Connected);
}

// --- a `Session` lane over a scripted socket -------------------------------------------------------

fn bar(ts: i64, close: f64) -> Bar {
    kline_to_bar(ts, close, close, close, close, 1.0)
}

/// A kline lane plan: `seed` once, then each frame word decodes to one event (`closed`, `forming`),
/// a fault (`reject`) or nothing.
fn kline_plan(seed: Option<Result<Vec<Bar>, String>>) -> DataClientLanePlan {
    DataClientLanePlan::Session {
        url: "wss://test.invalid/ws".into(),
        subscribe: None,
        keepalive: None,
        seed: seed.map(|s| Box::new(move || s) as Box<dyn FnOnce() -> Result<Vec<Bar>, String>>),
        decode: Box::new(|txt, emit| match txt {
            "closed" => {
                emit(DataClientEvent::Bar(BarKline { bar: bar(10, 5.0), closed: true }));
                WebsocketSessionFrameOutcome::Confirm
            }
            "forming" => {
                emit(DataClientEvent::Bar(BarKline { bar: bar(20, 6.0), closed: false }));
                WebsocketSessionFrameOutcome::Confirm
            }
            "reject" => {
                WebsocketSessionFrameOutcome::Fatal("subscribe rejected: bad symbol".into())
            }
            _ => WebsocketSessionFrameOutcome::Ignore,
        }),
    }
}

/// Run `plan` as the bars lane of `BTCUSDT@1m` over ONE scripted session of `frames`, until its
/// first `ws error`: the sink calls and the distinct status texts, in order.
fn run_one_session<S: DataClientSpec>(
    plan: DataClientLanePlan,
    frames: &[&str],
) -> (Vec<SinkCall>, Vec<String>) {
    let sink = Arc::new(RecordingSink::default());
    let lane = LaneStatus::halting_at("ws error");
    let ctx = DataClientContext {
        venue: S::VENUE,
        sink: sink.clone(),
        status: Arc::clone(&lane.status),
        wake: Arc::clone(&lane.wake),
        stop: Arc::clone(&lane.stop),
        own: (),
    };
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut session = one_session(frames, &sent);
    run_plan_on::<S, _>(plan, DataClientLane::Bars, "BTCUSDT", "1m", &ctx, |_url, opts| {
        session(opts)
    });
    assert!(sent.borrow().is_empty(), "a URL-subscribed lane sends nothing: {:?}", sent.borrow());
    let seen = lane.seen.lock().unwrap().clone();
    (sink.recorded(), seen)
}

/// The seed's newest bar is forming: the rest are seeded, it ticks the close cache and lands on the
/// forming lane, the healthy text follows; then each frame ticks the cache and lands on its lane, and
/// the session's end is the lane's `ws error`.
#[test]
fn a_bars_lane_seeds_streams_and_names_its_fault() {
    let seed = vec![bar(1, 1.0), bar(2, 2.0), bar(3, 3.0)];
    let (calls, seen) = run_one_session::<Spec>(kline_plan(Some(Ok(seed))), &["forming", "closed"]);
    let shape: Vec<String> = calls
        .iter()
        .map(|c| match c {
            SinkCall::SeedBars { venue, symbol, interval, bars } => {
                format!("seed {venue} {symbol} {interval} {}", bars.len())
            }
            SinkCall::BarCloseTick { price: px, timestamp: ts, .. } => format!("tick {px} {ts}"),
            SinkCall::FormingBar { bar, .. } => format!("forming {}", bar.timestamp),
            SinkCall::CloseBar { bar, .. } => format!("closed {}", bar.timestamp),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        shape,
        [
            "seed binance BTCUSDT 1m 2",
            "tick 3 3",
            "forming 3",
            "tick 6 20",
            "forming 20",
            "tick 5 10",
            "closed 10"
        ]
    );
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0], "LIVE · Test");
    assert!(seen[1].starts_with("BTCUSDT@1m ws error (reconnecting): "), "{seen:?}");
}

/// A failed seed is the lane's `seed error` and the stream starts anyway: its first confirmed frame
/// is the healthy text.
#[test]
fn a_failed_seed_is_named_and_the_stream_still_runs() {
    let (calls, seen) =
        run_one_session::<Spec>(kline_plan(Some(Err("HTTP 451".into()))), &["closed"]);
    assert_eq!(calls.len(), 2, "the closed bar's tick and close: {calls:?}");
    assert_eq!(seen[0], "BTCUSDT@1m seed error: HTTP 451");
    assert_eq!(seen[1], "LIVE · Test");
    assert!(seen[2].starts_with("BTCUSDT@1m ws error (reconnecting): "), "{seen:?}");
}

/// A venue whose seed holds closed bars only seeds them all and forms nothing.
#[test]
fn a_closed_only_seed_forms_nothing() {
    let seed = vec![bar(1, 1.0), bar(2, 2.0), bar(3, 3.0)];
    let (calls, _) = run_one_session::<ClosedSeedSpec>(kline_plan(Some(Ok(seed))), &[]);
    assert!(
        matches!(&calls[..], [SinkCall::SeedBars { bars, .. }] if bars.len() == 3),
        "{calls:?}"
    );
}

/// A frame the decoder calls fatal (a subscribe reject) ends the session in the venue's own words.
#[test]
fn a_fatal_frame_is_the_sessions_error_in_the_venues_words() {
    let (calls, seen) = run_one_session::<Spec>(kline_plan(None), &["reject"]);
    assert!(calls.is_empty(), "{calls:?}");
    assert_eq!(seen, ["BTCUSDT@1m ws error (reconnecting): subscribe rejected: bad symbol"]);
}

// --- a `SessionOn` lane: the venue's hooks on the session driver ----------------------------------

/// The venue's hooks dial, read each frame and speak for each session's start and end, in the driver's
/// order and with the lane's context; the plan's backoff replaces the row's (binance's fixed 3 s); and
/// the engine itself writes no status and no sink call.
#[test]
fn a_session_on_lane_runs_the_venues_hooks_and_writes_nothing_of_its_own() {
    let said = Rc::new(RefCell::new(Vec::<String>::new()));
    let note = |said: &Rc<RefCell<Vec<String>>>| {
        let said = Rc::clone(said);
        move |line: String| said.borrow_mut().push(line)
    };
    let (dialed, read, live, ended) = (note(&said), note(&said), note(&said), note(&said));
    let mut dials = 0;
    let plan = DataClientLanePlan::SessionOn {
        connect: Box::new(move |ctx: &DataClientContext| {
            dials += 1;
            dialed(format!("dial {dials} for {}", ctx.venue));
            if dials > 1 {
                ctx.stop.store(true, Ordering::SeqCst);
                return Err("the venue refuses a second session".into());
            }
            let stream =
                crate::scripted::WebsocketSessionMarketStreamScript::from_texts(["data", "noise"]);
            Ok(Box::new(stream) as Box<dyn WebsocketSessionMarketStream>)
        }),
        on_text: Box::new(move |_, txt| {
            read(format!("frame {txt}"));
            if txt == "data" {
                WebsocketSessionFrameOutcome::Confirm
            } else {
                WebsocketSessionFrameOutcome::Ignore
            }
        }),
        on_tick: Box::new(|_| {}),
        on_live: Box::new(move |_| live("live".into())),
        on_error: Box::new(move |_, e| ended(format!("ended: {e}"))),
        backoff: Some(WebsocketSessionBackoff::Fixed(Duration::from_millis(10))),
    };
    let sink = Arc::new(RecordingSink::default());
    let lane = LaneStatus::halting_at("never written");
    let ctx = DataClientContext {
        venue: Spec::VENUE,
        sink: sink.clone(),
        status: Arc::clone(&lane.status),
        wake: Arc::clone(&lane.wake),
        stop: Arc::clone(&lane.stop),
        own: (),
    };
    let started = Instant::now();
    run_plan::<Spec>(plan, DataClientLane::Quotes, "BTCUSDT", "quotes", &ctx);

    let said = said.borrow().clone();
    assert_eq!(said[..4], ["dial 1 for binance", "frame data", "live", "frame noise"], "{said:?}");
    assert!(said[4].starts_with("ended: "), "the script's end is the session's fault: {said:?}");
    assert_eq!(
        said[5..],
        ["dial 2 for binance", "ended: the venue refuses a second session"],
        "{said:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2), "the row's 3 s backoff ran");
    assert!(sink.recorded().is_empty(), "the engine wrote a sink call: {:?}", sink.recorded());
    assert!(lane.status.lock().unwrap().is_empty(), "the engine wrote the status line");
    assert!(lane.seen.lock().unwrap().is_empty(), "the engine woke the GUI");
}

// --- the one emission ----------------------------------------------------------------------------

fn emitting(sink: &Arc<RecordingSink>, wakes: &Arc<AtomicUsize>) -> DataClientContext {
    let wakes = Arc::clone(wakes);
    DataClientContext {
        venue: "binance",
        sink: sink.clone(),
        status: Arc::default(),
        wake: Arc::new(move || {
            wakes.fetch_add(1, Ordering::SeqCst);
        }),
        stop: Arc::default(),
        own: (),
    }
}

fn quote(ts: i64) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid: 1.0,
        ask: 2.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    }
}

fn trade_at(price: f64, size: f64) -> TickTrade {
    TickTrade {
        timestamp: 7,
        local_timestamp: 0,
        price,
        size,
        is_buyer_maker: false,
        symbol: String::new(),
    }
}

/// A quote carries its receive time, and a quote with no venue time takes it as its `ts` (B-21, the
/// A1 rule); one with a venue time keeps it.
#[test]
fn a_quote_with_no_venue_time_takes_its_receive_time() {
    let (sink, wakes) = (Arc::new(RecordingSink::default()), Arc::new(AtomicUsize::new(0)));
    let ctx = emitting(&sink, &wakes);
    emit(&ctx, "BTCUSDT", "quotes", DataClientEvent::Quote(quote(0)));
    emit(&ctx, "BTCUSDT", "quotes", DataClientEvent::Quote(quote(5)));
    let quotes = sink.quotes();
    let (timeless, timed) = (&quotes[0].2, &quotes[1].2);
    assert!(
        timeless.local_timestamp > 0 && timeless.timestamp == timeless.local_timestamp,
        "{timeless:?}"
    );
    assert!(timed.local_timestamp > 0 && timed.timestamp == 5, "{timed:?}");
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
}

/// A print the one trade guard refuses never reaches the sink (and wakes nothing); a good one carries
/// its receive time.
#[test]
fn a_print_the_guard_refuses_never_reaches_the_sink() {
    let (sink, wakes) = (Arc::new(RecordingSink::default()), Arc::new(AtomicUsize::new(0)));
    let ctx = emitting(&sink, &wakes);
    emit(&ctx, "BTCUSDT", "trades", DataClientEvent::Trade(trade_at(0.0, 1.0)));
    assert!(sink.trades().is_empty());
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
    let frame = vec![trade_at(10.0, 1.0), trade_at(f64::NAN, 1.0), trade_at(11.0, -1.0)];
    emit(&ctx, "BTCUSDT", "trades", DataClientEvent::Trades(frame));
    let trades = sink.trades();
    assert_eq!(trades.len(), 1, "{trades:?}");
    assert!(trades[0].2.price == 10.0 && trades[0].2.local_timestamp > 0, "{trades:?}");
    assert_eq!(wakes.load(Ordering::SeqCst), 1, "one wake per frame");
}

/// A mark lands on the mark verb and its funding on the funding verb (which wakes nothing alone);
/// the book's own vocabulary reaches nothing.
#[test]
fn a_mark_and_its_funding_reach_their_verbs_and_the_book_words_reach_nothing() {
    let (sink, wakes) = (Arc::new(RecordingSink::default()), Arc::new(AtomicUsize::new(0)));
    let ctx = emitting(&sink, &wakes);
    let funding = FundingRate {
        timestamp: 1,
        symbol: "BTCUSDT.P".into(),
        rate: 0.0001,
        interval_ms: None,
        next_timestamp: Some(2),
        predicted: None,
        price: None,
    };
    emit(&ctx, "BTCUSDT.P", "mark", DataClientEvent::Mark(DataClientMark { px: 100.0, ts: 1 }));
    emit(&ctx, "BTCUSDT.P", "mark", DataClientEvent::Funding(funding.clone()));
    for word in [
        DataClientEvent::BookUpdated { timestamp_ms: 1 },
        DataClientEvent::Resync,
        DataClientEvent::Ignored,
    ] {
        emit(&ctx, "BTCUSDT.P", "mark", word);
    }
    let calls = sink.recorded();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        matches!(&calls[0], SinkCall::MarkTick { price: px, timestamp: 1, .. } if *px == 100.0),
        "{calls:?}"
    );
    assert_eq!(sink.fundings(), [("binance".to_string(), "BTCUSDT.P".to_string(), funding)]);
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "the mark wakes; funding and the book words do not"
    );
}

/// K1: an inferred close lands on the lossless lane ALONE (no bar-close tick, no wake); the successor's
/// forming bar, from the same frame, ticks the cache and wakes once.
#[test]
fn an_inferred_close_lands_alone_and_its_successor_ticks_and_wakes() {
    let (sink, wakes) = (Arc::new(RecordingSink::default()), Arc::new(AtomicUsize::new(0)));
    let ctx = emitting(&sink, &wakes);
    emit(&ctx, "BTC", "1m", DataClientEvent::InferredClose(bar(60, 5.0)));
    assert_eq!(sink.calls(), ["close_bar(binance,BTC,1m,5)"]);
    assert_eq!(wakes.load(Ordering::SeqCst), 0, "the close rides its successor's push");
    emit(&ctx, "BTC", "1m", DataClientEvent::Bar(BarKline { bar: bar(120, 6.0), closed: false }));
    assert_eq!(
        sink.calls(),
        [
            "close_bar(binance,BTC,1m,5)",
            "bar_close_tick(binance,BTC,6,120)",
            "forming_bar(binance,BTC,1m,6)"
        ]
    );
    assert_eq!(sink.closed_bars()[0].3, bar(60, 5.0), "the bar verbatim");
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
}

/// K3: a self-folded book lands whole on the lossless `book` lane, and wakes. Two book events are equal
/// when they hand over the same `Arc`.
#[test]
fn a_folded_book_lands_whole_on_the_book_lane() {
    let (sink, wakes) = (Arc::new(RecordingSink::default()), Arc::new(AtomicUsize::new(0)));
    let ctx = emitting(&sink, &wakes);
    let mut folded = L2Book::new(0.5);
    folded.apply_snapshot(7, &[L2BookLevel::new(9.5, 1.0)], &[L2BookLevel::new(10.0, 2.0)]);
    let book = Arc::new(folded);
    emit(&ctx, "BTC", "book", DataClientEvent::Book(DataClientBook(Arc::clone(&book))));
    assert_eq!(sink.calls(), ["book:binance:BTC:Some(9.75)"]);
    assert_eq!(sink.books()[0].2.last_sequence, 7);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert_eq!(DataClientBook(Arc::clone(&book)), DataClientBook(Arc::clone(&book)));
    assert_ne!(DataClientBook(book), DataClientBook(Arc::new(L2Book::new(0.5))));
}

/// K2: a full-depth snapshot lands on the DOM's `l2_snapshot` verbatim (grid, levels, the venue's
/// time; no receive-time stamp), and wakes.
#[test]
fn a_full_depth_snapshot_lands_verbatim_on_the_dom_lane() {
    let (sink, wakes) = (Arc::new(RecordingSink::default()), Arc::new(AtomicUsize::new(0)));
    let ctx = emitting(&sink, &wakes);
    let bids = vec![L2BookLevel::new(9.5, 1.0), L2BookLevel::new(9.0, 2.0)];
    let asks = vec![L2BookLevel::new(10.0, 3.0)];
    let snapshot = DataClientEvent::DepthSnapshot {
        tick_size: 0.5,
        bids: bids.clone(),
        asks: asks.clone(),
        ts: 1_700_000_000_000,
    };
    emit(&ctx, "BTC", "depth", snapshot);
    assert_eq!(
        sink.l2_snapshots(),
        [("binance".to_string(), "BTC".to_string(), 0.5, bids, asks, 1_700_000_000_000)]
    );
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
}
