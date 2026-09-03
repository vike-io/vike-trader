//! Deribit private-WS fill stream. Port of `exec/deribit/user_data.py`: bare connect (no
//! browser UA needed) → public/auth client_credentials (id-matched JSON-RPC ack, tokens
//! captured for the refresh ping) → private/subscribe `user.trades.any.any.raw` — the
//! `any`-kind, `any`-currency channel, so futures AND options fills across every currency
//! stream on ONE subscription (id-matched ack) → the venue-neutral pump with the fills-only
//! mapper. Keepalive = a NON-blocking `public/test` ping every 20s, with the public/auth
//! refresh_token send folded into every 30th tick so it keeps its original 10-min cadence; errors
//! swallowed (a dead token self-heals via re-auth on reconnect).
//!
//! The 20s ping exists for the silent-stall watchdog ([`IDLE_THRESHOLD`]): its reply is the only
//! dependable inbound traffic on an idle account, since fills are the sole other source. Before it,
//! this venue's inbound cadence was the 600s refresh reply — too sparse for a useful threshold,
//! which is why deribit was the last venue left unarmed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tungstenite::Message;
use vike_bridge_core::ratelimit::KeyedRateGate;
use vike_bridge_core::ws::{configure_ws_stream, is_timeout, send_gated, TungsteniteStream};
use vike_exec::EventSender;
use vike_model::events::Event;

use crate::event_mapper::map_deribit_private;
use crate::rpc::{parse_response, JsonRpcBuilder};
use crate::ws_auth::{
    build_client_credentials_auth, build_private_subscribe, build_public_test,
    build_refresh_token_auth,
};
use vike_bridge_core::user_data::{
    run_user_data_forever_with_idle, spawn_pump, spawn_pump_with_resync, OpenOutcome,
    ReconnectHook, ResyncPumpSpec, UserDataAuthError, UserDataFeed,
};

const POLL: Duration = Duration::from_secs(1);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REFRESH_EVERY: Duration = Duration::from_secs(600); // 10 min, under token lifetime
/// App-level ping cadence — a `public/test` whose reply is the lane's ONLY dependable inbound
/// traffic on an idle account (see [`build_public_test`]). The keepalive hook now fires at this
/// rate and folds the 600s token refresh into every 30th tick, so `REFRESH_EVERY` is unchanged in
/// effect.
const PING_EVERY: Duration = Duration::from_secs(20);
/// How many `PING_EVERY` ticks make up one `REFRESH_EVERY` — 600 / 20.
const TICKS_PER_REFRESH: u32 = (REFRESH_EVERY.as_secs() / PING_EVERY.as_secs()) as u32;
/// Silent-stall watchdog: no inbound frame of ANY kind for this long ⇒ the socket is dead behind an
/// open connection. 3x [`PING_EVERY`] — Deribit answers every `public/test`, so a healthy socket
/// delivers a frame every ~20s even on a totally idle account. Keyed off that server-answer
/// cadence, NOT data cadence: an account with no orders is legitimately silent.
///
/// This venue was the last one left unarmed: before the `public/test` ping its only inbound traffic
/// was the 600s refresh reply, which would have forced a useless ~40-minute threshold.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(60);

type WsSocket = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

/// The pump's keepalive hook: a `public/test` ping every tick, plus the token refresh every
/// [`TICKS_PER_REFRESH`]th tick so the refresh keeps its original 600s cadence exactly.
///
/// Hoisted so both pump bodies (with and without the A3 resync supervisor) share ONE definition —
/// they previously held byte-identical refresh closures, and a ping that drifted between them would
/// silently invalidate [`IDLE_THRESHOLD`] on one of the two paths.
fn keepalive_hook<'a>(
    builder: &'a JsonRpcBuilder,
    token_cell: &'a Mutex<String>,
) -> impl FnMut(&mut TungsteniteStream) + 'a {
    let mut ticks: u32 = 0;
    move |ws: &mut TungsteniteStream| {
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
fn deribit_ack_is_transient(code: i64) -> bool {
    code == 10028
}

/// The Deribit `user.trades` fill channel we subscribe. We use the `any`-kind, `any`-currency
/// channel (`user.trades.any.any.raw`) so futures AND options fills — across BTC/ETH/SOL/USDC/… —
/// all stream on ONE subscription. The deribit exec client mounts `BTC-PERPETUAL`, so a per-kind
/// (`future`) / per-currency channel would MISS every option fill (they'd surface only on the
/// reconcile poll). Deribit's `user.trades.{kind}.{currency}.{interval}` accepts
/// `kind ∈ {future, option, spot, future_combo, option_combo, any}` and
/// `currency ∈ {BTC, ETH, SOL, USDC, …, any}`; the `raw` interval streams each fill immediately.
/// Incoming option/future trade notifications share the same shape, so the mapper is unchanged.
fn deribit_trades_channel() -> String {
    "user.trades.any.any.raw".to_string()
}

fn await_rpc_ack(
    socket: &mut WsSocket,
    rid: i64,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<serde_json::Value, Box<OpenOutcome<TungsteniteStream>>> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(Box::new(OpenOutcome::Stopped));
        }
        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) if is_timeout(&e) => {
                if Instant::now() >= deadline {
                    // audit A3: a timed-out ack is transient (busy/latency), not auth — reconnect.
                    return Err(Box::new(OpenOutcome::Transport(
                        "Deribit WS ack timed out".into(),
                    )));
                }
                continue;
            }
            Err(e) => return Err(Box::new(OpenOutcome::Transport(format!("handshake recv: {e}")))),
        };
        match msg {
            Message::Text(raw) => {
                let Ok(frame) = serde_json::from_str::<serde_json::Value>(raw.as_str()) else {
                    continue;
                };
                let (fid, result, error) = parse_response(&frame);
                if fid != Some(rid) {
                    continue; // interleaved notification / different reply
                }
                if let Some(err) = error.filter(|e| !e.is_null()) {
                    // NEVER include credentials
                    let text = format!(
                        "Deribit auth failed: {}",
                        err.get("message").and_then(|m| m.as_str()).unwrap_or("")
                    );
                    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                    // audit A3: 10028 = too_many_requests (rate limit) is TRANSIENT → reconnect;
                    // genuine auth errors (e.g. 13009 unauthorized) still surface + stop.
                    return Err(Box::new(if deribit_ack_is_transient(code) {
                        OpenOutcome::Transport(text)
                    } else {
                        OpenOutcome::Auth(UserDataAuthError(text))
                    }));
                }
                return Ok(result.unwrap_or(serde_json::json!({})));
            }
            Message::Ping(p) => {
                let _ = socket.send(Message::Pong(p));
            }
            Message::Close(_) => {
                return Err(Box::new(OpenOutcome::Transport("closed during handshake".into())))
            }
            _ => {}
        }
    }
}

/// Connect → auth (tokens into `token_cell`) → subscribe. Socket closed on any exit.
// handshake takes url + 2 creds + channels + builder + token cell + stop + the WS rate gate
#[allow(clippy::too_many_arguments)]
pub fn open_deribit_user_data_ws(
    ws_url: &str,
    client_id: &str,
    client_secret: &str,
    channels: &[String],
    builder: &JsonRpcBuilder,
    token_cell: &Mutex<String>,
    stop: &AtomicBool,
    gate: &KeyedRateGate,
) -> OpenOutcome<TungsteniteStream> {
    let (mut socket, _resp) = match tungstenite::connect(ws_url) {
        Ok(ok) => ok,
        Err(e) => return OpenOutcome::Transport(format!("connect: {e}")),
    };
    configure_ws_stream(&socket, POLL);
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;

    let auth_id = builder.next_id();
    let auth = build_client_credentials_auth(client_id, client_secret, None, auth_id);
    if let Err(e) = send_gated(&mut socket, Some(gate), "login", &auth.to_string()) {
        let _ = socket.close(None);
        return OpenOutcome::Transport(format!("auth send: {e:?}"));
    }
    let result = match await_rpc_ack(&mut socket, auth_id, stop, deadline) {
        Ok(r) => r,
        Err(outcome) => {
            let _ = socket.close(None);
            return *outcome;
        }
    };
    // capture the refresh token for the keepalive (NEVER logged)
    *token_cell.lock().unwrap() =
        result.get("refresh_token").and_then(|t| t.as_str()).unwrap_or("").to_string();

    let sub_id = builder.next_id();
    let sub = build_private_subscribe(channels, sub_id);
    if let Err(e) = send_gated(&mut socket, Some(gate), "subscribe", &sub.to_string()) {
        let _ = socket.close(None);
        return OpenOutcome::Transport(format!("subscribe send: {e:?}"));
    }
    if let Err(outcome) = await_rpc_ack(&mut socket, sub_id, stop, deadline) {
        let _ = socket.close(None);
        return *outcome;
    }
    OpenOutcome::Ready(TungsteniteStream(socket))
}

/// Spawn the persistent Deribit fill-stream pump feeding the vt-core ingest.
pub fn spawn_deribit_user_data(
    ws_url: String,
    client_id: String,
    client_secret: String,
    symbol: String,
    events: EventSender,
) -> UserDataFeed {
    let channel = deribit_trades_channel();
    spawn_pump(format!("deribit-userdata-{symbol}"), move |stop: Arc<AtomicBool>| {
        let _span = tracing::info_span!("user_data_pump", venue = "deribit", %symbol).entered();
        let builder = JsonRpcBuilder::new();
        let token_cell = Mutex::new(String::new());
        let channels = vec![channel];
        let mut refresh = keepalive_hook(&builder, &token_cell);
        // built once per pump; persists across reconnects (spec §A keyed WS gate)
        let ws_gate = crate::ratelimit::ws_rate_gate();
        run_user_data_forever_with_idle(
            || {
                open_deribit_user_data_ws(
                    &ws_url,
                    &client_id,
                    &client_secret,
                    &channels,
                    &builder,
                    &token_cell,
                    &stop,
                    &ws_gate,
                )
            },
            |frame| map_deribit_private(frame, "deribit", &symbol),
            |event| events.blocking_send(event).is_ok(),
            &stop,
            POLL,
            Duration::from_secs(30),
            Some((PING_EVERY, &mut refresh)),
            || {}, // no resync: use spawn_deribit_user_data_with_resync to enable audit A3
            Some(IDLE_THRESHOLD),
        )
    })
}

/// Join half of the Deribit pump + its audit-A3 resync supervisor (see
/// [`spawn_deribit_user_data_with_resync`]). Twin of the spot `BinanceUserDataResync`; thin alias
/// for the shared [`UserDataResyncFeed`](vike_bridge_core::user_data::UserDataResyncFeed).
pub type DeribitUserDataResync = vike_bridge_core::user_data::UserDataResyncFeed;

/// Like [`spawn_deribit_user_data`], plus the audit-A3 resync supervisor — the Deribit mirror of
/// the spot `spawn_binance_user_data_with_resync`.
/// On every WS re-open the pump bumps a generation counter, and the supervisor (after a settle
/// delay) replays recent history via `resync_fetch` so a fill/cancel/reject that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's WS-RPC
/// history source (typically `get_order_history` + `get_user_trades` →
/// [`map_deribit_history`](crate::history::map_deribit_history)) on a SEPARATE Deribit
/// client from the order-submit path. Both threads tear down on `shutdown`; the supervisor also
/// self-exits if the pump dies or the core is gone. (Exercised by live smokes.)
pub fn spawn_deribit_user_data_with_resync(
    ws_url: String,
    client_id: String,
    client_secret: String,
    symbol: String,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
) -> DeribitUserDataResync {
    let channel = deribit_trades_channel();
    let events_p = events.clone();
    let sym_p = symbol.clone();
    spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: format!("deribit-userdata-{symbol}"),
        resync_thread_name: format!("deribit-resync-{symbol}"),
        poll: POLL,
        settle: Duration::from_secs(1), // let the reopened WS drain its backlog first
        events,
        resync_fetch,
        on_reconcile: None, // Task 13: no reconcile trigger wired yet — event-replay only
        pump_body: move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
            let builder = JsonRpcBuilder::new();
            let token_cell = Mutex::new(String::new());
            let channels = vec![channel];
            let mut refresh = keepalive_hook(&builder, &token_cell);
            // built once per pump; persists across reconnects (spec §A keyed WS gate)
            let ws_gate = crate::ratelimit::ws_rate_gate();
            run_user_data_forever_with_idle(
                || {
                    open_deribit_user_data_ws(
                        &ws_url,
                        &client_id,
                        &client_secret,
                        &channels,
                        &builder,
                        &token_cell,
                        &stop,
                        &ws_gate,
                    )
                },
                |frame| map_deribit_private(frame, "deribit", &sym_p),
                |event| events_p.blocking_send(event).is_ok(),
                &stop,
                POLL,
                Duration::from_secs(30),
                Some((PING_EVERY, &mut refresh)),
                on_reconnect,
                Some(IDLE_THRESHOLD),
            )
        },
    })
}

#[cfg(test)]
mod channel_tests {
    use super::deribit_trades_channel;

    #[test]
    fn subscribes_any_kind_any_currency() {
        // `any` kind + `any` currency: futures AND options fills, every currency, one subscription.
        // (The exec client mounts BTC-PERPETUAL, so a per-kind/per-currency channel would drop
        // every option fill — they'd only surface on the reconcile poll.)
        assert_eq!(deribit_trades_channel(), "user.trades.any.any.raw");
    }
}
