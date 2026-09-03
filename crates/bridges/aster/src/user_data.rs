//! Aster spot (sapi) listenKey user-data WS — the venue face of the shared
//! [`vike_binance::family::listenkey`] pump.
//!
//! Aster spot ALSO uses the listenKey model (Binance-verbatim events), so it runs the SAME pump as
//! the perp stream — NOT Binance spot's WS-API subscribe/ack pump, which is a different mechanism
//! entirely. Mint key → connect `wss://<ws_base>/ws/<listenKey>` → NO subscribe frame, NO ack
//! handshake → stream `executionReport` / `outboundAccountPosition` → best-effort bounded PUT
//! keepalive every ~30 min → never DELETE. That body — previously a third near-verbatim copy of
//! Binance's `perp_user_data.rs` — now lives once in the Binance-wire-grammar core (F13, dedup rung
//! 4a).
//!
//! What stays HERE is the genuine per-venue delta: the v3-SIGNED auth
//! ([`AsterListenKeyAuth`](crate::listenkey_auth::AsterListenKeyAuth), shared with the perp pump),
//! the `/api/v3/listenKey` endpoint, this crate's own `urls::urls_for(env)` REST resolution, and the
//! `/ws` WS segment.

use std::sync::mpsc;
use std::time::Duration;

use vike_binance::family::listenkey::{self, PumpSpec};
use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::Environment;
use vike_exec::EventSender;
use vike_model::events::Event;

use crate::event_mapper::map_aster_private;
use crate::listenkey_auth::AsterListenKeyAuth;
use crate::urls;

/// The v3 listenKey endpoint (spot / sapi). SIGNED (unlike Binance's apiKey-header path).
pub const LISTENKEY_PATH: &str = "/api/v3/listenKey";

/// Silent-stall watchdog window — 4x this lane's MEASURED server-ping interval.
///
/// This family has no app-level ping (the keepalive is a REST listenKey PUT that never touches the
/// socket), so the venue's own server ping is the only liveness an idle account produces. MEASURED
/// (`tests/aster_userdata_soak.rs`, 900s window against sstream): gaps of 55s (connect offset),
/// 300.001s, 299.999s — a 300s cadence, matching the perp lane.
///
/// That match is a RESULT, not an assumption: it was measured separately precisely because Binance
/// proves the two lanes of one venue can diverge (spot 20s vs perp 180s). Here they agree; there
/// they do not.
const IDLE_THRESHOLD: Duration = Duration::from_secs(1200);

/// The pump/thread identity + endpoints for one spot user-data stream. The sapi REST base is
/// resolved from `env`; the WS base is passed in (`urls_for(env).sapi_ws`).
fn spec(env: Environment, ws_base_url: String) -> PumpSpec {
    PumpSpec {
        venue: "aster",
        thread_tag: "aster-userdata",
        resync_tag: "aster-resync",
        rest_url: urls::urls_for(env).sapi_rest.to_string(),
        ws_base_url,
        ws_path: "/ws",
        listenkey_path: LISTENKEY_PATH,
        idle_threshold: Some(IDLE_THRESHOLD),
    }
}

/// Spawn the persistent spot fill-stream pump feeding the vt-core ingest (lossless). The
/// [`AsterSigner`](crate::signing::AsterSigner) is NOT `Clone`, so it is built ONCE inside the pump
/// thread from `creds` (which is moved in) and borrowed by both the connect and keepalive.
pub fn spawn_aster_user_data(
    env: Environment,
    creds: Credentials,
    ws_base_url: String,
    symbol: String,
    events: EventSender,
) -> vike_bridge_core::user_data::UserDataFeed {
    let sym = symbol.clone();
    listenkey::spawn_user_data(
        spec(env, ws_base_url),
        symbol,
        move || AsterListenKeyAuth::new(&creds, LISTENKEY_PATH),
        move |frame| map_aster_private(frame, "aster", &sym),
        events,
    )
}

/// Join half of the spot pump + its audit-A3 resync supervisor (see
/// [`spawn_aster_user_data_with_resync`]); thin alias for the shared
/// [`UserDataResyncFeed`](vike_bridge_core::user_data::UserDataResyncFeed).
pub type AsterUserDataResync = vike_bridge_core::user_data::UserDataResyncFeed;

/// Like [`spawn_aster_user_data`], plus the audit-A3 resync supervisor. On every WS re-open the pump
/// bumps a generation counter, and the supervisor (after a settle delay) replays recent order
/// history via `resync_fetch` so a terminal (fill/cancel/expire/reject) that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST
/// history source (typically the `get_all_orders`/`get_my_trades` fetch →
/// [`map_aster_history`](crate::history::map_aster_history)) on a SEPARATE `AsterSpotRest` from the
/// order-submit client. Both threads tear down on `shutdown`; the supervisor also self-exits if the
/// pump dies or the core is gone. (Exercised by the live smoke, like the other spawn functions.)
///
/// `on_reconcile` is threaded straight to the shared pump's `on_reconcile` param — `Some` pokes the
/// reconcile driver after every reconnect's event-replay settles; `None` (every non-recon caller,
/// e.g. the exec driver) reproduces the event-replay-only behavior.
pub fn spawn_aster_user_data_with_resync(
    env: Environment,
    creds: Credentials,
    ws_base_url: String,
    symbol: String,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
) -> AsterUserDataResync {
    let sym = symbol.clone();
    listenkey::spawn_user_data_with_resync(
        spec(env, ws_base_url),
        symbol,
        move || AsterListenKeyAuth::new(&creds, LISTENKEY_PATH),
        move |frame| map_aster_private(frame, "aster", &sym),
        events,
        resync_fetch,
        on_reconcile,
    )
}

#[cfg(test)]
mod user_data_tests {
    use super::*;

    #[test]
    fn spec_pins_aster_spot_wire_shape() {
        let s = spec(Environment::Live, "wss://sstream.asterdex.com".into());
        assert_eq!(s.listenkey_path, "/api/v3/listenKey", "spot listenKey is the sapi v3 path");
        assert_eq!(s.ws_path, "/ws");
        assert_eq!(s.rest_url, "https://sapi.asterdex.com");
        // Distinct from the perp pump's threads — two streams can run for the same symbol.
        assert_eq!(s.thread_tag, "aster-userdata");
        assert_eq!(s.resync_tag, "aster-resync");
    }
}
