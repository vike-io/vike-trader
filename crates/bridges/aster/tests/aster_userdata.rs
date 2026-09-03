//! Offline reliability gates for the Aster listenKey user-data pumps: the venue-neutral
//! `run_user_data_forever` loop, driven by a scripted `UserStream` and the Aster `map_aster_private`
//! decode closure (spot `executionReport` frames — Binance-verbatim). Ported from binance's
//! `r6_binance_userdata.rs`. NO socket, NO listenKey REST — this exercises the pump's reliability
//! contract (lossless decode→emit, bad-JSON tolerance, reconnect-with-backoff on transport drops,
//! auth errors never reconnect-looped, a stop / gone-core is a clean end).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_aster::event_mapper::map_aster_private;
// The shared scripted user-data stream double (testing-arch Phase 4c, `test-support` feature) —
// replaces the "Ported from binance's" inline `Scripted` copy that used to live here.
use vike_bridge_core::scripted::ScriptedUserStream as Scripted;
use vike_bridge_core::user_data::{
    run_user_data_forever, OpenOutcome, StreamError, StreamMsg, UserDataAuthError,
};
use vike_model::events::Event;

/// One Aster spot `executionReport` frame (Binance-verbatim shape: `x` drives the mapping).
fn exec_report(coid: &str, x: &str) -> String {
    serde_json::json!({"e": "executionReport", "s": "BTCUSDT", "c": coid,
                       "S": "BUY", "T": 1, "i": 9, "x": x, "X": x})
    .to_string()
}

#[test]
fn pump_decodes_reconnects_and_stops() {
    // session 1: one good frame, junk tolerated, ping answered, then a transport drop;
    // session 2 (after reconnect): another frame, then stop flag ends it cleanly.
    let stop = AtomicBool::new(false);
    let mut sessions = vec![
        Scripted::new(vec![
            Ok(StreamMsg::Text(exec_report("a1", "NEW"))),
            Ok(StreamMsg::Text("not json {".into())),
            Ok(StreamMsg::Ping(vec![1])),
            Err(StreamError::Timeout),
            Err(StreamError::Closed("drop".into())),
        ]),
        Scripted::new(vec![Ok(StreamMsg::Text(exec_report("a2", "CANCELED")))]),
    ];
    sessions.reverse(); // pop() takes session 1 first
    let mut opens = 0;
    let mut emitted: Vec<String> = Vec::new();
    let stop_ref = &stop;
    let result = run_user_data_forever(
        || {
            opens += 1;
            match sessions.pop() {
                Some(s) => OpenOutcome::Ready(s),
                None => OpenOutcome::Stopped, // script done — end the loop
            }
        },
        |frame| map_aster_private(frame, "aster", "BTCUSDT"),
        |event| {
            if let Event::OrderCanceled(_) = &event {
                stop_ref.store(true, Ordering::Relaxed); // stop after the 2nd session's event
            }
            emitted.push(format!("{event:?}"));
            true
        },
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(4), // tiny backoff so the reconnect sleep is fast
        None,
        || {},
    );
    assert!(result.is_ok());
    assert_eq!(opens, 2, "transport drop must reconnect exactly once");
    assert_eq!(emitted.len(), 2, "one event per good frame: {emitted:?}");
    assert!(emitted[0].contains("OrderAccepted"), "{emitted:?}");
    assert!(emitted[1].contains("OrderCanceled"), "{emitted:?}");
}

/// Audit A3 closure: the pump fires `on_reconnect` on a RE-open only (not the first open), so a
/// supervisor can trigger a post-reconnect resync. Session 1 drops, session 2 opens → exactly one
/// reconnect callback.
#[test]
fn pump_fires_on_reconnect_only_on_reopen() {
    let stop = AtomicBool::new(false);
    let mut sessions = vec![
        Scripted::new(vec![
            Ok(StreamMsg::Text(exec_report("a1", "NEW"))),
            Err(StreamError::Closed("drop".into())),
        ]),
        Scripted::new(vec![Ok(StreamMsg::Text(exec_report("a2", "CANCELED")))]),
    ];
    sessions.reverse();
    let mut reconnects = 0;
    let stop_ref = &stop;
    let result = run_user_data_forever(
        || match sessions.pop() {
            Some(s) => OpenOutcome::Ready(s),
            None => OpenOutcome::Stopped,
        },
        |frame| map_aster_private(frame, "aster", "BTCUSDT"),
        |event| {
            if let Event::OrderCanceled(_) = &event {
                stop_ref.store(true, Ordering::Relaxed);
            }
            true
        },
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(4),
        None,
        || reconnects += 1,
    );
    assert!(result.is_ok());
    assert_eq!(reconnects, 1, "on_reconnect fires once — on the re-open, NOT the first open");
}

#[test]
fn pump_auth_error_never_reconnects() {
    let stop = AtomicBool::new(false);
    let mut opens = 0;
    let result = run_user_data_forever::<Scripted>(
        || {
            opens += 1;
            OpenOutcome::Auth(UserDataAuthError("listenKey create: bad sig".into()))
        },
        |_| Vec::new(),
        |_| true,
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(2),
        None,
        || {},
    );
    assert_eq!(result, Err(UserDataAuthError("listenKey create: bad sig".into())));
    assert_eq!(opens, 1, "auth errors must NOT reconnect-loop");
}

#[test]
fn pump_core_gone_ends_pump() {
    let stop = AtomicBool::new(false);
    let mut sessions = vec![Scripted::new(vec![Ok(StreamMsg::Text(exec_report("a1", "NEW")))])];
    let result = run_user_data_forever(
        || match sessions.pop() {
            Some(s) => OpenOutcome::Ready(s),
            None => OpenOutcome::Stopped,
        },
        |frame| map_aster_private(frame, "aster", "BTCUSDT"),
        |_| false, // core channel closed
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(2),
        None,
        || {},
    );
    assert!(result.is_ok(), "a gone core is a clean end, not an error");
}
