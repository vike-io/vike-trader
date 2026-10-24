//! The session guard: one live session per guarded venue, a stream ceiling, a spacing of opens, and a
//! release that never holds up a subscribe.

use std::sync::Mutex;

use vike_data::{RecordingSink, SinkCall};

use super::*;
use crate::testkit::data::{DataCall, ScriptedData, SubVerb, SubscribeCall};

/// The spacing of the guarded venue in these tests. Wide, so the margins below are scheduling jitter on a
/// loaded runner and not the test's flake: every bound is a fraction of it.
const SPACING: Duration = Duration::from_millis(150);

/// OANDA with three streams and a short spacing; every other venue unguarded.
fn short(venue: &str) -> Option<StreamLimits> {
    (venue == "oanda").then_some(StreamLimits { max_streams: 3, min_open_spacing: SPACING })
}

/// OANDA with room for a burst and a spacing too small to matter; every other venue unguarded.
fn wide(venue: &str) -> Option<StreamLimits> {
    (venue == "oanda")
        .then_some(StreamLimits { max_streams: 16, min_open_spacing: Duration::from_millis(1) })
}

/// One session of `venue`: the guarded client and the sink the server would have given it.
fn open(
    guard: &SessionGuard,
    data: &ScriptedData,
    venue: &str,
    session: u64,
) -> (Box<dyn DataClient + Send>, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let client = data.build(venue, None, sink.clone()).expect("the scripted lane builds");
    (guard.admit(venue, session, sink.clone(), Box::new(client)), sink)
}

fn gaps(sink: &RecordingSink) -> Vec<String> {
    sink.recorded()
        .into_iter()
        .filter_map(|call| match call {
            SinkCall::StreamStatus {
                symbol,
                stream,
                status: StreamStatus::GapStart { .. },
                ..
            } => Some(format!("{symbol}/{stream}")),
            _ => None,
        })
        .collect()
}

/// Poll `cond` for up to five seconds (the wind-down of a replaced session runs on a thread of its own).
fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **A second `cs_venue=oanda` session replaces the first** (acceptance A8): the older session's streams
/// are told to stop, it discloses a gap on every key it held, it refuses every later subscribe naming the
/// newer session, and the newer one works.
#[test]
fn a_second_session_replaces_the_first_and_the_first_refuses_in_words() {
    let guard = SessionGuard::new(short);
    let data = ScriptedData::new(&[]);
    let (mut first, first_sink) = open(&guard, &data, "oanda", 1);
    first.subscribe_quotes("EURUSD").expect("the first session streams");
    first.subscribe_bars("EURUSD", "1m").expect("and polls");

    let (mut second, _second_sink) = open(&guard, &data, "oanda", 2);

    let older = data.session(0);
    older.wait_for_call(|c| *c == DataCall::BeginShutdown);
    wait_until("the older session's subscriber is told its keys have a gap", || {
        gaps(&first_sink).len() == 2
    });
    assert_eq!(older.begin_shutdowns(), 1, "the older session's streams are told to stop once");
    assert_eq!(
        gaps(&first_sink),
        vec!["EURUSD/quotes".to_string(), "EURUSD/1m".to_string()],
        "and its subscriber is told every key it held has a gap"
    );
    let refused = first.subscribe_quotes("GBPUSD").expect_err("a replaced session opens nothing");
    let text = refused.to_string();
    assert!(text.contains("replaced by session 2"), "{text}");
    assert!(text.contains("oanda"), "{text}");
    assert!(
        !older.calls().iter().any(|c| matches!(c, DataCall::Subscribe(s) if s.symbol == "GBPUSD")),
        "the venue client was never asked: {:?}",
        older.calls()
    );
    for refused in
        [first.subscribe_trades("X"), first.subscribe_book("X"), first.subscribe_depth("X")]
    {
        assert!(refused.is_err());
    }
    // It is still a client: the held keys can be released and it can be shut down.
    first.unsubscribe(SubscriptionId(1));
    older.wait_for_call(|c| *c == DataCall::Unsubscribe(1));

    second.subscribe_quotes("GBPUSD").expect("the newer session streams");
    assert_eq!(data.session(1).begin_shutdowns(), 0, "the newer session is untouched");
}

/// **A replacement lasts as long as the replacing session does** (review of wave A, finding 4): when the
/// newer session has ended the older one is no longer refused, instead of refusing for ever.
#[test]
fn a_replaced_session_may_open_streams_again_once_its_replacement_has_ended() {
    let guard = SessionGuard::new(short);
    let data = ScriptedData::new(&[]);
    let (mut first, _sink) = open(&guard, &data, "oanda", 1);
    let (mut second, _sink) = open(&guard, &data, "oanda", 2);
    first.subscribe_quotes("EURUSD").expect_err("replaced while the newer session stands");

    second.shutdown();
    first.subscribe_quotes("EURUSD").expect("the venue is free again");
    // ...and a replacing session that is merely DROPPED frees it too.
    let (third, _sink) = open(&guard, &data, "oanda", 3);
    first.subscribe_quotes("GBPUSD").expect_err("replaced again by the third session");
    drop(third);
    first.subscribe_quotes("GBPUSD").expect("a dropped replacer holds nothing");
}

/// A session that has ENDED is not replaced (no warning, no shutdown of a client nobody holds), and a
/// venue is guarded apart from every other.
#[test]
fn an_ended_session_is_not_replaced_and_venues_are_guarded_apart() {
    let guard = SessionGuard::new(short);
    let data = ScriptedData::new(&[]);
    let (mut first, _sink) = open(&guard, &data, "oanda", 1);
    first.subscribe_quotes("EURUSD").unwrap();
    first.shutdown();
    drop(first);
    let (_second, _sink) = open(&guard, &data, "oanda", 2);
    assert_eq!(data.session(0).begin_shutdowns(), 0, "an ended session is nobody's to wind down");

    // Another guarded-or-not venue is another slot: opening it replaces nothing of oanda's.
    let (mut other, _sink) = open(&guard, &data, "alpaca", 3);
    assert_eq!(data.session(1).begin_shutdowns(), 0);
    other.subscribe_quotes("AAPL").expect("an unguarded venue is returned as built");
}

/// **A session that is still joining its streams is neither waited for nor replaced** (review of wave A,
/// finding 4): after an unclean datahub restart the old session is inside `shutdown` for up to a heartbeat
/// per stream, holding its client, and the new datahub's admission must not queue behind that, log a false
/// "two datahubs" warning or wind down a session that is already ending.
#[test]
fn a_session_that_is_shutting_down_is_not_waited_for_or_replaced() {
    let guard = SessionGuard::new(short);
    let data = ScriptedData::new(&[]);
    let (mut first, _sink) = open(&guard, &data, "oanda", 1);
    first.subscribe_quotes("EURUSD").unwrap();
    let older = data.session(0);
    older.set_join_delay(Duration::from_millis(800));
    let joining = std::thread::spawn(move || {
        first.shutdown();
        first
    });
    older.wait_for_call(|c| *c == DataCall::Shutdown);

    let started = Instant::now();
    let (mut second, _sink) = open(&guard, &data, "oanda", 2);
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "the admission waited for the old session's join: {:?}",
        started.elapsed()
    );
    second.subscribe_quotes("GBPUSD").expect("the new session streams");
    let first = joining.join().expect("the old session's shutdown finished");
    assert_eq!(older.begin_shutdowns(), 0, "an ending session is not wound down again");
    drop(first);
}

/// **The ceiling** (acceptance A5, the gatewayhub's half): the stream past it is refused with the number
/// and the cap mark, a bar lane is not a stream, and a released key makes room.
#[test]
fn the_ceiling_refuses_the_stream_past_it_with_the_number_and_the_cap_mark() {
    let guard = SessionGuard::new(short);
    let data = ScriptedData::new(&[]);
    let (mut client, _sink) = open(&guard, &data, "oanda", 1);
    let ids: Vec<SubscriptionId> = ["A", "B", "C"]
        .iter()
        .map(|s| client.subscribe_quotes(s).expect("inside the ceiling"))
        .collect();
    let text = client.subscribe_quotes("D").expect_err("past the ceiling").to_string();
    assert!(text.contains(CAP_MARK) && text.contains("(3)"), "{text}");
    assert!(
        !text.contains("raise the venue"),
        "the ceiling is a constant of the release: it must not tell an operator to raise a row: {text}"
    );
    let asked_for_d = |data: &ScriptedData| {
        data.session(0)
            .calls()
            .iter()
            .any(|c| matches!(c, DataCall::Subscribe(s) if s.symbol == "D"))
    };
    assert!(!asked_for_d(&data), "the venue client was never asked");

    client.subscribe_bars("D", "1m").expect("a bar lane is REST and is not counted");
    client.unsubscribe(ids[0]);
    client.subscribe_quotes("D").expect("a released stream makes room");
    // The stream the release asked to stop was stopped BEFORE the new one opened: the venue never saw four.
    let calls = data.session(0).calls();
    let stopped = calls.iter().position(|c| *c == DataCall::Unsubscribe(ids[0].0));
    let opened = calls.iter().position(
        |c| matches!(c, DataCall::Subscribe(s) if s.symbol == "D" && s.verb == SubVerb::Quotes),
    );
    assert!(stopped < opened && stopped.is_some(), "stop before open: {calls:?}");
}

/// **A release never holds up a subscribe** (review of wave A, finding 2): eight streams whose join takes a
/// long time each are released together, and the next subscribe is answered after at most one join, not
/// after all eight. The release itself returns at once.
#[test]
fn a_burst_of_releases_does_not_hold_up_the_next_subscribe() {
    let guard = SessionGuard::new(wide);
    let data = ScriptedData::new(&[]);
    let (mut client, _sink) = open(&guard, &data, "oanda", 1);
    let ids: Vec<SubscriptionId> = (0..8)
        .map(|k| client.subscribe_quotes(&format!("S{k}")).expect("inside the ceiling"))
        .collect();
    let session = data.session(0);
    let join = Duration::from_millis(300);
    session.set_join_delay(join);

    let started = Instant::now();
    for id in &ids {
        client.unsubscribe(*id);
    }
    assert!(
        started.elapsed() < join,
        "a release joined the stream instead of queueing it: {:?}",
        started.elapsed()
    );

    client.subscribe_quotes("NEW").expect("the next subscribe is answered");
    assert!(
        started.elapsed() < join * 4,
        "the subscribe queued behind the joins (eight of them take {:?}): {:?}",
        join * 8,
        started.elapsed()
    );
    // Nothing is lost: every released stream is still joined, one at a time, by the guard's own thread.
    wait_until("the eight released streams are all stopped", || {
        session.calls().iter().filter(|c| matches!(c, DataCall::Unsubscribe(_))).count() == 8
    });
}

/// A symbol released and subscribed again at once is never streamed twice: the queued release of the SAME
/// symbol is made first.
#[test]
fn a_symbol_released_and_taken_again_is_stopped_before_it_is_opened_again() {
    let guard = SessionGuard::new(wide);
    let data = ScriptedData::new(&[]);
    let (mut client, _sink) = open(&guard, &data, "oanda", 1);
    let id = client.subscribe_quotes("EURUSD").unwrap();
    client.unsubscribe(id);
    client.subscribe_quotes("EURUSD").unwrap();
    let calls = data.session(0).calls();
    let stop = calls.iter().position(|c| *c == DataCall::Unsubscribe(id.0)).expect("stopped");
    let opens: Vec<usize> = calls
        .iter()
        .enumerate()
        .filter_map(|(i, c)| matches!(c, DataCall::Subscribe(_)).then_some(i))
        .collect();
    assert_eq!(opens.len(), 2, "{calls:?}");
    assert!(opens[0] < stop && stop < opens[1], "stop between the two opens: {calls:?}");
}

/// **The spacing** (acceptance A7): quote opens of one venue start at least the row's spacing apart, a
/// second session's included, and nothing else is slowed.
#[test]
fn stream_opens_are_spaced_across_sessions() {
    let guard = SessionGuard::new(short);
    let data = ScriptedData::new(&[]);
    let opened: Arc<Mutex<Vec<Instant>>> = Arc::default();
    let record = Arc::clone(&opened);
    data.set_default_hook(Arc::new(move |_sink: &dyn LiveDataSink, call: &SubscribeCall| {
        if call.verb == SubVerb::Quotes {
            record.lock().unwrap().push(Instant::now());
        }
    }));
    let (mut first, _sink) = open(&guard, &data, "oanda", 1);
    first.subscribe_quotes("A").unwrap();
    let (mut second, _sink) = open(&guard, &data, "oanda", 2);
    second.subscribe_quotes("B").unwrap();
    second.subscribe_quotes("C").unwrap();

    let at = opened.lock().unwrap().clone();
    assert_eq!(at.len(), 3);
    // The reservation is taken a moment before the hook runs, so a pair may read a little short: by the
    // delay of the earlier open's thread, which on a loaded runner is far under half a spacing.
    let slack = SPACING / 2;
    assert!(at[1] - at[0] + slack >= SPACING, "the second session's open: {:?}", at[1] - at[0]);
    assert!(at[2] - at[1] + slack >= SPACING, "the next open: {:?}", at[2] - at[1]);

    // Only an open takes a slot: a release waits for none. (The first session was replaced by the second; a
    // release is still accepted.)
    let started = Instant::now();
    second.unsubscribe(SubscriptionId(1));
    second.unsubscribe(SubscriptionId(2));
    assert!(
        started.elapsed() < SPACING / 2,
        "an unsubscribe waits for no slot: {:?}",
        started.elapsed()
    );
}

/// A venue with no row is returned exactly as built: no spacing, no ceiling, no replacement.
#[test]
fn a_venue_with_no_row_is_not_guarded_at_all() {
    let guard = SessionGuard::new(short);
    let data = ScriptedData::new(&[]);
    let (mut first, _sink) = open(&guard, &data, "ibkr", 1);
    let (mut second, _sink) = open(&guard, &data, "ibkr", 2);
    let started = Instant::now();
    for k in 0..16 {
        first.subscribe_quotes(&format!("S{k}")).expect("no ceiling");
    }
    second.subscribe_quotes("T").expect("no replacement");
    assert!(started.elapsed() < SPACING * 4, "no spacing: {:?}", started.elapsed());
    assert_eq!(data.session(0).begin_shutdowns(), 0);
}

/// What a bridge client does with the two verbs the guard forwards: `set_delivery` is recorded and
/// `is_dead` answers for an id.
struct Probe {
    seen: Arc<Mutex<Vec<String>>>,
}

impl DataClient for Probe {
    fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("probe"))
    }
    fn subscribe_quotes(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Ok(SubscriptionId(7))
    }
    fn subscribe_trades(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("probe"))
    }
    fn subscribe_book(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("probe"))
    }
    fn unsubscribe(&mut self, _: SubscriptionId) {}
    fn set_delivery(
        &mut self,
        symbol: &str,
        lane: StreamLabel,
        class: Delivery,
    ) -> Result<(), LiveDataError> {
        self.seen.lock().unwrap().push(format!("{symbol} {} {class:?}", lane.as_str()));
        Ok(())
    }
    fn is_dead(&self, id: SubscriptionId) -> bool {
        id == SubscriptionId(7)
    }
    fn shutdown(&mut self) {}
}

/// **The guard hides nothing the venue client answers**: `set_delivery` and `is_dead` reach the client
/// instead of the trait's defaults (a client that is no guard's would answer them the same way).
#[test]
fn the_guard_forwards_set_delivery_and_is_dead_to_the_venue_client() {
    let guard = SessionGuard::new(short);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(RecordingSink::default());
    let mut client = guard.admit("oanda", 1, sink, Box::new(Probe { seen: Arc::clone(&seen) }));
    let id = client.subscribe_quotes("EURUSD").unwrap();
    client.set_delivery("EURUSD", StreamLabel::Quotes, Delivery::Lossless).unwrap();
    assert_eq!(*seen.lock().unwrap(), vec!["EURUSD quotes Lossless".to_string()]);
    assert!(client.is_dead(id));
    assert!(!client.is_dead(SubscriptionId(8)));
}

/// **`begin_shutdown` never waits for a join in progress** (second review of wave A): the session's single
/// reader calls it when the datahub's drain sends `BeginShutdown`, and a reaper mid-join holds the venue
/// client for up to a heartbeat. The call returns at once, and the stop flags are raised the moment the join
/// is done.
#[test]
fn begin_shutdown_does_not_wait_for_a_join_in_progress_and_is_honoured_after_it() {
    let guard = SessionGuard::new(wide);
    let data = ScriptedData::new(&[]);
    let (mut client, _sink) = open(&guard, &data, "oanda", 1);
    let id = client.subscribe_quotes("EURUSD").unwrap();
    let session = data.session(0);
    session.set_join_delay(Duration::from_millis(600));
    client.unsubscribe(id);
    session.wait_for_call(|c| matches!(c, DataCall::Unsubscribe(_))); // the reaper is inside the join now

    let started = Instant::now();
    client.begin_shutdown();
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "begin_shutdown waited for the join: {:?}",
        started.elapsed()
    );
    wait_until("the stop flags are raised once the join is done", || {
        session.begin_shutdowns() == 1
    });
}

/// **A replaced session that is let back in holds no phantom keys** (second review of wave A): the streams
/// it held when it was replaced were stopped, so they stop counting against its ceiling; before, the three
/// stopped keys of a three-stream ceiling refused every later subscribe of the recovered session.
#[test]
fn a_replaced_session_that_is_let_back_in_holds_no_phantom_keys() {
    let guard = SessionGuard::new(short);
    let data = ScriptedData::new(&[]);
    let (mut first, _sink) = open(&guard, &data, "oanda", 1);
    for symbol in ["A", "B", "C"] {
        first.subscribe_quotes(symbol).unwrap();
    }
    let (mut second, _sink) = open(&guard, &data, "oanda", 2);
    let older = data.session(0);
    wait_until("the replaced session's three stopped streams are joined", || {
        older.calls().iter().filter(|c| matches!(c, DataCall::Unsubscribe(_))).count() == 3
    });

    second.shutdown();
    first.subscribe_quotes("D").expect("its stopped keys no longer count against its ceiling");
}

/// **A subscribe that has waited too long behind a join is refused as TRANSIENT, with its own release still
/// queued** (second review of wave A): a returning tradehub's first `Sub` could wait out the join in progress
/// and then the join of its own symbol's release, and outlast the datahub's ten-second reply. Past the budget
/// no further join is begun; the refusal carries the transient mark (the caller's next pass is the cure), no
/// stream was opened, and the retry is answered.
#[test]
fn a_subscribe_that_waited_too_long_behind_a_join_is_refused_as_transient_and_a_retry_is_answered()
{
    let mut guard = SessionGuard::new(wide);
    guard.join_budget = Duration::from_millis(300);
    let data = ScriptedData::new(&[]);
    let (mut client, _sink) = open(&guard, &data, "oanda", 1);
    let a = client.subscribe_quotes("A").unwrap();
    let b = client.subscribe_quotes("B").unwrap();
    let session = data.session(0);
    session.set_join_delay(Duration::from_millis(500));
    client.unsubscribe(b);
    client.unsubscribe(a);
    session.wait_for_call(|c| *c == DataCall::Unsubscribe(b.0)); // the reaper is inside B's join

    let opens = |session: &crate::testkit::data::ScriptedSession| {
        session
            .calls()
            .iter()
            .filter(|c| matches!(c, DataCall::Subscribe(s) if s.symbol == "A"))
            .count()
    };
    let text = client.subscribe_quotes("A").expect_err("A waited behind B's join").to_string();
    assert!(text.contains(TRANSIENT_MARK) && !text.contains(CAP_MARK), "{text}");
    assert_eq!(opens(&session), 1, "no second stream of A was opened");

    client.subscribe_quotes("A").expect("the retry is answered once the joins are done");
    assert_eq!(opens(&session), 2);
}

/// A bridge whose first `unsubscribe` panics (a bug in a join) and which behaves afterwards.
struct Panicky {
    joined: Arc<Mutex<Vec<u64>>>,
    panicked: Arc<AtomicBool>,
    next: u64,
}

impl DataClient for Panicky {
    fn subscribe_bars(&mut self, _: &str, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("panicky"))
    }
    fn subscribe_quotes(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        self.next += 1;
        Ok(SubscriptionId(self.next))
    }
    fn subscribe_trades(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("panicky"))
    }
    fn subscribe_book(&mut self, _: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("panicky"))
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        assert!(self.panicked.swap(true, Ordering::SeqCst), "scripted failure in a join");
        self.joined.lock().unwrap().push(id.0);
    }
    fn shutdown(&mut self) {}
}

/// **A panic in a bridge's join does not strand the queue** (second review of wave A): `Held::reaping` stayed
/// true for ever after the reaper thread died, so no later release was joined. The next release starts a new
/// reaper.
#[test]
fn a_panic_in_a_join_does_not_stop_the_next_release_from_being_joined() {
    let guard = SessionGuard::new(wide);
    let (joined, panicked) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
    let sink = Arc::new(RecordingSink::default());
    let mut client = guard.admit(
        "oanda",
        1,
        sink,
        Box::new(Panicky { joined: Arc::clone(&joined), panicked: Arc::clone(&panicked), next: 0 }),
    );
    let a = client.subscribe_quotes("A").unwrap();
    let b = client.subscribe_quotes("B").unwrap();
    client.unsubscribe(a); // the reaper panics in this join
    wait_until("the join panicked", || panicked.load(Ordering::SeqCst));
    std::thread::sleep(Duration::from_millis(100)); // the reaper thread unwinds

    client.unsubscribe(b);
    wait_until("the next release is joined by a new reaper", || {
        joined.lock().unwrap().contains(&b.0)
    });
}
