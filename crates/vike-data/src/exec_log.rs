//! Execution trade-log rows — the ACCOUNT fill/order series (Tier-2), NOT market prints.
//!
//! Two durable series persist a strategy's own execution history:
//! - `kind=exec_fill` ([`ExecFillRow`]) — one account fill (a trade against the book WE did).
//! - `kind=execution_order` ([`ExecOrderRow`]) — one order lifecycle snapshot (submit/accept/…/terminal).
//!
//! CRITICAL namespace note: the store's existing `kind=trade` series is MARKET trade ticks (a
//! symbol's public prints, [`vike_marketdata::TickTrade`]). These two kinds are DISTINCT so account fills
//! never collide with a symbol's market prints even when `(venue, symbol)` match — see the codecs in
//! `vike-store`'s `datafusion_hist::codec` and the namespace-guard test in
//! `crates/vike-store/tests/series/execution_log_series.rs`.
//!
//! Pure data — no I/O. The DataFusion+Parquet codecs (schema/encode/decode) live next to the other
//! series codecs in `vike-store`'s `datafusion_hist::codec` (they impl that crate's private
//! `SeriesCodec` trait, so they must sit inside that module); these row types are model-only,
//! exactly like [`vike_model::EquitySample`], so the trait signatures in [`crate::HistoryStore`] can
//! name them.

use serde::{Deserialize, Serialize};

/// One ACCOUNT fill (our trade against the book), persisted as the `kind=exec_fill` HistoryStore series.
/// Identity `(venue, symbol)` is BOTH the partition path AND stored columns (so the row round-trips
/// whole through compaction, which re-encodes without a decode-time re-injection context).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecFillRow {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// venue-assigned trade/execution id (the fill's unique id).
    pub trade_id: String,
    /// OUR order client id this fill belongs to, or `""` when the source did not return one.
    ///
    /// ⚠ Never the venue's own order id: a venue trade-history import (Binance `myTrades` carries
    /// `orderId` and no `clientOrderId`) leaves this EMPTY and puts the venue's id in
    /// [`Self::venue_order_id`], so a fill never claims to belong to an order of ours it cannot
    /// name, and joining fills to `execution_order` rows by this field never matches a venue id. The
    /// STORED column keeps its text: the `exec_*` store kinds are never renamed.
    #[serde(rename = "client_order_id")]
    pub order_client_id: String,
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
    /// The fill's hedge leg, `LONG` or `SHORT` (`vike_model::events::PositionSide`'s wire word);
    /// `None` for a one-way fill, and for every row written before the column existed, which read
    /// back `BOTH`. Additive, same old-part and serde contract as [`Self::venue_order_id`], so a
    /// one-way row is byte-identical to the old one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_side: Option<String>,
    /// `Some` marks a FEE CORRECTION row and names the fill it restates
    /// ([`Self::fee_correction_row`]). Additive, same contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrects_trade_id: Option<String>,
    /// A correction row's journal record seq: which of an execution's corrections is the last
    /// ([`Self::fill_events`]). `None` on a fill. Additive, same contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_seq: Option<i64>,
}

impl ExecFillRow {
    /// A fee correction (`Event::FeeAdjustment`, decision 0238) as a row of the append-only series:
    /// a fill's own row is never rewritten.
    ///
    /// - [`Self::commission`] is the correction's total over the fill, in
    ///   [`Self::commission_asset`], and the row moves no quantity.
    /// - [`Self::ts`] is the event's, which the one sender stamps with the fill's own time, so a
    ///   windowed read that holds the fill holds its corrections (reads never split a `ts`).
    /// - ⚠ **[`Self::trade_id`] is EMPTY on purpose, with the fill's id in
    ///   [`Self::corrects_trade_id`].** A build older than the column cannot tell this row from a
    ///   fill, and an empty id is the one row it already skips: its sheet stays what it was, where
    ///   the fill's id would fold a zero-quantity fill and close a size-0 trade on an open long.
    /// - [`Self::correction_seq`] is there because the store's order cannot say which correction
    ///   is the last: they tie on `ts`, and a read orders the parts by their `ts` span, so a later
    ///   part can come first.
    pub fn fee_correction_row(
        a: &vike_model::events::FeeAdjustmentEvent,
        journal_sequence: u64,
        venue_order_id: Option<String>,
        account: Option<String>,
    ) -> Self {
        ExecFillRow {
            ts: a.timestamp,
            trade_id: String::new(),
            order_client_id: a.order_client_id.clone(),
            venue: a.venue.to_string(),
            symbol: a.symbol.to_string(),
            side: 0,
            qty: 0.0,
            px: a.last_price,
            commission: a.extra_commission,
            mark_price: None,
            liquidity_side: String::new(),
            commission_asset: a.commission_asset.to_string(),
            venue_order_id,
            account,
            position_side: None,
            corrects_trade_id: Some(a.trade_id.to_string()),
            correction_seq: Some(journal_sequence as i64),
        }
    }

    /// **THE read of stored rows as fills**, in `rows`' order: the desktop's Tearsheet hands these
    /// to `vike_analytics::tearsheet_from_fills`. Fee corrections are folded into the fills they
    /// restate through the journal door's own `vike_model::events::FeeCorrections`, whatever order
    /// `rows` comes in, and a correction whose fill is not in `rows` moves nothing.
    ///
    /// A row written before the corrections were stored keeps the fee it was written with: nothing
    /// rewrites it, so only the journal door knows that older correction.
    pub fn fill_events(rows: &[ExecFillRow]) -> Vec<vike_model::events::FillEvent> {
        let mut corrections: Vec<&ExecFillRow> =
            rows.iter().filter(|r| r.corrects_trade_id.is_some()).collect();
        corrections.sort_by_key(|r| r.correction_seq);
        let mut fees = vike_model::events::FeeCorrections::default();
        for a in corrections.into_iter().filter_map(Self::to_fee_adjustment) {
            fees.note(&a);
        }
        let mut fills: Vec<_> = rows.iter().filter_map(Self::to_fill_event).collect();
        fees.fold_into(&mut fills);
        fills
    }

    /// The inverse of [`Self::fee_correction_row`]; `None` for a fill's row.
    fn to_fee_adjustment(&self) -> Option<vike_model::events::FeeAdjustmentEvent> {
        let trade_id = self.corrects_trade_id.as_deref()?;
        Some(vike_model::events::FeeAdjustmentEvent {
            trade_id: vike_model::events::TradeId::new(trade_id).ok()?,
            order_client_id: self.order_client_id.clone(),
            venue: self.venue.as_str().into(),
            symbol: self.symbol.as_str().into(),
            last_price: self.px,
            extra_commission: self.commission,
            commission_asset: self.commission_asset.as_str().into(),
            timestamp: self.ts,
        })
    }

    /// This stored row back as the `FillEvent` a trade reconstruction folds. Every
    /// account-affecting field round-trips, the fee as the fill stated it ([`Self::fill_events`]
    /// folds the corrections).
    ///
    /// **`None` for a row whose `trade_id` is empty**, which is why this is fallible and what a
    /// correction row is:
    /// [`Self::trade_id`] stays a `String` (the stored schema keeps reading parts written before
    /// `vike_model::events::TradeId` existed), so validation belongs where a stored row becomes a
    /// `FillEvent` again. One such row must not make a whole history unreadable: a caller SKIPS it
    /// and keeps the rest (the GUI shows the stored row on its Fills tab, so the skip is visible as
    /// a fill the Summary did not count).
    fn to_fill_event(&self) -> Option<vike_model::events::FillEvent> {
        let trade_id = vike_model::events::TradeId::new(&self.trade_id).ok()?;
        Some(vike_model::events::FillEvent {
            trade_id,
            order_client_id: self.order_client_id.clone(),
            venue: self.venue.as_str().into(),
            symbol: self.symbol.as_str().into(),
            side: self.side,
            last_qty: self.qty,
            last_price: self.px,
            commission: self.commission,
            commission_asset: self.commission_asset.as_str().into(),
            liquidity_side: self.liquidity_side.as_str().into(),
            timestamp: self.ts,
            mark_price: self.mark_price,
            position_side: vike_model::events::PositionSide::from_word_or_both(
                self.position_side.as_deref().unwrap_or_default(),
            ),
        })
    }
}

/// One ORDER lifecycle snapshot (submit → accept → … → terminal), persisted as the `kind=execution_order`
/// HistoryStore series. Same identity contract as [`ExecFillRow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecOrderRow {
    /// epoch milliseconds (UTC)
    pub ts: i64,
    /// the order client id (our idempotency handle for the order). The STORED column keeps its text:
    /// the `exec_*` store kinds are never renamed.
    #[serde(rename = "client_order_id")]
    pub order_client_id: String,
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
    fn execution_rows_roundtrip_serde_and_eq() {
        let fill = ExecFillRow {
            ts: 1_000,
            trade_id: "t1".into(),
            order_client_id: "c1".into(),
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
            position_side: Some("LONG".into()),
            corrects_trade_id: Some("t0".into()),
            correction_seq: Some(7),
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
        fill_no_mark.position_side = None;
        fill_no_mark.corrects_trade_id = None;
        fill_no_mark.correction_seq = None;
        let back: ExecFillRow =
            serde_json::from_str(&serde_json::to_string(&fill_no_mark).unwrap()).unwrap();
        assert_eq!(fill_no_mark, back);

        let order = ExecOrderRow {
            ts: 1_001,
            order_client_id: "c1".into(),
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

    /// The additive fields are ABSENT from the JSON when `None` and read back `None` from a
    /// document that never had them — the wire half of the store's old-part contract: a datahub
    /// older than the columns answers rows without the keys, and a new reader must accept them.
    #[test]
    fn the_additive_fields_are_skipped_when_none_and_default_when_absent() {
        let old = r#"{"ts":1,"trade_id":"t","client_order_id":"","venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,"px":2.0,"commission":0.0,"mark_price":null,"liquidity_side":"","commission_asset":""}"#;
        let fill: ExecFillRow = serde_json::from_str(old).unwrap();
        assert_eq!((fill.venue_order_id.as_deref(), fill.account.as_deref()), (None, None));
        assert_eq!(
            (fill.position_side.as_deref(), fill.corrects_trade_id.as_deref()),
            (None, None)
        );
        assert_eq!(fill.correction_seq, None);
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
            order_client_id: "c1".into(),
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
            position_side: None,
            corrects_trade_id: None,
            correction_seq: None,
        }
    }

    /// `mark_price`, `liquidity_side` and `commission_asset` round-trip verbatim.
    #[test]
    fn to_fill_event_carries_the_stored_fields() {
        let event = fill_row("t1").to_fill_event().expect("a non-empty trade_id converts");
        assert_eq!(event.mark_price, Some(100.5));
        assert!(event.liquidity_side.is_maker());
        assert_eq!(event.commission_asset.as_str(), "USDT");
        assert_eq!((event.last_qty, event.last_price, event.timestamp), (1.0, 100.0, 10));
    }

    /// A stated leg comes back as stated; a row without the column is the one-way fill it always was.
    #[test]
    fn a_stated_leg_round_trips_and_an_absent_one_reads_both() {
        use vike_model::events::PositionSide;
        let side = |word: Option<&str>| {
            let row = ExecFillRow { position_side: word.map(Into::into), ..fill_row("t1") };
            row.to_fill_event().unwrap().position_side
        };
        assert_eq!(side(Some("LONG")), PositionSide::Long);
        assert_eq!(side(Some("SHORT")), PositionSide::Short);
        assert_eq!(side(None), PositionSide::Both);
    }

    /// A row with an EMPTY trade id converts to nothing, so a caller skips it instead of failing.
    #[test]
    fn an_empty_trade_id_converts_to_nothing() {
        assert!(fill_row("").to_fill_event().is_none());
    }

    fn correction(trade_id: &str, extra: f64, seq: u64) -> ExecFillRow {
        let a = vike_model::events::FeeAdjustmentEvent {
            trade_id: vike_model::events::TradeId::new(trade_id).unwrap(),
            order_client_id: "c1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            last_price: 100.0,
            extra_commission: extra,
            commission_asset: "BNB".into(),
            timestamp: 10,
        };
        ExecFillRow::fee_correction_row(&a, seq, None, None)
    }

    /// A row carrying no correction reads as the same fills it read as one by one, in `rows`' order.
    #[test]
    fn rows_with_no_correction_read_as_they_did() {
        let rows = [fill_row("a"), fill_row("b")];
        let per_row: Vec<_> = rows.iter().filter_map(ExecFillRow::to_fill_event).collect();
        assert_eq!(ExecFillRow::fill_events(&rows), per_row);
    }

    /// **The last correction by journal seq wins, whatever order the rows come in**: the desktop hands
    /// them newest first, and the store's order of rows tied on `ts` is not journal order. A correction
    /// is no fill of its own, and a fill that stated no fee currency takes the correction's.
    #[test]
    fn the_last_correction_by_journal_sequence_wins_in_any_row_order() {
        let unknown =
            ExecFillRow { commission: 0.0, commission_asset: String::new(), ..fill_row("a") };
        let (first, last) = (correction("a", 1.00, 5), correction("a", 1.25, 9));
        for rows in [
            [unknown.clone(), first.clone(), last.clone()],
            [unknown.clone(), last.clone(), first.clone()],
            [last.clone(), first.clone(), unknown.clone()],
        ] {
            let fills = ExecFillRow::fill_events(&rows);
            assert_eq!(fills.len(), 1, "two corrections are no fills");
            assert_eq!(fills[0].commission, 1.25);
            assert_eq!(fills[0].commission_asset.as_str(), "BNB");
        }
    }

    /// A correction whose fill is outside the rows read moves nothing, and adds to a stated fee.
    #[test]
    fn a_correction_adds_to_its_own_fill_and_no_other() {
        let rows =
            [fill_row("a"), fill_row("b"), correction("a", 0.25, 3), correction("gone", 9.0, 4)];
        let fills = ExecFillRow::fill_events(&rows);
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].commission, 0.1 + 0.25);
        assert_eq!(fills[1].commission, 0.1);
    }

    /// **What a build older than the columns does with a correction row**: it decodes (an unknown
    /// JSON key is ignored) and, the id being empty, it is the row that build already skips.
    #[test]
    fn a_correction_row_is_the_row_an_older_reader_skips() {
        let row = correction("a", 1.25, 3);
        assert_eq!((row.trade_id.as_str(), row.qty), ("", 0.0));
        assert!(row.to_fill_event().is_none());
        let json = serde_json::to_string(&row).unwrap();
        assert!(json.contains(r#""corrects_trade_id":"a""#), "{json}");
        #[derive(Deserialize)]
        struct OlderRow {
            trade_id: String,
        }
        let older: OlderRow = serde_json::from_str(&json).unwrap();
        assert_eq!(older.trade_id, "");
    }
}
