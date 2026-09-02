//! cTrader Open API venue bridge. One multiplexed protobuf/TLS socket carries BOTH
//! market data and order flow; OAuth2 token auth; two-stage ApplicationAuth+AccountAuth.
//! No Python twin — cite https://help.ctrader.com/open-api/.
pub mod catalog;
pub mod client;
pub mod config;
pub mod conn;
pub mod data;
pub mod event_mapper;
pub mod exec;
pub mod framing;
pub mod oauth;
pub mod positions;
pub mod proto;
pub mod recon_client;
pub mod symbols;
pub mod token_store;

pub use catalog::CtraderCatalog;
pub use recon_client::{recon_client, CtraderReconClient};

/// This adapter's DECLARED static capability row. Values live once in
/// [`vike_model::venue_caps`]; re-exported here for discoverability next to the adapter and, via
/// [`caps_test`], to pin the row against what `exec.rs`/`data.rs` actually wire — the last roster
/// venue that was missing this bridge-side tie (every sibling crate already has one).
pub const CAPS: vike_model::VenueCaps = vike_model::venue_caps::CTRADER;

#[cfg(test)]
mod caps_test {
    #[test]
    fn declared_caps_match_registry() {
        let caps = vike_model::caps_for("ctrader");
        assert_eq!(super::CAPS, caps);
        // Native in-place amend IS wired: `CtraderExec::modify` (exec.rs) enqueues a real
        // `Command::AmendOrder` → `AMEND_ORDER_REQ`, and `ORDER_REPLACED` maps to `OrderModified`.
        // Batch is the fan-out default (no seam override), and `order_to_new_order` sets no
        // `reduceOnly`, so both stay false.
        assert!(caps.supports_modify);
        assert!(!caps.supports_native_batch);
        assert!(!caps.supports_reduce_only);
        assert!(!caps.supports_combo);
        // `CtraderData` serves trendbars (bars) + spot L1 (quotes); trades/book/depth all return
        // `Unsupported`, so only those two live-data axes are set.
        assert!(caps.has_live_data());
        assert!(caps.live_data.bars);
        assert!(caps.live_data.quotes);
        assert!(!caps.live_data.trades);
        assert!(!caps.live_data.book);
        assert!(!caps.live_data.depth);
        // Expanded axes: `order_type_of` maps exactly `"market"`/`"limit"`/`"stop"` (Stop wires a
        // `trigger_price`); any other kind is already a local terminal `OrderRejected`. TIF is
        // never emitted (protocol default GTC), so the admit set is GTC only. `margin_mode` is
        // never read; no batch endpoint is wired at the seam.
        use vike_model::{MarginMode, TimeInForce, TriggerType};
        assert_eq!(caps.supported_order_kinds, &["market", "limit", "stop"]);
        assert_eq!(caps.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(caps.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(caps.supported_tifs, &[TimeInForce::Gtc]);
        assert_eq!(caps.margin_modes, &[MarginMode::Cross]);
        assert_eq!(caps.max_batch, 0);
        assert!(!caps.supports_post_only);
        assert!(!caps.backfill_bars);
        assert!(!caps.backfill_ticks);
    }
}
