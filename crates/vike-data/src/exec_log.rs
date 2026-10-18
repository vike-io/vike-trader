//! Execution trade-log rows — the ACCOUNT fill/order series (Tier-2), NOT market prints.
//!
//! Two durable series persist a strategy's own execution history:
//! - `kind=exec_fill` ([`ExecFillRow`]) — one account fill (a trade against the book WE did).
//! - `kind=exec_order` ([`ExecOrderRow`]) — one order lifecycle snapshot (submit/accept/…/terminal).
//!
//! CRITICAL namespace note: the store's existing `kind=trade` series is MARKET trade ticks (a
//! symbol's public prints, [`vike_marketdata::TradeTick`]). These two kinds are DISTINCT so account fills
//! never collide with a symbol's market prints even when `(venue, symbol)` match — see the codecs in
//! `vike-store`'s `datafusion_hist::codec` and the namespace-guard test in
//! `crates/vike-store/tests/series/exec_log_series.rs`.
//!
//! Pure data — no I/O. The DataFusion+Parquet codecs (schema/encode/decode) live next to the other
//! series codecs in `vike-store`'s `datafusion_hist::codec` (they impl that crate's private
//! `SeriesCodec` trait, so they must sit inside that module); these row types are model-only,
//! exactly like [`vike_model::EquitySample`], so the trait signatures in [`crate::HistStore`] can
//! name them.

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
    /// OUR client order id this fill belongs to, or `""` when the source did not return one.
    ///
    /// ⚠ Never the venue's own order id: a venue trade-history import (Binance `myTrades` carries
    /// `orderId` and no `clientOrderId`) leaves this EMPTY and puts the venue's id in
    /// [`Self::venue_order_id`], so a fill never claims to belong to an order of ours it cannot
    /// name, and joining fills to `exec_order` rows by this field never matches a venue id.
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
    /// The VENUE's own order id for the order this fill belongs to; `None` when the writer did not
    /// know it. Additive (appended after `commission_asset`): a part written before the column
    /// existed reads back `None` (`kind=exec_fill` in `crate::store::store_kind`), and the serde
    /// field is skipped when `None`, so a reply carrying no id is byte-identical to the old one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue_order_id: Option<String>,
    /// The ACCOUNT label the fill was booked on (`vike_model::accounts::account_keys::AccountLabel`'s
    /// screen spelling, `DEFAULT` for the unlabelled account); `None` when the writer did not know
    /// it. The partition is `venue + symbol`, so before this column two accounts of one venue were
    /// indistinguishable in the history. Additive, same old-part and serde contract as
    /// [`Self::venue_order_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

impl ExecFillRow {
    /// This stored row back as the `FillEvent` a trade reconstruction folds — the desktop's
    /// Tearsheet reads its fills from the datahub's store and hands these to
    /// `vike_analytics::tearsheet_from_fills`. Every account-affecting field round-trips.
    ///
    /// ⚠ **One field does not: `position_side`.** The exec-fill series does not persist it, so
    /// every converted fill is `BOTH` (one-way). Trade reconstruction and every metric ignore it on a
    /// one-way account; on a HEDGE-mode perp account a long and a short leg of one symbol would fold
    /// as one book. Recorded rather than guessed.
    ///
    /// **`None` for a row whose `trade_id` is empty**, which is why this is fallible:
    /// [`Self::trade_id`] stays a `String` (the stored schema keeps reading parts written before
    /// `vike_model::events::TradeId` existed), so validation belongs where a stored row becomes a
    /// `FillEvent` again. One such row must not make a whole history unreadable: a caller SKIPS it
    /// and keeps the rest (the GUI shows the stored row on its Fills tab, so the skip is visible as
    /// a fill the Summary did not count).
    pub fn to_fill_event(&self) -> Option<vike_model::events::FillEvent> {
        let trade_id = vike_model::events::TradeId::new(&self.trade_id).ok()?;
        Some(vike_model::events::FillEvent {
            trade_id,
            client_order_id: self.client_order_id.clone(),
            venue: self.venue.as_str().into(),
            symbol: self.symbol.as_str().into(),
            side: self.side,
            last_qty: self.qty,
            last_px: self.px,
            commission: self.commission,
            commission_asset: self.commission_asset.as_str().into(),
            liquidity_side: self.liquidity_side.as_str().into(),
            ts: self.ts,
            mark_price: self.mark_price,
            position_side: vike_model::events::PositionSide::Both,
        })
    }
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
    /// The ACCOUNT label the order was routed to — [`ExecFillRow::account`]'s contract: `None` when
    /// the writer did not know it, appended LAST, an old part reads back `None`, skipped by serde
    /// when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
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
            venue_order_id: Some("v1".into()),
            account: Some("ALT".into()),
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
        fill_no_mark.venue_order_id = None;
        fill_no_mark.account = None;
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
            account: Some("ALT".into()),
        };
        let back: ExecOrderRow =
            serde_json::from_str(&serde_json::to_string(&order).unwrap()).unwrap();
        assert_eq!(order, back);
    }

    /// The two additive fields are ABSENT from the JSON when `None` and read back `None` from a
    /// document that never had them — the wire half of the store's old-part contract: a datahub
    /// older than the columns answers rows without the keys, and a new reader must accept them.
    #[test]
    fn the_additive_fields_are_skipped_when_none_and_default_when_absent() {
        let old = r#"{"ts":1,"trade_id":"t","client_order_id":"","venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,"px":2.0,"commission":0.0,"mark_price":null,"liquidity_side":"","commission_asset":""}"#;
        let fill: ExecFillRow = serde_json::from_str(old).unwrap();
        assert_eq!((fill.venue_order_id.as_deref(), fill.account.as_deref()), (None, None));
        assert_eq!(serde_json::to_string(&fill).unwrap(), old, "a None field adds no key");

        let old_order = r#"{"ts":1,"client_order_id":"c","venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"LIMIT","status":"FILLED","price":null,"trigger_price":null,"venue_order_id":null,"filled_qty":1.0,"avg_fill_px":2.0}"#;
        let order: ExecOrderRow = serde_json::from_str(old_order).unwrap();
        assert_eq!(order.account, None);
        assert_eq!(serde_json::to_string(&order).unwrap(), old_order);
    }

    fn fill_row(trade_id: &str) -> ExecFillRow {
        ExecFillRow {
            ts: 10,
            trade_id: trade_id.into(),
            client_order_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            px: 100.0,
            commission: 0.1,
            mark_price: Some(100.5),
            liquidity_side: "maker".into(),
            commission_asset: "USDT".into(),
            venue_order_id: None,
            account: None,
        }
    }

    /// `mark_price`, `liquidity_side` and `commission_asset` round-trip verbatim; `position_side`
    /// is the one field the series does not persist and reads `BOTH`.
    #[test]
    fn to_fill_event_carries_the_stored_fields() {
        let ev = fill_row("t1").to_fill_event().expect("a non-empty trade_id converts");
        assert_eq!(ev.mark_price, Some(100.5));
        assert!(ev.liquidity_side.is_maker());
        assert_eq!(ev.commission_asset.as_str(), "USDT");
        assert_eq!((ev.last_qty, ev.last_px, ev.ts), (1.0, 100.0, 10));
    }

    /// A row with an EMPTY trade id converts to nothing, so a caller skips it instead of failing.
    #[test]
    fn an_empty_trade_id_converts_to_nothing() {
        assert!(fill_row("").to_fill_event().is_none());
    }
}
