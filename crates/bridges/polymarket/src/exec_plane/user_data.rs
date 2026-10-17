//! Polymarket authenticated user-channel WS pump. Connects the CLOB user stream, sends the L2-auth
//! subscribe frame, then hands the socket to the shared venue-neutral pump
//! (`run_user_data_forever_with_idle`, with the silent-stall watchdog armed — the exec lane of
//! `vike_venues::venue_timings::POLYMARKET` declares its threshold and why)
//! decoding fills/orders via [`decode_user`] and re-keying CLOB ids → coids through the shared
//! [`PolymarketRegistry`]. Feeds the vt-core ingest LOSSLESSLY (fills must never drop). Like the
//! other venues' pumps this is exercised end-to-end by a live smoke (real-money mainnet + proxy).
//!
//! The dial goes through [`vike_bridge_core::ws_proxy::connect_ws`] so this lane rides the same
//! SOCKS tunnel as the REST lane whenever [`crate::egress::ws_proxy`] answers one — by default
//! whenever the REST lane proxies (`venue.polymarket.proxy_enabled`), with
//! `venue.polymarket.ws_proxy_enabled` as the per-lane override; with no proxy it is
//! `tungstenite::connect` verbatim. (The override was the `POLY_WS_PROXY_ENABLED` variable until
//! decision 0095.)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use vike_bridge_core::ratelimit::KeyedRateGate;
use vike_bridge_core::ws::{TungsteniteStream, configure_ws_stream, send_gated};
use vike_bridge_core::ws_proxy::connect_ws;
use vike_exec::EventSender;
use vike_model::events::Event;
use vike_venues::venue_timings;

use crate::config::PolymarketCreds;
use crate::exec_plane::fill_tracker::FillTracker;
use crate::exec_plane::registry::PolymarketRegistry;
use crate::exec_plane::user_ws::{decode_user_with_tracker, user_subscribe_message};
// The shared pump scaffold is a DIRECT bridge-core path — this file is itself the crate's
// `user_data` module, so a `crate::exec_plane::user_data::` spelling would be a self-reference (the same
// name-collision trap the deribit/okx/binance moves documented).
use vike_bridge_core::user_data::{
    OpenOutcome, RESYNC_SETTLE, ReconnectHook, ResyncPumpSpec, USER_DATA_MAX_BACKOFF,
    USER_DATA_POLL, UserDataResyncFeed, run_user_data_forever_with_idle, spawn_pump_with_resync,
};

// No cadence is declared in this file. The venue's own — the CLOB idle keepalive (a text PING) and
// the silent-stall watchdog — are the exec lane of `venue_timings::POLYMARKET`, read where each is
// used in `spawn_polymarket_user_data_with_resync_tracked`; the row carries their reasons (the
// watchdog is keyed off the server-answer cadence, NOT data cadence: a wallet with no orders is
// legitimately silent, and on Polymarket EVERY post-acceptance terminal arrives ONLY on this
// channel, so a silently-stalled socket strands orders outright). The venue-free ones are
// `vike_bridge_core::user_data`'s `USER_DATA_POLL` (the stop-poll and socket read timeout),
// `USER_DATA_MAX_BACKOFF` (the reconnect ceiling) and `RESYNC_SETTLE` (let the reopened WS drain
// its backlog before the resync replays).

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
    configure_ws_stream(&socket, USER_DATA_POLL);
    let sub = user_subscribe_message(creds, markets); // carries the L2 creds — NEVER log
    if let Err(e) = send_gated(&mut socket, Some(gate), "subscribe", &sub) {
        let _ = socket.close(None);
        return OpenOutcome::Transport(format!("subscribe send: {e:?}"));
    }
    OpenOutcome::Ready(TungsteniteStream(socket))
}

/// Spawn the persistent Polymarket user-channel pump feeding the vt-core ingest (lossless), plus the
/// audit-A3 resync supervisor: on every WS re-open the pump bumps a generation counter and the
/// supervisor (after a settle delay) replays recent history via `resync_fetch` (typically
/// `get_trades` + `get_orders` →
/// [`map_polymarket_history`](crate::exec_plane::history::map_polymarket_history)) on a SEPARATE
/// client, so a fill/cancel that landed during the reconnect gap — or a MATCHED-then-FAILED
/// divergence — is recovered; the core dedups the overlap. Both threads tear down on `shutdown`; the
/// supervisor self-exits if the pump dies or the core is gone. (Exercised by a live smoke.)
///
/// Pass the SAME `registry` clone the exec client records accepted orders in, so decoded
/// fills/cancels re-key back to the coid. `tracker` is the optional dust-snap [`FillTracker`]
/// ([`crate::exec_plane::fill_tracker`] — cent-tick overfill/residual reconciliation): pass the
/// SAME clone the exec client registers submitted quantities into. The resync replay itself
/// decodes UNTRACKED (it overlaps live events the tracker already folded); only the live pump
/// snaps. `None` snaps nothing.
///
/// The ONE user-channel spawner; production starts it from inside the exec thread
/// ([`PolymarketExecutionClient::spawn_live`](super::client::PolymarketExecutionClient::spawn_live)).
pub fn spawn_polymarket_user_data_with_resync_tracked(
    ws_url: String,
    creds: PolymarketCreds,
    markets: Vec<String>,
    registry: PolymarketRegistry,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    tracker: Option<FillTracker>,
) -> UserDataResyncFeed {
    let events_p = events.clone();
    spawn_pump_with_resync(ResyncPumpSpec {
        pump_thread_name: "polymarket-userdata".to_string(),
        resync_thread_name: "polymarket-resync".to_string(),
        poll: USER_DATA_POLL,
        settle: RESYNC_SETTLE,
        events,
        resync_fetch,
        on_reconcile: None, // Task 13: no reconcile trigger wired yet — event-replay only
        pump_body: move |stop: Arc<AtomicBool>, on_reconnect: ReconnectHook| {
            let mut ping = |ws: &mut TungsteniteStream| {
                let _ = ws.send_text("PING");
            };
            // built once per pump; persists across reconnects (spec §A keyed WS gate)
            let ws_gate = crate::exec_plane::ratelimit::ws_rate_gate();
            run_user_data_forever_with_idle(
                || open_polymarket_user_data_ws(&ws_url, &creds, &markets, &stop, &ws_gate),
                |frame| decode_user_with_tracker(frame, &registry, tracker.as_ref()),
                |event| events_p.blocking_send(event).is_ok(),
                &stop,
                USER_DATA_POLL,
                USER_DATA_MAX_BACKOFF,
                Some((venue_timings::POLYMARKET.exec().keepalive_every(), &mut ping)),
                on_reconnect,
                Some(venue_timings::POLYMARKET.exec().idle_threshold()),
            )
        },
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The cadences the pump hands the shared driver, as EFFECTIVE values. The CLOB keepalive and
    /// the silent-stall watchdog are the exec lane of `venue_timings::POLYMARKET` (read inline at
    /// the call); the poll, the reconnect ceiling and the resync settle are
    /// `vike_bridge_core::user_data`'s pump defaults. A retune of any of them is a deliberate edit
    /// to the literal here.
    #[test]
    fn the_pump_runs_on_the_declared_cadences() {
        let lane = venue_timings::POLYMARKET.exec();
        assert_eq!(lane.keepalive_every(), Duration::from_secs(10), "CLOB WS idle keepalive");
        assert_eq!(lane.idle_threshold(), Duration::from_secs(60), "silent-stall watchdog");
        assert_eq!(USER_DATA_POLL, Duration::from_secs(1), "stop-poll / socket read timeout");
        assert_eq!(USER_DATA_MAX_BACKOFF, Duration::from_secs(30), "reconnect ceiling");
        assert_eq!(RESYNC_SETTLE, Duration::from_secs(1), "resync settle after a re-open");
    }

    #[test]
    fn ws_user_is_the_user_channel() {
        assert!(crate::config::WS_USER.starts_with("wss://"));
        assert!(crate::config::WS_USER.ends_with("/user"));
    }

    #[test]
    fn resync_api_is_exposed() {
        // Type-level proof the resync entry points exist with the expected shapes (no I/O): the
        // generic fn coerces to a concrete fn pointer, and the returned handle exposes `shutdown`.
        #[expect(clippy::type_complexity)]
        let f: fn(
            String,
            PolymarketCreds,
            Vec<String>,
            PolymarketRegistry,
            EventSender,
            fn() -> Vec<Event>,
            Option<FillTracker>,
        ) -> UserDataResyncFeed = spawn_polymarket_user_data_with_resync_tracked;
        let _ = f;
        let _shutdown: fn(
            UserDataResyncFeed,
        ) -> Result<(), vike_bridge_core::user_data::UserDataAuthError> =
            UserDataResyncFeed::shutdown;
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
