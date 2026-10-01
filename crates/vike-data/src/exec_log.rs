//! Execution trade-log rows — the ACCOUNT fill/order series (Tier-2), NOT market prints.
//!
//! Two durable series persist a strategy's own execution history:
//! - `kind=exec_fill` ([`ExecFillRow`]) — one account fill (a trade against the book WE did).
//! - `kind=exec_order` ([`ExecOrderRow`]) — one order lifecycle snapshot (submit/accept/…/terminal).
//!
//! CRITICAL namespace note: the store's existing `kind=trade` series is MARKET trade ticks (a
//! symbol's public prints, [`vike_model::TradeTick`]). These two kinds are DISTINCT so account fills
//! never collide with a symbol's market prints even when `(venue, symbol)` match — see the codecs in
//! `datafusion_hist::codec` and the namespace-guard test in `tests/exec_log_series.rs`.
//!
//! Pure data — no I/O. The DataFusion+Parquet codecs (schema/encode/decode) live next to the other
//! series codecs in `datafusion_hist::codec` (they impl the crate-private `SeriesCodec` trait, so
//! they must sit inside that module); these row types are always compiled and model-only, exactly
//! like [`vike_model::EquitySample`], so the trait signatures in [`crate::HistStore`] can name them
//! without the `hist-datafusion` feature.

use serde::{Deserialize, Serialize};

/// One ACCOUNT fill (our trade against the book), persisted as the `kind=exec_fill` HistStore series.
/// Identity `(venue, symbol)` is BOTH the partition path AND stored columns (so the row round-trips
/// whole through compaction, which re-encodes without a decode-time re-injection context).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecFillRow {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// venue-assigned trade/execution id (the fill's unique id).
    pub trade_id: String,
    /// the client order id this fill belongs to.
    pub client_order_id: String,
    pub venue: String,
    pub symbol: String,
    /// order side, ±1 (buy = +1, sell = -1).
    pub side: i32,
    pub qty: f64,
    pub px: f64,
    pub commission: f64,
    /// venue mark price at fill time (perp markout); None if the venue didn't surface it
    pub mark_price: Option<f64>,
    /// "maker" | "taker" | "" (venue didn't surface it) — MM fill-rate/adverse-selection analytics.
    pub liquidity_side: String,
    /// fee currency of `commission` (e.g. "USDT", "BNB"); "" if the venue didn't surface it.
    pub commission_asset: String,
}

/// One ORDER lifecycle snapshot (submit → accept → … → terminal), persisted as the `kind=exec_order`
/// HistStore series. Same identity contract as [`ExecFillRow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecOrderRow {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// the client order id (our idempotency handle for the order).
    pub client_order_id: String,
    pub venue: String,
    pub symbol: String,
    /// order side, ±1 (buy = +1, sell = -1).
    pub side: i32,
    pub qty: f64,
    pub order_type: String,
    pub status: String,
    /// limit price (`None` for market orders).
    pub price: Option<f64>,
    /// stop/trigger price (`None` when not a triggered order).
    pub trigger_price: Option<f64>,
    /// venue-assigned order id (`None` before the venue acknowledges).
    pub venue_order_id: Option<String>,
    pub filled_qty: f64,
    pub avg_fill_px: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_rows_roundtrip_serde_and_eq() {
        let fill = ExecFillRow {
            ts: 1_000,
            trade_id: "t1".into(),
            client_order_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            px: 65_000.0,
            commission: 0.13,
            mark_price: Some(65_001.0),
            liquidity_side: "maker".into(),
            commission_asset: "BNB".into(),
        };
        let back: ExecFillRow =
            serde_json::from_str(&serde_json::to_string(&fill).unwrap()).unwrap();
        assert_eq!(fill, back);

        // the "not surfaced" states (mark_price = None, liquidity_side/commission_asset = "") also
        // round-trip through serde.
        let mut fill_no_mark = fill.clone();
        fill_no_mark.mark_price = None;
        fill_no_mark.liquidity_side = String::new();
        fill_no_mark.commission_asset = String::new();
        let back: ExecFillRow =
            serde_json::from_str(&serde_json::to_string(&fill_no_mark).unwrap()).unwrap();
        assert_eq!(fill_no_mark, back);

        let order = ExecOrderRow {
            ts: 1_001,
            client_order_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 0.5,
            order_type: "LIMIT".into(),
            status: "FILLED".into(),
            price: Some(65_000.0),
            trigger_price: None,
            venue_order_id: Some("v1".into()),
            filled_qty: 0.5,
            avg_fill_px: 65_000.0,
        };
        let back: ExecOrderRow =
            serde_json::from_str(&serde_json::to_string(&order).unwrap()).unwrap();
        assert_eq!(order, back);
    }
}
