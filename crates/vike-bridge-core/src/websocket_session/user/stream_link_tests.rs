//! What the shared pump says of its stream, through the real loop over scripted sessions, read back from the account's
//! one link cell (`crate::venue_link::venue_link`).

use std::collections::VecDeque;
use std::time::Instant;

use super::super::*;
use super::*;

/// The pump's first reconnect wait in these tests (`ReconnectBackoff::new` hands it out unclamped, whatever the
/// ceiling): the 1 s every pump-driven exec row declares.
const FIRST_WAIT: Duration = Duration::from_secs(1);

/// A link of its own for one test: the cells are process-wide, keyed by route key, and the tests run side by side.
fn fresh_link() -> VenueLinkStream {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    stream_link(&format!("test#STREAM-LINK-{}", NEXT.fetch_add(1, Ordering::Relaxed)))
}

/// What the account's cell reads now.
fn state(link: &VenueLinkStream) -> VenueLinkState {
    venue_link(&link.0).state()
}

/// One scripted session: from `until` its stream closes on the next read with these words, or with none it stays open.
struct Session {
    closes: Option<&'static str>,
    until: Instant,
}

impl WebsocketSessionUserStream for Session {
    fn recv(&mut self) -> Result<WebsocketSessionUserMessage, WebsocketSessionUserError> {
        match self.closes.take_if(|_| Instant::now() >= self.until) {
            Some(why) => Err(WebsocketSessionUserError::Closed(why.to_string())),
            None => {
                std::thread::sleep(Duration::from_millis(1));
                Err(WebsocketSessionUserError::Timeout)
            }
        }
    }
    fn pong(&mut self, _payload: Vec<u8>) -> Result<(), WebsocketSessionUserError> {
        Ok(())
    }
}

enum Open {
    Ready(Option<&'static str>),
    /// A session that stays up this long and then closes with these words.
    Lives(Duration, &'static str),
    Transport(&'static str),
    Refused(&'static str),
    Auth(&'static str),
}

/// Runs the loop over `script`, on a thread told to report to a link of its own, on the pump's ladder up to `max`. The
/// last open repeats for ever.
fn pump(
    script: Vec<Open>,
    max: Duration,
) -> (
    VenueLinkStream,
    Arc<AtomicBool>,
    std::thread::JoinHandle<Result<(), WebsocketSessionUserAuthenticationError>>,
) {
    pump_on(fresh_link(), script, ReconnectBackoff::new(FIRST_WAIT, max))
}

/// [`pump`] on a given link and ladder.
fn pump_on(
    link: VenueLinkStream,
    script: Vec<Open>,
    backoff: ReconnectBackoff,
) -> (
    VenueLinkStream,
    Arc<AtomicBool>,
    std::thread::JoinHandle<Result<(), WebsocketSessionUserAuthenticationError>>,
) {
    let stop = Arc::new(AtomicBool::new(false));
    let (for_thread, stop_thread) = (link.clone(), Arc::clone(&stop));
    let handle = std::thread::spawn(move || {
        report_on_this_thread(Some(for_thread));
        let mut script: VecDeque<Open> = script.into();
        run_user(
            move || {
                let next = script.pop_front().expect("the script names the last open");
                let outcome = match &next {
                    Open::Ready(closes) => WebsocketSessionUserOpenOutcome::Ready(Session {
                        closes: *closes,
                        until: Instant::now(),
                    }),
                    Open::Lives(span, why) => WebsocketSessionUserOpenOutcome::Ready(Session {
                        closes: Some(*why),
                        until: Instant::now() + *span,
                    }),
                    Open::Transport(why) => {
                        WebsocketSessionUserOpenOutcome::Transport((*why).to_string())
                    }
                    Open::Refused(why) => {
                        WebsocketSessionUserOpenOutcome::Refused((*why).to_string())
                    }
                    Open::Auth(why) => WebsocketSessionUserOpenOutcome::Auth(
                        WebsocketSessionUserAuthenticationError((*why).to_string()),
                    ),
                };
                if script.is_empty() {
                    script.push_back(next);
                }
                outcome
            },
            |_| vec![],
            |_| true,
            &stop_thread,
            Duration::from_millis(1),
            backoff,
            None,
            || {},
            None,
            None,
        )
    });
    (link, stop, handle)
}

fn wait_for(link: &VenueLinkStream, wanted: impl Fn(&VenueLinkState) -> bool) -> VenueLinkState {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = state(link);
        if wanted(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "the reading never came: {state:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The loss the cell reads, which the caller's `wait_for` asked for.
fn down_of(state: VenueLinkState) -> VenueLinkDown {
    match state {
        VenueLinkState::Down(down) => down,
        other => unreachable!("not a loss: {other:?}"),
    }
}

/// **A stream that closes reads down with the words it closed on, and open again once the pump reopened it.** The wait
/// is the backoff's, so `trade health` can say when the next try comes.
#[test]
fn a_stream_that_closes_reads_down_and_a_reopen_ends_it() {
    let (link, stop, pump) =
        pump(vec![Open::Ready(Some("reset by peer")), Open::Ready(None)], Duration::from_secs(1));
    let down = down_of(wait_for(
        &link,
        |s| matches!(s, VenueLinkState::Down(d) if d.why == "reset by peer"),
    ));
    assert_eq!(down.failed_restarts, 0, "the session closed; no open failed");
    assert!(down.retry_in > Duration::ZERO && down.effect == REOPENING_EFFECT, "{down:?}");
    wait_for(&link, |s| *s == VenueLinkState::Up);
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap().unwrap();
    assert_eq!(state(&link), VenueLinkState::Unknown, "a pump that stopped leaves no reading");
}

/// **A session that lived starts the wait over; a server that accepts and closes at once keeps backing off.** the CI box,
/// 2026-11-07: aster and hyperliquid sat at the ceiling for good, so a socket that had been fine for ten minutes stayed
/// dark for 31 s at every close. The readings are the pump's own promises, so a slow box cannot fake either answer.
#[test]
fn a_session_that_lived_starts_the_wait_over_and_one_that_closed_at_once_does_not() {
    let ceiling = Duration::from_millis(1_500);
    let (link, stop, pump) = pump(
        vec![
            Open::Ready(Some("closed at once")),
            Open::Ready(Some("closed at once again")),
            // Past the rule's twice the ceiling, whenever the pump starts its clock.
            Open::Lives(ceiling * 2 + Duration::from_millis(100), "lived"),
            Open::Ready(None),
        ],
        ceiling,
    );
    // The next try's instant: the cell counts the wait down from the word, so now + what is left is when it comes.
    let next_try = |why: &str| {
        let down =
            down_of(wait_for(&link, |s| matches!(s, VenueLinkState::Down(d) if d.why == why)));
        Instant::now() + down.retry_in
    };
    let first = next_try("closed at once");
    let second = next_try("closed at once again");
    // The second close comes at least the first wait (1 s) later, so this is the second wait's half past that.
    assert!(second - first >= ceiling, "the wait grew: {:?}", second - first);
    let lived = next_try("lived");
    let wait = lived.saturating_duration_since(Instant::now());
    assert!(wait <= FIRST_WAIT, "the wait started over, it is {wait:?}");
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap().unwrap();
}

/// **A session's end is said at the order path's level rule, in the state the lane then reads** (`feed_ctx::set_status`'s
/// rule: only `Error` warns). A session the venue closed (hyperliquid testnet's "1000 Expired", every ~10 min on the CI box) is
/// a reopen: `Connecting`, said at `info`. A session the pump ended for its silence failed: `Error`, said at `warn`.
#[test]
fn a_close_reopens_at_info_and_a_stall_is_an_error_at_warn() {
    use tracing::Level;
    use vike_model::feed_status::DataClientConnectionState::{Connecting, Error};

    let link = fresh_link();
    report_on_this_thread(Some(link.clone()));
    let stop = AtomicBool::new(false);
    let (mut opens, mut read_at_reopen) = (0, Vec::new());
    let (ended, logged) = vike_log::capture::captured(|| {
        run_user(
            || {
                opens += 1;
                if opens > 1 {
                    read_at_reopen.push(state(&link).connection_state());
                }
                let session = |closes| Session { closes, until: Instant::now() };
                match opens {
                    1 => WebsocketSessionUserOpenOutcome::Ready(session(Some(
                        "server closed: 1000 Expired",
                    ))),
                    2 => WebsocketSessionUserOpenOutcome::Ready(session(None)),
                    _ => WebsocketSessionUserOpenOutcome::Stopped,
                }
            },
            |_| vec![],
            |_| true,
            &stop,
            Duration::from_millis(1),
            ReconnectBackoff::new(FIRST_WAIT, Duration::from_millis(10)),
            None,
            || {},
            Some(Duration::from_millis(30)),
            None,
        )
    });
    report_on_this_thread(None);
    ended.expect("no refusal");

    assert_eq!(read_at_reopen, [Connecting, Error]);
    let said: Vec<_> = logged
        .iter()
        .filter(|e| e.target == "vike_bridge_core::websocket_session::user")
        .map(|e| (e.level, e.message.as_str()))
        .collect();
    assert_eq!(
        said,
        [
            (Level::INFO, "user-data WS connected"),
            (Level::INFO, "user-data WS closed mid-session — reconnecting"),
            (Level::INFO, "user-data WS reconnected"),
            (Level::WARN, "user-data WS silent stall — reconnecting"),
        ]
    );
}

/// **A silence the ladder spares is a reopen, never a failure** (`ReconnectBackoff::with_silence_spared`, a lane whose
/// venue documents no heartbeat cadence): the watchdog still ends each quiet session at the idle bound and the pump
/// reopens, but every reopen reads `Connecting` with nothing counted, said at `info`.
#[test]
fn a_silence_the_ladder_spares_reopens_at_info_and_reads_connecting() {
    use tracing::Level;
    use vike_model::feed_status::DataClientConnectionState::Connecting;

    let link = fresh_link();
    report_on_this_thread(Some(link.clone()));
    let stop = AtomicBool::new(false);
    let (mut opens, mut read_at_reopen) = (0, Vec::new());
    let (ended, logged) = vike_log::capture::captured(|| {
        run_user(
            || {
                opens += 1;
                if opens > 1 {
                    let state = state(&link);
                    let failed = match &state {
                        VenueLinkState::Down(down) => down.failed_restarts,
                        _ => u32::MAX,
                    };
                    read_at_reopen.push((state.connection_state(), failed));
                }
                match opens {
                    1 | 2 => WebsocketSessionUserOpenOutcome::Ready(Session {
                        closes: None,
                        until: Instant::now(),
                    }),
                    _ => WebsocketSessionUserOpenOutcome::Stopped,
                }
            },
            |_| vec![],
            |_| true,
            &stop,
            Duration::from_millis(1),
            ReconnectBackoff::new(FIRST_WAIT, Duration::from_millis(10)).with_silence_spared(),
            None,
            || {},
            Some(Duration::from_millis(30)),
            None,
        )
    });
    report_on_this_thread(None);
    ended.expect("no refusal");

    assert_eq!(read_at_reopen, [(Connecting, 0), (Connecting, 0)]);
    let said: Vec<_> = logged
        .iter()
        .filter(|e| e.target == "vike_bridge_core::websocket_session::user")
        .map(|e| (e.level, e.message.as_str()))
        .collect();
    assert_eq!(
        said,
        [
            (Level::INFO, "user-data WS connected"),
            (Level::INFO, "user-data WS quiet past its idle bound — reopening"),
            (Level::INFO, "user-data WS reconnected"),
            (Level::INFO, "user-data WS quiet past its idle bound — reopening"),
        ]
    );
}

/// A failed open counts, and the first open that works clears the count with the outage.
#[test]
fn failed_opens_count_up_and_the_next_open_clears_them() {
    let (link, stop, pump) = pump(
        vec![
            Open::Transport("connect: refused"),
            Open::Transport("connect: refused"),
            Open::Ready(None),
        ],
        Duration::from_millis(500),
    );
    wait_for(&link, |s| matches!(s, VenueLinkState::Down(d) if d.failed_restarts == 2));
    wait_for(&link, |s| *s == VenueLinkState::Up);
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap().unwrap();
}

/// **A refused open waits on the refusal mode of the ladder the spec hands the pump, and never ends it** (connection plan
/// B5: oanda's 401 budget is `with_refusal_wait(3, 30 s)`). Two refusals wait the row's short step, an open that works
/// ends the run, and only the third refusal of a new run waits the slow clock: were the mode applied early, or the run
/// not ended by the open, the third refusal after it would never come inside `wait_for`'s ten seconds.
#[test]
fn a_refused_open_waits_on_the_specs_refusal_mode_and_an_open_ends_the_run() {
    let slow = Duration::from_secs(30);
    let (link, stop, pump) = pump_on(
        fresh_link(),
        vec![
            Open::Refused("HTTP 401 on stream open"),
            Open::Refused("HTTP 401 on stream open"),
            Open::Ready(Some("the stream's body ended")),
            Open::Refused("HTTP 401 again"),
        ],
        ReconnectBackoff::new(Duration::from_millis(1), Duration::from_millis(1))
            .with_refusal_wait(3, slow),
    );
    let down = down_of(wait_for(
        &link,
        |s| matches!(s, VenueLinkState::Down(d) if d.failed_restarts == 3),
    ));
    assert_eq!(down.why, "HTTP 401 again");
    assert_eq!(down.effect, REOPENING_EFFECT, "a refusal is retried: {down:?}");
    assert!(
        down.retry_in > slow / 2,
        "the third refusal in a row waits the slow clock, it waits {:?}",
        down.retry_in
    );
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap().expect("a refusal never ends the pump");
}

/// The order path's state and the failed opens its link counts, read at each open after the first, through the real loop
/// over `outcomes` on `backoff` (it stops when they run out), and the pump's lines.
fn readings(
    outcomes: Vec<WebsocketSessionUserOpenOutcome<Session>>,
    backoff: ReconnectBackoff,
) -> (Vec<(vike_model::feed_status::DataClientConnectionState, u32)>, Vec<tracing::Level>) {
    let link = fresh_link();
    report_on_this_thread(Some(link.clone()));
    let stop = AtomicBool::new(false);
    let mut outcomes: VecDeque<_> = outcomes.into();
    let (mut first, mut read) = (true, Vec::new());
    let (ended, logged) = vike_log::capture::captured(|| {
        run_user(
            || {
                if !std::mem::take(&mut first) {
                    let state = state(&link);
                    let failed = match &state {
                        VenueLinkState::Down(down) => down.failed_restarts,
                        _ => 0,
                    };
                    read.push((state.connection_state(), failed));
                }
                outcomes.pop_front().unwrap_or(WebsocketSessionUserOpenOutcome::Stopped)
            },
            |_| vec![],
            |_| true,
            &stop,
            Duration::from_millis(1),
            backoff,
            None,
            || {},
            None,
            None,
        )
    });
    report_on_this_thread(None);
    ended.expect("no key was refused");
    let levels = logged
        .iter()
        .filter(|e| e.target == "vike_bridge_core::websocket_session::user")
        .map(|e| e.level)
        .collect();
    (read, levels)
}

/// **A refusal the venue's refusal mode spares is a reopen, never a failure** (owner, "do", 2026-11-22: one rule in the
/// shared pump). On oanda's mode (`with_refusal_wait(3, …)`) two refused opens read `Connecting` with nothing counted,
/// said at `info`, and the third, which spends the mode, reads `Error` counting all three, said at `warn`.
#[test]
fn a_spared_refusal_reads_connecting_at_info_and_the_one_that_spends_the_mode_reads_error() {
    use tracing::Level;
    use vike_model::feed_status::DataClientConnectionState::{Connecting, Error};
    let ms = Duration::from_millis(1);
    let refused =
        || WebsocketSessionUserOpenOutcome::Refused("HTTP 401 on stream open".to_string());
    let (read, levels) = readings(
        vec![refused(), refused(), refused()],
        ReconnectBackoff::new(ms, ms).with_refusal_wait(3, ms),
    );
    assert_eq!(read, [(Connecting, 0), (Connecting, 0), (Error, 3)]);
    assert_eq!(levels, [Level::INFO, Level::INFO, Level::WARN]);
}

/// **A transport failure reads `Error` at once, and a spared refusal after it does not undo it**; with no refusal mode a
/// refusal is a failed open.
#[test]
fn a_transport_failure_reads_error_and_a_spared_refusal_after_it_keeps_it() {
    use vike_model::feed_status::DataClientConnectionState::Error;
    let ms = Duration::from_millis(1);
    let refused =
        || WebsocketSessionUserOpenOutcome::Refused("HTTP 401 on stream open".to_string());
    let transport = WebsocketSessionUserOpenOutcome::Transport("connect: refused".to_string());
    let (read, _) = readings(
        vec![transport, refused()],
        ReconnectBackoff::new(ms, ms).with_refusal_wait(3, ms),
    );
    assert_eq!(read, [(Error, 1), (Error, 2)]);
    let (read, _) = readings(vec![refused()], ReconnectBackoff::new(ms, ms));
    assert_eq!(read, [(Error, 1)], "no refusal mode: a refusal is a failed open");
}

/// **The venue refusing the key ends the pump, and that reading stays**: the engine reads RUNNING and no fill will ever
/// arrive. Only the next start clears it (the gatewayhub resets the account's cell before it starts the adapter).
#[test]
fn a_refused_key_stays_down_after_the_pump_ended_until_the_next_start() {
    let (link, _stop, pump) = pump(vec![Open::Auth("invalid api key")], Duration::from_millis(1));
    assert!(pump.join().unwrap().is_err());
    let down = down_of(state(&link));
    assert!(down.effect == GAVE_UP_EFFECT && down.retry_in == Duration::ZERO, "{down:?}");
    assert_eq!(down.failed_restarts, 1, "a refusal is a failed open");
    assert_eq!(down.why, "invalid api key");

    venue_link(&link.0).reset();
    assert_eq!(state(&link), VenueLinkState::Unknown);
}

/// **A pump that PANICS reads failed, and that reading stays until the next start**: its lane, dropped while the thread
/// unwinds, gives its path up instead of leaving it as a clean stop does, so the account never reads up (or not known)
/// over a stream that is gone.
#[test]
fn a_pump_that_panics_reads_failed_until_the_next_start() {
    let link = fresh_link();
    let for_thread = link.clone();
    let pump = std::thread::spawn(move || {
        report_on_this_thread(Some(for_thread));
        let mut lane = VenueLinkStreamLane::of_this_thread();
        lane.open();
        panic!("a bug in the frame decoder");
    });
    assert!(pump.join().is_err(), "the pump panicked");
    let down = down_of(state(&link));
    assert_eq!(
        (down.why.as_str(), down.effect, down.failed_restarts, down.retry_in),
        (PANICKED_WHY, PANICKED_EFFECT, 1, Duration::ZERO),
        "a failure, said at once, with nothing reopening it"
    );

    venue_link(&link.0).reset();
    assert_eq!(state(&link), VenueLinkState::Unknown);
}

/// One pump of two reopening is the account's outage, and one that gave up outranks it.
#[test]
fn one_pump_down_is_the_accounts_outage_and_a_refusal_outranks_a_reopen() {
    let link = fresh_link();
    report_on_this_thread(Some(link.clone()));
    let (mut open, mut reopening, mut refused) = (
        VenueLinkStreamLane::of_this_thread(),
        VenueLinkStreamLane::of_this_thread(),
        VenueLinkStreamLane::of_this_thread(),
    );
    report_on_this_thread(None);
    open.open();
    reopening.open();
    refused.open();
    assert_eq!(state(&link), VenueLinkState::Up);
    reopening.down("reset", 0, Duration::from_secs(1));
    assert!(matches!(state(&link), VenueLinkState::Down(d) if d.why == "reset"));
    refused.gave_up("no such key", 1);
    assert!(matches!(state(&link), VenueLinkState::Down(d) if d.effect == GAVE_UP_EFFECT));
}

/// A pump on a thread nobody told writes nothing: the drivers and the capture taps stay as they were.
#[test]
fn a_pump_on_a_thread_nobody_told_writes_nothing() {
    let mut lane = VenueLinkStreamLane::of_this_thread();
    lane.down("reset", 0, Duration::from_secs(1));
    assert!(lane.link.is_none());
}

/// ⚠ **The owner's numbers come from here: one row per session, written by the pump's own transitions.** Through the real
/// loop: a session that closes says the venue's words, a failed open is no session at all, and a stop is ours.
#[test]
fn each_session_of_a_pump_reports_once_and_a_failed_open_reports_nothing() {
    use vike_secrets::link_sessions::{LinkEvent, PLANE_TRADE};

    use crate::link_history::{ENDED_BY_US, reported_for};

    let route = "hyperliquid#PUMP_ROWS";
    reported_for(route);
    let (_link, stop, handle) = pump_on(
        stream_link(route),
        vec![
            Open::Ready(Some("server closed: 1006 no pong")),
            Open::Transport("connection refused"),
            Open::Ready(None),
        ],
        ReconnectBackoff::new(FIRST_WAIT, Duration::from_millis(10)),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while reported_for(route).len() < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.store(true, Ordering::Relaxed);
    handle.join().expect("no panic").expect("no refusal");

    let heard = reported_for(route);
    assert_eq!(heard.len(), 4, "{heard:?}");
    let LinkEvent::Opened { key, .. } = &heard[0] else { panic!("{heard:?}") };
    assert_eq!(
        (key.venue.as_str(), key.route.as_str(), key.plane),
        ("hyperliquid", route, PLANE_TRADE)
    );
    assert!(
        matches!(&heard[1], LinkEvent::Closed { how, ours: false, .. } if how == "server closed: 1006 no pong")
    );
    assert!(matches!(&heard[2], LinkEvent::Opened { .. }));
    assert!(matches!(&heard[3], LinkEvent::Closed { how, ours: true, .. } if how == ENDED_BY_US));
}

/// **The account's cell is the one link cell of its route key** (`crate::venue_link::venue_link`, which every reporting
/// venue writes and the gatewayhub reads), and a bare lane has none.
#[test]
fn one_route_key_one_cell_and_a_bare_lane_has_none() {
    venue_link("binance#LINKONE").reset();
    report_on_this_thread(Some(stream_link("binance#LINKONE")));
    let mut lane = VenueLinkStreamLane::of_this_thread();
    report_on_this_thread(None);
    lane.open();
    assert_eq!(venue_link("binance#LINKONE").state(), VenueLinkState::Up);

    let (tx, _rx) = vike_execution::event_channel(1);
    assert!(VenueLinkStream::of(&tx).is_none());
    assert_eq!(
        VenueLinkStream::of(&tx.routed("binance#LINKONE")).map(|link| state(&link)),
        Some(VenueLinkState::Up)
    );
}

/// A long venue reason is cut before it reaches every observe frame.
#[test]
fn a_long_reason_is_cut() {
    let link = fresh_link();
    report_on_this_thread(Some(link.clone()));
    let mut lane = VenueLinkStreamLane::of_this_thread();
    report_on_this_thread(None);
    lane.down(&"x".repeat(WHY_LIMIT * 2), 0, Duration::ZERO);
    assert_eq!(down_of(state(&link)).why.chars().count(), WHY_LIMIT);
}

/// **Only an open stream reads up, and each way down says what it costs the orders**, in the words the gatewayhub's
/// status verb has always sent for these venues, byte for byte: a pump that never started is not known (ruled U-6),
/// never up, a reopen states its wait (counted down by the cell), and a refused key outranks it with no wait.
#[test]
fn only_an_open_stream_reads_up_and_each_way_down_says_what_it_costs() {
    let link = fresh_link();
    assert_eq!(state(&link), VenueLinkState::Unknown, "no pump: not known");
    report_on_this_thread(Some(link.clone()));
    let (mut reopening, mut refused) =
        (VenueLinkStreamLane::of_this_thread(), VenueLinkStreamLane::of_this_thread());
    report_on_this_thread(None);
    reopening.open();
    refused.open();
    assert_eq!(state(&link), VenueLinkState::Up);

    reopening.down("reset by peer", 2, Duration::from_secs(30));
    let down = down_of(state(&link));
    assert_eq!(
        (down.why.as_str(), down.effect, down.failed_restarts),
        (
            "reset by peer",
            "orders still go out on their own path; fills and venue-side cancels arrive when the stream \
             reopens, which the bridge keeps trying by itself, and what the gap holds is read back",
            2
        )
    );
    assert!(
        down.retry_in <= Duration::from_secs(30) && down.retry_in > Duration::from_secs(25),
        "the wait as of the reading: {:?}",
        down.retry_in
    );

    refused.gave_up("invalid api key", 1);
    assert_eq!(
        state(&link),
        VenueLinkState::Down(VenueLinkDown {
            why: "invalid api key".to_string(),
            effect: "fills and venue-side cancels will NOT arrive, and orders signed with this key may be refused \
                     too: fix the key, then set the account's `account.active` off and on",
            failed_restarts: 1,
            retry_in: Duration::ZERO,
        })
    );
}

/// The account's cell answers the reading its pumps state now.
#[test]
fn the_cell_answers_the_reading_of_its_pumps() {
    let link = fresh_link();
    assert_eq!(state(&link), VenueLinkState::Unknown, "no pump has said anything");
    report_on_this_thread(Some(link.clone()));
    let mut lane = VenueLinkStreamLane::of_this_thread();
    report_on_this_thread(None);
    assert_eq!(down_of(state(&link)).why, "the private stream is opening");
    lane.open();
    assert_eq!(state(&link), VenueLinkState::Up);
}
