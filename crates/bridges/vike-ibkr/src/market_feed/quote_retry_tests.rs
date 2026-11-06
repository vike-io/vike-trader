//! The quote loop over a RECORDING wire: what was sent to the gateway, in order (the market-data type before EVERY open),
//! what the sink was told, and when the next open was made. Nothing here opens a socket; the schedule is the
//! milliseconds of [`FAST`], and every bound is a LOWER bound (a slow machine only waits longer).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ibapi::{Error, Notice};
use vike_model::StreamStatus;

use super::*;
use crate::data_door::test_support::{Call, RecSink};

const SYMBOL: &str = "MSFT.SMART.USD";

const FAST: Plan = Plan {
    poll: Duration::from_millis(1),
    ladder: &[Duration::from_millis(5), Duration::from_millis(20), Duration::from_millis(40)],
    live_after: Duration::from_millis(15),
    disclose_at: 2,
};

fn raw(code: i32) -> Notice {
    Notice {
        request_id: None,
        code,
        message: "scripted".into(),
        error_time: None,
        advanced_order_reject_json: String::new(),
    }
}

fn notice(code: i32) -> Error {
    Error::Notice(raw(code))
}

/// One scripted OPEN: the looks it answers, then quiet for `quiet` looks; `stop_at_end` stops the pump when the last
/// scripted item has been handed out (the test's way of ending a loop that would otherwise run for ever).
struct Scripted {
    items: VecDeque<Next>,
    quiet: usize,
    stop_at_end: bool,
}

fn answers(items: Vec<Next>) -> Scripted {
    Scripted { items: items.into(), quiet: 0, stop_at_end: false }
}

fn then_quiet(quiet: usize) -> Scripted {
    Scripted { items: VecDeque::new(), quiet, stop_at_end: false }
}

fn ends_with(code: i32) -> Scripted {
    answers(vec![Next::Ended(notice(code))])
}

struct Drain {
    script: Scripted,
    stop: Arc<AtomicBool>,
}

impl QuoteDrain for Drain {
    fn next(&mut self, _wait: Duration) -> Next {
        if let Some(item) = self.script.items.pop_front() {
            if self.script.items.is_empty() && self.script.stop_at_end {
                self.stop.store(true, Ordering::SeqCst);
            }
            return item;
        }
        if self.script.quiet == 0 {
            self.stop.store(true, Ordering::SeqCst);
        } else {
            self.script.quiet -= 1;
        }
        Next::Quiet
    }
}

enum Step {
    Opens(Scripted),
    Refused(Error),
}

/// A wire that answers its opens from a script and RECORDS every call it got, with the instant.
struct RecWire {
    steps: Mutex<VecDeque<Step>>,
    log: Mutex<Vec<(&'static str, Instant)>>,
    stop: Arc<AtomicBool>,
}

impl QuoteWire for RecWire {
    fn apply_data_type(&self) {
        self.log.lock().unwrap().push(("type", Instant::now()));
    }

    fn open(&self) -> Result<Box<dyn QuoteDrain>, Error> {
        self.log.lock().unwrap().push(("open", Instant::now()));
        match self.steps.lock().unwrap().pop_front() {
            Some(Step::Opens(script)) => {
                Ok(Box::new(Drain { script, stop: Arc::clone(&self.stop) }))
            }
            Some(Step::Refused(e)) => Err(e),
            None => {
                // The script is spent: end the run (a bad subscribe ends the pump as it always did).
                self.stop.store(true, Ordering::SeqCst);
                Err(Error::Simple("script exhausted".into()))
            }
        }
    }
}

struct Outcome {
    log: Vec<(&'static str, Instant)>,
    calls: Vec<Call>,
    lost: bool,
}

impl Outcome {
    fn kinds(&self) -> Vec<&'static str> {
        self.log.iter().map(|(k, _)| *k).collect()
    }

    fn opens(&self) -> Vec<Instant> {
        self.log.iter().filter(|(k, _)| *k == "open").map(|(_, at)| *at).collect()
    }

    fn statuses(&self) -> Vec<StreamStatus> {
        self.calls
            .iter()
            .filter_map(|c| match c {
                Call::Status(symbol, stream, status) => {
                    assert_eq!((symbol.as_str(), stream.as_str()), (SYMBOL, "quotes"));
                    Some(*status)
                }
                _ => None,
            })
            .collect()
    }
}

fn run(steps: Vec<Step>) -> Outcome {
    let stop = Arc::new(AtomicBool::new(false));
    let lost = AtomicBool::new(false);
    let sink = RecSink::arc();
    let wire = RecWire {
        steps: Mutex::new(steps.into()),
        log: Mutex::new(Vec::new()),
        stop: Arc::clone(&stop),
    };
    run_quotes(&wire, SYMBOL, sink.as_ref(), &stop, &lost, &FAST);
    let log = std::mem::take(&mut *wire.log.lock().unwrap());
    Outcome { log, calls: sink.take(), lost: lost.load(Ordering::SeqCst) }
}

/// THE FIX for the `[354]` after a rebuilt connection: the market-data type goes to the gateway before EVERY subscribe,
/// the first one included, so a connection `ibapi` rebuilt (a new session, real-time by default) is never asked for
/// real-time stock data.
#[test]
fn the_market_data_type_is_sent_before_every_open() {
    let out = run(vec![
        Step::Opens(ends_with(10197)),
        Step::Opens(ends_with(354)),
        Step::Opens(then_quiet(3)),
    ]);
    assert_eq!(out.kinds(), ["type", "open", "type", "open", "type", "open"]);
}

/// THE FIX for the dead lane: a stream IB ends with `[10197]` is opened AGAIN (the base ended the pump and left the
/// symbol dead for good), and the first refusal is a transient told to nobody but the log.
#[test]
fn a_stream_ended_by_a_competing_session_is_reopened_without_a_gap() {
    let out = run(vec![Step::Opens(ends_with(10197)), Step::Opens(then_quiet(30))]);
    assert_eq!(
        out.kinds(),
        ["type", "open", "type", "open"],
        "re-opened once, with the type sent again"
    );
    assert!(out.statuses().is_empty(), "one refusal discloses nothing: {:?}", out.statuses());
    assert!(!out.lost);
}

/// A refusal that comes back is a lane DOWN: the second one discloses `GapStart` (the door's refusal then says the
/// stream is down), and the re-opened stream standing past `live_after` closes it with the echo of its start.
#[test]
fn the_second_refusal_discloses_the_gap_and_a_standing_stream_closes_it() {
    let out = run(vec![
        Step::Opens(ends_with(354)),
        Step::Opens(ends_with(10197)),
        Step::Opens(then_quiet(60)),
    ]);
    let statuses = out.statuses();
    let [StreamStatus::GapStart { at_ts_ms }, StreamStatus::Live { gap_started_ts_ms }] =
        statuses.as_slice()
    else {
        panic!("a GapStart then a Live, got {statuses:?}")
    };
    assert_eq!(*gap_started_ts_ms, Some(*at_ts_ms), "the Live names the gap it closes");
    assert_eq!(out.kinds().iter().filter(|k| **k == "open").count(), 3);
}

/// The ladder goes up and stays: the waits after the 1st, 2nd and 3rd refusal are the plan's, the 4th repeats the last.
#[test]
fn a_refusal_that_keeps_coming_waits_the_ladder() {
    let out = run(vec![
        Step::Opens(ends_with(10197)),
        Step::Opens(ends_with(10197)),
        Step::Opens(ends_with(10197)),
        Step::Opens(ends_with(10197)),
        Step::Opens(then_quiet(5)),
    ]);
    let opens = out.opens();
    assert_eq!(opens.len(), 5);
    for (n, want) in [5u64, 20, 40, 40].into_iter().enumerate() {
        let waited = opens[n + 1].duration_since(opens[n]);
        assert!(
            waited >= Duration::from_millis(want),
            "wait {n} was {waited:?}, the ladder says {want} ms"
        );
    }
}

/// A pump STOPPED with its gap open closes it: a window nobody closes trips the link dead-man after its grace.
#[test]
fn a_stopped_pump_closes_the_gap_it_told() {
    let mut second = ends_with(10197);
    second.stop_at_end = true;
    let out = run(vec![Step::Opens(ends_with(10197)), Step::Opens(second)]);
    let statuses = out.statuses();
    assert!(
        matches!(
            statuses.as_slice(),
            [StreamStatus::GapStart { .. }, StreamStatus::Live { gap_started_ts_ms: Some(_) }]
        ),
        "{statuses:?}"
    );
}

/// A refusal that arrives as the answer to the subscribe itself is handled the same way.
#[test]
fn a_refused_subscribe_is_retried_too() {
    let out = run(vec![Step::Refused(notice(354)), Step::Opens(then_quiet(3))]);
    assert_eq!(out.kinds(), ["type", "open", "type", "open"]);
}

/// A re-open that meets a connection `ibapi` is rebuilding fails with a dead-connection error: the pump ends with `lost`
/// raised (the supervisor's), never quietly with a dead stream nothing re-dials.
#[test]
fn a_subscribe_that_meets_a_dead_connection_raises_lost() {
    let out = run(vec![Step::Opens(ends_with(10197)), Step::Refused(Error::ConnectionReset)]);
    assert!(out.lost);
    assert_eq!(out.kinds(), ["type", "open", "type", "open"]);
}

/// A lost CONNECTION is the supervisor's: the pump ends with `lost` raised and says no `Live` from here (the
/// supervisor's own outage window is open and its re-dial closes it), even with a gap of its own open.
#[test]
fn a_lost_connection_ends_the_pump_for_the_supervisor_and_closes_nothing() {
    let out = run(vec![
        Step::Opens(ends_with(10197)),
        Step::Opens(ends_with(10197)),
        Step::Opens(answers(vec![Next::Ended(Error::ConnectionReset)])),
    ]);
    assert!(out.lost, "the connection is gone");
    assert_eq!(
        out.kinds().iter().filter(|k| **k == "open").count(),
        3,
        "not re-opened by the pump"
    );
    let statuses = out.statuses();
    assert!(
        matches!(statuses.as_slice(), [StreamStatus::GapStart { .. }]),
        "no Live: {statuses:?}"
    );

    let out = run(vec![Step::Opens(answers(vec![Next::Notice(raw(1100))]))]);
    assert!(out.lost, "a link-gone notice is a lost connection");
}

/// Every other end is as it was: a refused contract (200) ends the pump quietly, it is not re-opened and not a lost
/// connection; an advisory notice leaves the stream open.
#[test]
fn an_end_that_is_no_market_data_refusal_ends_as_it_did() {
    let out = run(vec![Step::Opens(ends_with(200)), Step::Opens(then_quiet(3))]);
    assert_eq!(out.kinds(), ["type", "open"], "a bad contract is not asked again");
    assert!(!out.lost && out.statuses().is_empty());

    let mut open = answers(vec![Next::Notice(raw(2104))]);
    open.quiet = 3;
    let out = run(vec![Step::Opens(open)]);
    assert_eq!(out.kinds().iter().filter(|k| **k == "open").count(), 1, "an advisory ends nothing");
}

/// Which ends are a market-data refusal, and what the operator reads.
#[test]
fn the_refusals_are_named_in_words() {
    for code in [354, 10168, 10197] {
        let r = data_refusal(&notice(code)).unwrap_or_else(|| panic!("{code} is a refusal"));
        assert_eq!(r.code, code);
    }
    for e in [
        notice(200),
        notice(2103),
        notice(1100),
        notice(326),
        Error::ConnectionReset,
        Error::Cancelled,
    ] {
        assert!(data_refusal(&e).is_none(), "{e:?} is not a market-data refusal");
    }
    let competing = refusal_words(&Refusal {
        code: 10197,
        message: "No market data during competing live session".into(),
    });
    assert!(competing.contains("[10197]") && competing.contains("another login"), "{competing}");
    let unsubscribed = refusal_words(&Refusal {
        code: 354,
        message: "Requested market data is not subscribed.".into(),
    });
    assert!(
        unsubscribed.contains("DELAYED") && unsubscribed.contains("reconnect"),
        "{unsubscribed}"
    );
}
