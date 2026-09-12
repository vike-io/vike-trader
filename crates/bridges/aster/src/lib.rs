//! Aster DEX adapter — spot REST (`/api/v3`) + USDⓈ-M perp REST (`/fapi/v3`) + listenKey user-data
//! streams + a live `vike_data::DataClient` feed + public L2/tick market data.
//!
//! Aster's API is a near-verbatim Binance USDⓈ-M-futures + spot fork (same endpoint names,
//! `X-MBX-*` weight headers, `exchangeInfo` filters, 12-element klines, listenKey user-data,
//! depth-diff `U/u/pu` recovery). The sole divergence is authentication: v3 EIP-712 wallet
//! signatures ([`signing::AsterSigner`]) instead of Binance's HMAC — every signed request
//! (orders, listenKey, account, recon) rides the same signer.
//!
//! Because of that fork, the wire code this crate would otherwise COPY from `vike-binance` instead
//! lives once in [`vike_binance::family`] — the shared Binance-wire-grammar core, NOT a venue — and
//! this crate's [`event_mapper`]/[`perp_mapper`]/[`history`]/[`filters_rec`] (rung 1),
//! [`market_data`]/[`trades`]/[`market_feed`] (rung 2), and [`catalog`] (rung 3) are thin venue
//! faces over it. Each passes `"aster"` plus its own per-venue values: the market-data stack its
//! `env`-resolved host table (see [`trades::spec`]), [`catalog`] its mainnet-always `exchangeInfo`
//! URLs. What stays wholly Aster's own: [`signing`], signed REST ([`spot`]/[`perp`]/[`exec`]),
//! [`recon_client`], [`urls`], and [`ratelimit`] (its rate budgets differ from Binance's in both
//! window and size — see that module).

// The exec/feeds seam (split-plane Phase-5 hardening): everything that signs (v3 EIP-712),
// pumps private listenKey user-data or reconciles the account sits behind the default-on `exec`
// feature, so a `default-features = false` consumer links the FEED surface only —
// market_feed/market_data/data/trades/catalog over the keyless public endpoints — and
// vike-bridge-core's `eip712` (the whole k256 stack) never enters its binary. The
// `bridges-feeds` arm of scripts/ci_feature_suite.sh is the gate.
pub mod catalog;
pub mod data;
#[cfg(feature = "exec")]
pub mod event_mapper;
#[cfg(feature = "exec")]
pub mod exec;
#[cfg(feature = "exec")]
pub mod filters_rec;
#[cfg(feature = "exec")]
pub mod history;
#[cfg(feature = "exec")]
pub mod listenkey_auth;
pub mod market_data;
pub mod market_feed;
#[cfg(feature = "exec")]
pub mod perp;
#[cfg(feature = "exec")]
pub mod perp_mapper;
#[cfg(feature = "exec")]
pub mod perp_user_data;
pub mod ratelimit;
#[cfg(feature = "exec")]
pub mod recon_client;
#[cfg(feature = "exec")]
pub mod signing;
#[cfg(feature = "exec")]
pub mod spot;
pub(crate) mod trades;
pub mod urls;
#[cfg(feature = "exec")]
pub mod user_data;

pub use catalog::AsterCatalog;
#[cfg(feature = "exec")]
pub use exec::{AsterExecutionClient, fetch_aster_properties};
#[cfg(feature = "exec")]
pub use recon_client::{AsterReconClient, recon_client};

// `trades` is `pub(crate)` (its module-internal types like `AggTrade` stay crate-private), so
// `latest_agg_trade` is re-exported here to give the module a public face: a small helper the
// real-network smoke test would use to derive a real, always-current `before_id`/`earliest_ts`
// pair; nothing in production calls it yet.
//
// There is no `agg_trades_backfill` seam on this venue. The one that existed wrapped
// `vike_binance::family::trades`'s `()`-returning variant, both are retired, and no caller ever
// reached either — binance's `agg_trades_backfill_reported` is the live one. A future aster
// backfill wraps `agg_trades_backfill_reported` with `spec(env)` the way `latest_agg_trade` does.
pub use trades::latest_agg_trade;

/// This adapter's DECLARED static capability row. Values live once in [`vike_model::venue_caps`];
/// re-exported here next to the adapter, tied to real behavior by the test below.
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::ASTER;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("aster");
        assert_eq!(super::CAPS, caps);
        // reality (wired in perp.rs/exec.rs): AsterPerpRest overrides modify + native batch, and
        // builds reduceOnly on every order.
        assert!(caps.supports_modify);
        assert!(caps.supports_native_batch);
        assert!(caps.supports_reduce_only);
        // Expanded axes (w2-task-5): Aster shares binance's family builders, so kinds/triggers
        // match (perp STOP_MARKET stop; take_profit → the MARKET fallthrough → unwired). TIF is
        // the UNFLIPPED default-GTC row — the exec loop never emits a TIF, so only a GTC request
        // matches what actually rests → a non-GTC limit is preflight-refused (the loud-deny flip).
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(caps.supported_tifs, &[TimeInForce::Gtc]);
        assert_eq!(caps.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 5); // /fapi/v3/batchOrders chunk cap
        assert!(!caps.supports_post_only);
    }
}
