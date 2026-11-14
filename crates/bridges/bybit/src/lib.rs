//! Bybit V5 adapter. The DATA half covers all three books — spot, linear and inverse; the EXEC
//! half signs a linear perpetual and dated linear future, a coin-margined (inverse) contract and a spot pair
//! (spelled `PAIR.S`), each in the category the venue's own row names ([`instrument::class`]); an option is refused by name.
//!
//! DATA half: [`history`] (REST kline history) + [`feed::DataClientSpecBybit`] (bybit's
//! half of the shared live `vike_data::DataClient` engine: klines, mark, trades and DOM depth). EXEC half:
//! [`execution::rest::VenueRestBybit`] (signed V5 REST, one book per client) + [`execution::user_data`] (private-WS fill/order
//! pump) + [`execution::funding::BybitFundingPoller`]. Also owns the R8 HFT public L2/tick track
//! ([`feed::tick`]).

// The exec/feeds seam (ruling 8 of the datahub market-data wire design), the same shape
// vike-hyperliquid and vike-aster already carry: everything that signs, pumps the private WS or
// reconciles an account sits behind the default-on `execution` feature, so a `default-features = false`
// consumer links the FEED surface only — data/market_data/market_feed/catalog/ratelimit over the
// keyless public endpoints. The `bridges-feeds` arm of scripts/ci_feature_suite.sh is the gate.
//
// ⚠ Unlike hl/aster this removes no CRATE from the dependency tree — bybit's signer is
// vike-bridge-core's HMAC one, and the feeds half needs that crate for its transport (it has no
// `full` feature to split since docs/decisions/0135). What the seam removes is COMPILED CODE. Cargo.toml's [features] block
// carries the measurement and the shared-home change that would close the rest.
pub mod catalog;
// THE VENUE ADAPTER (docs/decisions/0096) — exec-plane, like every module it builds on: it signs,
// spawns the exec client and builds the reconcile client, so a feeds-only build
// (`--no-default-features`) compiles no start.
#[cfg(feature = "execution")]
pub mod adapter;
// What the gatewayhub asks of bybit to admit any symbol (keyless one-symbol reads): the venue's list spec.
pub mod symbol_properties;
// The V5 request signer every signed REST call of this crate goes through (it left `vike-bridge-core`: one-venue code).
#[cfg(feature = "execution")]
pub mod signer;

#[cfg(feature = "execution")]
pub mod account;
#[cfg(feature = "execution")]
pub mod execution;
pub mod feed;
pub mod history;
pub mod instrument;
#[cfg(feature = "execution")]
pub mod reconcile;
pub mod transport;

pub use catalog::CatalogProviderBybit;
#[cfg(feature = "execution")]
pub use execution::fetch_bybit_properties;
#[cfg(feature = "execution")]
pub use reconcile::{ReconcileClientBybit, recon_client};

// The live market-DATA seam: `feed::DataClientSpecBybit` is bybit's half of the shared engine
// `vike_bridge_core::data_client::ws::DataClientWebsocket`, which implements `vike_data::DataClient`.

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter. Reality is
/// proven by `vike_bridge_core::exec_actor::run_loop_tests::modify_routes_to_modify_order_and_forwards_events`
/// (the hoisted shared loop, #461) plus this crate's own `capabilities_test` (the `supports_modify` reality tie).
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::BYBIT;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("bybit");
        assert_eq!(super::CAPABILITIES, capabilities);
        // native amend wired (ExecutionClientActorCommand::Modify → rest.modify_order); batch NOT wired at the
        // client seam (fan-out default), so supports_native_batch is FALSE.
        assert!(capabilities.supports_modify);
        // a stop's modify sends its trigger as the amend's `triggerPrice` (`execution/rest/mod.rs`'s `modify_order`,
        // `bybit_trigger_amend_smoke`), so it moves in place, never cancel-then-new
        use vike_venues::venue_amend_mode::{AmendKind, AmendMode, amend_mode};
        assert_eq!(amend_mode("bybit", AmendKind::Stop), AmendMode::Native);
        assert!(!capabilities.supports_native_batch);
        assert!(capabilities.supports_reduce_only);
        // Expanded axes (w2-task-5): `build_order_params` wires a native conditional from `"stop"`
        // (triggerPrice/triggerDirection, Market-on-trigger) and from `"stop_limit"` (the same
        // conditional firing as a Limit at `price`; flipped behind `bybit_stop_limit_smoke`, green on
        // the demo host 2026-11-08), both stop-loss triggers; `"take_profit"` is built (the same
        // conditional firing the other way) but not listed until its demo smoke runs
        // (`bybit_take_profit_smoke`). A reduce-only stop-limit is not listed: never placed (the demo
        // account was flat) and not stated by the venue's docs. TIF FLIPPED: GTC/IOC/FOK honored,
        // GTD/Day loud-denied venue-side (so the admit set == the honored trio). `margin_mode` never
        // read (UTA account-level). `max_batch: 0` — the V5 batch endpoints exist on the REST type
        // but are NOT wired at the ExecutionClient seam (consistent with supports_native_batch ==
        // false).
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(
            capabilities.supported_order_kinds,
            &["market", "limit", "stop", "stop_limit", "take_profit"]
        );
        assert_eq!(capabilities.reduce_only_kinds, &["market", "limit", "stop"]);
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss, TriggerType::TakeProfit]);
        assert_eq!(
            capabilities.accepted_tifs,
            &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly]
        );
        assert_eq!(capabilities.supported_tifs, capabilities.accepted_tifs);
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, 0);
        assert!(capabilities.supports_post_only, "V5 timeInForce=PostOnly");
        // Live-data row: the engine over `feed::DataClientSpecBybit` serves bars + `publicTrade` trades + `orderbook.200`
        // depth (no quotes/book lane). `live_data.trades` was flipped false→true after the
        // `bybit_trades_feed_smoke` live smoke proved `DataClient::subscribe_trades` delivers real
        // `TickTrade`s (2026-07-20) — the cap now matches the wired feed.
        assert!(capabilities.live_data.bars);
        assert!(capabilities.live_data.trades);
        assert!(capabilities.live_data.depth);
        assert!(!capabilities.live_data.quotes);
        assert!(!capabilities.live_data.book);
    }
}
