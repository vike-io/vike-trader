//! DeribitTransport — a persistent, authed JSON-RPC-over-WS ORDER transport. Port of
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
use vike_transport::websocket::{WsSocket, configure_websocket_stream, is_timeout};
use vike_transport::websocket_proxy::connect_websocket;

use crate::transport::rpc::{JsonRpcBuilder, parse_response};
use vike_bridge_core::ratelimit::RateGatePerKey;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_bridge_core::websocket_session::user::WebsocketSessionUserAuthenticationError;
use vike_venues::venue_timings::DERIBIT;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// The `recv_until_id` deadline-exceeded message. The socket may be perfectly alive and the venue
/// merely slow, so this is the ONE transport failure that is NOT a [`CallError::DeadSocket`] — while
/// still carrying [`E_TIMEOUT_AMBIGUOUS`] like every other read-half failure.
const WEBSOCKET_REQUEST_TIMED_OUT: &str = "Deribit order-WS request timed out";

// The four DEAD-SOCKET messages, each formatted at the one site that returns it as a
// [`CallError::DeadSocket`]: a caller decides on the variant, never on these words. ⚠ They do NOT
// all carry the same code, and the split is the audit-T1 one: the two that happen BEFORE anything
// reached the wire are definite (`code: 0`), the two that happen after it did are ambiguous. "The
// socket is unusable" and "the ORDER's outcome is unknown" are two different questions — the
// variant answers only the first, and `code` answers the second.
/// `call`'s send-failure prefix, `code: 0` — DEFINITE (tungstenite's own error text follows; on the
/// the CI box incident of 2026-08-23 it was "Trying to work with closed connection", 795 times).
const WEBSOCKET_SEND_FAILED: &str = "send: ";
/// `recv_until_id`'s read-failure prefix, [`E_TIMEOUT_AMBIGUOUS`] — the request was already sent.
const WEBSOCKET_RECV_FAILED: &str = "recv: ";
/// `recv_until_id` saw the peer's `Close` while waiting, [`E_TIMEOUT_AMBIGUOUS`] — same reason.
const WEBSOCKET_CLOSED_MID_REQUEST: &str = "closed mid-request";
/// `call` was reached with no socket at all, `code: 0` — DEFINITE, nothing was sent.
/// [`DeribitTransport::connect`] never ran, or a [`DeribitTransport::close`] took it.
const WEBSOCKET_NOT_CONNECTED: &str = "DeribitTransport.connect() not called";

/// **A failed [`DeribitTransport::call`]**, typed for the one question its owners ask of the failure
/// itself: is THIS TRANSPORT'S SOCKET unusable, as opposed to the venue having answered with a
/// JSON-RPC error object? Both variants carry the [`VenueApiError`] (its code and words, unchanged)
/// every other question is asked of.
///
/// It exists because [`DeribitTransport::call`] leaves a failed socket in place
/// (`self.socket` stays `Some`) and the only re-dial in this transport is the PRE-SEND one of
/// `heal_if_stale` (an idle socket that fails its probe, before an order frame exists): `connect()`
/// is otherwise called once, by whoever owns the transport. A caller that wants to survive a venue-side close has to
/// ask this question and re-connect ITSELF —
/// `crates/bridges/deribit/src/reconcile/mod.rs`'s `ReconcileClientDeribit::call` and
/// `crates/bridges/deribit/src/execution/rest.rs`'s `VenueRestDeribit::redial_order_socket` are the two that do.
///
/// ⚠ **A [`CallError::DeadSocket`] licenses a RE-DIAL and nothing more.** It says the socket must be
/// replaced before it can carry another frame; it says NOTHING about whether the failed request
/// reached the venue. That second question is the error's `code` — and on the exec path re-SENDING
/// an order because the socket was replaced would be a second real order. See
/// `VenueRestDeribit::dispatch_submit`.
#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    /// The socket is unusable: the send failed (`code: 0`, nothing written), the read failed or the
    /// peer closed it mid-request ([`E_TIMEOUT_AMBIGUOUS`], the frame was on the wire), or there is
    /// none (`code: 0`: [`DeribitTransport::connect`] never ran, or [`DeribitTransport::close`] took
    /// it).
    DeadSocket(VenueApiError),
    /// Anything else: the venue's own answer, a RESPONSE timeout ([`WEBSOCKET_REQUEST_TIMED_OUT`]: a
    /// slow venue is not a dead socket, and whether one earns a re-dial anyway is the CALLER's
    /// policy), an order frame answered 13009, a heal whose re-dial failed (it dialed already, so
    /// the caller must not dial again), or a reply that states nothing.
    Failed(VenueApiError),
}

impl CallError {
    /// Whether THIS transport's socket must be replaced before it carries another frame.
    #[must_use]
    pub fn is_dead_socket(&self) -> bool {
        matches!(self, CallError::DeadSocket(_))
    }

    /// The error itself: its `code` (definite `0`, [`E_TIMEOUT_AMBIGUOUS`] or the venue's) and words.
    #[must_use]
    pub fn error(&self) -> &VenueApiError {
        match self {
            CallError::DeadSocket(error) | CallError::Failed(error) => error,
        }
    }
}

impl From<CallError> for VenueApiError {
    fn from(error: CallError) -> Self {
        match error {
            CallError::DeadSocket(error) | CallError::Failed(error) => error,
        }
    }
}

/// The error's own words, exactly as [`VenueApiError`] writes them.
impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error().fmt(f)
    }
}

impl std::error::Error for CallError {}

/// The REST base every exec-side keyless read (the instrument rows, the door's symbol facts, the clock)
/// asks: the `demo` `trade` `rest` row (`test.deribit.com`). The TESTNET whatever the account's tier:
/// deribit's live trading is not wired (`crates/bridges/deribit/CLAUDE.md`'s "The two halves point at
/// DIFFERENT NETWORKS"), so the type is `demo` here, never the tier's.
///
/// # Errors
/// The row's refusal, whose words are the account's NOT RUNNING reason.
pub fn testnet_rest(urls: &VenueUrls) -> Result<&str, UrlRefusal> {
    urls.url(UrlType::Demo, UrlChannel::Trade, UrlProtocol::Rest, "")
}

/// **The exec half's two TESTNET addresses**, looked up once at the start: every authed socket (order,
/// A3 resync, user data, reconcile, funding, the operator's read) dials [`DeribitUrls::ws`], and the
/// keyless instrument reads [`DeribitUrls::rest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeribitUrls {
    /// The `demo` `trade` `ws` row plus [`crate::transport::rpc::WEBSOCKET_PATH`].
    pub ws: String,
    /// [`testnet_rest`].
    pub rest: String,
}

impl DeribitUrls {
    /// The testnet's addresses out of deribit's rows (`AdapterInputs::settings`' `urls()`).
    ///
    /// # Errors
    /// The first row refused, whose words are the account's NOT RUNNING reason.
    pub fn of(urls: &VenueUrls) -> Result<DeribitUrls, UrlRefusal> {
        let ws = urls.url(UrlType::Demo, UrlChannel::Trade, UrlProtocol::Websocket, "")?;
        Ok(DeribitUrls {
            ws: format!("{ws}{}", crate::transport::rpc::WEBSOCKET_PATH),
            rest: testnet_rest(urls)?.to_string(),
        })
    }
}

/// PUBLIC (keyless) server clock, over the JSON-RPC-shaped REST surface. Read by the startup
/// preflight through `crates/bridges/deribit/src/adapter.rs`'s `VenueAdapterDeribit`
/// (`server_time_ms`).
///
/// ⚠ **A drifted clock cannot get a deribit order rejected**: [`crate::transport::ws_auth`]'s
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

/// The longest one reply can hold its caller: the exec lane's reply wait, plus the poll of the read
/// that straddles its deadline ([`recv_until_id`]).
pub(crate) fn reply_bound() -> Duration {
    DERIBIT.exec().response_timeout() + POLL
}

/// The longest [`DeribitTransport::connect`] can hold its caller: the dial and the auth reply. Not
/// counted: a TLS + WS handshake answering each of its few reads just in time, each with a dial window of
/// its own (`vike_transport::websocket_proxy`'s module doc).
pub(crate) fn redial_bound() -> Duration {
    DERIBIT.exec().dial_timeout() + reply_bound()
}

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
/// gets it is never re-sent: see [`DeribitTransport::call`].
const UNAUTHORIZED: i64 = 13009;
/// The [`E_TIMEOUT_AMBIGUOUS`] message an order frame's [`UNAUTHORIZED`] reply becomes. Deliberately
/// a [`CallError::Failed`]: the socket is fine (it was re-authenticated).
const UNAUTHORIZED_ORDER: &str =
    "order-WS answered 13009 unauthorized to an order frame; not re-sent, outcome unconfirmed";
/// The heal's re-dial failed. Deliberately a [`CallError::Failed`]: the heal already dialed once, so
/// the caller must not dial a second time. Nothing was written (`code: 0`).
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

/// A READ-ONLY private method: the only kind [`DeribitTransport::call`] re-sends after a
/// [`UNAUTHORIZED`] reply. An allow-list on purpose: a method nobody classified is treated as a write.
fn is_read_only(method: &str) -> bool {
    method.starts_with("private/get_")
}

/// Is this `exchange` reply the venue's [`UNAUTHORIZED`] error object?
fn is_unauthorized(reply: &Value) -> bool {
    reply.pointer("/error/code").and_then(Value::as_i64) == Some(UNAUTHORIZED)
}

pub struct DeribitTransport {
    websocket_url: String,
    client_id: String,     // SECRET — never logged
    client_secret: String, // SECRET — never logged
    scope: Option<String>,
    pub builder: JsonRpcBuilder,
    /// How long each request, this socket's own auth included, waits for its id-matched reply:
    /// the deribit exec lane's `response_timeout` in `crates/vike-venues/src/venue_timings.rs`
    /// (decision 0160), which carries the value and the reason.
    request_timeout: Duration,
    /// The bound of [`Self::connect`]'s dial: the TCP phase AND, since 0.1.97, each read of the TLS + WS
    /// handshake behind it (`vike_transport::websocket_proxy::connect_websocket`'s module doc). The exec lane's
    /// `dial_timeout` in `crates/vike-venues/src/venue_timings.rs` (decision 0160), which carries the
    /// value and the reason.
    dial_timeout: Duration,
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
    gate: RateGatePerKey,
}

impl DeribitTransport {
    pub fn new(
        websocket_url: &str,
        client_id: &str,
        client_secret: &str,
        scope: Option<&str>,
    ) -> Self {
        DeribitTransport {
            websocket_url: websocket_url.to_string(),
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            scope: scope.map(str::to_string),
            builder: JsonRpcBuilder::new(),
            request_timeout: DERIBIT.exec().response_timeout(),
            dial_timeout: DERIBIT.exec().dial_timeout(),
            socket: None,
            last_io: Instant::now(),
            authed_at: Instant::now(),
            auth_max_age: AUTH_MAX_AGE,
            idle_probe_after: IDLE_PROBE_AFTER,
            gate: crate::transport::ratelimit::order_websocket_gate(),
        }
    }

    /// Override [`IDLE_PROBE_AFTER`] (the offline tests use `Duration::ZERO` to make a socket stale
    /// without sleeping).
    pub fn with_idle_probe_after(mut self, idle: Duration) -> Self {
        self.idle_probe_after = idle;
        self
    }

    /// Override the dial bound, the exec lane's `dial_timeout` (the offline tests use a few hundred
    /// milliseconds to prove that a peer which accepts the TCP connection and then says nothing cannot
    /// hold [`Self::connect`]).
    pub fn with_dial_timeout(mut self, dial: Duration) -> Self {
        self.dial_timeout = dial;
        self
    }

    /// Override [`AUTH_MAX_AGE`] (`Duration::ZERO` = "the token is already old").
    pub fn with_auth_max_age(mut self, age: Duration) -> Self {
        self.auth_max_age = age;
        self
    }

    /// Open ONE socket + complete public/auth. Idempotent: re-connect closes the prior. The dial is
    /// bounded by the exec lane's `dial_timeout` (a bare `tungstenite::connect` takes none): the TCP
    /// phase under that one window, and each read of the TLS + WS handshake after it under the same
    /// one, so a peer that accepts the connection and then says nothing fails this call instead of
    /// holding it (and the thread that made it) for ever. The auth reply that follows is bounded by
    /// `request_timeout`.
    pub fn connect(&mut self) -> Result<(), WebsocketSessionUserAuthenticationError> {
        self.close();
        let mut socket = connect_websocket(&self.websocket_url, None, Some(self.dial_timeout)).map_err(|e| {
            tracing::error!(target: "vike_deribit::transport::client", error = %e, "order-WS connect failed");
            WebsocketSessionUserAuthenticationError(format!("Deribit order-WS connect failed: {e}"))
        })?;
        configure_websocket_stream(&socket, POLL);
        let auth_id = self.builder.next_id();
        let frame = crate::transport::ws_auth::build_client_credentials_auth(
            &self.client_id,
            &self.client_secret,
            self.scope.as_deref(),
            auth_id,
        );
        self.gate.gate("login"); // non-matching-engine (default quota)
        socket
            .send(Message::Text(frame.to_string().into())) // NEVER log (carries creds)
            .map_err(|e| {
                WebsocketSessionUserAuthenticationError(format!(
                    "Deribit order-WS auth send failed: {e}"
                ))
            })?;
        let (_result, error) =
            recv_until_id(&mut socket, auth_id, self.request_timeout).map_err(|e| {
                WebsocketSessionUserAuthenticationError(format!(
                    "Deribit order-WS auth failed: {}",
                    e.error().msg
                ))
            })?;
        if let Some(err) = error {
            let _ = socket.close(None);
            // NEVER include client_id/client_secret in the message
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
            tracing::error!(target: "vike_deribit::transport::client", msg, "order-WS auth failed");
            return Err(WebsocketSessionUserAuthenticationError(format!(
                "Deribit order-WS auth failed: {msg}"
            )));
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
    ///   label (`VenueRestDeribit::resolve_ambiguous_submit`), exactly as for a lost reply.
    pub fn call(&mut self, method: &str, params: &Value) -> Result<Value, CallError> {
        let private = method.starts_with("private/");
        if private {
            self.heal_if_stale(is_matching_engine(method))?;
        }
        let mut reply = self.exchange(method, params, self.request_timeout);
        if private && reply.as_ref().is_ok_and(is_unauthorized) {
            tracing::warn!(target: "vike_deribit::transport::client", method, "order-WS token refused (13009) - re-authenticating");
            let reauthed = self.reauth().is_ok();
            if !is_read_only(method) {
                return Err(CallError::Failed(VenueApiError {
                    code: E_TIMEOUT_AMBIGUOUS,
                    msg: UNAUTHORIZED_ORDER.into(),
                }));
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
    /// (`code: 0`, [`REDIAL_FAILED`]) that is a [`CallError::Failed`], so no caller dials again.
    fn heal_if_stale(&mut self, probe: bool) -> Result<(), CallError> {
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
            target: "vike_deribit::transport::client",
            "order-WS failed its re-auth/liveness probe - re-dialing before the order is sent"
        );
        self.connect().map_err(|e| {
            CallError::Failed(VenueApiError { code: 0, msg: format!("{REDIAL_FAILED}{e}") })
        })
    }

    /// `public/auth` again on the open socket (same credentials). Authenticated by construction: a
    /// success proves both liveness and a fresh token.
    fn reauth(&mut self) -> Result<(), CallError> {
        let frame = crate::transport::ws_auth::build_client_credentials_auth(
            &self.client_id,
            &self.client_secret,
            self.scope.as_deref(),
            0,
        );
        let reply = self.exchange("public/auth", &frame["params"], self.request_timeout)?;
        if let Some(err) = reply.get("error").filter(|e| !e.is_null()) {
            // message only -- the params (credentials) never reach a log or an error
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
            return Err(CallError::Failed(VenueApiError {
                code: 0,
                msg: format!("order-WS re-auth refused: {msg}"),
            }));
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
    ) -> Result<Value, CallError> {
        let rid = self.builder.next_id();
        // Key by method CLASS: matching-engine order ops (buy/sell/cancel/edit/close) draw the
        // ~5/s Tier4 "order" budget so a market-maker's cancel/replace burst blocks here instead of
        // racing into rejections; reconcile/resync/T1-requery READS (get_*) fall to the non-ME
        // default (~18/s) so they don't starve real orders — and, critically, so the T1 phantom
        // re-query (fired right after an ambiguous submit timeout) isn't throttled by a saturated
        // order budget at the worst moment.
        self.gate.gate(if is_matching_engine(method) { "order" } else { "read" });
        let socket = self.socket.as_mut().ok_or(CallError::DeadSocket(VenueApiError {
            code: 0,
            msg: WEBSOCKET_NOT_CONNECTED.into(),
        }))?;
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": rid, "method": method, "params": params,
        });
        socket
            .send(Message::Text(frame.to_string().into()))
            .map_err(|e| {
                tracing::warn!(target: "vike_deribit::transport::client", error = %e, "order-WS request failed");
                CallError::DeadSocket(VenueApiError {
                    code: 0,
                    msg: format!("{WEBSOCKET_SEND_FAILED}{e}"),
                })
            })?;
        // ⚠ audit T1 — the WS twin of `vike_bridge_core::transport::read_body_ambiguous`'s rule,
        // which this transport did not mirror: **the READ half is ALWAYS ambiguous.** `recv_until_id`
        // is only ever reached once the send above SUCCEEDED, so the request frame is demonstrably on
        // the wire and Deribit may have acted on it — whether we then time out, read the peer's
        // `Close`, or hit a reset makes no difference to that, so every error `recv_until_id` returns
        // carries `E_TIMEOUT_AMBIGUOUS`. Only the send is a definite pre-send failure that may
        // synthesize a terminal reject.
        //
        // This used to read `if e == WEBSOCKET_REQUEST_TIMED_OUT { … } else { 0 }`, which sent
        // `closed mid-request` — the COMMON first error on a dying socket, not a corner case — down
        // `execution/rest.rs`'s definite arm as an `OrderRejected` for an order the venue may already
        // have accepted: the exact stranded phantom audit T1 exists to prevent, wired only to the
        // timeout door.
        let (result, error) = recv_until_id(socket, rid, timeout)?;
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
/// non-JSON keepalives. Answers protocol pings. Every error is [`E_TIMEOUT_AMBIGUOUS`] (it is reached
/// only after the request was sent): the deadline is a [`CallError::Failed`] (a slow venue), a read
/// failure or the peer's `Close` a [`CallError::DeadSocket`].
fn recv_until_id(
    socket: &mut WsSocket,
    rid: i64,
    timeout: Duration,
) -> Result<(Option<Value>, Option<Value>), CallError> {
    let read_half = |msg: String| VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg };
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return Err(CallError::Failed(read_half(WEBSOCKET_REQUEST_TIMED_OUT.into())));
        }
        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) if is_timeout(&e) => continue,
            Err(e) => {
                return Err(CallError::DeadSocket(read_half(format!(
                    "{WEBSOCKET_RECV_FAILED}{e}"
                ))));
            }
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
            Message::Close(_) => {
                return Err(CallError::DeadSocket(read_half(WEBSOCKET_CLOSED_MID_REQUEST.into())));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CallError, DeribitTransport, WEBSOCKET_NOT_CONNECTED, is_matching_engine, is_read_only,
        is_unauthorized,
    };
    use std::time::Duration;
    use vike_bridge_core::transport::VenueApiError;

    /// **A transport with no socket says so as a [`CallError::DeadSocket`]**, definite (`code: 0`: nothing was
    /// written), for a public and a private method alike, in the words it always used. The send, read and
    /// close-mid-request producers need a real socket and are asserted against the fake venue
    /// (`tests/offline/execution_ambiguous_submit.rs`'s `a_dead_socket_is_a_variant_of_the_call_and_a_venue_answer_is_not`).
    #[test]
    fn a_transport_with_no_socket_is_a_dead_socket_nothing_was_written_to() {
        let mut transport = DeribitTransport::new("wss://unused.invalid", "id", "secret", None);
        for method in ["public/test", "private/get_positions", "private/buy"] {
            assert_eq!(
                transport.call(method, &serde_json::json!({})),
                Err(CallError::DeadSocket(VenueApiError {
                    code: 0,
                    msg: WEBSOCKET_NOT_CONNECTED.into()
                })),
                "{method}"
            );
        }
    }

    /// The variant is the decision, the error inside it everything else: its code and words, and its
    /// `Display`, exactly what the [`VenueApiError`] said before it was wrapped.
    #[test]
    fn a_call_error_keeps_its_venue_api_error_byte_for_byte() {
        let error = VenueApiError { code: 10009, msg: "not_enough_funds".into() };
        let dead = CallError::DeadSocket(error.clone());
        let failed = CallError::Failed(error.clone());
        assert!(dead.is_dead_socket() && !failed.is_dead_socket());
        assert_eq!((dead.error(), failed.error()), (&error, &error));
        assert_eq!((dead.to_string(), failed.to_string()), (error.to_string(), error.to_string()));
        assert_eq!(VenueApiError::from(dead), error);
    }

    /// The reply wait the order socket really runs on is the 10 s its `new` held as a literal before
    /// the venue timings table existed (decision 0160): evidence beside the table's own matrix pin.
    /// Read off a constructed transport, so it is the field `connect` and `call` hand to
    /// `recv_until_id`, not a restatement of the row.
    #[test]
    fn the_order_socket_waits_ten_seconds_for_each_reply() {
        let transport = DeribitTransport::new("wss://unused.invalid", "id", "secret", None);
        assert_eq!(transport.request_timeout, Duration::from_secs(10));
    }

    // The read half being ALWAYS ambiguous is a property of `call`, not of a constant, so it is
    // asserted where a real socket can die under a real request:
    // `tests/offline/execution_ambiguous_submit.rs`. A unit test here could only restate the literal it
    // is meant to be checking.

    /// A 13009 reply is recognised by its code, nothing else. (A heal whose re-dial failed is a
    /// [`CallError::Failed`], so its caller does not dial a second time:
    /// `tests/offline/execution_ambiguous_submit.rs`'s `a_failed_heal_redial_is_a_definite_reject_with_an_accurate_reason`.)
    #[test]
    fn a_13009_reply_is_recognised_by_its_code() {
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
