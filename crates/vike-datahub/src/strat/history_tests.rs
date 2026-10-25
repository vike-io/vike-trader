//! The start history's contracts with no socket and no real store: the pure pieces (read, splice, span,
//! compare, prepare), the saver behind `StratSink`'s two bar verbs, the shadow, and the served preload. The
//! lane over a real socket is `crates/vike-datahub/tests/start_history.rs`, and the real store's half (the
//! kline ingest, the name, the volume) is `crates/vike-datahub/tests/start_history_store.rs`.
//!
//! Every test that names `Core` runs the series rule of `vike-core`'s `Ingest::BarSeed` and `Ingest::BarClose`
//! (a seed REPLACES the series whole; a close of the last time replaces it, an older one is dropped, a newer
//! one is appended), because the harm this module exists to avoid - the core losing its newest bar - is the
//! consequence of that rule, not of anything in this crate.

use std::sync::atomic::AtomicBool;

use vike_data::{MemHistStore, RecordingSink, SinkCall};

use super::*;
use crate::feeds::BrokerLane;
use crate::feeds::routing::StratKeys;
use crate::strat::sink::StratSink;

const STEP: i64 = 60_000;
/// A minute boundary: `BASE / STEP` is whole.
const BASE: i64 = 1_760_000_040_000;
const WAIT: Duration = Duration::from_secs(5);

/// The bar that opens `i` minutes after [`BASE`]; its values follow `i`, so two different bars differ.
fn bar_at(i: i64) -> Bar {
    let f = i as f64 / 1000.0;
    Bar {
        ts: BASE + i * STEP,
        open: 1.0 + f,
        high: 1.1 + f,
        low: 0.9 + f,
        close: 1.05 + f,
        volume: 10.0 + i as f64,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn bars(from: i64, to: i64) -> Vec<Bar> {
    (from..to).map(bar_at).collect()
}

/// The clock `k` whole minutes after [`BASE`] and thirty seconds: the bar that opened at minute `k - 1` is the
/// newest CLOSED one, the one at minute `k` is forming.
fn clock_at(k: i64) -> i64 {
    BASE + k * STEP + 30_000
}

fn put(store: &MemHistStore, symbol: &str, bars: &[Bar]) {
    store.append_bars("oanda", symbol, "1m", bars, None).expect("the double appends");
}

fn stored(store: &MemHistStore, symbol: &str) -> Vec<Bar> {
    store.load_bars("oanda", symbol, "1m", TsRange::all()).expect("the double reads")
}

fn times(bars: &[Bar]) -> Vec<i64> {
    bars.iter().map(|b| (b.ts - BASE) / STEP).collect()
}

// ---- the series rule of the core ------------------------------------------------------------------------------

#[derive(Default)]
struct Core {
    closed: Vec<Bar>,
}

impl Core {
    fn seed(&mut self, bars: Vec<Bar>) {
        self.closed = bars;
    }

    fn close(&mut self, bar: Bar) {
        match self.closed.last().map(|b| b.ts) {
            Some(t) if bar.ts == t => *self.closed.last_mut().expect("a last bar") = bar,
            Some(t) if bar.ts < t => {}
            _ => self.closed.push(bar),
        }
    }

    fn newest(&self) -> Option<i64> {
        self.closed.last().map(|b| (b.ts - BASE) / STEP)
    }
}

// ---- the pure pieces ---------------------------------------------------------------------------------------------

/// **A stored series reads as one row per time, ascending, closed only**: of two rows with one time the later
/// wins, and the bar whose interval has not ended is dropped even if something stored it.
#[test]
fn closed_ascending_keeps_one_row_per_time_the_later_wins_and_drops_the_open_bar() {
    let mut late = bar_at(2);
    late.close = 9.0;
    let shuffled = vec![bar_at(3), bar_at(2), bar_at(1), late.clone(), bar_at(4)];
    // Minute 4 opened at BASE + 4 steps and has not ended at 4.5 steps.
    let out = closed_ascending(shuffled, STEP, BASE + 4 * STEP + 30_000);
    assert_eq!(times(&out), vec![1, 2, 3], "ascending, one per time, the open bar 4 dropped");
    assert_eq!(out[1], late, "the later row of a time wins");
}

/// **The tail read returns the newest N closed bars** and nothing newer than the newest close.
#[test]
fn read_tail_returns_the_newest_n_closed_bars() {
    let store = MemHistStore::new();
    put(&store, "EURUSD", &bars(0, 20));
    // At 12.5 steps the newest closed bar opened at minute 11.
    let tail = read_tail(&store, "oanda", "EURUSD", "1m", 5, clock_at(12)).expect("reads");
    assert_eq!(times(&tail), vec![7, 8, 9, 10, 11]);
    assert!(
        read_tail(&store, "oanda", "EURUSD", "1w", 5, clock_at(12)).is_err(),
        "an interval with no width is not read"
    );
    let none = read_tail(&store, "oanda", "GBPUSD", "1m", 5, clock_at(12)).expect("unknown series");
    assert!(none.is_empty());
}

/// **A splice is a union with no duplicate**, the later part winning a time.
#[test]
fn a_splice_is_a_union_with_no_duplicate_and_the_later_part_wins() {
    let stored_part = bars(0, 6);
    let mut venue_part = bars(4, 9);
    venue_part[0].close = 7.0; // minute 4 differs: the venue is the later part
    let out = splice(&[stored_part.as_slice(), venue_part.as_slice()], 100);
    assert_eq!(times(&out), vec![0, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(out[4].close, 7.0, "the venue's bar wins minute 4");
    let newest_three = splice(&[stored_part.as_slice(), venue_part.as_slice()], 3);
    assert_eq!(times(&newest_three), vec![6, 7, 8]);
    assert!(splice(&[], 10).is_empty());
}

/// **The missing span** is empty when the stored tail is the newest close, starts right after a tail that is
/// behind, and is the whole N when nothing is stored.
#[test]
fn the_missing_span_starts_after_the_tail_and_is_whole_for_an_empty_store() {
    let now = clock_at(10); // the newest closed bar is minute 9
    assert_eq!(missing_span(&bars(0, 10), STEP, now, 600), None);
    assert_eq!(
        missing_span(&bars(0, 7), STEP, now, 600),
        Some((bar_at(7).ts, upto(10))),
        "minutes 7, 8 and 9 are missing; the span ends at the latest time a closed bar can have opened"
    );
    assert_eq!(
        missing_span(&bars(0, 11), STEP, now, 600),
        None,
        "a tail in the future misses nothing"
    );
    assert_eq!(missing_span(&[], STEP, now, 4), Some((upto(10) - 3 * STEP, upto(10))));
    // A store that ended long ago is asked for the newest `keep` slots and no more.
    assert_eq!(
        missing_span(&bars(0, 2), STEP, now, 4),
        Some((upto(10) - 3 * STEP, upto(10))),
        "the venue is not asked for minutes 2 to 5: four slots are all a start history holds"
    );
}

/// The latest open time a closed bar can have when the clock reads [`clock_at`]`(k)`: `now - step`.
fn upto(k: i64) -> i64 {
    clock_at(k) - STEP
}

// ---- the comparison ---------------------------------------------------------------------------------------------

/// **Equal series compare equal**, and a store behind the venue says by how many minutes.
#[test]
fn a_store_equal_to_the_venue_reports_no_difference_and_a_store_behind_says_by_how_much() {
    let report = compare_bars("oanda", "EURUSD", "1m", &bars(0, 10), &bars(0, 10));
    assert_eq!((report.overlap, report.equal), (10, 10));
    assert_eq!(report.first_difference, None);
    assert_eq!(report.gap_minutes, Some(0));

    let behind = compare_bars("oanda", "EURUSD", "1m", &bars(0, 8), &bars(3, 11));
    assert_eq!((behind.overlap, behind.equal), (5, 5));
    assert_eq!(behind.store_behind, 3, "minutes 8, 9 and 10 are newer than the store");
    assert_eq!(behind.gap_minutes, Some(3));
    assert_eq!(behind.first_difference, None, "the tail beyond the store is not a difference");
}

/// **The first difference is the earliest by time**, whether it is a column, a hole or an extra bar.
#[test]
fn the_first_difference_is_the_earliest_by_time_and_names_what_it_is() {
    let seed = bars(0, 10);
    let mut store = bars(0, 10);
    store[7].close = 5.0; // a column at minute 7
    store.remove(3); // a hole at minute 3
    let report = compare_bars("oanda", "EURUSD", "1m", &store, &seed);
    assert_eq!(report.store_lacks_inside, 1);
    assert_eq!(report.first_difference, Some(Difference::StoreLacks { ts: bar_at(3).ts }));
    assert_eq!((report.overlap, report.equal), (9, 8));

    let mut only_column = bars(0, 10);
    only_column[7].close = 5.0;
    let report = compare_bars("oanda", "EURUSD", "1m", &only_column, &seed);
    match report.first_difference {
        Some(Difference::Field { ts, field, store, venue }) => {
            assert_eq!((ts, field), (bar_at(7).ts, "close"));
            assert_eq!((store, venue), (5.0, bar_at(7).close));
        }
        other => panic!("expected a column difference, got {other:?}"),
    }

    let mut extra = bars(0, 10);
    let mut stray = bar_at(5);
    stray.ts += 1; // a bar the venue does not have, inside its span
    extra.push(stray);
    let report = compare_bars("oanda", "EURUSD", "1m", &extra, &seed);
    assert_eq!(report.store_extra, 1);
    assert!(matches!(report.first_difference, Some(Difference::VenueLacks { .. })));
}

/// **An empty store is all behind** and says the gap is unknown, not zero.
#[test]
fn an_empty_store_is_all_behind_and_its_gap_is_unknown() {
    let report = compare_bars("oanda", "EURUSD", "1m", &[], &bars(0, 4));
    assert_eq!((report.store_bars, report.overlap, report.store_behind), (0, 0, 4));
    assert_eq!(report.gap_minutes, None);
}

/// **The journal line is ONE plain line** carrying the numbers an operator reads.
#[test]
fn the_shadow_report_is_one_plain_line() {
    let mut store = bars(0, 8);
    store.remove(2);
    let line = compare_bars("oanda", "EURUSD", "1m", &store, &bars(0, 10)).to_string();
    assert!(!line.contains('\n'), "{line}");
    for needle in [
        "oanda EURUSD 1m",
        "holds 7 closed bars",
        "seeded 10",
        "both have 7, equal 7",
        "lacks 1 inside its span and 2 newer",
        "behind the venue by 2 min",
        "the store lacks this bar",
    ] {
        assert!(line.contains(needle), "missing {needle:?} in {line}");
    }
}

// ---- prepare: the DB-first path and its seams (R1 and R2) --------------------------------------------------------

/// A venue's history channel that knows a fixed set of bars, can be told to fail or to ignore the span it was
/// asked for, and remembers what it was asked.
struct Truth {
    bars: Vec<Bar>,
    fail: AtomicBool,
    raw: AtomicBool,
    asked: Mutex<Vec<(i64, i64)>>,
    delay: Duration,
}

impl Truth {
    fn with_delay(bars: Vec<Bar>, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            bars,
            fail: AtomicBool::new(false),
            raw: AtomicBool::new(false),
            asked: Mutex::new(Vec::new()),
            delay,
        })
    }

    fn of(bars: Vec<Bar>) -> Arc<Self> {
        Self::with_delay(bars, Duration::ZERO)
    }

    fn asked(&self) -> Vec<(i64, i64)> {
        self.asked.lock().expect("lock").clone()
    }
}

impl GapSource for Truth {
    fn fetch(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, String> {
        self.asked.lock().expect("lock").push((start_ms, end_ms));
        std::thread::sleep(self.delay);
        if self.fail.load(Ordering::SeqCst) {
            return Err("the venue refused".to_string());
        }
        let raw = self.raw.load(Ordering::SeqCst);
        Ok(self
            .bars
            .iter()
            .filter(|b| raw || (b.ts >= start_ms && b.ts <= end_ms))
            .cloned()
            .collect())
    }
}

fn prepared(store: &MemHistStore, gap: Option<&Arc<Truth>>, now: i64) -> Prepared {
    prepare(store, gap.map(|g| g.as_ref() as &dyn GapSource), "oanda", "EURUSD", "1m", now)
}

/// **A store that is current needs no fetch**: the gap source is not even asked, and nothing is a duplicate.
#[test]
fn a_current_store_needs_no_fetch_and_the_channel_is_not_asked() {
    let store = MemHistStore::new();
    put(&store, "EURUSD", &bars(0, 10));
    let truth = Truth::of(bars(0, 10));
    match prepared(&store, Some(&truth), clock_at(10)) {
        Prepared::Ready { seed, stored, fetched, gap } => {
            assert_eq!(times(&seed), (0..10).collect::<Vec<_>>());
            assert_eq!((stored, fetched, gap), (10, 0, None));
        }
        other => panic!("{other:?}"),
    }
    assert!(truth.asked().is_empty(), "never re-request what we already have");
}

/// **The seam: a store behind is completed by exactly the missing span** - no hole, no duplicate - and the
/// channel is asked for that span and nothing else.
#[test]
fn a_store_behind_is_completed_by_exactly_the_missing_span() {
    let store = MemHistStore::new();
    put(&store, "EURUSD", &bars(0, 7));
    let truth = Truth::of(bars(0, 10));
    match prepared(&store, Some(&truth), clock_at(10)) {
        Prepared::Ready { seed, stored, fetched, gap } => {
            assert_eq!(times(&seed), (0..10).collect::<Vec<_>>(), "contiguous, once each");
            assert_eq!((stored, fetched), (7, 3));
            assert_eq!(gap, Some((bar_at(7).ts, upto(10))));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(truth.asked(), vec![(bar_at(7).ts, upto(10))], "only the span the store lacked");
}

/// **A closed market answers the gap with nothing, and that is an answer**: the stored tail is the history.
#[test]
fn a_closed_market_answers_the_gap_with_nothing_and_the_stored_tail_is_the_history() {
    let store = MemHistStore::new();
    put(&store, "EURUSD", &bars(0, 7));
    let truth = Truth::of(bars(0, 7)); // the venue has no bar after minute 6
    match prepared(&store, Some(&truth), clock_at(10)) {
        Prepared::Ready { seed, fetched, .. } => {
            assert_eq!(times(&seed), (0..7).collect::<Vec<_>>());
            assert_eq!(fetched, 0);
        }
        other => panic!("{other:?}"),
    }
}

/// **R1, the rule itself.** A store whose tail is behind and a history channel that cannot answer (no channel,
/// or a failed fetch) must WITHHOLD: sending the stored tail would make a core that already holds the newest
/// bar lose it, because a seed replaces the series whole.
#[test]
fn a_missing_span_that_cannot_be_fetched_withholds_the_whole_history() {
    let store = MemHistStore::new();
    put(&store, "EURUSD", &bars(0, 7));
    let no_channel = prepared(&store, None, clock_at(10));
    assert!(
        matches!(&no_channel, Prepared::Withheld { why, stored: 7 } if why.contains("no history channel")),
        "{no_channel:?}"
    );
    let truth = Truth::of(bars(0, 10));
    truth.fail.store(true, Ordering::SeqCst);
    let failed = prepared(&store, Some(&truth), clock_at(10));
    assert!(
        matches!(&failed, Prepared::Withheld { why, .. } if why.contains("could not be fetched")),
        "{failed:?}"
    );
}

/// **R1 against the core's own rule.** The core holds bars up to minute 10. Where the store lost minute 9
/// INSIDE its span but kept its tail, the history is WITHHELD (it has a hole) and the core is left alone. Where
/// the store lost its TAIL (the saver dropped minutes 9 and 10), the channel completes it and the core keeps
/// its newest bar; with the channel dead nothing is sent, and the core keeps it too. A stub that sends the
/// stored tail regardless would leave the core at minute 8: the loss this test is the bite for.
#[test]
fn the_core_never_loses_its_newest_bar_to_a_start_history() {
    let now = clock_at(11); // the newest closed bar is minute 10

    let mut core = Core::default();
    core.seed(bars(0, 11));
    let hole_inside = MemHistStore::new();
    put(&hole_inside, "EURUSD", &bars(0, 9));
    put(&hole_inside, "EURUSD", &[bar_at(10)]);
    match prepared(&hole_inside, None, now) {
        Prepared::Withheld { why, .. } => assert!(why.contains("missing inside"), "{why}"),
        other => panic!("a history with a hole is withheld: {other:?}"),
    }
    assert_eq!(core.newest(), Some(10), "nothing was sent, so the core keeps its series");

    let lost_tail = MemHistStore::new();
    put(&lost_tail, "EURUSD", &bars(0, 9)); // minutes 9 and 10 never reached the store
    let truth = Truth::of(bars(0, 11));
    let mut core = Core::default();
    core.seed(bars(0, 11));
    match prepared(&lost_tail, Some(&truth), now) {
        Prepared::Ready { seed, .. } => core.seed(seed),
        Prepared::Withheld { why, .. } => panic!("the channel answers, so it is ready: {why}"),
    }
    assert_eq!(core.newest(), Some(10), "the channel completed the tail");
    // The next live close joins the seeded series with no hole and no duplicate at the seam.
    core.close(bar_at(11));
    core.close(bar_at(11)); // a reconnect overlap re-closes the same time: replaced, not appended
    assert_eq!(times(&core.closed), (0..12).collect::<Vec<_>>());

    truth.fail.store(true, Ordering::SeqCst);
    let mut core = Core::default();
    core.seed(bars(0, 11));
    if let Prepared::Ready { seed, .. } = prepared(&lost_tail, Some(&truth), now) {
        core.seed(seed);
    }
    assert_eq!(core.newest(), Some(10), "a withheld history leaves the core's series alone");
}

/// **An empty store is filled from the channel alone**, and an empty store with an empty answer has nothing to
/// give.
#[test]
fn an_empty_store_is_filled_from_the_channel_and_nothing_at_all_withholds() {
    let store = MemHistStore::new();
    let truth = Truth::of(bars(0, 10));
    match prepared(&store, Some(&truth), clock_at(10)) {
        Prepared::Ready { seed, stored, fetched, .. } => {
            assert_eq!(times(&seed), (0..10).collect::<Vec<_>>());
            assert_eq!((stored, fetched), (0, 10));
        }
        other => panic!("{other:?}"),
    }
    let empty = Truth::of(Vec::new());
    assert!(matches!(
        prepared(&store, Some(&empty), clock_at(10)),
        Prepared::Withheld { why, .. } if why.contains("holds no bar")
    ));
}

/// **A fetched bar outside the asked span, or still forming, never joins the history.**
#[test]
fn a_fetched_bar_outside_the_span_or_still_forming_is_dropped() {
    let store = MemHistStore::new();
    put(&store, "EURUSD", &bars(0, 7));
    // The channel ignores the span it was asked for and answers everything it has, the forming minute 10 too.
    let truth = Truth::of(bars(0, 11));
    truth.raw.store(true, Ordering::SeqCst);
    match prepared(&store, Some(&truth), clock_at(10)) {
        Prepared::Ready { seed, fetched, .. } => {
            assert_eq!(
                times(&seed),
                (0..10).collect::<Vec<_>>(),
                "the forming bar 10 is not there"
            );
            assert_eq!(fetched, 3, "only minutes 7, 8 and 9 were asked for");
        }
        other => panic!("{other:?}"),
    }
}

/// **A read that outlasts its budget is abandoned**, and a quick one returns its answer.
#[test]
fn a_slow_read_is_abandoned_at_the_budget_and_a_quick_one_answers() {
    assert_eq!(run_within(Duration::from_secs(2), || 7), Some(7));
    let started = Instant::now();
    let slow = run_within(Duration::from_millis(50), || {
        std::thread::sleep(Duration::from_secs(2));
        7
    });
    assert_eq!(slow, None);
    assert!(started.elapsed() < Duration::from_secs(1), "the caller did not wait for the work");
}

// ---- the saver behind StratSink ---------------------------------------------------------------------------------

/// The writer a test hands the saver: the existing ingest's commit-key shape over the in-memory double.
fn mem_writer(store: &Arc<MemHistStore>) -> BarWriter {
    let store = Arc::clone(store);
    Box::new(move |venue, symbol, interval, bar| {
        let step = step_of(interval).ok_or("no width")?;
        let key = format!("{venue}:{symbol}:{interval}:{}-{}", bar.ts, bar.ts + step - 1);
        store
            .append_bars(venue, symbol, interval, std::slice::from_ref(bar), Some(&key))
            .map_err(|e| e.to_string())
    })
}

struct Rig {
    sink: StratSink,
    session: Arc<RecordingSink>,
    session_dyn: Arc<dyn LiveDataSink>,
    history: Arc<StartHistory>,
    store: Arc<MemHistStore>,
}

fn keys(intervals: &[&str]) -> StratKeys {
    let mut keys = StratKeys::default();
    for i in intervals {
        keys.set(&BrokerLane::Bars((*i).to_string()), true);
    }
    keys
}

fn rig_over(store: Arc<MemHistStore>, serve: bool, gap: Option<Arc<Truth>>, now: i64) -> Rig {
    let reader: Arc<dyn HistStore + Send + Sync> = store.clone();
    let gap = gap.map(|g| g as Arc<dyn GapSource>);
    let history = StartHistory::spawn_with_clock(
        reader,
        mem_writer(&store),
        gap,
        serve,
        Arc::new(move || now),
    )
    .expect("the worker starts");
    let sink = StratSink::default();
    sink.attach_history(Arc::clone(&history));
    let session = Arc::new(RecordingSink::default());
    let session_dyn: Arc<dyn LiveDataSink> = session.clone();
    sink.set("oanda", "EURUSD", 1, &session_dyn, keys(&["1m", "5s"]));
    Rig { sink, session, session_dyn, history, store }
}

fn rig(serve: bool, gap: Option<Arc<Truth>>, now: i64) -> Rig {
    rig_over(Arc::new(MemHistStore::new()), serve, gap, now)
}

/// **A closed bar is saved once, at its close**, under the name the lane uses.
#[test]
fn a_closed_bar_is_saved_once_at_its_close() {
    let r = rig(false, None, clock_at(10));
    r.sink.close_bar("oanda", "EURUSD", "1m", bar_at(8));
    assert!(r.history.settle(WAIT));
    assert_eq!(stored(&r.store, "EURUSD"), vec![bar_at(8)]);
    assert!(stored(&r.store, "EUR_USD").is_empty(), "never under OANDA's own spelling");
    assert_eq!(r.history.stats(), StartHistoryStats { saved: 1, ..StartHistoryStats::default() });
}

/// **A forming bar is never saved**, nor a mark.
#[test]
fn a_forming_bar_is_never_saved() {
    let r = rig(false, None, clock_at(10));
    r.sink.forming_bar("oanda", "EURUSD", "1m", bar_at(9));
    r.sink.bar_close_tick("oanda", "EURUSD", 1.0, bar_at(9).ts);
    r.sink.mark_tick("oanda", "EURUSD", 1.0, bar_at(9).ts);
    assert!(r.history.settle(WAIT));
    assert!(stored(&r.store, "EURUSD").is_empty());
    assert_eq!(r.history.stats(), StartHistoryStats::default());
}

/// **A close nobody holds is not saved**: the history is what strategies were handed.
#[test]
fn a_close_no_session_holds_is_not_saved() {
    let r = rig(false, None, clock_at(10));
    r.sink.close_bar("oanda", "GBPUSD", "1m", bar_at(8)); // no session holds GBPUSD
    r.sink.forget(1);
    r.sink.close_bar("oanda", "EURUSD", "1m", bar_at(8)); // the session left
    assert!(r.history.settle(WAIT));
    assert!(stored(&r.store, "GBPUSD").is_empty() && stored(&r.store, "EURUSD").is_empty());
    assert_eq!(r.history.stats().saved, 0);
}

/// **Only a saved venue at a minute step and up is saved.**
#[test]
fn only_saved_venues_at_a_minute_step_and_up_are_saved() {
    let r = rig(false, None, clock_at(10));
    r.sink.set("binance", "BTCUSDT", 1, &r.session_dyn, keys(&["1m"]));
    r.sink.close_bar("binance", "BTCUSDT", "1m", bar_at(8)); // a venue that is not saved
    r.sink.close_bar("oanda", "EURUSD", "5s", bar_at(8)); // a step under a minute
    assert!(r.history.settle(WAIT));
    assert_eq!(r.history.stats().saved, 0);
    assert!(covers("oanda", "1m") && covers("oanda", "1d"));
    assert!(!covers("oanda", "5s") && !covers("binance", "1m"));
}

/// **Idempotent on restart**: the same close twice, and again after a restart (a second holder over the same
/// store), leaves ONE row - and the second write is counted as already stored, not as a loss.
#[test]
fn a_repeated_close_and_a_restart_leave_one_row() {
    let r = rig(false, None, clock_at(10));
    r.sink.close_bar("oanda", "EURUSD", "1m", bar_at(8));
    r.sink.close_bar("oanda", "EURUSD", "1m", bar_at(8));
    assert!(r.history.settle(WAIT));
    assert_eq!(stored(&r.store, "EURUSD").len(), 1);
    assert_eq!(
        r.history.stats(),
        StartHistoryStats { saved: 1, already_stored: 1, ..StartHistoryStats::default() }
    );

    let restarted = rig_over(Arc::clone(&r.store), false, None, clock_at(10));
    restarted.sink.close_bar("oanda", "EURUSD", "1m", bar_at(8)); // the bridge re-emits after a restart
    restarted.sink.close_bar("oanda", "EURUSD", "1m", bar_at(9)); // and goes on
    assert!(restarted.history.settle(WAIT));
    assert_eq!(times(&stored(&r.store, "EURUSD")), vec![8, 9], "no duplicate, no hole");
    assert_eq!(restarted.history.stats().failed, 0);
}

/// **A refused bar is a counted, announced hole, never a panic or a silent loss.**
#[test]
fn a_bar_the_store_refuses_is_counted() {
    let store = Arc::new(MemHistStore::new());
    let reader: Arc<dyn HistStore + Send + Sync> = store.clone();
    let writer: BarWriter = Box::new(|_, symbol, _, _| {
        if symbol.contains('/') { Err("a slash adds a partition level".to_string()) } else { Ok(1) }
    });
    let history = StartHistory::spawn(reader, writer, None, false).expect("starts");
    let sink = StratSink::default();
    sink.attach_history(Arc::clone(&history));
    let session: Arc<dyn LiveDataSink> = Arc::new(RecordingSink::default());
    sink.set("oanda", "EUR/USD", 1, &session, keys(&["1m"]));
    sink.close_bar("oanda", "EUR/USD", "1m", bar_at(8));
    assert!(history.settle(WAIT));
    assert_eq!(history.stats().failed, 1);
    assert_eq!(history.stats().saved, 0);
}

/// **R2: a close the queue cannot take is counted and announced** - the saver never waits for the store and
/// never blocks the venue's thread.
#[test]
fn a_full_save_queue_counts_the_dropped_close_and_never_blocks() {
    let store = Arc::new(MemHistStore::new());
    let reader: Arc<dyn HistStore + Send + Sync> = store.clone();
    let (release, gate) = mpsc::channel::<()>();
    let writer: BarWriter = Box::new(move |_, _, _, _| {
        let _ = gate.recv(); // blocks until the test lets go (or drops the sender)
        Ok(1)
    });
    let history = StartHistory::spawn(reader, writer, None, false).expect("starts");
    let sink = StratSink::default();
    sink.attach_history(Arc::clone(&history));
    let session: Arc<dyn LiveDataSink> = Arc::new(RecordingSink::default());
    sink.set("oanda", "EURUSD", 1, &session, keys(&["1m"]));
    let sent = QUEUE_CAP + 10;
    let started = Instant::now();
    for i in 0..sent {
        sink.close_bar("oanda", "EURUSD", "1m", bar_at(i64::try_from(i).expect("small")));
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the venue's thread never waited on the store"
    );
    // One job is in the blocked writer's hand and QUEUE_CAP wait in the queue: the rest are dropped.
    assert!(history.stats().dropped >= 9, "{:?}", history.stats());
    drop(release);
    assert!(history.settle(WAIT));
}

// ---- the shadow -------------------------------------------------------------------------------------------------

fn seeds_of(rec: &RecordingSink) -> Vec<Vec<Bar>> {
    rec.recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::SeedBars { bars, .. } => Some(bars),
            _ => None,
        })
        .collect()
}

/// **The venue's seed is compared with the store ONCE per subscription and goes on to the session
/// UNCHANGED.**
#[test]
fn the_venue_seed_is_compared_once_and_goes_on_to_the_session_unchanged() {
    let r = rig(false, None, clock_at(12));
    put(&r.store, "EURUSD", &bars(0, 10));
    let seed = bars(5, 12);
    r.sink.seed_bars("oanda", "EURUSD", "1m", seed.clone());
    r.sink.close_bar("oanda", "EURUSD", "1m", bar_at(12)); // a close after the seed: no second report
    assert!(r.history.settle(WAIT));

    assert_eq!(
        seeds_of(&r.session),
        vec![seed],
        "the session received the venue's seed, bar for bar"
    );
    let reports = r.history.reports();
    assert_eq!(reports.len(), 1, "one line per subscription");
    let report = &reports[0];
    assert_eq!((report.store_bars, report.venue_bars), (10, 7));
    assert_eq!((report.overlap, report.equal, report.store_behind), (5, 5, 2));
    assert_eq!(report.gap_minutes, Some(2));
    assert_eq!(report.first_difference, None);
    // The comparison is of the store AS IT WAS; the two seed bars it lacked are saved after it.
    assert_eq!(report.repaired, 2);
    assert_eq!(times(&stored(&r.store, "EURUSD")), (0..13).collect::<Vec<_>>());
    assert_eq!(r.history.stats().repaired, 2);
}

/// **Shadow delivers NOTHING different**: the same calls through a sink with and without the start history
/// give a session the same frames, in the same order.
#[test]
fn a_shadowed_sink_hands_its_sessions_exactly_what_a_plain_one_does() {
    let drive = |sink: &StratSink| {
        sink.seed_bars("oanda", "EURUSD", "1m", bars(0, 6));
        sink.forming_bar("oanda", "EURUSD", "1m", bar_at(6));
        sink.close_bar("oanda", "EURUSD", "1m", bar_at(6));
        sink.close_bar("oanda", "EURUSD", "5s", bar_at(7));
        sink.stream_status(
            "oanda",
            "EURUSD",
            "1m",
            vike_model::StreamStatus::Live { gap_started_ts_ms: None },
        );
        sink.bar_close_tick("oanda", "EURUSD", 1.0, bar_at(6).ts);
    };
    let shadowed = rig(false, None, clock_at(8));
    drive(&shadowed.sink);
    assert!(shadowed.history.settle(WAIT));

    let plain = StratSink::default();
    let plain_session = Arc::new(RecordingSink::default());
    let plain_dyn: Arc<dyn LiveDataSink> = plain_session.clone();
    plain.set("oanda", "EURUSD", 1, &plain_dyn, keys(&["1m", "5s"]));
    drive(&plain);
    assert_eq!(shadowed.session.calls(), plain_session.calls());
}

/// **The shadow names a hole in the store**, which is what a later wave reads before it lets the store serve.
#[test]
fn the_shadow_names_a_hole_in_the_store() {
    let r = rig(false, None, clock_at(12));
    let mut held = bars(0, 12);
    held.remove(6);
    put(&r.store, "EURUSD", &held);
    r.sink.seed_bars("oanda", "EURUSD", "1m", bars(0, 12));
    assert!(r.history.settle(WAIT));
    let report = &r.history.reports()[0];
    assert_eq!(report.store_lacks_inside, 1);
    assert_eq!(report.holes_inside, 1, "the store's own series has one bar missing inside it");
    assert_eq!(report.first_difference, Some(Difference::StoreLacks { ts: bar_at(6).ts }));
    // ...and the venue's seed fills the hole: saved after the comparison, whole from then on.
    assert_eq!(report.repaired, 1);
    assert_eq!(times(&stored(&r.store, "EURUSD")), (0..12).collect::<Vec<_>>());
}

// ---- the served preload ------------------------------------------------------------------------------------------

/// **With the flag off, the preload does nothing at all**: no seed, no stash, no time spent.
#[test]
fn with_the_flag_off_the_preload_does_nothing() {
    let r = rig(false, Some(Truth::of(bars(0, 10))), clock_at(10));
    put(&r.store, "EURUSD", &bars(0, 7));
    let spent =
        r.history.preload(1, "oanda", "EURUSD", "1m", PRELOAD_KEY_BUDGET, r.session_dyn.as_ref());
    assert_eq!(spent, Duration::ZERO);
    assert!(seeds_of(&r.session).is_empty());
    assert_eq!(r.history.waiting(), 0);
    assert!(!r.history.serves());
    // ...and the venue's seed passes through untouched.
    let venue_seed = bars(3, 5);
    let through = r.history.under_venue_seed(1, "oanda", "EURUSD", "1m", &venue_seed);
    assert_eq!(times(&through), vec![3, 4]);
}

/// **With the flag on, the store and the missing span go out as a seed BEFORE the venue is asked**, and the
/// venue's own (shorter) seed is spliced over it afterwards, so it never shrinks what the strategy was given.
#[test]
fn serving_sends_the_start_history_first_and_the_venue_seed_never_shrinks_it() {
    let truth = Truth::of(bars(0, 10));
    let r = rig(true, Some(Arc::clone(&truth)), clock_at(10));
    put(&r.store, "EURUSD", &bars(0, 7));
    let spent =
        r.history.preload(1, "oanda", "EURUSD", "1m", PRELOAD_KEY_BUDGET, r.session_dyn.as_ref());
    assert!(spent > Duration::ZERO);
    let first = seeds_of(&r.session);
    assert_eq!(first.len(), 1);
    assert_eq!(
        times(&first[0]),
        (0..10).collect::<Vec<_>>(),
        "the store's seven and the fetched three"
    );
    assert_eq!(truth.asked(), vec![(bar_at(7).ts, upto(10))], "only the missing span was fetched");
    assert_eq!(r.history.waiting(), 1);

    // The venue seeds 300 bars in real life; here a short one whose newest bar differs (the venue wins).
    let mut venue = bars(8, 10);
    venue[1].close = 3.0;
    r.sink.seed_bars("oanda", "EURUSD", "1m", venue);
    assert!(r.history.settle(WAIT));
    let seeds = seeds_of(&r.session);
    assert_eq!(seeds.len(), 2, "the session got the preloaded seed, then the spliced one");
    assert_eq!(
        times(&seeds[1]),
        (0..10).collect::<Vec<_>>(),
        "the venue's seed did not shrink the history"
    );
    assert_eq!(seeds[1][9].close, 3.0, "and the venue's bar won its time");
    assert_eq!(r.history.waiting(), 0, "a prepared history is spliced once");
    assert_eq!(r.history.reports().len(), 1, "the shadow still wrote its one comparison");
}

/// **A withheld history sends nothing**, leaves no stash, and the venue's seed passes untouched.
#[test]
fn a_withheld_history_sends_nothing_and_the_venue_seed_passes_untouched() {
    let truth = Truth::of(bars(0, 10));
    truth.fail.store(true, Ordering::SeqCst);
    let r = rig(true, Some(Arc::clone(&truth)), clock_at(10));
    put(&r.store, "EURUSD", &bars(0, 7));
    r.history.preload(1, "oanda", "EURUSD", "1m", PRELOAD_KEY_BUDGET, r.session_dyn.as_ref());
    assert!(
        seeds_of(&r.session).is_empty(),
        "R1: a tail that is behind and cannot be completed is not sent"
    );
    assert_eq!(r.history.waiting(), 0);
    r.sink.seed_bars("oanda", "EURUSD", "1m", bars(2, 4));
    let seeds = seeds_of(&r.session);
    assert_eq!(seeds.len(), 1);
    assert_eq!(times(&seeds[0]), vec![2, 3], "today's behaviour: the venue's seed, as it sent it");
}

/// **A venue that is not saved is never preloaded**, even with the flag on.
#[test]
fn a_venue_that_is_not_saved_is_never_preloaded() {
    let r = rig(true, Some(Truth::of(bars(0, 10))), clock_at(10));
    let spent = r.history.preload(
        1,
        "binance",
        "BTCUSDT",
        "1m",
        PRELOAD_KEY_BUDGET,
        r.session_dyn.as_ref(),
    );
    assert_eq!(spent, Duration::ZERO);
    assert!(seeds_of(&r.session).is_empty());
}

/// **A history channel that outlasts the key's budget costs the session that budget and nothing more**, and
/// the strategy gets what the venue seeds.
#[test]
fn a_slow_channel_costs_the_session_its_budget_and_nothing_more() {
    let r = rig(true, Some(Truth::with_delay(bars(0, 10), Duration::from_secs(3))), clock_at(10));
    put(&r.store, "EURUSD", &bars(0, 7));
    let budget = Duration::from_millis(100);
    let spent = r.history.preload(1, "oanda", "EURUSD", "1m", budget, r.session_dyn.as_ref());
    assert!(spent >= budget && spent < Duration::from_secs(2), "{spent:?}");
    assert!(seeds_of(&r.session).is_empty());
    assert_eq!(r.history.waiting(), 0);
}

/// **A session's pending history is its own, and goes with it.** Two sessions on one key each get a preload
/// of their own and the venue's seed is joined under EACH one's, so the second does not take the first's
/// pending entry; a joiner never gets a venue seed (the key is live already), so its entry is dropped with its
/// session instead of waiting for ever.
#[test]
fn two_sessions_keep_their_own_pending_history_and_it_goes_with_the_session() {
    let truth = Truth::of(bars(0, 10));
    let r = rig(true, Some(Arc::clone(&truth)), clock_at(10));
    put(&r.store, "EURUSD", &bars(0, 7));
    let second = Arc::new(RecordingSink::default());
    let second_dyn: Arc<dyn LiveDataSink> = second.clone();
    r.sink.set("oanda", "EURUSD", 2, &second_dyn, keys(&["1m"]));
    r.history.preload(1, "oanda", "EURUSD", "1m", PRELOAD_KEY_BUDGET, r.session_dyn.as_ref());
    r.history.preload(2, "oanda", "EURUSD", "1m", PRELOAD_KEY_BUDGET, second_dyn.as_ref());
    assert_eq!(r.history.waiting(), 2, "the second session did not take the first's entry");

    let mut venue = bars(8, 10);
    venue[1].close = 3.0;
    r.sink.seed_bars("oanda", "EURUSD", "1m", venue);
    assert!(r.history.settle(WAIT));
    for rec in [&r.session, &second] {
        let seeds = seeds_of(rec);
        assert_eq!(seeds.len(), 2, "the preload, then the venue's seed joined over it");
        assert_eq!(times(&seeds[1]), (0..10).collect::<Vec<_>>());
        assert_eq!(seeds[1][9].close, 3.0);
    }
    assert_eq!(r.history.waiting(), 0);

    // The venue's seed saved its own two bars; the store's one remaining hole is filled so a history is whole.
    put(&r.store, "EURUSD", &[bar_at(7)]);
    r.history.preload(2, "oanda", "EURUSD", "1m", PRELOAD_KEY_BUDGET, second_dyn.as_ref());
    assert_eq!(r.history.waiting(), 1, "a joiner's pending entry, which no venue seed will take");
    r.sink.forget(2);
    assert_eq!(r.history.waiting(), 0, "…is dropped with its session");
}

/// **A symbol with an underscore is never saved, compared or preloaded.** `EUR_USD` is OANDA's own spelling,
/// the name of the operator-backfilled DAY-GRID series; per-bar keys written into it would store the bars the
/// two ingests share twice, and no verb retires a key. Skipped (counted, announced once), whichever door the
/// bar comes in by; the lane's own spelling beside it is saved as ever.
#[test]
fn a_symbol_spelled_with_an_underscore_is_never_saved() {
    let r = rig(true, Some(Truth::of(bars(0, 10))), clock_at(10));
    r.sink.set("oanda", "EUR_USD", 1, &r.session_dyn, keys(&["1m"]));
    r.sink.close_bar("oanda", "EUR_USD", "1m", bar_at(8));
    r.sink.seed_bars("oanda", "EUR_USD", "1m", bars(0, 8));
    let spent =
        r.history.preload(1, "oanda", "EUR_USD", "1m", PRELOAD_KEY_BUDGET, r.session_dyn.as_ref());
    assert!(r.history.settle(WAIT));
    assert_eq!(spent, Duration::ZERO, "no preload for it either");
    assert!(stored(&r.store, "EUR_USD").is_empty(), "nothing was written into the day-grid series");
    assert!(r.history.reports().is_empty(), "…nor compared");
    let stats = r.history.stats();
    assert_eq!(
        (stats.saved, stats.skipped),
        (0, 3),
        "the close, the seed and the preload: {stats:?}"
    );
    assert_eq!(
        seeds_of(&r.session).len(),
        1,
        "the venue's seed still went to the session, as it came"
    );

    r.sink.close_bar("oanda", "EURUSD", "1m", bar_at(8));
    assert!(r.history.settle(WAIT));
    assert_eq!(
        times(&stored(&r.store, "EURUSD")),
        vec![8],
        "the lane's own spelling is saved as ever"
    );
}

/// **At most `MAX_SAVED_KEYS` keys are saved**: the first ones seen win and a further key is skipped and
/// counted, preload included.
#[test]
fn a_key_past_the_most_the_saver_holds_is_neither_saved_nor_preloaded() {
    let r = rig(true, Some(Truth::of(bars(0, 10))), clock_at(10));
    let symbols = ["AAAUSD", "BBBUSD", "CCCUSD", "DDDUSD"];
    assert_eq!(MAX_SAVED_KEYS, 3);
    for s in symbols {
        r.sink.set("oanda", s, 1, &r.session_dyn, keys(&["1m"]));
        r.sink.close_bar("oanda", s, "1m", bar_at(8));
    }
    assert!(r.history.settle(WAIT));
    for s in &symbols[..3] {
        assert_eq!(stored(&r.store, s).len(), 1, "{s}");
    }
    assert!(stored(&r.store, "DDDUSD").is_empty(), "the fourth key is not saved");
    assert_eq!(r.history.stats().skipped, 1);
    let spent =
        r.history.preload(1, "oanda", "DDDUSD", "1m", PRELOAD_KEY_BUDGET, r.session_dyn.as_ref());
    assert_eq!(spent, Duration::ZERO, "…nor preloaded");
    assert!(seeds_of(&r.session).is_empty());
}

/// **A key a preload claimed goes back to the saver when its subscription fails**, unless a seed or a close
/// has shown it live: a refused mount must not hold one of the three slots until the datahub restarts.
#[test]
fn a_preloaded_key_whose_subscription_fails_gives_its_slot_back_unless_it_went_live() {
    let r = rig(true, Some(Truth::of(bars(0, 10))), clock_at(10));
    let take = |s: &str| {
        r.history.preload(1, "oanda", s, "1m", PRELOAD_KEY_BUDGET, r.session_dyn.as_ref());
    };
    for s in ["AAAUSD", "BBBUSD", "CCCUSD"] {
        take(s);
    }
    assert_eq!(seeds_of(&r.session).len(), 3, "three preloads claim the three slots");
    take("DDDUSD");
    assert_eq!(seeds_of(&r.session).len(), 3, "a fourth key finds no slot");

    r.sink.set("oanda", "BBBUSD", 1, &r.session_dyn, keys(&["1m"]));
    r.sink.close_bar("oanda", "BBBUSD", "1m", bar_at(8)); // BBBUSD is held live
    for s in ["AAAUSD", "BBBUSD"] {
        r.history.release_unproven("oanda", s, "1m"); // both subscriptions failed
    }
    take("DDDUSD");
    assert_eq!(seeds_of(&r.session).len(), 4, "AAAUSD's slot came back");
    take("EEEUSD");
    assert_eq!(seeds_of(&r.session).len(), 4, "…and BBBUSD, live, kept its own");
}

/// **The venue's seed bars the store lacks are saved after the comparison, the newest seed's worth at most,
/// and a second subscription saves nothing it already has**: a restart of the datahub, the gatewayhub or the
/// tradehub leaves a hole that the venue's seed covers, and this is what closes it.
#[test]
fn the_seed_bars_the_store_lacks_are_saved_the_newest_seeds_worth_at_most_and_once() {
    let r = rig(false, None, clock_at(400));
    r.sink.seed_bars("oanda", "EURUSD", "1m", bars(0, 320));
    assert!(r.history.settle(WAIT));
    let first = r.history.reports()[0].clone();
    assert_eq!((first.store_bars, first.venue_bars, first.repaired), (0, 320, REPAIR_CAP));
    let held = stored(&r.store, "EURUSD");
    assert_eq!(held.len(), REPAIR_CAP);
    assert_eq!(times(&held)[0], 20, "the OLDEST twenty are the ones left out");

    r.sink.seed_bars("oanda", "EURUSD", "1m", bars(20, 320));
    assert!(r.history.settle(WAIT));
    let second = r.history.reports()[1].clone();
    assert_eq!(
        (second.store_bars, second.overlap, second.equal, second.repaired),
        (300, 300, 300, 0)
    );
    assert_eq!(stored(&r.store, "EURUSD").len(), REPAIR_CAP, "nothing doubled");
    assert_eq!(r.history.stats().repaired, REPAIR_CAP as u64);
}

/// **`settle` is bounded when the queue is full**: a writer stuck in the store must not hang the daemon's
/// stop (a blocking send would wait for room that never comes).
#[test]
fn settle_gives_up_at_its_timeout_when_the_queue_is_full_and_the_writer_stuck() {
    let store = Arc::new(MemHistStore::new());
    let reader: Arc<dyn HistStore + Send + Sync> = store.clone();
    let (release, gate) = mpsc::channel::<()>();
    let writer: BarWriter = Box::new(move |_, _, _, _| {
        let _ = gate.recv();
        Ok(1)
    });
    let history = StartHistory::spawn(reader, writer, None, false).expect("starts");
    let sink = StratSink::default();
    sink.attach_history(Arc::clone(&history));
    let session: Arc<dyn LiveDataSink> = Arc::new(RecordingSink::default());
    sink.set("oanda", "EURUSD", 1, &session, keys(&["1m"]));
    for i in 0..(QUEUE_CAP + 5) {
        sink.close_bar("oanda", "EURUSD", "1m", bar_at(i64::try_from(i).expect("small")));
    }
    let started = Instant::now();
    assert!(!history.settle(Duration::from_millis(200)), "a stuck writer cannot settle");
    assert!(started.elapsed() < Duration::from_secs(3), "…and the wait was bounded");
    drop(release);
    assert!(history.settle(WAIT), "freed, it settles");
}

// ---- the grid, the holes and the read window ---------------------------------------------------------------------

/// **A bar on a grid that is not the epoch's keeps its newest closed bar.** OANDA's four-hour candles open on
/// a 17:00 New York grid, two hours off the epoch grid in winter: at half an hour after one closed, the epoch
/// grid says the newest closed bar opened two hours before the real one, the store's real newest bar is cut
/// off, and a running core would lose it to the seed. Bounded by `now - step` instead, it is kept.
#[test]
fn an_interval_off_the_epoch_grid_keeps_its_newest_closed_bar() {
    const H: i64 = 3_600_000;
    let z = BASE.div_euclid(4 * H) * 4 * H + 2 * H; // bar 0 opens two hours off the 4h epoch grid
    let bar4 = |k: i64| Bar { ts: z + k * 4 * H, ..bar_at(k) };
    let store = MemHistStore::new();
    let held: Vec<Bar> = (0..10).map(bar4).collect();
    store.append_bars("oanda", "EURUSD", "4h", &held, None).expect("the double appends");
    let now = z + 10 * 4 * H + 30 * 60_000; // thirty minutes after bar 9 closed

    let tail = read_tail(&store, "oanda", "EURUSD", "4h", 600, now).expect("reads");
    assert_eq!(tail.last().map(|b| b.ts), Some(bar4(9).ts), "the newest closed bar is read");
    assert_eq!(missing_span(&tail, 4 * H, now, 600), None, "…and nothing is missing after it");
    match prepare(&store, None, "oanda", "EURUSD", "4h", now) {
        Prepared::Ready { seed, .. } => assert_eq!(seed.last().map(|b| b.ts), Some(bar4(9).ts)),
        other => panic!("{other:?}"),
    }
}

/// **Missing slots are counted between consecutive bars and a shifted grid is not a hole**: a daily candle
/// after a twenty-five hour day, a four-hour candle after a one-hour shift of the venue's grid, are whole.
#[test]
fn holes_inside_counts_the_missing_slots_and_forgives_a_shifted_grid() {
    const H: i64 = 3_600_000;
    assert_eq!(holes_inside(&[], STEP), 0);
    assert_eq!(holes_inside(&bars(0, 5), STEP), 0);
    let mut gappy = bars(0, 6);
    gappy.remove(3);
    gappy.remove(3);
    assert_eq!(holes_inside(&gappy, STEP), 2);
    let at = |ts: i64| Bar { ts, ..bar_at(0) };
    assert_eq!(holes_inside(&[at(0), at(25 * H)], 24 * H), 0, "a long day is not a hole");
    assert_eq!(holes_inside(&[at(0), at(5 * H)], 4 * H), 0, "a shifted grid is not a hole");
    assert_eq!(holes_inside(&[at(0), at(8 * H)], 4 * H), 1, "a lost four-hour candle is one");
}

/// **A bar missing INSIDE the stored span withholds the history** (a closed market counts too: the venue's
/// own seed serves instead), and the journal line counts the store's holes.
#[test]
fn a_hole_in_the_middle_of_the_stored_span_withholds_the_history_and_the_shadow_counts_it() {
    let store = MemHistStore::new();
    let mut held = bars(0, 10);
    held.remove(5);
    put(&store, "EURUSD", &held);
    match prepared(&store, None, clock_at(10)) {
        Prepared::Withheld { why, stored } => {
            assert!(why.contains("1 bars are missing inside"), "{why}");
            assert_eq!(stored, 9);
        }
        other => panic!("{other:?}"),
    }
    let report = compare_bars("oanda", "EURUSD", "1m", &held, &bars(0, 10));
    assert_eq!(report.holes_inside, 1);
    assert!(report.to_string().contains("holes inside: 1"), "{report}");
}

/// **A store that ended before the history's horizon joins nothing across a gap nobody asked about**: only the
/// newest slots are asked for, and the old bars are dropped rather than glued to them.
#[test]
fn a_store_older_than_the_horizon_adds_no_hole_between_itself_and_the_fetched_bars() {
    let store = MemHistStore::new();
    put(&store, "EURUSD", &bars(0, 3));
    let truth = Truth::of(bars(1000, 2000));
    match prepared(&store, Some(&truth), clock_at(2000)) {
        Prepared::Ready { seed, stored, fetched, .. } => {
            assert_eq!(stored, 3, "it did read the old bars");
            assert_eq!(fetched, seed.len(), "…and the history is the fetched ones alone");
            assert_eq!((times(&seed)[0], *times(&seed).last().expect("bars")), (1401, 1999));
        }
        other => panic!("{other:?}"),
    }
}

/// **A weekend does not empty the read of a one-minute key**: forty-four hours after the newest stored bar
/// (a Saturday evening) the bars are still read, twenty hours' window or not (the shadow line compares them and
/// the seed's repair needs them). The start history itself covers the newest six hundred SLOTS, ten hours of a
/// one-minute key: a closed market that answers those slots with nothing leaves it nothing to serve, and it is
/// withheld, so the venue's own seed serves.
#[test]
fn a_weekend_does_not_empty_the_read_of_a_one_minute_key() {
    const H: i64 = 3_600_000;
    let store = MemHistStore::new();
    put(&store, "EURUSD", &bars(0, 50));
    let now = bar_at(49).ts + 44 * H + 30_000;
    let tail = read_tail(&store, "oanda", "EURUSD", "1m", 600, now).expect("reads");
    assert_eq!(tail.len(), 50, "the weekend is inside the read window");
    let closed = Truth::of(bars(0, 50)); // the venue has nothing after minute 49
    match prepared(&store, Some(&closed), now) {
        Prepared::Withheld { why, stored } => {
            assert!(why.contains("holds no bar"), "{why}");
            assert_eq!(
                stored, 50,
                "the stored bars were read, and are beyond the history's horizon"
            );
        }
        other => panic!("{other:?}"),
    }
}

/// **A real store reads back the bars once and in order after a compaction pass**, and a repeat of a merged
/// bar is still the no-op (the commit key went into the merged part with its rows): the store-side half of
/// "idempotent on restart", asked of the engine itself and not of the double.
#[cfg(feature = "backfill-serve")]
#[test]
fn a_real_store_reads_back_the_bars_once_and_in_order_after_a_compaction_pass() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let store = Arc::new(vike_store::DataFusionHist::open(dir.path()).expect("the store opens"));
    let write = ingest_writer(Arc::clone(&store));
    for i in 0..20 {
        assert_eq!(write("oanda", "EURUSD", "1m", &bar_at(i)), Ok(1));
    }
    store.run_maintenance(&vike_store::MaintenanceConfig::default()).expect("a pass");
    let now = clock_at(20);
    let tail = read_tail(store.as_ref(), "oanda", "EURUSD", "1m", 600, now).expect("reads");
    assert_eq!(times(&tail), (0..20).collect::<Vec<_>>(), "once each, ascending, in this process");
    match prepare(store.as_ref(), None, "oanda", "EURUSD", "1m", now) {
        Prepared::Ready { seed, .. } => assert_eq!(seed, tail),
        other => panic!("{other:?}"),
    }
    assert_eq!(write("oanda", "EURUSD", "1m", &bar_at(3)), Ok(0), "a merged bar is still spent");
    assert!(
        write("oanda", "EUR_USD", "1m", &bar_at(3)).is_err(),
        "the writer itself refuses the day-grid name"
    );
}

// ---- the hot path -----------------------------------------------------------------------------------------------

/// **Nothing is added per message**: of the sink's verbs only `seed_bars` (once per subscription) and
/// `close_bar` (once per bar close) name the start history. A quote, a trade, a book, a forming bar or a mark
/// cannot grow a call to it without this test going red.
#[test]
fn only_the_two_bar_verbs_of_the_sink_name_the_history() {
    let text = include_str!("sink.rs");
    let at = text.find("impl LiveDataSink for StratSink {").expect("the impl");
    let mut naming: Vec<&str> = Vec::new();
    for verb in text[at..].split("\n    fn ").skip(1) {
        if verb.contains("history") {
            naming.push(verb.split('(').next().expect("a name"));
        }
    }
    assert_eq!(naming, vec!["seed_bars", "close_bar"]);
}
