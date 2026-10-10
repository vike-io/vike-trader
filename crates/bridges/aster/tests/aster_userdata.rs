//! Offline wiring gate for the Aster listenKey user-data pumps: the venue-neutral
//! `run_user_data_forever` loop, driven by a scripted `UserStream` and the Aster `map_aster_private`
//! decode closure (spot `executionReport` frames — Binance-verbatim). NO socket, NO listenKey REST.
//!
//! `pump_decodes_reconnects_and_stops` is the file's one test: it proves `map_aster_private` plugs
//! into the pump as its decode closure (lossless decode→emit, bad-JSON tolerance, reconnect on a
//! transport drop, a stop is a clean end). The pump's other reliability gates (`on_reconnect` fires
//! on a re-open only, an auth error never reconnects, a gone core is a clean end, transport
//! failures back off) exercise the shared loop rather than aster code, so they live once, in
//! `crates/bridges/binance/tests/offline/r6_binance_userdata.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_aster::event_mapper::map_aster_private;
// The shared scripted user-data stream double (testing-arch Phase 4c, `test-support` feature) —
// replaces the "Ported from binance's" inline `Scripted` copy that used to live here.
use vike_bridge_core::scripted::ScriptedUserStream as Scripted;
use vike_bridge_core::user_data::{OpenOutcome, StreamError, StreamMsg, run_user_data_forever};
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
