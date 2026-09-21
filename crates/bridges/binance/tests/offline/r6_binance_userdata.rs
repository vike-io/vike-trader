//! R6 slice-2 offline gates: ws_auth signature/subscribe parity against the FROZEN
//! `fixtures/r6/ws_auth.json` bytes, the ack matcher's handshake semantics, and the venue-neutral
//! pump's reliability contract (lossless decode→emit, bad-JSON tolerance, reconnect-with-backoff
//! on transport drops, auth errors never reconnect-looped, stop is clean).
//!
//! That fixture was exported from the PySide6 vterminal app before it was retired: PROVENANCE, not
//! a live comparison. The first clause read "parity vs the Python oracle"; no exporter survives in
//! this tree and nothing here consults Python at run time, so what the exact comparison claims is
//! that THIS port's signature and frame bytes have not moved unnoticed
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). The other gates listed
//! above hold no fixture at all — they drive scripted streams and assert behaviour, so they were
//! never a cross-implementation comparison in the first place.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_binance::event_mapper::map_binance_private;
use vike_binance::ws_auth::{
    AckResult, binance_ws_sign, build_subscribe_request, match_subscribe_ack,
};
// The shared scripted user-data stream double (testing-arch Phase 4c, `test-support` feature) —
// replaces the inline `Scripted` copy that used to live here (and that aster/bridge_conformance
// had each re-copied).
use vike_bridge_core::scripted::ScriptedUserStream as Scripted;
use vike_bridge_core::user_data::{
    OpenOutcome, StreamError, StreamMsg, UserDataAuthError, run_user_data_forever,
};
use vike_model::events::Event;

fn fixture(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/r6").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[test]
fn ws_auth_signature_and_request_parity() {
    let fx = fixture("ws_auth.json");
    let secret = fx["api_secret"].as_str().unwrap();
    for (i, case) in fx["sign_cases"].as_array().unwrap().iter().enumerate() {
        let owned: Vec<(String, String)> = case["params"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string()))
            .collect();
        let pairs: Vec<(&str, String)> =
            owned.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        assert_eq!(
            binance_ws_sign(secret, &pairs),
            case["signature"].as_str().unwrap(),
            "sign case {i}"
        );
    }
    let request = build_subscribe_request(
        fx["api_key"].as_str().unwrap(),
        secret,
        fx["now_ms"].as_i64().unwrap(),
        5000,
        "fixedreqid123",
    );
    assert_eq!(request, fx["subscribe_request"], "full subscribe request");
}

#[test]
fn ack_matcher_handshake_semantics() {
    let ok = serde_json::json!({"id": "rq1", "status": 200, "result": {}});
    let wrong_id = serde_json::json!({"id": "other", "status": 200});
    let bad_status = serde_json::json!({"id": "rq1", "status": 401});
    let with_error =
        serde_json::json!({"id": "rq1", "status": 200, "error": {"code": -1022, "msg": "bad sig"}});
    let not_dict = serde_json::json!(["x"]);
    assert_eq!(match_subscribe_ack(&ok, "rq1"), AckResult::Ok);
    assert_eq!(match_subscribe_ack(&wrong_id, "rq1"), AckResult::NotOurs);
    assert_eq!(match_subscribe_ack(&not_dict, "rq1"), AckResult::NotOurs);
    assert!(matches!(match_subscribe_ack(&bad_status, "rq1"), AckResult::Err(_)));
    match match_subscribe_ack(&with_error, "rq1") {
        AckResult::Err(msg) => assert_eq!(msg, "Binance WS subscribe failed: bad sig"),
        other => panic!("expected Err, got {other:?}"),
    }
}

/// Audit A3: a transient ack failure (clock skew -1021, rate limit -1003) must classify as
/// TransientErr (→ reconnect), NOT Err (→ surface + stop). A genuine bad key (-2015) stays Err.
#[test]
fn ack_matcher_classifies_transient_codes() {
    let ts_skew = serde_json::json!({"id": "rq1", "status": 400, "error": {"code": -1021, "msg": "recvWindow"}});
    let rate = serde_json::json!({"id": "rq1", "status": 429, "error": {"code": -1003, "msg": "too many"}});
    let bad_key = serde_json::json!({"id": "rq1", "status": 401, "error": {"code": -2015, "msg": "invalid key"}});
    assert!(matches!(match_subscribe_ack(&ts_skew, "rq1"), AckResult::TransientErr(_)));
    assert!(matches!(match_subscribe_ack(&rate, "rq1"), AckResult::TransientErr(_)));
    assert!(matches!(match_subscribe_ack(&bad_key, "rq1"), AckResult::Err(_)));
}

fn exec_report(coid: &str, x: &str) -> String {
    serde_json::json!({"e": "executionReport", "s": "BTCUSDT", "c": coid,
                       "T": 1, "i": 9, "x": x})
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
        |frame| map_binance_private(frame, "binance", "BTCUSDT"),
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

/// Audit A3 full closure: the pump fires `on_reconnect` on a RE-open only (not the first open),
/// so a supervisor can trigger a post-reconnect resync. Here: session 1 drops, session 2 opens →
/// exactly one reconnect callback.
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
        |frame| map_binance_private(frame, "binance", "BTCUSDT"),
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
            OpenOutcome::Auth(UserDataAuthError("Binance WS subscribe failed: bad sig".into()))
        },
        |_| Vec::new(),
        |_| true,
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(2),
        None,
        || {},
    );
    assert_eq!(result, Err(UserDataAuthError("Binance WS subscribe failed: bad sig".into())));
    assert_eq!(opens, 1, "auth errors must NOT reconnect-loop");
}

#[test]
fn pump_transport_failures_backoff_until_stop() {
    let stop = AtomicBool::new(false);
    let mut opens = 0;
    let stop_ref = &stop;
    let result = run_user_data_forever::<Scripted>(
        || {
            opens += 1;
            if opens >= 3 {
                stop_ref.store(true, Ordering::Relaxed);
            }
            OpenOutcome::Transport("refused".into())
        },
        |_| Vec::new(),
        |_| true,
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(4),
        None,
        || {},
    );
    assert!(result.is_ok(), "transport failures end cleanly on stop");
    assert!(opens >= 3, "kept retrying with backoff: {opens}");
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
        |frame| map_binance_private(frame, "binance", "BTCUSDT"),
        |_| false, // core channel closed
        &stop,
        Duration::from_millis(1),
        Duration::from_millis(2),
        None,
        || {},
    );
    assert!(result.is_ok(), "a gone core is a clean end, not an error");
}
