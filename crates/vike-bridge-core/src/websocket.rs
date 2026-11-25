//! Venue-neutral tungstenite WS plumbing every WS venue bridge builds its handshake on: the gated send seam, the
//! handshake-ack loop and the pump's stream adapter. The socket type, its stream options and the timed-out-read test
//! are the generic network layer's (`vike_transport::websocket`), and so is the dial (`vike_transport::websocket_proxy`).
//!
//! **One login step is [`send_and_await`]**: a gated send, then the awaited ack, with the socket closed
//! on either failure, and the matched frame handed back (a JSON-RPC venue reads its `result` there).
//! Each venue keeps its own frames, its gate keys, its words and its `classify`; a refusal it can
//! survive (an optional channel) is its `classify` answering [`AckResult::Ok`] after its own warning.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::Value;
use tungstenite::Message;
use vike_transport::websocket::{WsSocket, is_timeout};

use crate::ratelimit::RateGatePerKey;
use crate::websocket_session::user::{
    WebsocketSessionUserAuthenticationError, WebsocketSessionUserError,
    WebsocketSessionUserMessage, WebsocketSessionUserOpenOutcome, WebsocketSessionUserStream,
};

/// THE single outbound-WS-send seam: consult the keyed rate gate (if any) for `key`, then write the
/// text frame. Every RATE-LIMITED app-level send — handshake `login`/`subscribe`, market-feed
/// `subscribe`, deribit JSON-RPC `order` — routes through here so one keyed limiter paces them all
/// (reconnect-replay too, since a reconnect re-runs the handshake). Keepalive pings and Pongs do
/// NOT use this: a throttled control frame would starve the connection it protects.
pub fn send_gated(
    socket: &mut WsSocket,
    gate: Option<&RateGatePerKey>,
    key: &str,
    text: &str,
) -> Result<(), WebsocketSessionUserError> {
    send_gated_cost(socket, gate, key, 1, text)
}

/// Weighted twin of [`send_gated`]: charge `cost` permits against `key`'s window before the write,
/// for a send heavier than one frame (a batch subscribe opening N channels, a weight-N REST-over-WS
/// request). `cost == 1` is exactly [`send_gated`].
pub fn send_gated_cost(
    socket: &mut WsSocket,
    gate: Option<&RateGatePerKey>,
    key: &str,
    cost: usize,
    text: &str,
) -> Result<(), WebsocketSessionUserError> {
    if let Some(g) = gate {
        g.gate_cost(key, cost);
    }
    socket
        .send(Message::Text(text.to_string().into()))
        .map_err(|e| WebsocketSessionUserError::Closed(e.to_string()))
}

/// One handshake frame's meaning relative to an expected ack. Venue-specific classifiers
/// (`match_subscribe_ack`/`match_op_ack`/`match_event_ack`) map a decoded frame to this;
/// [`await_ack`] drives the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckResult {
    /// non-JSON / non-object / id-or-op mismatch — keep looping
    NotOurs,
    /// the awaited ack, success
    Ok,
    /// auth/protocol failure — do NOT reconnect-loop (message NEVER contains creds)
    Err(String),
    /// TRANSIENT failure (clock skew, rate limit): reconnect rather than surface-and-stop, so a
    /// latency blip cannot permanently kill the fill lane. NEVER contains creds.
    TransientErr(String),
}

impl AckResult {
    /// The venue refused the awaited request with `code`: [`AckResult::TransientErr`] when the shared
    /// table (`crate::transport::websocket_ack_is_transient`) lists the code, else [`AckResult::Err`]. An absent
    /// code read as `0` is not transient, so a malformed refusal still surfaces. `text` NEVER holds creds.
    pub fn refusal(code: i64, text: String) -> Self {
        if crate::transport::websocket_ack_is_transient(code) {
            Self::TransientErr(text)
        } else {
            Self::Err(text)
        }
    }
}

/// One login step: send `frame` through [`send_gated`] under `key`, then [`await_ack`] its reply, and
/// hand back the frame `classify` matched. On any failure the socket is CLOSED and the open's outcome
/// returned: a failed send is `Transport("{step} send: …")` (`step` is the venue's word, `"auth"` where
/// the gate key is `"login"`), the ack's outcomes are [`await_ack`]'s. `frame` is never logged.
pub fn send_and_await(
    socket: &mut WsSocket,
    gate: Option<&RateGatePerKey>,
    key: &str,
    frame: &str,
    step: &str,
    stop: &AtomicBool,
    deadline: Instant,
    what: &str,
    classify: impl Fn(&Value) -> AckResult,
) -> Result<Value, Box<WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite>>> {
    let reply = match send_gated(socket, gate, key, frame) {
        Ok(()) => await_reply(socket, stop, deadline, what, classify),
        Err(e) => {
            Err(Box::new(WebsocketSessionUserOpenOutcome::Transport(format!("{step} send: {e:?}"))))
        }
    };
    if reply.is_err() {
        let _ = socket.close(None);
    }
    reply
}

/// Await ONE handshake ack: skip interleaved frames, bounded by `deadline` and the `stop` flag. The
/// socket is left OPEN on every outcome — the caller consumes it into `Ready` on `Ok(())` or closes
/// it on `Err` (fits both a two-step auth→subscribe handshake and a single-step one). `what` is the
/// venue+step label baked into the timeout message (`"Bybit WS auth"` →
/// `"Bybit WS auth ack timed out"`); `classify` maps each decoded frame to an [`AckResult`] and owns
/// any op/id/event correlation, so it stays venue-specific.
pub fn await_ack(
    socket: &mut WsSocket,
    stop: &AtomicBool,
    deadline: Instant,
    what: &str,
    classify: impl Fn(&Value) -> AckResult,
) -> Result<(), Box<WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite>>> {
    await_reply(socket, stop, deadline, what, classify).map(drop)
}

/// [`await_ack`], handing back the frame `classify` answered [`AckResult::Ok`] for.
fn await_reply(
    socket: &mut WsSocket,
    stop: &AtomicBool,
    deadline: Instant,
    what: &str,
    classify: impl Fn(&Value) -> AckResult,
) -> Result<Value, Box<WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite>>> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Box::new(WebsocketSessionUserOpenOutcome::Stopped));
        }
        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) if is_timeout(&e) => {
                if Instant::now() >= deadline {
                    // A timed-out ack is transient (busy/latency), not auth: reconnect.
                    return Err(Box::new(WebsocketSessionUserOpenOutcome::Transport(format!(
                        "{what} ack timed out"
                    ))));
                }
                continue;
            }
            Err(e) => {
                return Err(Box::new(WebsocketSessionUserOpenOutcome::Transport(format!(
                    "handshake recv: {e}"
                ))));
            }
        };
        match msg {
            Message::Text(raw) => {
                // tolerate a non-JSON keepalive (e.g. OKX's raw 'pong') mid-handshake
                let Ok(frame) = serde_json::from_str::<Value>(raw.as_str()) else {
                    continue;
                };
                match classify(&frame) {
                    AckResult::NotOurs => continue,
                    AckResult::Ok => return Ok(frame),
                    AckResult::TransientErr(msg) => {
                        return Err(Box::new(WebsocketSessionUserOpenOutcome::Transport(msg))); // audit A3: reconnect
                    }
                    AckResult::Err(msg) => {
                        return Err(Box::new(WebsocketSessionUserOpenOutcome::Auth(
                            WebsocketSessionUserAuthenticationError(msg),
                        )));
                    }
                }
            }
            Message::Ping(p) => {
                // Best-effort pong: a dead socket surfaces as the next read error.
                let _ = socket.send(Message::Pong(p));
            }
            Message::Close(_) => {
                return Err(Box::new(WebsocketSessionUserOpenOutcome::Transport(
                    "closed during handshake".into(),
                )));
            }
            _ => {}
        }
    }
}

/// tungstenite adapter for the venue-neutral pump.
pub struct WebsocketSessionUserStreamTungstenite(pub WsSocket);

impl WebsocketSessionUserStream for WebsocketSessionUserStreamTungstenite {
    fn recv(&mut self) -> Result<WebsocketSessionUserMessage, WebsocketSessionUserError> {
        match self.0.read() {
            Ok(Message::Text(t)) => Ok(WebsocketSessionUserMessage::Text(t.to_string())),
            Ok(Message::Ping(p)) => Ok(WebsocketSessionUserMessage::Ping(p.to_vec())),
            Ok(Message::Close(Some(said))) => Err(server_closed(said.code, &said.reason)),
            Ok(Message::Close(None)) => {
                Err(WebsocketSessionUserError::Closed("server closed".into()))
            }
            Ok(_) => Ok(WebsocketSessionUserMessage::Other),
            Err(e) if is_timeout(&e) => Err(WebsocketSessionUserError::Timeout),
            Err(e) => Err(WebsocketSessionUserError::Closed(e.to_string())),
        }
    }

    fn pong(&mut self, payload: Vec<u8>) -> Result<(), WebsocketSessionUserError> {
        self.0
            .send(Message::Pong(payload.into()))
            .map_err(|e| WebsocketSessionUserError::Closed(e.to_string()))
    }
}

/// The session end a venue's close frame says, in the one wording a pump reports it in: the code and reason are all a
/// journal ever has on why a venue hung up (`crate::scripted`'s close step speaks it too).
pub(crate) fn server_closed(
    code: impl std::fmt::Display,
    reason: &str,
) -> WebsocketSessionUserError {
    WebsocketSessionUserError::Closed(format!("server closed: {code} {reason}"))
}

impl WebsocketSessionUserStreamTungstenite {
    /// Send an app-level text frame (Bybit's `{"op":"ping"}` keepalive).
    pub fn send_text(&mut self, text: &str) -> Result<(), WebsocketSessionUserError> {
        self.0
            .send(Message::Text(text.to_string().into()))
            .map_err(|e| WebsocketSessionUserError::Closed(e.to_string()))
    }
}

#[path = "websocket_tests.rs"]
#[cfg(test)]
mod websocket_tests;
