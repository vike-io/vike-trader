//! Aster spot (sapi) listenKey user-data WS — the venue face of the shared
//! [`vike_binance::family::execution::user_data`] pump.
//!
//! Aster spot ALSO uses the listenKey model (Binance-verbatim events), so it runs the SAME pump as
//! the perp stream — NOT Binance spot's WS-API subscribe/ack pump, which is a different mechanism
//! entirely. Mint key → connect `wss://<ws_base>/ws/<listenKey>` → NO subscribe frame, NO ack
//! handshake → stream `executionReport` / `outboundAccountPosition` → bounded PUT keepalive every
//! ~30 min, a key it cannot renew ending the session → never DELETE. That body — previously a third
//! near-verbatim copy of Binance's `execution/user_data/perp.rs` — now lives once in the Binance-wire-grammar core (F13, dedup rung
//! 4a).
//!
//! What stays HERE is the genuine per-venue delta: the v3-SIGNED auth
//! ([`ListenKeyAuthenticationAster`](crate::transport::listenkey_auth::ListenKeyAuthenticationAster), shared with the perp pump),
//! the `/api/v3/listenKey` endpoint, the account's own exec hosts (the caller's
//! [`crate::transport::urls::AsterUrls::exec`]: the sapi REST base and the sstream WS base), and the `/ws` WS
//! segment.

use std::sync::{Arc, mpsc};

use vike_binance::family::execution::user_data::{self, WebsocketSessionUserSpecBinance};
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::signer::VenueClock;
use vike_execution::EventSender;
use vike_model::events::Event;
use vike_secrets::credentials::Credentials;

use crate::execution::event_mapper::spot::map_aster_private;
use crate::transport::listenkey_auth::ListenKeyAuthenticationAster;

/// The v3 listenKey endpoint (spot / sapi). SIGNED (unlike Binance's apiKey-header path).
pub const LISTENKEY_PATH: &str = "/api/v3/listenKey";

/// The pump/thread identity + endpoints for one spot user-data stream. Both bases are passed in,
/// resolved by the caller: the sapi REST base (`rest_url`, the listenKey mint) and the sstream WS
/// base (`websocket_base_url`), the account's `AsterUrls::exec`.
///
/// **The silent-stall watchdog window is `vike_venues::venue_timings::ASTER_SPOT`'s exec lane
/// `idle_threshold`**, with its reason: 4x this lane's MEASURED server-ping interval, the one inbound
/// frame the venue sends unasked on an idle account. MEASURED (`tests/aster_userdata_soak.rs`, 900s
/// window against sstream): gaps of 55s (connect offset), 300.001s, 299.999s — a 300s cadence, matching
/// the perp lane.
///
/// **`socket_keepalive` is the perp lane's cure (`execution/user_data/perp.rs`'s `spec`) on the row both lanes
/// share.** Only the perp socket's 240 s timer was measured; this lane takes it because a frame a
/// minute costs nothing, and a spot socket with the same timer would cost a fill stream.
///
/// That match is a RESULT, not an assumption: it was measured separately precisely because Binance
/// proves the two lanes of one venue can diverge (spot 20s vs perp 180s). Here they agree; there
/// they do not.
fn spec(rest_url: String, websocket_base_url: String) -> WebsocketSessionUserSpecBinance {
    WebsocketSessionUserSpecBinance {
        venue: "aster",
        lane: "aster",
        rest_url,
        websocket_base_url,
        websocket_path: "/ws",
        listenkey_path: LISTENKEY_PATH,
        idle_threshold: Some(vike_venues::venue_timings::ASTER_SPOT.exec().idle_threshold()),
        socket_keepalive: Some(vike_venues::venue_timings::ASTER_SPOT.exec().keepalive_every()),
    }
}

/// Spawn the persistent spot fill-stream pump feeding the th-core ingest (lossless), with the audit-A3
/// resync supervisor. The [`SignerAster`](crate::transport::sign::SignerAster) is NOT `Clone`, so its listenKey auth is
/// built ONCE here from `creds` and shared by the connect and the keepalive. On every WS re-open the pump
/// bumps a generation counter, and the supervisor (after a settle delay) replays recent order
/// history via `resync_fetch` so a terminal (fill/cancel/expire/reject) that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST
/// history source (typically the `get_all_orders`/`get_my_trades` fetch →
/// [`map_aster_history`](crate::execution::resync::map_aster_history)) on a SEPARATE `VenueRestAsterSpot` from the
/// order-submit client. Both threads tear down on `shutdown`; the supervisor also self-exits if the
/// pump dies or the core is gone. (Exercised by the live smoke.)
///
/// `on_reconcile` is threaded straight to the shared pump's `on_reconcile` param — `Some` fires the
/// recon trigger after every reconnect's event-replay settles; `None` (every non-recon caller,
/// e.g. the exec driver) reproduces the event-replay-only behavior.
pub fn spawn_aster_user_data_with_resync(
    rest_url: String,
    creds: Credentials,
    websocket_base_url: String,
    symbol: String,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
    clock: VenueClock,
) -> vike_bridge_core::websocket_session::user::WebsocketSessionUserResyncHandle {
    let owned_symbol = symbol.clone();
    user_data::spawn_user_data_with_resync(
        spec(rest_url, websocket_base_url),
        symbol,
        ListenKeyAuthenticationAster::new(&creds, LISTENKEY_PATH, &clock),
        move |frame| map_aster_private(frame, "aster", &owned_symbol),
        events,
        fence,
        resync_fetch,
        on_reconcile,
    )
}

#[cfg(test)]
mod user_data_tests {
    use std::time::Duration;

    use super::*;

    /// The spec the exec plane builds for a `live` account on the seed's rows.
    fn live_spec() -> WebsocketSessionUserSpecBinance {
        let u = crate::transport::urls::AsterUrls::exec(
            &vike_venues::venue_urls::VenueUrls::seed(crate::transport::urls::VENUE),
            vike_model::AccountTier::Live,
        )
        .unwrap();
        spec(u.sapi_rest.to_string(), u.sapi_websocket.to_string())
    }

    #[test]
    fn spec_pins_aster_spot_wire_shape() {
        let s = live_spec();
        assert_eq!(s.listenkey_path, "/api/v3/listenKey", "spot listenKey is the sapi v3 path");
        assert_eq!(s.websocket_path, "/ws");
        assert_eq!(s.rest_url, "https://sapi.asterdex.com");
        assert_eq!(s.websocket_base_url, "wss://sstream.asterdex.com");
        // Distinct from the perp pump's threads — two streams can run for the same symbol.
        assert_eq!(s.lane, "aster", "threads aster-userdata-<symbol> and aster-resync-<symbol>");
    }

    /// The watchdog window the spot pump is handed, as a literal: 4x the MEASURED 300 s server ping
    /// (`ASTER_SPOT`'s exec lane in `vike_venues::venue_timings`). A retune is a deliberate
    /// edit here too, beside the matrix pin in vike-model.
    #[test]
    fn spec_arms_the_watchdog_at_four_server_pings() {
        let s = live_spec();
        assert_eq!(s.idle_threshold, Some(Duration::from_secs(1200)));
    }

    /// The socket's cadence the spot pump is handed, as a literal: the perp lane's.
    #[test]
    fn spec_keeps_the_socket_talking_inside_the_venues_timer() {
        let s = live_spec();
        assert_eq!(s.socket_keepalive, Some(Duration::from_secs(60)));
    }

    /// The shared pump PUTs the listenKey on ONE cadence for every family venue (it reads binance
    /// perp's row — see `vike_binance::family::execution::user_data`'s tests), so this lane's row must declare
    /// the same thirty minutes, a literal here beside the matrix pin in vike-model.
    #[test]
    fn the_row_declares_the_cadence_the_shared_pump_renews_the_key_on() {
        let exec = vike_venues::venue_timings::ASTER_SPOT.exec();
        assert_eq!(exec.session_renew_every(), Duration::from_secs(1800));
    }
}
