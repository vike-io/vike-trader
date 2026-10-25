//! Bybit private-WS session. Port of `exec/bybit/user_data.py`: connect → auth frame
//! (never logged) → op=='auth' ack → subscribe frame → op=='subscribe' ack → hand the
//! socket to the venue-neutral pump, decoding with the PERP mapper ([`perp_frame_decoder`]: the
//! mapper plus a one-terminal-per-order memory) and feeding the vt-core ingest losslessly. App-level `{"op":"ping"}` keepalive every 20s (Bybit's
//! server does not WS-protocol-ping like Binance).
//!
//! Fast-fill hint (opt-in `venue.bybit.fast_exec = 1`): when on, `execution.fast` is PREPENDED to
//! the private-topic subscribe list ([`private_topics`]) — Bybit's low-latency fill stream on the
//! SAME authed socket. `execution` is ALWAYS retained: the fast stream AUGMENTS, never replaces it
//! (the slow twin carries execFee + the terminal wrap). The perp mapper's
//! [`crate::event_mapper::map_execution_fast`] turns each fast row into an EARLY bare Fill; dedup by
//! execId (the engine's `seen_trade_ids`) books it exactly once. Off ⇒ the historical
//! `["execution", "order"]` list, byte-identical (the mapper's fast arm is inert with no fast frames).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::ratelimit::KeyedRateGate;
use vike_bridge_core::ws::{TungsteniteStream, await_ack, configure_ws_stream, send_gated};
use vike_exec::EventSender;
use vike_model::events::{Event, OrderCanceled, OrderExpired};
use vike_venues::venue_timings::BYBIT;

use crate::event_mapper::map_bybit_perp;
use crate::ws_auth::{build_auth_frame, build_subscribe_frame, match_op_ack};
use vike_bridge_core::user_data::{
    OpenOutcome, RESYNC_SETTLE, ReconnectHook, ResyncPumpSpec, USER_DATA_MAX_BACKOFF,
    USER_DATA_POLL, UserDataFeed, UserDataResyncFeed, run_user_data_forever_with_idle, spawn_pump,
    spawn_pump_with_resync,
};

// The three timings below are this venue's exec-lane row in
// `crates/vike-venues/src/venue_timings.rs` (`BYBIT`), which carries each value and its
// reason (decision 0160). The pump's poll, reconnect ceiling and resync settle are the venue-free
// mechanism defaults of `vike_bridge_core::user_data` and are read from there.

/// Auth-ack plus subscribe-ack deadline, started once the socket is up.
const HANDSHAKE_TIMEOUT: Duration = BYBIT.exec().handshake_timeout();
/// The app-level `{"op":"ping"}` cadence: Bybit's server does not protocol-ping.
const PING_EVERY: Duration = BYBIT.exec().keepalive_every();
/// Silent-stall watchdog: no inbound frame of ANY kind for this long ⇒ the socket is dead behind
/// an open connection, so end the session and let the reconnect + audit-A3 resync repair it.
/// 3x [`PING_EVERY`] — Bybit answers each app-level `{"op":"ping"}` with a pong, so a healthy
/// socket delivers an inbound frame every ~20s even on a totally idle account. Keyed off that
/// server-answer cadence, NOT data cadence: an account with no orders is legitimately silent.
const IDLE_THRESHOLD: Duration = BYBIT.exec().idle_threshold();

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

/// Whether the `execution.fast` hint is on: the exact `"1"` (trimmed) of `venue.bybit.fast_exec` in
/// the venue's settings. Read ONCE, by the start, and handed to the pump as a `bool`. (It was the
/// `VIKE_BYBIT_FAST_EXEC` process-environment read until decision 0095 — a set variable refuses
/// startup now — and then a credential-map name the settings store folded the row under, until
/// that decision's Task 7 retired the fold.)
pub fn fast_exec(settings: &vike_secrets::venue_setting::VenueSettings) -> bool {
    settings.get(vike_secrets::venue_setting::SettingTier::Any, "fast_exec").map(str::trim)
        == Some("1")
}

/// How many cancelled or expired orders [`perp_frame_decoder`] remembers. The venue's second row for
/// a cancelled conditional order lands within milliseconds of the first, so the memory only has to
/// outlast that gap; it is a ring (the oldest id goes first), never a growing set.
pub const REPORTED_TERMINALS_KEPT: usize = 256;

/// The decoder the private-WS pump runs on every frame: `map_bybit_perp`, minus a SECOND cancel or
/// expiry for an order this decoder already reported one for.
///
/// Bybit answers a cancelled CONDITIONAL order with a `Cancelled` row and then a `Deactivated` row
/// (measured on the demo host, 2026-10-24), which the mapper reads as `OrderCanceled` then
/// `OrderExpired`: two terminal events for one cancel. The engine drops the second without a counter
/// (the order row is right), but the journal and the CLI's outcome line carried both. The first
/// terminal wins, whichever word it is, so a lone `Deactivated` (a stop that expired, or one the venue
/// pulled by itself) is still `OrderExpired`, and a `Deactivated` that comes first is not followed by
/// a cancel either. A row with no `orderLinkId` names no order and is never held back. Fills are not
/// touched: their replay is the engine's `execId` dedup.
///
/// Built ONCE per pump and kept across its reconnects. The audit-A3 resync replay does not pass
/// through it (the history path keeps no memory), so a terminal the replay repeats after a reconnect
/// is still the engine's to drop.
pub fn perp_frame_decoder(symbol: &str) -> impl Fn(&Value) -> Vec<Event> {
    let symbol = symbol.to_owned();
    let reported = RefCell::new(VecDeque::<String>::new());
    move |frame| {
        let mut events = map_bybit_perp(frame, "bybit", &symbol);
        events.retain(|event| {
            let (Event::OrderCanceled(OrderCanceled { client_order_id: coid, .. })
            | Event::OrderExpired(OrderExpired { client_order_id: coid, .. })) = event
            else {
                return true;
            };
            if coid.is_empty() {
                return true;
            }
            let mut reported = reported.borrow_mut();
            if reported.contains(coid) {
                return false;
            }
            if reported.len() == REPORTED_TERMINALS_KEPT {
                reported.pop_front();
            }
            reported.push_back(coid.clone());
            true
        });
        events
    }
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
    configure_ws_stream(&socket, USER_DATA_POLL);
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
///
/// `fast_exec` is the `venue.bybit.fast_exec` setting — the caller reads it (the start through
/// [`fast_exec`]); `false` keeps the two-topic subscription.
pub fn spawn_bybit_perp_user_data(
    ws_url: String,
    api_key: String,
    api_secret: String,
    symbol: String,
    events: EventSender,
    ready: Arc<AtomicBool>,
    fast_exec: bool,
) -> UserDataFeed {
    spawn_pump(format!("bybit-userdata-{symbol}"), move |stop: Arc<AtomicBool>| {
        let _span = tracing::info_span!("user_data_pump", venue = "bybit", %symbol).entered();
        let now_ms = vike_model::now_ms;
        let mut ping = |ws: &mut TungsteniteStream| {
            let _ = ws.send_text(&crate::ws_auth::ping_frame());
        };
        // Built ONCE per pump; the reconnect loop reuses this closure, so the gate persists
        // across reconnects and paces cumulative login/subscribe sends (spec §A keyed WS gate).
        let ws_gate = crate::ratelimit::ws_rate_gate();
        // Opt-in `execution.fast` fast-fill hint (`venue.bybit.fast_exec`);
        // off ⇒ ["execution","order"].
        let topics = private_topics(fast_exec);
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
            perp_frame_decoder(&symbol),
            |event| events.blocking_send(event).is_ok(),
            &stop,
            USER_DATA_POLL,
            USER_DATA_MAX_BACKOFF,
            Some((PING_EVERY, &mut ping)),
            || {}, // no resync: use spawn_bybit_perp_user_data_with_resync to enable audit A3
            Some(IDLE_THRESHOLD),
        )
    })
}

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
/// `run_resync_supervisor`'s own `on_reconcile` param — `Some` fires the recon trigger after
/// every reconnect's event-replay settles; `None` (every non-recon caller) reproduces the
/// pre-Task-7 event-replay-only behavior byte-for-byte.
///
/// `fast_exec` is the `venue.bybit.fast_exec` setting, as in [`spawn_bybit_perp_user_data`].
///
/// ⚠ BOTH producers of this function go through `fence`, never straight to `events`: the pump's sink is
/// `fence.deliver` and every batch `resync_fetch` returns passes `fence.hold` (`fenced_fetch`). The
/// `execution` topic can push an order's fill before the `order` topic's `New` row and before the REST
/// answer, so a fill of an order whose submit is still in flight waits behind that order's own
/// `OrderSubmitted`/`OrderAccepted` (`vike_bridge_core::exec_actor::submit_fence`'s module doc).
/// `fence` must wrap the SAME lane as `events`, and the exec thread's `FencedRest` must wrap the same
/// `fence`.
pub fn spawn_bybit_perp_user_data_with_resync(
    ws_url: String,
    api_key: String,
    api_secret: String,
    symbol: String,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
    fast_exec: bool,
) -> UserDataResyncFeed {
    let sym_p = symbol.clone();
    spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: format!("bybit-userdata-{symbol}"),
        resync_thread_name: format!("bybit-resync-{symbol}"),
        poll: USER_DATA_POLL,
        settle: RESYNC_SETTLE,
        events,
        resync_fetch: fence.fenced_fetch(resync_fetch),
        on_reconcile,
        pump_body: move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
            let now_ms = vike_model::now_ms;
            let mut ping = |ws: &mut TungsteniteStream| {
                let _ = ws.send_text(&crate::ws_auth::ping_frame());
            };
            // Built ONCE per pump; the reconnect loop reuses this closure, so the gate persists
            // across reconnects and paces cumulative login/subscribe sends (spec §A keyed WS gate).
            let ws_gate = crate::ratelimit::ws_rate_gate();
            // Opt-in `execution.fast` fast-fill hint (`venue.bybit.fast_exec`);
            // off ⇒ ["execution","order"].
            let topics = private_topics(fast_exec);
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
                perp_frame_decoder(&sym_p),
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

    /// Decision 0095: the fast-fill hint is the exact `1` of `venue.bybit.fast_exec`, read from the
    /// venue's settings (Task 7 retired the credential-map fold that carried it) — never the
    /// process environment.
    #[test]
    fn fast_exec_reads_its_row_exactly() {
        use vike_secrets::venue_setting::VenueSettings;
        let settings = |v: &str| {
            VenueSettings::from_rows(
                "bybit",
                &[vike_secrets::VenueSettingRow {
                    venue: "bybit".to_string(),
                    tier: None,
                    field: "FAST_EXEC".to_string(),
                    value: v.to_string(),
                }],
            )
        };
        assert!(fast_exec(&settings("1")));
        assert!(fast_exec(&settings(" 1 ")));
        for off in ["0", "true", ""] {
            assert!(!fast_exec(&settings(off)), "{off:?}");
        }
        assert!(!fast_exec(&VenueSettings::default()), "no row ⇒ OFF");
    }

    /// The EFFECTIVE timings the pump runs on, each a literal this file held before the venue
    /// timings table existed (decision 0160): evidence beside the table's own matrix pin, so a row
    /// edit that moves bybit's liveness contract fails here too and is a deliberate act in both.
    #[test]
    fn the_pump_runs_on_the_timings_it_always_did() {
        assert_eq!(HANDSHAKE_TIMEOUT, Duration::from_secs(10));
        assert_eq!(PING_EVERY, Duration::from_secs(20));
        assert_eq!(IDLE_THRESHOLD, Duration::from_secs(60));
        assert_eq!(USER_DATA_POLL, Duration::from_secs(1));
        assert_eq!(USER_DATA_MAX_BACKOFF, Duration::from_secs(30));
        assert_eq!(RESYNC_SETTLE, Duration::from_secs(1));
    }

    /// The watchdog must outlast a late pong: three ping periods (the figure the `IDLE_THRESHOLD`
    /// doc and the row's `why` argue), so a healthy idle socket is never torn down.
    #[test]
    fn the_idle_threshold_is_three_ping_periods() {
        assert_eq!(IDLE_THRESHOLD, PING_EVERY * 3);
    }
}
