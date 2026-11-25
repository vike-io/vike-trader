//! [`run_user_with_notice`]'s planned reconnect over scripted sessions on a virtual clock: a notice the venue's hook
//! answers ends the session as a reopen, at once after a long session and on the ladder after a young one.
use super::*;
use crate::pump_clock::VirtualClock;
use crate::scripted::{UserScriptStep, WebsocketSessionUserStreamScript};
use crate::venue_link::{VenueLinkState, venue_link};
use stream_link::{report_on_this_thread, stream_link};
use vike_model::feed_status::DataClientConnectionState;

const S: Duration = Duration::from_secs(1);

/// A venue's notice hook: `{"closing": …}` is its announced end.
fn notice(frame: &Value) -> Option<String> {
    frame.get("closing").and_then(Value::as_str).map(|why| format!("the venue announced: {why}"))
}

fn text(frame: &str) -> UserScriptStep {
    UserScriptStep::Read(Ok(WebsocketSessionUserMessage::Text(frame.to_string())))
}

/// Three sessions on the 1 s -> 30 s ladder: the first lives twice the ceiling and is announced closed, the second is
/// announced closed at once, the third ends the run. The first reopen comes at the notice itself, the second the
/// ladder's first step later (a venue announcing the end of every new session never loops hot); the order path reads
/// `Connecting` in the notice's words at each, never `Error`; the frame the hook answers is never decoded.
#[test]
fn an_announced_close_reopens_at_once_after_a_long_session_and_on_the_ladder_after_a_young_one() {
    let (clock, max, key) = (VirtualClock::new(1), 30 * S, "announced-close-tests");
    let notice_frame = r#"{"closing":"upgrade"}"#;
    let mut sessions = vec![
        vec![text(r#"{"other":1}"#), UserScriptStep::Stall(2 * max), text(notice_frame)],
        vec![text(notice_frame)],
        vec![UserScriptStep::Stall(S)],
    ]
    .into_iter();
    let stop = Arc::new(AtomicBool::new(false));
    let pump = std::thread::spawn(move || {
        let _seat = clock.enter();
        report_on_this_thread(Some(stream_link(key)));
        let (mut opens, decoded, mut reconnects) =
            (Vec::new(), std::cell::RefCell::new(Vec::new()), 0);
        let ended = run_user_with_notice(
            || {
                let link = venue_link(key).state();
                let why = match &link {
                    VenueLinkState::Down(down) => down.why.clone(),
                    _ => String::new(),
                };
                opens.push((clock.offset(clock.now()), link.connection_state(), why));
                let Some(steps) = sessions.next() else {
                    return WebsocketSessionUserOpenOutcome::Stopped;
                };
                let mut stream = WebsocketSessionUserStreamScript::from_steps(steps);
                if sessions.as_slice().is_empty() {
                    stream = stream.stopping(Arc::clone(&stop));
                }
                WebsocketSessionUserOpenOutcome::Ready(stream)
            },
            |frame| {
                decoded.borrow_mut().push(frame.clone());
                Vec::new()
            },
            Some(notice),
            |_| true,
            &stop,
            USER_DATA_POLL,
            ReconnectBackoff::new(S, max),
            None,
            || reconnects += 1,
            None,
            None,
        );
        (ended, opens, decoded.into_inner(), reconnects)
    });
    let (ended, opens, decoded, reconnects) = pump.join().expect("the pump thread");
    assert!(ended.is_ok(), "{ended:?}");
    let at: Vec<Duration> = opens.iter().map(|(at, ..)| *at).collect();
    assert_eq!(at, [Duration::ZERO, 2 * max, 2 * max + S], "at once, then the ladder's first step");
    for (_, state, why) in &opens[1..] {
        assert_eq!(*state, DataClientConnectionState::Connecting, "a planned reopen ({why})");
        assert!(why.contains("the venue announced: upgrade"), "the notice's words: {why}");
    }
    assert_eq!(reconnects, 2, "on_reconnect at each reopen");
    assert_eq!(
        decoded,
        [serde_json::json!({"other": 1})],
        "the notice frame is the hook's, not the decoder's"
    );
}
