//! Bybit V5 adapter — linear perp (R6 slice 4). Spot is deferred (the Python app trades
//! Bybit linear; spot shares the same signer/transport when needed).
//!
//! DATA half: [`data`] (spot REST kline history) + [`market_feed::Feeds`] (the live
//! `vike_data::DataClient` kline feed). EXEC half: [`perp::BybitPerpRest`] (signed V5 linear-perp
//! REST) + [`user_data`] (private-WS fill/order pump) + [`funding::BybitFundingPoller`]. Also owns
//! the R8 HFT public L2/tick track ([`market_data`]).
//!
//! Moved into crates/bridges/bybit (crate-reorg Phase 3, PR E — first venue move with a real
//! external consumer (`vike-backfill`) and real integration tests to relocate).

// The exec/feeds seam (ruling 8 of the datahub market-data wire design), the same shape
// vike-hyperliquid and vike-aster already carry: everything that signs, pumps the private WS or
// reconciles an account sits behind the default-on `exec` feature, so a `default-features = false`
// consumer links the FEED surface only — data/market_data/market_feed/catalog/ratelimit over the
// keyless public endpoints. The `bridges-feeds` arm of scripts/ci_feature_suite.sh is the gate.
//
// ⚠ Unlike hl/aster this removes no CRATE from the dependency tree — bybit's signer is
// vike-bridge-core's HMAC one, which rides that crate's `full` feature and the feeds half needs
// `full` for its transport. What the seam removes is COMPILED CODE. Cargo.toml's [features] block
// carries the measurement and the shared-home change that would close the rest.
pub mod catalog;
pub mod data;
#[cfg(feature = "exec")]
pub mod error_codes;
#[cfg(feature = "exec")]
pub mod event_mapper;
#[cfg(feature = "exec")]
pub mod exec;
#[cfg(feature = "exec")]
pub mod funding;
#[cfg(feature = "exec")]
pub mod history;
pub mod market_data;
pub mod market_feed;
#[cfg(feature = "exec")]
pub mod perp;
pub mod ratelimit;
#[cfg(feature = "exec")]
pub mod recon_client;
#[cfg(feature = "exec")]
pub mod transport;
#[cfg(feature = "exec")]
pub mod user_data;
#[cfg(feature = "exec")]
pub mod ws_auth;

pub use catalog::BybitCatalog;
#[cfg(feature = "exec")]
pub use exec::BybitExecutionClient;
#[cfg(feature = "exec")]
pub use exec::fetch_bybit_properties;
#[cfg(feature = "exec")]
pub use recon_client::{BybitReconClient, recon_client};

// The live market-DATA seam impl: `market_feed::Feeds` implements `vike_data::DataClient` directly
// (the per-venue `*MarketData` thin-factory wrapper was retired — Phase 1 crate-reorg). `Feeds`
// stays reachable at `market_feed::Feeds` for a future GUI's direct use, mirroring binance.

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_model::venue_caps`]; re-exported here for discoverability next to the adapter. Reality is
/// proven by `vike_bridge_core::exec_actor::run_loop_tests::modify_routes_to_modify_order_and_forwards_events`
/// (the hoisted shared loop, #461) plus this crate's own `caps_test` (the `supports_modify` reality tie).
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::BYBIT;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("bybit");
        assert_eq!(super::CAPS, caps);
        // native amend wired (ExecCommand::Modify → rest.modify_order); batch NOT wired at the
        // client seam (fan-out default), so supports_native_batch is FALSE.
        assert!(caps.supports_modify);
        assert!(!caps.supports_native_batch);
        assert!(caps.supports_reduce_only);
        // Expanded axes (w2-task-5): `build_order_params` wires a native conditional from `"stop"`
        // (triggerPrice/triggerDirection, Market-on-trigger); `"take_profit"` → the `_ => Market`
        // coercion → unwired. TIF FLIPPED: GTC/IOC/FOK honored, GTD/Day loud-denied venue-side (so
        // the admit set == the honored trio). `margin_mode` never read (UTA account-level).
        // `max_batch: 0` — the V5 batch endpoints exist on the REST type but are NOT wired at the
        // ExecutionClient seam (consistent with supports_native_batch == false).
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(caps.accepted_tifs, &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok]);
        assert_eq!(caps.supported_tifs, caps.accepted_tifs);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
        // Live-data row: `market_feed::Feeds` serves bars + `publicTrade` trades + `orderbook.200`
        // depth (no quotes/book lane). `live_data.trades` was flipped false→true after the
        // `bybit_trades_feed_smoke` live smoke proved `DataClient::subscribe_trades` delivers real
        // `TradeTick`s (2026-07-20) — the cap now matches the wired feed.
        assert!(caps.live_data.bars);
        assert!(caps.live_data.trades);
        assert!(caps.live_data.depth);
        assert!(!caps.live_data.quotes);
        assert!(!caps.live_data.book);
    }
}
