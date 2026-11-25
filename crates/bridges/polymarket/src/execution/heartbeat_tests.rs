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
    // A non-2xx's EMPTY expected id is the venue's word too (PM-6) → Resync to it.
    assert_eq!(
        interpret_beat(400, &json!({ "error_msg": "Invalid Heartbeat ID", "heartbeat_id": "" })),
        BeatOutcome::Resync(String::new())
    );
    // A non-2xx with no corrected id (e.g. an auth failure) → Err, not Resync, worded by the body's summary
    // (`vike_bridge_core::transport::body_summary`): the CLOB's `error` sentence, else the body's size.
    assert_eq!(
        interpret_beat(401, &json!({ "error": " unauthorized " })),
        BeatOutcome::Err("heartbeat 401: unauthorized".into())
    );
    assert_eq!(
        interpret_beat(500, &json!({ "detail": "x" })),
        BeatOutcome::Err("heartbeat 500: 14-byte body not shown".into())
    );
}

/// **A beat's error never hands back what the request carried**: a proxy page in front of the CLOB answers a
/// beat with a JSON `error` that echoes every `POLY_*` header it received, and the beat's `Failed` words carry
/// the sentence with each value `[redacted]` (`crate::execution::rest::post_signed_raw` redacts the body it hands
/// the beat; [`interpret_beat`] words it through the shared summary), so none of the values.
#[test]
fn an_echoed_request_never_reaches_a_beats_error() {
    use crate::execution::test_support::{FakeServer, Reply, declare_direct_egress};
    declare_direct_egress();
    let creds = PolymarketConfig {
        secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string().into(),
        address: "0x00000000000000000000000000000000000000ab".to_string(),
        api_key: "key-00000001".to_string().into(),
        passphrase: "pass-00000001".to_string().into(),
        ..Default::default()
    };
    fn poly_headers(headers: &[(String, String)]) -> Vec<&(String, String)> {
        headers.iter().filter(|(name, _)| name.starts_with("poly_")).collect()
    }
    let clob = FakeServer::start(|request| {
        let echo: Vec<String> =
            poly_headers(&request.headers).iter().map(|(k, v)| format!("{k}={v}")).collect();
        Reply::json(401, json!({ "error": format!("denied {}", echo.join(" ")) }))
    });
    let transport = HeartbeatTransportProduction::new(creds, clob.base.clone());
    let report = beat_once(&transport, &mut HeartbeatState::new());
    let seen = clob.seen();
    let sent = poly_headers(&seen[0].headers);
    assert_eq!(sent.len(), 5, "the beat carried every POLY_* header: {seen:?}");
    let redacted: Vec<String> = sent.iter().map(|(k, _)| format!("{k}=[redacted]")).collect();
    assert_eq!(report, BeatReport::Failed(format!("heartbeat 401: denied {}", redacted.join(" "))));
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

/// **PM-6: an EMPTY expected id is adopted, never the stale one kept.** After another integration's legacy beat the
/// venue expects `""` (the mismatch body the venue's SDKs parse, `error_msg` and `heartbeat_id`); a beat that kept its
/// stale id would mismatch on every tick until the venue cancels every open order.
#[test]
fn an_empty_expected_id_is_adopted_and_the_retry_sends_it() {
    let scripted = Scripted::new(vec![
        Ok((400, json!({ "error_msg": "Invalid Heartbeat ID", "heartbeat_id": "" }))),
        Ok((200, json!({ "heartbeat_id": "fresh", "error": null }))),
    ]);
    let mut st = HeartbeatState::new();
    st.apply(&BeatOutcome::Ok("stale-id".into()));
    assert_eq!(beat_once(&scripted, &mut st), BeatReport::Beat("fresh".into()));
    let calls = scripted.calls.borrow();
    assert_eq!(calls[1]["heartbeat_id"], "", "the retry sent the venue's expected (empty) id");
    assert_eq!(st.heartbeat_id(), Some("fresh"));
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
fn spawn_returns_none_without_credentials() {
    let seed = vike_venues::venue_urls::VenueUrls::seed("polymarket");
    let h = HeartbeatPoller::spawn(None, true, DEFAULT_BEAT_INTERVAL, &seed);
    assert!(h.is_none(), "no creds ⇒ nothing spawned");
}

/// D4 of decision 0095: creds that could sign a beat start nothing unless the caller enables it —
/// no thread, no network call.
#[test]
fn spawn_returns_none_when_not_enabled() {
    let creds = PolymarketConfig {
        api_key: "k".to_string().into(),
        secret: "cG9seW1hcmtldA==".to_string().into(),
        ..Default::default()
    };
    let seed = vike_venues::venue_urls::VenueUrls::seed("polymarket");
    let h = HeartbeatPoller::spawn(Some(creds), false, DEFAULT_BEAT_INTERVAL, &seed);
    assert!(h.is_none(), "not enabled ⇒ nothing spawned");
}
