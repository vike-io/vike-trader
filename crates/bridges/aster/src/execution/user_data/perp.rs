//! Aster USDⓈ-M futures (fapi) listenKey user-data WS — the venue face of the shared
//! [`vike_binance::family::execution::user_data`] pump.
//!
//! Aster's perp user-data transport is a Binance fork (mint key → connect `wss://<ws_base>/ws/
//! <listenKey>` → NO subscribe frame, NO ack handshake → stream ORDER_TRADE_UPDATE → bounded PUT
//! keepalive every ~30 min, a key it cannot renew ending the session → never DELETE), so this
//! module's body — previously a near-verbatim copy of Binance's `execution/user_data/perp.rs` — now lives once in the Binance-wire-grammar
//! core and this crate passes `"aster"` (F13, dedup rung 4a).
//!
//! What stays HERE is the genuine per-venue delta: the v3-SIGNED auth
//! ([`ListenKeyAuthenticationAster`](crate::transport::listenkey_auth::ListenKeyAuthenticationAster), shared with the spot pump),
//! the `/fapi/v3/listenKey` endpoint, the account's own exec hosts (the caller's
//! [`crate::transport::urls::AsterUrls::exec`]: the fapi REST base and the fstream WS base, invisible to the
//! shared core), and the `/ws` WS segment Binance perp does not have — a REAL second divergence,
//! parameterized rather than unified.

use std::sync::{Arc, mpsc};

use vike_binance::family::execution::event_mapper::perp::map_perp_stream;
use vike_binance::family::execution::user_data::{self, WebsocketSessionUserSpecBinance};
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::signer::VenueClock;
use vike_execution::EventSender;
use vike_model::events::Event;
use vike_secrets::credentials::Credentials;

use crate::transport::listenkey_auth::ListenKeyAuthenticationAster;

/// The v3 listenKey endpoint (perp / fapi). SIGNED (unlike Binance's apiKey-header path).
pub const LISTENKEY_PATH: &str = "/fapi/v3/listenKey";

/// The pump/thread identity + endpoints for one perp user-data stream. Both bases are passed in,
/// resolved by the caller: the fapi REST base (`rest_url`, the listenKey mint) and the fstream WS
/// base (`websocket_base_url`), the account's `AsterUrls::exec`.
///
/// **The silent-stall watchdog window is `vike_venues::venue_timings::ASTER_PERP`'s exec lane
/// `idle_threshold`**, with its reason: 4x Aster's MEASURED server-ping interval, the one inbound
/// frame the venue sends unasked on an idle account. MEASURED (`tests/aster_userdata_soak.rs`, 900s
/// window against the perp stream): gaps of 11s (connect offset), 299.998s, 300.004s — a 300s cadence.
///
/// **The socket keeps talking (`socket_keepalive`, the same row's `keepalive_every`)**, because aster's
/// mainnet resets a socket that has sent it nothing for 240 s (the row has the measurement). The answer
/// to its 300 s ping cannot hold that off, and the listenKey PUT never touches the socket.
///
/// ⚠ **Aster is not Binance, and neither Binance is the other.** Three venues share this one pump
/// and their server pings are 20s (binance spot), 180s (binance perp) and 300s (aster) — a 15x
/// spread. Every plausible inference between them is wrong: the 60s value four other venues use
/// would tear down a healthy aster socket FIVE times per ping period, forever.
///
/// Aster **spot** (sapi/sstream) is a different stream and remains unmeasured — see `execution/user_data/spot.rs`.
fn spec(rest_url: String, websocket_base_url: String) -> WebsocketSessionUserSpecBinance {
    WebsocketSessionUserSpecBinance {
        venue: "aster",
        lane: "aster-perp",
        rest_url,
        websocket_base_url,
        websocket_path: "/ws", // aster connects `<ws_base>/ws/<key>` — binance perp has no such segment
        listenkey_path: LISTENKEY_PATH,
        idle_threshold: Some(vike_venues::venue_timings::ASTER_PERP.exec().idle_threshold()),
        socket_keepalive: Some(vike_venues::venue_timings::ASTER_PERP.exec().keepalive_every()),
    }
}

/// Spawn the persistent perp fill-stream pump feeding the th-core ingest (lossless), with the audit-A3
/// resync supervisor. The [`SignerAster`](crate::transport::sign::SignerAster) is NOT `Clone`, so its listenKey auth is
/// built ONCE here from `creds` and shared by the connect and the keepalive. On every WS re-open the
/// pump bumps a generation counter, and the supervisor (after a settle delay) replays recent fapi
/// order history via `resync_fetch` so a fill/cancel/expire/reject that landed during the reconnect
/// gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST history
/// source (typically `get_all_orders` + `get_user_trades` →
/// [`map_aster_perp_history`](crate::execution::resync::map_aster_perp_history)) on a SEPARATE `VenueRestAsterPerp`
/// from the order-submit client. Both threads tear down on `shutdown`; the supervisor also
/// self-exits if the pump dies or the core is gone. (Exercised by the live smoke.)
///
/// `on_reconcile` is threaded straight to the shared pump's `on_reconcile` param — `Some` fires the
/// recon trigger after every reconnect's event-replay settles; `None` (every non-recon caller,
/// e.g. the exec driver) reproduces the event-replay-only behavior.
pub fn spawn_aster_perp_user_data_with_resync(
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
        // Aster stamps no broker prefix: no link id.
        move |frame| map_perp_stream(frame, "aster", &owned_symbol, None),
        events,
        fence,
        resync_fetch,
        on_reconcile,
    )
}

#[cfg(test)]
mod perp_user_data_tests {
    use std::time::Duration;

    use super::*;

    /// The family's fix proven over this venue's wrapper: a fill's wallet statement is not folded, any other wallet
    /// frame is (`vike_binance::family::execution::event_mapper::perp`'s `is_fill_wallet_frame`).
    #[test]
    fn the_stream_leaves_out_the_wallet_statement_of_a_fill() {
        let frame = |reason: &str| {
            serde_json::json!({
                "e": "ACCOUNT_UPDATE", "T": 7, "a": { "m": reason, "B": [{ "a": "USDT", "wb": "9.5" }] },
            })
        };
        assert!(map_perp_stream(&frame("ORDER"), "aster", "BTCUSDT.P", None).is_empty());
        assert!(matches!(
            map_perp_stream(&frame("DEPOSIT"), "aster", "BTCUSDT.P", None).as_slice(),
            [Event::AccountState(_)]
        ));
    }

    /// The spec the exec plane builds for a `live` account on the seed's rows.
    fn live_spec() -> WebsocketSessionUserSpecBinance {
        let u = crate::transport::urls::AsterUrls::exec(
            &vike_venues::venue_urls::VenueUrls::seed(crate::transport::urls::VENUE),
            vike_model::AccountTier::Live,
        )
        .unwrap();
        spec(u.fapi_rest.to_string(), u.fapi_websocket.to_string())
    }

    #[test]
    fn spec_pins_aster_perp_wire_shape() {
        let s = live_spec();
        assert_eq!(s.listenkey_path, "/fapi/v3/listenKey", "aster fapi listenKey is v3, NOT v1");
        assert_eq!(s.websocket_path, "/ws", "aster carries the /ws segment binance perp lacks");
        assert_eq!(s.rest_url, "https://fapi.asterdex.com");
        assert_eq!(s.websocket_base_url, "wss://fstream.asterdex.com");
        assert_eq!(s.venue, "aster");
    }

    /// The watchdog window the perp pump is handed, as a literal: 4x the MEASURED 300 s server ping
    /// (`ASTER_PERP`'s exec lane in `vike_venues::venue_timings`). A retune is a deliberate
    /// edit here too, beside the matrix pin in vike-model.
    #[test]
    fn spec_arms_the_watchdog_at_four_server_pings() {
        let s = live_spec();
        assert_eq!(s.idle_threshold, Some(Duration::from_secs(1200)));
    }

    /// The socket's cadence the perp pump is handed, as a literal: well inside the 240 s after which
    /// aster's mainnet resets a socket that has sent it nothing.
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
        let exec = vike_venues::venue_timings::ASTER_PERP.exec();
        assert_eq!(exec.session_renew_every(), Duration::from_secs(1800));
    }
}
