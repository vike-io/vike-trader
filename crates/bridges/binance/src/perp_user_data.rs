//! Binance USDS-M futures (fapi) listenKey user-data WS — the venue face of the shared
//! [`vike_binance::family::listenkey`](crate::family::listenkey) pump. Exact port of
//! `exec/binance/perp_user_data.py`.
//!
//! The pump shape (mint key → connect `wss://<ws_base>/<listenKey>` → NO subscribe, NO ack → PUT
//! keepalive every ~30 min, best-effort and bounded → never DELETE) is Binance wire grammar Aster
//! forked verbatim, so it now lives ONCE in the family core and this module passes `"binance"`
//! (F13, dedup rung 4a). Do NOT copy the spot ack loop here — the fapi stream never acks and it
//! would hang; that warning now guards the shared code too.
//!
//! What stays HERE is the genuine per-venue delta: [`HeaderAuth`] — Binance mints the key with the
//! apiKey in an `X-MBX-APIKEY` HEADER and the HMAC signer BYPASSED (no timestamp, no signature),
//! where Aster's v3 endpoint is EIP-712-signed instead. The `fapi` REST/WS hosts stay
//! caller-supplied exactly as before (this module has never resolved them), so the effective URLs
//! are unchanged — `family::listenkey`'s own tests pin both venues' exact strings.
//!
//! NOTE the sibling that is NOT this: [`crate::user_data`] is Binance's SPOT user-data path, a
//! WS-API subscribe/ack session — a different mechanism entirely, deliberately not folded in here.

use std::sync::mpsc;
use std::time::Duration;

use vike_exec::EventSender;
use vike_model::events::Event;

use crate::family::listenkey::{self, ListenKeyAuth, PumpSpec};
use crate::perp_mapper::map_binance_perp_opts;

pub const LISTENKEY_PATH: &str = "/fapi/v1/listenKey";

/// Opt-in gate for the Binance **TRADE_LITE** early fast-fill hint on the USDⓈ-M perp user stream
/// (default OFF). Set to the exact string `"1"` — same idiom as `VIKE_RECONCILE` — and the perp
/// mapper emits an EARLY bare fill from each `TRADE_LITE` frame so inventory-skew and the fill-rate
/// breaker react sooner; the authoritative `ORDER_TRADE_UPDATE` still drives the FSM. Unset /
/// anything-else ⇒ TRADE_LITE frames are dropped exactly as before (byte-identical). See
/// [`map_binance_perp_opts`](crate::perp_mapper::map_binance_perp_opts) for the dedup proof and the
/// commission caveat.
pub const TRADE_LITE_FILL_ENV: &str = "VIKE_BINANCE_TRADE_LITE_FILL";

/// Pure predicate: does this env value enable the early-fill hint? ONLY the exact `"1"` (mirrors
/// the reconcile master-gate idiom) — testable without touching the process env.
fn trade_lite_fill_enabled(val: Option<&str>) -> bool {
    val == Some("1")
}

/// Read [`TRADE_LITE_FILL_ENV`] from the process env ONCE at pump construction (never per-frame).
fn trade_lite_fill_from_env() -> bool {
    trade_lite_fill_enabled(std::env::var(TRADE_LITE_FILL_ENV).ok().as_deref())
}

/// Binance's listenKey auth — **the sole divergence from Aster's pump**: the apiKey rides in an
/// `X-MBX-APIKEY` header and NOTHING is signed (the listenKey endpoints are the one Binance path
/// where the HMAC signer is bypassed; they never need the secret either — only the key flows in).
pub struct HeaderAuth {
    api_key: String,
}

impl HeaderAuth {
    /// Build one from the api_key alone (listenKey endpoints never need the secret). Exists so an
    /// out-of-crate caller — the idle-cadence soak in `tests/` — can open a real user-data socket
    /// without duplicating the auth shape.
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

impl ListenKeyAuth for HeaderAuth {
    fn query(&self, _method: &str) -> String {
        String::new() // header auth — nothing hangs off the URL
    }
    fn header(&self) -> Option<(&'static str, String)> {
        Some(("X-MBX-APIKEY", self.api_key.clone()))
    }
}

/// Silent-stall watchdog window — 4x Binance's MEASURED server-ping interval.
///
/// This lane has NO app-level ping (the keepalive is a REST listenKey PUT that never touches the
/// socket), so the only inbound traffic on an idle account is Binance's own server ping. That
/// interval was measured on the demo perp socket — `tests/binance_userdata_soak.rs`, 900s window —
/// at **180.0s**, four consecutive periods within 160ms of each other. 4x it is.
///
/// ⚠ The margin is not decoration. A short threshold here does NOT fail safe: it would tear down
/// and rebuild a perfectly HEALTHY socket every window, forever — at the Class-A value of 60s,
/// three times per server-ping period. That hazard is exactly why this venue was held back when
/// bybit/okx/polymarket/hyperliquid were armed.
///
/// ⚠ Measured on TESTNET (`fstream.binancefuture.com`). If mainnet ever proves to ping slower, this
/// is the constant to raise — and the soak is the way to find out, not inference.
const IDLE_THRESHOLD: Duration = Duration::from_secs(720);

/// The pump/thread identity + endpoints for one perp user-data stream.
fn spec(fapi_rest_url: String, ws_base_url: String) -> PumpSpec {
    PumpSpec {
        venue: "binance",
        thread_tag: "binance-perp-userdata",
        resync_tag: "binance-perp-resync",
        rest_url: fapi_rest_url,
        ws_base_url,
        ws_path: "", // Binance perp connects `<ws_base>/<key>` — no `/ws` segment (Aster has one)
        listenkey_path: LISTENKEY_PATH,
        idle_threshold: Some(IDLE_THRESHOLD),
    }
}

/// Spawn the persistent perp fill-stream pump feeding the vt-core ingest (lossless).
/// Only the api_key flows in (listenKey endpoints never need the secret).
pub fn spawn_binance_perp_user_data(
    fapi_rest_url: String,
    ws_base_url: String,
    api_key: String,
    symbol: String,
    events: EventSender,
) -> vike_bridge_core::user_data::UserDataFeed {
    let sym = symbol.clone();
    let early = trade_lite_fill_from_env();
    listenkey::spawn_user_data(
        spec(fapi_rest_url, ws_base_url),
        symbol,
        move || HeaderAuth { api_key },
        move |frame| map_binance_perp_opts(frame, "binance", &sym, early),
        events,
    )
}

/// Join half of the perp pump + its audit-A3 resync supervisor (see
/// [`spawn_binance_perp_user_data_with_resync`]); thin alias for the shared
/// [`UserDataResyncFeed`](vike_bridge_core::user_data::UserDataResyncFeed).
pub type BinancePerpUserDataResync = vike_bridge_core::user_data::UserDataResyncFeed;

/// Like [`spawn_binance_perp_user_data`], plus the audit-A3 resync supervisor. On every WS re-open
/// the pump bumps a generation counter, and the supervisor (after a settle delay) replays recent
/// fapi order history via `resync_fetch` so a fill/cancel/expire/reject that landed during the
/// reconnect gap is recovered — the core dedups the overlap. `resync_fetch` is the caller's REST
/// history source (typically `get_all_orders` + `get_user_trades` →
/// [`map_binance_perp_history`](super::history::map_binance_perp_history)) on a SEPARATE
/// `BinancePerpRest` from the order-submit client. Both threads tear down on `shutdown`; the
/// supervisor also self-exits if the pump dies or the core is gone. (Exercised by live smokes, like
/// the other spawn functions.)
///
/// `on_reconcile` (reconciliation-activation Task 7) is threaded straight to the shared pump's
/// `on_reconcile` param — `Some` pokes the reconcile driver after every reconnect's event-replay
/// settles; `None` (every non-recon caller) reproduces the pre-Task-7 event-replay-only behavior.
pub fn spawn_binance_perp_user_data_with_resync(
    fapi_rest_url: String,
    ws_base_url: String,
    api_key: String,
    symbol: String,
    events: EventSender,
    resync_fetch: impl FnMut() -> Vec<Event> + Send + 'static,
    on_reconcile: Option<mpsc::Sender<()>>,
) -> BinancePerpUserDataResync {
    let sym = symbol.clone();
    let early = trade_lite_fill_from_env();
    listenkey::spawn_user_data_with_resync(
        spec(fapi_rest_url, ws_base_url),
        symbol,
        move || HeaderAuth { api_key },
        move |frame| map_binance_perp_opts(frame, "binance", &sym, early),
        events,
        resync_fetch,
        on_reconcile,
    )
}

#[cfg(test)]
mod perp_user_data_tests {
    use super::*;

    #[test]
    fn spec_pins_binance_perp_wire_shape() {
        let s = spec("https://fapi.binance.com".into(), "wss://fstream.binance.com".into());
        assert_eq!(s.listenkey_path, "/fapi/v1/listenKey", "fapi listenKey is v1, NOT aster's v3");
        assert_eq!(s.ws_path, "", "binance perp has NO /ws segment — `<ws_base>/<key>`");
        assert_eq!(s.venue, "binance");
    }

    #[test]
    fn auth_is_header_only_and_never_signs_the_query() {
        let auth = HeaderAuth { api_key: "SECRETKEY".to_string() };
        assert_eq!(auth.header(), Some(("X-MBX-APIKEY", "SECRETKEY".to_string())));
        assert!(auth.query("POST").is_empty(), "the HMAC signer is bypassed on this path");
        assert!(auth.query("PUT").is_empty());
    }

    /// The TRADE_LITE early-fill gate is EXACT-`"1"` (mirrors the `VIKE_RECONCILE` master-gate
    /// idiom): unset / `0` / `true` / empty / padded all stay OFF, so a default build keeps dropping
    /// TRADE_LITE frames (byte-identical).
    #[test]
    fn trade_lite_fill_gate_is_exact_one() {
        assert!(trade_lite_fill_enabled(Some("1")), "exactly \"1\" enables the hint");
        assert!(!trade_lite_fill_enabled(None), "unset ⇒ OFF (the default)");
        assert!(!trade_lite_fill_enabled(Some("0")));
        assert!(!trade_lite_fill_enabled(Some("true")));
        assert!(!trade_lite_fill_enabled(Some("")));
        assert!(!trade_lite_fill_enabled(Some(" 1 ")), "no trimming — exact match only");
    }
}
