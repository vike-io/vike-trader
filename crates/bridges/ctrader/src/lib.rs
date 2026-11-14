//! cTrader Open API venue bridge. One multiplexed protobuf/TLS socket carries BOTH
//! market data and order flow; OAuth2 token auth; two-stage ApplicationAuth+AccountAuth.
//! No Python twin — cite https://help.ctrader.com/open-api/.
pub mod adapter;
pub mod catalog;
pub mod config;
// The history lane's grant read and the spec the gatewayhub's credentialed kline engine runs it through.
pub mod credentialed_klines;
// What the gatewayhub asks of cTrader to admit any symbol: the account-listed spec, the rate guard's spec, the rate.
pub mod symbol_properties;

// The in-process fake cTrader server and the order/ingest helpers this crate's integration binaries
// share, compiled once here instead of once per binary. Behind `test-support`, which only the self
// dev-dependency turns on.
#[cfg(feature = "test-support")]
pub mod test_support;

pub mod account;
pub mod execution;
pub mod feed;
pub mod history;
pub mod instrument;
pub mod reconcile;
pub mod transport;

pub use catalog::CatalogProviderCtrader;
pub use reconcile::{ReconcileClientCtrader, recon_client};

/// This adapter's DECLARED static capability row. Values live once in
/// [`vike_venues::venue_caps`]; re-exported here for discoverability next to the adapter and, via
/// `capabilities_test`, to pin the row against what `execution/mod.rs`/`feed/mod.rs` actually wire — the last roster
/// venue that was missing this bridge-side tie (every sibling crate already has one).
pub const CAPABILITIES: vike_venues::VenueCapabilities = vike_venues::venue_caps::CTRADER;

#[cfg(test)]
mod capabilities_test {
    #[test]
    fn declared_capabilities_match_registry() {
        let capabilities = vike_venues::capabilities_for("ctrader");
        assert_eq!(super::CAPABILITIES, capabilities);
        // Native in-place amend IS wired: `ExecutionClientCtrader::modify` (execution/mod.rs) enqueues a real
        // `Command::AmendOrder` → `AMEND_ORDER_REQ`, and `ORDER_REPLACED` maps to `OrderModified`.
        // Batch is the fan-out default (no seam override), and `order_to_new_order` sets no
        // `reduceOnly`, so both stay false.
        assert!(capabilities.supports_modify);
        assert!(!capabilities.supports_native_batch);
        assert!(!capabilities.supports_reduce_only);
        assert!(!capabilities.supports_combo);
        // `DataClientBusSpecCtrader` serves trendbars (bars) + spot L1 (quotes); trades/book/depth all return
        // `Unsupported`, so only those two live-data axes are set.
        assert!(capabilities.has_live_data());
        assert!(capabilities.live_data.bars);
        assert!(capabilities.live_data.quotes);
        assert!(!capabilities.live_data.trades);
        assert!(!capabilities.live_data.book);
        assert!(!capabilities.live_data.depth);
        // Expanded axes: `order_type_of` maps exactly `"market"`/`"limit"`/`"stop"`/`"stop_limit"`
        // (Stop wires a `trigger_price`; a stop-limit is `STOP_LIMIT` with `slippageInPoints`); any
        // other kind is already a local terminal `OrderRejected`. A `reduce_only` stop-limit binds
        // as a stop does on a hedge-capable account. TIF is never emitted (protocol default GTC),
        // so the admit set is GTC only. `margin_mode` is never read; no batch endpoint is wired at
        // the seam.
        use vike_model::{MarginMode, TimeInForce};
        use vike_venues::TriggerType;
        assert_eq!(capabilities.supported_order_kinds, &["market", "limit", "stop", "stop_limit"]);
        assert_eq!(capabilities.reduce_only_kinds, &["market", "limit", "stop", "stop_limit"]);
        assert_eq!(capabilities.trigger_types, &[TriggerType::StopLoss]);
        assert_eq!(capabilities.accepted_tifs, &[TimeInForce::Gtc]);
        assert_eq!(capabilities.supported_tifs, &[TimeInForce::Gtc]);
        assert_eq!(capabilities.margin_modes, &[MarginMode::Cross]);
        assert_eq!(capabilities.max_batch, 0);
        assert!(!capabilities.supports_post_only);
        // `history::KlineSourceCtrader` is the bars-only history source; no tick request is wired.
        assert!(capabilities.backfill_bars);
        assert!(!capabilities.backfill_ticks);
    }
}
