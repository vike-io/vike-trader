//! Hyperliquid venue bridge — **spot + perpetuals** on the shared workspace stack.
//!
//! A from-scratch adapter (no HL SDK dependency): EIP-712/msgpack action signing built on the
//! workspace's existing `k256`/`tiny-keccak` (the shared `vike_bridge_core::eip712` primitive) plus
//! one new msgpack dep (`rmp-serde`), over `ureq` (blocking REST) and `tungstenite` (blocking WS). One
//! crate, two flavors: spot and perp differ only in asset-id arithmetic, symbology, the balance
//! endpoint, and the fills channel — everything else (signing, wire encoding, WS pump, l2Book
//! handling) is common.
//!
//! DATA half: [`feeds::market_feed::Feeds`] (the live `vike_data::DataClient`: candle bars, `bbo` quotes,
//! `trades`, `l2Book` full-snapshot depth). EXEC half: [`exec`] (`ExecActor` + signed `/exchange`
//! orders) + [`exec::user_data`] (the private `orderUpdates`/`userFills` WS pump) + [`mod@exec::recon_client`].
//! Instruments/catalog from `meta` + `spotMeta`. [`mount`] is the live-MOUNT seam — signer +
//! transport wiring, the `userRole` identity probe, and the exec+recon construction, behind
//! [`mount::HyperliquidVenueMount`] — moved down from `vike-mount` on decision 0088's B3 and
//! wrapped in the venue mount contract by docs/decisions/0096.
//!
//! Folders are ROLES, and a module path is its file path: [`base`] (credentials + endpoints, the
//! rate gate, the REST transport, price/size wire formatting, symbology), [`feeds`] (the keyless
//! `/info` + WS data surface), [`exec`] (the exec plane: client, private user-data pump, recon,
//! event mapper, HIP-4 settlement), [`user_signed`] (standalone user-signed actions), plus the
//! top-level [`signing`] and [`mount`].
//!
//! Layers (down-only, mirrors the Bybit/OKX anatomy): `base::{config, consts, ratelimit}` →
//! `signing` → `base::transport` → `base::{symbology, px}` and `exec::event_mapper` (pure) →
//! `feeds::instruments` → `exec`/`feeds::market_feed`/`exec::recon_client`/`feeds::catalog` →
//! [`mount`], which calls down into all of them. `user_signed::transfer` (usdClassTransfer) and
//! `user_signed::builder_fee` (approveBuilderFee) are standalone user-signed-action leaves off
//! `signing`/`base::transport`, not in the exec hot path.
//! Design + wire-format ground truth: `docs/superpowers/specs/2026-07-16-hyperliquid-bridge-design.md`
//! and `docs/research/2026-07-16-hyperliquid-adapters/README.md`.

// The exec/feeds seam (split-plane Phase-5 hardening): everything that signs, pumps private
// user-data or reconciles an account sits behind the default-on `exec` feature, so a
// `default-features = false` consumer links the FEED surface only — `feeds` over the keyless
// `/info` half of `base::transport` — with no k256/tiny-keccak/rmp-serde in its binary. The
// `bridges-feeds` arm of scripts/ci_feature_suite.sh is the gate.
pub mod base;
#[cfg(feature = "exec")]
pub mod exec;
pub mod feeds;
// The live-mount seam (decision 0088's B3): signer/transport wiring, the `userRole` identity
// probe and the exec+recon client construction, behind the venue mount contract's
// `HyperliquidVenueMount` (docs/decisions/0096). `exec`-gated because it builds a real signer and
// an `ExecutionClient` — the same reason `exec`/`signing`/`exec::recon_client` are.
#[cfg(feature = "exec")]
pub mod mount;
#[cfg(feature = "exec")]
pub mod signing;
#[cfg(feature = "exec")]
pub mod user_signed;

#[cfg(feature = "exec")]
pub use exec::HyperliquidExecutionClient;
#[cfg(feature = "exec")]
pub use exec::recon_client::{HyperliquidReconClient, recon_client};
pub use feeds::catalog::HyperliquidCatalog;

/// This adapter's DECLARED static capability row. Values live once in
/// [`vike_model::venues::venue_caps`]; re-exported here for discoverability next to the adapter. Modify is
/// venue cancel-replace, batch is native (HL is batch-first), reduce-only on perps; TIFs Gtc/Ioc.
pub const CAPS: vike_model::VenueCaps = vike_model::venues::venue_caps::HYPERLIQUID;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("hyperliquid");
        assert_eq!(super::CAPS, caps);
        assert!(caps.supports_modify); // cancel-replace modify wired at the exec seam
        assert!(caps.supports_native_batch); // HL /exchange order action is batch-first
        assert!(caps.supports_reduce_only); // perp orders carry reduceOnly

        // `vike_hyperliquid::feeds::history::HyperliquidKlines` pages candleSnapshot into `append_bars`
        // (the datahub's `Backfill` verb, through `vike_datahub::backfill::KLINE_SOURCES`; first
        // shipped as a hyperliquid_backfill program, deleted since, and the impl itself lived
        // in vike-backfill's own `venues/hyperliquid.rs` until docs/decisions/0094 moved
        // it here); the caps row said `false` long after that landed.
        assert!(caps.backfill_bars && !caps.backfill_ticks);

        // Expanded axes (w2-task-5): the RICH trigger venue — `build_order_wire` routes `"stop"`,
        // `"stop_limit"` AND `"take_profit"` through `build_trigger_kind` (triggerPx ←
        // trigger_price, tpsl:"sl"/"tp", market-or-limit by whether `price` is set), and
        // `"market"` is the emulated IOC limit at ±5% off mid. `accepted_tifs`
        // is all five (Fok→Ioc, Gtd/Day→Gtc are the live coercions — a separate flip), while the
        // honored set is only Gtc/Ioc. `Alo` (post-only) is unreachable from OrderRequest. The
        // batch action carries N wires with NO adapter-side cap → usize::MAX.
        //
        // `"stop_limit"` was MISSING from this list, so `vike_model::preflight_order` terminally
        // rejected an order the adapter builds correctly. `exec.rs`'s
        // `stop_limit_kind_is_declared_and_built` is the REAL tie (it drives `build_order_wire`
        // and asserts this row in one test) — unlike everything else in this module, which only
        // restates the constant.
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(
            caps.supported_order_kinds,
            &["market", "limit", "stop", "stop_limit", "take_profit"]
        );
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss, TriggerType::TakeProfit]);
        assert_eq!(caps.accepted_tifs.len(), 5);
        assert_eq!(caps.supported_tifs, &[TimeInForce::Gtc, TimeInForce::Ioc]);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, usize::MAX);
        assert!(!caps.supports_post_only);
    }
}
