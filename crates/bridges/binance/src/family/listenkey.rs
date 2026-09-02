//! The listenKey user-data pump both family venues run — mint a key over REST, connect the raw
//! `<ws_base>[/ws]/<listenKey>` socket, hand it to bridge-core's venue-neutral pump. Shared by
//! vike-binance (`crate::perp_user_data`) and vike-aster (`vike_aster::{user_data,
//! perp_user_data}`), which had THREE copies of this one shape (F13).
//!
//! The shape, identical in all three: POST the listenKey endpoint → the 64-char key → connect
//! `wss://<ws_base>[/ws]/<key>` (**NO subscribe frame, NO ack handshake** — the key in the URL IS
//! the auth; do NOT copy Binance spot's WS-API ack loop here, the stream never acks and it would
//! hang) → [`run_user_data_forever`]. A PUT keepalive fires on a ~30-min cadence (60-min server
//! expiry) via the pump's keepalive hook — BOUNDED ([`KEEPALIVE_TIMEOUT`]) and BEST-EFFORT (any
//! error swallowed; a lost key self-heals on the next reconnect, which mints a fresh one). The key
//! is NEVER DELETEd on teardown (server-side expiry handles it).
//!
//! **The per-venue delta is [`ListenKeyAuth`] — how the listenKey REST call is authed.** Binance
//! sends the apiKey in an `X-MBX-APIKEY` header with the HMAC signer BYPASSED (no timestamp, no
//! signature); Aster's v3 endpoint is instead EIP-712-SIGNED, the auth riding in the query. That
//! ONE divergence is the whole reason these were three files. Everything else here — the bounded
//! request, the create/keepalive pair, the connect, the thread wiring, the resync supervisor — is
//! venue-neutral and lives once.
//!
//! **A SECOND, smaller delta is real and stays parameterized, not unified:** the WS path segment.
//! Binance perp connects `<ws_base>/<key>`; both Aster streams connect `<ws_base>/ws/<key>`. It is
//! passed as `ws_path` (`""` / `"/ws"`) — see [`user_data_ws_url`], whose tests pin both venues'
//! exact effective URLs.
//!
//! **Hosts and endpoint paths stay OUT of this module**, exactly as in rungs 2–3: each venue passes
//! its own `rest_url`/`ws_base_url`/`listenkey_path`, so Binance's stay the `const`s its callers
//! already threaded in and Aster's stay its own `urls::urls_for(env)` resolution. Nothing here
//! decides which network it is on, and nothing here became env-resolved.
//!
//! **Why family and not vike-bridge-core:** listenKey is Binance *wire grammar*, not a neutral
//! transport primitive — all three copies are family venues. Keeping it here preserves
//! bridge-core's venue-neutral charter; the genuinely neutral half (the reconnect/backoff/keepalive
//! reliability loop) already lives there as [`run_user_data_forever`] and is what this module
//! drives.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use serde_json::Value;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::user_data::{
    run_user_data_forever_with_idle, spawn_pump, spawn_pump_with_resync, OpenOutcome,
    ReconnectHook, ResyncPumpSpec, UserDataAuthError, UserDataFeed, UserDataResyncFeed,
};
use vike_bridge_core::ws::TungsteniteStream;
use vike_exec::EventSender;
use vike_model::events::Event;

/// Keepalive PUT bound — never blocks teardown.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(2);
/// Keepalive cadence: ~30 min, comfortably inside the venues' 60-min key expiry.
const KEEPALIVE_EVERY: Duration = Duration::from_secs(1800);
/// Create POST bound (looser than the keepalive's — a failed create costs a reconnect).
const CREATE_TIMEOUT: Duration = Duration::from_secs(5);
/// WS read-poll tick: how often the recv loop wakes to re-check `stop`.
const POLL: Duration = Duration::from_secs(1);
/// Reconnect backoff ceiling.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Post-reconnect settle before the resync supervisor replays — let the WS drain its backlog first.
const RESYNC_SETTLE: Duration = Duration::from_secs(1);

/// How one family venue authenticates its listenKey REST calls — **the SOLE divergence** between
/// the venues' otherwise identical pumps (see the module doc).
///
/// Deliberately a trait and not an enum: this module lives in vike-binance, which cannot name
/// Aster's signer (the dependency runs vike-aster → vike-binance, never back). Each venue
/// implements it on its own tiny struct and keeps ONLY its auth here.
///
/// Built ONCE per pump thread by the `make_auth` factory the spawn fns take, then borrowed `&self`
/// (shared) by both the on-connect create and the periodic keepalive — which is what lets Aster's
/// non-`Clone` signer live as a thread-local instead of being moved into either closure.
pub trait ListenKeyAuth {
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
/// which network it names (rung-2's [`UrlTable`](super::UrlTable) discipline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpSpec {
    /// Canonical venue key, stamped as a span field (`"binance"` / `"aster"`).
    pub venue: &'static str,
    /// Pump thread-name prefix; the thread is named `"{thread_tag}-{symbol}"`
    /// (e.g. `"binance-perp-userdata"`).
    pub thread_tag: &'static str,
    /// Resync-supervisor thread-name prefix, named `"{resync_tag}-{symbol}"`. Only read by
    /// [`spawn_user_data_with_resync`].
    pub resync_tag: &'static str,
    /// REST origin the listenKey is minted against, bare (e.g. `https://fapi.binance.com`).
    pub rest_url: String,
    /// User-data WS host, bare — no trailing `/ws` (see `ws_path`).
    pub ws_base_url: String,
    /// WS path segment before the key: `""` (Binance perp) or `"/ws"` (both Aster streams). A real
    /// venue divergence, NOT unified — see [`user_data_ws_url`].
    pub ws_path: &'static str,
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

/// The user-data WS URL. `ws_path` is the venue divergence: `""` ⇒ `<base>/<key>` (Binance perp),
/// `"/ws"` ⇒ `<base>/ws/<key>` (Aster spot + perp).
pub fn user_data_ws_url(ws_base_url: &str, ws_path: &str, listen_key: &str) -> String {
    format!("{ws_base_url}{ws_path}/{listen_key}")
}

/// One authed listenKey call. Small dedicated agent so the keepalive's 2s bound is independent of
/// the shared transport's 30s default.
fn listenkey_request(
    rest_url: &str,
    listenkey_path: &str,
    auth: &dyn ListenKeyAuth,
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
    let mut resp = req
        .send_empty()
        .map_err(|e| VenueApiError { code: 0, msg: format!("network error: {e}") })?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().unwrap_or_default();
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
    auth: &dyn ListenKeyAuth,
) -> Result<String, VenueApiError> {
    let body = listenkey_request(rest_url, listenkey_path, auth, "POST", CREATE_TIMEOUT)?;
    Ok(body.get("listenKey").and_then(|k| k.as_str()).unwrap_or("").to_string())
}

/// Best-effort PUT keepalive — swallow ANY error (a raised keepalive would trigger the pump's
/// reconnect-with-backoff for nothing; a truly dead key reconnects and re-mints).
pub fn listenkey_keepalive(rest_url: &str, listenkey_path: &str, auth: &dyn ListenKeyAuth) {
    let _ = listenkey_request(rest_url, listenkey_path, auth, "PUT", KEEPALIVE_TIMEOUT);
}

/// Create the listenKey then connect the user-data socket. No subscribe frame, no ack loop —
/// connected = subscribed.
pub fn open_user_data_ws(
    rest_url: &str,
    listenkey_path: &str,
    ws_base_url: &str,
    ws_path: &str,
    auth: &dyn ListenKeyAuth,
    stop: &AtomicBool,
) -> OpenOutcome<TungsteniteStream> {
    if stop.load(Ordering::Relaxed) {
        return OpenOutcome::Stopped;
    }
    let listen_key = match listenkey_create(rest_url, listenkey_path, auth) {
        Ok(k) if !k.is_empty() => k,
        Ok(_) => return OpenOutcome::Transport("empty listenKey".into()),
        Err(e) => return OpenOutcome::Transport(format!("listenKey create: {e}")),
    };
    let url = user_data_ws_url(ws_base_url, ws_path, &listen_key);
    let (socket, _resp) = match tungstenite::connect(&url) {
        Ok(ok) => ok,
        Err(e) => return OpenOutcome::Transport(format!("connect: {e}")),
    };
    vike_bridge_core::ws::configure_ws_stream(&socket, POLL);
    OpenOutcome::Ready(TungsteniteStream(socket))
}

/// The pump-thread BODY [`spawn_user_data`] and [`spawn_user_data_with_resync`] share — the closure
/// that runs ON the pump thread (bridge-core's [`spawn_pump`]/[`spawn_pump_with_resync`] own the
/// thread spawn + stop flag + the resync `Weak`/`drop` self-exit; dedup A8). `on_reconnect` is the
/// only thing that differs between the two callers (`|| {}` vs the builder's generation bump).
///
/// `make_auth` runs ON the spawned thread, which is what lets a non-`Clone` signer (Aster's) be
/// built from moved `Credentials` and then borrowed `&self` by both closures below.
fn make_pump_body<A, F, M>(
    spec: PumpSpec,
    symbol: String,
    make_auth: F,
    decode: M,
    events: EventSender,
) -> impl FnOnce(Arc<AtomicBool>, ReconnectHook) -> Result<(), UserDataAuthError> + Send + 'static
where
    A: ListenKeyAuth,
    F: FnOnce() -> A + Send + 'static,
    M: Fn(&Value) -> Vec<Event> + Send + 'static,
{
    move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
        let venue = spec.venue;
        let _span = tracing::info_span!("user_data_pump", venue, symbol = %symbol).entered();
        // Built ONCE on this thread; may be non-`Clone`, so it stays owned here and both
        // closures below borrow it `&self` (shared) — never moved into either.
        let auth = make_auth();
        let mut keepalive = |ws: &mut TungsteniteStream| {
            let _ = ws; // the keepalive is an HTTP PUT, not a WS frame
            listenkey_keepalive(&spec.rest_url, spec.listenkey_path, &auth);
        };
        run_user_data_forever_with_idle(
            || {
                open_user_data_ws(
                    &spec.rest_url,
                    spec.listenkey_path,
                    &spec.ws_base_url,
                    spec.ws_path,
                    &auth,
                    &stop,
                )
            },
            decode,
            |event| events.blocking_send(event).is_ok(),
            &stop,
            POLL,
            MAX_BACKOFF,
            Some((KEEPALIVE_EVERY, &mut keepalive)),
            on_reconnect,
            spec.idle_threshold,
        )
    }
}

/// Spawn the persistent fill-stream pump feeding the vt-core ingest (lossless), with NO resync —
/// use [`spawn_user_data_with_resync`] to enable audit A3.
pub fn spawn_user_data<A, F, M>(
    spec: PumpSpec,
    symbol: String,
    make_auth: F,
    decode: M,
    events: EventSender,
) -> UserDataFeed
where
    A: ListenKeyAuth,
    F: FnOnce() -> A + Send + 'static,
    M: Fn(&Value) -> Vec<Event> + Send + 'static,
{
    let pump_name = format!("{}-{symbol}", spec.thread_tag);
    let body = make_pump_body(spec, symbol, make_auth, decode, events);
    // No resync ⇒ a no-op reconnect hook (byte-identical to the prior `|| {}`).
    spawn_pump(pump_name, move |stop: Arc<AtomicBool>| body(stop, Box::new(|| {})))
}

/// Like [`spawn_user_data`], plus the audit-A3 resync supervisor. On every WS re-open the pump
/// bumps a generation counter, and the supervisor (after [`RESYNC_SETTLE`]) replays recent order
/// history via `resync_fetch` so a fill/cancel/expire/reject that landed during the reconnect gap
/// is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST history source,
/// on a SEPARATE REST client from the order-submit path. Both threads tear down on `shutdown`; the
/// supervisor also self-exits if the pump dies or the core is gone.
///
/// `on_reconcile` is threaded straight to [`run_resync_supervisor`]'s own param — `Some` pokes the
/// reconcile driver after every reconnect's event-replay settles; `None` (every non-recon caller)
/// reproduces the event-replay-only behavior.
pub fn spawn_user_data_with_resync<A, F, M>(
    spec: PumpSpec,
    symbol: String,
    make_auth: F,
    decode: M,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
) -> UserDataResyncFeed
where
    A: ListenKeyAuth,
    F: FnOnce() -> A + Send + 'static,
    M: Fn(&Value) -> Vec<Event> + Send + 'static,
{
    let pump_name = format!("{}-{symbol}", spec.thread_tag);
    let resync_name = format!("{}-{symbol}", spec.resync_tag);
    let events_p = events.clone();
    let body = make_pump_body(spec, symbol, make_auth, decode, events_p);
    // The stop flag + session-gen + reconnect-bump + `Weak`/`drop` self-exit + resync thread all
    // live ONCE in `spawn_pump_with_resync` (dedup A8); we supply only the venue body + names.
    spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: pump_name,
        resync_thread_name: resync_name,
        poll: POLL,
        settle: RESYNC_SETTLE,
        events,
        resync_fetch,
        on_reconcile,
        pump_body: body,
    })
}

#[cfg(test)]
mod listenkey_tests {
    use super::*;

    /// Binance's auth shape (apiKey header, no query) — the real one lives in
    /// `vike_binance::perp_user_data`.
    struct HeaderOnly;
    impl ListenKeyAuth for HeaderOnly {
        fn query(&self, _method: &str) -> String {
            String::new()
        }
        fn header(&self) -> Option<(&'static str, String)> {
            Some(("X-MBX-APIKEY", "KEY".to_string()))
        }
    }

    /// Aster's auth shape (signed query, no header).
    struct SignedQuery;
    impl ListenKeyAuth for SignedQuery {
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
            user_data_ws_url("wss://fstream.binance.com", "", "LKEY"),
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
            user_data_ws_url("wss://fstream.asterdex.com", "/ws", "LKEY"),
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
}
