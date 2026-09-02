//! OKX private-WS session. Port of `exec/okx/user_data.py`: connect WITH the browser UA
//! (Cloudflare 1010 dodge on the demo endpoint) → login frame (SECONDS timestamp, never
//! logged) → event=='login' ack → subscribe (orders/SWAP) → event=='subscribe' ack →
//! the venue-neutral pump with the PERP mapper (ct_val rescale). Keepalive: raw text
//! "ping" every 15s (under OKX's 30s idle timeout); the raw "pong" reply is non-JSON
//! and the pump/handshake tolerate it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use vike_bridge_core::ratelimit::KeyedRateGate;
use vike_bridge_core::ws::{await_ack, configure_ws_stream, send_gated, TungsteniteStream};
use vike_exec::EventSender;
use vike_model::events::Event;

use crate::event_mapper::map_okx_perp;
use crate::transport::BROWSER_UA;
use crate::ws_auth::{build_login_frame, build_subscribe_frame, match_event_ack, PING_TEXT};
use vike_bridge_core::user_data::{
    run_user_data_forever_with_idle, spawn_pump, spawn_pump_with_resync, OpenOutcome,
    ReconnectHook, ResyncPumpSpec, UserDataFeed,
};

const POLL: Duration = Duration::from_secs(1);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(15);
/// Silent-stall watchdog: no inbound frame of ANY kind for this long ⇒ the socket is dead behind
/// an open connection, so end the session and let the reconnect + audit-A3 resync repair it.
/// 4x [`PING_EVERY`] — OKX answers each raw-text `ping` with a `pong`, so a healthy socket
/// delivers an inbound frame every ~15s even on a totally idle account. Keyed off that
/// server-answer cadence, NOT data cadence: an account with no orders is legitimately silent.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(60);

/// Connect (browser UA) → login (SECONDS ts) → ack → subscribe(SWAP) → ack.
// handshake takes url + 3 creds + clock + inst_type + stop + the WS rate gate
#[allow(clippy::too_many_arguments)]
pub fn open_okx_user_data_ws(
    ws_url: &str,
    api_key: &str,
    api_secret: &str,
    passphrase: &str,
    now_ms: impl Fn() -> i64,
    inst_type: &str,
    stop: &AtomicBool,
    gate: &KeyedRateGate,
) -> OpenOutcome<TungsteniteStream> {
    let Ok(mut request) = ws_url.into_client_request() else {
        return OpenOutcome::Transport("bad ws url".into());
    };
    request
        .headers_mut()
        .insert("User-Agent", tungstenite::http::HeaderValue::from_static(BROWSER_UA));
    let (mut socket, _resp) = match tungstenite::connect(request) {
        Ok(ok) => ok,
        Err(e) => return OpenOutcome::Transport(format!("connect: {e}")),
    };
    configure_ws_stream(&socket, POLL);
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;

    // login — NEVER log (apiKey + passphrase + sign); OKX wants epoch SECONDS
    let login = build_login_frame(api_key, api_secret, passphrase, now_ms() / 1000);
    if let Err(e) = send_gated(&mut socket, Some(gate), "login", &login.to_string()) {
        let _ = socket.close(None);
        return OpenOutcome::Transport(format!("login send: {e:?}"));
    }
    if let Err(outcome) =
        await_ack(&mut socket, stop, deadline, "OKX WS login", |f| match_event_ack(f, "login"))
    {
        let _ = socket.close(None);
        return *outcome;
    }

    let sub = build_subscribe_frame(inst_type);
    if let Err(e) = send_gated(&mut socket, Some(gate), "subscribe", &sub.to_string()) {
        let _ = socket.close(None);
        return OpenOutcome::Transport(format!("subscribe send: {e:?}"));
    }
    if let Err(outcome) = await_ack(&mut socket, stop, deadline, "OKX WS subscribe", |f| {
        match_event_ack(f, "subscribe")
    }) {
        let _ = socket.close(None);
        return *outcome;
    }
    OpenOutcome::Ready(TungsteniteStream(socket))
}

/// Spawn the persistent OKX SWAP fill-stream pump feeding the vt-core ingest.
///
/// `ready` is set `true` the first time the login+subscribe handshake completes — a caller can wait on
/// it to KNOW the pump is subscribed before it acts (condition-based, vs a fixed sleep that races a
/// cold-start handshake and drops a fill that lands before subscribe). The exec adapter uses the
/// `_with_resync` twin + a fill-sentinel instead, so this readiness gate is for the manual smokes.
#[allow(clippy::too_many_arguments)] // OKX handshake needs 3 creds + inst + clock + sink + ready
pub fn spawn_okx_perp_user_data(
    ws_url: String,
    api_key: String,
    api_secret: String,
    passphrase: String,
    symbol: String,
    ct_val: f64,
    events: EventSender,
    ready: Arc<AtomicBool>,
) -> UserDataFeed {
    spawn_pump(format!("okx-userdata-{symbol}"), move |stop: Arc<AtomicBool>| {
        let _span = tracing::info_span!("user_data_pump", venue = "okx", %symbol).entered();
        let now_ms = vike_model::clock::now_ms;
        let mut ping = |ws: &mut TungsteniteStream| {
            let _ = ws.send_text(PING_TEXT); // raw text, NOT JSON
        };
        // Built ONCE per pump; the reconnect loop reuses this closure, so the gate persists
        // across reconnects and paces cumulative login/subscribe sends (spec §A keyed WS gate).
        let ws_gate = crate::ratelimit::ws_rate_gate();
        run_user_data_forever_with_idle(
            || {
                let outcome = open_okx_user_data_ws(
                    &ws_url,
                    &api_key,
                    &api_secret,
                    &passphrase,
                    now_ms,
                    "SWAP",
                    &stop,
                    &ws_gate,
                );
                // Subscribe acked → the pump can now receive fills. Latch ready so a waiter
                // proceeds only once a fill can't be missed.
                if matches!(outcome, OpenOutcome::Ready(_)) {
                    ready.store(true, Ordering::Release);
                }
                outcome
            },
            |frame| map_okx_perp(frame, "okx", &symbol, ct_val),
            |event| events.blocking_send(event).is_ok(),
            &stop,
            POLL,
            Duration::from_secs(30),
            Some((PING_EVERY, &mut ping)),
            || {}, // no resync: use spawn_okx_perp_user_data_with_resync to enable audit A3
            Some(IDLE_THRESHOLD),
        )
    })
}

/// Join half of the OKX pump + its audit-A3 resync supervisor (see
/// [`spawn_okx_perp_user_data_with_resync`]). Perp twin of the spot `BinanceUserDataResync`; thin
/// alias for the shared [`UserDataResyncFeed`](vike_bridge_core::user_data::UserDataResyncFeed).
pub type OkxUserDataResync = vike_bridge_core::user_data::UserDataResyncFeed;

/// Like [`spawn_okx_perp_user_data`], plus the audit-A3 resync supervisor — the OKX mirror of the
/// spot `spawn_binance_user_data_with_resync`.
/// On every WS re-open the pump bumps a generation counter, and the supervisor (after a settle
/// delay) replays recent history via `resync_fetch` so a fill/cancel/reject that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST
/// history source (typically `get_orders_history` + `get_fills_history` →
/// [`map_okx_history`](crate::history::map_okx_history)) on a SEPARATE `OkxPerpRest` from the
/// order-submit client. Both threads tear down on `shutdown`; the supervisor also self-exits if the
/// pump dies or the core is gone. (Exercised by live smokes.)
///
/// `on_reconcile` (reconciliation-activation Task 7) is threaded straight to
/// `run_resync_supervisor`'s own `on_reconcile` param — `Some` pokes the reconcile driver after
/// every reconnect's event-replay settles; `None` (every non-recon caller) reproduces the
/// pre-Task-7 event-replay-only behavior byte-for-byte.
#[allow(clippy::too_many_arguments)]
pub fn spawn_okx_perp_user_data_with_resync(
    ws_url: String,
    api_key: String,
    api_secret: String,
    passphrase: String,
    symbol: String,
    ct_val: f64,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
) -> OkxUserDataResync {
    let events_p = events.clone();
    let sym_p = symbol.clone();
    spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: format!("okx-userdata-{symbol}"),
        resync_thread_name: format!("okx-resync-{symbol}"),
        poll: POLL,
        settle: Duration::from_secs(1), // let the reopened WS drain its backlog first
        events,
        resync_fetch,
        on_reconcile,
        pump_body: move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
            let now_ms = vike_model::clock::now_ms;
            let mut ping = |ws: &mut TungsteniteStream| {
                let _ = ws.send_text(PING_TEXT); // raw text, NOT JSON
            };
            // Built ONCE per pump; the reconnect loop reuses this closure, so the gate persists
            // across reconnects and paces cumulative login/subscribe sends (spec §A keyed WS gate).
            let ws_gate = crate::ratelimit::ws_rate_gate();
            run_user_data_forever_with_idle(
                || {
                    open_okx_user_data_ws(
                        &ws_url,
                        &api_key,
                        &api_secret,
                        &passphrase,
                        now_ms,
                        "SWAP",
                        &stop,
                        &ws_gate,
                    )
                },
                |frame| map_okx_perp(frame, "okx", &sym_p, ct_val),
                |event| events_p.blocking_send(event).is_ok(),
                &stop,
                POLL,
                Duration::from_secs(30),
                Some((PING_EVERY, &mut ping)),
                on_reconnect,
                Some(IDLE_THRESHOLD),
            )
        },
    })
}
