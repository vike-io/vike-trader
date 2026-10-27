//! Binance WS-API private-stream session. Port of `exec/binance/user_data.py`:
//! connect → send ONE signed subscribe (never logged — carries apiKey + signature) →
//! loop until the id-matched status==200 ack (bounded by a per-recv poll timeout and an
//! overall handshake deadline, so a half-open handshake can never hang the thread) →
//! hand the socket to the venue-neutral pump, decoding frames with `map_binance_private`
//! and feeding the vt-core ingest LOSSLESSLY via `EventSender`.
//!
//! Binance's server sends 20-second protocol PING control frames — answered with Pongs
//! at this layer (no app-level ping, matching Python's `ping=None`).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use vike_bridge_core::ratelimit::KeyedRateGate;
use vike_bridge_core::ws::{TungsteniteStream, await_ack, configure_ws_stream, send_gated};
use vike_exec::EventSender;
use vike_model::events::Event;

use crate::event_mapper::map_binance_private;
use crate::ws_auth::{build_subscribe_request, match_subscribe_ack};
use vike_bridge_core::user_data::{
    OpenOutcome, RESYNC_SETTLE, ReconnectHook, ResyncPumpSpec, USER_DATA_MAX_BACKOFF,
    USER_DATA_POLL, UserDataFeed, UserDataResyncFeed, run_user_data_forever_with_idle, spawn_pump,
    spawn_pump_with_resync,
};

pub const DEMO_WS: &str = "wss://demo-ws-api.binance.com/ws-api/v3";
pub const MAINNET_WS: &str = "wss://ws-api.binance.com/ws-api/v3";

/// Silent-stall watchdog: no inbound frame of ANY kind for this long ⇒ the socket is dead behind an
/// open connection, so end the session and let the reconnect + audit-A3 resync repair it.
///
/// The VALUE is `vike_venues::venue_timings::BINANCE_SPOT`'s exec-lane `idle_threshold`, with
/// its reason: 3x the WS-API server-ping interval. There is no app-level ping on this lane — the
/// SERVER pings and tungstenite pongs — so that server cadence is the only liveness an idle account
/// produces. MEASURED (`tests/binance_spot_userdata_soak.rs`, 900s window): 45 inbound frames, every
/// gap 19-20s, worst 20.004s.
///
/// ⚠ **Spot is NOT perp.** The perp lane's server ping is 180s and its threshold 720s
/// (`BINANCE_PERP`'s row, read by `perp_user_data.rs`) — a 9x difference on the same exchange,
/// because spot rides the WS-API host while perp rides a listenKey stream. Neither venue's number
/// may be assumed for the other; both were measured separately for exactly this reason.
pub const IDLE_THRESHOLD: Duration =
    vike_venues::venue_timings::BINANCE_SPOT.exec().idle_threshold();

/// Connect + signed subscribe + ack-await. The socket is closed on ANY handshake exit.
/// `req_id` derives from the clock (unique per attempt, no uuid dependency).
pub fn open_binance_user_data_ws(
    ws_url: &str,
    api_key: &str,
    api_secret: &str,
    now_ms: impl Fn() -> i64,
    stop: &AtomicBool,
    gate: &KeyedRateGate,
) -> OpenOutcome<TungsteniteStream> {
    let (mut socket, _resp) = match tungstenite::connect(ws_url) {
        Ok(ok) => ok,
        Err(e) => return OpenOutcome::Transport(format!("connect: {e}")),
    };
    configure_ws_stream(&socket, USER_DATA_POLL);

    let req_id = format!("vtr{}", now_ms());
    let request = build_subscribe_request(api_key, api_secret, now_ms(), 5000, &req_id);
    // The single signed subscribe+auth frame — WS-gated (spec §A keyed WS gate). StreamError is
    // Debug-only, so format it with `{e:?}`.
    if let Err(e) = send_gated(&mut socket, Some(gate), "subscribe", &request.to_string()) {
        let _ = socket.close(None);
        return OpenOutcome::Transport(format!("subscribe send: {e:?}"));
    }

    // Single-step handshake: the id-matched subscribe ack IS the whole handshake, so on Ok the socket
    // becomes the live stream (bybit/okx run this twice for auth then subscribe). The shared helper
    // leaves the socket open; close it on any error path. The bound is the exec lane's
    // `handshake_timeout` in `BINANCE_SPOT`'s row: one deadline for the single subscribe and its ack.
    let deadline =
        Instant::now() + vike_venues::venue_timings::BINANCE_SPOT.exec().handshake_timeout();
    match await_ack(&mut socket, stop, deadline, "Binance WS subscribe", |f| {
        match_subscribe_ack(f, &req_id)
    }) {
        Ok(()) => OpenOutcome::Ready(TungsteniteStream(socket)),
        Err(outcome) => {
            let _ = socket.close(None);
            *outcome
        }
    }
}

/// Spawn the persistent Binance user-data pump feeding the vt-core ingest. Decoded
/// events go through `EventSender::blocking_send` — LOSSLESS (fills must never drop);
/// a closed core ends the pump cleanly. `shutdown()` joins deterministically.
pub fn spawn_binance_user_data(
    ws_url: String,
    api_key: String,
    api_secret: String,
    symbol: String,
    events: EventSender,
) -> UserDataFeed {
    spawn_pump(format!("binance-userdata-{symbol}"), move |stop: Arc<AtomicBool>| {
        let _span = tracing::info_span!("user_data_pump", venue = "binance", %symbol).entered();
        let now_ms = vike_model::now_ms;
        // Built ONCE per pump; the reconnect loop reuses this closure, so the gate persists
        // across reconnects and paces cumulative subscribe sends (spec §A keyed WS gate).
        let ws_gate = crate::ratelimit::ws_rate_gate();
        run_user_data_forever_with_idle(
            || open_binance_user_data_ws(&ws_url, &api_key, &api_secret, now_ms, &stop, &ws_gate),
            |frame| map_binance_private(frame, "binance", &symbol),
            |event| events.blocking_send(event).is_ok(),
            &stop,
            USER_DATA_POLL,
            USER_DATA_MAX_BACKOFF,
            None,  // the WS-API server pings every 20s; tungstenite pongs — no app ping
            || {}, // no resync: use spawn_binance_user_data_with_resync to enable audit A3
            Some(IDLE_THRESHOLD),
        )
    })
}

/// Like [`spawn_binance_user_data`], plus the audit-A3 resync supervisor: on every WS re-open the
/// pump bumps a generation counter, and the supervisor (after a settle delay) replays recent order
/// history via `resync_fetch` so a terminal that landed during the reconnect gap is recovered — the
/// core dedups the overlap. `resync_fetch` is the caller's REST history source (typically
/// `get_all_orders` + `get_my_trades` → [`map_binance_history`](super::history::map_binance_history))
/// on a SEPARATE `BinanceSpotRest` from the order-submit client. Both threads tear down on `shutdown`;
/// the supervisor also self-exits if the pump dies or the core is gone. (Exercised by live smokes,
/// like the other spawn functions.)
///
/// The stop flag + session-gen + the pump's reconnect-bump + the `Weak`/`drop` self-exit trick +
/// the resync thread all live ONCE in [`spawn_pump_with_resync`] (dedup A8); this function supplies
/// only the venue closures + names. `on_reconcile` (reconciliation-activation Task 7) is threaded
/// straight to `run_resync_supervisor` — `Some` fires the recon trigger after every reconnect's
/// event-replay settles; `None` (every non-recon caller) reproduces the pre-Task-7
/// event-replay-only behavior byte-for-byte.
pub fn spawn_binance_user_data_with_resync(
    ws_url: String,
    api_key: String,
    api_secret: String,
    symbol: String,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
) -> UserDataResyncFeed {
    let events_p = events.clone();
    let sym_p = symbol.clone();
    spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: format!("binance-userdata-{symbol}"),
        resync_thread_name: format!("binance-resync-{symbol}"),
        poll: USER_DATA_POLL,
        settle: RESYNC_SETTLE, // let the reopened WS drain its backlog first
        events,
        resync_fetch,
        on_reconcile,
        pump_body: move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
            let _span =
                tracing::info_span!("user_data_pump", venue = "binance", symbol = %sym_p).entered();
            let now_ms = vike_model::now_ms;
            // Built ONCE per pump; the reconnect loop reuses this closure, so the gate persists
            // across reconnects and paces cumulative subscribe sends (spec §A keyed WS gate).
            let ws_gate = crate::ratelimit::ws_rate_gate();
            run_user_data_forever_with_idle(
                || {
                    open_binance_user_data_ws(
                        &ws_url,
                        &api_key,
                        &api_secret,
                        now_ms,
                        &stop,
                        &ws_gate,
                    )
                },
                |frame| map_binance_private(frame, "binance", &sym_p),
                |event| events_p.blocking_send(event).is_ok(),
                &stop,
                USER_DATA_POLL,
                USER_DATA_MAX_BACKOFF,
                None,
                on_reconnect,
                Some(IDLE_THRESHOLD),
            )
        },
    })
}

#[cfg(test)]
mod user_data_tests {
    use super::*;
    use vike_venues::venue_timings::BINANCE_SPOT;

    /// The numbers this session ran with before they moved into `venue_timings`, pinned here as
    /// literals: a retune of the row is then a deliberate edit in this file too, beside the matrix
    /// pin in vike-model. `IDLE_THRESHOLD` is what both spawn sites hand the pump, so its EFFECTIVE
    /// value is the const; the handshake bound and the pump defaults are read straight from where
    /// `open_binance_user_data_ws` and the two spawn functions read them.
    #[test]
    fn the_session_runs_with_the_values_it_had_before_the_table() {
        assert_eq!(IDLE_THRESHOLD, Duration::from_secs(60), "3x the 20 s WS-API server ping");
        assert_eq!(BINANCE_SPOT.exec().handshake_timeout(), Duration::from_secs(10));
        assert_eq!(USER_DATA_POLL, Duration::from_secs(1));
        assert_eq!(USER_DATA_MAX_BACKOFF, Duration::from_secs(30));
        assert_eq!(RESYNC_SETTLE, Duration::from_secs(1));
    }

    /// The row declares the reconnect policy the spawn sites really pass: the pump's own first
    /// backoff growing to the `USER_DATA_MAX_BACKOFF` both of them hand it.
    #[test]
    fn the_row_declares_the_backoff_the_pump_is_given() {
        let reconnect = BINANCE_SPOT.exec().reconnect();
        assert_eq!(reconnect.initial, vike_bridge_core::user_data::USER_DATA_INITIAL_BACKOFF);
        assert_eq!(reconnect.max, USER_DATA_MAX_BACKOFF);
    }
}
