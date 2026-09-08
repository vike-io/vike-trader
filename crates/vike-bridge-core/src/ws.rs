//! Cleaved from `binance/user_data.rs` (plus the `send_text` inherent impl cleaved out of
//! `bybit/user_data.rs`) — venue-neutral tungstenite WS socket plumbing every WS-based venue
//! bridge (binance/bybit/okx/deribit/polymarket) builds its handshake on (Phase 3 PR A).

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::ratelimit::KeyedRateGate;
use crate::user_data::{OpenOutcome, StreamError, StreamMsg, UserDataAuthError, UserStream};

pub type WsSocket = WebSocket<MaybeTlsStream<TcpStream>>;

/// THE single outbound-WS-send seam (net-hardening spec §A, Nautilus-shaped): consult the keyed
/// rate gate (if any) for `key`, then write the text frame. Every RATE-LIMITED app-level send —
/// handshake `login`/`subscribe`, market-feed `subscribe`, deribit JSON-RPC `order` — routes
/// through here so one keyed limiter paces them all (and reconnect-replay rides it, since reconnect
/// just re-runs the handshake). Keepalive pings and Pongs deliberately do NOT use this — a control
/// frame must never be throttled (that would starve the very connection we're protecting).
pub fn send_gated(
    socket: &mut WsSocket,
    gate: Option<&KeyedRateGate>,
    key: &str,
    text: &str,
) -> Result<(), StreamError> {
    send_gated_cost(socket, gate, key, 1, text)
}

/// Weighted twin of [`send_gated`]: charge `cost` permits against `key`'s window before the write —
/// for a heavier send that should consume more of the venue's per-key budget than a single frame
/// (e.g. a batch subscribe that opens N channels, or a weight-N REST-over-WS request). `cost == 1`
/// is exactly [`send_gated`], so existing call sites keep their behavior verbatim.
pub fn send_gated_cost(
    socket: &mut WsSocket,
    gate: Option<&KeyedRateGate>,
    key: &str,
    cost: usize,
    text: &str,
) -> Result<(), StreamError> {
    if let Some(g) = gate {
        g.gate_cost(key, cost);
    }
    socket
        .send(Message::Text(text.to_string().into()))
        .map_err(|e| StreamError::Closed(e.to_string()))
}

/// Configure the raw TCP stream under a freshly-opened WS socket for the pump:
/// 1. a read timeout so `read()` returns periodically for stop-polls (shared with the perp
///    listenKey stream), and
/// 2. `TCP_NODELAY` — disable Nagle's algorithm so small outbound order/subscribe/quote frames go
///    on the wire immediately instead of being coalesced (tens of ms of latency on tiny frames).
///    Every venue WS carries latency-sensitive small frames, so this is unconditionally desirable.
///
/// Both are best-effort (`let _`): a transient socket-option failure must not abort a connection.
pub fn configure_ws_stream(socket: &WsSocket, timeout: Duration) {
    match socket.get_ref() {
        MaybeTlsStream::Plain(s) => {
            let _ = s.set_read_timeout(Some(timeout));
            let _ = s.set_nodelay(true);
        }
        MaybeTlsStream::Rustls(t) => {
            let _ = t.get_ref().set_read_timeout(Some(timeout));
            let _ = t.get_ref().set_nodelay(true);
        }
        _ => {}
    }
}

/// Shared WouldBlock/TimedOut classifier — used both by [`TungsteniteStream::recv`] below and by
/// binance's WS-API handshake recv loop (which reads the raw socket directly, before it is
/// wrapped; other venues carry private copies of this classifier).
pub fn is_timeout(err: &tungstenite::Error) -> bool {
    matches!(
        err,
        tungstenite::Error::Io(e) if matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    )
}

/// One handshake frame's meaning relative to an expected ack — the pure half of every venue's
/// `_await_ack` loop (was duplicated per venue in binance/bybit/okx `ws_auth.rs`). Venue-specific
/// classifiers (`match_subscribe_ack`/`match_op_ack`/`match_event_ack`) map a decoded frame to this;
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
    /// latency blip cannot permanently kill the fill lane (audit A3). NEVER contains creds.
    TransientErr(String),
}

/// Await ONE handshake ack: skip interleaved frames, bounded by `deadline` and the `stop` flag. The
/// socket is left OPEN on every outcome — the caller consumes it into `Ready` on `Ok(())` or closes
/// it on `Err` (matching the bybit/okx two-step auth→subscribe handshake, and binance's single-step
/// one). `what` is the venue+step label baked into the timeout message (e.g. `"Bybit WS auth"` →
/// `"Bybit WS auth ack timed out"`); `classify` maps each decoded frame to an [`AckResult`] (it also
/// owns any op/id/event correlation, so it stays venue-specific). Extracted verbatim from the
/// bybit/okx `await_ack` fns + binance's inlined loop (Phase-3 dedup).
pub fn await_ack(
    socket: &mut WsSocket,
    stop: &AtomicBool,
    deadline: Instant,
    what: &str,
    classify: impl Fn(&Value) -> AckResult,
) -> Result<(), Box<OpenOutcome<TungsteniteStream>>> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Box::new(OpenOutcome::Stopped));
        }
        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) if is_timeout(&e) => {
                if Instant::now() >= deadline {
                    // audit A3: a timed-out ack is transient (busy/latency), not auth — reconnect.
                    return Err(Box::new(OpenOutcome::Transport(format!("{what} ack timed out"))));
                }
                continue;
            }
            Err(e) => return Err(Box::new(OpenOutcome::Transport(format!("handshake recv: {e}")))),
        };
        match msg {
            Message::Text(raw) => {
                // tolerate a non-JSON keepalive (e.g. OKX's raw 'pong') mid-handshake
                let Ok(frame) = serde_json::from_str::<Value>(raw.as_str()) else {
                    continue;
                };
                match classify(&frame) {
                    AckResult::NotOurs => continue,
                    AckResult::Ok => return Ok(()),
                    AckResult::TransientErr(msg) => {
                        return Err(Box::new(OpenOutcome::Transport(msg))); // audit A3: reconnect
                    }
                    AckResult::Err(msg) => {
                        return Err(Box::new(OpenOutcome::Auth(UserDataAuthError(msg))));
                    }
                }
            }
            Message::Ping(p) => {
                let _ = socket.send(Message::Pong(p));
            }
            Message::Close(_) => {
                return Err(Box::new(OpenOutcome::Transport("closed during handshake".into())));
            }
            _ => {}
        }
    }
}

/// tungstenite adapter for the venue-neutral pump.
pub struct TungsteniteStream(pub WsSocket);

impl UserStream for TungsteniteStream {
    fn recv(&mut self) -> Result<StreamMsg, StreamError> {
        match self.0.read() {
            Ok(Message::Text(t)) => Ok(StreamMsg::Text(t.to_string())),
            Ok(Message::Ping(p)) => Ok(StreamMsg::Ping(p.to_vec())),
            Ok(Message::Close(_)) => Err(StreamError::Closed("server closed".into())),
            Ok(_) => Ok(StreamMsg::Other),
            Err(e) if is_timeout(&e) => Err(StreamError::Timeout),
            Err(e) => Err(StreamError::Closed(e.to_string())),
        }
    }

    fn pong(&mut self, payload: Vec<u8>) -> Result<(), StreamError> {
        self.0.send(Message::Pong(payload.into())).map_err(|e| StreamError::Closed(e.to_string()))
    }
}

impl TungsteniteStream {
    /// Send an app-level text frame (Bybit's `{"op":"ping"}` keepalive).
    pub fn send_text(&mut self, text: &str) -> Result<(), StreamError> {
        self.0
            .send(Message::Text(text.to_string().into()))
            .map_err(|e| StreamError::Closed(e.to_string()))
    }
}
