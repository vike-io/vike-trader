use super::*;
use serde_json::json;
use std::assert_matches;
use std::cell::RefCell;
use std::collections::VecDeque;

// --- request body shape: first call null, then echo the venue id ---------------------------

#[test]
fn body_shape_first_call_null_then_echoes_the_id() {
    let mut st = HeartbeatState::new();
    // First beat opens the session with an explicit JSON null.
    let b0 = st.body();
    assert!(b0["heartbeat_id"].is_null(), "first beat sends heartbeat_id: null");
    assert!(st.heartbeat_id().is_none());
    // After a valid beat, subsequent beats echo the id verbatim.
    st.apply(&BeatOutcome::Ok("hb-uuid-1".into()));
    let b1 = st.body();
    assert_eq!(b1["heartbeat_id"], "hb-uuid-1");
    assert_eq!(st.heartbeat_id(), Some("hb-uuid-1"));
}

// --- the pure classifier: success / resync / error ------------------------------------------

#[test]
fn interpret_classifies_success_resync_and_error() {
    // 200 with an id → Ok (the first-beat success shape).
    assert_eq!(
        interpret_beat(200, &json!({ "heartbeat_id": "u1", "error": null })),
        BeatOutcome::Ok("u1".into())
    );
    // The documented 400 → Resync carrying the CORRECT id.
    assert_eq!(
        interpret_beat(400, &json!({ "error": "Invalid Heartbeat ID", "heartbeat_id": "u2" })),
        BeatOutcome::Resync("u2".into())
    );
    // 2xx with no usable id → Err (defensive; leaves state alone).
    assert_matches!(interpret_beat(200, &json!({ "error": null })), BeatOutcome::Err(_));
    assert_matches!(interpret_beat(200, &json!({ "heartbeat_id": "" })), BeatOutcome::Err(_));
    // A non-2xx with no corrected id (e.g. an auth failure) → Err, not Resync.
    assert_matches!(interpret_beat(401, &json!({ "error": "unauthorized" })), BeatOutcome::Err(_));
}

/// A scripted transport: replies are popped in order; every posted body is recorded so a test
/// can assert exactly what the beat put on the wire.
struct Scripted {
    calls: RefCell<Vec<Value>>,
    replies: RefCell<VecDeque<Result<(u16, Value), String>>>,
}
impl Scripted {
    fn new(replies: Vec<Result<(u16, Value), String>>) -> Self {
        Scripted { calls: RefCell::new(Vec::new()), replies: RefCell::new(replies.into()) }
    }
}
impl HeartbeatTransport for Scripted {
    fn post_heartbeat(&self, body: &Value) -> Result<(u16, Value), String> {
        self.calls.borrow_mut().push(body.clone());
        self.replies.borrow_mut().pop_front().expect("a scripted reply for each beat")
    }
}

// --- the 400-resync arm: the NEXT beat uses the corrected id ---------------------------------

#[test]
fn resync_then_the_retry_uses_the_corrected_id() {
    // First POST → 400 Invalid-Heartbeat-ID(correct-id); the immediate retry → 200 echo.
    let scripted = Scripted::new(vec![
        Ok((400, json!({ "error": "Invalid Heartbeat ID", "heartbeat_id": "correct-id" }))),
        Ok((200, json!({ "heartbeat_id": "correct-id", "error": null }))),
    ]);
    // Start with a STALE id so the first beat is the one that gets rejected.
    let mut st = HeartbeatState::new();
    st.apply(&BeatOutcome::Ok("stale-id".into()));

    let report = beat_once(&scripted, &mut st);
    assert_eq!(report, BeatReport::Beat("correct-id".into()));

    let calls = scripted.calls.borrow();
    assert_eq!(calls.len(), 2, "one rejected beat + one in-tick retry");
    assert_eq!(calls[0]["heartbeat_id"], "stale-id", "the first beat sent the stale id");
    assert_eq!(calls[1]["heartbeat_id"], "correct-id", "the retry sent the venue's corrected id");
    assert_eq!(st.heartbeat_id(), Some("correct-id"), "rolling id resynced to the venue's");
}

#[test]
fn two_resyncs_in_a_row_store_the_newest_id_for_next_tick() {
    let scripted = Scripted::new(vec![
        Ok((400, json!({ "error": "Invalid Heartbeat ID", "heartbeat_id": "id-b" }))),
        Ok((400, json!({ "error": "Invalid Heartbeat ID", "heartbeat_id": "id-c" }))),
    ]);
    let mut st = HeartbeatState::new();
    st.apply(&BeatOutcome::Ok("id-a".into()));
    let report = beat_once(&scripted, &mut st);
    assert_eq!(report, BeatReport::ResyncPending, "gave up this tick after the bounded retry");
    assert_eq!(scripted.calls.borrow().len(), 2, "bounded to two POSTs, no infinite loop");
    assert_eq!(st.heartbeat_id(), Some("id-c"), "the newest corrected id is stored for next tick");
}

#[test]
fn a_network_error_is_failed_and_keeps_the_rolling_id() {
    // A failed beat must NOT lose the id — the next tick retries with it (fail-soft).
    let scripted = Scripted::new(vec![Err("network: connection refused".into())]);
    let mut st = HeartbeatState::new();
    st.apply(&BeatOutcome::Ok("keep-me".into()));
    let report = beat_once(&scripted, &mut st);
    assert_matches!(report, BeatReport::Failed(_));
    assert_eq!(st.heartbeat_id(), Some("keep-me"), "a failed beat leaves the id for the retry");
}

#[test]
fn first_beat_success_arms_from_null() {
    // The whole first-beat happy path: send null → 200 with a fresh id → id adopted.
    let scripted =
        Scripted::new(vec![Ok((200, json!({ "heartbeat_id": "fresh", "error": null })))]);
    let mut st = HeartbeatState::new();
    let report = beat_once(&scripted, &mut st);
    assert_eq!(report, BeatReport::Beat("fresh".into()));
    assert!(scripted.calls.borrow()[0]["heartbeat_id"].is_null(), "opened with null");
    assert_eq!(st.heartbeat_id(), Some("fresh"));
}

// --- the gates: nothing spawns without creds, or unless the caller enables it --------------

/// ENABLED, so the missing creds are the only thing that can stop it.
#[test]
fn spawn_returns_none_without_creds() {
    let h = HeartbeatPoller::spawn(None, true, DEFAULT_BEAT_INTERVAL);
    assert!(h.is_none(), "no creds ⇒ nothing spawned");
}

/// D4 of decision 0095: creds that could sign a beat start nothing unless the caller enables it —
/// no thread, no network call.
#[test]
fn spawn_returns_none_when_not_enabled() {
    let creds = PolymarketCreds {
        api_key: "k".into(),
        secret: "cG9seW1hcmtldA==".into(),
        ..Default::default()
    };
    let h = HeartbeatPoller::spawn(Some(creds), false, DEFAULT_BEAT_INTERVAL);
    assert!(h.is_none(), "not enabled ⇒ nothing spawned");
}
