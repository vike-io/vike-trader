//! The lane's fold, driven offline through [`Book`]: routing by conId, the per-lane delivery, bars
//! aggregated from prints (and from the mid for a `CASH` pair), the delay label, and the gap
//! bookkeeping. No socket, no REST. Then the lane on the shared market pump ([`run_on`]) over
//! scripted gateway sessions on a virtual clock (`vike_bridge_core::pump_clock`'s `VirtualClock`):
//! the waits, the tickle, the idle limit and the commands, at the rows' real durations.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::json;
use vike_bridge_core::data_client::context::DataClientContext;
use vike_bridge_core::pump_clock::VirtualClock;
use vike_bridge_core::scripted::{
    UserScriptHeard, UserScriptLog, UserScriptStep, WebsocketSessionUserStreamScript,
};
use vike_bridge_core::websocket_session::spec;
use vike_bridge_core::websocket_session::user::WebsocketSessionUserMessage;
use vike_data::{DataClientSink, DataClientSubscriptionId};
use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
use vike_model::{StreamDelay, StreamStatus};
use vike_venues::venue_timings;

use super::{Book, FeedCpapiCommand, FeedLane, Lane, TICKLE_EVERY, run_on};
use crate::feed::VENUE;
use crate::feed::bar::BarAggregator;
use crate::feed::cpapi::decode::{smd_subscribe, smd_unsubscribe};
use crate::feed::test_support::{Call, RecSink};
use crate::transport::CpapiRest;

fn bars(interval_ms: i64, from_mid: bool) -> FeedLane {
    FeedLane::Bars {
        interval: "1m".into(),
        interval_ms,
        agg: BarAggregator::new(interval_ms),
        from_mid,
    }
}

fn book(sink: &Arc<RecSink>) -> Book {
    Book::new(Arc::clone(sink) as Arc<dyn vike_data::DataClientSink>)
}

/// A frame reaches every stream on ITS conId, each lane takes what it serves, and a frame for a
/// conId nobody holds (or on another topic) is ignored.
#[test]
fn frames_route_by_conid_and_lane() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    assert!(b.add(DataClientSubscriptionId(0), 265598, "AAPL.SMART.USD".into(), FeedLane::Quotes));
    assert!(!b.add(DataClientSubscriptionId(1), 265598, "AAPL.SMART.USD".into(), FeedLane::Trades));

    b.on_frame(&json!({ "topic": "smd+265598", "_updated": 1_000, "6509": "DPB",
                        "84": "190.10", "86": "190.12", "31": "190.11", "7059": "100" }));
    b.on_frame(&json!({ "topic": "smd+999", "84": "1.0", "86": "2.0", "31": "1.5" }));
    b.on_frame(&json!({ "topic": "sor", "args": [] }));

    let calls = sink.take();
    let data: Vec<_> = calls.iter().filter(|c| !matches!(c, Call::Delay(..))).collect();
    assert_eq!(data.len(), 2, "one quote + one print, nothing for the other conId: {calls:?}");
    assert!(matches!(data[0], Call::Quote(s, q) if s == "AAPL.SMART.USD" && q.bid == 190.10));
    assert!(matches!(data[1], Call::Trade(_, t) if t.price == 190.11 && t.size == 100.0));
}

/// A stock bar stream folds PRINTS through the shared aggregator; a bucket boundary closes the
/// prior bar, every print updates the forming one and the bar-close slot.
#[test]
fn bars_aggregate_prints_and_close_on_the_boundary() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 1, "AAPL.SMART.USD".into(), bars(60_000, false));
    for (ts, px) in [(60_000, 10.0), (61_000, 12.0), (62_000, 9.0), (120_000, 11.0)] {
        b.on_frame(&json!({ "topic": "smd+1", "_updated": ts, "31": px, "7059": 1 }));
    }
    let calls = sink.take();
    let closed: Vec<_> = calls
        .iter()
        .filter_map(|c| if let Call::Close(_, bar) = c { Some(bar) } else { None })
        .collect();
    assert_eq!(closed.len(), 1, "one boundary crossed: {calls:?}");
    let c = closed[0];
    assert_eq!(
        (c.timestamp, c.open, c.high, c.low, c.close, c.volume),
        (60_000, 10.0, 12.0, 9.0, 9.0, 3.0)
    );
    let forming = calls.iter().filter(|c| matches!(c, Call::Forming(..))).count();
    assert_eq!(forming, 4, "every print refreshes the forming bar");
    assert!(calls.contains(&Call::BarCloseTick(11.0)));
}

/// A `CASH` pair prints no last price, so its bars fold the quote MID — and a print would not move
/// them.
#[test]
fn a_cash_bar_stream_aggregates_the_mid() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 2, "EUR.USD.IDEALPRO".into(), bars(60_000, true));
    b.on_frame(&json!({ "topic": "smd+2", "_updated": 60_000, "84": "1.1000", "86": "1.1002" }));
    let calls = sink.take();
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, Call::Forming(_, bar) if (bar.close - 1.1001).abs() < 1e-12)),
        "the forming bar is the mid: {calls:?}"
    );
}

/// The `data_delay` calls alone, in order.
fn delays(calls: &[Call]) -> Vec<(String, StreamDelay)> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Delay(_, stream, d) => Some((stream.clone(), *d)),
            _ => None,
        })
        .collect()
}

/// A delayed cpapi row labels the quotes stream `StreamDelay::Delayed` ONCE — at first knowledge,
/// before the quote it rides on — and later rows (with or without the marker) send no label.
#[test]
fn a_delayed_row_is_labelled_delayed_once() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 3, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+3", "6509": "DPB", "84": 1.0, "86": 2.0 }));
    b.on_frame(&json!({ "topic": "smd+3", "84": 1.5 }));
    b.on_frame(&json!({ "topic": "smd+3", "6509": "DPB", "84": 1.6 }));
    let calls = sink.take();
    assert_eq!(delays(&calls), vec![("quotes".to_string(), StreamDelay::DELAYED)], "{calls:?}");
    assert!(matches!(calls[0], Call::Delay(..)), "the label precedes the data: {calls:?}");
    assert_eq!(calls.iter().filter(|c| matches!(c, Call::Quote(..))).count(), 3);
}

/// A real-time row labels `RealTime`; a row before any marker sends no label at all.
#[test]
fn a_real_time_row_is_labelled_real_time() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 4, "EUR.USD.IDEALPRO".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+4", "84": 1.1, "86": 1.2 }));
    assert!(delays(&sink.take()).is_empty(), "no marker yet, no label");
    b.on_frame(&json!({ "topic": "smd+4", "6509": "RpB", "84": 1.11 }));
    assert_eq!(delays(&sink.take()), vec![("quotes".to_string(), StreamDelay::RealTime)]);
}

/// **The delay rule's terms** (`DataClientSinkRouting::data_delay`: only a `RealTime` label under the word `quotes` lets the
/// recorder back in, and a re-subscribe does not): on one client, each symbol says its own label under that word, and a
/// stream new on a conId that already streams says the label in force before its first quote. A delayed stream that
/// turns real time says so, before the first real-time quote.
#[test]
fn each_stream_says_its_own_label_under_the_word_quotes_and_says_when_it_turns_real_time() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 10, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    b.add(DataClientSubscriptionId(1), 11, "EUR.USD.IDEALPRO".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+10", "6509": "D", "84": 1.0, "86": 2.0 }));
    b.on_frame(&json!({ "topic": "smd+11", "6509": "R", "84": 1.1, "86": 1.2 }));
    let calls = sink.take();
    assert!(calls.contains(&Call::Delay(
        "AAPL.SMART.USD".into(),
        "quotes".into(),
        StreamDelay::DELAYED
    )));
    assert!(calls.contains(&Call::Delay(
        "EUR.USD.IDEALPRO".into(),
        "quotes".into(),
        StreamDelay::RealTime
    )));

    assert_eq!(b.remove(DataClientSubscriptionId(0)), Some(10));
    b.add(DataClientSubscriptionId(2), 11, "EUR.USD.IDEALPRO.CASH".into(), FeedLane::Quotes);
    let calls = sink.take();
    assert!(
        matches!(
            calls.as_slice(),
            [Call::Delay(s, lane, StreamDelay::RealTime), Call::Quote(q, _)]
                if s == "EUR.USD.IDEALPRO.CASH" && lane == "quotes" && q == s
        ),
        "{calls:?}"
    );

    b.add(DataClientSubscriptionId(3), 12, "MSFT.SMART.USD".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+12", "6509": "D", "84": 1.0, "86": 2.0 }));
    sink.take();
    b.on_frame(&json!({ "topic": "smd+12", "6509": "R", "84": 1.5 }));
    let calls = sink.take();
    assert!(
        matches!(calls.as_slice(), [Call::Delay(_, _, StreamDelay::RealTime), Call::Quote(_, q)] if q.bid == 1.5),
        "the label first, or the recorder would stay withheld for this quote: {calls:?}"
    );
}

/// **A quote with no label in force is not delivered** (decision 0193 verdict 7): the recorder stores what is not
/// labelled delayed or frozen, so a row with no marker must not reach it as real time. The fold keeps the row's fields,
/// so the first row WITH a marker delivers its label and then the whole quote, and a row without a marker keeps the
/// label in force. A marker the decoder does not read (`N`, not subscribed) ends it, also after a real-time one.
#[test]
fn a_quote_with_no_label_in_force_is_not_delivered() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 8, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+8", "84": 1.0, "86": 2.0 }));
    b.on_frame(&json!({ "topic": "smd+8", "6509": "N", "84": 1.1 }));
    assert!(sink.take().is_empty(), "no label yet: neither a label nor a quote");

    b.on_frame(&json!({ "topic": "smd+8", "6509": "DPB", "84": 1.2 }));
    let calls = sink.take();
    assert!(
        matches!(
            calls.as_slice(),
            [Call::Delay(_, _, StreamDelay::Delayed { .. }), Call::Quote(_, q)]
                if q.bid == 1.2 && q.ask == 2.0
        ),
        "the label, then the quote the row completes: {calls:?}"
    );

    b.on_frame(&json!({ "topic": "smd+8", "84": 1.3 }));
    assert!(matches!(sink.take().as_slice(), [Call::Quote(_, q)] if q.bid == 1.3));

    b.on_frame(&json!({ "topic": "smd+8", "6509": "R", "84": 1.4 }));
    sink.take();
    b.on_frame(&json!({ "topic": "smd+8", "6509": "N", "84": 1.5 }));
    assert!(sink.take().is_empty(), "not subscribed: not real time on the last row's word");
    b.on_frame(&json!({ "topic": "smd+8", "6509": "R", "84": 1.6 }));
    assert!(
        matches!(sink.take().as_slice(), [Call::Quote(_, q)] if q.bid == 1.6),
        "the label did not change, so none is said again"
    );
}

/// A change of marker is reported ONCE, per stream that carries it: delayed → frozen (`Y`) on the
/// quotes and trades lanes, then repeats of the new marker send nothing; `N` and unknown letters
/// send no label and keep the last one.
#[test]
fn a_change_is_reported_once() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 5, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    b.add(DataClientSubscriptionId(1), 5, "AAPL.SMART.USD".into(), FeedLane::Trades);
    b.on_frame(&json!({ "topic": "smd+5", "6509": "D", "84": 1.0, "86": 2.0, "31": 1.5 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "Y", "84": 1.1, "31": 1.6 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "Y", "84": 1.2, "31": 1.7 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "N", "84": 1.3, "31": 1.8 }));
    b.on_frame(&json!({ "topic": "smd+5", "6509": "?", "84": 1.4, "31": 1.9 }));
    assert_eq!(
        delays(&sink.take()),
        vec![
            ("quotes".to_string(), StreamDelay::DELAYED),
            ("trades".to_string(), StreamDelay::DELAYED),
            ("quotes".to_string(), StreamDelay::Frozen),
            ("trades".to_string(), StreamDelay::Frozen),
        ]
    );
}

/// A lane that gets no data from a row gets no label from it: a quote-only row labels the quotes
/// stream, not the trades stream beside it.
#[test]
fn a_lane_without_data_in_the_row_is_not_labelled() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 6, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    b.add(DataClientSubscriptionId(1), 6, "AAPL.SMART.USD".into(), FeedLane::Trades);
    b.on_frame(&json!({ "topic": "smd+6", "6509": "D", "84": 1.0, "86": 2.0 }));
    assert_eq!(delays(&sink.take()), vec![("quotes".to_string(), StreamDelay::DELAYED)]);
}

/// A quote stream joining a conId that already holds a quote is handed it at once, without
/// waiting for the next row, its label first. A held quote with no label stays held.
#[test]
fn a_second_quote_stream_gets_the_held_quote() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 4, "X.SMART.USD".into(), FeedLane::Trades);
    b.on_frame(&json!({ "topic": "smd+4", "6509": "D", "84": 5.0, "86": 6.0 }));
    assert!(sink.take().is_empty(), "a trade lane takes no quote");
    b.add(DataClientSubscriptionId(1), 4, "X.SMART.USD".into(), FeedLane::Quotes);
    let calls = sink.take();
    assert!(matches!(
        calls.as_slice(),
        [Call::Delay(..), Call::Quote(_, q)] if q.bid == 5.0 && q.ask == 6.0
    ));

    b.add(DataClientSubscriptionId(2), 7, "Y.SMART.USD".into(), FeedLane::Trades);
    b.on_frame(&json!({ "topic": "smd+7", "84": 5.0, "86": 6.0 }));
    b.add(DataClientSubscriptionId(3), 7, "Y.SMART.USD".into(), FeedLane::Quotes);
    assert!(sink.take().is_empty(), "no label yet: not handed over");
}

/// The held quote a late subscriber is handed keeps the VENUE time it was first delivered with
/// (`_updated` of the row that made it), so its age reads true; the hand-over time is its
/// `local_ts`. It used to be stamped with the hand-over time, so a quote quiet for minutes read as
/// fresh.
#[test]
fn a_held_quote_keeps_its_venue_time_for_a_late_subscriber() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 8, "Z.SMART.USD".into(), FeedLane::Quotes);
    b.on_frame(&json!({ "topic": "smd+8", "_updated": 1_000, "6509": "R", "84": 5.0, "86": 6.0 }));
    // A later row that changes no quote field leaves the quote's time where it was.
    b.on_frame(&json!({ "topic": "smd+8", "_updated": 2_000, "7059": 3 }));
    let first = sink.take();
    assert!(
        matches!(first.as_slice(), [Call::Delay(..), Call::Quote(_, q)] if q.timestamp == 1_000)
    );

    b.add(DataClientSubscriptionId(1), 8, "Z.SMART.USD".into(), FeedLane::Quotes);
    let calls = sink.take();
    let [Call::Delay(..), Call::Quote(_, q)] = calls.as_slice() else {
        panic!("the held quote, its label first: {calls:?}")
    };
    assert_eq!(q.timestamp, 1_000, "the venue time the quote first had");
    assert!(
        q.local_timestamp > 1_000,
        "the receipt time is the hand-over's: {}",
        q.local_timestamp
    );
}

/// Removing streams: the conId is reported for unsubscription only with its LAST stream, and an
/// unknown id is a no-op.
#[test]
fn remove_reports_the_conid_with_its_last_stream() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 5, "A.SMART.USD".into(), FeedLane::Quotes);
    b.add(DataClientSubscriptionId(1), 5, "A.SMART.USD".into(), FeedLane::Trades);
    assert_eq!(b.remove(DataClientSubscriptionId(9)), None);
    assert_eq!(b.remove(DataClientSubscriptionId(0)), None);
    assert_eq!(b.len(), 1);
    assert_eq!(b.remove(DataClientSubscriptionId(1)), Some(5));
    assert_eq!(b.len(), 0);
}

/// A dropped socket opens every stream's gap ONCE, however many redials fail.
#[test]
fn a_gap_opens_once_per_drop() {
    let sink = RecSink::arc();
    let mut b = book(&sink);
    b.add(DataClientSubscriptionId(0), 6, "A.SMART.USD".into(), FeedLane::Quotes);
    b.open_gaps(100);
    b.open_gaps(200);
    let calls = sink.take();
    assert_eq!(
        calls,
        vec![Call::Status(
            "A.SMART.USD".into(),
            "quotes".into(),
            StreamStatus::GapStart { at_timestamp_ms: 100 }
        )]
    );
}

/// The tickle and the waits this lane runs are the market rows': a 30 s REST renewal, a FIXED 2 s
/// wait after every fault (the pump row reads the timing row's `initial`, the whole policy only while
/// that row is fixed), the 60 s idle limit, no socket keepalive and the 200 ms command poll.
#[test]
fn the_market_timings_are_the_rows_tickle_and_fixed_redial() {
    let s = Duration::from_secs;
    assert_eq!(TICKLE_EVERY, s(30));
    assert!(venue_timings::IBKR_CPAPI.market().reconnect().is_fixed());
    let row = spec::IBKR_CPAPI;
    assert_eq!((row.backoff.initial, row.backoff.max), (s(2), s(2)), "a fixed 2 s wait");
    assert_eq!(row.idle_threshold, Some(s(60)));
    assert_eq!(row.keepalive_every, None, "the tickle is a REST renewal, no frame on the socket");
    assert_eq!(row.read_timeout, Duration::from_millis(200));
}

// ---------------------------------------------------------------------------------------------
// The lane on the shared market pump, over scripted gateway sessions on a virtual clock.
// ---------------------------------------------------------------------------------------------

/// The conId every driven run holds one quotes stream of.
const HELD: i64 = 265_598;

/// One open of a scripted gateway: a session that plays its steps, or a dial that fails.
enum Open {
    Session(Vec<UserScriptStep>),
    Fails,
}

/// The gateway's `system` heartbeat, which IBKR documents every 10 s on a connected socket.
fn heartbeat() -> UserScriptStep {
    let frame = json!({ "topic": "system", "hb": 1_700_000_000_000_i64 });
    UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Text(frame.to_string())))
}

/// A quiet session of `beats` heartbeats 10 s apart, each followed by its 10 s of silence.
fn quiet(beats: usize) -> Vec<UserScriptStep> {
    (0..beats).flat_map(|_| [heartbeat(), UserScriptStep::Stall(Duration::from_secs(10))]).collect()
}

/// What one run saw, each time on the virtual clock from its birth.
struct Run {
    /// When each dial was asked for.
    opens: Vec<Duration>,
    /// Each text frame the lane sent on a session.
    sent: Vec<(Duration, String)>,
    /// Each REST tickle.
    tickles: Vec<Duration>,
    /// The held stream's statuses, in order.
    statuses: Vec<StreamStatus>,
    /// The lane's status line at the end.
    status: DataClientStatus,
}

/// The lane over `opens`, holding one quotes stream of [`HELD`]; the last session raises the stop once
/// it is drained. The REST lane is the gateway's closed loopback port, so the one first-quote
/// snapshot fails at once.
fn drive(opens: Vec<Open>) -> Run {
    let clock = VirtualClock::new(1);
    let _seat = clock.enter();
    let since_birth = || clock.offset(clock.now());
    let sink = RecSink::arc();
    let stop = Arc::new(AtomicBool::new(false));
    let ctx = DataClientContext {
        venue: VENUE,
        sink: Arc::clone(&sink) as Arc<dyn DataClientSink>,
        status: Arc::default(),
        wake: Arc::new(|| {}),
        stop: Arc::clone(&stop),
        own: (),
    };
    let mut held = book(&sink);
    held.add(DataClientSubscriptionId(0), HELD, "AAPL.SMART.USD".into(), FeedLane::Quotes);
    let (_commands, rx) = mpsc::channel::<FeedCpapiCommand>();
    let rest = CpapiRest::new("https://127.0.0.1:1", "DU0000000");
    let log = UserScriptLog::default();
    let tickles = RefCell::new(Vec::new());
    let last = opens.len() - 1;
    let mut script = opens.into_iter().enumerate();
    let mut asked = Vec::new();
    run_on(
        || {
            asked.push(since_birth());
            match script.next().expect("the script names the last open") {
                (i, Open::Session(steps)) => {
                    let session = WebsocketSessionUserStreamScript::from_steps(steps)
                        .logged(Arc::clone(&log));
                    Ok(if i == last { session.stopping(Arc::clone(&stop)) } else { session })
                }
                (_, Open::Fails) => Err("refused".to_string()),
            }
        },
        &rest,
        held,
        &rx,
        &ctx,
        &|| tickles.borrow_mut().push(since_birth()),
    );
    let sent = log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(at, heard)| match heard {
            UserScriptHeard::Text(t) => Some((clock.offset(*at), t.clone())),
            UserScriptHeard::Pong => None,
        })
        .collect();
    let statuses = sink
        .take()
        .into_iter()
        .filter_map(|call| match call {
            Call::Status(_, _, status) => Some(status),
            _ => None,
        })
        .collect();
    let status = ctx.status.lock().unwrap().clone();
    Run { opens: asked, sent, tickles: tickles.into_inner(), statuses, status }
}

/// **The old loop's waits and cadence, verbatim** (pinned before the lane left its own loop): the
/// market row's fixed 2 s after a dropped session and after a failed dial alike, the REST tickle every
/// 30 s of the lane's life while a session is open (its clock not reset by a redial), and on the
/// socket only the held conId's `smd` subscribe, sent again at every open: no keepalive frame. The
/// drop opens the stream's gap once and the next session's prime closes it.
#[test]
fn the_waits_and_the_tickle_are_the_old_loops_two_seconds_after_every_fault_and_thirty() {
    let s = Duration::from_secs;
    let drops = vec![
        heartbeat(),
        UserScriptStep::Stall(s(40)),
        UserScriptStep::Close { code: 1000, reason: "bye".into() },
    ];
    let run = drive(vec![Open::Session(drops), Open::Fails, Open::Session(quiet(3))]);
    assert_eq!(run.opens, [s(0), s(42), s(44)]);
    assert_eq!(run.tickles, [s(30), s(60)]);
    let subscribe = smd_subscribe(HELD);
    assert_eq!(run.sent, [(s(0), subscribe.clone()), (s(44), subscribe)]);
    assert!(
        matches!(
            run.statuses.as_slice(),
            [StreamStatus::GapStart { at_timestamp_ms }, StreamStatus::Live { gap_started_timestamp_ms: Some(gap) }]
                if gap == at_timestamp_ms
        ),
        "{:?}",
        run.statuses
    );
    assert_eq!(run.status.state, DataClientConnectionState::Connected, "{:?}", run.status);
}

/// **The idle limit** (new with the pump): a socket that stays open and says nothing is called dead
/// 60 s after its last frame, its stream's gap opens, the status says why, and the next dial comes
/// the row's 2 s later.
#[test]
fn a_silent_session_ends_at_the_idle_limit_and_the_next_dial_comes_two_seconds_later() {
    let s = Duration::from_secs;
    let run = drive(vec![
        Open::Session(vec![heartbeat(), UserScriptStep::Stall(s(120))]),
        Open::Session(vec![]),
    ]);
    assert_eq!(run.opens, [s(0), s(62)]);
    assert!(matches!(
        run.statuses.as_slice(),
        [StreamStatus::GapStart { .. }, StreamStatus::Live { .. }]
    ));
    assert_eq!(run.status.state, DataClientConnectionState::Error);
    assert!(run.status.text.contains("no frames within 60s (silent stall)"), "{}", run.status.text);
}

/// The gateway's 10 s heartbeat keeps a quiet session up past the idle limit: two minutes, one dial,
/// no gap, and the tickle on its 30 s cadence throughout.
#[test]
fn the_ten_second_heartbeat_keeps_a_quiet_session_up_past_the_idle_limit() {
    let s = Duration::from_secs;
    let run = drive(vec![Open::Session(quiet(12))]);
    assert_eq!(run.opens, [s(0)]);
    assert_eq!(run.tickles, [s(30), s(60), s(90), s(120)]);
    assert!(run.statuses.is_empty(), "no gap: {:?}", run.statuses);
    assert_eq!(run.status.state, DataClientConnectionState::Connected);
}

/// On a live session a new conId is subscribed once (a second stream on it sends nothing) and
/// unsubscribed only with its last stream; every `Remove` is answered; a closed command channel is
/// the backend gone.
#[test]
fn commands_on_a_live_session_subscribe_a_new_conid_once_and_unsubscribe_it_with_its_last_stream() {
    let sink = RecSink::arc();
    let (tx, rx) = mpsc::channel();
    let rest = CpapiRest::new("https://127.0.0.1:1", "DU0000000");
    let stop = AtomicBool::new(false);
    let lane = Lane {
        book: RefCell::new(book(&sink)),
        rest: &rest,
        commands: &rx,
        stop: &stop,
        tickle: &|| {},
        last_tickle: Cell::new(Instant::now()),
    };
    let log = UserScriptLog::default();
    let mut socket = WebsocketSessionUserStreamScript::from_steps(vec![]).logged(Arc::clone(&log));
    let (done, answered) = mpsc::channel();
    let symbol = || "A.SMART.USD".to_string();
    let id = DataClientSubscriptionId;
    for (n, feed) in [(0, FeedLane::Trades), (1, FeedLane::Quotes)] {
        tx.send(FeedCpapiCommand::Add { id: id(n), conid: 5, symbol: symbol(), lane: feed })
            .unwrap();
    }
    for n in [0, 1] {
        tx.send(FeedCpapiCommand::Remove { id: id(n), done: done.clone() }).unwrap();
    }
    assert!(!lane.apply_commands(Some(&mut socket)), "the backend is there");
    let sent: Vec<String> = log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, heard)| match heard {
            UserScriptHeard::Text(t) => Some(t.clone()),
            UserScriptHeard::Pong => None,
        })
        .collect();
    assert_eq!(sent, [smd_subscribe(5), smd_unsubscribe(5)]);
    assert_eq!(answered.try_iter().count(), 2, "both removes answered");
    drop(tx);
    assert!(lane.apply_commands(None), "a closed channel is the backend gone");
}
