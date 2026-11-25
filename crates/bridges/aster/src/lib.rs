//! Aster DEX adapter — spot REST (`/api/v3`) + USDⓈ-M perp REST (`/fapi/v3`) + listenKey user-data
//! streams + a live `vike_data::DataClient` feed + public L2/tick market data.
//!
//! Aster's API is a near-verbatim Binance USDⓈ-M-futures + spot fork (same endpoint names,
//! `X-MBX-*` weight headers, `exchangeInfo` filters, 12-element klines, listenKey user-data,
//! depth-diff `U/u/pu` recovery). The sole divergence is authentication: v3 EIP-712 wallet
//! signatures ([`transport::sign::SignerAster`]) instead of Binance's HMAC — every signed request
//! (orders, listenKey, account, recon) rides the same signer.
//!
//! Because of that fork, the wire code this crate would otherwise COPY from `vike-binance` instead
//! lives once in [`vike_binance::family`] — the shared Binance-wire-grammar core, NOT a venue — and
//! this crate's [`execution::event_mapper::spot`]/[`execution::event_mapper::perp`]/[`execution::resync`]/[`instrument`] (rung 1),
//! [`feed::tick`]/[`feed::tick::trades`]/[`feed`] (rung 2), and [`catalog`] (rung 3) are thin venue
//! faces over it. Each passes `"aster"` plus its own per-venue values: the market-data stack its
//! host table, resolved from the venue's `venue_url` rows (see [`feed::tick::trades::spec`] and [`transport::urls`]),
//! [`catalog`] its mainnet-always `exchangeInfo` URLs.
//!
//! **The signed REST and the exec plane are the family's too** (the owner's ruling of 2026-11-13,
//! "both parts, behaviour as now"; this note said "wholly Aster's own" before it): the request bodies
//! are `vike_binance::family::execution::signed_rest`'s and the confirm, the foreign-symbol factories, the grid
//! fetch, the spot/perp drivers and the history replay `vike_binance::family::execution`'s.
//! [`execution::rest::spot`]/[`execution::rest::perp`] keep their clients (fields, order params with the builder pair and the spot
//! `STOP_MARKET`, `/api/v3` + `/fapi/v3` paths, the perp's ONE floor — the trigger gate; binance's other
//! floors and its post-only cause are not aster's) and their own verbs (the position-mode read, the
//! builder grant); [`execution`] keeps the client builds, the actor and aster's facts for the plane (hosts,
//! builder pair, listenKey pumps, the perp start's read and leverage post, the `vike_aster::execution` log
//! target). What stays wholly Aster's own: [`transport::sign`], [`mod@reconcile`], [`transport::urls`], and
//! [`transport::ratelimit`] (its rate budgets differ from Binance's in both window and size — see that module).

// The exec/feeds seam (split-plane Phase-5 hardening): everything that signs (v3 EIP-712),
// pumps private listenKey user-data or reconciles the account sits behind the default-on `exec`
// feature, so a `default-features = false` consumer links the FEED surface only —
// market_feed/market_data/data/trades/catalog over the keyless public endpoints — and
// vike-bridge-core's `eip712` (the whole k256 stack) never enters its binary. The
// `bridges-feeds` arm of scripts/ci_feature_suite.sh is the gate.
pub mod catalog;
// The venue-adapter contract (`vike_bridge_core::venue_adapter::VenueAdapter`) for aster — exec-plane,
// because it signs: a feeds-only build (`--no-default-features`) compiles no start.
#[cfg(feature = "execution")]
pub mod adapter;
// What the gatewayhub asks of aster to admit any symbol (keyless lists): the venue's list spec.
pub mod symbol_properties;

#[cfg(feature = "execution")]
pub mod execution;
pub mod feed;
pub mod history;
#[cfg(feature = "execution")]
pub mod instrument;
#[cfg(feature = "execution")]
pub mod reconcile;
// Every refusal this crate makes, its id and its one text (`vike_utility::refusal_enum!`).
pub mod refusals;
pub mod transport;

pub use catalog::CatalogProviderAster;
#[cfg(feature = "execution")]
pub use execution::fetch_aster_properties;
#[cfg(feature = "execution")]
pub use reconcile::{ReconcileClientAster, recon_client};

// `trades` is `pub(crate)` (its module-internal types like `AggTrade` stay crate-private), so
// `latest_agg_trade` is re-exported here to give the module a public face: a small helper the
// real-network smoke test would use to derive a real, always-current `before_id`/`earliest_timestamp`
// pair; nothing in production calls it yet.
//
// There is no `agg_trades_backfill` seam on this venue. The one that existed wrapped
// `vike_binance::family::feed::tick::trades`'s `()`-returning variant, both are retired, and no caller ever
// reached either — binance's `agg_trades_backfill_reported` is the live one. A future aster
// backfill wraps `agg_trades_backfill_reported` with `spec(urls)` the way `latest_agg_trade` does.
pub use feed::tick::trades::latest_agg_trade;

/// This adapter's DECLARED static capability row. Values live once in [`vike_venues::venue_caps`];
/// re-exported here next to the adapter, tied to real behavior by the test below.
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::ASTER;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("aster");
        assert_eq!(super::CAPABILITIES, capabilities);
        // reality (wired in execution/rest/perp.rs/exec.rs): VenueRestAsterPerp overrides modify + native batch, and
        // builds reduceOnly on every order.
        assert!(capabilities.supports_modify);
        assert!(capabilities.supports_native_batch);
        assert!(capabilities.supports_reduce_only);
        // Expanded axes (w2-task-5): Aster shares binance's family builders, so kinds/triggers
        // match (perp STOP_MARKET stop; take_profit → the MARKET fallthrough → unwired). TIF is
        // the UNFLIPPED default-GTC row — the exec loop never emits a TIF, so only a GTC request
        // matches what actually rests → a non-GTC limit is preflight-refused (the loud-deny flip).
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(capabilities.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(capabilities.supported_tifs, &[TimeInForce::Gtc]);
        assert_eq!(capabilities.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, 5); // /fapi/v3/batchOrders chunk cap
        assert!(!capabilities.supports_post_only);
    }
}
