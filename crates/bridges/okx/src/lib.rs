//! OKX V5 adapter — SWAP perp (R6 slice 5). Demo = same REST host + the
//! `x-simulated-trading: 1` header; browser UA everywhere (Cloudflare).
//!
//! DATA half: [`history`] (REST kline/candlestick history) + [`feed::DataClientSpecOkx`] (okx's half of
//! the shared live `vike_data::DataClient` engine, one file per lane under `market_feed/`: klines, mark,
//! trades, depth). EXEC half: [`execution::rest::VenueRestOkx`] (signed V5 SWAP REST, incl. `to_contracts` — one of the
//! pinned Decimal wire sites, see `crates/vike-bridge-core/tests/wire_quantizer_probe.rs`'s
//! `WIRE_FILES`; the batch, stop-algo and ticker-read methods sit in `perp/`) + [`execution::user_data`]
//! (private-WS login/subscribe/fill pump) + [`execution::funding::OkxFundingPoller`] + [`execution::resync`] (audit-A3 resync
//! replay). Also owns the R8 HFT public L2/tick track ([`feed::tick`]).

pub mod catalog;
// What the gatewayhub asks of okx to admit any symbol (keyless lists): the venue's list spec.
pub mod symbol_properties;
// The venue-adapter contract impl (docs/decisions/0096): it spawns the exec client and builds the
// reconcile client, so it rides `exec` with them.
#[cfg(feature = "execution")]
pub mod adapter;
// The V5 request signer every signed REST call of this crate goes through (it left `vike-bridge-core`: one-venue code).
#[cfg(feature = "execution")]
pub mod signer;
// The crate's own offline doubles (`signer`, `NoWire`), for its unit tests and, through the self
// dev-dependency, its `tests/` binaries; never in a normal build.
#[cfg(all(feature = "execution", any(test, feature = "test-support")))]
pub mod test_support;

// The exec/feeds seam (ruling 8 of the datahub market-data wire design), the same shape
// vike-hyperliquid and vike-aster already carry: everything that signs, pumps the private WS or
// reconciles an account sits behind the default-on `execution` feature, so a `default-features = false`
// consumer links the FEED surface only — data/market_data/market_feed/catalog/
// ratelimit over the keyless public endpoints. The `bridges-feeds` arm of
// scripts/ci_feature_suite.sh is the gate.
//
// ⚠ Unlike hl/aster this removes no CRATE from the dependency tree — okx's signer is
// vike-bridge-core's HMAC one, and the feeds half needs that crate for its transport (it has no
// `full` feature to split since docs/decisions/0135). What the seam removes is COMPILED CODE. Cargo.toml's [features] block
// carries the measurement and the shared-home change that would close the rest.
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

/// The full browser User-Agent every OKX request carries — signed AND public alike — as the
/// Cloudflare 1010/403 dodge the Python port established.
///
/// It lived in [`transport::client`] until the exec/feeds seam landed, where a FEEDS-only build cannot see
/// it: [`history`]'s keyless kline fetch sends it too, and [`transport::client`] is the SIGNED V5 client. The
/// dodge is a fact about the VENUE's edge rather than about the signed transport, so it sits at the
/// crate root where both planes reach it. No re-export was left behind at the old path (CLAUDE.md's
/// no-shim rule); all three call sites moved.
pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

pub use catalog::CatalogProviderOkx;
#[cfg(feature = "execution")]
pub use execution::fetch_okx_instrument;
#[cfg(feature = "execution")]
pub use reconcile::{ReconcileClientOkx, recon_client};

// The live market-DATA seam: `feed::DataClientSpecOkx` is okx's half of the shared engine
// `vike_bridge_core::data_client::ws::DataClientWebsocket`, which implements `vike_data::DataClient`.

/// This adapter's DECLARED static capability row (audit br6). Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter. Reality is
/// proven by `vike_bridge_core::exec_actor::run_loop_tests::modify_routes_to_modify_order_and_forwards_events`
/// (the hoisted shared loop, #461) plus this crate's own `capabilities_test` (the `supports_modify` reality tie).
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::OKX;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("okx");
        assert_eq!(super::CAPABILITIES, capabilities);
        // native amend wired (ExecutionClientActorCommand::Modify → rest.modify_order); batch NOT wired at the
        // client seam (fan-out default), so supports_native_batch is FALSE.
        assert!(capabilities.supports_modify);
        assert!(!capabilities.supports_native_batch);
        assert!(capabilities.supports_reduce_only);
        // Expanded axes (w2-task-5): `"stop"` routes to the native ALGO conditional
        // (`submit_stop_algo`, slTriggerPx ← trigger_price), and `"stop_limit"` to the same order
        // with its limit in `slOrdPx`. `"take_profit"` is BUILT (the same order's `tpTriggerPx`/
        // `tpOrdPx`, `submit_stop_algo`) and the row does not list it until its demo smoke runs
        // (`tests/okx_smokes/okx_take_profit_smoke.rs`). TIF FLIPPED: GTC default + IOC/FOK mapped,
        // GTD/Day denied. THE margin venue — `swap_td_mode` honors a per-order `margin_mode`:
        // Cross AND Isolated (Cash denied venue-side, spot-only). `max_batch: 0` (batch endpoints
        // exist but are not wired at the client seam). `"trailing"` is the `move_order_stop` algo order,
        // declared since its demo smoke ran.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        let kinds: &[&str] = &["market", "limit", "stop", "stop_limit", "take_profit", "trailing"];
        assert_eq!(capabilities.supported_order_kinds, kinds);
        assert_eq!(capabilities.reduce_only_kinds, kinds);
        // the attributes wave: the same `move_order_stop` by a PERCENT (`callbackRatio`), reduce-only too
        assert_eq!(capabilities.percent_trail_kinds, &["trailing"]);
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss, TriggerType::TakeProfit]);
        assert_eq!(
            capabilities.accepted_tifs,
            &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::PostOnly]
        );
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross, MarginMode::Isolated]);
        assert_eq!(capabilities.max_batch, 0);
        assert!(capabilities.supports_post_only, "ordType post_only");
        // Live-data row: the engine over `feed::DataClientSpecOkx` serves bars + `trades` prints + `books` depth (no
        // quotes/book lane). `live_data.trades` was flipped false→true after the
        // `okx_trades_feed_smoke` live smoke proved `DataClient::subscribe_trades` delivers real
        // `TickTrade`s (2026-07-20) — the cap now matches the wired feed.
        assert!(capabilities.live_data.bars);
        assert!(capabilities.live_data.trades);
        assert!(capabilities.live_data.depth);
        assert!(!capabilities.live_data.quotes);
        assert!(!capabilities.live_data.book);
    }
}
