//! The listenKey user-data pump both family venues run — mint a key over REST, connect the raw
//! `<websocket_base>[/ws]/<listenKey>` socket, hand it to bridge-core's venue-neutral pump. Shared by
//! vike-binance (`crate::execution::user_data::perp`) and vike-aster (`vike_aster::{user_data,
//! perp_user_data}`), which had THREE copies of this one shape (F13).
//!
//! The shape, identical in all three: POST the listenKey endpoint → the 64-char key → connect
//! `wss://<ws_base>[/ws]/<key>` (**NO subscribe frame, NO ack handshake** — the key in the URL IS
//! the auth; do NOT copy Binance spot's WS-API ack loop here, the stream never acks and it would
//! hang) → [`vike_bridge_core::websocket_session::user::spawn_user_with_resync`], the one spawn of every private WS stream. A PUT
//! keepalive fires on a ~30-min cadence (60-min server expiry) via the pump's keepalive hook, BOUNDED ([`KEEPALIVE_TIMEOUT`]). The key is NEVER DELETEd
//! on teardown (server-side expiry handles it). A venue that drops a socket it hears nothing on also
//! says so in [`WebsocketSessionUserSpecBinance::socket_keepalive`], and the same hook keeps that socket talking.
//!
//! **A key that lapses does not end the socket.** It stays open and delivers nothing, pongs keep the
//! idle watchdog quiet, and only a `listenKeyExpired` frame says so. No reconnect means no resync, so
//! fills and venue cancels would be lost with the status reading open. So a failed PUT warns and is
//! retried at each of the hook's wakes ([`RENEW_RETRY_EVERY`]) with the socket left up, and
//! [`WebsocketSessionUserStreamKeyed`] ends the session only on that frame or when the key has gone [`KEY_AT_RISK_AFTER`]
//! unrenewed. The re-open mints a key and the resync replays the hole.
//!
//! **The per-venue delta is [`ListenKeyAuthentication`] — how the listenKey REST call is authed.** Binance
//! sends the apiKey in an `X-MBX-APIKEY` header with the HMAC signer BYPASSED (no timestamp, no
//! signature); Aster's v3 endpoint is instead EIP-712-SIGNED, the auth riding in the query. That
//! ONE divergence is the whole reason these were three files. Everything else here — the bounded
//! request, the create/keepalive pair, the connect, the thread wiring, the resync supervisor — is
//! venue-neutral and lives once.
//!
//! **A SECOND, smaller delta is real and stays parameterized, not unified:** the WS path segment.
//! Binance perp connects `<ws_base>/<key>`; both Aster streams connect `<ws_base>/ws/<key>`. It is
//! passed as `websocket_path` (`""` / `"/ws"`) — see [`user_data_websocket_url`], whose tests pin both venues'
//! exact effective URLs.
//!
//! **Hosts and endpoint paths stay OUT of this module**, exactly as in rungs 2–3: each venue passes
//! its own `rest_url`/`websocket_base_url`/`listenkey_path`, so each venue's hosts stay the ones its
//! callers looked up in its `venue_url` rows (binance's `crate::execution`'s `BinanceUrls`, Aster's own
//! `urls::AsterUrls`). Nothing here decides which network it is on, and nothing here became env-resolved.
//!
//! **Why family and not vike-bridge-core:** listenKey is Binance *wire grammar*, not a neutral
//! transport primitive — all three copies are family venues. Keeping it here preserves
//! bridge-core's venue-neutral charter; the genuinely neutral half (the reconnect/backoff/keepalive
//! reliability loop, the threads, the span, the fence) already lives there as
//! [`vike_bridge_core::websocket_session::user::spawn_user_with_resync`] and is what this module drives: it hands over the session's open
//! and its keepalive hook.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::Message;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::websocket::WebsocketSessionUserStreamTungstenite;
use vike_bridge_core::websocket_session::spec::CONNECT_10S;
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserError, WebsocketSessionUserKeepalive, WebsocketSessionUserMessage,
    WebsocketSessionUserOpenOutcome, WebsocketSessionUserResyncHandle,
    WebsocketSessionUserResyncSpec, WebsocketSessionUserStream, dial_user_stream,
    spawn_user_with_resync,
};
use vike_execution::EventSender;
use vike_model::events::Event;

// The pump's cadences are not constants of this file. The listenKey PUT cadence is a row of
// `vike_venues::venue_timings` (`session_renew_every`, read in `spawn_user_data_with_resync`); the recv
// poll, the reconnect ceiling and the post-reconnect settle have no venue reason and are
// `vike_bridge_core::websocket_session::user`'s `USER_DATA_POLL`, `USER_DATA_MAX_BACKOFF` and `RESYNC_SETTLE`.
// The retry of a failed PUT is this pump's own, below.

/// Keepalive PUT bound — never blocks teardown.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(2);
/// Create POST bound (looser than the keepalive's — a failed create costs a reconnect).
const CREATE_TIMEOUT: Duration = Duration::from_secs(5);
/// ~30 min, comfortably inside the venues' 60-min key expiry. ONE pump serves binance perp and both
/// aster markets, so this is binance perp's row and the aster rows are held equal to it by
/// `listenkey_tests::the_renewal_cadence_is_one_fact_for_the_family`.
const RENEW_EVERY: Duration = vike_venues::venue_timings::BINANCE_PERP.exec().session_renew_every();
/// How often the hook wakes on a venue with no socket keepalive, so a failed PUT is retried long before
/// the key's expiry instead of 30 minutes later, on it. The pump's own, no venue's reason. The wake
/// itself sends nothing.
const RENEW_RETRY_EVERY: Duration = Duration::from_secs(60);
/// How long since the session's POST or its last PUT that worked a key may go unrenewed before the
/// session ends: the venues expire a key at 60 minutes, and this leaves the re-open and its resync
/// five of them. Not sooner: until then the socket works, and in a REST outage the re-open's POST
/// would fail too, so closing early only stops the fills.
const KEY_AT_RISK_AFTER: Duration = Duration::from_secs(55 * 60);

/// How one family venue authenticates its listenKey REST calls — **the SOLE divergence** between
/// the venues' otherwise identical pumps (see the module doc).
///
/// Deliberately a trait and not an enum: this module lives in vike-binance, which cannot name
/// Aster's signer (the dependency runs vike-aster → vike-binance, never back). Each venue
/// implements it on its own tiny struct and keeps ONLY its auth here.
///
/// Built ONCE per pump and shared (`Arc`) by both the on-connect create and the periodic keepalive,
/// which is what lets Aster's non-`Clone` signer serve both without being cloned.
pub trait ListenKeyAuthentication {
    /// The query string to hang off the listenKey URL, or `""` for none. Aster returns its signed
    /// `nonce`/`user`/`signer`/`signature`; Binance returns empty (its auth is a header).
    ///
    /// Takes `method` because a signature covers it — the create is a POST, the keepalive a PUT.
    fn query(&self, method: &str) -> String;

    /// An auth header to set, or `None`. Binance returns its `X-MBX-APIKEY`; Aster returns `None`
    /// (v3 auth rides in the signed query — sending the header would be wrong, not merely
    /// redundant).
    fn header(&self) -> Option<(&'static str, String)>;
}

/// Everything the shared pump needs about the venue+stream it is serving. Owned `String` hosts
/// because a pump thread outlives its caller.
///
/// Deliberately carries resolved VALUES, not a resolver: whoever builds it has already decided
/// which network it names (rung-2's [`UrlTable`](crate::family::UrlTable) discipline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsocketSessionUserSpecBinance {
    /// Canonical venue key, stamped as a span field (`"binance"` / `"aster"`).
    pub venue: &'static str,
    /// The threads' prefix: the pump is `"{lane}-userdata-{symbol}"` and the resync supervisor
    /// `"{lane}-resync-{symbol}"` (e.g. `"binance-perp"`).
    pub lane: &'static str,
    /// REST origin the listenKey is minted against, bare (e.g. `https://fapi.binance.com`).
    pub rest_url: String,
    /// User-data WS host, bare — no trailing `/ws` (see `websocket_path`).
    pub websocket_base_url: String,
    /// WS path segment before the key: `""` (Binance perp) or `"/ws"` (both Aster streams). A real
    /// venue divergence, NOT unified — see [`user_data_websocket_url`].
    pub websocket_path: &'static str,
    /// The listenKey endpoint, leading slash (`/fapi/v1/listenKey`, `/fapi/v3/listenKey`,
    /// `/api/v3/listenKey` — all three differ).
    pub listenkey_path: &'static str,
    /// Silent-stall watchdog window for this venue's socket, or `None` to disable it.
    ///
    /// **Per-venue on purpose.** This pump is shared by Binance spot, Binance perp AND Aster, but
    /// the threshold is NOT a shared constant: on this family there is no app-level ping (the
    /// keepalive is a REST listenKey PUT that never touches the socket), so the only inbound
    /// traffic on an idle account is the venue's OWN server ping — and that cadence is a property
    /// of the venue, not of the pump. A value measured on one venue must never be assumed for a
    /// sibling. See each `spec()` for what its value is based on.
    pub idle_threshold: Option<Duration>,
    /// How often this venue's socket must hear from us, or `None`: it needs nothing but its answers
    /// to the server's pings (binance perp). **Per-venue on purpose**, for the reason above: this is
    /// a venue's own timer, and a value measured on one says nothing of a sibling.
    pub socket_keepalive: Option<Duration>,
}

/// The listenKey REST URL. `query` empty ⇒ no `?` at all, which is what keeps Binance's effective
/// URL byte-identical to the pre-family `format!("{rest}{path}")`.
pub fn listenkey_url(rest_url: &str, listenkey_path: &str, query: &str) -> String {
    // `listenkey_path` already starts with `/`, so no separator between it and `rest_url`.
    if query.is_empty() {
        format!("{rest_url}{listenkey_path}")
    } else {
        format!("{rest_url}{listenkey_path}?{query}")
    }
}

/// The user-data WS URL. `websocket_path` is the venue divergence: `""` ⇒ `<base>/<key>` (Binance perp),
/// `"/ws"` ⇒ `<base>/ws/<key>` (Aster spot + perp).
pub fn user_data_websocket_url(
    websocket_base_url: &str,
    websocket_path: &str,
    listen_key: &str,
) -> String {
    format!("{websocket_base_url}{websocket_path}/{listen_key}")
}

/// One authed listenKey call. Small dedicated agent so the keepalive's 2s bound is independent of
/// the shared transport's 30s default.
fn listenkey_request(
    rest_url: &str,
    listenkey_path: &str,
    auth: &dyn ListenKeyAuthentication,
    method: &str,
    timeout: Duration,
) -> Result<Value, VenueApiError> {
    let url = listenkey_url(rest_url, listenkey_path, &auth.query(method));
    let agent = vike_bridge_core::http::blocking_agent_with_timeout(timeout);
    let req = match method {
        "POST" => agent.post(&url),
        "PUT" => agent.put(&url),
        other => return Err(VenueApiError { code: 0, msg: format!("unsupported method {other}") }),
    };
    // The ONE auth divergence, applied: a header for Binance, nothing for Aster (signed query).
    let req = match auth.header() {
        Some((name, value)) => req.header(name, value.as_str()),
        None => req,
    };
    let mut response = req
        .send_empty()
        .map_err(|e| VenueApiError { code: 0, msg: format!("network error: {e}") })?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(VenueApiError { code: i64::from(status), msg: text });
    }
    if text.is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&text)
        .map_err(|e| VenueApiError { code: 0, msg: format!("bad json: {e}") })
}

/// POST the listenKey endpoint → the listenKey string.
pub fn listenkey_create(
    rest_url: &str,
    listenkey_path: &str,
    auth: &dyn ListenKeyAuthentication,
) -> Result<String, VenueApiError> {
    let body = listenkey_request(rest_url, listenkey_path, auth, "POST", CREATE_TIMEOUT)?;
    Ok(body.get("listenKey").and_then(|k| k.as_str()).unwrap_or("").to_string())
}

/// The PUT keepalive. What a failure means is the pump's to decide: the module doc's retry and re-open.
pub fn listenkey_keepalive(
    rest_url: &str,
    listenkey_path: &str,
    auth: &dyn ListenKeyAuthentication,
) -> Result<(), VenueApiError> {
    listenkey_request(rest_url, listenkey_path, auth, "PUT", KEEPALIVE_TIMEOUT).map(drop)
}

/// One session of the pump: the socket, the key it reads by and that key's renewal (the module doc).
/// Holds the key, which reads the private stream for up to an hour, so it has no `Debug`.
pub struct WebsocketSessionUserStreamKeyed {
    socket: WebsocketSessionUserStreamTungstenite,
    listen_key: String,
    /// The POST that opened this session, then each PUT that worked.
    renewed: Instant,
    /// The pump's hook found the key unrenewed for [`KEY_AT_RISK_AFTER`]: the next read ends the session.
    key_at_risk: bool,
}

impl WebsocketSessionUserStreamKeyed {
    /// Close the socket and say why, so the pump re-opens. Best effort: a socket that cannot take the
    /// close is one the re-open replaces anyway.
    fn end(&mut self, why: &str) -> WebsocketSessionUserError {
        let _ = self.socket.0.close(None);
        WebsocketSessionUserError::Closed(why.to_string())
    }
}

impl WebsocketSessionUserStream for WebsocketSessionUserStreamKeyed {
    fn recv(&mut self) -> Result<WebsocketSessionUserMessage, WebsocketSessionUserError> {
        if self.key_at_risk {
            return Err(self.end("the listenKey could not be renewed and is about to expire"));
        }
        match self.socket.recv() {
            // No parse: a false match costs one re-open, and no other frame says these words.
            Ok(WebsocketSessionUserMessage::Text(raw)) if raw.contains("listenKeyExpired") => {
                Err(self.end("the venue said the listenKey expired"))
            }
            // A server's close reason is the venue's own text.
            Err(WebsocketSessionUserError::Closed(why)) => {
                Err(WebsocketSessionUserError::Closed(why.replace(&self.listen_key, "<listenKey>")))
            }
            other => other,
        }
    }

    fn pong(&mut self, payload: Vec<u8>) -> Result<(), WebsocketSessionUserError> {
        self.socket.pong(payload)
    }
}

/// Create the listenKey then connect the user-data socket. No subscribe frame, no ack loop —
/// connected = subscribed.
pub fn open_user_data_websocket(
    rest_url: &str,
    listenkey_path: &str,
    websocket_base_url: &str,
    websocket_path: &str,
    auth: &dyn ListenKeyAuthentication,
    stop: &AtomicBool,
) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamKeyed> {
    if stop.load(Ordering::Relaxed) {
        return WebsocketSessionUserOpenOutcome::Stopped;
    }
    let listen_key = match listenkey_create(rest_url, listenkey_path, auth) {
        Ok(k) if !k.is_empty() => k,
        Ok(_) => return WebsocketSessionUserOpenOutcome::Transport("empty listenKey".into()),
        Err(e) => {
            return WebsocketSessionUserOpenOutcome::Transport(format!("listenKey create: {e}"));
        }
    };
    let url = user_data_websocket_url(websocket_base_url, websocket_path, &listen_key);
    let socket = match dial_user_stream(url.as_str(), CONNECT_10S) {
        Ok(socket) => socket,
        // This text goes on the observe frames, `trade health`, MCP and the log, and the URL is the key.
        Err(why) => {
            return WebsocketSessionUserOpenOutcome::Transport(
                why.replace(&listen_key, "<listenKey>"),
            );
        }
    };
    WebsocketSessionUserOpenOutcome::Ready(WebsocketSessionUserStreamKeyed {
        socket: WebsocketSessionUserStreamTungstenite(socket),
        listen_key,
        renewed: Instant::now(),
        key_at_risk: false,
    })
}

/// How often the pump's hook wakes: the venue's socket keepalive, else [`RENEW_RETRY_EVERY`].
fn hook_every(socket_keepalive: Option<Duration>, renew_every: Duration) -> Duration {
    socket_keepalive.unwrap_or(RENEW_RETRY_EVERY).min(renew_every)
}

/// The session's open, run by the shared pump once per (re)connect: [`open_user_data_websocket`] on `spec`'s hosts with
/// `auth`.
fn session_open<A: ListenKeyAuthentication + Send + Sync + 'static>(
    spec: &WebsocketSessionUserSpecBinance,
    auth: Arc<A>,
) -> impl FnMut(&AtomicBool) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamKeyed>
+ Send
+ 'static {
    let (rest_url, listenkey_path) = (spec.rest_url.clone(), spec.listenkey_path);
    let (websocket_base_url, websocket_path) =
        (spec.websocket_base_url.clone(), spec.websocket_path);
    move |stop: &AtomicBool| {
        open_user_data_websocket(
            &rest_url,
            listenkey_path,
            &websocket_base_url,
            websocket_path,
            &*auth,
            stop,
        )
    }
}

/// The session's keepalive hook and its cadence ([`hook_every`]), run by the shared pump while a session is up: on a
/// venue that asks for it ([`WebsocketSessionUserSpecBinance::socket_keepalive`]), a ping and a pong on the socket; and the listenKey PUT once
/// `renew_every` has passed since the key was last renewed, a failure warned and retried at the next wake until the key
/// has gone `key_at_risk_after` unrenewed (the module doc).
fn keepalive_hook<A: ListenKeyAuthentication + Send + Sync + 'static>(
    spec: &WebsocketSessionUserSpecBinance,
    auth: Arc<A>,
    renew_every: Duration,
    key_at_risk_after: Duration,
) -> WebsocketSessionUserKeepalive<WebsocketSessionUserStreamKeyed> {
    let (rest_url, listenkey_path) = (spec.rest_url.clone(), spec.listenkey_path);
    let socket_keepalive = spec.socket_keepalive;
    let hook = move |ws: &mut WebsocketSessionUserStreamKeyed| {
        if socket_keepalive.is_some() {
            // Both: a venue's timer may count any frame or only a pong, the journal cannot say
            // which, and finding out costs a deploy. A dead socket surfaces at the next read.
            let _ = ws.socket.0.send(Message::Ping(Vec::new().into()));
            let _ = ws.pong(Vec::new());
        }
        // The PUT never touches the socket. It runs on the key's clock, so a failure stays due.
        if ws.renewed.elapsed() >= renew_every {
            match listenkey_keepalive(&rest_url, listenkey_path, &*auth) {
                Ok(()) => ws.renewed = Instant::now(),
                Err(e) => {
                    ws.key_at_risk = ws.renewed.elapsed() >= key_at_risk_after;
                    tracing::warn!(
                        renewed_s_ago = ws.renewed.elapsed().as_secs(),
                        error = %e.to_string().replace(&ws.listen_key, "<listenKey>"),
                        "listenKey renewal failed; the socket stays up and this is retried at \
                         the next wake, until the key is about to expire"
                    );
                }
            }
        }
    };
    (hook_every(socket_keepalive, renew_every), Box::new(hook))
}

/// Spawn the persistent fill-stream pump feeding the th-core ingest (lossless), with the audit-A3
/// resync supervisor: `vike_bridge_core::websocket_session::user`'s `spawn_user_with_resync` over this session's open
/// and keepalive hook. On every WS re-open the pump bumps a generation counter, and the supervisor (after
/// its settle) replays recent order history via `resync_fetch` so a fill/cancel/expire/reject that landed
/// during the reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's
/// REST history source, on a SEPARATE REST client from the order-submit path; a caller with no replay
/// hands `Vec::new`. Both threads tear down on `shutdown`; the supervisor also self-exits if the pump dies
/// or the core is gone.
///
/// `on_reconcile` is threaded straight to [`vike_bridge_core::websocket_session::user::run_user_resync`]'s own param — `Some` fires the
/// recon trigger after every reconnect's event-replay settles; `None` (every non-recon caller)
/// reproduces the event-replay-only behavior.
///
/// `fence` is the one the exec thread's client runs its submits through: both producers here pass it
/// (`vike_bridge_core::exec_actor::submit_fence`'s module doc). `auth` is built once per pump and
/// shared by the create and the keepalive.
pub fn spawn_user_data_with_resync<A, M>(
    spec: WebsocketSessionUserSpecBinance,
    symbol: String,
    auth: A,
    decode: M,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
) -> WebsocketSessionUserResyncHandle
where
    A: ListenKeyAuthentication + Send + Sync + 'static,
    M: Fn(&Value) -> Vec<Event> + Send + 'static,
{
    let auth = Arc::new(auth);
    spawn_user_with_resync(WebsocketSessionUserResyncSpec {
        venue: spec.venue,
        lane: spec.lane,
        symbol,
        events,
        fence: Some(fence),
        resync_fetch,
        on_reconcile,
        open: session_open(&spec, Arc::clone(&auth)),
        decode,
        keepalive: Some(keepalive_hook(&spec, auth, RENEW_EVERY, KEY_AT_RISK_AFTER)),
        idle_threshold: spec.idle_threshold,
    })
}

/// **What a family pump owes the fence, for a pump `spawn` starts**: an order with no answer and an order whose fill is
/// pushed before its answer each reach the lane as `Submitted, Accepted, Fill, Filled`, the first through the replay
/// `spawn` is handed (one re-open), the second through the pump's own sink (a frame of `inflight`'s fill, which it sends
/// while `spawn`'s caller is inside that submit). Both ids are alphanumeric, the shape every id of ours has: a spot
/// fill under any other id is a hand trade and carries none (`super::order_map::spot_fill_order_client_id`).
#[cfg(test)]
pub(crate) fn assert_the_pump_and_its_replay_go_through_the_fence(
    spawn: impl FnOnce(
        EventSender,
        Arc<SubmitFence>,
        Box<dyn FnMut() -> Vec<Event> + Send>,
    ) -> WebsocketSessionUserResyncHandle,
) {
    use vike_model::events::{FillEvent, LiquiditySide, OrderAccepted, OrderSubmitted};

    let submitted = |order_client_id: &str| {
        Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.to_string(),
            timestamp: 1,
        })
    };
    let replay = Box::new(|| {
        let ts = vike_model::now_ms();
        let fill = FillEvent {
            trade_id: "t-replay".into(),
            order_client_id: "silent".to_string(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_price: 100.0,
            commission: 0.0,
            commission_asset: "".into(),
            liquidity_side: LiquiditySide::Taker,
            timestamp: ts,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        };
        vike_bridge_core::event_map::terminal_events("silent".to_string(), fill, ts, true)
    });
    let (events, mut lane) = vike_execution::event_channel(64);
    let fence = Arc::new(SubmitFence::new(events.clone()));
    fence.submit("silent", || vec![submitted("silent")]);
    let mut feed = None;
    fence.submit("inflight", || {
        feed = Some(spawn(events.clone(), Arc::clone(&fence), replay));
        let deadline = Instant::now() + Duration::from_secs(20);
        while fence.parked("inflight") < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        vec![
            submitted("inflight"),
            Event::OrderAccepted(OrderAccepted {
                order_client_id: "inflight".to_string(),
                venue_order_id: Some("v-1".to_string().into()),
                timestamp: 1,
            }),
        ]
    });

    let mut seen: Vec<(String, &str)> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !seen
        .iter()
        .any(|(order_client_id, kind)| order_client_id == "silent" && *kind == "Filled")
        && Instant::now() < deadline
    {
        while let Ok(ingest) = lane.try_recv() {
            let vike_execution::Ingest::Event(event) = ingest else { continue };
            seen.push(match event {
                Event::OrderSubmitted(e) => (e.order_client_id, "Submitted"),
                Event::OrderAccepted(e) => (e.order_client_id, "Accepted"),
                Event::Fill(e) => (e.order_client_id, "Fill"),
                Event::OrderFilled(e) => (e.order_client_id, "Filled"),
                other => panic!("{other:?}"),
            });
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = feed.expect("the pump").shutdown();
    for order_client_id in ["silent", "inflight"] {
        let kinds: Vec<&str> =
            seen.iter().filter(|(c, _)| c == order_client_id).map(|(_, kind)| *kind).collect();
        assert_eq!(
            kinds,
            ["Submitted", "Accepted", "Fill", "Filled"],
            "{order_client_id}: {seen:?}"
        );
    }
}

#[cfg(test)]
mod listenkey_tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use tungstenite::protocol::CloseFrame;
    use tungstenite::protocol::frame::coding::CloseCode;
    use vike_bridge_core::websocket_session::user::{
        RESYNC_SETTLE, USER_DATA_MAX_BACKOFF, USER_DATA_POLL, WebsocketSessionUserHandle, run_user,
    };
    use vike_log::capture::CapturedEvent;

    use super::*;

    /// The key the loopback venue mints, long enough that hiding it can touch no other word.
    const KEY: &str = "theListenKeyItselfReadsThePrivateStream";

    /// Binance's auth shape (apiKey header, no query) — the real one lives in
    /// `vike_binance::execution::user_data::perp`.
    struct HeaderOnly;
    impl ListenKeyAuthentication for HeaderOnly {
        fn query(&self, _method: &str) -> String {
            String::new()
        }
        fn header(&self) -> Option<(&'static str, String)> {
            Some(("X-MBX-APIKEY", "KEY".to_string()))
        }
    }

    /// Aster's auth shape (signed query, no header).
    struct SignedQuery;
    impl ListenKeyAuthentication for SignedQuery {
        fn query(&self, method: &str) -> String {
            format!("nonce=1&method={method}")
        }
        fn header(&self) -> Option<(&'static str, String)> {
            None
        }
    }

    #[test]
    fn binance_perp_effective_urls_are_unchanged() {
        // Pins the EXACT pre-family strings: `format!("{rest}{LISTENKEY_PATH}")` with no `?`, and
        // `format!("{ws_base}/{listen_key}")` with NO `/ws` segment.
        assert_eq!(
            listenkey_url(
                "https://fapi.binance.com",
                "/fapi/v1/listenKey",
                &HeaderOnly.query("POST")
            ),
            "https://fapi.binance.com/fapi/v1/listenKey"
        );
        assert_eq!(
            user_data_websocket_url("wss://fstream.binance.com", "", "LKEY"),
            "wss://fstream.binance.com/LKEY"
        );
    }

    #[test]
    fn aster_effective_urls_are_unchanged() {
        // Aster signs into the query AND carries the `/ws` segment Binance perp does not.
        assert_eq!(
            listenkey_url(
                "https://fapi.asterdex.com",
                "/fapi/v3/listenKey",
                &SignedQuery.query("POST")
            ),
            "https://fapi.asterdex.com/fapi/v3/listenKey?nonce=1&method=POST"
        );
        assert_eq!(
            listenkey_url(
                "https://sapi.asterdex.com",
                "/api/v3/listenKey",
                &SignedQuery.query("PUT")
            ),
            "https://sapi.asterdex.com/api/v3/listenKey?nonce=1&method=PUT"
        );
        assert_eq!(
            user_data_websocket_url("wss://fstream.asterdex.com", "/ws", "LKEY"),
            "wss://fstream.asterdex.com/ws/LKEY"
        );
    }

    #[test]
    fn auth_divergence_is_exclusive() {
        // The invariant that made three files one: a venue sets EITHER a header OR a query, never
        // both — Binance's header path must never gain a query, Aster's signed query must never
        // send an apiKey header.
        assert!(HeaderOnly.query("POST").is_empty(), "binance auth must not touch the query");
        assert!(HeaderOnly.header().is_some());
        assert!(SignedQuery.header().is_none(), "aster must not send X-MBX-APIKEY");
        assert!(!SignedQuery.query("POST").is_empty());
    }

    #[test]
    fn keepalive_signs_put_not_post() {
        // The keepalive is a PUT; a signer that covered only POST would silently 401 every 30 min.
        assert_ne!(SignedQuery.query("PUT"), SignedQuery.query("POST"));
    }

    /// A loopback venue's script, and what it heard from one pump.
    #[derive(Default)]
    struct Heard {
        posts: AtomicUsize,
        puts: AtomicUsize,
        pings: AtomicUsize,
        pongs: AtomicUsize,
        /// When each PUT arrived.
        put_times: Mutex<Vec<Instant>>,
        /// The venue refuses this many PUTs, in words that echo the key.
        refuse_puts: AtomicUsize,
        /// The venue pushes one `listenKeyExpired` frame.
        say_expired: AtomicBool,
        /// The venue closes the socket with the key in the reason.
        close_saying_key: AtomicBool,
        /// The re-opens the pump told its resync hook about.
        reopens: AtomicUsize,
        /// Every line the pump logged.
        logged: Mutex<Vec<CapturedEvent>>,
    }

    /// One loopback connection: a listenKey call is counted and answered, a socket's control frames
    /// are counted, and a frame is pushed every few ms so the pump's 1 s read never holds its hook up.
    fn serve(mut stream: std::net::TcpStream, heard: &Heard) {
        let mut head = [0u8; 4];
        loop {
            match stream.peek(&mut head) {
                Ok(4) => break,
                Ok(0) | Err(_) => return,
                Ok(_) => std::thread::yield_now(),
            }
        }
        if &head == b"GET " {
            let mut ws = tungstenite::accept(stream).expect("the upgrade");
            ws.get_ref().set_read_timeout(Some(Duration::from_millis(3))).unwrap();
            loop {
                match ws.read() {
                    Ok(Message::Ping(_)) => heard.pings.fetch_add(1, Ordering::Relaxed),
                    Ok(Message::Pong(_)) => heard.pongs.fetch_add(1, Ordering::Relaxed),
                    Ok(_) => 0,
                    Err(e) if vike_bridge_core::websocket::is_timeout(&e) => {
                        if heard.close_saying_key.load(Ordering::Relaxed) {
                            let reason = CloseFrame { code: CloseCode::Normal, reason: KEY.into() };
                            let _ = ws.close(Some(reason));
                        }
                        let frame = if heard.say_expired.swap(false, Ordering::Relaxed) {
                            r#"{"e":"listenKeyExpired","E":1}"#
                        } else {
                            "{}"
                        };
                        if ws.send(Message::Text(frame.into())).is_err() {
                            return;
                        }
                        0
                    }
                    Err(_) => return,
                };
            }
        }
        let mut refused = false;
        if &head == b"POST" {
            heard.posts.fetch_add(1, Ordering::Relaxed);
        } else {
            heard.put_times.lock().unwrap().push(Instant::now());
            heard.puts.fetch_add(1, Ordering::Relaxed);
            refused = heard
                .refuse_puts
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(1))
                .is_ok();
        }
        let mut request = [0u8; 4096];
        let _ = std::io::Read::read(&mut stream, &mut request);
        let (status, body) = if refused {
            ("400 Bad Request", format!(r#"{{"code":-1125,"msg":"listenKey {KEY} is unknown"}}"#))
        } else {
            ("200 OK", format!(r#"{{"listenKey":"{KEY}"}}"#))
        };
        let answer = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = std::io::Write::write_all(&mut stream, answer.as_bytes());
    }

    /// A venue on a loopback port, following `heard`'s script.
    fn loopback_venue(heard: &Arc<Heard>) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = Arc::clone(heard);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let heard = Arc::clone(&served);
                std::thread::spawn(move || serve(stream, &heard));
            }
        });
        addr
    }

    /// Run the real pump body against a loopback venue playing `script`, until `enough`.
    fn pump_against_loopback(
        script: Heard,
        socket_keepalive: Option<Duration>,
        renew_every: Duration,
        enough: impl Fn(&Heard) -> bool,
    ) -> Arc<Heard> {
        let heard = Arc::new(script);
        let addr = loopback_venue(&heard);
        let spec = WebsocketSessionUserSpecBinance {
            venue: "loopback",
            lane: "t",
            rest_url: format!("http://{addr}"),
            websocket_base_url: format!("ws://{addr}"),
            websocket_path: "/ws",
            listenkey_path: "/listenKey",
            idle_threshold: None,
            socket_keepalive,
        };
        let (events, _lane) = vike_execution::event_channel(1);
        // The production session's two steps, on the shared loop the pump runs them in.
        let auth = Arc::new(HeaderOnly);
        let mut open = session_open(&spec, Arc::clone(&auth));
        // Many failed PUTs after the first, as in production, and inside a test's patience.
        let keepalive = keepalive_hook(&spec, auth, renew_every, renew_every * 10);
        let pump_side = Arc::clone(&heard);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_pump = Arc::clone(&stop);
        // The capture is a thread's, so the loop runs on a thread of this test's own.
        let handle = std::thread::spawn(move || {
            let hook_side = Arc::clone(&pump_side);
            let on_reconnect = move || {
                hook_side.reopens.fetch_add(1, Ordering::Relaxed);
            };
            let (out, logged) = vike_log::capture::captured(|| {
                run_user(
                    || open(&stop_pump),
                    |_: &Value| Vec::new(),
                    move |event| events.blocking_send(event).is_ok(),
                    &stop_pump,
                    USER_DATA_POLL,
                    USER_DATA_MAX_BACKOFF,
                    Some(keepalive),
                    on_reconnect,
                    None,
                    None,
                )
            });
            *pump_side.logged.lock().unwrap() = logged;
            out
        });
        let feed = WebsocketSessionUserHandle { stop, handle };
        let deadline = Instant::now() + Duration::from_secs(20);
        while !enough(&heard) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        feed.shutdown().expect("no refusal");
        heard
    }

    /// **One outage can take an order's POST answer, its re-query, its `NEW` frame and the sentinel's read**, and then the
    /// replayed wrap was refused on `Submitted`: the order read REJECTED and a bracket's stop never went out. The real pump
    /// of binance perp and aster, on a loopback venue. Reddens on either producer sending straight to the lane.
    #[test]
    fn the_family_pump_and_its_replay_go_through_the_fence() {
        let heard = Arc::new(Heard { say_expired: AtomicBool::new(true), ..Heard::default() });
        let addr = loopback_venue(&heard);
        let spec = WebsocketSessionUserSpecBinance {
            venue: "loopback",
            lane: "t",
            rest_url: format!("http://{addr}"),
            websocket_base_url: format!("ws://{addr}"),
            websocket_path: "/ws",
            listenkey_path: "/listenKey",
            idle_threshold: None,
            socket_keepalive: None,
        };
        let once = AtomicBool::new(true);
        let frame = serde_json::json!({
            "e": "executionReport", "s": "BTCUSDT", "c": "inflight", "T": 1, "x": "TRADE", "X": "FILLED",
            "S": "BUY", "t": 1, "l": "1", "L": "100", "n": "0", "N": "USDT", "m": false
        });
        assert_the_pump_and_its_replay_go_through_the_fence(|events, fence, replay| {
            spawn_user_data_with_resync(
                spec,
                "BTCUSDT".to_string(),
                HeaderOnly,
                move |_: &Value| {
                    if once.swap(false, Ordering::Relaxed) {
                        crate::execution::event_mapper::spot::map_binance_private(
                            &frame, "binance", "BTCUSDT", None,
                        )
                    } else {
                        Vec::new()
                    }
                },
                events,
                fence,
                replay,
                None,
            )
        });
    }

    /// **the CI box, 2026-11-07: aster's mainnet reset a socket that had sent it nothing for 240 s, 64 times
    /// in 6 hours.** A venue that says so has its socket talk on that cadence, a ping and a pong, while
    /// the key's PUT keeps its own much longer one, on the one socket with no reconnect.
    #[test]
    fn a_venue_that_asks_for_it_hears_from_the_socket_and_the_key_keeps_its_own_cadence() {
        let every = Duration::from_millis(20);
        let heard =
            pump_against_loopback(Heard::default(), Some(every), Duration::from_millis(120), |h| {
                h.puts.load(Ordering::Relaxed) >= 2
                    && h.pings.load(Ordering::Relaxed) >= 6
                    && h.pongs.load(Ordering::Relaxed) >= 6
            });
        let (puts, pings) =
            (heard.puts.load(Ordering::Relaxed), heard.pings.load(Ordering::Relaxed));
        assert!(puts >= 2 && pings >= 6, "{puts} PUTs, {pings} pings");
        assert!(heard.pongs.load(Ordering::Relaxed) >= 6);
        assert!(pings > puts, "the socket's cadence is not the key's: {pings} pings, {puts} PUTs");
        assert_eq!(heard.posts.load(Ordering::Relaxed), 1, "one key, one socket, no reconnect");
    }

    /// Binance's wire is as it was while renewals work: no frame of ours on the socket, the PUT on
    /// its cadence, one key, one socket.
    #[test]
    fn a_venue_that_does_not_ask_for_it_sends_the_socket_nothing() {
        let heard =
            pump_against_loopback(Heard::default(), None, Duration::from_millis(120), |h| {
                h.puts.load(Ordering::Relaxed) >= 2
            });
        assert!(heard.puts.load(Ordering::Relaxed) >= 2, "the key is renewed");
        assert_eq!(heard.pings.load(Ordering::Relaxed), 0);
        assert_eq!(heard.pongs.load(Ordering::Relaxed), 0);
        assert_eq!(heard.posts.load(Ordering::Relaxed), 1);
        assert_eq!(heard.reopens.load(Ordering::Relaxed), 0);
    }

    /// **A key that lapses leaves a socket that stays open and delivers nothing**, pongs keeping it
    /// looking alive, so a key the venue will not renew ends the session while it still has time:
    /// the re-open mints a key and the resync replays the hole. Only then, and many failures on:
    /// until that point the socket is working. Both kinds of family venue.
    #[test]
    fn a_key_that_cannot_be_renewed_ends_the_session_only_when_it_is_at_risk() {
        for socket_keepalive in [None, Some(Duration::from_millis(20))] {
            let heard = pump_against_loopback(
                Heard { refuse_puts: AtomicUsize::new(usize::MAX), ..Heard::default() },
                socket_keepalive,
                Duration::from_millis(50),
                |h| h.reopens.load(Ordering::Relaxed) >= 1,
            );
            assert!(heard.reopens.load(Ordering::Relaxed) >= 1, "{socket_keepalive:?}");
            assert!(heard.posts.load(Ordering::Relaxed) >= 2, "{socket_keepalive:?}");
            let puts = heard.puts.load(Ordering::Relaxed);
            assert!(puts >= 3, "{socket_keepalive:?}: closed after {puts} failed PUTs");
        }
    }

    /// The words a key at risk ends its session in: the engine's line says them, and the account's link history keeps
    /// them as the session's end.
    #[test]
    fn a_key_at_risk_ends_the_session_in_its_own_words() {
        let heard = pump_against_loopback(
            Heard { refuse_puts: AtomicUsize::new(usize::MAX), ..Heard::default() },
            None,
            Duration::from_millis(50),
            |h| h.reopens.load(Ordering::Relaxed) >= 1,
        );
        let logged = heard.logged.lock().unwrap();
        let ended = logged
            .iter()
            .find(|e| e.message == "user-data WS closed mid-session — reconnecting")
            .unwrap_or_else(|| panic!("{logged:?}"));
        assert_eq!(ended.level, tracing::Level::WARN);
        assert_eq!(
            ended.field("error"),
            Some("the listenKey could not be renewed and is about to expire")
        );
    }

    /// A REST outage must not cost a working stream: its key has about half an hour left when the
    /// first PUT fails, and the POST of a re-open would fail too. Each wake retries (not the next
    /// renewal, which on the old clock was the key's expiry), the socket stays through every failure,
    /// and once a PUT works it is back on its cadence.
    #[test]
    fn failed_puts_are_retried_at_each_wake_and_the_socket_stays() {
        let renew = Duration::from_secs(3);
        let heard = pump_against_loopback(
            Heard { refuse_puts: AtomicUsize::new(3), ..Heard::default() },
            Some(Duration::from_millis(20)),
            renew,
            |h| h.puts.load(Ordering::Relaxed) >= 5,
        );
        let at = heard.put_times.lock().unwrap();
        for retry in 1..=3 {
            let waited = at[retry] - at[retry - 1];
            assert!(waited < renew / 2, "retry {retry} waited {waited:?}");
        }
        assert!(at[4] - at[3] >= renew, "back on its cadence, not {:?}", at[4] - at[3]);
        assert_eq!(heard.posts.load(Ordering::Relaxed), 1, "one key, one socket");
        assert_eq!(heard.reopens.load(Ordering::Relaxed), 0);
    }

    /// A venue with no socket keepalive still wakes the hook often enough to retry inside the key's
    /// 60 minutes, and its wire does not change: that wake sends nothing and the PUT keeps `renew_every`.
    #[test]
    fn the_hook_wakes_often_enough_for_a_retry_to_save_the_key() {
        let minute = Duration::from_secs(60);
        assert_eq!(hook_every(None, RENEW_EVERY), minute, "binance perp");
        assert_eq!(hook_every(Some(minute), RENEW_EVERY), minute, "aster");
        assert_eq!(KEY_AT_RISK_AFTER, minute * 55, "inside the venues' 60-minute key expiry");
    }

    /// The frame is the venue's own word that nothing more will be delivered on this socket, which
    /// it keeps open: `V3(Recommended)/EN/aster-finance-futures-api-v3.md` of `asterdex/api-docs`.
    #[test]
    fn the_venue_saying_the_key_expired_ends_the_session() {
        let heard = pump_against_loopback(
            Heard { say_expired: AtomicBool::new(true), ..Heard::default() },
            None,
            Duration::from_secs(3600),
            |h| h.reopens.load(Ordering::Relaxed) >= 1,
        );
        assert_eq!(heard.reopens.load(Ordering::Relaxed), 1);
        assert_eq!(heard.posts.load(Ordering::Relaxed), 2);
        assert_eq!(heard.puts.load(Ordering::Relaxed), 0, "no PUT was due");
    }

    /// A failed PUT logged nothing at all, so a key lost for this reason read as nothing happening. The
    /// venue's code stays in the line; the key does not, even when the venue's words echo it.
    #[test]
    fn a_failed_put_warns_and_no_line_says_the_key() {
        let heard = pump_against_loopback(
            Heard { refuse_puts: AtomicUsize::new(2), ..Heard::default() },
            None,
            Duration::from_millis(120),
            |h| h.puts.load(Ordering::Relaxed) >= 3,
        );
        let logged = heard.logged.lock().unwrap();
        let warned: Vec<&CapturedEvent> = logged
            .iter()
            .filter(|e| {
                e.level == tracing::Level::WARN && e.message.contains("listenKey renewal failed")
            })
            .collect();
        assert_eq!(warned.len(), 2, "{logged:?}");
        assert!(warned[0].field("error").is_some_and(|text| text.contains("400")), "{warned:?}");
        for line in logged.iter() {
            assert!(!line.message.contains(KEY), "{line:?}");
            assert!(!line.fields.values().any(|text| text.contains(KEY)), "{line:?}");
        }
    }

    /// The POST works and the WS host is down: the pump's reason goes on every observe frame, `trade
    /// health`, MCP and the log, and the dial names the host, never the path the key sits in.
    #[test]
    fn a_socket_that_cannot_connect_does_not_say_the_key() {
        let rest = loopback_venue(&Arc::new(Heard::default()));
        // Port 1: nothing listens, and no test's listener can be handed it (a released port can be).
        let nobody = "127.0.0.1:1";
        let outcome = open_user_data_websocket(
            &format!("http://{rest}"),
            "/listenKey",
            &format!("ws://{nobody}"),
            "/ws",
            &HeaderOnly,
            &AtomicBool::new(false),
        );
        let WebsocketSessionUserOpenOutcome::Transport(why) = outcome else {
            panic!("the WS host is down")
        };
        assert!(!why.contains(KEY), "{why}");
        assert!(why.starts_with(&format!("connect: connect {nobody}")), "what failed: {why}");
    }

    /// A server's close reason is the venue's own text, and it reaches the same places.
    #[test]
    fn a_close_reason_that_echoes_the_key_does_not_say_it() {
        let script = Heard { close_saying_key: AtomicBool::new(true), ..Heard::default() };
        let addr = loopback_venue(&Arc::new(script));
        let outcome = open_user_data_websocket(
            &format!("http://{addr}"),
            "/listenKey",
            &format!("ws://{addr}"),
            "/ws",
            &HeaderOnly,
            &AtomicBool::new(false),
        );
        let WebsocketSessionUserOpenOutcome::Ready(mut ws) = outcome else {
            panic!("the venue is up")
        };
        let why = loop {
            if let Err(WebsocketSessionUserError::Closed(why)) = ws.recv() {
                break why;
            }
        };
        assert!(!why.contains(KEY), "{why}");
        assert!(why.contains("<listenKey>"), "{why}");
    }

    /// The listenKey PUT cadence, as the literal it was before it moved into `venue_timings`. The
    /// pump reads it from binance perp's row in `spawn_user_data_with_resync` and runs it for binance perp AND
    /// both aster markets — one pump can run only ONE cadence — so aster's rows, which declare the
    /// same renewal, must agree with the row the pump reads. A divergent aster row would be a
    /// declaration no code honours.
    #[test]
    fn the_renewal_cadence_is_one_fact_for_the_family() {
        use vike_venues::venue_timings::{ASTER_PERP, ASTER_SPOT, BINANCE_PERP};
        let cadence = BINANCE_PERP.exec().session_renew_every();
        assert_eq!(
            cadence,
            Duration::from_secs(1800),
            "every 30 min, inside the 60-min key expiry"
        );
        assert_eq!(ASTER_SPOT.exec().session_renew_every(), cadence);
        assert_eq!(ASTER_PERP.exec().session_renew_every(), cadence);
    }

    /// The recv poll, the reconnect ceiling and the settle the pump runs with, as the literals the
    /// private constants of this file held, and the reconnect each family row declares agreeing with
    /// what the pump is actually handed (its own first backoff growing to `USER_DATA_MAX_BACKOFF`).
    #[test]
    fn the_pump_runs_with_the_poll_ceiling_and_settle_it_had_before_the_table() {
        use vike_bridge_core::websocket_session::user::USER_DATA_INITIAL_BACKOFF;
        use vike_venues::venue_timings::{ASTER_PERP, ASTER_SPOT, BINANCE_PERP};
        assert_eq!(USER_DATA_POLL, Duration::from_secs(1));
        assert_eq!(USER_DATA_MAX_BACKOFF, Duration::from_secs(30));
        assert_eq!(RESYNC_SETTLE, Duration::from_secs(1));
        for row in [BINANCE_PERP, ASTER_SPOT, ASTER_PERP] {
            let reconnect = row.exec().reconnect();
            assert_eq!(reconnect.initial, USER_DATA_INITIAL_BACKOFF);
            assert_eq!(reconnect.max, USER_DATA_MAX_BACKOFF);
        }
    }
}
