//! Bybit private-WS session. Port of `exec/bybit/user_data.py`: connect → auth frame
//! (never logged) → op=='auth' ack → subscribe frame → op=='subscribe' ack → hand the
//! socket to the venue-neutral pump, decoding with the PERP mapper and feeding the
//! vt-core ingest losslessly. App-level `{"op":"ping"}` keepalive every 20s (Bybit's
//! server does not WS-protocol-ping like Binance).
//!
//! Fast-fill hint (opt-in `VIKE_BYBIT_FAST_EXEC=1`): when set, `execution.fast` is PREPENDED to the
//! private-topic subscribe list ([`private_topics`]) — Bybit's low-latency fill stream on the SAME
//! authed socket. `execution` is ALWAYS retained: the fast stream AUGMENTS, never replaces it (the
//! slow twin carries execFee + the terminal wrap). The perp mapper's
//! [`crate::event_mapper::map_execution_fast`] turns each fast row into an EARLY bare Fill; dedup by
//! execId (the engine's `seen_trade_ids`) books it exactly once. Unset ⇒ the historical
//! `["execution", "order"]` list, byte-identical (the mapper's fast arm is inert with no fast frames).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use vike_bridge_core::ratelimit::KeyedRateGate;
use vike_bridge_core::ws::{await_ack, configure_ws_stream, send_gated, TungsteniteStream};
use vike_exec::EventSender;
use vike_model::events::Event;

use crate::event_mapper::map_bybit_perp;
use crate::ws_auth::{build_auth_frame, build_subscribe_frame, match_op_ack};
use vike_bridge_core::user_data::{
    run_user_data_forever_with_idle, spawn_pump, spawn_pump_with_resync, OpenOutcome,
    ReconnectHook, ResyncPumpSpec, UserDataFeed,
};

const POLL: Duration = Duration::from_secs(1);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(20);
/// Silent-stall watchdog: no inbound frame of ANY kind for this long ⇒ the socket is dead behind
/// an open connection, so end the session and let the reconnect + audit-A3 resync repair it.
/// 3x [`PING_EVERY`] — Bybit answers each app-level `{"op":"ping"}` with a pong, so a healthy
/// socket delivers an inbound frame every ~20s even on a totally idle account. Keyed off that
/// server-answer cadence, NOT data cadence: an account with no orders is legitimately silent.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(60);

/// The private-WS topic list. Bybit's low-latency `execution.fast` fill hint is PREPENDED only when
/// `fast_exec` is true; otherwise the historical `["execution", "order"]`. `execution` is ALWAYS
/// present — the fast stream augments it (the slow twin carries execFee + the terminal wrap the FSM
/// folds), never replaces it. Pure (takes the resolved flag) so it is unit-testable without env.
fn private_topics(fast_exec: bool) -> Vec<&'static str> {
    if fast_exec {
        vec!["execution.fast", "execution", "order"]
    } else {
        vec!["execution", "order"]
    }
}

/// Reads the `VIKE_BYBIT_FAST_EXEC` opt-in gate off the real process env — ON only for the EXACT
/// string `"1"` (same idiom as `VIKE_RECONCILE`), OFF (byte-identical to today) otherwise.
fn fast_exec_enabled() -> bool {
    std::env::var("VIKE_BYBIT_FAST_EXEC").ok().as_deref() == Some("1")
}

/// Connect → auth → ack → subscribe → ack. Socket closed on ANY handshake exit.
pub fn open_bybit_user_data_ws(
    ws_url: &str,
    api_key: &str,
    api_secret: &str,
    now_ms: impl Fn() -> i64,
    topics: &[&str],
    stop: &AtomicBool,
    gate: &KeyedRateGate,
) -> OpenOutcome<TungsteniteStream> {
    let (mut socket, _resp) = match tungstenite::connect(ws_url) {
        Ok(ok) => ok,
        Err(e) => return OpenOutcome::Transport(format!("connect: {e}")),
    };
    configure_ws_stream(&socket, POLL);
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;

    // auth — NEVER log the frame (api_key + signature)
    let auth = build_auth_frame(api_key, api_secret, now_ms());
    if let Err(e) = send_gated(&mut socket, Some(gate), "login", &auth.to_string()) {
        let _ = socket.close(None);
        return OpenOutcome::Transport(format!("auth send: {e:?}"));
    }
    if let Err(outcome) =
        await_ack(&mut socket, stop, deadline, "Bybit WS auth", |f| match_op_ack(f, "auth"))
    {
        let _ = socket.close(None);
        return *outcome;
    }

    // subscribe
    let sub = build_subscribe_frame(topics);
    if let Err(e) = send_gated(&mut socket, Some(gate), "subscribe", &sub.to_string()) {
        let _ = socket.close(None);
        return OpenOutcome::Transport(format!("subscribe send: {e:?}"));
    }
    if let Err(outcome) = await_ack(&mut socket, stop, deadline, "Bybit WS subscribe", |f| {
        match_op_ack(f, "subscribe")
    }) {
        let _ = socket.close(None);
        return *outcome;
    }
    OpenOutcome::Ready(TungsteniteStream(socket))
}

/// Spawn the persistent Bybit perp fill-stream pump feeding the vt-core ingest.
///
/// `ready` is set `true` the first time the auth+subscribe handshake completes — a caller can wait on
/// it to KNOW the pump is subscribed before it acts (condition-based, vs a fixed sleep that races a
/// cold-start handshake and drops a fill that lands before subscribe). The exec adapter uses the
/// `_with_resync` twin + a fill-sentinel instead, so this readiness gate is for the manual smokes.
pub fn spawn_bybit_perp_user_data(
    ws_url: String,
    api_key: String,
    api_secret: String,
    symbol: String,
    events: EventSender,
    ready: Arc<AtomicBool>,
) -> UserDataFeed {
    spawn_pump(format!("bybit-userdata-{symbol}"), move |stop: Arc<AtomicBool>| {
        let _span = tracing::info_span!("user_data_pump", venue = "bybit", %symbol).entered();
        let now_ms = vike_model::clock::now_ms;
        let mut ping = |ws: &mut TungsteniteStream| {
            let _ = ws.send_text(&crate::ws_auth::ping_frame());
        };
        // Built ONCE per pump; the reconnect loop reuses this closure, so the gate persists
        // across reconnects and paces cumulative login/subscribe sends (spec §A keyed WS gate).
        let ws_gate = crate::ratelimit::ws_rate_gate();
        // Opt-in `execution.fast` fast-fill hint (VIKE_BYBIT_FAST_EXEC=1); unset ⇒ ["execution","order"].
        let topics = private_topics(fast_exec_enabled());
        run_user_data_forever_with_idle(
            || {
                let outcome = open_bybit_user_data_ws(
                    &ws_url,
                    &api_key,
                    &api_secret,
                    now_ms,
                    &topics,
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
            |frame| map_bybit_perp(frame, "bybit", &symbol),
            |event| events.blocking_send(event).is_ok(),
            &stop,
            POLL,
            Duration::from_secs(30),
            Some((PING_EVERY, &mut ping)),
            || {}, // no resync: use spawn_bybit_perp_user_data_with_resync to enable audit A3
            Some(IDLE_THRESHOLD),
        )
    })
}

/// Join half of the Bybit pump + its audit-A3 resync supervisor (see
/// [`spawn_bybit_perp_user_data_with_resync`]). Perp twin of the spot `BinanceUserDataResync`; thin
/// alias for the shared [`UserDataResyncFeed`](vike_bridge_core::user_data::UserDataResyncFeed).
pub type BybitUserDataResync = vike_bridge_core::user_data::UserDataResyncFeed;

/// Like [`spawn_bybit_perp_user_data`], plus the audit-A3 resync supervisor — the Bybit mirror of
/// the spot `spawn_binance_user_data_with_resync`.
/// On every WS re-open the pump bumps a generation counter, and the supervisor (after a settle
/// delay) replays recent history via `resync_fetch` so a fill/cancel/reject that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST
/// history source (typically `get_order_history` + `get_execution_history` →
/// [`map_bybit_history`](crate::history::map_bybit_history)) on a SEPARATE `BybitPerpRest`
/// from the order-submit client. Both threads tear down on `shutdown`; the supervisor also
/// self-exits if the pump dies or the core is gone. (Exercised by live smokes.)
///
/// `on_reconcile` (reconciliation-activation Task 7) is threaded straight to
/// `run_resync_supervisor`'s own `on_reconcile` param — `Some` pokes the reconcile driver after
/// every reconnect's event-replay settles; `None` (every non-recon caller) reproduces the
/// pre-Task-7 event-replay-only behavior byte-for-byte.
pub fn spawn_bybit_perp_user_data_with_resync(
    ws_url: String,
    api_key: String,
    api_secret: String,
    symbol: String,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
) -> BybitUserDataResync {
    let events_p = events.clone();
    let sym_p = symbol.clone();
    spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: format!("bybit-userdata-{symbol}"),
        resync_thread_name: format!("bybit-resync-{symbol}"),
        poll: POLL,
        settle: Duration::from_secs(1), // let the reopened WS drain its backlog first
        events,
        resync_fetch,
        on_reconcile,
        pump_body: move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
            let now_ms = vike_model::clock::now_ms;
            let mut ping = |ws: &mut TungsteniteStream| {
                let _ = ws.send_text(&crate::ws_auth::ping_frame());
            };
            // Built ONCE per pump; the reconnect loop reuses this closure, so the gate persists
            // across reconnects and paces cumulative login/subscribe sends (spec §A keyed WS gate).
            let ws_gate = crate::ratelimit::ws_rate_gate();
            // Opt-in `execution.fast` fast-fill hint (VIKE_BYBIT_FAST_EXEC=1); unset ⇒ ["execution","order"].
            let topics = private_topics(fast_exec_enabled());
            run_user_data_forever_with_idle(
                || {
                    open_bybit_user_data_ws(
                        &ws_url,
                        &api_key,
                        &api_secret,
                        now_ms,
                        &topics,
                        &stop,
                        &ws_gate,
                    )
                },
                |frame| map_bybit_perp(frame, "bybit", &sym_p),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// OFF path (the default): the subscribe list is byte-identical to today's two-topic list — no
    /// `execution.fast`, so a default build never subscribes the fast stream and the mapper's fast
    /// arm is never reached. This is the exact list the r6 `subscribe_frame` fixture pins.
    #[test]
    fn default_topics_unchanged_when_fast_exec_off() {
        assert_eq!(private_topics(false), vec!["execution", "order"]);
    }

    /// ON path: `execution.fast` is PREPENDED and `execution` is RETAINED (the fast stream augments,
    /// never replaces — the slow `execution` twin still carries execFee + the FSM terminal wrap).
    #[test]
    fn fast_exec_prepends_execution_fast_and_keeps_the_slow_twin() {
        assert_eq!(private_topics(true), vec!["execution.fast", "execution", "order"]);
    }
}
