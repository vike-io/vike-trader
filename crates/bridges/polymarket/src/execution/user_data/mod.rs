//! Polymarket authenticated user-channel WS pump. Connects the CLOB user stream, sends the L2-auth
//! subscribe frame, then hands the socket to the shared venue-neutral pump, built from one
//! `vike_bridge_core::websocket_session::user::WebsocketSessionUserResyncSpec` by `spawn_user_with_resync` (its
//! `user_data_pump` span carries `venue = "polymarket"`, and its failed-open line is paced, owner ruling
//! B-13), with the silent-stall watchdog armed — the exec lane of
//! `vike_venues::venue_timings::POLYMARKET` declares its threshold and why —
//! decoding fills/orders via [`decode_user`] and re-keying CLOB ids → order client ids through the shared
//! [`OrderVenueIdTracker`]. Feeds the th-core ingest LOSSLESSLY (fills must never drop). Like the
//! other venues' pumps this is exercised end-to-end by a live smoke (real-money mainnet + proxy).
//!
//! The dial is the shared `vike_transport::websocket_proxy::connect_websocket`, so this lane rides the same SOCKS tunnel as the
//! REST lane whenever [`crate::transport::websocket_proxy`] answers one — whenever the REST lane proxies,
//! which is OFF by default (`venue.polymarket.proxy_enabled = true` turns it on), with
//! `venue.polymarket.ws_proxy_enabled` as the per-lane override. Direct or through the tunnel, it spends `vike_bridge_core::websocket_session::spec`'s
//! `CONNECT_10S`: a hung tunnel fails the open into the pump's reconnect, and one that answers is
//! not cut. (The override was the `POLY_WS_PROXY_ENABLED` variable until decision 0095.)

pub(crate) mod decode;

use std::sync::atomic::{AtomicBool, Ordering};

use vike_bridge_core::ratelimit::RateGatePerKey;
use vike_bridge_core::websocket::{WebsocketSessionUserStreamTungstenite, send_gated};
use vike_bridge_core::websocket_session::spec::CONNECT_10S;
use vike_execution::EventSender;
use vike_model::events::Event;
use vike_transport::websocket::configure_websocket_stream;
use vike_venues::venue_timings;

use crate::config::PolymarketConfig;
use crate::execution::fill_tracker::FillTracker;
use crate::execution::registry::OrderVenueIdTracker;
use crate::execution::user_data::decode::{decode_user_with_tracker, user_subscribe_message};
// The shared pump scaffold is a DIRECT bridge-core path — this file is itself the crate's
// `user_data` module, so a `crate::execution::user_data::` spelling would be a self-reference (the same
// name-collision trap the deribit/okx/binance moves documented).
use vike_bridge_core::websocket_session::user::{
    USER_DATA_POLL, WebsocketSessionUserOpenOutcome, WebsocketSessionUserResyncHandle,
    WebsocketSessionUserResyncSpec, spawn_user_with_resync,
};
use vike_venues::reconnect::ReconnectBackoff;

// No cadence is declared in this file. The venue's own — the CLOB idle keepalive (a text PING) and
// the silent-stall watchdog — are the exec lane of `venue_timings::POLYMARKET`, read where each is
// used in `spawn_polymarket_user_data_with_resync_tracked`; the row carries their reasons (the
// watchdog is keyed off the server-answer cadence, NOT data cadence: a wallet with no orders is
// legitimately silent, and on Polymarket EVERY post-acceptance terminal arrives ONLY on this
// channel, so a silently-stalled socket strands orders outright), and so is the reconnect ladder. The
// venue-free ones are `vike_bridge_core::websocket_session::user`'s `USER_DATA_POLL` (the stop-poll and socket
// read timeout) and `RESYNC_SETTLE` (let the reopened WS drain its backlog before the resync replays), which
// `spawn_user_with_resync` applies.

/// The user channel's keepalive text, sent at the row's `keepalive_every` (the link harness,
/// `crates/vike-bridge-core/tests/link_conformance.rs`, sends it at that cadence too).
pub const USER_CHANNEL_PING: &str = "PING";

/// Connect `wss://…/user` and send the L2-auth subscribe frame. Connected+subscribed = ready
/// (the user channel begins streaming on subscribe; no ack handshake).
pub fn open_polymarket_user_data_websocket(
    websocket_url: &str,
    creds: &PolymarketConfig,
    markets: &[String],
    stop: &AtomicBool,
    gate: &RateGatePerKey,
) -> WebsocketSessionUserOpenOutcome<WebsocketSessionUserStreamTungstenite> {
    if stop.load(Ordering::Relaxed) {
        return WebsocketSessionUserOpenOutcome::Stopped;
    }
    // Optional SOCKS5 egress (spec §0.1), resolved per connect so a reconnect picks up a changed
    // tunnel. The window is the direct arm's (the module doc).
    let proxy = crate::transport::websocket_proxy();
    let mut socket = match vike_transport::websocket_proxy::connect_websocket(
        websocket_url,
        proxy.as_ref(),
        Some(CONNECT_10S),
    ) {
        Ok(ok) => ok,
        Err(e) => return WebsocketSessionUserOpenOutcome::Transport(format!("connect: {e}")),
    };
    configure_websocket_stream(&socket, USER_DATA_POLL);
    let sub = user_subscribe_message(creds, markets); // carries the L2 creds — NEVER log
    if let Err(e) = send_gated(&mut socket, Some(gate), "subscribe", &sub) {
        let _ = socket.close(None);
        return WebsocketSessionUserOpenOutcome::Transport(format!("subscribe send: {e:?}"));
    }
    WebsocketSessionUserOpenOutcome::Ready(WebsocketSessionUserStreamTungstenite(socket))
}

/// Spawn the persistent Polymarket user-channel pump feeding the th-core ingest (lossless), plus the
/// audit-A3 resync supervisor: on every WS re-open the pump bumps a generation counter and the
/// supervisor (after a settle delay) replays recent history via `resync_fetch` (typically
/// `get_trades` + `get_orders` →
/// [`map_polymarket_history`](crate::execution::resync::map_polymarket_history)) on a SEPARATE
/// client, so a fill/cancel that landed during the reconnect gap — or a MATCHED-then-FAILED
/// divergence — is recovered; the core dedups the overlap. Both threads tear down on `shutdown`; the
/// supervisor self-exits if the pump dies or the core is gone. (Exercised by a live smoke.)
///
/// Pass the SAME `registry` clone the exec client records accepted orders in, so decoded
/// fills/cancels re-key back to the order client id. `tracker` is the optional dust-snap [`FillTracker`]
/// ([`crate::execution::fill_tracker`] — cent-tick overfill/residual reconciliation): pass the
/// SAME clone the exec client registers submitted quantities into. The resync replay itself
/// decodes UNTRACKED (it overlaps live events the tracker already folded); only the live pump
/// snaps. `None` snaps nothing.
///
/// The ONE user-channel spawner; production starts it from inside the exec thread
/// ([`spawn_live`](crate::execution::spawn_live)).
pub fn spawn_polymarket_user_data_with_resync_tracked(
    websocket_url: String,
    creds: PolymarketConfig,
    markets: Vec<String>,
    registry: OrderVenueIdTracker,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    tracker: Option<FillTracker>,
) -> WebsocketSessionUserResyncHandle {
    // Built once per pump; persists across reconnects (spec §A keyed WS gate).
    let websocket_gate = crate::transport::ratelimit::ws::websocket_rate_gate();
    let ping: Box<dyn FnMut(&mut WebsocketSessionUserStreamTungstenite) + Send> = Box::new(|ws| {
        let _ = ws.send_text(USER_CHANNEL_PING);
    });
    spawn_user_with_resync(WebsocketSessionUserResyncSpec {
        venue: crate::VENUE,
        // The threads stay `polymarket-userdata` / `polymarket-resync`: the stream is the whole wallet's.
        lane: "polymarket",
        symbol: String::new(),
        events,
        // No shared fence: an event that beats its order's accept is parked in the registry
        // (`crate::execution::pending_events`) and replayed by the exec thread.
        fence: None,
        resync_fetch,
        on_reconcile: None, // Task 13: no reconcile trigger wired yet — event-replay only
        open: move |stop: &AtomicBool| {
            open_polymarket_user_data_websocket(
                &websocket_url,
                &creds,
                &markets,
                stop,
                &websocket_gate,
            )
        },
        decode: move |frame: &serde_json::Value| {
            decode_user_with_tracker(frame, &registry, tracker.as_ref())
        },
        notice: None,
        keepalive: Some((venue_timings::POLYMARKET.exec().keepalive_every(), ping)),
        idle_threshold: Some(venue_timings::POLYMARKET.exec().idle_threshold()),
        backoff: ReconnectBackoff::from_row(&venue_timings::POLYMARKET.exec().reconnect()),
        history_lag: venue_timings::POLYMARKET.exec().history_lag.map(|lag| lag.value),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use vike_bridge_core::websocket_session::user::RESYNC_SETTLE;

    /// The cadences the pump hands the shared driver, as EFFECTIVE values. The CLOB keepalive, the
    /// silent-stall watchdog and the reconnect ladder are the exec lane of `venue_timings::POLYMARKET`
    /// (read inline at the call); the poll and the resync settle are
    /// `vike_bridge_core::websocket_session::user`'s pump defaults. A retune of any of them is a deliberate edit
    /// to the literal here.
    #[test]
    fn the_pump_runs_on_the_declared_cadences() {
        let lane = venue_timings::POLYMARKET.exec();
        assert_eq!(lane.keepalive_every(), Duration::from_secs(10), "CLOB WS idle keepalive");
        assert_eq!(lane.idle_threshold(), Duration::from_secs(60), "silent-stall watchdog");
        assert_eq!(USER_DATA_POLL, Duration::from_secs(1), "stop-poll / socket read timeout");
        assert_eq!(lane.reconnect().max, Duration::from_secs(30), "reconnect ceiling");
        assert_eq!(RESYNC_SETTLE, Duration::from_secs(1), "resync settle after a re-open");
    }

    /// The direct dial of the fill pump is bounded through the handshake, like
    /// `crates/bridges/deribit/tests/offline/dial_bound.rs`'s: this channel alone carries a resting
    /// order's end, and a peer that accepts and says nothing would hold the pump for ever. A unit test
    /// because the open is `pub(crate)`.
    #[test]
    fn a_silent_peer_cannot_hold_the_fill_pump_open() {
        crate::execution::test_support::declare_direct_egress();
        let creds = PolymarketConfig {
            private_key: "0x01".to_string().into(),
            address: "0x02".into(),
            api_key: "key".to_string().into(),
            secret: "secret".to_string().into(),
            passphrase: "passphrase".to_string().into(),
            relayer_key: String::new().into(),
            relayer_address: String::new(),
        };
        vike_bridge_core::silent_peer::open_silent_peer(move |url| {
            open_polymarket_user_data_websocket(
                &url,
                &creds,
                &[],
                &AtomicBool::new(false),
                &crate::transport::ratelimit::ws::websocket_rate_gate(),
            )
        });
    }

    #[test]
    fn websocket_user_is_the_user_channel() {
        let seed = vike_venues::venue_urls::VenueUrls::seed(crate::VENUE);
        let url = crate::config::user_websocket_url(&seed).expect("the seed's user row");
        assert!(url.starts_with("wss://"));
        assert!(url.ends_with("/user"));
    }

    #[test]
    fn resync_api_is_exposed() {
        // Type-level proof the resync entry points exist with the expected shapes (no I/O): the
        // generic fn coerces to a concrete fn pointer, and the returned handle exposes `shutdown`.
        #[expect(clippy::type_complexity)]
        let f: fn(
            String,
            PolymarketConfig,
            Vec<String>,
            OrderVenueIdTracker,
            EventSender,
            fn() -> Vec<Event>,
            Option<FillTracker>,
        ) -> WebsocketSessionUserResyncHandle = spawn_polymarket_user_data_with_resync_tracked;
        let _ = f;
        let _shutdown: fn(
            WebsocketSessionUserResyncHandle,
        ) -> Result<
            (),
            vike_bridge_core::websocket_session::user::WebsocketSessionUserAuthenticationError,
        > = WebsocketSessionUserResyncHandle::shutdown;
    }

    #[test]
    fn open_builds_the_user_subscribe_frame() {
        // The pump's first send is user_subscribe_message(creds, markets); prove it targets "user".
        let creds = PolymarketConfig {
            api_key: "k".to_string().into(),
            secret: "s".to_string().into(),
            passphrase: "p".to_string().into(),
            ..Default::default()
        };
        let frame: serde_json::Value =
            serde_json::from_str(&user_subscribe_message(&creds, &["111".into()])).unwrap();
        assert_eq!(frame["type"], "user");
    }
}
