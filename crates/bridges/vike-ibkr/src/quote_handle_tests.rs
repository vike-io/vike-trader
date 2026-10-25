//! The lazy price stream over a SCRIPTED data client: nothing dialled until a symbol is asked for, one dial for N
//! symbols, a failed dial remembered, the cap, the quotes-only refusals and the teardown; and its SUPERVISOR on a
//! FAKE clock (`Shared::supervise` is handed the instant, so nothing sleeps): a dead connection is disclosed, re-dialled
//! once per backoff step, re-subscribed and told live, without a flapping storm.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vike_data::{LiveDataError, NoopSink, SubscriptionId};
use vike_model::StreamStatus;

use super::*;
use crate::data_door::test_support::{Call, RecSink};
use crate::redial::{LIVE_AFTER, SELF_HEAL};

const AAPL: &str = "AAPL.SMART.USD";
const TSLA: &str = "TSLA.SMART.USD";

/// Midday UTC in epoch ms: nowhere near IB's daily logoff.
const NOON_MS: i64 = (3 * 86_400 + 12 * 3600) * 1000;

fn s(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// One scripted CONNECTION: whether it is dead, and whether it was closed.
#[derive(Default)]
struct Line {
    dead: AtomicBool,
    closed: AtomicBool,
}

/// A data client that records the quote streams it was asked for and what it was told to stop.
struct Scripted {
    line: Arc<Line>,
    asked: Arc<Mutex<Vec<String>>>,
    stopped: Arc<Mutex<Vec<u64>>>,
    closed: Arc<AtomicBool>,
    refuse: bool,
}

impl DataClient for Scripted {
    fn subscribe_bars(&mut self, _s: &str, _i: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("bars"))
    }
    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        if self.refuse {
            return Err(LiveDataError::Subscribe("no such contract".into()));
        }
        let mut asked = self.asked.lock().expect("asked");
        asked.push(symbol.to_string());
        Ok(SubscriptionId(asked.len() as u64))
    }
    fn subscribe_trades(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("trades"))
    }
    fn subscribe_book(&mut self, _s: &str) -> Result<SubscriptionId, LiveDataError> {
        Err(LiveDataError::Unsupported("book"))
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.stopped.lock().expect("stopped").push(id.0);
    }
    fn is_dead(&self, _id: SubscriptionId) -> bool {
        self.line.dead.load(Ordering::SeqCst)
    }
    fn shutdown(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        self.line.closed.store(true, Ordering::SeqCst);
    }
}

/// What the supervisor did on a look, tagged with the second of the fake clock it happened at.
#[derive(Debug, PartialEq)]
enum Event {
    Dial(u64, DialKind),
    Status(u64, String, StreamStatus),
}

/// A stream over a scripted dial: how many dials were made (and of what kind), what the clients were asked and told to
/// stop, whether any was closed, switches that make dials fail, and what the gateway's readiness answers.
struct Rig {
    stream: LazyQuotes,
    sink: Arc<RecSink>,
    dials: Arc<AtomicUsize>,
    kinds: Arc<Mutex<Vec<DialKind>>>,
    lines: Arc<Mutex<Vec<Arc<Line>>>>,
    asked: Arc<Mutex<Vec<String>>>,
    stopped: Arc<Mutex<Vec<u64>>>,
    closed: Arc<AtomicBool>,
    /// Every dial fails while set.
    dial_fails: Arc<AtomicBool>,
    /// The next N dials fail.
    fail_next: Arc<AtomicUsize>,
    /// The next N connections dial fine and refuse every subscription.
    refuse_next: Arc<AtomicUsize>,
    /// What the gateway's readiness answers, and how often it was asked.
    ready: Arc<Mutex<Result<(), Duration>>>,
    asks: Arc<AtomicUsize>,
}

/// `true` and one fewer pending when `n` was above zero.
fn take(n: &AtomicUsize) -> bool {
    n.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1)).is_ok()
}

fn rig(refuse: bool) -> Rig {
    let dials = Arc::new(AtomicUsize::new(0));
    let kinds = Arc::new(Mutex::new(Vec::new()));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let stopped = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(AtomicBool::new(false));
    let dial_fails = Arc::new(AtomicBool::new(false));
    let fail_next = Arc::new(AtomicUsize::new(0));
    let refuse_next = Arc::new(AtomicUsize::new(0));
    let ready = Arc::new(Mutex::new(Ok(())));
    let asks = Arc::new(AtomicUsize::new(0));
    let sink = RecSink::arc();
    let dial: Dial = {
        let (d, k, l) = (dials.clone(), kinds.clone(), lines.clone());
        let (a, st, c) = (asked.clone(), stopped.clone(), closed.clone());
        let (f, fnext, rnext) = (dial_fails.clone(), fail_next.clone(), refuse_next.clone());
        Box::new(move |_sink, kind| {
            d.fetch_add(1, Ordering::SeqCst);
            k.lock().expect("kinds").push(kind);
            if f.load(Ordering::SeqCst) || take(&fnext) {
                return Err(IbkrError::Connect(
                    "gateway took the socket and never answered".into(),
                ));
            }
            let line = Arc::new(Line::default());
            l.lock().expect("lines").push(Arc::clone(&line));
            let refuse = refuse || take(&rnext);
            Ok(Box::new(Scripted {
                line,
                asked: a.clone(),
                stopped: st.clone(),
                closed: c.clone(),
                refuse,
            }))
        })
    };
    let ready_fn: Ready = {
        let (state, asks) = (ready.clone(), asks.clone());
        Box::new(move || {
            asks.fetch_add(1, Ordering::SeqCst);
            *state.lock().expect("ready")
        })
    };
    let sink_dyn: Arc<dyn LiveDataSink> = sink.clone();
    Rig {
        stream: LazyQuotes::build(sink_dyn, dial, ready_fn, false),
        sink,
        dials,
        kinds,
        lines,
        asked,
        stopped,
        closed,
        dial_fails,
        fail_next,
        refuse_next,
        ready,
        asks,
    }
}

impl Rig {
    fn dial_count(&self) -> usize {
        self.dials.load(Ordering::SeqCst)
    }

    fn line(&self, i: usize) -> Arc<Line> {
        Arc::clone(&self.lines.lock().expect("lines")[i])
    }

    fn kinds(&self) -> Vec<DialKind> {
        self.kinds.lock().expect("kinds").clone()
    }

    /// The symbols asked of the clients, sorted (the held symbols are re-subscribed in no particular order).
    fn asked_sorted(&self) -> Vec<String> {
        let mut asked = self.asked.lock().expect("asked").clone();
        asked.sort();
        asked
    }

    /// The id the CURRENT client gave `symbol`.
    fn inner(&self, symbol: &str) -> SubscriptionId {
        self.stream.shared.lock().held[symbol].inner
    }

    fn set_ready(&self, answer: Result<(), Duration>) {
        *self.ready.lock().expect("ready") = answer;
    }

    /// One look at second `n` of the fake clock (wall clock = midday + `n`).
    fn look(&self, t0: Instant, n: u64) -> Vec<Event> {
        self.look_at(t0, n, NOON_MS + n as i64 * 1000)
    }

    /// One look at second `n` of the fake clock with the wall clock at `wall_ms`; what it did, in order.
    fn look_at(&self, t0: Instant, n: u64, wall_ms: i64) -> Vec<Event> {
        let before = self.kinds().len();
        self.stream.shared.supervise(t0 + s(n), wall_ms);
        let mut events: Vec<Event> =
            self.kinds().into_iter().skip(before).map(|kind| Event::Dial(n, kind)).collect();
        let mut statuses: Vec<Event> = self
            .sink
            .take()
            .into_iter()
            .filter_map(|call| match call {
                Call::Status(symbol, stream, status) => {
                    assert_eq!(stream, "quotes", "the lane a price stream discloses on");
                    Some(Event::Status(n, symbol, status))
                }
                _ => None,
            })
            .collect();
        statuses.sort_by_key(|e| format!("{e:?}"));
        events.extend(statuses);
        events
    }

    /// Looks at every second of `secs`, all the events in order.
    fn run(&self, t0: Instant, secs: std::ops::RangeInclusive<u64>) -> Vec<Event> {
        secs.flat_map(|n| self.look(t0, n)).collect()
    }
}

fn dials_of(events: &[Event]) -> Vec<u64> {
    events.iter().filter_map(|e| if let Event::Dial(n, _) = e { Some(*n) } else { None }).collect()
}

/// The (second, symbol) of every `GapStart` in `events`, and of every `Live`.
fn gap_starts(events: &[Event]) -> Vec<(u64, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Status(n, symbol, StreamStatus::GapStart { .. }) => Some((*n, symbol.clone())),
            _ => None,
        })
        .collect()
}

fn lives(events: &[Event]) -> Vec<(u64, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Status(n, symbol, StreamStatus::Live { .. }) => Some((*n, symbol.clone())),
            _ => None,
        })
        .collect()
}

/// LAZY: building the stream dials nothing and subscribes nothing, and the verbs the price lane does not
/// need are refused by name without a dial.
#[test]
fn building_the_stream_dials_nothing_and_only_quotes_are_served() {
    let mut rig = rig(false);
    assert_eq!(rig.dial_count(), 0);
    for refused in [
        rig.stream.subscribe_bars(AAPL, "1m"),
        rig.stream.subscribe_trades(AAPL),
        rig.stream.subscribe_book(AAPL),
        rig.stream.subscribe_depth(AAPL),
    ] {
        assert!(matches!(refused, Err(LiveDataError::Unsupported(_))), "{refused:?}");
    }
    assert_eq!(rig.dial_count(), 0, "a refused verb dials nothing");
    assert!(rig.stream.shared.lock().held.is_empty());
}

/// N symbols are ONE dial and N subscriptions, however many times each is asked: the second ask of a symbol
/// answers the same id and subscribes nothing.
#[test]
fn many_symbols_are_one_dial() {
    let mut rig = rig(false);
    let first = rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    for symbol in [TSLA, "MSFT.SMART.USD", AAPL, "tsla.smart.usd"] {
        rig.stream.subscribe_quotes(symbol).expect("subscribed");
    }
    let again = rig.stream.subscribe_quotes(AAPL).expect("still subscribed");
    assert_eq!(again, first, "the same stream, the same id");
    assert_eq!(rig.dial_count(), 1);
    assert_eq!(rig.kinds(), [DialKind::First]);
    assert_eq!(*rig.asked.lock().expect("asked"), [AAPL, TSLA, "MSFT.SMART.USD"]);
    assert_eq!(rig.stream.shared.lock().held.len(), 3);
}

/// The dial is handed the sink the stream was built over: the quotes and labels reach the tube through it.
#[test]
fn the_dial_is_given_the_streams_own_sink() {
    let sink: Arc<dyn LiveDataSink> = Arc::new(NoopSink);
    let seen: Arc<Mutex<Option<Arc<dyn LiveDataSink>>>> = Arc::default();
    let keep = Arc::clone(&seen);
    let dial: Dial = Box::new(move |s, _| {
        *keep.lock().expect("seen") = Some(s);
        Err(IbkrError::Unavailable)
    });
    let mut stream = LazyQuotes::build(Arc::clone(&sink), dial, Box::new(|| Ok(())), false);
    let _ = stream.subscribe_quotes(AAPL);
    let given = seen.lock().expect("seen").take().expect("the dial ran");
    assert!(Arc::ptr_eq(&given, &sink));
}

/// A failed dial is remembered: the asks inside the window answer the same sentence and make no dial, so a
/// burst of orders on a dead gateway is not a burst of dials; the symbol is not recorded as subscribed.
#[test]
fn a_failed_dial_is_remembered_and_not_repeated() {
    let mut rig = rig(false);
    rig.dial_fails.store(true, Ordering::SeqCst);
    let first = rig.stream.subscribe_quotes(AAPL).expect_err("the dial failed");
    let words = first.to_string();
    assert!(
        words.contains("no market-data connection") && words.contains("never answered"),
        "{words}"
    );
    for _ in 0..5 {
        let again = rig.stream.subscribe_quotes(TSLA).expect_err("remembered");
        assert_eq!(again.to_string(), words, "the same sentence, no dial");
    }
    assert_eq!(rig.dial_count(), 1, "one dial for six asks");
    assert!(rig.stream.shared.lock().held.is_empty());
}

/// Past the window the dial is tried again, and a client that came up serves the symbol.
#[test]
fn the_dial_is_tried_again_after_the_window() {
    let mut rig = rig(false);
    rig.dial_fails.store(true, Ordering::SeqCst);
    assert!(rig.stream.subscribe_quotes(AAPL).is_err());
    rig.dial_fails.store(false, Ordering::SeqCst);
    // Age the remembered failure past the window without sleeping.
    let aged =
        Instant::now().checked_sub(REDIAL_AFTER + Duration::from_secs(1)).expect("an old instant");
    {
        let mut core = rig.stream.shared.lock();
        core.failed = core.failed.take().map(|(_, why)| (aged, why));
    }
    assert!(rig.stream.subscribe_quotes(AAPL).is_ok());
    assert_eq!(rig.dial_count(), 2);
}

/// A subscription the client refuses is an error naming the symbol and is not recorded.
#[test]
fn a_refused_subscription_is_not_recorded() {
    let mut rig = rig(true);
    let why = rig.stream.subscribe_quotes(AAPL).expect_err("refused").to_string();
    assert!(why.contains(AAPL) && why.contains("no such contract"), "{why}");
    assert!(rig.stream.shared.lock().held.is_empty());
}

/// The engine's own cap on admitted symbols bounds the market-data lines the stream opens; one past it is
/// refused and the client is not asked, and a symbol let go makes room again.
#[test]
fn the_symbols_priced_are_capped_at_the_engines_cap() {
    let mut rig = rig(false);
    let cap = vike_exec::MAX_EXTRA_SYMBOLS;
    let mut first = None;
    for n in 0..cap {
        let id = rig.stream.subscribe_quotes(&format!("S{n}.SMART.USD")).expect("within the cap");
        first.get_or_insert(id);
    }
    let why = rig.stream.subscribe_quotes("ONEMORE.SMART.USD").expect_err("past the cap");
    assert!(why.to_string().contains(&format!("at most {cap} symbols")), "{why}");
    assert_eq!(rig.asked.lock().expect("asked").len(), cap);
    // A symbol already streaming is still answered.
    assert!(rig.stream.subscribe_quotes("S0.SMART.USD").is_ok());
    // Letting one go forwards the stop (of the CLIENT's id for it) and frees its place.
    let inner = rig.inner("S0.SMART.USD");
    rig.stream.unsubscribe(first.expect("one id"));
    assert_eq!(*rig.stopped.lock().expect("stopped"), [inner.0]);
    assert!(rig.stream.subscribe_quotes("ONEMORE.SMART.USD").is_ok());
}

/// Shutting the stream down closes its client.
#[test]
fn shutdown_closes_the_client() {
    let mut rig = rig(false);
    rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    rig.stream.shutdown();
    assert!(rig.closed.load(Ordering::SeqCst));
    assert!(rig.stream.shared.lock().held.is_empty());
}

/// Dropping the stream closes its client too (a session that ends without its shutdown).
#[test]
fn dropping_the_stream_closes_the_client() {
    let mut rig = rig(false);
    rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    let closed = Arc::clone(&rig.closed);
    drop(rig);
    assert!(closed.load(Ordering::SeqCst));
}

// --- the supervisor ------------------------------------------------------------------------------------------------

/// A healthy stream is left alone: no disclosure, no dial, however long it is watched.
#[test]
fn a_healthy_stream_discloses_nothing_and_dials_nothing() {
    let mut rig = rig(false);
    rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    let t0 = Instant::now();
    assert!(rig.run(t0, 0..=600).is_empty());
    assert_eq!(rig.dial_count(), 1);
    assert_eq!(rig.asks.load(Ordering::SeqCst), 0, "the gateway is not even asked");
}

/// **THE CASE**: the connection dies, the outage is disclosed once per symbol at once, the dial is made once per
/// backoff step (20 s of grace, then 60 s, 2 min, 5 min after each failure), every held symbol is subscribed again
/// when one holds, and `Live` follows when it has stood its probation: no second `GapStart` on the way.
#[test]
fn a_dead_connection_is_redialled_once_per_backoff_step_resubscribed_and_told_live() {
    let mut rig = rig(false);
    let aapl = rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    let tsla = rig.stream.subscribe_quotes(TSLA).expect("subscribed");
    let t0 = Instant::now();
    assert!(rig.run(t0, 0..=29).is_empty(), "healthy for 30 s");
    rig.fail_next.store(3, Ordering::SeqCst);
    rig.line(0).dead.store(true, Ordering::SeqCst);

    let events = rig.run(t0, 30..=700);
    let wall = |n: u64| NOON_MS + n as i64 * 1000;

    // The outage is told at the first look that sees it, once per symbol, with that look's stamp.
    let (down, symbols) = (30, [AAPL.to_string(), TSLA.to_string()]);
    let mut expect_gaps: Vec<_> = symbols.iter().map(|sym| (down, sym.clone())).collect();
    expect_gaps.sort();
    let mut gaps = gap_starts(&events);
    gaps.sort();
    assert_eq!(gaps, expect_gaps, "ONE GapStart per symbol, however long and however many dials");
    for e in &events {
        if let Event::Status(_, _, StreamStatus::GapStart { at_ts_ms }) = e {
            assert_eq!(*at_ts_ms, wall(down));
        }
    }

    // One dial per step: 20 s after the death, then 60 s, 120 s and 300 s after each failure (three fail).
    assert_eq!(dials_of(&events), [50, 110, 230, 530]);
    assert_eq!(
        rig.kinds(),
        [DialKind::First, DialKind::Redial, DialKind::Redial, DialKind::Redial, DialKind::Redial]
    );

    // The fourth dial worked: both symbols are subscribed again, on the new connection.
    assert_eq!(rig.asked_sorted(), [AAPL, AAPL, TSLA, TSLA]);
    assert!(rig.line(0).closed.load(Ordering::SeqCst), "the dead connection was shut down");
    assert!(!rig.line(1).closed.load(Ordering::SeqCst));

    // `Live` after the probation, for both, echoing the stamp of the gap it closes.
    let mut live = lives(&events);
    live.sort();
    let at = 530 + LIVE_AFTER.as_secs();
    assert_eq!(live, symbols.iter().map(|sym| (at, sym.clone())).collect::<Vec<_>>());
    for e in &events {
        if let Event::Status(_, _, StreamStatus::Live { gap_started_ts_ms }) = e {
            assert_eq!(*gap_started_ts_ms, Some(wall(down)));
        }
    }

    // The caller's ids survived the re-subscribe, and a stop reaches the NEW connection's stream.
    assert_eq!(rig.stream.subscribe_quotes(AAPL).expect("held"), aapl);
    let fresh = rig.inner(AAPL);
    rig.stream.unsubscribe(aapl);
    assert_eq!(*rig.stopped.lock().expect("stopped"), [fresh.0]);
    assert_ne!(tsla, aapl);
}

/// A gateway whose port is closed (the daily logoff, an operator restart) is asked about again and again and is NEVER
/// dialled, spends no attempt and climbs no backoff step; the dial is made on the first look after it is ready.
#[test]
fn a_closed_port_costs_no_attempt_and_the_dial_follows_the_port() {
    let mut rig = rig(false);
    rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    let t0 = Instant::now();
    rig.line(0).dead.store(true, Ordering::SeqCst);
    rig.set_ready(Err(PROBE_EVERY));
    let events = rig.run(t0, 0..=100);
    assert!(dials_of(&events).is_empty(), "no dial while the port is closed");
    assert_eq!(rig.dial_count(), 1);
    assert_eq!(rig.stream.shared.lock().policy.failed(), 0, "a closed port is not a failed dial");
    // From second 20 on, one question every 3 s: 20, 23, ..., 98.
    assert_eq!(rig.asks.load(Ordering::SeqCst), (20..=100).step_by(3).count());

    rig.set_ready(Ok(()));
    let events = rig.run(t0, 101..=110);
    assert_eq!(dials_of(&events), [101], "the first look after the port is back dials");
    assert_eq!(rig.kinds().last(), Some(&DialKind::Redial));
}

/// **Budget exhausted**: the gate says the re-dial lane's share of the hour is spent for 50 minutes. The stream STAYS
/// down and says so (the `GapStart` stays open, nothing is dialled, the gate is asked once, not every second), and it
/// dials the moment the gate allows it.
#[test]
fn a_spent_budget_keeps_the_stream_down_and_disclosed_until_the_gate_allows() {
    let mut rig = rig(false);
    rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    let t0 = Instant::now();
    rig.line(0).dead.store(true, Ordering::SeqCst);
    rig.set_ready(Err(s(3000)));
    let events = rig.run(t0, 0..=3019);
    assert_eq!(gap_starts(&events), [(0, AAPL.to_string())], "disclosed once, at the first look");
    assert!(lives(&events).is_empty(), "and never told live");
    assert!(dials_of(&events).is_empty());
    assert_eq!(rig.dial_count(), 1);
    assert_eq!(
        rig.asks.load(Ordering::SeqCst),
        1,
        "the gate is asked once, then not before it said"
    );
    assert!(rig.stream.shared.lock().policy.next_dial().is_some(), "still down");

    rig.set_ready(Ok(()));
    let events = rig.run(t0, 3020..=3030);
    assert_eq!(dials_of(&events), [3020]);
    assert_eq!(lives(&events), [(3025, AAPL.to_string())]);
}

/// A dial the gateway refuses or never answers is a FAILED dial (the ladder moves) even when the gate had said ready.
#[test]
fn a_dial_that_fails_moves_the_ladder_and_a_gate_refusal_in_it_reads_the_same() {
    let mut rig = rig(false);
    rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    let t0 = Instant::now();
    rig.line(0).dead.store(true, Ordering::SeqCst);
    rig.dial_fails.store(true, Ordering::SeqCst);
    let events = rig.run(t0, 0..=1000);
    assert_eq!(
        dials_of(&events),
        [20, 80, 200, 500, 800],
        "20 s, then the gate's own 60 s, 2 min, 5 min, 5 min"
    );
    assert_eq!(rig.stream.shared.lock().policy.failed(), 5);
    assert_eq!(gap_starts(&events).len(), 1);
}

/// IB's daily logoff: an outage that begins at 23:45:02 UTC is quiet until the gateway can be back (23:46:20), not
/// dialled at 20 s into a closed port; the same death at midday is dialled at 20 s.
#[test]
fn the_daily_logoff_holds_the_first_dial_until_the_gateway_can_be_back() {
    let logoff_ms = (3 * 86_400 + 23 * 3600 + 45 * 60 + 2) * 1000;
    let mut rig = rig(false);
    rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    let t0 = Instant::now();
    rig.line(0).dead.store(true, Ordering::SeqCst);
    let mut events = Vec::new();
    for n in 0..=100 {
        events.extend(rig.look_at(t0, n, logoff_ms + n as i64 * 1000));
    }
    assert_eq!(gap_starts(&events), [(0, AAPL.to_string())], "disclosed at once all the same");
    assert_eq!(dials_of(&events), [78], "23:46:20, 80 s after the logoff");
    assert_eq!(
        rig.asks.load(Ordering::SeqCst),
        1,
        "the gateway is not asked inside the quiet window"
    );

    // The same death at midday is dialled after the plain 20 s.
    let mut midday = self::rig(false);
    midday.stream.subscribe_quotes(AAPL).expect("subscribed");
    midday.line(0).dead.store(true, Ordering::SeqCst);
    assert_eq!(dials_of(&midday.run(t0, 0..=30)), [20]);
}

/// **No flapping storm**: a connection that is accepted and drops again inside the probation is one outage, with one
/// `GapStart` per symbol and one `Live` at its end; the ladder climbs from where it was.
#[test]
fn a_flapping_connection_is_one_outage_with_one_gap_and_one_live() {
    let mut rig = rig(false);
    rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    let t0 = Instant::now();
    rig.line(0).dead.store(true, Ordering::SeqCst);
    let mut events = rig.run(t0, 0..=21);
    assert_eq!(dials_of(&events), [20], "the first dial works");
    // It drops again 2 s after it came up.
    rig.line(1).dead.store(true, Ordering::SeqCst);
    events.extend(rig.run(t0, 22..=81));
    assert_eq!(dials_of(&events), [20], "the flap is a failed dial: the next waits 60 s from it");
    events.extend(rig.run(t0, 82..=90));
    assert_eq!(dials_of(&events), [20, 82]);
    assert_eq!(gap_starts(&events), [(0, AAPL.to_string())], "no second GapStart for the flap");
    assert_eq!(
        lives(&events),
        [(87, AAPL.to_string())],
        "one Live, after the connection that held"
    );
    assert_eq!(rig.stream.shared.lock().policy.failed(), 0, "and the next outage starts afresh");
}

/// A connection the gateway lets in and then refuses every subscription on is a failed dial too: all or nothing, the
/// old state is kept, the new connection is given straight back, and the ladder moves.
#[test]
fn a_new_connection_that_refuses_a_symbol_is_a_failed_dial() {
    let mut rig = rig(false);
    rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    let t0 = Instant::now();
    let held = rig.inner(AAPL);
    rig.line(0).dead.store(true, Ordering::SeqCst);
    rig.refuse_next.store(1, Ordering::SeqCst);
    let events = rig.run(t0, 0..=100);
    assert_eq!(
        dials_of(&events),
        [20, 80],
        "the refused re-subscribe waited the first backoff step"
    );
    assert!(rig.line(1).closed.load(Ordering::SeqCst), "the refusing connection was given back");
    assert!(rig.line(0).closed.load(Ordering::SeqCst), "the working re-dial replaced the dead one");
    assert_ne!(rig.inner(AAPL), held);
}

/// A symbol let go while its stream is down closes its own window (`Live`), so nothing trips the dead-man for a
/// key nobody holds; a symbol subscribed during the outage is told down on the next look.
#[test]
fn a_released_symbol_closes_its_window_and_a_late_one_is_told_down() {
    let mut rig = rig(false);
    let aapl = rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    rig.stream.subscribe_quotes(TSLA).expect("subscribed");
    let t0 = Instant::now();
    rig.line(0).dead.store(true, Ordering::SeqCst);
    let events = rig.look(t0, 0);
    assert_eq!(gap_starts(&events).len(), 2);

    rig.stream.unsubscribe(aapl);
    let calls = rig.sink.take();
    assert_eq!(
        calls,
        [Call::Status(
            AAPL.to_string(),
            "quotes".to_string(),
            StreamStatus::Live { gap_started_ts_ms: Some(NOON_MS) }
        )],
        "the released key's window is closed with the stamp it opened at"
    );

    rig.stream.subscribe_quotes("MSFT.SMART.USD").expect("subscribed during the outage");
    let events = rig.look(t0, 1);
    assert_eq!(gap_starts(&events), [(1, "MSFT.SMART.USD".to_string())]);
}

/// The shutdown of a stream that is down closes the windows of its keys, and the supervisor does nothing after it.
#[test]
fn a_shutdown_in_an_outage_closes_the_windows_and_the_supervisor_stops() {
    let mut rig = rig(false);
    rig.stream.subscribe_quotes(AAPL).expect("subscribed");
    let t0 = Instant::now();
    rig.line(0).dead.store(true, Ordering::SeqCst);
    rig.look(t0, 0);
    rig.stream.shutdown();
    let calls = rig.sink.take();
    assert!(
        matches!(calls.as_slice(), [Call::Status(_, _, StreamStatus::Live { .. })]),
        "{calls:?}"
    );
    assert!(rig.run(t0, 1..=600).is_empty(), "nothing after the shutdown");
    assert_eq!(rig.dial_count(), 1, "no re-dial of a stream that was shut down");
    assert!(matches!(rig.stream.subscribe_quotes(AAPL), Err(LiveDataError::Subscribe(_))));
}

/// A re-dial that was in flight when the stream was shut down gives its connection straight back.
#[test]
fn a_redial_that_lands_after_the_shutdown_is_given_back() {
    let (first, landed) = (Arc::new(Line::default()), Arc::new(Line::default()));
    // The stream's own `Shared`, which the dial reaches to model a shutdown that lands while the dial is out.
    let me: Arc<Mutex<Option<Arc<Shared>>>> = Arc::default();
    let dial: Dial = {
        let (first, landed, me) = (Arc::clone(&first), Arc::clone(&landed), Arc::clone(&me));
        Box::new(move |_sink, kind| {
            let line = match kind {
                DialKind::First => Arc::clone(&first),
                DialKind::Redial => {
                    let shared = me.lock().expect("me").clone().expect("built");
                    shared.lock().close(shared.sink.as_ref());
                    Arc::clone(&landed)
                }
            };
            let (asked, stopped, closed) = Default::default();
            Ok(Box::new(Scripted { line, asked, stopped, closed, refuse: false }))
        })
    };
    let mut stream = LazyQuotes::build(Arc::new(NoopSink), dial, Box::new(|| Ok(())), false);
    *me.lock().expect("me") = Some(Arc::clone(&stream.shared));
    stream.subscribe_quotes(AAPL).expect("dialled");
    first.dead.store(true, Ordering::SeqCst);
    let t0 = Instant::now();
    stream.shared.supervise(t0, NOON_MS);
    stream.shared.supervise(t0 + SELF_HEAL, NOON_MS + 20_000);
    assert!(
        landed.closed.load(Ordering::SeqCst),
        "the connection that landed after the close was given back"
    );
    assert!(stream.shared.lock().client.is_none(), "and none was installed");
    *me.lock().expect("me") = None; // the dial holds the stream's own state: let go of it
}

/// The supervisor THREAD: not started by a failed first dial (a box that never prices a stock has none), started by
/// the first connection, and stopped and joined at once by the shutdown (it does not sit out its tick).
#[test]
fn the_supervisor_thread_follows_the_connection() {
    let sink: Arc<dyn LiveDataSink> = Arc::new(NoopSink);
    let works = Arc::new(AtomicBool::new(false));
    let w = Arc::clone(&works);
    let dial: Dial = Box::new(move |_sink, _kind| {
        if !w.load(Ordering::SeqCst) {
            return Err(IbkrError::Unavailable);
        }
        let line = Arc::new(Line::default());
        let (asked, stopped, closed) = Default::default();
        Ok(Box::new(Scripted { line, asked, stopped, closed, refuse: false }))
    });
    let mut stream = LazyQuotes::new(sink, dial, Box::new(|| Ok(())));
    assert!(stream.subscribe_quotes(AAPL).is_err());
    assert!(stream.driver.is_none(), "a failed first dial starts no thread");
    works.store(true, Ordering::SeqCst);
    stream.shared.lock().failed = None;
    stream.subscribe_quotes(AAPL).expect("dialled");
    assert!(stream.driver.is_some(), "the first connection starts the supervisor");
    let began = Instant::now();
    stream.shutdown();
    assert!(stream.driver.is_none(), "joined");
    assert!(began.elapsed() < TICK * 3, "the shutdown woke the supervisor: {:?}", began.elapsed());
}
