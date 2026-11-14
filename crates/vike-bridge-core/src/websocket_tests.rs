//! [`send_and_await`] over a loopback peer (dialled by `crate::websocket_session::user::dial_user_stream`), and
//! [`AckResult::refusal`]'s split.
use super::*;
use crate::websocket_session::user::dial_user_stream;

/// A peer that upgrades, reads ONE text frame, answers `replies` in order, then reads until the client
/// is gone. It hands back the frame it read and whether the client CLOSED (a Close frame) rather than
/// dropping the connection.
fn answering_peer(
    replies: &'static [&'static str],
) -> (String, std::thread::JoinHandle<(String, bool)>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let peer = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(tcp).unwrap();
        let got = ws.read().unwrap().into_text().unwrap().to_string();
        for reply in replies {
            ws.send(Message::Text(reply.to_string().into())).unwrap();
        }
        let closed = loop {
            match ws.read() {
                Ok(Message::Close(_)) => break true,
                Ok(_) => continue,
                Err(_) => break false,
            }
        };
        (got, closed)
    });
    (url, peer)
}

fn by_id(frame: &Value) -> AckResult {
    match frame.get("id").and_then(Value::as_str) {
        Some("r1") => match frame.get("code").and_then(Value::as_i64) {
            Some(code) => AckResult::refusal(code, format!("refused {code}")),
            None => AckResult::Ok,
        },
        _ => AckResult::NotOurs,
    }
}

/// One step over a fresh dial of `url`; the socket is dropped (not closed) on the way out.
fn step(
    url: &str,
) -> Result<Value, Box<WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite>>> {
    let mut socket = dial_user_stream(url, Duration::from_secs(5)).expect("the loopback dial");
    let deadline = Instant::now() + Duration::from_secs(5);
    let stop = AtomicBool::new(false);
    send_and_await(
        &mut socket,
        None,
        "login",
        "{\"id\":\"r1\"}",
        "auth",
        &stop,
        deadline,
        "Test WS auth",
        by_id,
    )
}

/// The step sends its frame, skips a frame that is not its ack, hands back the ack itself and leaves the
/// socket open for the next step.
#[test]
fn a_step_sends_its_frame_and_hands_back_the_matched_ack() {
    let (url, peer) =
        answering_peer(&[r#"{"id":"other"}"#, r#"{"id":"r1","result":{"token":"t"}}"#]);
    let frame = step(&url).unwrap_or_else(|_| panic!("the ack was sent"));
    assert_eq!(frame["result"]["token"], "t", "the matched frame, not the one before it");
    let (got, closed) = peer.join().unwrap();
    assert_eq!(got, r#"{"id":"r1"}"#);
    assert!(!closed, "a step that worked leaves the socket to its caller");
}

/// A refusal ends the open in the shared table's class and CLOSES the socket.
#[test]
fn a_refused_step_closes_the_socket_and_keeps_the_class() {
    let (url, peer) = answering_peer(&[r#"{"id":"r1","code":-1021}"#]);
    let Err(outcome) = step(&url) else { panic!("the venue refused") };
    assert!(
        matches!(*outcome, WebsocketSessionUserOpenOutcome::Transport(ref why) if why == "refused -1021"),
        "a listed code reconnects"
    );
    assert!(peer.join().unwrap().1, "the step closed the socket on its way out");

    let (url, peer) = answering_peer(&[r#"{"id":"r1","code":13009}"#]);
    let Err(outcome) = step(&url) else { panic!("the venue refused") };
    assert!(
        matches!(*outcome, WebsocketSessionUserOpenOutcome::Auth(_)),
        "an unlisted code is the open's failure"
    );
    assert!(peer.join().unwrap().1);
}

/// The ack tail every venue wrote by hand: a listed code is transient, anything else (an absent code
/// read as 0 included) surfaces.
#[test]
fn a_refusal_is_transient_only_for_a_listed_code() {
    assert_eq!(AckResult::refusal(-1021, "skew".into()), AckResult::TransientErr("skew".into()));
    assert_eq!(AckResult::refusal(50011, "rate".into()), AckResult::TransientErr("rate".into()));
    assert_eq!(AckResult::refusal(0, "no code".into()), AckResult::Err("no code".into()));
    assert_eq!(AckResult::refusal(13009, "bad key".into()), AckResult::Err("bad key".into()));
}

/// An HTTP base becomes the WebSocket url of a path on its host: `http` → `ws`, anything else → `wss`.
#[test]
fn an_http_base_becomes_the_websocket_url_of_a_path_on_its_host() {
    let ws = websocket_url_from_http;
    assert_eq!(ws("https://127.0.0.1:5000", "/v1/api/ws"), "wss://127.0.0.1:5000/v1/api/ws");
    assert_eq!(ws("https://h.example:443/", "/ls"), "wss://h.example:443/ls");
    assert_eq!(ws("http://local:8080", "/ls"), "ws://local:8080/ls");
    assert_eq!(ws("push.example", "/ls"), "wss://push.example/ls");
}
