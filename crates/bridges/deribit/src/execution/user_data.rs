//! Deribit private-WS fill stream. Port of `exec/deribit/user_data.py`: connect (no
//! browser UA needed; bounded by the exec lane's `dial_timeout`, as the order socket's is) → public/auth client_credentials (id-matched JSON-RPC ack, tokens
//! captured for the refresh ping) → private/subscribe `user.trades.any.any.raw` — the
//! `any`-kind, `any`-currency channel, so futures AND options fills across every currency
//! stream on ONE subscription (id-matched ack; sent AFTER a second, OPTIONAL `private/subscribe`
//! of `user.orders.any.any.raw` for a STOP the venue ends or fires on its own, see
//! `deribit_orders_channel`) → the venue-neutral pump with the venue's private mapper
//! (`crate::execution::event_mapper::map_deribit_private`: the fills, and the stops' rows). Keepalive = a
//! NON-blocking `public/test` ping every 20s, with the public/auth
//! refresh_token send folded into every 30th tick so it keeps its original 10-min cadence; errors
//! swallowed (a dead token self-heals via re-auth on reconnect).
//!
//! The pump is the shared one, `vike_bridge_core::websocket_session::user::spawn_user_with_resync`: this file
//! hands it the venue's open (the dial is `dial_user_stream`, each of the three JSON-RPC steps is
//! `vike_bridge_core::websocket::send_and_await` with an id-matched classify), the mapper, the keepalive,
//! the idle threshold and the fence; the threads, the span, the backoff and the replay are the
//! builder's.
//!
//! The 20s ping exists for the silent-stall watchdog (`IDLE_THRESHOLD`): its reply is the only
//! dependable inbound traffic on an idle account, since fills are the sole other source. Before it,
//! this venue's inbound cadence was the 600s refresh reply — too sparse for a useful threshold,
//! which is why deribit was the last venue left unarmed.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::ratelimit::RateGatePerKey;
use vike_bridge_core::websocket::{
    AckResult, WebsocketSessionUserStreamTungstenite, send_and_await,
};
use vike_execution::EventSender;
use vike_model::events::Event;
use vike_venues::venue_timings::DERIBIT;

use crate::execution::event_mapper::map_deribit_private;
use crate::transport::rpc::{JsonRpcBuilder, parse_response};
use crate::transport::ws_auth::{
    build_client_credentials_auth, build_private_subscribe, build_public_test,
    build_refresh_token_auth,
};
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserOpenOutcome, WebsocketSessionUserResyncHandle,
    WebsocketSessionUserResyncSpec, dial_user_stream, spawn_user_with_resync,
};

// The three timings below are this venue's exec-lane row in
// `crates/vike-venues/src/venue_timings.rs` (`DERIBIT`), which carries each value and its
// reason (decision 0160). The pump's poll, reconnect ceiling and resync settle are the venue-free
// mechanism defaults of `vike_bridge_core::websocket_session::user`, which its shared spawn runs on. The token refresh
// is NOT modelled in the table (it rides every 30th ping tick) and stays a local constant.

/// The dial's window: the TCP connect, and each read of the TLS + WS handshake behind it
/// (`vike_bridge_core::websocket_proxy::connect_websocket`'s module doc). The same row the ORDER socket's
/// `DeribitTransport::connect` reads.
const DIAL_TIMEOUT: Duration = DERIBIT.exec().dial_timeout();
/// Auth-ack plus subscribe-ack deadline, started once the socket is up.
const HANDSHAKE_TIMEOUT: Duration = DERIBIT.exec().handshake_timeout();
const REFRESH_EVERY: Duration = Duration::from_secs(600); // 10 min, under token lifetime
/// App-level ping cadence — a `public/test` whose reply is the lane's ONLY dependable inbound
/// traffic on an idle account (see [`build_public_test`]). The keepalive hook now fires at this
/// rate and folds the 600s token refresh into every 30th tick, so `REFRESH_EVERY` is unchanged in
/// effect.
const PING_EVERY: Duration = DERIBIT.exec().keepalive_every();
/// How many `PING_EVERY` ticks make up one `REFRESH_EVERY` — 600 / 20.
const TICKS_PER_REFRESH: u32 = (REFRESH_EVERY.as_secs() / PING_EVERY.as_secs()) as u32;
/// Silent-stall watchdog: no inbound frame of ANY kind for this long ⇒ the socket is dead behind an
/// open connection. 3x [`PING_EVERY`] — Deribit answers every `public/test`, so a healthy socket
/// delivers a frame every ~20s even on a totally idle account. Keyed off that server-answer
/// cadence, NOT data cadence: an account with no orders is legitimately silent.
///
/// This venue was the last one left unarmed: before the `public/test` ping its only inbound traffic
/// was the 600s refresh reply, which would have forced a useless ~40-minute threshold.
const IDLE_THRESHOLD: Duration = DERIBIT.exec().idle_threshold();

/// The pump's keepalive hook: a `public/test` ping every tick, plus the token refresh every
/// [`TICKS_PER_REFRESH`]th tick so the refresh keeps its original 600s cadence exactly. Built once per
/// pump, so the tick count (and the token the open captured) persist across reconnects.
fn keepalive_hook(
    builder: Arc<JsonRpcBuilder>,
    token_cell: Arc<Mutex<String>>,
) -> impl FnMut(&mut WebsocketSessionUserStreamTungstenite) + Send + 'static {
    let mut ticks: u32 = 0;
    move |ws: &mut WebsocketSessionUserStreamTungstenite| {
        // Liveness first: cheap, unauthenticated, and its reply is what the watchdog counts on.
        let _ = ws.send_text(&build_public_test(builder.next_id()).to_string());

        ticks += 1;
        if !ticks.is_multiple_of(TICKS_PER_REFRESH) {
            return;
        }
        let token = token_cell.lock().unwrap().clone();
        if token.is_empty() {
            return;
        }
        let frame = build_refresh_token_auth(&token, builder.next_id());
        let _ = ws.send_text(&frame.to_string()); // NEVER log (carries token)
    }
}

/// Deribit JSON-RPC error codes that are TRANSIENT (reconnect), not genuine auth failures:
/// 10028 = too_many_requests (rate limit). Genuine auth errors (e.g. 13009 unauthorized) are NOT
/// listed, so they still surface + stop the pump (audit A3).
///
/// Not in `vike_bridge_core::transport`'s `websocket_ack_is_transient`: that table is ONE flat union, exact only
/// because its venues' codes are disjoint, and deribit's five-digit `10xxx` codes share bybit's range, so a
/// deribit code there would reconnect bybit's handshake on the same number.
fn deribit_ack_is_transient(code: i64) -> bool {
    code == 10028
}

/// The Deribit `user.trades` fill channel we subscribe. We use the `any`-kind, `any`-currency
/// channel (`user.trades.any.any.raw`) so futures AND options fills — across BTC/ETH/SOL/USDC/… —
/// all stream on ONE subscription. The deribit exec client starts `BTC-PERPETUAL`, so a per-kind
/// (`future`) / per-currency channel would MISS every option fill (they'd surface only on the
/// reconcile poll). Deribit's `user.trades.{kind}.{currency}.{interval}` accepts
/// `kind ∈ {future, option, spot, future_combo, option_combo, any}` and
/// `currency ∈ {BTC, ETH, SOL, USDC, …, any}`; the `raw` interval streams each fill immediately.
/// Incoming option/future trade notifications share the same shape, so the mapper is unchanged.
fn deribit_trades_channel() -> String {
    "user.trades.any.any.raw".to_string()
}

/// The `user.orders` channel, subscribed beside the fills for ONE reason: a STOP the venue ends or
/// fires on its own (cancelled by someone else, or fired into an order the venue refused) leaves no
/// fill, no order-history row and no label hit, so this channel is the node's only word of it
/// ([`crate::execution::event_mapper::map_deribit_order_row`] reads the rows; it keeps to stops and the orders
/// they fire). Same `any`/`any`/`raw` spelling as [`deribit_trades_channel`]: every kind and
/// currency on one subscription. It streams every change of every order of the account and the
/// mapper drops all but the stop rows, which is cheap per frame and bounded by the account's own
/// order flow.
///
/// ⚠ **It is OPTIONAL, and the fills are not.** It is subscribed in a call of its own, BEFORE the
/// fills' ([`open_deribit_user_data_websocket`] says why that way round), and a venue ERROR for it only
/// warns: one mistyped or refused channel inside the fills' own `private/subscribe` would fail the
/// whole call as an auth error and stop the fill pump for good, which is a far worse state than a
/// stop that is not heard. (A socket that dies, times out or is stopped during that call still ends
/// the open, as for any frame.)
fn deribit_orders_channel() -> String {
    "user.orders.any.any.raw".to_string()
}

/// The venue's error object of a JSON-RPC reply, as `(code, message)`: `None` for a success (an
/// absent or `null` `error`). An absent code reads `0`, an absent message `""`.
fn rpc_error(error: Option<Value>) -> Option<(i64, String)> {
    let err = error.filter(|e| !e.is_null())?;
    let message = err.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
    Some((err.get("code").and_then(|c| c.as_i64()).unwrap_or(0), message))
}

/// The classify of a step that MUST succeed (the auth and the fills' subscribe): the reply to request
/// `rid` (every other frame, an interleaved notification or another reply, is not ours), its success
/// `Ok`, the venue's error the open's failure, auth unless it is the transient rate limit.
fn rpc_ack(rid: i64) -> impl Fn(&Value) -> AckResult {
    move |frame| {
        let (fid, _, error) = parse_response(frame);
        if fid != Some(rid) {
            return AckResult::NotOurs;
        }
        let Some((code, message)) = rpc_error(error) else { return AckResult::Ok };
        // NEVER include credentials
        let text = format!("Deribit auth failed: {message}");
        // audit A3: 10028 = too_many_requests (rate limit) is TRANSIENT → reconnect;
        // genuine auth errors (e.g. 13009 unauthorized) still surface + stop.
        if deribit_ack_is_transient(code) {
            AckResult::TransientErr(text)
        } else {
            AckResult::Err(text)
        }
    }
}

/// The classify of the OPTIONAL stops' subscribe ([`deribit_orders_channel`]): the reply to request
/// `rid` is `Ok` whatever it says, and a venue error for it is a warning only.
fn optional_rpc_ack(rid: i64) -> impl Fn(&Value) -> AckResult {
    move |frame| {
        let (fid, _, error) = parse_response(frame);
        if fid != Some(rid) {
            return AckResult::NotOurs;
        }
        if let Some((code, message)) = rpc_error(error) {
            tracing::warn!(
                target: "vike_deribit::execution::user_data",
                code,
                message,
                channel = %deribit_orders_channel(),
                "the venue refused the user.orders subscription; fills still stream, but a stop it \
                 ends or fires without a fill will not be heard (it stays Accepted until a reconcile)"
            );
        }
        AckResult::Ok
    }
}

/// Connect → auth (tokens into `token_cell`) → subscribe the stops' `user.orders` channel (optional:
/// a venue refusal only warns) → subscribe `channels` (the fills; required, and the last ack awaited).
/// Socket closed on any exit.
pub fn open_deribit_user_data_websocket(
    websocket_url: &str,
    client_id: &str,
    client_secret: &str,
    channels: &[String],
    builder: &JsonRpcBuilder,
    token_cell: &Mutex<String>,
    stop: &AtomicBool,
    gate: &RateGatePerKey,
) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite> {
    // Bounded like the order socket's dial: a bare `tungstenite::connect` waits for ever on a peer that
    // accepts the connection (or a route that black-holes the SYNs) and says nothing, and this thread
    // reads its `stop` flag only after the dial returns.
    let mut socket = match dial_user_stream(websocket_url, DIAL_TIMEOUT) {
        Ok(socket) => socket,
        Err(words) => return WebsocketSessionUserOpenOutcome::Transport(words),
    };
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut step = |key: &str, frame: Value, word: &str, classify: &dyn Fn(&Value) -> AckResult| {
        send_and_await(
            &mut socket,
            Some(gate),
            key,
            &frame.to_string(),
            word,
            stop,
            deadline,
            "Deribit WS",
            classify,
        )
    };

    let auth_id = builder.next_id();
    let auth = build_client_credentials_auth(client_id, client_secret, None, auth_id);
    let reply = match step("login", auth, "auth", &rpc_ack(auth_id)) {
        Ok(reply) => reply,
        Err(outcome) => return *outcome,
    };
    // capture the refresh token for the keepalive (NEVER logged)
    *token_cell.lock().unwrap() = parse_response(&reply)
        .1
        .as_ref()
        .and_then(|result| result.get("refresh_token"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    // The stops' own channel FIRST, in a call of its own, so that (a) a refusal of it cannot cost the
    // fills (see [`deribit_orders_channel`]: a venue error is a warning here, a socket failure is
    // not) and (b) the fills' ack stays the LAST thing the open waits for, as it always was: the
    // wait for an ack skips every notification that is not the reply, so a notification of the
    // fills' own subscription must not be able to arrive during another call's wait.
    let orders_id = builder.next_id();
    let orders = build_private_subscribe(&[deribit_orders_channel()], orders_id);
    if let Err(outcome) = step("subscribe", orders, "subscribe", &optional_rpc_ack(orders_id)) {
        return *outcome;
    }

    let sub_id = builder.next_id();
    let sub = build_private_subscribe(channels, sub_id);
    if let Err(outcome) = step("subscribe", sub, "subscribe", &rpc_ack(sub_id)) {
        return *outcome;
    }
    WebsocketSessionUserOpenOutcome::Ready(WebsocketSessionUserStreamTungstenite(socket))
}

/// Spawn the persistent Deribit fill-stream pump feeding the th-core ingest, with the audit-A3 resync
/// supervisor, on the shared `spawn_user_with_resync`.
/// On every WS re-open the pump bumps a generation counter, and the supervisor (after a settle
/// delay) replays recent history via `resync_fetch` so a fill/cancel/reject that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's WS-RPC
/// history source (typically `get_order_history` + `get_user_trades` →
/// [`map_deribit_history`](crate::execution::resync::map_deribit_history)) on a SEPARATE Deribit
/// client from the order-submit path; a caller with no replay (a smoke) passes `Vec::new`. Both
/// threads tear down on `shutdown`; the supervisor also self-exits if the pump dies or the core is
/// gone.
///
/// ⚠ BOTH producers of this function go through `fence`, never straight to `events`: the pump's sink
/// is `fence.deliver`, and every batch `resync_fetch` returns passes `fence.hold` (through
/// `SubmitFence::fenced_fetch`) before the supervisor sees it. An event of an order whose submit is
/// still in flight must wait behind that order's own `OrderSubmitted`/`OrderAccepted`
/// (`vike_bridge_core::exec_actor::submit_fence`'s module doc: the replay can run while the submit is
/// open when one blip drops both sockets). `fence` must wrap the SAME lane as `events`, and the exec
/// thread's `VenueRestOrderSubmitFence` must wrap the same `fence`.
pub fn spawn_deribit_user_data_with_resync(
    websocket_url: String,
    client_id: String,
    client_secret: String,
    symbol: String,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
) -> WebsocketSessionUserResyncHandle {
    // Built once per pump, so each persists across reconnects: the request ids, the refresh token the
    // open captures for the keepalive, and the keyed WS gate (spec §A).
    let builder = Arc::new(JsonRpcBuilder::new());
    let token_cell = Arc::new(Mutex::new(String::new()));
    let websocket_gate = crate::transport::ratelimit::websocket_rate_gate();
    let channels = vec![deribit_trades_channel()];
    let keepalive: Box<dyn FnMut(&mut WebsocketSessionUserStreamTungstenite) + Send> =
        Box::new(keepalive_hook(Arc::clone(&builder), Arc::clone(&token_cell)));
    let decode_symbol = symbol.clone();
    spawn_user_with_resync(WebsocketSessionUserResyncSpec {
        venue: "deribit",
        lane: "deribit",
        symbol,
        events,
        fence: Some(fence),
        resync_fetch,
        on_reconcile: None, // Task 13: no reconcile trigger wired yet — event-replay only
        open: move |stop: &AtomicBool| {
            open_deribit_user_data_websocket(
                &websocket_url,
                &client_id,
                &client_secret,
                &channels,
                &builder,
                &token_cell,
                stop,
                &websocket_gate,
            )
        },
        decode: move |frame: &Value| map_deribit_private(frame, "deribit", &decode_symbol),
        keepalive: Some((PING_EVERY, keepalive)),
        idle_threshold: Some(IDLE_THRESHOLD),
    })
}

#[cfg(test)]
mod channel_tests {
    use super::{deribit_orders_channel, deribit_trades_channel};

    #[test]
    fn subscribes_any_kind_any_currency() {
        // `any` kind + `any` currency: futures AND options fills, every currency, one subscription.
        // (The exec client starts BTC-PERPETUAL, so a per-kind/per-currency channel would drop
        // every option fill — they'd only surface on the reconcile poll.)
        assert_eq!(deribit_trades_channel(), "user.trades.any.any.raw");
        // The stops' channel spells the same way: every kind and currency on one subscription (a
        // stop can sit on any perpetual the account trades, not only the mounted one).
        assert_eq!(deribit_orders_channel(), "user.orders.any.any.raw");
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    use vike_bridge_core::websocket_session::user::{
        RESYNC_SETTLE, USER_DATA_MAX_BACKOFF, USER_DATA_POLL,
    };

    /// The EFFECTIVE timings the pump runs on, each a literal this file held before the venue
    /// timings table existed (decision 0160): evidence beside the table's own matrix pin, so a row
    /// edit that moves deribit's liveness contract fails here too and is a deliberate act in both.
    #[test]
    fn the_pump_runs_on_the_timings_it_always_did() {
        assert_eq!(HANDSHAKE_TIMEOUT, Duration::from_secs(10));
        assert_eq!(PING_EVERY, Duration::from_secs(20));
        assert_eq!(IDLE_THRESHOLD, Duration::from_secs(60));
        assert_eq!(USER_DATA_POLL, Duration::from_secs(1));
        assert_eq!(USER_DATA_MAX_BACKOFF, Duration::from_secs(30));
        assert_eq!(RESYNC_SETTLE, Duration::from_secs(1));
    }

    /// The token refresh is NOT in the table and rides every 30th ping tick (600 s over 20 s), so a
    /// retuned `keepalive_every` would silently move the refresh cadence unless this count is held.
    #[test]
    fn the_token_refresh_still_rides_every_thirtieth_ping() {
        assert_eq!(REFRESH_EVERY, Duration::from_secs(600));
        assert_eq!(TICKS_PER_REFRESH, 30);
    }

    /// The watchdog must outlast a late reply: three ping periods (the figure the `IDLE_THRESHOLD`
    /// doc and the row's `why` argue), so a healthy idle socket is never torn down.
    #[test]
    fn the_idle_threshold_is_three_ping_periods() {
        assert_eq!(IDLE_THRESHOLD, PING_EVERY * 3);
    }
}
