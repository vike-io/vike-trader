//! Binance USDS-M futures (fapi) listenKey user-data WS — the venue face of the shared
//! [`vike_binance::family::execution::user_data`](crate::family::execution::user_data) pump. Exact port of
//! `exec/binance/perp_user_data.py`.
//!
//! The pump shape (mint key → connect `wss://<ws_base>/<listenKey>` → NO subscribe, NO ack → PUT
//! keepalive every ~30 min, bounded, a key it cannot renew ending the session → never DELETE) is
//! Binance wire grammar Aster forked verbatim, so it now lives ONCE in the family core and this module passes `"binance"`
//! (F13, dedup rung 4a). Do NOT copy the spot ack loop here — the fapi stream never acks and it
//! would hang; that warning now guards the shared code too.
//!
//! What stays HERE is the genuine per-venue delta: [`ListenKeyAuthenticationHeader`] — Binance mints the key with the
//! apiKey in an `X-MBX-APIKEY` HEADER and the HMAC signer BYPASSED (no timestamp, no signature),
//! where Aster's v3 endpoint is EIP-712-signed instead. The `fapi` REST/WS hosts stay
//! caller-supplied exactly as before (this module has never resolved them), so the effective URLs
//! are unchanged — `family::execution::user_data`'s own tests pin both venues' exact strings.
//!
//! NOTE the sibling that is NOT this: [`crate::execution::user_data::spot`] is Binance's SPOT user-data path, a
//! WS-API subscribe/ack session — a different mechanism entirely, deliberately not folded in here.

use std::sync::{Arc, mpsc};

use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_execution::EventSender;
use vike_model::events::Event;

use crate::execution::event_mapper::perp::map_binance_perp_stream;
use crate::family::execution::user_data::{
    self, ListenKeyAuthentication, WebsocketSessionUserSpecBinance,
};

pub const LISTENKEY_PATH: &str = "/fapi/v1/listenKey";

/// Whether the TRADE_LITE early-fill hint is on: the exact `"1"` (trimmed) of
/// `venue.binance.trade_lite_fill` in the venue's settings. Read ONCE, by the start, and handed to
/// the pump as a `bool`.
///
/// The setting is the opt-in Binance **TRADE_LITE** early fast-fill hint on the USDⓈ-M perp user
/// stream (default OFF): on, the perp mapper emits an EARLY bare fill from each `TRADE_LITE` frame
/// so inventory skew and the fill-rate breaker react sooner, while the authoritative
/// `ORDER_TRADE_UPDATE` still drives the FSM; off, TRADE_LITE frames are dropped exactly as before
/// (byte-identical). See [`map_perp_opts`](crate::family::execution::event_mapper::perp::map_perp_opts) for the dedup proof and the commission caveat.
/// (It was the `VIKE_BINANCE_TRADE_LITE_FILL` process environment read until decision 0095 — a set
/// variable refuses startup now — and then a credential-map name the settings store folded the row
/// under, until that decision's Task 7 retired the fold.)
pub fn trade_lite_fill(settings: &vike_secrets::venue_setting::VenueSettings) -> bool {
    settings.get(vike_secrets::venue_setting::SettingTier::Any, "trade_lite_fill").map(str::trim)
        == Some("1")
}

/// Binance's listenKey auth — **the sole divergence from Aster's pump**: the apiKey rides in an
/// `X-MBX-APIKEY` header and NOTHING is signed (the listenKey endpoints are the one Binance path
/// where the HMAC signer is bypassed; they never need the secret either — only the key flows in).
pub struct ListenKeyAuthenticationHeader {
    api_key: String,
}

impl ListenKeyAuthenticationHeader {
    /// Build one from the api_key alone (listenKey endpoints never need the secret). Exists so an
    /// out-of-crate caller — the idle-cadence soak in `tests/` — can open a real user-data socket
    /// without duplicating the auth shape.
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

impl ListenKeyAuthentication for ListenKeyAuthenticationHeader {
    fn query(&self, _method: &str) -> String {
        String::new() // header auth — nothing hangs off the URL
    }
    fn header(&self) -> Option<(&'static str, String)> {
        Some(("X-MBX-APIKEY", self.api_key.clone()))
    }
}

/// The pump/thread identity + endpoints for one perp user-data stream.
///
/// **The silent-stall watchdog window is `vike_venues::venue_timings::BINANCE_PERP`'s exec
/// lane `idle_threshold`**, with its reason: 4x Binance's MEASURED server-ping interval. This lane has
/// NO app-level ping (the keepalive is a REST listenKey PUT that never touches the socket), so the
/// only inbound traffic on an idle account is Binance's own server ping. That interval was measured
/// on the demo perp socket — `tests/binance_userdata_soak.rs`, 900s window — at **180.0s**, four
/// consecutive periods within 160ms of each other. 4x it is.
///
/// ⚠ The margin is not decoration. A short threshold here does NOT fail safe: it would tear down
/// and rebuild a perfectly HEALTHY socket every window, forever — at the Class-A value of 60s,
/// three times per server-ping period. That hazard is exactly why this venue was held back when
/// bybit/okx/polymarket/hyperliquid were armed.
///
/// ⚠ Measured on TESTNET (`fstream.binancefuture.com`). If mainnet ever proves to ping slower, the
/// row's `idle_threshold` is the number to raise — and the soak is the way to find out, not
/// inference.
///
/// A COIN-M stream (`coin_m`: the mount is COIN-M, `crate::execution::rest::perp::VenueRestBinancePerp::margining` `Inverse`, whose
/// `dapi` host `fapi_rest_url` then is) mints its key at `crate::execution::rest::coin_m::LISTENKEY_PATH`; the pump, the socket
/// shape and the frames are the same grammar.
fn spec(
    fapi_rest_url: String,
    websocket_base_url: String,
    coin_m: bool,
) -> WebsocketSessionUserSpecBinance {
    let listenkey_path =
        if coin_m { crate::execution::rest::coin_m::LISTENKEY_PATH } else { LISTENKEY_PATH };
    WebsocketSessionUserSpecBinance {
        venue: "binance",
        lane: "binance-perp",
        rest_url: fapi_rest_url,
        websocket_base_url,
        websocket_path: "", // Binance perp connects `<ws_base>/<key>` — no `/ws` segment (Aster has one)
        listenkey_path,
        idle_threshold: Some(vike_venues::venue_timings::BINANCE_PERP.exec().idle_threshold()),
        socket_keepalive: None,
    }
}

/// What a stream folds of one frame's `events`: all of them, except on a COIN-M stream (`coin_m`) its
/// `FUNDING_FEE` rows, which carry no symbol (the mapper labels them with the mount) and which the COIN-M
/// income read books with their own contract and coin (`crate::execution::funding::income_poller`): folding both would book
/// every settlement twice (`Account::apply_funding` is not idempotent).
pub fn coin_m_stream(mut events: Vec<Event>, coin_m: bool) -> Vec<Event> {
    if coin_m {
        events.retain(|e| !matches!(e, Event::Funding(_)));
    }
    events
}

/// Spawn the persistent perp fill-stream pump feeding the th-core ingest (lossless), with the audit-A3
/// resync supervisor. Only the api_key flows in (listenKey endpoints never need the secret). On every WS re-open
/// the pump bumps a generation counter, and the supervisor (after a settle delay) replays recent
/// fapi order history via `resync_fetch` so a fill/cancel/expire/reject that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST
/// history source (typically `get_all_orders` + `get_user_trades` →
/// [`map_binance_perp_history`](crate::execution::resync::map_binance_perp_history)) on a SEPARATE
/// `VenueRestBinancePerp` from the order-submit client; a caller with no replay hands `Vec::new`. Both threads
/// tear down on `shutdown`; the supervisor also self-exits if the pump dies or the core is gone.
/// (Exercised by live smokes.)
///
/// `on_reconcile` (reconciliation-activation Task 7) is threaded straight to the shared pump's
/// `on_reconcile` param — `Some` fires the recon trigger after every reconnect's event-replay
/// settles; `None` (every non-recon caller) reproduces the pre-Task-7 event-replay-only behavior.
///
/// `early_trade_lite_fill` is the `venue.binance.trade_lite_fill` setting — the caller reads it (the start
/// through [`trade_lite_fill`]); `false` drops TRADE_LITE frames. `coin_m` says the hosts are the COIN-M
/// book's (`crate::execution::rest::perp::VenueRestBinancePerp::margining` `Inverse`). `link_id` is the broker link id the
/// account's orders carry, which the decoder reads every echoed client id with.
#[expect(clippy::too_many_arguments)]
pub fn spawn_binance_perp_user_data_with_resync(
    fapi_rest_url: String,
    websocket_base_url: String,
    coin_m: bool,
    api_key: String,
    symbol: String,
    events: EventSender,
    fence: Arc<SubmitFence>,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
    early_trade_lite_fill: bool,
    link_id: Option<String>,
) -> vike_bridge_core::websocket_session::user::WebsocketSessionUserResyncHandle {
    let owned_symbol = symbol.clone();
    let early = early_trade_lite_fill;
    user_data::spawn_user_data_with_resync(
        spec(fapi_rest_url, websocket_base_url, coin_m),
        symbol,
        ListenKeyAuthenticationHeader { api_key },
        move |frame| {
            let events =
                map_binance_perp_stream(frame, "binance", &owned_symbol, early, link_id.as_deref());
            coin_m_stream(events, coin_m)
        },
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

    #[test]
    fn spec_pins_binance_perp_wire_shape() {
        let s = spec("https://fapi.binance.com".into(), "wss://fstream.binance.com".into(), false);
        assert_eq!(s.listenkey_path, "/fapi/v1/listenKey", "fapi listenKey is v1, NOT aster's v3");
        assert_eq!(s.websocket_path, "", "binance perp has NO /ws segment — `<ws_base>/<key>`");
        assert_eq!(s.venue, "binance");
        assert_eq!(s.socket_keepalive, None, "aster's socket timer is aster's");
        // A COIN-M stream mints its key on dapi, in the same socket shape, on either network.
        let urls = vike_venues::venue_urls::VenueUrls::seed("binance");
        for tier in [vike_model::AccountTier::Demo, vike_model::AccountTier::Live] {
            let hosts = crate::execution::BinanceUrls::resolve(&urls, tier).expect("the seed");
            let s = spec(hosts.dapi_rest.into(), hosts.dapi_websocket.into(), true);
            assert_eq!(s.listenkey_path, "/dapi/v1/listenKey", "{tier:?}");
            assert_eq!((s.websocket_path, s.rest_url.as_str()), ("", hosts.dapi_rest));
        }
    }

    /// A COIN-M stream folds no `FUNDING_FEE` row (the income read books it, with its contract and coin); a
    /// USDⓈ-M stream's filter passes every event, and every other event passes on both. The live stream
    /// ([`map_binance_perp_stream`]) already drops the whole funding frame on both books (each book's income
    /// poll books the payment), so the filter is held here on the mapper's unfiltered output.
    #[test]
    fn a_coin_m_stream_leaves_its_funding_to_the_income_read() {
        let frame = serde_json::json!({
            "e": "ACCOUNT_UPDATE", "T": 1_700_000_000_000_i64,
            "a": { "m": "FUNDING_FEE", "B": [ { "a": "BTC", "wb": "0.5", "cw": "0.5", "bc": "-0.0000012" } ] }
        });
        assert!(
            map_binance_perp_stream(&frame, "binance", "BTCUSD_PERP.P", false, None).is_empty()
        );
        let events = crate::execution::event_mapper::perp::map_binance_perp_opts(
            &frame,
            "binance",
            "BTCUSD_PERP.P",
            false,
            None,
        );
        assert!(events.iter().any(|e| matches!(e, Event::Funding(_))), "{events:?}");
        let usd_m = coin_m_stream(events.clone(), false);
        assert_eq!(usd_m.len(), events.len());
        let coin_m = coin_m_stream(events, true);
        assert!(!coin_m.iter().any(|e| matches!(e, Event::Funding(_))), "{coin_m:?}");
        assert!(
            coin_m.iter().any(|e| matches!(e, Event::AccountState(_))),
            "the wallet row still passes"
        );
    }

    /// The watchdog window the perp pump is handed, as a literal: 4x the MEASURED 180 s server ping
    /// (`BINANCE_PERP`'s exec lane in `vike_venues::venue_timings`). A retune is a deliberate
    /// edit here too, beside the matrix pin in vike-model.
    #[test]
    fn spec_arms_the_watchdog_at_four_server_pings() {
        let s = spec("https://fapi.binance.com".into(), "wss://fstream.binance.com".into(), false);
        assert_eq!(s.idle_threshold, Some(Duration::from_secs(720)));
    }

    #[test]
    fn auth_is_header_only_and_never_signs_the_query() {
        let auth = ListenKeyAuthenticationHeader { api_key: "SECRETKEY".to_string() };
        assert_eq!(auth.header(), Some(("X-MBX-APIKEY", "SECRETKEY".to_string())));
        assert!(auth.query("POST").is_empty(), "the HMAC signer is bypassed on this path");
        assert!(auth.query("PUT").is_empty());
    }

    /// Decision 0095: the TRADE_LITE hint is the exact `1` of `venue.binance.trade_lite_fill`, read
    /// from the venue's settings (Task 7 retired the credential-map fold that carried it) — never
    /// the process environment.
    #[test]
    fn trade_lite_fill_reads_its_row_exactly() {
        use vike_secrets::venue_setting::VenueSettings;
        let settings = |v: &str| {
            VenueSettings::from_rows(
                "binance",
                &[vike_secrets::VenueSettingRow {
                    venue: "binance".to_string(),
                    tier: None,
                    field: "TRADE_LITE_FILL".to_string(),
                    value: v.to_string(),
                }],
            )
        };
        assert!(trade_lite_fill(&settings("1")));
        assert!(trade_lite_fill(&settings(" 1 ")));
        for off in ["0", "true", ""] {
            assert!(!trade_lite_fill(&settings(off)), "{off:?}");
        }
        assert!(!trade_lite_fill(&VenueSettings::default()), "no row ⇒ OFF");
    }
}
