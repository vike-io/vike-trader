//! The exec event lane's own pieces: the URL and the cursor, the agent's bounds, the open's answers over a loopback
//! host, and the ladder the pump runs. The pump itself (its reconnect, its idle watchdog, the order path's state) is
//! `vike_bridge_core`'s, and `crates/vike-bridge-core/tests/link_conformance.rs` drives this lane's real decode and
//! cursor through it.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserError, WebsocketSessionUserMessage, WebsocketSessionUserStream,
};
use vike_log::loopback::{redirecting_origin, serve_replies};

use super::*;

const BASE: &str = "https://broker-api.sandbox.alpaca.markets";

#[test]
fn events_url_bare_on_first_connect() {
    assert_eq!(events_url(BASE, None), format!("{BASE}/v2/events/trades"));
}

#[test]
fn events_url_backfills_with_since_id() {
    let ulid = "01KXFZXK45JRC3RSRXN12SEPRD";
    assert_eq!(events_url(BASE, Some(ulid)), format!("{BASE}/v2/events/trades?since_id={ulid}"));
}

#[test]
fn event_id_extracted_when_present() {
    let v: Value = serde_json::from_str(
        r#"{"event":"accepted","event_id":"01KXFZXK45JRC3RSRXN12SEPRD","order":{}}"#,
    )
    .unwrap();
    assert_eq!(event_id_of(&v).as_deref(), Some("01KXFZXK45JRC3RSRXN12SEPRD"));
}

#[test]
fn event_id_none_when_absent() {
    let v: Value = serde_json::from_str(r#"{"event":"fill","order":{}}"#).unwrap();
    assert_eq!(event_id_of(&v), None);
}

fn event(id: &str) -> Value {
    serde_json::json!({"event": "new", "event_id": id, "order": {}})
}

/// **The cursor lives in the open.** Before any event the open is bare; after one it asks `since_id` the last id seen,
/// and exactly the one boundary event that open re-delivers is dropped, once; a later event, and a frame with no id,
/// are decoded.
#[test]
fn the_cursor_reopens_from_the_last_event_and_drops_its_redelivered_boundary_once() {
    let mut cursor = TradeEventsCursor::default();
    assert_eq!(cursor.open_url(BASE), format!("{BASE}/v2/events/trades"));
    assert!(cursor.admit(&event("01A")) && cursor.admit(&event("01B")));
    assert_eq!(cursor.open_url(BASE), format!("{BASE}/v2/events/trades?since_id=01B"));
    assert!(!cursor.admit(&event("01B")), "the boundary the open re-delivers");
    assert!(cursor.admit(&event("01B")), "the skip is spent on the one re-delivery");
    assert!(
        cursor.admit(&serde_json::json!({"event": "fill"})),
        "no id: decoded, the cursor stays"
    );
    assert!(cursor.admit(&event("01C")));
    assert_eq!(cursor.open_url(BASE), format!("{BASE}/v2/events/trades?since_id=01C"));
}

/// A token source over a counting fake exchange: the n-th exchange mints `tok-n`.
fn token() -> (TokenSource, Arc<AtomicU64>) {
    let minted = Arc::new(AtomicU64::new(0));
    let count = Arc::clone(&minted);
    let token = TokenSource::with_clock_and_exchanger(
        Box::new(|| 1_000_000),
        Box::new(move |_body: &str| {
            let n = count.fetch_add(1, Ordering::SeqCst) + 1;
            Ok((format!("tok-{n}"), 900))
        }),
    );
    (token, minted)
}

/// **A 401 refreshes the token and is a refused open, never the pump's end**; any other non-2xx is a transport failure.
/// Each open asks from the cursor.
///
/// MUTATION PROOF: match the 401 as an `Ok` response (the old loop's arm) — ureq answers a 4xx as
/// `Err(StatusCode)`, so the open reads as a transport failure and the token is never refreshed.
#[test]
fn a_refused_open_refreshes_the_token_and_another_answer_is_a_transport_failure() {
    let (base, heard) = serve_replies("", vec![(401, String::new()), (503, String::new())]);
    let (token, minted) = token();
    let cursor = Mutex::new(TradeEventsCursor::default());
    let agent = sse_agent(CONNECT_TIMEOUT, READ_IDLE);
    let refused = open_trade_events(&agent, &base, &token, &cursor);
    assert!(matches!(refused, WebsocketSessionUserOpenOutcome::Refused(_)));
    assert_eq!(minted.load(Ordering::SeqCst), 2, "the open's token, then the refresh");
    lock(&cursor).admit(&event("01Z"));
    let WebsocketSessionUserOpenOutcome::Transport(why) =
        open_trade_events(&agent, &base, &token, &cursor)
    else {
        panic!("a 503 is a transport failure")
    };
    assert_eq!(why, AlpacaStreamRefusal::NonSuccess { status: 503 }.to_string());
    assert_eq!(
        *heard.lock().unwrap(),
        ["GET /v2/events/trades HTTP/1.1", "GET /v2/events/trades?since_id=01Z HTTP/1.1"]
    );
}

/// **A 2xx is the pump's stream**: the body's `data:` lines are its frames, every other line proof of life, and the
/// body's end closes the session.
#[test]
fn an_open_stream_hands_the_pump_its_data_lines() {
    let body = ": heartbeat\nevent: trade_updates\ndata: {\"event\":\"fill\"}\n\n";
    let (base, _) = serve_replies("", vec![(200, body.to_string())]);
    let (token, _) = token();
    let cursor = Mutex::new(TradeEventsCursor::default());
    let opened = open_trade_events(&sse_agent(CONNECT_TIMEOUT, READ_IDLE), &base, &token, &cursor);
    let WebsocketSessionUserOpenOutcome::Ready(mut stream) = opened else {
        panic!("a 200 opens the stream")
    };
    let mut reads = Vec::new();
    loop {
        let read = stream.recv();
        let ended = read.is_err();
        reads.push(read);
        if ended {
            break;
        }
    }
    use WebsocketSessionUserMessage::{Other, Text};
    assert_eq!(
        reads,
        [
            Ok(Other),
            Ok(Other),
            Ok(Text(r#"{"event":"fill"}"#.to_string())),
            Ok(Other),
            Err(WebsocketSessionUserError::Closed("the stream's body ended".to_string())),
        ]
    );
}

/// **A body that waits out the read bound is the pump's idle tick, not the stream's end**, so the pump's watchdog
/// judges the silence (a `silent stall`, `Error`) where a timeout used to read as a stream the venue ended: the real
/// agent's ureq timeout, read through the shared `LineStream`.
#[test]
fn a_silent_body_is_the_pumps_idle_tick() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local_addr"));
    let (release, held) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("the agent dials");
        let mut request = [0u8; 2048];
        let _ = std::io::Read::read(&mut socket, &mut request);
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
        socket.write_all(head.as_bytes()).expect("the head");
        let _ = held.recv(); // and then nothing
    });
    let (token, _) = token();
    let cursor = Mutex::new(TradeEventsCursor::default());
    let agent = sse_agent(CONNECT_TIMEOUT, Duration::from_millis(300));
    let WebsocketSessionUserOpenOutcome::Ready(mut stream) =
        open_trade_events(&agent, &base, &token, &cursor)
    else {
        panic!("the head opens the stream")
    };
    assert_eq!(stream.recv(), Err(WebsocketSessionUserError::Timeout));
    drop(release);
    server.join().expect("the silent host exits once released");
}

/// The SSE agent carries the exec row's two bounds, observed on the BUILT agent: the dial bound
/// the driver passes in (`CONNECT_TIMEOUT`) and the read-idle bound of the long-lived body. The
/// 5 s dial is not arbitrary — it is sized against `ExecutionClientActor::stop`'s join of this thread
/// (the builder's comment) — so the effective value is pinned here as the literal, not as a
/// restatement of the row.
#[test]
fn the_sse_agent_carries_the_execution_rows_bounds() {
    assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(5));
    let agent = sse_agent(CONNECT_TIMEOUT, READ_IDLE);
    let timeouts = agent.config().timeouts();
    assert_eq!(timeouts.connect, Some(Duration::from_secs(5)), "the dial bound");
    assert_eq!(
        agent.config().timeout_per_read(),
        Some(Duration::from_secs(30)),
        "the read-idle bound"
    );
    assert_eq!(timeouts.recv_body, None, "a recv_body budget would end the stream");
    assert_eq!(timeouts.global, None, "a global budget would end the stream");
    assert_eq!(timeouts.per_call, None, "a per-call budget would end the stream");
    assert!(agent.config().http_status_as_error(), "the 401 arm reads Err(StatusCode(401))");
}

/// **A host that takes the request and never answers is given up on after the read-idle bound.**
/// ureq 3.5 caps every transport read at `timeout_per_read`, the one that waits for the response
/// head included, so this agent needs no head bound of its own. Built with a 500 ms bound.
///
/// MUTATION PROOF: delete `.timeout_per_read(…)` from `crates/vike-transport/src/http.rs`'s `stream_agent` — the
/// call never returns.
#[test]
fn the_sse_agent_gives_up_on_a_host_that_never_answers() {
    let read_idle = Duration::from_millis(500);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let base = format!("http://{}", listener.local_addr().expect("local_addr"));
    let (release, held) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("the agent dials");
        let mut request = [0u8; 2048];
        let _ = std::io::Read::read(&mut socket, &mut request); // the request arrives...
        let _ = held.recv(); // ...and no answer ever does
    });
    let started = std::time::Instant::now();
    let outcome = sse_agent(CONNECT_TIMEOUT, read_idle).get(&events_url(&base, None)).call();
    let waited = started.elapsed();
    drop(release);
    server.join().expect("the silent host exits once released");
    let err = outcome.err();
    assert!(matches!(err, Some(ureq::Error::Timeout(ureq::Timeout::PerRead))), "{err:?}");
    assert!(
        waited >= read_idle && waited < read_idle * 6,
        "gave up after {waited:?} against a {read_idle:?} read-idle bound"
    );
}

/// Every arm waits on the row's ladder as the pump runs it, a FIXED 2 s: a failed open, a refused one (the 401 that
/// used to retry at once) and the end of a session that lived any time at all.
#[test]
fn the_execution_reconnect_is_the_fixed_wait_the_pump_runs() {
    assert!(RECONNECT.is_fixed());
    let mut backoff = trade_events_backoff();
    let two = Duration::from_secs(2);
    assert_eq!((backoff.failed(), backoff.refused(), backoff.refused()), (two, two, two));
    backoff.opened();
    for lived in [Duration::ZERO, Duration::from_secs(3600)] {
        assert_eq!(backoff.ended(lived), two, "after a session of {lived:?}");
    }
}

/// **The 401 that refreshed the token is a reopen, the second in a row a failure** (the shared pump's refusal rule over
/// the ladder's budget of one): the pump reads the first `Connecting`, so a passing refusal does not say DOWN, and the
/// next refused to the fresh token `Error`. An open that works starts a new run.
#[test]
fn one_refused_open_is_spared_and_a_second_in_a_row_is_not() {
    let mut backoff = trade_events_backoff();
    let mut spared = || {
        backoff.refused();
        backoff.refusal_spared()
    };
    assert_eq!([spared(), spared()], [true, false]);
    backoff.opened();
    backoff.refused();
    assert!(backoff.refusal_spared(), "an open that worked starts a new run");
}

/// THE CONNECT BOUND, proven by BEHAVIOUR rather than by the constant being present.
///
/// ⚠ The defect this fixes is precisely "a timeout that is not set, in code that compiles":
/// `ureq`'s `Timeouts::default()` leaves `connect: None`, so the dial parked in the kernel for
/// the OS connect timeout (~130 s on Linux) with the `stop` flag unread — and
/// `ExecutionClientActor::stop` joins this thread with no timeout inside `run_with_deadline`'s sequential
/// TAIL, which the task counter cannot see. So a test that merely asserted `CONNECT_TIMEOUT`
/// exists would restate the bug's own shape. This one DIALS and asserts the call gives up.
///
/// The address is `203.0.113.1` — TEST-NET-3 (RFC 5737), reserved for documentation and routed
/// nowhere — so this needs no internet and reaches no third party. Either outcome of a dial to
/// it is a PASS as long as it is PROMPT: a silent blackhole hits the bound, and a router that
/// answers "network unreachable" returns sooner. The only way to fail is to hang, which is
/// exactly the regression. Bound is passed in short so the test costs a fraction of a second.
#[test]
fn the_sse_agent_gives_up_on_a_dial_that_never_completes() {
    let bound = Duration::from_millis(300);
    let agent = sse_agent(bound, READ_IDLE);
    let t0 = std::time::Instant::now();
    let outcome = agent.get("http://203.0.113.1:81/v2/events/trades").call();
    let waited = t0.elapsed();
    assert!(outcome.is_err(), "TEST-NET-3 is routed nowhere; a 2xx here means the URL moved");
    assert!(
        waited < bound * 10,
        "the dial took {waited:?} against a {bound:?} connect bound — the bound is not being \
         applied, so this thread cannot observe a stop while it is dialling an unreachable host"
    );
}

/// **A 3xx on the credentialed SSE open surfaces as a non-2xx response and the `Location` is
/// never dialled.** ureq's default follows ten redirects and replays every header but
/// `Authorization`/`Cookie` to the target.
///
/// MUTATION PROOF: end `crates/vike-transport/src/http.rs`'s `stream_agent` in `agent_following(_, 10)` instead of
/// `agent_no_redirect` — the 302 is followed and the second listener sees the replayed request.
#[test]
fn the_sse_agent_never_follows_a_redirect() {
    let (base, target_hits) = redirecting_origin();
    let response = sse_agent(CONNECT_TIMEOUT, READ_IDLE)
        .get(&events_url(&base, None))
        .header("Authorization", "Bearer test-token")
        .header("Accept", "text/event-stream")
        .call();
    assert_eq!(target_hits.load(Ordering::SeqCst), 0, "the target was dialled");
    assert_eq!(response.expect("a 302 is a response").status().as_u16(), 302);
}
