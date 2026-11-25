//! [`run_pump`] over scripted gateway sessions on a virtual clock (`vike_bridge_core::pump_clock`'s `VirtualClock`): the
//! waits, the `tic` and tickle, the idle limit and the resync on a re-open, at the row's real durations, with no socket.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use vike_bridge_core::pump_clock::VirtualClock;
use vike_bridge_core::scripted::{
    UserScriptHeard, UserScriptLog, UserScriptStep, WebsocketSessionUserStreamScript,
};
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserMessage, WebsocketSessionUserOpenOutcome,
};

use super::{IDLE_LIMIT, IbkrInbound, REDIAL_AFTER_DROP, TIC_EVERY, run_pump};

/// One open of a scripted gateway: a session that plays its steps, or a dial that fails.
enum Open {
    Session(Vec<UserScriptStep>),
    Fails,
}

fn text(frame: serde_json::Value) -> UserScriptStep {
    UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Text(frame.to_string())))
}

/// A `sor` status row of `order_ref`, as `decode_websocket_frame` reads it.
fn status(order_ref: &str) -> UserScriptStep {
    text(json!({ "topic": "sor", "args": [{ "order_ref": order_ref, "status": "Submitted" }] }))
}

/// What one run saw, each time on the virtual clock from its birth.
struct Run {
    /// When each open was asked for.
    opens: Vec<Duration>,
    /// Each text frame the pump sent on a session.
    sent: Vec<(Duration, String)>,
    /// Each REST tickle.
    tickles: Vec<Duration>,
    /// What reached the exec loop, in order.
    inbounds: Vec<String>,
}

/// The pump over `opens`; the last session raises the pump's stop once it is drained.
fn run(opens: Vec<Open>) -> Run {
    let clock = VirtualClock::new(1);
    let _seat = clock.enter();
    let since_birth = {
        let clock = Arc::clone(&clock);
        move || clock.offset(clock.now())
    };
    let stop = Arc::new(AtomicBool::new(false));
    let log = UserScriptLog::default();
    let tickles = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel();
    let last = opens.len() - 1;
    let mut script = opens.into_iter().enumerate();
    let mut asked = Vec::new();
    run_pump(
        || {
            asked.push(since_birth());
            match script.next().expect("the script names the last open") {
                (i, Open::Session(steps)) => {
                    let session = WebsocketSessionUserStreamScript::from_steps(steps)
                        .logged(Arc::clone(&log));
                    WebsocketSessionUserOpenOutcome::Ready(if i == last {
                        session.stopping(Arc::clone(&stop))
                    } else {
                        session
                    })
                }
                (_, Open::Fails) => WebsocketSessionUserOpenOutcome::Transport("refused".into()),
            }
        },
        |session: &mut WebsocketSessionUserStreamScript| {
            let _ = session.send_text("tic");
        },
        {
            let (tickles, since_birth) = (Arc::clone(&tickles), since_birth.clone());
            move || tickles.lock().unwrap().push(since_birth())
        },
        &tx,
        &stop,
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
    let inbounds = rx
        .try_iter()
        .map(|inbound| match inbound {
            IbkrInbound::OrderStatus(s) => format!("status {}", s.order_ref),
            IbkrInbound::StreamResync => "resync".to_string(),
            _ => "other".to_string(),
        })
        .collect();
    let tickles = tickles.lock().unwrap().clone();
    Run { opens: asked, sent, tickles, inbounds }
}

/// A drop mid-stream dials again after the local 1 s and a failed dial after the row's fixed 2 s, the sequence of the
/// pump's own loop before it moved onto `run_user`. The resync goes out on the RE-open, between the two sessions'
/// frames, never on the first open and never on a failed one.
#[test]
fn a_drop_redials_after_one_second_a_failed_dial_after_two_and_the_reopen_resyncs_once() {
    let s = Duration::from_secs;
    let run = run(vec![
        Open::Session(vec![
            status("a"),
            UserScriptStep::Close { code: 1000, reason: "bye".into() },
        ]),
        Open::Fails,
        Open::Session(vec![status("b")]),
    ]);
    assert_eq!(REDIAL_AFTER_DROP, s(1));
    assert_eq!(run.opens, [s(0), s(1), s(3)]);
    assert_eq!(run.inbounds, ["status a", "resync", "status b"]);
}

/// A socket that stays open and says nothing is called dead at the row's idle limit, and the session that replaces it
/// resyncs. While it was up the `tic` went out at the row's cadence, with the REST tickle beside it.
#[test]
fn a_silent_session_ends_at_the_idle_limit_with_the_tic_and_the_tickle_on_their_cadence() {
    let run = run(vec![
        Open::Session(vec![UserScriptStep::Stall(IDLE_LIMIT * 2)]),
        Open::Session(vec![]),
    ]);
    assert_eq!(run.opens, [Duration::ZERO, IDLE_LIMIT + REDIAL_AFTER_DROP]);
    assert_eq!(run.sent, [(TIC_EVERY, "tic".to_string())]);
    assert_eq!(run.tickles, [TIC_EVERY]);
    assert_eq!(run.inbounds, ["resync"]);
}

/// The gateway's documented `system` heartbeat, one every 10 s, keeps a quiet account's session up past the idle limit:
/// the limit counts any frame, and the `tic` is answered by none.
#[test]
fn the_ten_second_heartbeat_keeps_a_quiet_session_up_past_the_idle_limit() {
    let beats = 12;
    let mut steps = Vec::new();
    for n in 0..beats {
        steps.push(text(json!({ "topic": "system", "hb": 1_700_000_000_000_i64 + n * 10_000 })));
        steps.push(UserScriptStep::Stall(Duration::from_secs(10)));
    }
    steps.push(UserScriptStep::Close { code: 1000, reason: "bye".into() });
    let run = run(vec![Open::Session(steps), Open::Session(vec![])]);
    let lived = Duration::from_secs(10) * u32::try_from(beats).unwrap();
    assert!(lived > IDLE_LIMIT);
    assert_eq!(run.opens, [Duration::ZERO, lived + REDIAL_AFTER_DROP]);
    assert_eq!(run.inbounds, ["resync"]);
}
