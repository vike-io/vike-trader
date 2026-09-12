//! OKX V5 adapter — SWAP perp (R6 slice 5). Demo = same REST host + the
//! `x-simulated-trading: 1` header; browser UA everywhere (Cloudflare).
//!
//! DATA half: [`data`] (REST kline/candlestick history) + [`market_feed::Feeds`] (the live
//! `vike_data::DataClient` kline feed). EXEC half: [`perp::OkxPerpRest`] (signed V5 SWAP REST,
//! incl. `to_contracts` — the second pinned Decimal wire site) + [`user_data`] (private-WS
//! login/subscribe/fill pump) + [`funding::OkxFundingPoller`] + [`history`] (audit-A3 resync
//! replay). Also owns the R8 HFT public L2/tick track ([`market_data`]).
//!
//! Moved into crates/bridges/okx (crate-reorg Phase 3, PR G — the largest bridge move so far, but
//! mechanical: every cross-venue borrowing (kline_to_bar, SymbolProperties, VenueRest/LiveRestClient,
//! ws items, signer/transport/format/json helpers) was already a direct `vike_bridge_core`/
//! `vike_model` path before this move, per the bybit/deribit-established template).

// The exec/feeds seam (ruling 8 of the datahub market-data wire design), the same shape
// vike-hyperliquid and vike-aster already carry: everything that signs, pumps the private WS or
// reconciles an account sits behind the default-on `exec` feature, so a `default-features = false`
// consumer links the FEED surface only — data/market_data/market_feed/book_checksum/catalog/
// ratelimit over the keyless public endpoints. The `bridges-feeds` arm of
// scripts/ci_feature_suite.sh is the gate.
//
// ⚠ Unlike hl/aster this removes no CRATE from the dependency tree — okx's signer is
// vike-bridge-core's HMAC one, which rides that crate's `full` feature and the feeds half needs
// `full` for its transport. What the seam removes is COMPILED CODE. Cargo.toml's [features] block
// carries the measurement and the shared-home change that would close the rest.
pub mod book_checksum;
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

/// The full browser User-Agent every OKX request carries — signed AND public alike — as the
/// Cloudflare 1010/403 dodge the Python port established.
///
/// It lived in [`transport`] until the exec/feeds seam landed, where a FEEDS-only build cannot see
/// it: [`data`]'s keyless kline fetch sends it too, and [`transport`] is the SIGNED V5 client. The
/// dodge is a fact about the VENUE's edge rather than about the signed transport, so it sits at the
/// crate root where both planes reach it. No re-export was left behind at the old path (CLAUDE.md's
/// no-shim rule); all three call sites moved.
pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

pub use catalog::OkxCatalog;
#[cfg(feature = "exec")]
pub use exec::OkxExecutionClient;
#[cfg(feature = "exec")]
pub use exec::fetch_okx_instrument;
#[cfg(feature = "exec")]
pub use recon_client::{OkxReconClient, recon_client};

// The live market-DATA seam impl: `market_feed::Feeds` implements `vike_data::DataClient` directly
// (the per-venue `*MarketData` thin-factory wrapper was retired — Phase 1 crate-reorg). `Feeds`
// stays reachable at `market_feed::Feeds` for a future GUI's direct use, mirroring binance.

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_model::venue_caps`]; re-exported here for discoverability next to the adapter. Reality is
/// proven by `vike_bridge_core::exec_actor::run_loop_tests::modify_routes_to_modify_order_and_forwards_events`
/// (the hoisted shared loop, #461) plus this crate's own `caps_test` (the `supports_modify` reality tie).
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::OKX;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("okx");
        assert_eq!(super::CAPS, caps);
        // native amend wired (ExecCommand::Modify → rest.modify_order); batch NOT wired at the
        // client seam (fan-out default), so supports_native_batch is FALSE.
        assert!(caps.supports_modify);
        assert!(!caps.supports_native_batch);
        assert!(caps.supports_reduce_only);
        // Expanded axes (w2-task-5): `"stop"` routes to the native ALGO conditional
        // (`submit_stop_algo`, slTriggerPx ← trigger_price); `"take_profit"` → the
        // `ordType:"market"` coercion → unwired. TIF FLIPPED: GTC default + IOC/FOK mapped,
        // GTD/Day denied. THE margin venue — `swap_td_mode` honors a per-order `margin_mode`:
        // Cross AND Isolated (Cash denied venue-side, spot-only). `max_batch: 0` (batch endpoints
        // exist but are not wired at the client seam).
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(caps.accepted_tifs, &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok]);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross, MarginMode::Isolated]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
        // Live-data row: `market_feed::Feeds` serves bars + `trades` prints + `books` depth (no
        // quotes/book lane). `live_data.trades` was flipped false→true after the
        // `okx_trades_feed_smoke` live smoke proved `DataClient::subscribe_trades` delivers real
        // `TradeTick`s (2026-07-20) — the cap now matches the wired feed.
        assert!(caps.live_data.bars);
        assert!(caps.live_data.trades);
        assert!(caps.live_data.depth);
        assert!(!caps.live_data.quotes);
        assert!(!caps.live_data.book);
    }
}
