//! DeribitOrderTransport — a persistent, authed JSON-RPC-over-WS ORDER transport. Port of
//! `exec/deribit/transport.py` (blocking tungstenite instead of a dedicated asyncio loop
//! — same semantics: ONE socket, sync bounded request/response by id on the caller's
//! thread, mirroring the crypto sync-REST submit).
//!
//! `connect()` opens + completes the public/auth client_credentials handshake;
//! `call(method, params)` sends a request frame and recv's until the matching id
//! (interleaved subscription notifications / non-JSON keepalives skipped), bounded by
//! `request_timeout` so a stalled socket can never hang the core thread; `close()` is
//! bounded + idempotent. SECRETS: the auth frame is never logged; errors carry only
//! {code, message}.

use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use vike_bridge_core::ws::configure_ws_stream;

use crate::rpc::{parse_response, JsonRpcBuilder};
use vike_bridge_core::ratelimit::KeyedRateGate;
use vike_bridge_core::transport::{VenueApiError, E_TIMEOUT_AMBIGUOUS};
use vike_bridge_core::user_data::UserDataAuthError;

/// The `recv_until_id` deadline-exceeded message. The socket may be perfectly alive and the venue
/// merely slow, so this is the ONE transport failure that is NOT [`is_dead_socket_error`] — while
/// still carrying [`E_TIMEOUT_AMBIGUOUS`] like every other read-half failure.
const WS_REQUEST_TIMED_OUT: &str = "Deribit order-WS request timed out";

// The four DEAD-SOCKET messages, named so [`is_dead_socket_error`] classifies against the very
// constants the failure sites format — a caller matching these by hand would rot the moment a
// message was reworded. ⚠ They do NOT all carry the same code, and the split is the audit-T1 one:
// the two that happen BEFORE anything reached the wire are definite (`code: 0`), the two that
// happen after it did are ambiguous. "The socket is unusable" and "the ORDER's outcome is unknown"
// are two different questions — this predicate answers only the first, and `code` answers the
// second.
/// `call`'s send-failure prefix, `code: 0` — DEFINITE (tungstenite's own error text follows; on the
/// the CI box incident of 2026-08-23 it was "Trying to work with closed connection", 795 times).
const WS_SEND_FAILED: &str = "send: ";
/// `recv_until_id`'s read-failure prefix, [`E_TIMEOUT_AMBIGUOUS`] — the request was already sent.
const WS_RECV_FAILED: &str = "recv: ";
/// `recv_until_id` saw the peer's `Close` while waiting, [`E_TIMEOUT_AMBIGUOUS`] — same reason.
const WS_CLOSED_MID_REQUEST: &str = "closed mid-request";
/// `call` was reached with no socket at all, `code: 0` — DEFINITE, nothing was sent.
/// [`DeribitOrderTransport::connect`] never ran, or a [`DeribitOrderTransport::close`] took it.
const WS_NOT_CONNECTED: &str = "DeribitOrderTransport.connect() not called";

/// Does this [`VenueApiError`] mean THIS TRANSPORT'S SOCKET is unusable — as opposed to the venue
/// having answered with a JSON-RPC error object?
///
/// It exists because [`DeribitOrderTransport::call`] leaves a failed socket in place
/// (`self.socket` stays `Some`) and NOTHING in this transport ever re-dials: `connect()` is called
/// once, by whoever owns the transport. A caller that wants to survive a venue-side close has to
/// ask this question and re-connect ITSELF —
/// `crates/bridges/deribit/src/recon_client.rs`'s `DeribitReconClient::call` and
/// `crates/bridges/deribit/src/client.rs`'s `DeribitRest::redial_order_socket` are the two that do.
///
/// ⚠ **A `true` here licenses a RE-DIAL and nothing more.** It says the socket must be replaced
/// before it can carry another frame; it says NOTHING about whether the failed request reached the
/// venue. That second question is `err.code` — and on the exec path re-SENDING an order because
/// the socket was replaced would be a second real order. See `DeribitRest::dispatch_submit`.
///
/// Deliberately excluded: a RESPONSE timeout ([`WS_REQUEST_TIMED_OUT`]), because a slow venue is
/// not a dead socket — whether one earns a re-dial anyway is the CALLER's policy, not a fact this
/// transport can state.
pub fn is_dead_socket_error(err: &VenueApiError) -> bool {
    // Both transport sentinels are accepted: `0` (definite) and `E_TIMEOUT_AMBIGUOUS` (`i64::MIN`).
    // Pairing them with this module's own message text is what keeps a VENUE JSON-RPC error — which
    // carries the venue's code and the venue's words — out of this predicate.
    matches!(err.code, 0 | E_TIMEOUT_AMBIGUOUS)
        && (err.msg.starts_with(WS_SEND_FAILED)
            || err.msg.starts_with(WS_RECV_FAILED)
            || err.msg == WS_CLOSED_MID_REQUEST
            || err.msg == WS_NOT_CONNECTED)
}

pub const TESTNET_WS: &str = "wss://test.deribit.com/ws/api/v2";
pub const MAINNET_WS: &str = "wss://www.deribit.com/ws/api/v2";
pub const TESTNET_REST: &str = "https://test.deribit.com";
pub const MAINNET_REST: &str = "https://www.deribit.com";
/// PUBLIC (keyless) server clock, over the JSON-RPC-shaped REST surface. Read by the startup
/// preflight through `vike_mount::server_time`'s deribit row.
///
/// ⚠ **A drifted clock cannot get a deribit order rejected**: [`crate::ws_auth`]'s
/// `build_client_credentials_auth` sends `grant_type: client_credentials`, which carries neither a
/// timestamp nor a nonce, so the deribit clock leg is a HOST-HEALTH canary rather than the
/// rejection guard it is on binance/bybit/okx/aster. It is wired anyway because it is free and a
/// wrong host clock still misdates every local record of a deribit session.
///
/// The stamp is a BARE `result` i64 in epoch ms at the top level (no `data`, no `code`). The body
/// also carries `usIn`/`usOut` in MICROSECONDS and a self-describing `"testnet"` boolean — which is
/// what lets the caller assert it reached the host the mount actually binds. Measured from the CI box on
/// 2026-08-08: `{"jsonrpc":"2.0","result":1786218814983,"usIn":1786218814983507,
/// "usOut":1786218814983615,"usDiff":108,"testnet":true}`.
pub const PATH_TIME: &str = "/api/v2/public/get_time";

const POLL: Duration = Duration::from_secs(1);

type WsSocket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Deribit's matching-engine methods (order lifecycle) vs non-matching-engine reads/auth — the two
/// have separate credit pools, so `call()` gates them under different keys.
fn is_matching_engine(method: &str) -> bool {
    let m = method.strip_prefix("private/").unwrap_or(method);
    m.starts_with("buy")
        || m.starts_with("sell")
        || m.starts_with("cancel")
        || m.starts_with("edit")
        || m.starts_with("close")
}

pub struct DeribitOrderTransport {
    ws_url: String,
    client_id: String,     // SECRET — never logged
    client_secret: String, // SECRET — never logged
    scope: Option<String>,
    pub builder: JsonRpcBuilder,
    request_timeout: Duration,
    socket: Option<WsSocket>,
    /// keyed WS-send gate: `"order"` → matching-engine 5/s, default → non-ME 18/s (spec §A).
    /// Built once; persists across the transport's reconnects.
    gate: KeyedRateGate,
}

impl DeribitOrderTransport {
    pub fn new(ws_url: &str, client_id: &str, client_secret: &str, scope: Option<&str>) -> Self {
        DeribitOrderTransport {
            ws_url: ws_url.to_string(),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            scope: scope.map(str::to_string),
            builder: JsonRpcBuilder::new(),
            request_timeout: Duration::from_secs(10),
            socket: None,
            gate: crate::ratelimit::order_ws_gate(),
        }
    }

    /// Open ONE socket + complete public/auth. Idempotent: re-connect closes the prior.
    pub fn connect(&mut self) -> Result<(), UserDataAuthError> {
        self.close();
        let (mut socket, _resp) = tungstenite::connect(&self.ws_url).map_err(|e| {
            tracing::error!(target: "vike_deribit::transport", error = %e, "order-WS connect failed");
            UserDataAuthError(format!("Deribit order-WS connect failed: {e}"))
        })?;
        configure_ws_stream(&socket, POLL);
        let auth_id = self.builder.next_id();
        let frame = crate::ws_auth::build_client_credentials_auth(
            &self.client_id,
            &self.client_secret,
            self.scope.as_deref(),
            auth_id,
        );
        self.gate.gate("login"); // non-matching-engine (default quota)
        socket
            .send(Message::Text(frame.to_string().into())) // NEVER log (carries creds)
            .map_err(|e| UserDataAuthError(format!("Deribit order-WS auth send failed: {e}")))?;
        let (_result, error) = recv_until_id(&mut socket, auth_id, self.request_timeout)
            .map_err(|e| UserDataAuthError(format!("Deribit order-WS auth failed: {e}")))?;
        if let Some(err) = error {
            let _ = socket.close(None);
            // NEVER include client_id/client_secret in the message
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
            tracing::error!(target: "vike_deribit::transport", msg, "order-WS auth failed");
            return Err(UserDataAuthError(format!("Deribit order-WS auth failed: {msg}")));
        }
        self.socket = Some(socket);
        Ok(())
    }

    /// Sync bounded JSON-RPC request/response by id — the injected transport seam.
    pub fn call(&mut self, method: &str, params: &Value) -> Result<Value, VenueApiError> {
        let rid = self.builder.next_id();
        // Key by method CLASS: matching-engine order ops (buy/sell/cancel/edit/close) draw the
        // ~5/s Tier4 "order" budget so a market-maker's cancel/replace burst blocks here instead of
        // racing into rejections; reconcile/resync/T1-requery READS (get_*) fall to the non-ME
        // default (~18/s) so they don't starve real orders — and, critically, so the T1 phantom
        // re-query (fired right after an ambiguous submit timeout) isn't throttled by a saturated
        // order budget at the worst moment.
        self.gate.gate(if is_matching_engine(method) { "order" } else { "read" });
        let socket =
            self.socket.as_mut().ok_or(VenueApiError { code: 0, msg: WS_NOT_CONNECTED.into() })?;
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": rid, "method": method, "params": params,
        });
        socket
            .send(Message::Text(frame.to_string().into()))
            .map_err(|e| {
                tracing::warn!(target: "vike_deribit::transport", error = %e, "order-WS request failed");
                VenueApiError {
                    code: 0,
                    msg: format!("{WS_SEND_FAILED}{e}"),
                }
            })?;
        let (result, error) = recv_until_id(socket, rid, self.request_timeout).map_err(|e| {
            VenueApiError {
                // ⚠ audit T1 — the WS twin of `vike_bridge_core::transport::read_body_ambiguous`'s
                // rule, which this transport did not mirror: **the READ half is ALWAYS ambiguous.**
                // `recv_until_id` is only ever reached once the send above SUCCEEDED, so the
                // request frame is demonstrably on the wire and Deribit may have acted on it —
                // whether we then time out, read the peer's `Close`, or hit a reset makes no
                // difference to that. Only the send is a definite pre-send failure that may
                // synthesize a terminal reject.
                //
                // This used to read `if e == WS_REQUEST_TIMED_OUT { … } else { 0 }`, which sent
                // `closed mid-request` — the COMMON first error on a dying socket, not a corner
                // case — down `client.rs`'s definite arm as an `OrderRejected` for an order the
                // venue may already have accepted: the exact stranded phantom audit T1 exists to
                // prevent, wired only to the timeout door.
                code: E_TIMEOUT_AMBIGUOUS,
                msg: e,
            }
        })?;
        Ok(serde_json::json!({"id": rid, "result": result, "error": error}))
    }

    /// Bounded, idempotent close — every error swallowed (teardown safe).
    pub fn close(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None);
        }
    }
}

/// Recv (bounded) until a frame with id==rid; skip interleaved notifications, wrong ids,
/// non-JSON keepalives. Answers protocol pings. Err(String) on timeout/transport failure.
fn recv_until_id(
    socket: &mut WsSocket,
    rid: i64,
    timeout: Duration,
) -> Result<(Option<Value>, Option<Value>), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(WS_REQUEST_TIMED_OUT.into());
        }
        let msg = match socket.read() {
            Ok(m) => m,
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(e) => return Err(format!("{WS_RECV_FAILED}{e}")),
        };
        match msg {
            Message::Text(raw) => {
                let Ok(frame) = serde_json::from_str::<Value>(raw.as_str()) else {
                    continue; // non-JSON keepalive — skip
                };
                let (fid, result, error) = parse_response(&frame);
                if fid != Some(rid) {
                    continue; // subscription notification / stale reply
                }
                return Ok((result, error));
            }
            Message::Ping(p) => {
                let _ = socket.send(Message::Pong(p));
            }
            Message::Close(_) => return Err(WS_CLOSED_MID_REQUEST.into()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_dead_socket_error, is_matching_engine, WS_REQUEST_TIMED_OUT};
    use vike_bridge_core::transport::{VenueApiError, E_TIMEOUT_AMBIGUOUS};

    /// The four dead-socket shapes, each with the code `call` really pairs it with — so this test
    /// pins the audit-T1 split as well as the predicate. The first is the the CI box shape verbatim:
    /// 795 of these disabled deribit reconcile for a day. The middle two are the read-half pair
    /// that used to carry `code: 0` and therefore reached `client.rs`'s definite arm as an
    /// `OrderRejected` for an order the venue may have accepted.
    #[test]
    fn classifies_a_dead_socket_from_the_messages_this_module_formats() {
        for (code, msg) in [
            (0, "send: Trying to work with closed connection"),
            (E_TIMEOUT_AMBIGUOUS, "recv: Connection reset without closing handshake"),
            (E_TIMEOUT_AMBIGUOUS, "closed mid-request"),
            (0, "DeribitOrderTransport.connect() not called"),
        ] {
            let err = VenueApiError { code, msg: msg.into() };
            assert!(is_dead_socket_error(&err), "{msg} should read as a dead socket");
        }
    }

    // The read half being ALWAYS ambiguous is a property of `call`, not of a constant, so it is
    // asserted where a real socket can die under a real request:
    // `tests/offline/exec_ambiguous_submit.rs`. A unit test here could only restate the literal it
    // is meant to be checking.

    /// ...and what must NOT read as one. A JSON-RPC error object is the venue ANSWERING over a
    /// live socket (`private_result` builds it from `{code, message}`), and a RESPONSE timeout is
    /// an unknown, not a death — whether that earns a re-dial is the caller's policy.
    #[test]
    fn a_venue_reply_and_an_ambiguous_timeout_are_not_a_dead_socket() {
        for err in [
            VenueApiError { code: 10009, msg: "not_enough_funds".into() },
            VenueApiError { code: 13009, msg: "unauthorized".into() },
            VenueApiError { code: -32602, msg: "Invalid params".into() },
            // a code-less venue error still is not one of this module's four messages
            VenueApiError { code: 0, msg: "some_venue_refusal".into() },
            VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: WS_REQUEST_TIMED_OUT.into() },
        ] {
            assert!(!is_dead_socket_error(&err), "{} should not read as a dead socket", err.msg);
        }
    }

    #[test]
    fn classifies_matching_engine_vs_reads() {
        // matching-engine order ops → the tight "order" budget
        for m in [
            "private/buy",
            "private/sell",
            "private/cancel",
            "private/cancel_all",
            "private/edit",
            "private/close_position",
        ] {
            assert!(is_matching_engine(m), "{m} should be matching-engine");
        }
        // reconcile / resync / T1-requery reads → the non-ME default
        for m in [
            "private/get_positions",
            "private/get_open_orders_by_instrument",
            "private/get_order_state_by_label",
            "private/get_user_trades_by_instrument",
            "private/get_order_history_by_instrument",
            "public/auth",
        ] {
            assert!(!is_matching_engine(m), "{m} should be a non-ME read/auth");
        }
    }
}
