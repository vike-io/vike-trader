//! Aster USDⓈ-M futures (fapi) listenKey user-data WS — the venue face of the shared
//! [`vike_binance::family::listenkey`] pump.
//!
//! Aster's perp user-data transport is a Binance fork (mint key → connect `wss://<ws_base>/ws/
//! <listenKey>` → NO subscribe frame, NO ack handshake → stream ORDER_TRADE_UPDATE → best-effort
//! bounded PUT keepalive every ~30 min → never DELETE), so this module's body — previously a
//! near-verbatim copy of Binance's `perp_user_data.rs` — now lives once in the Binance-wire-grammar
//! core and this crate passes `"aster"` (F13, dedup rung 4a).
//!
//! What stays HERE is the genuine per-venue delta: the v3-SIGNED auth
//! ([`AsterListenKeyAuth`](crate::listenkey_auth::AsterListenKeyAuth), shared with the spot pump),
//! the `/fapi/v3/listenKey` endpoint, this crate's own `urls::urls_for(env)` REST resolution (Aster's
//! env threading is invisible to the shared core), and the `/ws` WS segment Binance perp does not
//! have — a REAL second divergence, parameterized rather than unified.

use std::sync::mpsc;
use std::time::Duration;

use vike_binance::family::listenkey::{self, PumpSpec};
use vike_bridge_core::Environment;
use vike_bridge_core::credentials::Credentials;
use vike_exec::EventSender;
use vike_model::events::Event;

use crate::listenkey_auth::AsterListenKeyAuth;
use crate::perp_mapper::map_aster_perp;
use crate::urls;

/// The v3 listenKey endpoint (perp / fapi). SIGNED (unlike Binance's apiKey-header path).
pub const LISTENKEY_PATH: &str = "/fapi/v3/listenKey";

/// Silent-stall watchdog window — 4x Aster's MEASURED server-ping interval.
///
/// This family has NO app-level ping (the keepalive is a REST listenKey PUT that never touches the
/// socket), so the only inbound traffic on an idle account is the venue's own server ping. MEASURED
/// (`tests/aster_userdata_soak.rs`, 900s window against the perp stream): gaps of 11s (connect
/// offset), 299.998s, 300.004s — a 300s cadence.
///
/// ⚠ **Aster is not Binance, and neither Binance is the other.** Three venues share this one pump
/// and their server pings are 20s (binance spot), 180s (binance perp) and 300s (aster) — a 15x
/// spread. Every plausible inference between them is wrong: the 60s value four other venues use
/// would tear down a healthy aster socket FIVE times per ping period, forever.
///
/// Aster **spot** (sapi/sstream) is a different stream and remains unmeasured — see `user_data.rs`.
const IDLE_THRESHOLD: Duration = Duration::from_secs(1200);

/// The pump/thread identity + endpoints for one perp user-data stream. The fapi REST base is
/// resolved from `env`; the WS base is passed in (`urls_for(env).fapi_ws`).
fn spec(env: Environment, ws_base_url: String) -> PumpSpec {
    PumpSpec {
        venue: "aster",
        thread_tag: "aster-perp-userdata",
        resync_tag: "aster-perp-resync",
        rest_url: urls::urls_for(env).fapi_rest.to_string(),
        ws_base_url,
        ws_path: "/ws", // aster connects `<ws_base>/ws/<key>` — binance perp has no such segment
        listenkey_path: LISTENKEY_PATH,
        idle_threshold: Some(IDLE_THRESHOLD),
    }
}

/// Spawn the persistent perp fill-stream pump feeding the vt-core ingest (lossless). The
/// [`AsterSigner`](crate::signing::AsterSigner) is NOT `Clone`, so it is built ONCE inside the pump
/// thread from `creds` (which is moved in) and borrowed by both the connect and keepalive.
pub fn spawn_aster_perp_user_data(
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
        move |frame| map_aster_perp(frame, "aster", &sym),
        events,
    )
}

/// Join half of the perp pump + its audit-A3 resync supervisor (see
/// [`spawn_aster_perp_user_data_with_resync`]); thin alias for the shared
/// [`UserDataResyncFeed`](vike_bridge_core::user_data::UserDataResyncFeed).
pub type AsterPerpUserDataResync = vike_bridge_core::user_data::UserDataResyncFeed;

/// Like [`spawn_aster_perp_user_data`], plus the audit-A3 resync supervisor. On every WS re-open the
/// pump bumps a generation counter, and the supervisor (after a settle delay) replays recent fapi
/// order history via `resync_fetch` so a fill/cancel/expire/reject that landed during the reconnect
/// gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST history
/// source (typically `get_all_orders` + `get_user_trades` →
/// [`map_aster_perp_history`](crate::history::map_aster_perp_history)) on a SEPARATE `AsterPerpRest`
/// from the order-submit client. Both threads tear down on `shutdown`; the supervisor also
/// self-exits if the pump dies or the core is gone. (Exercised by the live smoke, like the other
/// spawn functions.)
///
/// `on_reconcile` is threaded straight to the shared pump's `on_reconcile` param — `Some` pokes the
/// reconcile driver after every reconnect's event-replay settles; `None` (every non-recon caller,
/// e.g. the exec driver) reproduces the event-replay-only behavior.
pub fn spawn_aster_perp_user_data_with_resync(
    env: Environment,
    creds: Credentials,
    ws_base_url: String,
    symbol: String,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
) -> AsterPerpUserDataResync {
    let sym = symbol.clone();
    listenkey::spawn_user_data_with_resync(
        spec(env, ws_base_url),
        symbol,
        move || AsterListenKeyAuth::new(&creds, LISTENKEY_PATH),
        move |frame| map_aster_perp(frame, "aster", &sym),
        events,
        resync_fetch,
        on_reconcile,
    )
}

#[cfg(test)]
mod perp_user_data_tests {
    use super::*;

    #[test]
    fn spec_pins_aster_perp_wire_shape() {
        let s = spec(Environment::Live, "wss://fstream.asterdex.com".into());
        assert_eq!(s.listenkey_path, "/fapi/v3/listenKey", "aster fapi listenKey is v3, NOT v1");
        assert_eq!(s.ws_path, "/ws", "aster carries the /ws segment binance perp lacks");
        assert_eq!(s.rest_url, "https://fapi.asterdex.com");
        assert_eq!(s.venue, "aster");
    }
}
