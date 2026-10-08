use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

// The shared scripted MarketStream double (testing-arch Phase 4c) — the canonical copy of the
// `Step`/`ScriptedStream` pair that used to live inline here. `send_text` records into a
// shareable log (`with_sent_log`) so a feed test can assert across sessions; the shared
// `clock` (`clocked`) advances before every read so consuming the script ages the ack
// watchdog with zero real sleeps.
use crate::scripted::{ScriptStep as Step, ScriptedStream};

fn opts_with(ack: Option<Duration>, idle: Option<Duration>) -> MarketPumpOpts<'static> {
    MarketPumpOpts {
        subscribe: Some(r#"{"op":"subscribe"}"#),
        keepalive: None,
        ack_timeout: ack,
        idle_threshold: idle,
        read_timeout: Duration::from_secs(2),
        // 1 stop-aware 100 ms tick — keeps tests fast
        backoff: PumpBackoff::Fixed(Duration::from_millis(1)),
        connect_timeout: None,
    }
}

/// The subscribe frame is replayed VERBATIM at the start of EVERY session — the
/// reconnect-resubscribes pin. Session 1 ends by exhaustion (a disconnect); the driver backs
/// off, reconnects (stream 2), and the shared send log shows the subscribe twice, in order,
/// before the second session's data flows.
#[test]
fn reconnect_resends_the_subscribe_each_session() {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let stop = AtomicBool::new(false);
    let errors = RefCell::new(Vec::<String>::new());
    let mut streams = VecDeque::from([
        ScriptedStream::from_steps(vec![Step::Text("data1".into())])
            .with_sent_log(Rc::clone(&sent)),
        ScriptedStream::from_steps(vec![Step::Text("stop-now".into())])
            .with_sent_log(Rc::clone(&sent)),
    ]);
    let connects = Cell::new(0);
    run_market_feed_on(
        || {
            connects.set(connects.get() + 1);
            streams.pop_front().ok_or_else(|| "no more scripted streams".to_string())
        },
        &opts_with(None, None),
        &stop,
        &|| 0,
        |txt| {
            if txt == "stop-now" {
                stop.store(true, Ordering::Relaxed);
            }
            FrameOutcome::Confirm
        },
        || {},
        |s| {
            if let SessionStatus::Error(e) = s {
                errors.borrow_mut().push(e.to_string())
            }
        },
    );
    assert_eq!(connects.get(), 2, "one reconnect after the first session's disconnect");
    let sent = sent.borrow();
    assert_eq!(
        sent.iter().filter(|s| s.contains("subscribe")).count(),
        2,
        "the subscribe frame is re-sent on the reconnected session: {sent:?}"
    );
    assert_eq!(
        errors.borrow().len(),
        1,
        "exactly the first session's disconnect was disclosed: {:?}",
        errors.borrow()
    );
}

/// A pre-raised stop never connects at all — the deterministic-teardown property at the feed
/// level.
#[test]
fn a_pre_raised_stop_never_connects() {
    let stop = AtomicBool::new(true);
    let connects = Cell::new(0);
    run_market_feed_on(
        || {
            connects.set(connects.get() + 1);
            Ok(ScriptedStream::from_steps(vec![]))
        },
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| FrameOutcome::Ignore,
        || {},
        |_| {},
    );
    assert_eq!(connects.get(), 0, "a pre-raised stop must not open a socket");
}

/// A stop raised mid-session ends the session `Ok` on the next read tick and the feed loop
/// exits WITHOUT disclosing an error or reconnecting — the prompt-exit pin.
#[test]
fn a_stop_mid_session_exits_without_reconnect() {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let stop = AtomicBool::new(false);
    let connects = Cell::new(0);
    let errors = Cell::new(0);
    let mut stream = Some(
        ScriptedStream::from_steps(vec![Step::Text("x".into()), Step::Timeout, Step::Timeout])
            .with_sent_log(Rc::clone(&sent)),
    );
    run_market_feed_on(
        || {
            connects.set(connects.get() + 1);
            stream.take().ok_or_else(|| "reconnected after a requested stop".to_string())
        },
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| {
            stop.store(true, Ordering::Relaxed);
            FrameOutcome::Confirm
        },
        || {},
        |s| {
            if matches!(s, SessionStatus::Error(_)) {
                errors.set(errors.get() + 1)
            }
        },
    );
    assert_eq!(connects.get(), 1, "no reconnect after a requested stop");
    assert_eq!(errors.get(), 0, "a requested stop is not an error");
}

/// br7: a subscribe that never acks and never delivers data trips the ack watchdog once the
/// window elapses — the exact attributable `"no subscribe ack/data within {N}s"` error, which
/// the feed loop discloses (then the test's error hook raises stop, so exactly one session
/// runs).
#[test]
fn an_unacked_subscribe_trips_the_watchdog_with_the_attributable_error() {
    let clock = Rc::new(Cell::new(0_i64));
    let now = {
        let c = Rc::clone(&clock);
        move || c.get()
    };
    let stop = AtomicBool::new(false);
    let errors = RefCell::new(Vec::<String>::new());
    // 3 s of clock per read tick; deadline = 5 s (armed at t=0): tick1 t=3s (fresh),
    // tick2 t=6s → overdue.
    let mut stream = Some(
        ScriptedStream::from_steps(vec![Step::Timeout, Step::Timeout])
            .clocked(Rc::clone(&clock), 3_000),
    );
    run_market_feed_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(Some(Duration::from_secs(5)), None),
        &stop,
        &now,
        |_| FrameOutcome::Confirm,
        || {},
        |s| {
            if let SessionStatus::Error(e) = s {
                errors.borrow_mut().push(e.to_string());
                stop.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(
        errors.borrow().as_slice(),
        ["no subscribe ack/data within 5s"],
        "the trip is the attributable br7 error"
    );
}

/// First DATA disarms the ack watchdog exactly like a venue ack: after one Confirm frame, the
/// clock may run arbitrarily far past the deadline without a trip — the session ends on the
/// scripted disconnect instead, and that error must NOT be the ack message.
#[test]
fn first_data_disarms_the_ack_watchdog() {
    let clock = Rc::new(Cell::new(0_i64));
    let now = {
        let c = Rc::clone(&clock);
        move || c.get()
    };
    let stop = AtomicBool::new(false);
    let errors = RefCell::new(Vec::<String>::new());
    let mut stream = Some(
        ScriptedStream::from_steps(vec![
            Step::Text("data".into()),
            Step::Timeout,
            Step::Timeout,
            Step::Timeout,
        ])
        .clocked(Rc::clone(&clock), 3_000),
    );
    run_market_feed_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(Some(Duration::from_secs(5)), None),
        &stop,
        &now,
        |_| FrameOutcome::Confirm,
        || {},
        |s| {
            if let SessionStatus::Error(e) = s {
                errors.borrow_mut().push(e.to_string());
                stop.store(true, Ordering::Relaxed);
            }
        },
    );
    let errors = errors.borrow();
    assert_eq!(errors.len(), 1, "the session ends on the scripted disconnect: {errors:?}");
    assert!(
        !errors[0].contains("subscribe ack"),
        "a confirmed handshake never trips the ack watchdog: {errors:?}"
    );
}

/// A [`FrameOutcome::Fatal`] frame (a venue subscribe REJECT) ends the session with exactly
/// the venue's message — attributable, never silently dropped (br7).
#[test]
fn a_fatal_frame_ends_the_session_with_its_message() {
    let stop = AtomicBool::new(false);
    let errors = RefCell::new(Vec::<String>::new());
    let mut stream = Some(ScriptedStream::from_steps(vec![Step::Text("reject".into())]));
    run_market_feed_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| FrameOutcome::Fatal("Bybit subscribe rejected: Invalid symbol".into()),
        || {},
        |s| {
            if let SessionStatus::Error(e) = s {
                errors.borrow_mut().push(e.to_string());
                stop.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(
        errors.borrow().as_slice(),
        ["Bybit subscribe rejected: Invalid symbol"],
        "the venue's reject message is surfaced verbatim"
    );
}

/// The idle watchdog (hyperliquid's silent-stall shape): a stalled-but-open stream past the
/// threshold errs so the feed reconnects; the threshold disabled (`None`) never trips.
#[test]
fn the_idle_watchdog_trips_only_when_enabled_and_stalled() {
    let stop = AtomicBool::new(false);
    let errors = RefCell::new(Vec::<String>::new());
    let mut stream =
        Some(ScriptedStream::from_steps(vec![Step::Timeout]).with_stall(Duration::from_secs(90)));
    run_market_feed_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, Some(Duration::from_secs(60))),
        &stop,
        &|| 0,
        |_| FrameOutcome::Confirm,
        || {},
        |s| {
            if let SessionStatus::Error(e) = s {
                errors.borrow_mut().push(e.to_string());
                stop.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(
        errors.borrow().as_slice(),
        ["no frames within 60s (silent stall)"],
        "a stalled transport trips the idle watchdog"
    );

    // Disabled (`None`): the same stalled stream absorbs the timeout tick quietly and the
    // session ends on the scripted disconnect instead (the bybit kline/trades shape).
    let stop2 = AtomicBool::new(false);
    let errors2 = RefCell::new(Vec::<String>::new());
    let mut stream2 =
        Some(ScriptedStream::from_steps(vec![Step::Timeout]).with_stall(Duration::from_secs(90)));
    run_market_feed_on(
        || stream2.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, None),
        &stop2,
        &|| 0,
        |_| FrameOutcome::Confirm,
        || {},
        |s| {
            if let SessionStatus::Error(e) = s {
                errors2.borrow_mut().push(e.to_string());
                stop2.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(
        errors2.borrow().as_slice(),
        ["script exhausted"],
        "with the watchdog off, a stalled stream is not an idle fault"
    );
}

/// The app-level keepalive rides the read ticks on its cadence: with `every = ZERO` it is due
/// before every read, so the send log interleaves pings after the subscribe.
#[test]
fn the_keepalive_is_sent_on_cadence() {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let stop = AtomicBool::new(false);
    let mut stream = Some(
        ScriptedStream::from_steps(vec![Step::Timeout, Step::Timeout, Step::Timeout])
            .with_sent_log(Rc::clone(&sent)),
    );
    let opts = MarketPumpOpts {
        subscribe: Some(r#"{"op":"subscribe"}"#),
        keepalive: Some(Keepalive { payload: r#"{"op":"ping"}"#, every: Duration::ZERO }),
        ack_timeout: None,
        idle_threshold: None,
        read_timeout: Duration::from_secs(2),
        backoff: PumpBackoff::Fixed(Duration::from_millis(1)),
        connect_timeout: None,
    };
    run_market_feed_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts,
        &stop,
        &|| 0,
        |_| FrameOutcome::Confirm,
        || {},
        // Error-only. This script confirms nothing, so a blanket `|_|` would behave
        // identically today — spelled out anyway, because the identical blanket form in
        // `on_tick_fires_on_alive_timeout_ticks_only` DID break when the hook was widened, and
        // one line here is cheaper than rediscovering that from a red test.
        |s| {
            if matches!(s, SessionStatus::Error(_)) {
                stop.store(true, Ordering::Relaxed)
            }
        },
    );
    let sent = sent.borrow();
    assert_eq!(sent[0], r#"{"op":"subscribe"}"#, "subscribe first: {sent:?}");
    assert!(
        sent.iter().filter(|s| s.contains("ping")).count() >= 3,
        "one keepalive per due read tick: {sent:?}"
    );
}

/// `on_tick` (polymarket knob): fires on every transport-ALIVE read-timeout tick — never on a
/// data frame, and never on the tick that trips a watchdog (the trip returns first).
#[test]
fn on_tick_fires_on_alive_timeout_ticks_only() {
    let stop = AtomicBool::new(false);
    let ticks = Cell::new(0);
    let mut stream = Some(ScriptedStream::from_steps(vec![
        Step::Text("data".into()),
        Step::Timeout,
        Step::Timeout,
        Step::Timeout,
    ]));
    run_market_feed_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| FrameOutcome::Confirm,
        || ticks.set(ticks.get() + 1),
        // ⚠ ERROR-ONLY, and it has to be spelled that way now: this hook fires on
        // `SessionStatus::Live` too, and the first frame of this script CONFIRMS — so a
        // blanket `|_| stop` would end the session at frame one and count ZERO ticks.
        // Measured: this test failed exactly that way when the hook was widened.
        |s| {
            if matches!(s, SessionStatus::Error(_)) {
                stop.store(true, Ordering::Relaxed)
            }
        },
    );
    assert_eq!(ticks.get(), 3, "one on_tick per alive timeout tick, none for the data frame");

    // An idle-watchdog trip consumes its tick BEFORE the hook: a stalled stream's only
    // timeout tick errors out, so on_tick never fires.
    let stop2 = AtomicBool::new(false);
    let ticks2 = Cell::new(0);
    let mut stream2 =
        Some(ScriptedStream::from_steps(vec![Step::Timeout]).with_stall(Duration::from_secs(90)));
    run_market_feed_on(
        || stream2.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, Some(Duration::from_secs(60))),
        &stop2,
        &|| 0,
        |_| FrameOutcome::Confirm,
        || ticks2.set(ticks2.get() + 1),
        // Error-only for the same reason, though this script confirms nothing.
        |s| {
            if matches!(s, SessionStatus::Error(_)) {
                stop2.store(true, Ordering::Relaxed)
            }
        },
    );
    assert_eq!(ticks2.get(), 0, "a tripping tick returns before the on_tick hook");
}

/// **THE BAR TEST for the disclosure seam** — a session that faulted, then one that works, and
/// the hook must hear about BOTH in that order.
///
/// This could not be written against the pre-widening driver at all: there was no success
/// signal in the module to observe, which is the strongest form of "fails today" and is
/// exactly why the venue-level twin
/// (`crates/bridges/bybit/tests/offline/market_feed_scripted.rs`) exists beside it — that one
/// fails BEHAVIOURALLY, this one could not even compile.
///
/// The second assertion is the one that keeps the cost honest: `Live` fires ONCE per session
/// however many frames confirm. A venue's arm takes a status mutex, so a per-frame disclosure
/// would put a lock acquisition on every kline tick of every feed thread in the workspace.
#[test]
fn a_recovered_session_discloses_live_after_the_error() {
    let stop = AtomicBool::new(false);
    let seen: RefCell<Vec<String>> = RefCell::new(Vec::new());
    // Session 1 faults by exhaustion. Session 2 confirms FOUR times (one ack + three data
    // frames) and then ends by exhaustion too, so the recorded sequence shows how many `Live`
    // disclosures four confirmations produce.
    let mut streams = VecDeque::from([
        ScriptedStream::from_steps(vec![Step::Timeout]),
        ScriptedStream::from_steps(vec![
            Step::Text("ack".into()),
            Step::Text("d1".into()),
            Step::Text("d2".into()),
            Step::Text("d3".into()),
        ]),
    ]);
    run_market_feed_on(
        || streams.pop_front().ok_or_else(|| "no more scripted streams".to_string()),
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| FrameOutcome::Confirm,
        || {},
        |s| {
            match s {
                SessionStatus::Live => seen.borrow_mut().push("live".into()),
                SessionStatus::Error(e) => seen.borrow_mut().push(format!("error:{e}")),
            }
            // Two faults is the whole script; stop after the second so the loop ends.
            if seen.borrow().iter().filter(|x| x.starts_with("error")).count() >= 2 {
                stop.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(
        seen.borrow().as_slice(),
        [
            "error:script exhausted".to_string(),
            "live".to_string(),
            "error:script exhausted".to_string(),
        ],
        "a recovered session must disclose Live AFTER the error — the ordering IS the fix"
    );
    assert_eq!(
        seen.borrow().iter().filter(|s| *s == "live").count(),
        1,
        "exactly one Live per session, however many frames confirm — the guard against taking \
             a venue's status mutex on every tick of the hot feed thread"
    );
}

/// A session that never confirms discloses NO `Live` — the disclosure is evidence about the
/// VENUE (it accepted the subscription and is delivering), never about the socket having
/// opened. This is what makes the hook safe during an accept-then-close storm, where a
/// connect-Ok-keyed disclosure would write "healthy" on every three-second cycle and render
/// the health gate inert for that venue.
#[test]
fn a_session_that_never_confirms_discloses_no_live() {
    let stop = AtomicBool::new(false);
    let seen: RefCell<Vec<String>> = RefCell::new(Vec::new());
    // Frames arrive, but the venue's classifier calls every one of them uninteresting.
    let mut stream =
        Some(ScriptedStream::from_steps(vec![Step::Text("noise".into()), Step::Timeout]));
    run_market_feed_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| FrameOutcome::Ignore,
        || {},
        |s| {
            match s {
                SessionStatus::Live => seen.borrow_mut().push("live".into()),
                SessionStatus::Error(_) => seen.borrow_mut().push("error".into()),
            }
            stop.store(true, Ordering::Relaxed);
        },
    );
    assert_eq!(
        seen.borrow().as_slice(),
        ["error".to_string()],
        "an open socket delivering nothing the venue recognises is not a live subscription"
    );
}

/// A clean STOP discloses nothing at all — not `Live`, not `Error`. A requested teardown is
/// neither a fault nor a recovery, the same rule [`crate::depth`]'s `DepthFaultLog` states.
/// Without this the daemon's shutdown would rewrite every venue's status on the way out.
#[test]
fn a_clean_stop_discloses_nothing() {
    let stop = AtomicBool::new(false);
    let seen = Cell::new(0);
    let mut stream = Some(ScriptedStream::from_steps(vec![
        Step::Text("x".into()),
        Step::Timeout,
        Step::Timeout,
    ]));
    run_market_feed_on(
        || stream.take().ok_or_else(|| "reconnected after a requested stop".to_string()),
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| {
            stop.store(true, Ordering::Relaxed);
            // NOT Confirm: a `Live` here would be a legitimate disclosure and would mask what
            // this test is about, which is the `Ok(()) => break` arm's silence.
            FrameOutcome::Ignore
        },
        || {},
        |_| seen.set(seen.get() + 1),
    );
    assert_eq!(seen.get(), 0, "a requested stop is neither a fault nor a recovery");
}

/// [`PumpBackoff`] (polymarket knob): the exponential ladder doubles per consecutive fault,
/// caps at `max`, and restarts at `initial`; the fixed policy never moves.
#[test]
fn pump_backoff_ladder() {
    let exp = PumpBackoff::Exponential {
        initial: Duration::from_millis(500),
        max: Duration::from_secs(30),
    };
    assert_eq!(exp.initial(), Duration::from_millis(500));
    let mut cur = exp.initial();
    let mut walk = Vec::new();
    for _ in 0..8 {
        cur = exp.next(cur);
        walk.push(cur.as_millis());
    }
    assert_eq!(
        walk,
        vec![1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000],
        "500ms doubling capped at 30s — polymarket's pre-driver shape"
    );

    let fixed = PumpBackoff::Fixed(Duration::from_secs(3));
    assert_eq!(fixed.initial(), Duration::from_secs(3));
    assert_eq!(fixed.next(Duration::from_secs(3)), Duration::from_secs(3));

    // The reset contract [`run_market_feed_on`] applies on every successful connect: the next
    // wait after a success is `initial()` again, however far the ladder had climbed.
    assert_eq!(exp.initial(), Duration::from_millis(500), "success resets to initial");
}
