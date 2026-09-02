//! Polymarket authenticated user-channel WS pump. Connects the CLOB user stream, sends the L2-auth
//! subscribe frame, then hands the socket to the shared venue-neutral pump
//! (`run_user_data_forever_with_idle`, with the silent-stall watchdog armed — see [`IDLE_THRESHOLD`])
//! decoding fills/orders via [`decode_user`] and re-keying CLOB ids → coids through the shared
//! [`PolymarketRegistry`]. Feeds the vt-core ingest LOSSLESSLY (fills must never drop). Like the
//! other venues' pumps this is exercised end-to-end by a live smoke (real-money mainnet + proxy).
//!
//! The dial goes through [`vike_bridge_core::ws_proxy::connect_ws`] so this lane can ride the same
//! Dublin SOCKS tunnel the REST lane uses when `POLY_WS_PROXY_ENABLED` is set
//! ([`crate::egress::ws_proxy`]); with the gate off (the default) it is `tungstenite::connect`
//! verbatim.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::ratelimit::KeyedRateGate;
use vike_bridge_core::ws::{configure_ws_stream, send_gated, TungsteniteStream};
use vike_bridge_core::ws_proxy::connect_ws;
use vike_exec::EventSender;
use vike_model::events::Event;

use crate::config::PolymarketCreds;
use crate::fill_tracker::FillTracker;
use crate::registry::PolymarketRegistry;
use crate::user_ws::{decode_user_with_tracker, user_subscribe_message};
// The shared pump scaffold is a DIRECT bridge-core path — this file is itself the crate's
// `user_data` module, so a `crate::user_data::` spelling would be a self-reference (the same
// name-collision trap the deribit/okx/binance moves documented).
use vike_bridge_core::user_data::{
    run_user_data_forever_with_idle, spawn_pump, spawn_pump_with_resync, OpenOutcome,
    ReconnectHook, ResyncPumpSpec, UserDataFeed,
};

pub use crate::config::WS_USER;

const POLL: Duration = Duration::from_secs(1);
const PING_EVERY: Duration = Duration::from_secs(10); // CLOB WS idle keepalive
/// Silent-stall watchdog: no inbound frame of ANY kind for this long ⇒ the socket is dead behind
/// an open connection, so end the session and let the reconnect + audit-A3 resync repair it.
/// 6x [`PING_EVERY`] — the CLOB WS answers the idle keepalive, so a healthy socket delivers an
/// inbound frame every ~10s even on a totally idle account. Keyed off that server-answer cadence,
/// NOT data cadence: a wallet with no orders is legitimately silent.
///
/// This lane matters more here than anywhere else: on Polymarket EVERY post-acceptance terminal
/// arrives ONLY on the user channel, so a silently-stalled socket strands orders outright.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(60);

/// Connect `wss://…/user` and send the L2-auth subscribe frame. Connected+subscribed = ready
/// (the user channel begins streaming on subscribe; no ack handshake).
pub fn open_polymarket_user_data_ws(
    ws_url: &str,
    creds: &PolymarketCreds,
    markets: &[String],
    stop: &AtomicBool,
    gate: &KeyedRateGate,
) -> OpenOutcome<TungsteniteStream> {
    if stop.load(Ordering::Relaxed) {
        return OpenOutcome::Stopped;
    }
    // Optional SOCKS5 egress (spec §0.1): `ws_proxy() == None` (the default) makes this
    // `tungstenite::connect(ws_url)` verbatim — the pre-existing dial. Resolved per connect so a
    // reconnect picks up a changed tunnel.
    let mut socket = match connect_ws(ws_url, crate::egress::ws_proxy().as_ref(), None) {
        Ok(ok) => ok,
        Err(e) => return OpenOutcome::Transport(format!("connect: {e}")),
    };
    configure_ws_stream(&socket, POLL);
    let sub = user_subscribe_message(creds, markets); // carries the L2 creds — NEVER log
    if let Err(e) = send_gated(&mut socket, Some(gate), "subscribe", &sub) {
        let _ = socket.close(None);
        return OpenOutcome::Transport(format!("subscribe send: {e:?}"));
    }
    OpenOutcome::Ready(TungsteniteStream(socket))
}

/// Spawn the persistent Polymarket user-channel pump feeding the vt-core ingest (lossless). Pass the
/// SAME `registry` clone as [`PolymarketExecutionClient::spawn`](super::client::PolymarketExecutionClient::spawn)
/// so decoded fills/cancels re-key back to the coid.
pub fn spawn_polymarket_user_data(
    ws_url: String,
    creds: PolymarketCreds,
    markets: Vec<String>,
    registry: PolymarketRegistry,
    events: EventSender,
) -> UserDataFeed {
    spawn_polymarket_user_data_tracked(ws_url, creds, markets, registry, events, None)
}

/// [`spawn_polymarket_user_data`] plus the optional dust-snap [`FillTracker`]
/// ([`crate::fill_tracker`] — cent-tick overfill/residual reconciliation). Pass the SAME tracker
/// clone the exec client registers submitted quantities into. `None` is byte-identical to
/// [`spawn_polymarket_user_data`].
pub fn spawn_polymarket_user_data_tracked(
    ws_url: String,
    creds: PolymarketCreds,
    markets: Vec<String>,
    registry: PolymarketRegistry,
    events: EventSender,
    tracker: Option<FillTracker>,
) -> UserDataFeed {
    spawn_pump("polymarket-userdata".to_string(), move |stop: Arc<AtomicBool>| {
        let mut ping = |ws: &mut TungsteniteStream| {
            let _ = ws.send_text("PING"); // CLOB text keepalive; PONG reply is non-JSON, skipped
        };
        // built once per pump; persists across reconnects (spec §A keyed WS gate)
        let ws_gate = crate::ratelimit::ws_rate_gate();
        run_user_data_forever_with_idle(
            || open_polymarket_user_data_ws(&ws_url, &creds, &markets, &stop, &ws_gate),
            |frame| decode_user_with_tracker(frame, &registry, tracker.as_ref()),
            |event| events.blocking_send(event).is_ok(),
            &stop,
            POLL,
            Duration::from_secs(30),
            Some((PING_EVERY, &mut ping)),
            || {}, // no resync: use spawn_polymarket_user_data_with_resync to enable audit A3
            Some(IDLE_THRESHOLD),
        )
    })
}

/// Join half of the pump + its audit-A3 resync supervisor (see
/// [`spawn_polymarket_user_data_with_resync`]). Twin of the spot `BinanceUserDataResync`; thin alias
/// for the shared [`UserDataResyncFeed`](vike_bridge_core::user_data::UserDataResyncFeed).
pub type PolymarketUserDataResync = vike_bridge_core::user_data::UserDataResyncFeed;

/// Like [`spawn_polymarket_user_data`], plus the audit-A3 resync supervisor: on every WS re-open the
/// pump bumps a generation counter and the supervisor (after a settle delay) replays recent history
/// via `resync_fetch` (typically `get_trades` + `get_orders` →
/// [`map_polymarket_history`](crate::history::map_polymarket_history)) on a SEPARATE
/// client, so a fill/cancel that landed during the reconnect gap — or a MATCHED-then-FAILED
/// divergence — is recovered; the core dedups the overlap. Both threads tear down on `shutdown`; the
/// supervisor self-exits if the pump dies or the core is gone. (Exercised by a live smoke.)
pub fn spawn_polymarket_user_data_with_resync(
    ws_url: String,
    creds: PolymarketCreds,
    markets: Vec<String>,
    registry: PolymarketRegistry,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
) -> PolymarketUserDataResync {
    spawn_polymarket_user_data_with_resync_tracked(
        ws_url,
        creds,
        markets,
        registry,
        events,
        resync_fetch,
        None,
    )
}

/// [`spawn_polymarket_user_data_with_resync`] plus the optional dust-snap [`FillTracker`]. The
/// resync replay itself decodes UNTRACKED (it overlaps live events the tracker already folded);
/// only the live pump snaps. `None` is byte-identical to the untracked entry point.
#[allow(clippy::too_many_arguments)]
pub fn spawn_polymarket_user_data_with_resync_tracked(
    ws_url: String,
    creds: PolymarketCreds,
    markets: Vec<String>,
    registry: PolymarketRegistry,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    tracker: Option<FillTracker>,
) -> PolymarketUserDataResync {
    let events_p = events.clone();
    spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: "polymarket-userdata".to_string(),
        resync_thread_name: "polymarket-resync".to_string(),
        poll: POLL,
        settle: Duration::from_secs(1), // let the reopened WS drain its backlog first
        events,
        resync_fetch,
        on_reconcile: None, // Task 13: no reconcile trigger wired yet — event-replay only
        pump_body: move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
            let mut ping = |ws: &mut TungsteniteStream| {
                let _ = ws.send_text("PING");
            };
            // built once per pump; persists across reconnects (spec §A keyed WS gate)
            let ws_gate = crate::ratelimit::ws_rate_gate();
            run_user_data_forever_with_idle(
                || open_polymarket_user_data_ws(&ws_url, &creds, &markets, &stop, &ws_gate),
                |frame| decode_user_with_tracker(frame, &registry, tracker.as_ref()),
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

    #[test]
    fn ws_user_is_the_user_channel() {
        assert!(WS_USER.starts_with("wss://"));
        assert!(WS_USER.ends_with("/user"));
    }

    #[test]
    fn resync_api_is_exposed() {
        // Type-level proof the resync entry points exist with the expected shapes (no I/O): the
        // generic fn coerces to a concrete fn pointer, and the returned handle exposes `shutdown`.
        #[allow(clippy::type_complexity)]
        let f: fn(
            String,
            PolymarketCreds,
            Vec<String>,
            PolymarketRegistry,
            EventSender,
            fn() -> Vec<Event>,
        ) -> PolymarketUserDataResync = spawn_polymarket_user_data_with_resync;
        let _ = f;
        let _shutdown: fn(
            PolymarketUserDataResync,
        ) -> Result<(), vike_bridge_core::user_data::UserDataAuthError> =
            PolymarketUserDataResync::shutdown;
    }

    #[test]
    fn open_builds_the_user_subscribe_frame() {
        // The pump's first send is user_subscribe_message(creds, markets); prove it targets "user".
        let creds = PolymarketCreds {
            api_key: "k".into(),
            secret: "s".into(),
            passphrase: "p".into(),
            ..Default::default()
        };
        let frame: serde_json::Value =
            serde_json::from_str(&user_subscribe_message(&creds, &["111".into()])).unwrap();
        assert_eq!(frame["type"], "user");
    }
}
