//! Bybit private-WS session. Port of `exec/bybit/user_data.py`: connect → auth frame
//! (never logged) → op=='auth' ack → subscribe frame → op=='subscribe' ack → hand the
//! socket to the venue-neutral pump, decoding with the PERP mapper ([`perp_frame_decoder`]: the
//! mapper plus a one-terminal-per-order memory) and feeding the th-core ingest losslessly. App-level `{"op":"ping"}` keepalive every 20s (Bybit's
//! server does not WS-protocol-ping like Binance).
//!
//! Fast-fill hint (opt-in `venue.bybit.fast_exec = 1`): when on, `execution.fast` is PREPENDED to
//! the private-topic subscribe list ([`private_topics`]) — Bybit's low-latency fill stream on the
//! SAME authed socket. `execution` is ALWAYS retained: the fast stream AUGMENTS, never replaces it
//! (the slow twin carries execFee + the terminal wrap). The perp mapper's
//! [`crate::execution::event_mapper::map_execution_fast`] turns each fast row into an EARLY bare Fill; dedup by
//! execId (the engine's `seen_trade_ids`) books it exactly once. Off ⇒ the historical
//! `["execution", "order"]` list, byte-identical (the mapper's fast arm is inert with no fast frames).

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::ratelimit::RateGatePerKey;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::websocket::{WebsocketSessionUserStreamTungstenite, send_and_await};
use vike_bridge_core::websocket_session::spec::CONNECT_10S;
use vike_execution::EventSender;
use vike_model::events::{Event, OrderCanceled};
use vike_venues::venue_timings::BYBIT;

use crate::execution::event_mapper::map_bybit_perp;
use crate::history::Category;
use crate::transport::ws_auth::{build_auth_frame, build_subscribe_frame, match_op_ack};
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserOpenOutcome, WebsocketSessionUserResyncHandle,
    WebsocketSessionUserResyncSpec, dial_user_stream, spawn_user_with_resync,
};

// The three timings below are this venue's exec-lane row in
// `crates/vike-venues/src/venue_timings.rs` (`BYBIT`), which carries each value and its
// reason (decision 0160). The pump's poll, reconnect ceiling and resync settle are the venue-free
// mechanism defaults of `vike_bridge_core::websocket_session::user` and are read from there.

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
/// `fast_execution` is true; otherwise the historical `["execution", "order"]`. `execution` is ALWAYS
/// present — the fast stream augments it (the slow twin carries execFee + the terminal wrap the FSM
/// folds), never replaces it. Pure (takes the resolved flag) so it is unit-testable without env.
fn private_topics(fast_execution: bool) -> Vec<&'static str> {
    if fast_execution {
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
pub fn fast_execution(settings: &vike_secrets::venue_setting::VenueSettings) -> bool {
    settings.get(vike_secrets::venue_setting::SettingTier::Any, "fast_exec").map(str::trim)
        == Some("1")
}

/// How many cancelled orders [`perp_frame_decoder`] remembers. The venue's second row for a cancelled
/// conditional order lands within milliseconds of the first, so the memory only has to outlast that
/// gap; it is a ring (the oldest id goes first), never a growing set.
pub const REPORTED_TERMINALS_KEPT: usize = 256;

/// Whether a private-topic row is in a book the account mounted on `symbol` trades: an inverse mount trades
/// the `inverse` book alone, every other mount every book but `inverse` (its linear contracts and its spot
/// pairs; the door and `execution/mod.rs`'s `coin_fence` keep each engine to its own kind). A row that names no
/// `category` is kept, as every row was before.
fn in_the_mounts_book(row: &Value, inverse_mount: bool) -> bool {
    row.get("category")
        .and_then(Value::as_str)
        .is_none_or(|category| (category == Category::Inverse.wire()) == inverse_mount)
}

/// `frame` without the rows of the other book ([`in_the_mounts_book`]); borrowed, with no copy, when it has none.
fn rows_of_the_mounts_book(frame: &Value, inverse_mount: bool) -> Cow<'_, Value> {
    let foreign = |row: &Value| !in_the_mounts_book(row, inverse_mount);
    if !frame.get("data").and_then(Value::as_array).is_some_and(|rows| rows.iter().any(foreign)) {
        return Cow::Borrowed(frame);
    }
    let mut ours = frame.clone();
    if let Some(Value::Array(rows)) = ours.get_mut("data") {
        rows.retain(|row| !foreign(row));
    }
    Cow::Owned(ours)
}

/// The decoder the private-WS pump runs on every frame: `map_bybit_perp` over the rows of the mount's own
/// book, minus a SECOND cancel for an order this decoder already reported one for.
///
/// ⚠ **The venue's private socket carries every order and execution of the KEY, in every book**, and two
/// accounts may run on one key (the CI box's `DEMO`, linear, and `INV`, inverse BTCUSD: `vike-cli secrets account
/// copy-credentials`). A row of the other book is the other account's (or a hand order there), so this
/// pump drops it before the mapper (`rows_of_the_mounts_book`): before, `INV`'s pump handed the core
/// `DEMO`'s fills and terminals too (measured on the CI box: `ac8916990`, 2026-11-09, `INV`'s copy of the fill
/// 13 ms ahead of `DEMO`'s own; `pd4bystp1`, 2026-11-10, a second terminal). The book is the mounted NAME's
/// (`execution/mod.rs`'s `named_mount_book`, which the confirm client of the same mount reads too). Two accounts in ONE
/// book on one key are not told apart here.
///
/// Bybit answered a cancelled CONDITIONAL order with a `Cancelled` row and then a `Deactivated` row
/// (measured on the demo host, 2026-10-24; a cancelled stop got `Deactivated` alone on 2026-10-25), and
/// both are a cancel ([`crate::execution::event_mapper::map_order`]): one terminal is handed over, not two. A row
/// with no `orderLinkId` names no order and is never held back. Fills are not touched: their replay is
/// the engine's `execId` dedup.
///
/// Built ONCE per pump and kept across its reconnects. The audit-A3 resync replay does not pass
/// through the ring (the history path keeps no memory), so a terminal the replay repeats after a
/// reconnect is still the gatewayhub's and the engine's to drop.
pub fn perp_frame_decoder(symbol: &str) -> impl Fn(&Value) -> Vec<Event> + use<> {
    let symbol = symbol.to_owned();
    let inverse_mount = crate::execution::named_mount_book(&symbol) == Category::Inverse;
    let reported = RefCell::new(VecDeque::<String>::new());
    move |frame| {
        let frame = rows_of_the_mounts_book(frame, inverse_mount);
        let mut events = map_bybit_perp(&frame, "bybit", &symbol);
        events.retain(|event| {
            let Event::OrderCanceled(OrderCanceled { order_client_id, .. }) = event else {
                return true;
            };
            if order_client_id.is_empty() {
                return true;
            }
            let mut reported = reported.borrow_mut();
            if reported.contains(order_client_id) {
                return false;
            }
            if reported.len() == REPORTED_TERMINALS_KEPT {
                reported.pop_front();
            }
            reported.push_back(order_client_id.clone());
            true
        });
        events
    }
}

/// Connect → auth → ack → subscribe → ack, each step one shared login step
/// (`vike_bridge_core::websocket::send_and_await`, which closes the socket on ANY handshake exit), on a socket the shared
/// dial set up (`vike_bridge_core::websocket_session::user::dial_user_stream`: the read poll and no Nagle).
pub fn open_bybit_user_data_websocket(
    websocket_url: &str,
    api_key: &str,
    api_secret: &str,
    now_ms: impl Fn() -> i64,
    topics: &[&str],
    stop: &AtomicBool,
    gate: &RateGatePerKey,
) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite> {
    let mut socket = match dial_user_stream(websocket_url, CONNECT_10S) {
        Ok(socket) => socket,
        Err(e) => return WebsocketSessionUserOpenOutcome::Transport(e),
    };
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;

    // auth — NEVER log the frame (api_key + signature)
    let auth = build_auth_frame(api_key, api_secret, now_ms());
    if let Err(outcome) = send_and_await(
        &mut socket,
        Some(gate),
        "login",
        &auth.to_string(),
        "auth",
        stop,
        deadline,
        "Bybit WS auth",
        |f| match_op_ack(f, "auth"),
    ) {
        return *outcome;
    }

    // subscribe
    let sub = build_subscribe_frame(topics);
    if let Err(outcome) = send_and_await(
        &mut socket,
        Some(gate),
        "subscribe",
        &sub.to_string(),
        "subscribe",
        stop,
        deadline,
        "Bybit WS subscribe",
        |f| match_op_ack(f, "subscribe"),
    ) {
        return *outcome;
    }
    WebsocketSessionUserOpenOutcome::Ready(WebsocketSessionUserStreamTungstenite(socket))
}

/// Spawn the persistent Bybit perp fill-stream pump feeding the th-core ingest, for the MANUAL smokes: the same pump
/// as [`spawn_bybit_perp_user_data_with_resync`] with no replay, no fence (the lane takes every event as it comes) and
/// no reconcile trigger.
///
/// `ready` is set `true` the first time the auth+subscribe handshake completes — a caller can wait on
/// it to KNOW the pump is subscribed before it acts (condition-based, vs a fixed sleep that races a
/// cold-start handshake and drops a fill that lands before subscribe). The exec adapter uses the
/// `_with_resync` twin + a fill-sentinel instead, so this readiness gate is for the manual smokes.
///
/// `fast_execution` is the `venue.bybit.fast_exec` setting — the caller reads it (the start through
/// [`fast_execution`]); `false` keeps the two-topic subscription.
pub fn spawn_bybit_perp_user_data(
    websocket_url: String,
    api_key: String,
    api_secret: String,
    symbol: String,
    events: EventSender,
    ready: Arc<AtomicBool>,
    fast_execution: bool,
) -> WebsocketSessionUserResyncHandle {
    spawn_perp(
        websocket_url,
        api_key,
        api_secret,
        symbol,
        events,
        None,
        Vec::new,
        None,
        fast_execution,
        Some(ready),
        VenueClock::default(),
    )
}

/// Like [`spawn_bybit_perp_user_data`], plus the audit-A3 resync supervisor — the Bybit mirror of
/// the spot `spawn_binance_user_data_with_resync`.
/// On every WS re-open the pump bumps a generation counter, and the supervisor (after a settle
/// delay) replays recent history via `resync_fetch` so a fill/cancel/reject that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST
/// history source (typically `get_order_history` + `get_execution_history` →
/// [`map_bybit_history`](crate::execution::resync::map_bybit_history)) on a SEPARATE `VenueRestBybit`
/// from the order-submit client. Both threads tear down on `shutdown`; the supervisor also
/// self-exits if the pump dies or the core is gone. (Exercised by live smokes.)
///
/// `on_reconcile` (reconciliation-activation Task 7) is threaded straight to
/// `run_user_resync`'s own `on_reconcile` param — `Some` fires the recon trigger after
/// every reconnect's event-replay settles; `None` (every non-recon caller) reproduces the
/// pre-Task-7 event-replay-only behavior byte-for-byte.
///
/// `fast_execution` is the `venue.bybit.fast_exec` setting, as in [`spawn_bybit_perp_user_data`].
///
/// ⚠ BOTH producers of this function go through `fence`, never straight to `events`: the pump's sink is
/// `fence.deliver` and every batch `resync_fetch` returns passes `fence.hold` (`fenced_fetch`). The
/// `execution` topic can push an order's fill before the `order` topic's `New` row and before the REST
/// answer, so a fill of an order whose submit is still in flight waits behind that order's own
/// `OrderSubmitted`/`OrderAccepted` (`vike_bridge_core::exec_actor::submit_fence`'s module doc).
/// `fence` must wrap the SAME lane as `events`, and the exec thread's `VenueRestOrderSubmitFence` must wrap the same
/// `fence`.
pub fn spawn_bybit_perp_user_data_with_resync(
    websocket_url: String,
    api_key: String,
    api_secret: String,
    symbol: String,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
    fast_execution: bool,
    clock: VenueClock,
) -> WebsocketSessionUserResyncHandle {
    spawn_perp(
        websocket_url,
        api_key,
        api_secret,
        symbol,
        events,
        Some(fence),
        resync_fetch,
        on_reconcile,
        fast_execution,
        None,
        clock,
    )
}

/// The one bybit private stream, as `vike_bridge_core::websocket_session::user::spawn_user_with_resync` runs it: this venue's open,
/// decoder ([`perp_frame_decoder`]), app-level `{"op":"ping"}` keepalive and idle threshold. `ready`, when given, is
/// latched the first time a handshake completes. The login's `expires` is stamped on `clock` (owner ruling B-4: the
/// start's signing clock, read by its REST clients; the local clock until one answers).
#[expect(clippy::too_many_arguments)]
fn spawn_perp(
    websocket_url: String,
    api_key: String,
    api_secret: String,
    symbol: String,
    events: EventSender,
    fence: Option<Arc<SubmitFence>>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
    fast_execution: bool,
    ready: Option<Arc<AtomicBool>>,
    clock: VenueClock,
) -> WebsocketSessionUserResyncHandle {
    // Built ONCE per pump; the reconnect loop reuses the open, so the gate persists across reconnects and paces
    // cumulative login/subscribe sends (spec §A keyed WS gate).
    let websocket_gate = crate::transport::ratelimit::websocket_rate_gate();
    // Opt-in `execution.fast` fast-fill hint (`venue.bybit.fast_exec`); off ⇒ ["execution","order"].
    let topics = private_topics(fast_execution);
    let decode = perp_frame_decoder(&symbol);
    spawn_user_with_resync(WebsocketSessionUserResyncSpec {
        venue: "bybit",
        lane: "bybit",
        symbol,
        events,
        fence,
        resync_fetch,
        on_reconcile,
        open: move |stop: &AtomicBool| {
            let outcome = open_bybit_user_data_websocket(
                &websocket_url,
                &api_key,
                &api_secret,
                clock.ms(),
                &topics,
                stop,
                &websocket_gate,
            );
            // Subscribe acked → the pump can now receive fills. Latch ready so a waiter proceeds only once a
            // fill can't be missed.
            if let (Some(ready), WebsocketSessionUserOpenOutcome::Ready(_)) = (&ready, &outcome) {
                ready.store(true, Ordering::Release);
            }
            outcome
        },
        decode,
        keepalive: Some((
            PING_EVERY,
            Box::new(|ws: &mut WebsocketSessionUserStreamTungstenite| {
                let _ = ws.send_text(&crate::transport::ws_auth::ping_frame());
            }),
        )),
        idle_threshold: Some(IDLE_THRESHOLD),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OFF path (the default): the subscribe list is byte-identical to today's two-topic list — no
    /// `execution.fast`, so a default build never subscribes the fast stream and the mapper's fast
    /// arm is never reached. This is the exact list the r6 `subscribe_frame` fixture pins.
    #[test]
    fn default_topics_unchanged_when_fast_execution_off() {
        assert_eq!(private_topics(false), vec!["execution", "order"]);
    }

    /// ON path: `execution.fast` is PREPENDED and `execution` is RETAINED (the fast stream augments,
    /// never replaces — the slow `execution` twin still carries execFee + the FSM terminal wrap).
    #[test]
    fn fast_execution_prepends_execution_fast_and_keeps_the_slow_twin() {
        assert_eq!(private_topics(true), vec!["execution.fast", "execution", "order"]);
    }

    /// Decision 0095: the fast-fill hint is the exact `1` of `venue.bybit.fast_exec`, read from the
    /// venue's settings (Task 7 retired the credential-map fold that carried it) — never the
    /// process environment.
    #[test]
    fn fast_execution_reads_its_row_exactly() {
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
        assert!(fast_execution(&settings("1")));
        assert!(fast_execution(&settings(" 1 ")));
        for off in ["0", "true", ""] {
            assert!(!fast_execution(&settings(off)), "{off:?}");
        }
        assert!(!fast_execution(&VenueSettings::default()), "no row ⇒ OFF");
    }

    /// The EFFECTIVE timings the pump runs on, each a literal this file held before the venue
    /// timings table existed (decision 0160): evidence beside the table's own matrix pin, so a row
    /// edit that moves bybit's liveness contract fails here too and is a deliberate act in both.
    #[test]
    fn the_pump_runs_on_the_timings_it_always_did() {
        use vike_bridge_core::websocket_session::user::{
            RESYNC_SETTLE, USER_DATA_MAX_BACKOFF, USER_DATA_POLL,
        };
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
