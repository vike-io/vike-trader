//! Hyperliquid venue bridge — **spot + perpetuals** on the shared workspace stack.
//!
//! A from-scratch adapter (no HL SDK dependency): EIP-712/msgpack action signing built on the
//! workspace's existing `k256`/`tiny-keccak` (the shared `vike_bridge_core::eip712` primitive) plus
//! one new msgpack dep (`rmp-serde`), over `ureq` (blocking REST) and `tungstenite` (blocking WS). One
//! crate, two flavors: spot and perp differ only in asset-id arithmetic, symbology, the balance
//! endpoint, and the fills channel — everything else (signing, wire encoding, WS pump, l2Book
//! handling) is common.
//!
//! DATA half: [`feed::DataClientSpecHyperliquid`] (its half of the live
//! `vike_data::DataClient` engine: candle bars, `bbo` quotes, `trades`, `l2Book` full-snapshot depth).
//! EXEC half: [`execution`] (`ExecutionClientActor` + signed `/exchange` orders) + [`execution::user_data`] (the private `orderUpdates`/`userFills` WS pump) + [`mod@reconcile`].
//! Instruments/catalog from `meta` + `spotMeta`. [`adapter`] is the live-MOUNT seam — signer +
//! transport wiring, the `userRole` identity probe, and the exec+recon construction, behind
//! [`adapter::VenueAdapterHyperliquid`] — moved down from `vike-tradehub-core` on decision 0088's B3 and
//! wrapped in the venue-adapter contract by docs/decisions/0096.
//!
//! Folders are ROLES, and a module path is its file path: [`config`] (credentials + endpoints),
//! [`transport`] (the rate gate, the REST transport), [`instrument`] (price/size wire formatting,
//! symbology), [`feed`] and [`history`] (the keyless `/info` + WS data surface), [`execution`] (the exec plane: client, private user-data pump, recon,
//! event mapper, HIP-4 settlement), [`account::user_signed`] (standalone user-signed actions), plus the
//! top-level [`signing`] and [`adapter`].
//!
//! Layers (down-only, mirrors the Bybit/OKX anatomy): `config`, `transport::{consts, ratelimit}` →
//! `signing` → `transport` → `instrument::{symbology, price}` and `execution::event_mapper` (pure) →
//! `instrument::properties` → `exec`/`feed`/`reconcile`/`catalog` →
//! [`adapter`], which calls down into all of them. `account::user_signed::transfer` (usdClassTransfer) and
//! `account::user_signed::builder_fee` (approveBuilderFee) are standalone user-signed-action leaves off
//! `signing`/`transport`, not in the exec hot path.
//! Design + wire-format ground truth: `docs/superpowers/specs/2026-07-16-hyperliquid-bridge-design.md`
//! and `docs/research/2026-07-16-hyperliquid-adapters/README.md`.

// The exec/feeds seam (split-plane Phase-5 hardening): everything that signs, pumps private
// user-data or reconciles an account sits behind the default-on `execution` feature, so a
// `default-features = false` consumer links the FEED surface only — `feeds` over the keyless
// `/info` half of `transport` — with no k256/tiny-keccak/rmp-serde in its binary. The
// `bridges-feeds` arm of scripts/ci_feature_suite.sh is the gate.
// What the gatewayhub asks of hyperliquid to admit any symbol (keyless lists): the venue's list spec.
pub mod symbol_properties;
// The live-mount seam (decision 0088's B3): signer/transport wiring, the `userRole` identity
// probe and the exec+recon client construction, behind the venue-adapter contract's
// `VenueAdapterHyperliquid` (docs/decisions/0096). `exec`-gated because it builds a real signer and
// an `ExecutionClient` — the same reason `exec`/`signing`/`reconcile` are.
#[cfg(feature = "execution")]
pub mod adapter;
#[cfg(feature = "execution")]
pub mod signing;
// The crate's own test helpers (`sdk_order`, `post_testnet_exchange`), for its unit tests and, through
// the self dev-dependency, its `tests/` binaries; never in a normal build.
#[cfg(all(feature = "execution", any(test, feature = "test-support")))]
pub mod test_support;

pub mod account;
pub mod catalog;
pub mod config;
#[cfg(feature = "execution")]
pub mod execution;
pub mod feed;
pub mod history;
pub mod instrument;
#[cfg(feature = "execution")]
pub mod reconcile;
pub mod transport;

pub use catalog::CatalogProviderHyperliquid;
#[cfg(feature = "execution")]
pub use reconcile::{ReconcileClientHyperliquid, recon_client};

/// This adapter's DECLARED static capability row. Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter. Modify is
/// venue cancel-replace, batch is native (HL is batch-first), reduce-only on perps; TIFs Gtc/Ioc.
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::HYPERLIQUID;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("hyperliquid");
        assert_eq!(super::CAPABILITIES, capabilities);
        assert!(capabilities.supports_modify); // cancel-replace modify wired at the exec seam
        assert!(capabilities.supports_native_batch); // HL /exchange order action is batch-first
        assert!(capabilities.supports_reduce_only); // perp orders carry reduceOnly

        // `vike_hyperliquid::history::klines::KlineSourceHyperliquid` pages candleSnapshot into `append_bars`
        // (the datahub's `Backfill` verb, through `crates/vike-gatewayhub/src/rest/kline_sources.rs`'s `KLINE_SOURCES`; first
        // shipped as a hyperliquid_backfill program, deleted since, and the impl itself lived
        // in vike-backfill's own `venues/hyperliquid.rs` until docs/decisions/0094 moved
        // it here); the caps row said `false` long after that landed.
        assert!(capabilities.backfill_bars && !capabilities.backfill_ticks);

        // Expanded axes (w2-task-5): the RICH trigger venue — `build_order_wire` routes `"stop"`,
        // `"stop_limit"` AND `"take_profit"` through `build_trigger_kind` (triggerPx ←
        // trigger_price, tpsl:"sl"/"tp", market-or-limit by whether `price` is set), and
        // `"market"` is the emulated IOC limit at ±5% off mid. `accepted_tifs`
        // is all five (Fok→Ioc, Gtd/Day→Gtc are the live coercions — a separate flip), while the
        // honored set is only Gtc/Ioc. `Alo` (post-only) is unreachable from OrderRequest. The
        // batch action carries N wires with NO adapter-side cap → usize::MAX.
        //
        // `"stop_limit"` was MISSING from this list, so `vike_venues::preflight_order` terminally
        // rejected an order the adapter builds correctly. `execution/mod.rs`'s
        // `stop_limit_kind_is_declared_and_built` is the REAL tie (it drives `build_order_wire`
        // and asserts this row in one test) — unlike everything else in this module, which only
        // restates the constant.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(
            capabilities.supported_order_kinds,
            &["market", "limit", "stop", "stop_limit", "take_profit"]
        );
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss, TriggerType::TakeProfit]);
        assert_eq!(capabilities.accepted_tifs.len(), 6);
        assert_eq!(
            capabilities.supported_tifs,
            &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::PostOnly]
        );
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, usize::MAX);
        assert!(capabilities.supports_post_only, "tif Alo");
    }
}
