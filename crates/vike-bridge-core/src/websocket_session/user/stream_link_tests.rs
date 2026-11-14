//! What the shared pump says of its stream, through the real loop over scripted sessions.

use std::collections::VecDeque;

use super::super::*;
use super::*;

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
    Auth(&'static str),
}

/// Runs the loop over `script`, on a thread told to report to a link of its own. The last open repeats for ever.
fn pump(
    script: Vec<Open>,
    backoff: Duration,
) -> (
    VenueLinkStream,
    Arc<AtomicBool>,
    std::thread::JoinHandle<Result<(), WebsocketSessionUserAuthenticationError>>,
) {
    pump_on(VenueLinkStream::default(), script, backoff)
}

/// [`pump`] on a given link; a link with a route key also writes the history.
fn pump_on(
    link: VenueLinkStream,
    script: Vec<Open>,
    backoff: Duration,
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

fn wait_for(
    link: &VenueLinkStream,
    wanted: impl Fn(&VenueLinkStreamState) -> bool,
) -> VenueLinkStreamState {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let state = link.state();
        if wanted(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "the reading never came: {state:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// **A stream that closes reads down with the words it closed on, and open again once the pump reopened it.** The wait
/// is the backoff's, so `trade health` can say when the next try comes.
#[test]
fn a_stream_that_closes_reads_down_and_a_reopen_ends_it() {
    let (link, stop, pump) =
        pump(vec![Open::Ready(Some("reset by peer")), Open::Ready(None)], Duration::from_secs(1));
    let VenueLinkStreamState::Down(down) =
        wait_for(&link, |s| matches!(s, VenueLinkStreamState::Down(d) if d.why == "reset by peer"))
    else {
        unreachable!()
    };
    assert_eq!(down.failed_opens, 0, "the session closed; no open failed");
    assert!(down.retry_at.is_some() && !down.gave_up, "{down:?}");
    wait_for(&link, |s| *s == VenueLinkStreamState::Open);
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap().unwrap();
    assert_eq!(
        link.state(),
        VenueLinkStreamState::NotRunning,
        "a pump that stopped leaves no reading"
    );
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
    let next_try = |why: &str| {
        let VenueLinkStreamState::Down(down) =
            wait_for(&link, |s| matches!(s, VenueLinkStreamState::Down(d) if d.why == why))
        else {
            unreachable!()
        };
        down.retry_at.expect("it opens again")
    };
    let first = next_try("closed at once");
    let second = next_try("closed at once again");
    // The second close comes at least the first wait (1 s) later, so this is the second wait's half past that.
    assert!(second - first >= ceiling, "the wait grew: {:?}", second - first);
    let lived = next_try("lived");
    let wait = lived.saturating_duration_since(Instant::now());
    assert!(wait <= USER_DATA_INITIAL_BACKOFF, "the wait started over, it is {wait:?}");
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap().unwrap();
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
    wait_for(&link, |s| matches!(s, VenueLinkStreamState::Down(d) if d.failed_opens == 2));
    wait_for(&link, |s| *s == VenueLinkStreamState::Open);
    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap().unwrap();
}

/// **The venue refusing the key ends the pump, and that reading stays**: the engine reads RUNNING and no fill will ever
/// arrive. Only the next start clears it.
#[test]
fn a_refused_key_stays_down_after_the_pump_ended_until_the_next_start() {
    let (link, _stop, pump) = pump(vec![Open::Auth("invalid api key")], Duration::from_millis(1));
    assert!(pump.join().unwrap().is_err());
    let VenueLinkStreamState::Down(down) = link.state() else { panic!("{:?}", link.state()) };
    assert!(down.gave_up && down.retry_at.is_none(), "{down:?}");
    assert_eq!(down.failed_opens, 1, "a refusal is a failed open");
    assert_eq!(down.why, "invalid api key");

    link.reset();
    assert_eq!(link.state(), VenueLinkStreamState::NotRunning);
}

/// One pump of two reopening is the account's outage, and one that gave up outranks it.
#[test]
fn one_pump_down_is_the_accounts_outage_and_a_refusal_outranks_a_reopen() {
    let link = VenueLinkStream::default();
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
    assert_eq!(link.state(), VenueLinkStreamState::Open);
    reopening.down("reset", 0, Duration::from_secs(1));
    assert!(matches!(link.state(), VenueLinkStreamState::Down(d) if d.why == "reset"));
    refused.gave_up("no such key", 1);
    assert!(matches!(link.state(), VenueLinkStreamState::Down(d) if d.gave_up));
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
        Duration::from_millis(10),
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

/// The account's cell is one cell, and a bare lane has none.
#[test]
fn one_route_key_one_cell_and_a_bare_lane_has_none() {
    stream_link("binance#LINKONE").reset();
    report_on_this_thread(Some(stream_link("binance#LINKONE")));
    let mut lane = VenueLinkStreamLane::of_this_thread();
    report_on_this_thread(None);
    lane.open();
    assert_eq!(stream_link("binance#LINKONE").state(), VenueLinkStreamState::Open);

    let (tx, _rx) = vike_execution::event_channel(1);
    assert!(VenueLinkStream::of(&tx).is_none());
    assert_eq!(
        VenueLinkStream::of(&tx.routed("binance#LINKONE")).map(|l| l.state()),
        Some(VenueLinkStreamState::Open)
    );
}

/// A long venue reason is cut before it reaches every observe frame.
#[test]
fn a_long_reason_is_cut() {
    let link = VenueLinkStream::default();
    report_on_this_thread(Some(link.clone()));
    let mut lane = VenueLinkStreamLane::of_this_thread();
    report_on_this_thread(None);
    lane.down(&"x".repeat(WHY_LIMIT * 2), 0, Duration::ZERO);
    let VenueLinkStreamState::Down(down) = link.state() else { panic!() };
    assert_eq!(down.why.chars().count(), WHY_LIMIT);
}

/// **Only an open stream reads up, and each way down says what it costs the orders**, in the words the gatewayhub's
/// status verb has always sent for these venues, byte for byte: a pump that never started is not known (ruled U-6),
/// never up, and the wait is counted from the reading.
#[test]
fn only_an_open_stream_reads_up_and_each_way_down_says_what_it_costs() {
    let now = Instant::now();
    assert_eq!(venue_link_state_of(VenueLinkStreamState::Open, now), VenueLinkState::Up);
    assert_eq!(venue_link_state_of(VenueLinkStreamState::NotRunning, now), VenueLinkState::Unknown);

    let reopening = VenueLinkStreamDown {
        why: "reset by peer".to_string(),
        failed_opens: 2,
        retry_at: Some(now + Duration::from_secs(30)),
        gave_up: false,
    };
    assert_eq!(
        venue_link_state_of(VenueLinkStreamState::Down(reopening.clone()), now),
        VenueLinkState::Down(VenueLinkDown {
            why: "reset by peer".to_string(),
            effect: "orders still go out on their own path; fills and venue-side cancels arrive when the stream \
                     reopens, which the bridge keeps trying by itself, and what the gap holds is read back",
            failed_restarts: 2,
            retry_in: Duration::from_secs(30),
        })
    );
    let VenueLinkState::Down(due) =
        venue_link_state_of(VenueLinkStreamState::Down(reopening), now + Duration::from_secs(31))
    else {
        unreachable!()
    };
    assert_eq!(due.retry_in, Duration::ZERO, "a try that is due is no wait");

    let refused = VenueLinkStreamDown {
        why: "invalid api key".to_string(),
        failed_opens: 1,
        retry_at: None,
        gave_up: true,
    };
    assert_eq!(
        venue_link_state_of(VenueLinkStreamState::Down(refused), now),
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
    let link = VenueLinkStream::default();
    assert_eq!(link.venue_link_state(), VenueLinkState::Unknown, "no pump has said anything");
    report_on_this_thread(Some(link.clone()));
    let mut lane = VenueLinkStreamLane::of_this_thread();
    report_on_this_thread(None);
    let VenueLinkState::Down(opening) = link.venue_link_state() else { panic!() };
    assert_eq!(opening.why, "the private stream is opening");
    lane.open();
    assert_eq!(link.venue_link_state(), VenueLinkState::Up);
}
