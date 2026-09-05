//! The cpapi (Client Portal Web API) backend: REST place/cancel/modify/resync + a WebSocket push
//! pump, adapted onto the socket-shaped `IbkrTransport` seam (background pump → mpsc → next_recv,
//! exactly like `SocketTransport`). Assumes a RUNNING, browser-authenticated CP Gateway at
//! `cfg.cpapi_url` (default `https://127.0.0.1:5000`). Lifecycle/login is out of scope here.
//!
//! Modules: `endpoints` = REST wrappers (`CpapiRest`); `decode` = pure JSON→IbInbound / conId
//! decoders; `reply` = the order reply-confirmation state machine. This file wires them into
//! `CpapiTransport`.
//!
//! ## The coid→order_id map trick (keeps the shared `EventMapper` byte-identical)
//! Decoders emit `IbInbound` with `order_id: 0` and `order_ref: <cOID>` (the coid). `CpapiTransport`
//! keeps a `Numbering` map, populated in `place_order` (the coid the exec loop bound → the numeric
//! `order_id` arg it allocated). `next_recv`/`request_open_orders` fill the numeric via
//! `number_inbound`/`resolve_numeric` before handing the inbound to the shared `run_exec` loop, so
//! `EventMapper.fill_state` (keyed by numeric order_id) matches exactly like the socket backend —
//! no change needed to the shared layer. A coid with no allocation (a foreign/prior-session order
//! surfaced by reconnect resync) gets a fresh unique NEGATIVE synthetic id so distinct foreign
//! orders never collide on id 0 in the shared `IdRegistry`.
//!
//! ## Wire shapes — LIVE-VERIFIED against a paper CP Gateway (DUQ186573, 2026-07-15)
//! The submit→accept→cancel round-trip was proven end-to-end via `tests/ibkr_cpapi_smoke.rs`.
//! Ground truth captured that run:
//!   - REST paths under `/v1/api/iserver/...`; place = `POST /iserver/account/{acct}/orders` with
//!     body `{"orders":[{conid, orderType, side, tif, quantity, price}]}`.
//!   - Place returns a **CHAIN** of confirm questions (price-constraint → no-market-data → …), each
//!     answered by `POST /iserver/reply/{id}` `{"confirmed":true}`, until a terminal row
//!     `[{"order_id":"1794719002","order_status":"PreSubmitted"}]`. `place_order` loops the replies
//!     (bounded by `MAX_REPLY_HOPS`) and captures that `order_id`.
//!   - `orderType` `LMT`/`MKT`/`STP`; `side` `BUY`/`SELL`; `tif` `GTC` — all accepted.
//!   - **Cancel-key identity:** cancel/modify address the order by the GATEWAY's own `order_id`
//!     (captured at accept), NOT our allocated numeric — the numeric fails
//!     `{"error":"OrderID 101 doesn't exist"}`. Cancel = `DELETE /iserver/account/{acct}/order/{gw_id}`.
//!   - `secdef/search` returns `conid` as a JSON **string** (`decode.rs::decode_conid`).
//!   - The WS `sor` stream is a best-effort supplement; the terminal accept/cancel events are emitted
//!     directly from the REST acks (`place_order`/`cancel_order`), so the lifecycle does not depend
//!     on the WS payload shape.
//!
//! ## ⚠⚠ MEASURED 2026-08-23: the `sor` topic answers NOTHING, so cpapi FILLS ARE INVISIBLE
//!
//! The subscribe frame `pump_loop` sends (`sor+{}`) was carried as a GUESS. It is no longer a
//! guess — it is measured, and it does not work. Against the CI box's live authenticated gateway
//! (DUQ186573, Build 10.46.2d), on the SAME socket and in the SAME session:
//!
//! | subscribe frame | topic frames received |
//! |---|---|
//! | `sor+{}` | **none, ever** |
//! | `str+{}` (trades) | `{"topic":"str","args":[]}` |
//! | `spl+{}` (PnL) | `{"topic":"spl","args":{"DUQ186573.Core":{…}}}` |
//! | `sld+{acct}` (ledger) | full ledger row |
//! | `ssd+{acct}` (summary) | full summary row |
//!
//! So the socket, the auth and the subscribe mechanism are all fine — `sor` alone is inert. Three
//! spellings were tried (`sor+{}` on open, `sor+{}` after the `sts` auth frame with the REST order
//! snapshot primed first, and a bare `sor`); all three answered nothing. A place → accept → cancel
//! cycle driven WHILE subscribed produced `ntf` frames naming that very `orderId` and still no
//! `sor` frame, so the gateway plainly knew about the order.
//!
//! **Why the smoke is green anyway, and why that hid this:** `place_order` and `cancel_order`
//! synthesize their `OrderStatus` inbounds directly from the REST acks (see both methods), so the
//! whole submit→accept→cancel lifecycle the smoke asserts never touches the WS. The doc line above
//! calls the WS "a best-effort supplement" and is right about accept/cancel — but a FILL is not
//! supplementary and has no other live path:
//!
//! * `decode::decode_ws_frame` only decodes `topic == "sor"`, so no execution row can ever arrive.
//! * `request_executions` is deliberately NOT overridden here — `IbkrTransport`'s default doc argues
//!   a cpapi fill "structurally cannot strand" because one `sor` row decodes into both halves at
//!   once. That argument is sound and its premise is false: there is no `sor` row.
//! * The only surviving path is `request_open_orders`, which DOES fetch `/iserver/account/trades`
//!   and decode it — but it runs only on `IbInbound::StreamResync`, i.e. when the WS happens to
//!   drop. A fill is therefore recovered by accident, on a reconnect that may never come.
//!
//! **NOT FIXED HERE, deliberately.** The candidate is `str+{}`, which answers — but its row shape is
//! unknown while no fill exists to observe, and inventing a decoder for it would be exactly the
//! guess this section exists to retire. What it needs is a live fill: a marketable order during a
//! trading session (US equities were CLOSED when this was measured — Sunday), after which `str`'s
//! rows can be captured and `decode_ws_frame` extended against real bytes.

// `pub` (not `pub(crate)`): `transport::mod` re-exports this module further as `cpapi_decode` for
// the no-network fixture test (`tests/ibkr_cpapi_decode.rs`), which needs it visible outside the
// crate too — `pub(crate)` alone cannot be re-exported past the crate boundary (E0365).
pub mod decode;
mod endpoints;
mod reply;

pub use endpoints::CpapiRest;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;

use super::{IbInbound, IbkrTransport};
use crate::config::IbkrConfig;
use crate::contract::IbkrContract;
use crate::error::IbkrError;
use crate::order::IbOrderSpec;
use decode::{decode_conid, decode_executions, decode_open_orders, decode_ws_frame};
use reply::{PlaceOutcome, parse_place_response};

/// Bound on the parse→reply hop count for a single `place_order` call, so a pathological gateway
/// that never stops asking questions cannot hang the exec loop's calling thread forever.
const MAX_REPLY_HOPS: usize = 8;

/// The cOID→numeric-order_id map plus a synthetic-id counter (see [`resolve_numeric`]). Only the
/// exec-loop thread (which holds `&CpapiTransport`) touches it — the pump does not — so a plain
/// `Mutex` (no `Arc`) suffices.
struct Numbering {
    map: HashMap<String, i32>,
    /// Our numeric order_id → the GATEWAY's own order id string, captured from the place-accept
    /// response. cpapi cancel/modify address the order by THIS gateway id, NOT our allocated numeric
    /// (live-verified 2026-07-15: cancelling by the numeric → `{"error":"OrderID 101 doesn't exist"}`).
    gw: HashMap<i32, String>,
    /// Next synthetic id for a coid with no real allocation (a foreign/prior-session order surfaced
    /// by reconnect resync). Starts at -1 and decrements — negative so it never collides with the
    /// exec loop's 101+ allocations.
    synth_next: i32,
}

pub struct CpapiTransport {
    rest: CpapiRest,
    inbound: Receiver<IbInbound>,
    tx: Sender<IbInbound>,
    stop: Arc<AtomicBool>,
    /// coid→numeric-order_id recovery (see [`Numbering`] / [`resolve_numeric`]): own-session orders
    /// are inserted in `place_order`; inbound for an unknown coid gets a fresh synthetic id so the
    /// shared `EventMapper.fill_state` (keyed by order_id) matches and foreign orders never collide.
    numbering: Mutex<Numbering>,
    /// symbol → resolved conId cache (secdef/search is a network round trip; resolve once per
    /// symbol). ⚠ STILL ASSUMED: that the gateway's conId is stable across a session. The 2026-08-23
    /// live pass confirmed `secdef/search` RETURNS a conId (`265598` for AAPL) and that placing
    /// against it works, but a single session cannot observe a conId CHANGING — only a session that
    /// spans a contract roll or a gateway restart can, so this one is not retired.
    conid_cache: Mutex<HashMap<String, i64>>,
}

impl CpapiTransport {
    pub fn connect(cfg: &IbkrConfig) -> Result<CpapiTransport, IbkrError> {
        let rest = CpapiRest::new(&cfg.cpapi_url, &cfg.account);
        // Readiness: a tickle must succeed (gateway up + browser-authenticated) or we degrade to
        // paper — the cpapi backend has no in-crate login flow (design doc: lifecycle/login is out
        // of scope for this cluster).
        rest.tickle().map_err(|e| IbkrError::Connect(format!("cpapi tickle: {}", e.msg)))?;

        let (tx, rx) = mpsc::channel::<IbInbound>();
        // Seed id/account readiness (mirrors SocketTransport::connect); the cpapi backend has no
        // native nextValidId concept, so seed a floor the IdRegistry will raise as needed.
        let _ = tx.send(IbInbound::NextValidId(101));
        let _ = tx.send(IbInbound::AccountsReady);

        let stop = Arc::new(AtomicBool::new(false));

        // WS pump + tickle keepalive thread.
        let ws_url = ws_url_from(&cfg.cpapi_url);
        let pump_tx = tx.clone();
        let pump_stop = Arc::clone(&stop);
        let pump_rest_url = cfg.cpapi_url.clone();
        let pump_account = cfg.account.clone();
        thread::Builder::new()
            .name("ibkr-cpapi-pump".into())
            .spawn(move || pump_loop(&ws_url, &pump_rest_url, &pump_account, pump_tx, pump_stop))
            .map_err(|e| IbkrError::Connect(format!("cpapi pump spawn: {e}")))?;

        Ok(CpapiTransport {
            rest,
            inbound: rx,
            tx,
            stop,
            numbering: Mutex::new(Numbering {
                map: HashMap::new(),
                gw: HashMap::new(),
                synth_next: -1,
            }),
            conid_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Fill the numeric order_id into an inbound (see [`Numbering`] / [`resolve_numeric`]).
    fn number_inbound(&self, inbound: IbInbound) -> IbInbound {
        let mut n = self.numbering.lock().unwrap();
        let Numbering { map, synth_next, gw: _ } = &mut *n;
        resolve_numeric(map, synth_next, inbound)
    }

    /// Resolve `contract.con_id` via `/iserver/secdef/search` if absent, caching by symbol. Returns
    /// an owned contract with `con_id` filled when resolution succeeds; on any REST/decode failure
    /// (or an already-populated con_id), returns the contract unchanged so `order_json` still sends
    /// something (a `conid: 0` request the gateway will itself reject — never silently drop the
    /// submit).
    fn resolve_conid(&self, contract: &IbkrContract) -> IbkrContract {
        if contract.con_id.is_some() {
            return contract.clone();
        }
        if let Some(cached) = self.conid_cache.lock().unwrap().get(&contract.symbol).copied() {
            let mut c = contract.clone();
            c.con_id = Some(cached);
            return c;
        }
        match self.rest.secdef_search(&contract.symbol, contract.sec_type.as_ib_code()) {
            Ok(v) => match decode_conid(&v) {
                Some(conid) => {
                    self.conid_cache.lock().unwrap().insert(contract.symbol.clone(), conid);
                    let mut c = contract.clone();
                    c.con_id = Some(conid);
                    c
                }
                None => {
                    tracing::warn!(symbol = %contract.symbol, "cpapi secdef_search returned no conid");
                    contract.clone()
                }
            },
            Err(e) => {
                tracing::warn!(symbol = %contract.symbol, error = %e.msg, "cpapi secdef_search failed");
                contract.clone()
            }
        }
    }
}

impl IbkrTransport for CpapiTransport {
    fn place_order(&mut self, order_id: i32, spec: &IbOrderSpec, contract: &IbkrContract) {
        // Record coid→numeric so inbound (carrying cOID) recovers the numeric order_id.
        self.numbering.lock().unwrap().map.insert(spec.order_ref.clone(), order_id);
        let resolved = self.resolve_conid(contract);
        let body = order_json(spec, &resolved);
        let mut resp = match self.rest.place(&body) {
            Ok(v) => v,
            Err(e) => {
                self.reject(order_id, &e.msg);
                return;
            }
        };
        // Bounded reply-confirmation loop (see module doc's MAX_REPLY_HOPS). The live gateway
        // returns a CHAIN of confirm questions (price-constraint → no-market-data → …), each
        // answered reply yielding the next, until a terminal row with `order_id`/`order_status`.
        for _ in 0..MAX_REPLY_HOPS {
            match parse_place_response(&resp) {
                PlaceOutcome::Accepted { order_id: gw_id } => {
                    // Capture the gateway's own order id: cancel/modify must address the order by IT,
                    // not our allocated numeric (the gateway rejects the numeric as "doesn't exist").
                    self.numbering.lock().unwrap().gw.insert(order_id, gw_id);
                    // Emit acceptance directly: the place response's terminal row IS the accept
                    // (order_status PreSubmitted/Submitted). The WS 'sor' stream may also deliver a
                    // status; a duplicate accept is a no-op transition in the exec FSM. order_id 0 +
                    // coid → numbered to our numeric by `next_recv`, exactly like the WS path.
                    let _ =
                        self.tx.send(IbInbound::OrderStatus(crate::event_mapper::IbOrderStatus {
                            order_id: 0,
                            order_ref: spec.order_ref.clone(),
                            status: "PreSubmitted".to_string(),
                            filled: 0.0,
                            avg_fill_price: 0.0,
                        }));
                    return;
                }
                PlaceOutcome::Rejected { reason } => {
                    self.reject(order_id, &reason);
                    return;
                }
                PlaceOutcome::NeedsReply { reply_id } => {
                    resp = match self.rest.reply(&reply_id, true) {
                        Ok(v) => v,
                        Err(e) => {
                            self.reject(order_id, &e.msg);
                            return;
                        }
                    };
                }
            }
        }
        self.reject(order_id, "cpapi reply-confirmation exceeded MAX_REPLY_HOPS");
    }

    fn cancel_order(&mut self, order_id: i32) {
        // cpapi addresses the order by the GATEWAY's own id (captured at accept in `place_order`),
        // falling back to the numeric only if we never saw an accept — the gateway rejects an
        // unknown id, so no order silently vanishes. (Live-verified: cancelling by the numeric fails
        // with "OrderID N doesn't exist"; by the gateway id it succeeds.)
        let key = self.gateway_key(order_id);
        match self.rest.cancel(&key) {
            Ok(_) => {
                // The cpapi cancel REST returns a `{"msg":"Request was submitted","order_id":..}`
                // ack — the terminal event is NOT relied upon from the WS stream. Emit the Cancelled
                // status directly; `order_id` is already numeric (non-zero) so `resolve_numeric`
                // leaves it, and the shared IdRegistry resolves it to the coid.
                let _ = self.tx.send(IbInbound::OrderStatus(crate::event_mapper::IbOrderStatus {
                    order_id,
                    order_ref: String::new(),
                    status: "Cancelled".to_string(),
                    filled: 0.0,
                    avg_fill_price: 0.0,
                }));
            }
            Err(e) => tracing::warn!(order_id, key = %key, error = %e.msg, "cpapi cancel failed"),
        }
    }

    fn modify_order(&mut self, order_id: i32, spec: &IbOrderSpec, contract: &IbkrContract) {
        let resolved = self.resolve_conid(contract);
        let body = order_json(spec, &resolved);
        let key = self.gateway_key(order_id);
        if let Err(e) = self.rest.amend(&key, &body) {
            tracing::warn!(order_id, key = %key, error = %e.msg, "cpapi amend failed");
        }
    }

    fn request_open_orders(&mut self) {
        if let Ok(v) = self.rest.open_orders() {
            for inbound in decode_open_orders(&v) {
                let _ = self.tx.send(self.number_inbound(inbound));
            }
        }
        if let Ok(v) = self.rest.executions() {
            for inbound in decode_executions(&v) {
                let _ = self.tx.send(self.number_inbound(inbound));
            }
        }
    }

    fn next_recv(&mut self, timeout: Duration) -> Option<IbInbound> {
        match self.inbound.recv_timeout(timeout) {
            Ok(inbound) => Some(self.number_inbound(inbound)),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                thread::sleep(timeout);
                None
            }
        }
    }
}

impl CpapiTransport {
    /// Synthesize a resolvable `OrderRejected` (no order vanishes). Passes the REAL `order_id` — the
    /// exec loop bound coid→order_id BEFORE `place_order`, so `EventMapper::on_error` resolves it
    /// deterministically via `IdRegistry` (code 201 = order-rejection class). The id-less path
    /// (`order_id: 0`) is NOT used: it only resolves when exactly one order is unacked and silently
    /// drops the reject otherwise — an order vanishing with a second order in flight.
    fn reject(&self, order_id: i32, msg: &str) {
        let _ =
            self.tx.send(IbInbound::Error { code: 201, order_id, msg: format!("cpapi: {msg}") });
    }

    /// The cancel/modify address for our numeric `order_id`: the gateway's own order id captured at
    /// accept, else the numeric as a string (best-effort fallback if no accept was seen).
    fn gateway_key(&self, order_id: i32) -> String {
        self.numbering
            .lock()
            .unwrap()
            .gw
            .get(&order_id)
            .cloned()
            .unwrap_or_else(|| order_id.to_string())
    }
}

impl Drop for CpapiTransport {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The WS pump: connect to the gateway ws, subscribe to the 'sor' order-update topic, translate
/// frames → IbInbound; tickle every 30s to keep the session alive; on socket close emit
/// `StreamResync` and reconnect.
fn pump_loop(
    ws_url: &str,
    rest_url: &str,
    account: &str,
    tx: Sender<IbInbound>,
    stop: Arc<AtomicBool>,
) {
    use vike_bridge_core::user_data::UserStream;
    use vike_bridge_core::ws::{TungsteniteStream, configure_ws_stream};
    let keepalive = CpapiRest::new(rest_url, account);
    let mut last_tickle = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        let (mut socket, _resp) = match connect_ws(ws_url) {
            Ok(ok) => ok,
            Err(e) => {
                tracing::warn!(error = %e, "cpapi ws connect failed; retrying");
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        configure_ws_stream(&socket, Duration::from_millis(200));
        // Subscribe to order updates.
        //
        // ⚠ MEASURED 2026-08-23 against a live gateway: THIS FRAME ANSWERS NOTHING. It is left in
        // place rather than deleted because it is the documented spelling and costs nothing, but
        // nothing arrives on it — see this module's doc for the measurement, the sibling topics that
        // DO answer on the same socket, and why cpapi fills are consequently invisible. Do not read
        // a green smoke as evidence this works: the smoke's accept and cancel are both synthesized
        // from the REST acks and never touch this stream.
        let _ = socket.send(tungstenite::Message::Text("sor+{}".to_string().into()));
        let mut stream = TungsteniteStream(socket);
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            if last_tickle.elapsed() >= Duration::from_secs(30) {
                let _ = keepalive.tickle();
                last_tickle = Instant::now();
            }
            match stream.recv() {
                Ok(vike_bridge_core::user_data::StreamMsg::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                        for inbound in decode_ws_frame(&v) {
                            if tx.send(inbound).is_err() {
                                return;
                            }
                        }
                    }
                }
                Ok(vike_bridge_core::user_data::StreamMsg::Ping(p)) => {
                    let _ = stream.pong(p);
                }
                Ok(_) => {}
                Err(vike_bridge_core::user_data::StreamError::Timeout) => {}
                Err(vike_bridge_core::user_data::StreamError::Closed(_)) => {
                    // Socket dropped → RESYNC, not death: breaking this inner loop returns to the
                    // `while !stop` re-dial above, so this backend genuinely reconnects. The resync
                    // requests the exec loop answers with go out over REST, a connection the WS drop
                    // did not touch, so they work even before the new socket is up. This is the
                    // variant the socket backend must NOT use — see `IbInbound::StreamDead`.
                    let _ = tx.send(IbInbound::StreamResync);
                    break;
                }
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// `https://127.0.0.1:5000` → `wss://127.0.0.1:5000/v1/api/ws`. LIVE-VERIFIED 2026-07-15 against a
/// paper CP Gateway.
fn ws_url_from(cpapi_url: &str) -> String {
    let base = cpapi_url
        .trim_end_matches('/')
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    format!("{base}/v1/api/ws")
}

type WsConn = (
    tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    tungstenite::handshake::client::Response,
);

/// Connect the cpapi WS. The Client Portal Gateway is LOOPBACK-only and serves a self-signed (often
/// expired) cert, so — mirroring the REST `blocking_agent_loopback_insecure` path — a loopback URL
/// gets a rustls connector whose verifier accepts any cert. `is_loopback_url` gates it: a
/// non-loopback ws URL always uses tungstenite's default full-verification path. (Live-verified
/// 2026-07-15: plain `tungstenite::connect` rejected the gateway's expired cert, so no order-update
/// frames ever arrived.)
fn connect_ws(ws_url: &str) -> Result<WsConn, String> {
    use vike_bridge_core::http::is_loopback_url;
    if !is_loopback_url(ws_url) {
        return tungstenite::connect(ws_url).map_err(|e| e.to_string());
    }
    // authority = "127.0.0.1:5000" — strip scheme + path for the TcpStream connect.
    let authority =
        ws_url.split("://").nth(1).unwrap_or(ws_url).split('/').next().unwrap_or(ws_url);
    let stream = std::net::TcpStream::connect(authority).map_err(|e| e.to_string())?;
    let connector = tungstenite::Connector::Rustls(Arc::new(insecure_loopback_tls()));
    tungstenite::client_tls_with_config(ws_url, stream, None, Some(connector))
        .map_err(|e| e.to_string())
}

/// A rustls `ClientConfig` (ring provider) that accepts ANY server cert. ONLY ever handed to a
/// loopback connection (gated by `is_loopback_url` in `connect_ws`) — the Client Portal Gateway's
/// self-signed loopback cert. NEVER use this for a non-loopback host.
fn insecure_loopback_tls() -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth()
}

/// Certificate verifier that trusts everything. Sound ONLY behind the loopback gate in `connect_ws`.
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Substitute the numeric order_id into a decoder-produced inbound (which carries `order_id: 0` +
/// `order_ref: <cOID>`). Own-session coids resolve from `map` (populated at `place_order`); a coid
/// with no entry (a foreign/prior-session order surfaced by reconnect resync) is assigned a fresh
/// unique negative synthetic id and remembered, so distinct foreign orders never collide on id 0 in
/// the shared `IdRegistry` (and the same coid always resolves to the same id).
fn resolve_numeric(
    map: &mut HashMap<String, i32>,
    synth_next: &mut i32,
    mut inbound: IbInbound,
) -> IbInbound {
    fn fill(
        map: &mut HashMap<String, i32>,
        synth_next: &mut i32,
        order_id: &mut i32,
        order_ref: &str,
    ) {
        if *order_id != 0 || order_ref.is_empty() {
            return; // already numbered, or no coid to key on
        }
        let id = *map.entry(order_ref.to_string()).or_insert_with(|| {
            let s = *synth_next;
            *synth_next -= 1;
            s
        });
        *order_id = id;
    }
    match &mut inbound {
        IbInbound::OrderStatus(st) => {
            let r = st.order_ref.clone();
            fill(map, synth_next, &mut st.order_id, &r);
        }
        IbInbound::ExecDetails(e) => {
            let r = e.order_ref.clone();
            fill(map, synth_next, &mut e.order_id, &r);
        }
        IbInbound::OpenOrder { order_id, order_ref } => {
            let r = order_ref.clone();
            fill(map, synth_next, order_id, &r);
        }
        _ => {}
    }
    inbound
}

/// Build the CP Gateway order JSON from the pure spec + contract. `conId` is required by the
/// gateway; `place_order`/`modify_order` resolve it first via `resolve_conid` when absent.
/// Field names LIVE-VERIFIED 2026-08-23: a raw `POST` carrying exactly this body
/// (`conid`/`orderType`/`side`/`tif`/`quantity`/`price`/`cOID`) was accepted by the live gateway and
/// came back as `Buy 1 AAPL Limit 1.00, GTC` with `cOID` echoed as `local_order_id`. See the
/// module doc table in `transport/cpapi/endpoints.rs`.
fn order_json(spec: &IbOrderSpec, contract: &IbkrContract) -> serde_json::Value {
    json!({
        "conid": contract.con_id.unwrap_or(0),
        "orderType": spec.order_type,
        "side": spec.action,
        "tif": spec.tif,
        "quantity": spec.total_qty,
        "price": spec.lmt_price,
        "cOID": spec.order_ref,
    })
}

#[cfg(test)]
mod tests {
    use super::{IbInbound, resolve_numeric};
    use crate::event_mapper::{IbExecDetails, IbOrderStatus};
    use std::collections::HashMap;

    fn status(order_ref: &str) -> IbInbound {
        IbInbound::OrderStatus(IbOrderStatus {
            order_id: 0,
            order_ref: order_ref.into(),
            status: "Submitted".into(),
            filled: 0.0,
            avg_fill_price: 0.0,
        })
    }

    fn order_id_of(inbound: &IbInbound) -> i32 {
        match inbound {
            IbInbound::OrderStatus(s) => s.order_id,
            IbInbound::ExecDetails(e) => e.order_id,
            IbInbound::OpenOrder { order_id, .. } => *order_id,
            _ => panic!("no order_id"),
        }
    }

    #[test]
    fn own_session_coid_resolves_to_allocated_id() {
        let mut map = HashMap::from([("coid-A".to_string(), 101)]);
        let mut synth = -1;
        let out = resolve_numeric(&mut map, &mut synth, status("coid-A"));
        assert_eq!(order_id_of(&out), 101);
        assert_eq!(synth, -1, "no synthetic id allocated for a known coid");
    }

    #[test]
    fn distinct_foreign_coids_get_distinct_stable_synthetic_ids() {
        let mut map = HashMap::new();
        let mut synth = -1;
        let a = order_id_of(&resolve_numeric(&mut map, &mut synth, status("foreign-1")));
        let b = order_id_of(&resolve_numeric(&mut map, &mut synth, status("foreign-2")));
        assert_ne!(a, b, "distinct foreign coids must not collide (the reviewer's finding #2)");
        assert!(a < 0 && b < 0, "synthetic ids are negative (never collide with 101+ allocations)");
        // The same coid resolves to the SAME id on a later inbound.
        let a2 = order_id_of(&resolve_numeric(&mut map, &mut synth, status("foreign-1")));
        assert_eq!(a, a2, "a coid's synthetic id is stable across inbound");
    }

    #[test]
    fn exec_details_numeric_is_filled_too() {
        let mut map = HashMap::from([("coid-X".to_string(), 202)]);
        let mut synth = -1;
        let e = IbInbound::ExecDetails(IbExecDetails {
            order_id: 0,
            order_ref: "coid-X".into(),
            exec_id: "e1".into(),
            symbol: "S".into(),
            side_buy: true,
            shares: 1.0,
            price: 1.0,
            ts: 0,
        });
        assert_eq!(order_id_of(&resolve_numeric(&mut map, &mut synth, e)), 202);
    }
}
