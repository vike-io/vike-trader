use super::*;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

// The scripted WebsocketSessionMarketStream double — `crate::scripted` holds the one copy. `send_text` records
// into a shareable log (`with_sent_log`) so a feed test can assert across sessions; the shared
// `clock` (`clocked`) advances before every read so consuming the script ages the ack watchdog
// with zero real sleeps.
use crate::scripted::{ScriptStep as Step, WebsocketSessionMarketStreamScript};

fn opts_with(
    ack: Option<Duration>,
    idle: Option<Duration>,
) -> WebsocketSessionMarketOptions<'static> {
    WebsocketSessionMarketOptions {
        subscribe: Some(r#"{"op":"subscribe"}"#),
        keepalive: None,
        ack_timeout: ack,
        idle_threshold: idle,
        read_timeout: Duration::from_secs(2),
        // 1 stop-aware 100 ms tick — keeps tests fast
        backoff: VenueTimingsReconnect::fixed(Duration::from_millis(1), "a test's 1 ms redial"),
        connect_timeout: None,
        notice: None,
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
        WebsocketSessionMarketStreamScript::from_steps(vec![Step::Text("data1".into())])
            .with_sent_log(Rc::clone(&sent)),
        WebsocketSessionMarketStreamScript::from_steps(vec![Step::Text("stop-now".into())])
            .with_sent_log(Rc::clone(&sent)),
    ]);
    let connects = Cell::new(0);
    run_market_on(
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
            WebsocketSessionFrameOutcome::Confirm
        },
        || {},
        |s| {
            if let WebsocketSessionStatus::Error(e) = s {
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
    run_market_on(
        || {
            connects.set(connects.get() + 1);
            Ok(WebsocketSessionMarketStreamScript::from_steps(vec![]))
        },
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| WebsocketSessionFrameOutcome::Ignore,
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
        WebsocketSessionMarketStreamScript::from_steps(vec![
            Step::Text("x".into()),
            Step::Timeout,
            Step::Timeout,
        ])
        .with_sent_log(Rc::clone(&sent)),
    );
    run_market_on(
        || {
            connects.set(connects.get() + 1);
            stream.take().ok_or_else(|| "reconnected after a requested stop".to_string())
        },
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| {
            stop.store(true, Ordering::Relaxed);
            WebsocketSessionFrameOutcome::Confirm
        },
        || {},
        |s| {
            if matches!(s, WebsocketSessionStatus::Error(_)) {
                errors.set(errors.get() + 1)
            }
        },
    );
    assert_eq!(connects.get(), 1, "no reconnect after a requested stop");
    assert_eq!(errors.get(), 0, "a requested stop is not an error");
}

/// A subscribe that never acks and never delivers data trips the ack watchdog once the window
/// elapses — the exact attributable `"no subscribe ack/data within {N}s"` error, which the feed
/// loop discloses (then the test's error hook raises stop, so exactly one session runs).
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
        WebsocketSessionMarketStreamScript::from_steps(vec![Step::Timeout, Step::Timeout])
            .clocked(Rc::clone(&clock), 3_000),
    );
    run_market_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(Some(Duration::from_secs(5)), None),
        &stop,
        &now,
        |_| WebsocketSessionFrameOutcome::Confirm,
        || {},
        |s| {
            if let WebsocketSessionStatus::Error(e) = s {
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
        WebsocketSessionMarketStreamScript::from_steps(vec![
            Step::Text("data".into()),
            Step::Timeout,
            Step::Timeout,
            Step::Timeout,
        ])
        .clocked(Rc::clone(&clock), 3_000),
    );
    run_market_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(Some(Duration::from_secs(5)), None),
        &stop,
        &now,
        |_| WebsocketSessionFrameOutcome::Confirm,
        || {},
        |s| {
            if let WebsocketSessionStatus::Error(e) = s {
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

/// A [`WebsocketSessionFrameOutcome::Fatal`] frame (a venue subscribe REJECT) ends the session with exactly
/// the venue's message — attributable, never silently dropped.
#[test]
fn a_fatal_frame_ends_the_session_with_its_message() {
    let stop = AtomicBool::new(false);
    let errors = RefCell::new(Vec::<String>::new());
    let mut stream =
        Some(WebsocketSessionMarketStreamScript::from_steps(vec![Step::Text("reject".into())]));
    run_market_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| WebsocketSessionFrameOutcome::Fatal("Bybit subscribe rejected: Invalid symbol".into()),
        || {},
        |s| {
            if let WebsocketSessionStatus::Error(e) = s {
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

/// The idle watchdog: a stalled-but-open stream past the threshold errs so the feed reconnects;
/// the threshold disabled (`None`) never trips.
#[test]
fn the_idle_watchdog_trips_only_when_enabled_and_stalled() {
    let stop = AtomicBool::new(false);
    let errors = RefCell::new(Vec::<String>::new());
    let mut stream = Some(
        WebsocketSessionMarketStreamScript::from_steps(vec![Step::Timeout])
            .with_stall(Duration::from_secs(90)),
    );
    run_market_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, Some(Duration::from_secs(60))),
        &stop,
        &|| 0,
        |_| WebsocketSessionFrameOutcome::Confirm,
        || {},
        |s| {
            if let WebsocketSessionStatus::Error(e) = s {
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
    // session ends on the scripted disconnect instead.
    let stop2 = AtomicBool::new(false);
    let errors2 = RefCell::new(Vec::<String>::new());
    let mut stream2 = Some(
        WebsocketSessionMarketStreamScript::from_steps(vec![Step::Timeout])
            .with_stall(Duration::from_secs(90)),
    );
    run_market_on(
        || stream2.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, None),
        &stop2,
        &|| 0,
        |_| WebsocketSessionFrameOutcome::Confirm,
        || {},
        |s| {
            if let WebsocketSessionStatus::Error(e) = s {
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
        WebsocketSessionMarketStreamScript::from_steps(vec![
            Step::Timeout,
            Step::Timeout,
            Step::Timeout,
        ])
        .with_sent_log(Rc::clone(&sent)),
    );
    let opts = WebsocketSessionMarketOptions {
        subscribe: Some(r#"{"op":"subscribe"}"#),
        keepalive: Some(WebsocketSessionMarketKeepalive {
            payload: r#"{"op":"ping"}"#,
            every: Duration::ZERO,
        }),
        ack_timeout: None,
        idle_threshold: None,
        read_timeout: Duration::from_secs(2),
        backoff: VenueTimingsReconnect::fixed(Duration::from_millis(1), "a test's 1 ms redial"),
        connect_timeout: None,
        notice: None,
    };
    run_market_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts,
        &stop,
        &|| 0,
        |_| WebsocketSessionFrameOutcome::Confirm,
        || {},
        // Error-only, though this script confirms nothing: the hook hears `Live` too, and a
        // blanket `|_| stop` ends a confirming session at frame one (see
        // `on_tick_fires_on_alive_timeout_ticks_only`).
        |s| {
            if matches!(s, WebsocketSessionStatus::Error(_)) {
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

/// `on_tick` fires on every transport-ALIVE read-timeout tick — never on a
/// data frame, and never on the tick that trips a watchdog (the trip returns first).
#[test]
fn on_tick_fires_on_alive_timeout_ticks_only() {
    let stop = AtomicBool::new(false);
    let ticks = Cell::new(0);
    let mut stream = Some(WebsocketSessionMarketStreamScript::from_steps(vec![
        Step::Text("data".into()),
        Step::Timeout,
        Step::Timeout,
        Step::Timeout,
    ]));
    run_market_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| WebsocketSessionFrameOutcome::Confirm,
        || ticks.set(ticks.get() + 1),
        // ⚠ ERROR-ONLY: this hook fires on `WebsocketSessionStatus::Live` too, and the first frame of
        // this script CONFIRMS — so a blanket `|_| stop` would end the session at frame one and
        // count ZERO ticks.
        |s| {
            if matches!(s, WebsocketSessionStatus::Error(_)) {
                stop.store(true, Ordering::Relaxed)
            }
        },
    );
    assert_eq!(ticks.get(), 3, "one on_tick per alive timeout tick, none for the data frame");

    // An idle-watchdog trip consumes its tick BEFORE the hook: a stalled stream's only
    // timeout tick errors out, so on_tick never fires.
    let stop2 = AtomicBool::new(false);
    let ticks2 = Cell::new(0);
    let mut stream2 = Some(
        WebsocketSessionMarketStreamScript::from_steps(vec![Step::Timeout])
            .with_stall(Duration::from_secs(90)),
    );
    run_market_on(
        || stream2.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, Some(Duration::from_secs(60))),
        &stop2,
        &|| 0,
        |_| WebsocketSessionFrameOutcome::Confirm,
        || ticks2.set(ticks2.get() + 1),
        // Error-only for the same reason, though this script confirms nothing.
        |s| {
            if matches!(s, WebsocketSessionStatus::Error(_)) {
                stop2.store(true, Ordering::Relaxed)
            }
        },
    );
    assert_eq!(ticks2.get(), 0, "a tripping tick returns before the on_tick hook");
}

/// **THE BAR TEST for the disclosure seam** — a session that faulted, then one that works, and
/// the hook must hear about BOTH in that order. Its venue-level twin is
/// `crates/bridges/bybit/tests/offline/market_feed_scripted.rs`.
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
        WebsocketSessionMarketStreamScript::from_steps(vec![Step::Timeout]),
        WebsocketSessionMarketStreamScript::from_steps(vec![
            Step::Text("ack".into()),
            Step::Text("d1".into()),
            Step::Text("d2".into()),
            Step::Text("d3".into()),
        ]),
    ]);
    run_market_on(
        || streams.pop_front().ok_or_else(|| "no more scripted streams".to_string()),
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| WebsocketSessionFrameOutcome::Confirm,
        || {},
        |s| {
            match s {
                WebsocketSessionStatus::Live => seen.borrow_mut().push("live".into()),
                WebsocketSessionStatus::Error(e) => seen.borrow_mut().push(format!("error:{e}")),
                WebsocketSessionStatus::Announced(e) => {
                    seen.borrow_mut().push(format!("announced:{e}"))
                }
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
    let mut stream = Some(WebsocketSessionMarketStreamScript::from_steps(vec![
        Step::Text("noise".into()),
        Step::Timeout,
    ]));
    run_market_on(
        || stream.take().ok_or_else(|| "second connect".to_string()),
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| WebsocketSessionFrameOutcome::Ignore,
        || {},
        |s| {
            match s {
                WebsocketSessionStatus::Live => seen.borrow_mut().push("live".into()),
                WebsocketSessionStatus::Error(_) => seen.borrow_mut().push("error".into()),
                WebsocketSessionStatus::Announced(_) => seen.borrow_mut().push("announced".into()),
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
/// neither a fault nor a recovery, the same rule [`crate::depth`]'s `WebsocketSessionDepthFaultLog` states.
/// Without this the daemon's shutdown would rewrite every venue's status on the way out.
#[test]
fn a_clean_stop_discloses_nothing() {
    let stop = AtomicBool::new(false);
    let seen = Cell::new(0);
    let mut stream = Some(WebsocketSessionMarketStreamScript::from_steps(vec![
        Step::Text("x".into()),
        Step::Timeout,
        Step::Timeout,
    ]));
    run_market_on(
        || stream.take().ok_or_else(|| "reconnected after a requested stop".to_string()),
        &opts_with(None, None),
        &stop,
        &|| 0,
        |_| {
            stop.store(true, Ordering::Relaxed);
            // NOT Confirm: a `Live` here would be a legitimate disclosure and would mask what
            // this test is about, which is the `Ok(()) => break` arm's silence.
            WebsocketSessionFrameOutcome::Ignore
        },
        || {},
        |_| seen.set(seen.get() + 1),
    );
    assert_eq!(seen.get(), 0, "a requested stop is neither a fault nor a recovery");
}

/// Each of `waits` is its step of `steps` or later by at most half of it, never past `max` (a row's spread ladder,
/// `vike_venues::reconnect`'s module doc): a fixed row's waits are its steps exactly.
fn within_the_spread(waits: &[u128], steps: &[u128], max: u128) -> bool {
    waits.len() == steps.len()
        && waits
            .iter()
            .zip(steps)
            .all(|(wait, step)| *step <= *wait && *wait <= (step + step / 2).min(max))
}

/// **The market pump's reconnect waits, pinned verbatim before its ladder moved onto the one backoff** (connection
/// plan B3): the wait handed to each pause over three failed connects, a session that opened and then faulted, and
/// seven failed connects. polymarket's doubling row (500 ms to 30 s) starts over at the connect that worked, each step
/// spread under the ceiling (the pump's ladder is a row's); the fixed 3 s every other pump row declares never moves.
#[test]
fn the_market_pumps_reconnect_waits_are_pinned_verbatim() {
    let waits_of = |backoff: VenueTimingsReconnect| -> Vec<u128> {
        let mut opens: VecDeque<bool> =
            [false, false, false, true, false, false, false, false, false, false, false].into();
        let stop = AtomicBool::new(false);
        let mut waits = Vec::new();
        run_market_on_paced(
            || match opens.pop_front() {
                Some(true) => Ok(WebsocketSessionMarketStreamScript::from_steps(vec![])),
                _ => Err("refused".to_string()),
            },
            &WebsocketSessionMarketOptions { backoff, ..opts_with(None, None) },
            &stop,
            &|| 0,
            |_| WebsocketSessionFrameOutcome::Confirm,
            || {},
            |_| {},
            |wait| {
                waits.push(wait.as_millis());
                waits.len() < 11
            },
        );
        waits
    };
    let doubling = [500, 1000, 2000, 500, 1000, 2000, 4000, 8000, 16000, 30000, 30000];
    let exponential = VenueTimingsReconnect::new(
        Duration::from_millis(500),
        Duration::from_secs(30),
        "polymarket's doubling market redial",
    );
    let waits = waits_of(exponential);
    assert!(
        within_the_spread(&waits, &doubling, 30_000),
        "doubling per fault, from the start again after the connect that worked, capped at 30 s: {waits:?}"
    );
    let fixed = VenueTimingsReconnect::fixed(Duration::from_secs(3), "the pump's classic redial");
    assert_eq!(waits_of(fixed), [3000; 11]);
    // The rows the pump runs, read from the venue table since B3.07: polymarket's grows, binance's is the fixed 3 s.
    let row = crate::websocket_session::spec::row;
    let polymarket = waits_of(row("polymarket").backoff);
    assert!(
        within_the_spread(&polymarket, &doubling, 30_000),
        "polymarket's market row: {polymarket:?}"
    );
    assert_eq!(waits_of(row("binance").backoff), [3000; 11], "binance's market row");
    // oanda's quote lane, pinned before it left its own loop for this pump: that loop waited the market row's 3 s
    // after every failed open and every dropped stream (its `RECONNECT_BACKOFF`), and its pump row's ladder does too.
    let oanda = row("oanda").backoff;
    assert_eq!(waits_of(oanda), [3000; 11], "oanda's quote lane: 3 s after every fault");
    // IBKR's Client Portal market lane, pinned before it left its own loop for this pump: that loop waited the market
    // row's 2 s (its `REDIAL`) after every failed dial and every dropped session alike, and its pump row's ladder does too.
    let cpapi = crate::websocket_session::spec::IBKR_CPAPI.backoff;
    assert_eq!(waits_of(cpapi), [2000; 11], "the cpapi market lane: 2 s after every fault");
}

/// A venue's notice hook: `closing` is its announced end.
fn closing_notice(txt: &str) -> Option<String> {
    (txt == "closing").then(|| "the venue announced: upgrade".to_string())
}

/// **The planned reconnect**, on the virtual clock (`crate::pump_clock`), over three sessions on a 1 s -> 30 s
/// ladder: the first lives twice the ceiling and is announced closed, the second is announced closed at once, the
/// third stops. The first reopen comes at the notice itself, the second the ladder's first step later (a venue
/// announcing the end of every new session never loops hot); each end is `Announced` in the hook's words, never
/// `Error`; the frame the hook answers never reaches `on_text`.
#[test]
fn an_announced_close_reopens_at_once_after_a_long_session_and_on_the_ladder_after_a_young_one() {
    let clock = crate::pump_clock::VirtualClock::new(1);
    let (initial, max) = (Duration::from_secs(1), Duration::from_secs(30));
    let pump = std::thread::spawn(move || {
        let _seat = clock.enter();
        let stop = AtomicBool::new(false);
        let mut sessions =
            VecDeque::from([vec!["data", "age", "closing"], vec!["closing"], vec!["stop-now"]]);
        let (mut dials, mut said, mut read) = (Vec::new(), Vec::new(), Vec::new());
        run_market_on(
            || {
                dials.push(clock.offset(clock.now()));
                let frames = sessions.pop_front().ok_or_else(|| "no more sessions".to_string())?;
                Ok(WebsocketSessionMarketStreamScript::from_texts(frames))
            },
            &WebsocketSessionMarketOptions {
                backoff: VenueTimingsReconnect::new(initial, max, "a test's 1 s -> 30 s ladder"),
                notice: Some(closing_notice),
                ..opts_with(None, None)
            },
            &stop,
            &|| 0,
            |txt| {
                read.push(txt.to_string());
                match txt {
                    "age" => crate::pump_clock::sleep(2 * max),
                    "stop-now" => stop.store(true, Ordering::Relaxed),
                    _ => {}
                }
                WebsocketSessionFrameOutcome::Confirm
            },
            || {},
            |s| {
                said.push(match s {
                    WebsocketSessionStatus::Live => "live".to_string(),
                    WebsocketSessionStatus::Error(e) => format!("error: {e}"),
                    WebsocketSessionStatus::Announced(words) => format!("announced: {words}"),
                });
            },
        );
        (dials, said, read)
    });
    let (dials, said, read) = pump.join().expect("the pump thread");
    assert_eq!(dials[..2], [Duration::ZERO, 2 * max], "at once: {dials:?}");
    assert!(
        dials.len() == 3
            && (2 * max + initial..=2 * max + initial + initial / 2).contains(&dials[2]),
        "then the ladder's first step, spread by up to half of it: {dials:?}"
    );
    let announced = "announced: the venue announced: upgrade";
    assert_eq!(said, ["live", announced, announced, "live"], "a planned reopen is never an error");
    assert_eq!(
        read,
        ["data", "age", "stop-now"],
        "the notice frame is the hook's, not the venue's decoder's"
    );
}

/// The pump's ladder over a reconnect row ([`ReconnectBackoff::from_row`]): a growing row doubles per consecutive
/// fault, caps at `max`, and restarts at `initial`; a fixed row never moves.
#[test]
fn pump_backoff_ladder() {
    let exp = VenueTimingsReconnect::new(
        Duration::from_millis(500),
        Duration::from_secs(30),
        "polymarket's doubling market redial",
    );
    let mut ladder = ReconnectBackoff::from_row(&exp).without_jitter();
    let walk: Vec<u128> = (0..9).map(|_| ladder.failed().as_millis()).collect();
    assert_eq!(
        walk,
        vec![500, 1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000],
        "500ms doubling capped at 30s — polymarket's pre-driver shape"
    );

    let mut fixed = ReconnectBackoff::from_row(&VenueTimingsReconnect::fixed(
        Duration::from_secs(3),
        "the pump's classic redial",
    ));
    assert_eq!([fixed.failed(), fixed.failed()], [Duration::from_secs(3); 2]);

    // The reset contract [`run_market_on`] applies on every successful connect: the next
    // wait after a success is `initial` again, however far the ladder had climbed.
    ladder.reset();
    assert_eq!(ladder.failed(), Duration::from_millis(500), "success resets to initial");
}

/// The ONE stamped tungstenite stream behind the market pump, the depth driver (its `WebsocketSessionDepthStream`
/// impl) and ig's Lightstreamer session, which each carried their own copy of `read_frame`, over a
/// real loopback WebSocket: a Ping is answered and read past, a non-text frame is read past, a Text
/// is returned (through either trait), and a server Close is `Closed("server closed")`.
#[test]
fn the_stamped_stream_reads_past_control_frames_through_both_traits() {
    use tungstenite::Message;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(stream).expect("the server side of the upgrade");
        ws.send(Message::Ping(b"p1".to_vec().into())).unwrap();
        ws.send(Message::Binary(b"ignored".to_vec().into())).unwrap();
        ws.send(Message::Text("one".into())).unwrap();
        ws.send(Message::Text("two".into())).unwrap();
        let mut ponged = false;
        while !ponged {
            match ws.read() {
                Ok(Message::Pong(p)) => ponged = &p[..] == b"p1",
                Ok(_) => {}
                Err(e) => panic!("the client never answered the ping: {e}"),
            }
        }
        ws.close(None).unwrap();
        while ws.read().is_ok() {} // drain until the close handshake ends
        ponged
    });

    let socket = connect_websocket(&url, None, Some(Duration::from_secs(5))).expect("a live peer");
    configure_websocket_stream(&socket, Duration::from_secs(5));
    let mut s =
        WebsocketSessionMarketStreamTungstenite::new(WebsocketSessionUserStreamTungstenite(socket));
    assert_eq!(WebsocketSessionMarketStream::read_frame(&mut s).unwrap(), "one");
    assert_eq!(crate::depth::WebsocketSessionDepthStream::read_frame(&mut s).unwrap(), "two");
    assert!(WebsocketSessionMarketStream::since_last_frame(&s) < Duration::from_secs(5));
    match WebsocketSessionMarketStream::read_frame(&mut s) {
        Err(WebsocketSessionUserError::Closed(m)) => assert_eq!(m, "server closed"),
        other => panic!("a server close must surface as Closed, got {other:?}"),
    }
    drop(s);
    assert!(server.join().unwrap(), "the ping was answered with its own payload");
}

/// A server that says why it hangs up is heard: the pump's journal line and `trade health` carry its code and reason.
#[test]
fn a_server_close_that_gives_a_code_and_reason_says_them() {
    use tungstenite::protocol::CloseFrame;
    use tungstenite::protocol::frame::coding::CloseCode;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(stream).expect("the server side of the upgrade");
        ws.close(Some(CloseFrame { code: CloseCode::Away, reason: "rolling".into() })).unwrap();
        while ws.read().is_ok() {} // drain until the close handshake ends
    });
    let socket = connect_websocket(&url, None, Some(Duration::from_secs(5))).expect("a live peer");
    configure_websocket_stream(&socket, Duration::from_secs(5));
    match WebsocketSessionUserStreamTungstenite(socket).recv() {
        Err(WebsocketSessionUserError::Closed(m)) => assert_eq!(m, "server closed: 1001 rolling"),
        Err(WebsocketSessionUserError::Timeout) => {
            panic!("a server close must surface as Closed, got a timeout")
        }
        Ok(_) => panic!("a server close must surface as Closed, got a frame"),
    }
    server.join().unwrap();
}
