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

use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::Message;
use vike_bridge_core::ws::{WsSocket, configure_ws_stream, is_timeout};
use vike_bridge_core::ws_proxy::connect_ws;

use crate::rpc::{JsonRpcBuilder, parse_response};
use vike_bridge_core::ratelimit::KeyedRateGate;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_bridge_core::user_data::UserDataAuthError;
use vike_venues::venue_timings::DERIBIT;

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
/// (`self.socket` stays `Some`) and the only re-dial in this transport is the PRE-SEND one of
/// `heal_if_stale` (an idle socket that fails its probe, before an order frame exists): `connect()`
/// is otherwise called once, by whoever owns the transport. A caller that wants to survive a venue-side close has to
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
/// preflight through `crates/bridges/deribit/src/adapter.rs`'s `DeribitVenueAdapter`
/// (`server_time_ms`).
///
/// ⚠ **A drifted clock cannot get a deribit order rejected**: [`crate::ws_auth`]'s
/// `build_client_credentials_auth` sends `grant_type: client_credentials`, which carries neither a
/// timestamp nor a nonce, so the deribit clock leg is a HOST-HEALTH canary rather than the
/// rejection guard it is on binance/bybit/okx/aster. It is wired anyway because it is free and a
/// wrong host clock still misdates every local record of a deribit session.
///
/// The stamp is a BARE `result` i64 in epoch ms at the top level (no `data`, no `code`). The body
/// also carries `usIn`/`usOut` in MICROSECONDS and a self-describing `"testnet"` boolean — which is
/// what lets the caller assert it reached the host the start actually binds. Measured from the CI box on
/// 2026-08-08: `{"jsonrpc":"2.0","result":1786218814983,"usIn":1786218814983507,
/// "usOut":1786218814983615,"usDiff":108,"testnet":true}`.
pub const PATH_TIME: &str = "/api/v2/public/get_time";

const POLL: Duration = Duration::from_secs(1);

/// Idle time after which a matching-engine call probes the socket first (one `public/test` round
/// trip, ~30 ms) instead of trusting it. Deribit's `public/set_heartbeat` is NOT used: its
/// `test_request` frames need a reader between calls and this transport reads only inside `call`,
/// so an unanswered heartbeat would make the venue close the socket itself.
/// shortcut: a fixed 5 s; measure the real idle cutoff on the CI box before raising it.
pub const IDLE_PROBE_AFTER: Duration = Duration::from_secs(5);
/// How long that probe waits: a black-holed socket must cost an order seconds, not the 10 s reply
/// wait. 4 s, not 2: a slow-but-ALIVE venue that answers in 2-4 s must not be torn down and re-dialed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// The access token `public/auth` hands out lives `expires_in` ~900 s, and this socket never refreshes
/// it (only the user-data socket does): past that, every `private/*` call answers [`UNAUTHORIZED`],
/// and `public/test` -- the idle probe -- does not check authentication at all. So a socket whose
/// auth is older than this (a third of the lifetime) is re-authenticated BEFORE the next private frame.
pub const AUTH_MAX_AGE: Duration = Duration::from_secs(300);
/// Deribit's `unauthorized` (expired/missing token). NOT proven to be a pre-execution refusal (the
/// docs say only "Wrong or expired authorization token or bad signature"), so an ORDER frame that
/// gets it is never re-sent: see [`DeribitOrderTransport::call`].
const UNAUTHORIZED: i64 = 13009;
/// The [`E_TIMEOUT_AMBIGUOUS`] message an order frame's [`UNAUTHORIZED`] reply becomes. Deliberately
/// NOT one of [`is_dead_socket_error`]'s messages: the socket is fine (it was re-authenticated).
const UNAUTHORIZED_ORDER: &str =
    "order-WS answered 13009 unauthorized to an order frame; not re-sent, outcome unconfirmed";
/// The heal's re-dial failed. Deliberately NOT one of [`is_dead_socket_error`]'s messages: the heal
/// already dialed once, so the caller must not dial a second time. Nothing was written (`code: 0`).
const REDIAL_FAILED: &str = "request not sent: stale order-WS re-dial failed: ";

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

/// A READ-ONLY private method: the only kind [`DeribitOrderTransport::call`] re-sends after a
/// [`UNAUTHORIZED`] reply. An allow-list on purpose: a method nobody classified is treated as a write.
fn is_read_only(method: &str) -> bool {
    method.starts_with("private/get_")
}

/// Is this `exchange` reply the venue's [`UNAUTHORIZED`] error object?
fn is_unauthorized(reply: &Value) -> bool {
    reply.pointer("/error/code").and_then(Value::as_i64) == Some(UNAUTHORIZED)
}

pub struct DeribitOrderTransport {
    ws_url: String,
    client_id: String,     // SECRET — never logged
    client_secret: String, // SECRET — never logged
    scope: Option<String>,
    pub builder: JsonRpcBuilder,
    /// How long each request, this socket's own auth included, waits for its id-matched reply:
    /// the deribit exec lane's `response_timeout` in `crates/vike-venues/src/venue_timings.rs`
    /// (decision 0160), which carries the value and the reason.
    request_timeout: Duration,
    socket: Option<WsSocket>,
    /// When the socket last carried a completed exchange (connect included) -- what
    /// [`Self::heal_if_stale`] measures idleness from.
    last_io: Instant,
    /// When the socket last completed `public/auth` (connect or in-place re-auth).
    authed_at: Instant,
    /// Re-authenticate before a private frame once `authed_at` is this old ([`AUTH_MAX_AGE`]).
    auth_max_age: Duration,
    /// A matching-engine frame sent after the socket sat this long is preceded by a `public/test`
    /// liveness probe. Nothing reads this socket between calls, so a peer/NAT that dropped it while
    /// idle is invisible until the first write -- which "succeeds" into the dead socket and loses
    /// the order (the the CI box drill of 2026-10-16).
    idle_probe_after: Duration,
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
            request_timeout: DERIBIT.exec().response_timeout(),
            socket: None,
            last_io: Instant::now(),
            authed_at: Instant::now(),
            auth_max_age: AUTH_MAX_AGE,
            idle_probe_after: IDLE_PROBE_AFTER,
            gate: crate::ratelimit::order_ws_gate(),
        }
    }

    /// Override [`IDLE_PROBE_AFTER`] (the offline tests use `Duration::ZERO` to make a socket stale
    /// without sleeping).
    pub fn with_idle_probe_after(mut self, idle: Duration) -> Self {
        self.idle_probe_after = idle;
        self
    }

    /// Override [`AUTH_MAX_AGE`] (`Duration::ZERO` = "the token is already old").
    pub fn with_auth_max_age(mut self, age: Duration) -> Self {
        self.auth_max_age = age;
        self
    }

    /// Open ONE socket + complete public/auth. Idempotent: re-connect closes the prior. The TCP dial
    /// is bounded by the exec lane's `dial_timeout` (a bare `tungstenite::connect` takes none).
    pub fn connect(&mut self) -> Result<(), UserDataAuthError> {
        self.close();
        let mut socket = connect_ws(&self.ws_url, None, Some(DERIBIT.exec().dial_timeout()))
            .map_err(|e| {
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
        self.last_io = Instant::now();
        self.authed_at = Instant::now();
        Ok(())
    }

    /// Sync bounded JSON-RPC request/response by id — the injected transport seam.
    ///
    /// A `private/*` call is first preceded by [`Self::heal_if_stale`] (token age; for a
    /// matching-engine call also idleness): the replacement/re-auth happens BEFORE the order frame
    /// exists, so it can never re-send anything. **This transport never re-sends an ORDER frame.**
    /// A [`UNAUTHORIZED`] reply is handled by [`is_read_only`] alone:
    ///
    /// * a READ-ONLY private method (`private/get_*`) is re-authenticated and re-asked once: a
    ///   second copy of a read changes nothing;
    /// * anything else (buy/sell/edit/cancel/close...) is NOT re-sent. The only evidence that 13009
    ///   is "refused before execution" is an architecture argument; Deribit's docs say just "Wrong
    ///   or expired authorization token or bad signature", and a Deribit `label` is not unique, so
    ///   a resend that the venue DID execute is a second real order. The socket is re-authenticated
    ///   in place (no frame is written, so that is safe and lets the caller's follow-up read ride a
    ///   valid token) and the call returns [`E_TIMEOUT_AMBIGUOUS`]: the caller resolves it by
    ///   label (`DeribitRest::resolve_ambiguous_submit`), exactly as for a lost reply.
    pub fn call(&mut self, method: &str, params: &Value) -> Result<Value, VenueApiError> {
        let private = method.starts_with("private/");
        if private {
            self.heal_if_stale(is_matching_engine(method))?;
        }
        let mut reply = self.exchange(method, params, self.request_timeout);
        if private && reply.as_ref().is_ok_and(is_unauthorized) {
            tracing::warn!(target: "vike_deribit::transport", method, "order-WS token refused (13009) - re-authenticating");
            let reauthed = self.reauth().is_ok();
            if !is_read_only(method) {
                return Err(VenueApiError {
                    code: E_TIMEOUT_AMBIGUOUS,
                    msg: UNAUTHORIZED_ORDER.into(),
                });
            }
            if reauthed {
                reply = self.exchange(method, params, self.request_timeout);
            }
        }
        if reply.is_ok() {
            self.last_io = Instant::now();
        }
        reply
    }

    /// Before a private frame: re-authenticate a token older than [`AUTH_MAX_AGE`] in place, else (a
    /// matching-engine call, `probe` true) probe a socket idle for [`IDLE_PROBE_AFTER`] with
    /// `public/test`. A failed re-auth or probe re-dials ONCE (`connect()` re-authenticates too).
    /// None of this is an order: no order frame has been written, so replacing the socket is safe and
    /// the caller's frame then rides a live one. A failed re-dial is a DEFINITE pre-send error
    /// (`code: 0`, [`REDIAL_FAILED`]) that is not a dead-socket error, so no caller dials again.
    fn heal_if_stale(&mut self, probe: bool) -> Result<(), VenueApiError> {
        if self.socket.is_none() {
            return Ok(());
        }
        if self.authed_at.elapsed() >= self.auth_max_age {
            if self.reauth().is_ok() {
                return Ok(());
            }
        } else if !probe || self.last_io.elapsed() < self.idle_probe_after {
            return Ok(());
        } else if self.exchange("public/test", &serde_json::json!({}), PROBE_TIMEOUT).is_ok() {
            self.last_io = Instant::now();
            return Ok(());
        }
        tracing::warn!(
            target: "vike_deribit::transport",
            "order-WS failed its re-auth/liveness probe - re-dialing before the order is sent"
        );
        self.connect().map_err(|e| VenueApiError { code: 0, msg: format!("{REDIAL_FAILED}{e}") })
    }

    /// `public/auth` again on the open socket (same credentials). Authenticated by construction: a
    /// success proves both liveness and a fresh token.
    fn reauth(&mut self) -> Result<(), VenueApiError> {
        let frame = crate::ws_auth::build_client_credentials_auth(
            &self.client_id,
            &self.client_secret,
            self.scope.as_deref(),
            0,
        );
        let reply = self.exchange("public/auth", &frame["params"], self.request_timeout)?;
        if let Some(err) = reply.get("error").filter(|e| !e.is_null()) {
            // message only -- the params (credentials) never reach a log or an error
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
            return Err(VenueApiError { code: 0, msg: format!("order-WS re-auth refused: {msg}") });
        }
        self.authed_at = Instant::now();
        self.last_io = Instant::now();
        Ok(())
    }

    fn exchange(
        &mut self,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, VenueApiError> {
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
        let (result, error) = recv_until_id(socket, rid, timeout).map_err(|e| {
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
            Err(e) if is_timeout(&e) => continue,
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
    use super::{
        DeribitOrderTransport, REDIAL_FAILED, WS_REQUEST_TIMED_OUT, is_dead_socket_error,
        is_matching_engine, is_read_only, is_unauthorized,
    };
    use std::time::Duration;
    use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};

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

    /// The reply wait the order socket really runs on is the 10 s its `new` held as a literal before
    /// the venue timings table existed (decision 0160): evidence beside the table's own matrix pin.
    /// Read off a constructed transport, so it is the field `connect` and `call` hand to
    /// `recv_until_id`, not a restatement of the row.
    #[test]
    fn the_order_socket_waits_ten_seconds_for_each_reply() {
        let transport = DeribitOrderTransport::new("wss://unused.invalid", "id", "secret", None);
        assert_eq!(transport.request_timeout, Duration::from_secs(10));
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

    /// The heal dialed once already: its failure must not read as a dead socket, or the caller dials
    /// a second time. And a 13009 reply is recognised by its code, nothing else.
    #[test]
    fn a_failed_heal_redial_is_not_a_dead_socket_and_13009_is_recognised() {
        let err = VenueApiError { code: 0, msg: format!("{REDIAL_FAILED}connection refused") };
        assert!(!is_dead_socket_error(&err));
        assert!(is_unauthorized(&serde_json::json!({"error": {"code": 13009}})));
        assert!(!is_unauthorized(&serde_json::json!({"error": {"code": 10009}})));
        assert!(!is_unauthorized(&serde_json::json!({"error": null, "result": {}})));
    }
    /// Only a `private/get_*` read may be re-sent after a 13009; every order verb (and anything
    /// unclassified) is a write.
    #[test]
    fn only_a_private_get_is_read_only() {
        assert!(is_read_only("private/get_order_state_by_label"));
        for m in
            ["private/buy", "private/edit", "private/cancel", "private/create_combo", "public/auth"]
        {
            assert!(!is_read_only(m), "{m}");
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
