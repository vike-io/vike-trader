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
//! refused at the trigger, fired). While okx's row lists `trailing` it also subscribes `algo-advance`,
//! where the venue pushes a TRAILING order's own state ([`open_okx_algo_websocket`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::client::IntoClientRequest;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::ratelimit::RateGatePerKey;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::websocket::{
    AckResult, WebsocketSessionUserStreamTungstenite, send_and_await,
};
use vike_bridge_core::websocket_session::spec::CONNECT_10S;
use vike_execution::EventSender;
use vike_model::events::Event;
use vike_venues::venue_timings::OKX;

use crate::BROWSER_UA;
use crate::execution::event_mapper::{
    map_okx_algo_frame, map_okx_perp, map_okx_perp_by_instrument,
};
use crate::transport::ws_auth::{
    PING_TEXT, build_login_frame, build_subscribe_frame, match_event_ack,
};
use vike_bridge_core::websocket_session::user::{
    WebsocketSessionUserOpenOutcome, WebsocketSessionUserResyncHandle,
    WebsocketSessionUserResyncSpec, dial_user_stream, spawn_user_with_resync,
};

// The three timings below are this venue's exec-lane row in
// `crates/vike-venues/src/venue_timings.rs` (`OKX`), which carries each value and its
// reason (decision 0160). The pump's poll, reconnect ceiling and resync settle are the venue-free
// mechanism defaults of `vike_bridge_core::websocket_session::user` and are read from there.

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
/// spot pair or future is dropped where its size is looked up (`crate::execution`'s `load_linear_ct_val`), so a
/// coin traded by hand in the OKX app never reaches the engine.
const ORDERS_INST_TYPE: &str = "ANY";

/// Connect (browser UA) → login (SECONDS ts) → ack → subscribe(`channel`, `inst_type`) → ack. The
/// private socket subscribes `orders`; the business socket (`websocket_url` = [`crate::execution::rest::OkxNet::business_websocket`])
/// subscribes `orders-algo`. The login is the same signature on both.
pub fn open_okx_user_data_websocket(
    websocket_url: &str,
    api_key: &str,
    api_secret: &str,
    passphrase: &str,
    now_ms: impl Fn() -> i64,
    channel: &str,
    inst_type: &str,
    stop: &AtomicBool,
    gate: &RateGatePerKey,
) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite> {
    open_okx_websocket(
        websocket_url,
        api_key,
        api_secret,
        passphrase,
        now_ms,
        &[],
        channel,
        inst_type,
        stop,
        gate,
    )
}

/// The business socket's open for [`spawn_okx_algo_user_data_with_resync`]: `orders-algo` exactly as
/// [`open_okx_user_data_websocket`] subscribes it, and FIRST, only when `trailing` (okx's row lists
/// `trailing`, [`crate::execution::rest::trailing_listed`]), `algo-advance`: the channel the venue pushes a trailing
/// order's own state on ("Retrieve advance algo orders (including Iceberg order, TWAP order, Trailing
/// order)", the official WS table; `orders-algo` carries trigger, oco and conditional orders only).
/// `algo-advance` goes first so that its subscribe snapshot ("Data will be pushed when first subscribed")
/// and the gap between the two acks can only cost a trailing order's frame, never a stop's.
///
/// ⚠ A REFUSED `algo-advance` subscription (an `error` event, unmeasured) is logged and the socket goes
/// on with `orders-algo` alone: the stops' channel never depends on the new one. A dead socket or a
/// timeout in that ack is the open's failure, as for any subscribe.
pub fn open_okx_algo_websocket(
    websocket_url: &str,
    api_key: &str,
    api_secret: &str,
    passphrase: &str,
    now_ms: impl Fn() -> i64,
    trailing: bool,
    stop: &AtomicBool,
    gate: &RateGatePerKey,
) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite> {
    let optional: &[&str] = if trailing { &[ALGO_ADVANCE] } else { &[] };
    open_okx_websocket(
        websocket_url,
        api_key,
        api_secret,
        passphrase,
        now_ms,
        optional,
        "orders-algo",
        ORDERS_INST_TYPE,
        stop,
        gate,
    )
}

/// The business socket's channel of a trailing order's own state (gated: [`open_okx_algo_websocket`]).
pub const ALGO_ADVANCE: &str = "algo-advance";

/// Connect, login, then subscribe each of `optional` (a refusal logged and skipped) and `channel` (a
/// refusal is the open's), all under `inst_type`, every ack inside one handshake deadline. Each step is one shared
/// login step (`vike_bridge_core::websocket::send_and_await`, which closes the socket on any failure), on a socket the
/// shared dial set up (`vike_bridge_core::websocket_session::user::dial_user_stream`) with the browser UA on its upgrade.
fn open_okx_websocket(
    websocket_url: &str,
    api_key: &str,
    api_secret: &str,
    passphrase: &str,
    now_ms: impl Fn() -> i64,
    optional: &[&str],
    channel: &str,
    inst_type: &str,
    stop: &AtomicBool,
    gate: &RateGatePerKey,
) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite> {
    let Ok(mut request) = websocket_url.into_client_request() else {
        return WebsocketSessionUserOpenOutcome::Transport("bad ws url".into());
    };
    request
        .headers_mut()
        .insert("User-Agent", tungstenite::http::HeaderValue::from_static(BROWSER_UA));
    let mut socket = match dial_user_stream(request, CONNECT_10S) {
        Ok(socket) => socket,
        Err(e) => return WebsocketSessionUserOpenOutcome::Transport(e),
    };
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;

    // login — NEVER log (apiKey + passphrase + sign); OKX wants epoch SECONDS
    let login = build_login_frame(api_key, api_secret, passphrase, now_ms() / 1000);
    if let Err(outcome) = send_and_await(
        &mut socket,
        Some(gate),
        "login",
        &login.to_string(),
        "login",
        stop,
        deadline,
        "OKX WS login",
        |f| match_event_ack(f, "login"),
    ) {
        return *outcome;
    }

    for extra in optional {
        let sub = build_subscribe_frame(extra, inst_type);
        // the venue refused this channel alone (an `error` event): the socket stays up without it
        let survivable = |f: &Value| match match_event_ack(f, "subscribe") {
            AckResult::Err(_) => {
                tracing::warn!(venue = "okx", channel = %extra, "the venue refused the subscription; the socket goes on without it");
                AckResult::Ok
            }
            other => other,
        };
        if let Err(outcome) = send_and_await(
            &mut socket,
            Some(gate),
            "subscribe",
            &sub.to_string(),
            "subscribe",
            stop,
            deadline,
            "OKX WS subscribe",
            survivable,
        ) {
            return *outcome;
        }
    }

    let sub = build_subscribe_frame(channel, inst_type);
    if let Err(outcome) = send_and_await(
        &mut socket,
        Some(gate),
        "subscribe",
        &sub.to_string(),
        "subscribe",
        stop,
        deadline,
        "OKX WS subscribe",
        |f| match_event_ack(f, "subscribe"),
    ) {
        return *outcome;
    }
    WebsocketSessionUserOpenOutcome::Ready(WebsocketSessionUserStreamTungstenite(socket))
}

/// Spawn the persistent OKX SWAP fill-stream pump feeding the th-core ingest, for the MANUAL smokes: the same pump
/// as [`spawn_okx_perp_user_data_with_resync`] on `orders`/`SWAP` with one `ct_val`, no replay, no fence (the lane
/// takes every event as it comes) and no reconcile trigger.
///
/// `ready` is set `true` the first time the login+subscribe handshake completes — a caller can wait on
/// it to KNOW the pump is subscribed before it acts (condition-based, vs a fixed sleep that races a
/// cold-start handshake and drops a fill that lands before subscribe). The exec adapter uses the
/// `_with_resync` twin + a fill-sentinel instead, so this readiness gate is for the manual smokes.
pub fn spawn_okx_perp_user_data(
    websocket_url: String,
    api_key: String,
    api_secret: String,
    passphrase: String,
    symbol: String,
    ct_val: f64,
    events: EventSender,
    ready: Arc<AtomicBool>,
) -> WebsocketSessionUserResyncHandle {
    let mapped = symbol.clone();
    // The manual smokes' pump: a clock no client reads, so the login stamps the local time, as it always did.
    let clock = VenueClock::default();
    spawn_okx_stream(
        "okx",
        symbol,
        events,
        None,
        Vec::new,
        None,
        move |stop, gate| {
            let outcome = open_okx_user_data_websocket(
                &websocket_url,
                &api_key,
                &api_secret,
                &passphrase,
                clock.ms(),
                "orders",
                "SWAP",
                stop,
                gate,
            );
            // Subscribe acked → the pump can now receive fills. Latch ready so a waiter
            // proceeds only once a fill can't be missed.
            if matches!(outcome, WebsocketSessionUserOpenOutcome::Ready(_)) {
                ready.store(true, Ordering::Release);
            }
            outcome
        },
        move |frame| map_okx_perp(frame, "okx", &mapped, ct_val),
    )
}

/// Like [`spawn_okx_perp_user_data`], plus the audit-A3 resync supervisor — the OKX mirror of the
/// spot `spawn_binance_user_data_with_resync`.
/// On every WS re-open the pump bumps a generation counter, and the supervisor (after a settle
/// delay) replays recent history via `resync_fetch` so a fill/cancel/reject that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST
/// history source (typically `get_orders_history` + `get_fills_history` →
/// [`map_okx_history`](crate::execution::resync::map_okx_history)) on a SEPARATE `VenueRestOkx` from the
/// order-submit client. Both threads tear down on `shutdown`; the supervisor also self-exits if the
/// pump dies or the core is gone. (Exercised by live smokes.)
///
/// `on_reconcile` (reconciliation-activation Task 7) is threaded straight to
/// `run_user_resync`'s own `on_reconcile` param — `Some` fires the recon trigger after
/// every reconnect's event-replay settles; `None` (every non-recon caller) reproduces the
/// pre-Task-7 event-replay-only behavior byte-for-byte.
///
/// `ct_val_of` answers the contracts→base factor for the `instId` a fill row names (the private
/// `orders` channel carries every SWAP of the account, not only `symbol`): the caller's
/// [`ContractSizes`](crate::execution::rest::per_symbol::ContractSizes). `None` drops that fill
/// ([`map_okx_perp_by_instrument`]'s contract). It runs on the pump thread, so a lookup that loads a row
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
/// `events`, and the exec thread's `VenueRestOrderSubmitFence` must wrap the same `fence`.
///
/// The login's timestamp is stamped on `clock` (owner ruling B-4: the start's signing clock, read by its REST
/// clients; the local clock until one answers).
#[expect(clippy::too_many_arguments)]
pub fn spawn_okx_perp_user_data_with_resync(
    websocket_url: String,
    api_key: String,
    api_secret: String,
    passphrase: String,
    symbol: String,
    ct_val_of: impl Fn(&str) -> Option<f64> + Send + 'static,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
    clock: VenueClock,
) -> WebsocketSessionUserResyncHandle {
    let mapped = symbol.clone();
    spawn_okx_stream(
        "okx",
        symbol,
        events,
        Some(fence),
        resync_fetch,
        on_reconcile,
        move |stop, gate| {
            open_okx_user_data_websocket(
                &websocket_url,
                &api_key,
                &api_secret,
                &passphrase,
                clock.ms(),
                "orders",
                ORDERS_INST_TYPE,
                stop,
                gate,
            )
        },
        move |frame| map_okx_perp_by_instrument(frame, "okx", &mapped, &ct_val_of),
    )
}

/// The OKX ALGO-order socket: `orders-algo` on the BUSINESS URL ([`crate::execution::rest::OkxNet::business_websocket`]), the
/// only place the venue pushes a STOP's own state (`effective` = it fired, `canceled`, `order_failed`).
/// The private `orders` socket carries the fired stop's CHILD order, never the stop, so without this
/// pump a stop that was cancelled, expired or refused at the venue stays `ACCEPTED` in the node for
/// good. Same login, ping and idle watchdog as the private pump; its events are mapped by
/// [`map_okx_algo_frame`] and go through the SAME `fence` as the private pump's.
///
/// `resync_fetch` is the audit-A3 twin for THIS socket (the caller's closure over
/// [`map_okx_algo_history`](crate::execution::resync::map_okx_algo_history), on a client of its own): after a
/// reconnect it replays the stops that ended in the gap. No reconcile trigger is wired
/// (the private pump's is enough: it fires after its own reconnects, and a reconcile pass reads the
/// stops from `orders-algo-pending` regardless of this socket). Its login is stamped on `clock`, as the private
/// pump's is (owner ruling B-4).
pub fn spawn_okx_algo_user_data_with_resync(
    websocket_url: String,
    api_key: String,
    api_secret: String,
    passphrase: String,
    symbol: String,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    clock: VenueClock,
) -> WebsocketSessionUserResyncHandle {
    // the trailing orders' channel rides this socket only while okx's row lists `trailing`
    let trailing = crate::execution::rest::trailing_listed();
    spawn_okx_stream(
        "okx-algo",
        symbol,
        events,
        Some(fence),
        resync_fetch,
        None,
        move |stop, gate| {
            open_okx_algo_websocket(
                &websocket_url,
                &api_key,
                &api_secret,
                &passphrase,
                clock.ms(),
                trailing,
                stop,
                gate,
            )
        },
        map_okx_algo_frame,
    )
}

/// One okx private socket, as `vike_bridge_core::websocket_session::user::spawn_user_with_resync` runs it: this venue's open
/// (handed the pump's stop and the socket's keyed WS gate), decoder, raw-text `ping` keepalive and idle threshold.
/// `lane` names the threads (`okx-userdata-{symbol}`, `okx-algo-resync-{symbol}`).
fn spawn_okx_stream(
    lane: &'static str,
    symbol: String,
    events: EventSender,
    fence: Option<Arc<SubmitFence>>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
    mut open: impl FnMut(
        &AtomicBool,
        &RateGatePerKey,
    ) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite>
    + Send
    + 'static,
    decode: impl Fn(&Value) -> Vec<Event> + Send + 'static,
) -> WebsocketSessionUserResyncHandle {
    // Built ONCE per pump; the reconnect loop reuses the open, so the gate persists across reconnects and paces
    // cumulative login/subscribe sends (spec §A keyed WS gate).
    let websocket_gate = crate::transport::ratelimit::websocket_rate_gate();
    spawn_user_with_resync(WebsocketSessionUserResyncSpec {
        venue: "okx",
        lane,
        symbol,
        events,
        fence,
        resync_fetch,
        on_reconcile,
        open: move |stop: &AtomicBool| open(stop, &websocket_gate),
        decode,
        keepalive: Some((
            PING_EVERY,
            Box::new(|ws: &mut WebsocketSessionUserStreamTungstenite| {
                let _ = ws.send_text(PING_TEXT); // raw text, NOT JSON
            }),
        )),
        idle_threshold: Some(IDLE_THRESHOLD),
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
        use vike_bridge_core::websocket_session::user::{
            RESYNC_SETTLE, USER_DATA_MAX_BACKOFF, USER_DATA_POLL,
        };
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
