//! OKX private-WS session. Port of `exec/okx/user_data.py`: connect WITH the browser UA
//! (Cloudflare 1010 dodge on the demo endpoint) → login frame (SECONDS timestamp, never
//! logged) → event=='login' ack → subscribe (orders/SWAP) → event=='subscribe' ack →
//! the venue-neutral pump with the PERP mapper (ct_val rescale). Keepalive: raw text
//! "ping" every 15s (under OKX's 30s idle timeout); the raw "pong" reply is non-JSON
//! and the pump/handshake tolerate it.
//!
//! A SECOND socket carries the stops' own state: [`spawn_okx_algo_user_data_with_resync`] logs in the
//! same way on the BUSINESS url and subscribes `orders-algo`, because the private socket's `orders`
//! channel carries only the CHILD order a fired stop spawns, never the stop (cancelled by the venue,
//! refused at the trigger, fired).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::ratelimit::KeyedRateGate;
use vike_bridge_core::ws::{TungsteniteStream, await_ack, configure_ws_stream, send_gated};
use vike_exec::EventSender;
use vike_model::events::Event;
use vike_venues::venue_timings::OKX;

use crate::BROWSER_UA;
use crate::event_mapper::{map_okx_algo_frame, map_okx_perp, map_okx_perp_by_inst};
use crate::ws_auth::{PING_TEXT, build_login_frame, build_subscribe_frame, match_event_ack};
use vike_bridge_core::user_data::{
    OpenOutcome, RESYNC_SETTLE, ReconnectHook, ResyncPumpSpec, USER_DATA_MAX_BACKOFF,
    USER_DATA_POLL, UserDataFeed, UserDataResyncFeed, run_user_data_forever_with_idle, spawn_pump,
    spawn_pump_with_resync,
};

// The three timings below are this venue's exec-lane row in
// `crates/vike-venues/src/venue_timings.rs` (`OKX`), which carries each value and its
// reason (decision 0160). The pump's poll, reconnect ceiling and resync settle are the venue-free
// mechanism defaults of `vike_bridge_core::user_data` and are read from there.

/// Login-ack plus subscribe-ack deadline, started once the socket is up.
const HANDSHAKE_TIMEOUT: Duration = OKX.exec().handshake_timeout();
/// The raw-text `ping` cadence, under OKX's 30 s idle timeout.
const PING_EVERY: Duration = OKX.exec().keepalive_every();
/// Silent-stall watchdog: no inbound frame of ANY kind for this long ⇒ the socket is dead behind
/// an open connection, so end the session and let the reconnect + audit-A3 resync repair it.
/// 4x [`PING_EVERY`] — OKX answers each raw-text `ping` with a `pong`, so a healthy socket
/// delivers an inbound frame every ~15s even on a totally idle account. Keyed off that
/// server-answer cadence, NOT data cadence: an account with no orders is legitimately silent.
const IDLE_THRESHOLD: Duration = OKX.exec().idle_threshold();

/// The `instType` the production pumps subscribe the private `orders` channel and the business socket's
/// `orders-algo` channel with: `ANY`, one subscription for the SWAPs, the dated futures and the SPOT pairs an
/// account trades (decision 0232; ACCEPTED on the demo host for both channels, 2026-10-30, and a spot fill
/// arrived on `orders` with `instType SPOT`, `fillFee` and `fillFeeCcy` in the base coin).
/// It carries MARGIN and OPTION orders too: a fill of an instrument that is neither a SWAP nor an admitted
/// spot pair or future is dropped where its size is looked up (`crate::exec`'s `load_linear_ct_val`), so a
/// coin traded by hand in the OKX app never reaches the engine.
const ORDERS_INST_TYPE: &str = "ANY";

/// Connect (browser UA) → login (SECONDS ts) → ack → subscribe(`channel`, `inst_type`) → ack. The
/// private socket subscribes `orders`; the business socket (`ws_url` = [`crate::perp::business_ws_url`])
/// subscribes `orders-algo`. The login is the same signature on both.
pub fn open_okx_user_data_ws(
    ws_url: &str,
    api_key: &str,
    api_secret: &str,
    passphrase: &str,
    now_ms: impl Fn() -> i64,
    channel: &str,
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
    configure_ws_stream(&socket, USER_DATA_POLL);
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

    let sub = build_subscribe_frame(channel, inst_type);
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
        let now_ms = vike_model::now_ms;
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
                    "orders",
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
            USER_DATA_POLL,
            USER_DATA_MAX_BACKOFF,
            Some((PING_EVERY, &mut ping)),
            || {}, // no resync: use spawn_okx_perp_user_data_with_resync to enable audit A3
            Some(IDLE_THRESHOLD),
        )
    })
}

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
/// `run_resync_supervisor`'s own `on_reconcile` param — `Some` fires the recon trigger after
/// every reconnect's event-replay settles; `None` (every non-recon caller) reproduces the
/// pre-Task-7 event-replay-only behavior byte-for-byte.
///
/// `ct_val_of` answers the contracts→base factor for the `instId` a fill row names (the private
/// `orders` channel carries every SWAP of the account, not only `symbol`): the caller's
/// [`ContractSizes`](crate::per_symbol::ContractSizes). `None` drops that fill
/// ([`map_okx_perp_by_inst`]'s contract). It runs on the pump thread, so a lookup that loads a row
/// blocks the pump for one bounded REST call, once per instrument.
///
/// ⚠ BOTH producers of this function go through `fence`, never straight to `events`: the pump's sink is
/// `fence.deliver` and every batch `resync_fetch` returns passes `fence.hold` (`fenced_fetch`). The
/// `orders` channel pushes a `live` row for a MARKET order too (measured on the demo: a `live` row and its
/// `filled` row in the same millisecond, `live` first), and that row is the pump's own bare
/// `OrderAccepted`, never held. The fill behind it can still reach the lane before the REST answer, and
/// the A3 replay's rows carry no `live` row at all, so a fill of an order whose submit is still in flight
/// waits behind that order's own `OrderSubmitted`/`OrderAccepted`
/// (`vike_bridge_core::exec_actor::submit_fence`'s module doc). `fence` must wrap the SAME lane as
/// `events`, and the exec thread's `FencedRest` must wrap the same `fence`.
pub fn spawn_okx_perp_user_data_with_resync(
    ws_url: String,
    api_key: String,
    api_secret: String,
    passphrase: String,
    symbol: String,
    ct_val_of: impl Fn(&str) -> Option<f64> + Send + 'static,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
) -> UserDataResyncFeed {
    let sym_p = symbol.clone();
    spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: format!("okx-userdata-{symbol}"),
        resync_thread_name: format!("okx-resync-{symbol}"),
        poll: USER_DATA_POLL,
        settle: RESYNC_SETTLE,
        events,
        resync_fetch: fence.fenced_fetch(resync_fetch),
        on_reconcile,
        pump_body: move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
            let now_ms = vike_model::now_ms;
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
                        "orders",
                        ORDERS_INST_TYPE,
                        &stop,
                        &ws_gate,
                    )
                },
                |frame| map_okx_perp_by_inst(frame, "okx", &sym_p, &ct_val_of),
                |event| fence.deliver(event),
                &stop,
                USER_DATA_POLL,
                USER_DATA_MAX_BACKOFF,
                Some((PING_EVERY, &mut ping)),
                on_reconnect,
                Some(IDLE_THRESHOLD),
            )
        },
    })
}

/// The OKX ALGO-order socket: `orders-algo` on the BUSINESS URL ([`crate::perp::business_ws_url`]), the
/// only place the venue pushes a STOP's own state (`effective` = it fired, `canceled`, `order_failed`).
/// The private `orders` socket carries the fired stop's CHILD order, never the stop, so without this
/// pump a stop that was cancelled, expired or refused at the venue stays `ACCEPTED` in the node for
/// good. Same login, ping and idle watchdog as the private pump; its events are mapped by
/// [`map_okx_algo_frame`] and go through the SAME `fence` as the private pump's.
///
/// `resync_fetch` is the audit-A3 twin for THIS socket (the caller's closure over
/// [`map_okx_algo_history`](crate::history::map_okx_algo_history), on a client of its own): after a
/// reconnect it replays the stops that ended in the gap. No reconcile trigger is wired
/// (the private pump's is enough: it fires after its own reconnects, and a reconcile pass reads the
/// stops from `orders-algo-pending` regardless of this socket).
pub fn spawn_okx_algo_user_data_with_resync(
    ws_url: String,
    api_key: String,
    api_secret: String,
    passphrase: String,
    symbol: String,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
) -> UserDataResyncFeed {
    spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: format!("okx-algo-userdata-{symbol}"),
        resync_thread_name: format!("okx-algo-resync-{symbol}"),
        poll: USER_DATA_POLL,
        settle: RESYNC_SETTLE,
        events,
        resync_fetch: fence.fenced_fetch(resync_fetch),
        on_reconcile: None,
        pump_body: move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
            let now_ms = vike_model::now_ms;
            let mut ping = |ws: &mut TungsteniteStream| {
                let _ = ws.send_text(PING_TEXT); // raw text, NOT JSON
            };
            let ws_gate = crate::ratelimit::ws_rate_gate();
            run_user_data_forever_with_idle(
                || {
                    open_okx_user_data_ws(
                        &ws_url,
                        &api_key,
                        &api_secret,
                        &passphrase,
                        now_ms,
                        "orders-algo",
                        ORDERS_INST_TYPE,
                        &stop,
                        &ws_gate,
                    )
                },
                map_okx_algo_frame,
                |event| fence.deliver(event),
                &stop,
                USER_DATA_POLL,
                USER_DATA_MAX_BACKOFF,
                Some((PING_EVERY, &mut ping)),
                on_reconnect,
                Some(IDLE_THRESHOLD),
            )
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The EFFECTIVE timings the pump runs on, each a literal this file held before the venue
    /// timings table existed (decision 0160): evidence beside the table's own matrix pin, so a row
    /// edit that moves okx's liveness contract fails here too and is a deliberate act in both.
    #[test]
    fn the_pump_runs_on_the_timings_it_always_did() {
        assert_eq!(HANDSHAKE_TIMEOUT, Duration::from_secs(10));
        assert_eq!(PING_EVERY, Duration::from_secs(15));
        assert_eq!(IDLE_THRESHOLD, Duration::from_secs(60));
        assert_eq!(USER_DATA_POLL, Duration::from_secs(1));
        assert_eq!(USER_DATA_MAX_BACKOFF, Duration::from_secs(30));
        assert_eq!(RESYNC_SETTLE, Duration::from_secs(1));
    }

    /// The watchdog must outlast a late pong: four ping periods (the figure the `IDLE_THRESHOLD`
    /// doc and the row's `why` argue), so a healthy idle socket is never torn down.
    #[test]
    fn the_idle_threshold_is_four_ping_periods() {
        assert_eq!(IDLE_THRESHOLD, PING_EVERY * 4);
    }
}
