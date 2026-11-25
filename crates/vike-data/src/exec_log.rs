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
    /// The MOUNT (live strategy) that minted the order this fill belongs to, so two strategies on one
    /// account and symbol are told apart; a fee correction row carries its fill's. `None` for an
    /// operator's order and whenever the writer did not know it. Additive, same contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_id: Option<String>,
    /// When vike RECEIVED the row's event, epoch milliseconds UTC (the journal record's `now_ms`),
    /// set on a fee correction row: its [`Self::ts`] is its FILL's, so this is the clock that says
    /// when the corrected fee became known. `None` on a fill and whenever the writer did not know it
    /// (a correction row written before the column). Additive, same contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_ms: Option<i64>,
    /// Whose execution it is, as the tradehub's execution stream stated it: [`OWNERSHIP_OURS`] (this install's
    /// order, the one kind a strategy's results fold) or [`OWNERSHIP_FOREIGN`] (another install's, a venue-UI
    /// order, a reconcile `EXT-` id: never a strategy's, but the ACCOUNT's, so an account's results fold it too and
    /// mark it, the coordinator's ruling of 2026-11-18). `None` marks a row written BEFORE the stream (the retired journal
    /// materializer's, or a venue trade-history import's). Additive, same contract as [`Self::venue_order_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<String>,
    /// The journal sequence of the stream report this row states ([`ExecOrderRow::journal_sequence`]'s twin): where
    /// the fill falls against a tradehub's POSITION SNAPSHOT, whose book holds every fill before it in the journal
    /// whatever their venue times ([`Self::after_position_snapshot`]). `None` on a row written before the stream.
    /// Additive, same contract as [`Self::venue_order_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_sequence: Option<i64>,
    /// Which of the datahub's journals [`Self::journal_sequence`] counts in ([`ExecOrderRow::journal_generation`]'s
    /// twin). `None` on a row written before the stream. Additive, same contract as [`Self::venue_order_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_generation: Option<i64>,
}

/// [`ExecFillRow::ownership`]'s word for this install's own execution.
pub const OWNERSHIP_OURS: &str = "ours";
/// [`ExecFillRow::ownership`]'s word for an execution that is not provably this install's.
pub const OWNERSHIP_FOREIGN: &str = "foreign";

/// **A fee correction's own facts as plain fields**, what [`ExecFillRow::fee_correction_row`] writes a row
/// from: a writer that holds them as text (the datahub's execution stream, off the wire) passes them as they
/// are, where building a `vike_model::events::FeeAdjustmentEvent` first would intern remote text. An event
/// converts into them (`From`), so the journal door's callers pass the event itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeeCorrectionFields<'a> {
    /// The execution id of the fill it restates ([`ExecFillRow::corrects_trade_id`]).
    pub corrects_trade_id: &'a str,
    /// The order of that fill ([`ExecFillRow::order_client_id`]).
    pub order_client_id: &'a str,
    /// The fill's venue.
    pub venue: &'a str,
    /// The fill's symbol.
    pub symbol: &'a str,
    /// The FILL's time, epoch ms: the row's [`ExecFillRow::ts`].
    pub ts: i64,
    /// The fill's price ([`ExecFillRow::px`]): an inverse contract's fee enters the ledger at it.
    pub price: f64,
    /// How much more the execution's fee is than its fill carried, in total ([`ExecFillRow::commission`]).
    pub commission: f64,
    /// The currency of [`Self::commission`]; empty when the venue did not say.
    pub commission_asset: &'a str,
}

impl<'a> From<&'a vike_model::events::FeeAdjustmentEvent> for FeeCorrectionFields<'a> {
    fn from(a: &'a vike_model::events::FeeAdjustmentEvent) -> Self {
        Self {
            corrects_trade_id: a.trade_id.as_str(),
            order_client_id: &a.order_client_id,
            venue: a.venue.as_str(),
            symbol: a.symbol.as_str(),
            ts: a.timestamp,
            price: a.last_price,
            commission: a.extra_commission,
            commission_asset: a.commission_asset.as_str(),
        }
    }
}

impl ExecFillRow {
    /// A fee correction (`Event::FeeAdjustment`, decision 0238) as a row of the append-only series:
    /// a fill's own row is never rewritten.
    ///
    /// - [`Self::commission`] is the correction's total over the fill, in
    ///   [`Self::commission_asset`], and the row moves no quantity.
    /// - [`Self::ts`] is the event's, which the one sender stamps with the fill's own time, so a
    ///   windowed read that holds the fill holds its corrections (reads never split a `ts`).
    /// - ⚠ **So `ts` says nothing about WHEN the fee was known: [`Self::received_ms`] does**, the
    ///   journal record's `now_ms`. A point-in-time read drops a correction received after its end
    ///   (`vike_user_research::StudyContext::execution_fills`).
    /// - ⚠ **[`Self::trade_id`] is EMPTY on purpose, with the fill's id in
    ///   [`Self::corrects_trade_id`].** A build older than the column cannot tell this row from a
    ///   fill, and an empty id is the one row it already skips: its sheet stays what it was, where
    ///   the fill's id would fold a zero-quantity fill and close a size-0 trade on an open long.
    /// - [`Self::correction_seq`] is there because the store's order cannot say which correction
    ///   is the last: they tie on `ts`, and a read orders the parts by their `ts` span, so a later
    ///   part can come first.
    ///
    /// `correction` is its plain fields ([`FeeCorrectionFields`]) or the journal's event itself, which
    /// converts into them. The mount, the ownership and the journal stamps stay `None`; a writer that knows
    /// them sets them on the row it gets back.
    pub fn fee_correction_row<'a>(
        correction: impl Into<FeeCorrectionFields<'a>>,
        journal_sequence: u64,
        received_ms: i64,
        venue_order_id: Option<String>,
        account: Option<String>,
    ) -> Self {
        let a = correction.into();
        ExecFillRow {
            ts: a.ts,
            trade_id: String::new(),
            order_client_id: a.order_client_id.to_string(),
            venue: a.venue.to_string(),
            symbol: a.symbol.to_string(),
            side: 0,
            qty: 0.0,
            px: a.price,
            commission: a.commission,
            mark_price: None,
            liquidity_side: String::new(),
            commission_asset: a.commission_asset.to_string(),
            venue_order_id,
            account,
            position_side: None,
            corrects_trade_id: Some(a.corrects_trade_id.to_string()),
            // Saturating, as the datahub's stream writes its sequence columns: no journal reaches 2^63.
            correction_seq: Some(i64::try_from(journal_sequence).unwrap_or(i64::MAX)),
            mount_id: None,
            received_ms: Some(received_ms),
            ownership: None,
            journal_sequence: None,
            journal_generation: None,
        }
    }

    /// **THE read of stored rows as fills**, in `rows`' order: the desktop's Tearsheet hands these
    /// to `vike_analytics::trading_results_table_from_fills`. Fee corrections are folded into the fills they
    /// restate through the journal door's own `vike_model::events::FeeCorrections`, whatever order
    /// `rows` comes in, and a correction whose fill is not in `rows` moves nothing.
    ///
    /// A row written before the corrections were stored keeps the fee it was written with: nothing
    /// rewrites it, so only the journal door knows that older correction.
    ///
    /// A row [`Self::superseded`] names is left out first, so an execution stored both by the retired journal
    /// materializer and by the execution stream is folded once, as the stream stated it.
    pub fn fill_events(rows: &[ExecFillRow]) -> Vec<vike_model::events::FillEvent> {
        let superseded = Self::superseded(rows);
        let kept = || rows.iter().zip(&superseded).filter(|(_, gone)| !**gone).map(|(r, _)| r);
        let mut corrections: Vec<&ExecFillRow> =
            kept().filter(|r| r.corrects_trade_id.is_some()).collect();
        corrections.sort_by_key(|r| r.correction_seq);
        let mut fees = vike_model::events::FeeCorrections::default();
        for a in corrections.into_iter().filter_map(Self::to_fee_adjustment) {
            fees.note(&a);
        }
        let mut fills: Vec<_> = kept().filter_map(Self::to_fill_event).collect();
        fees.fold_into(&mut fills);
        fills
    }

    /// **The one supersede rule of the fill log, never a silent merge**: within `rows`, a row the tradehub's
    /// execution stream wrote ([`Self::ownership`] set) supersedes every row of the same execution written BEFORE
    /// the stream (`ownership` `None`: the retired journal materializer's, a venue trade-history import's), which
    /// carry no account and no ownership. Returns, aligned with `rows`, whether each row is superseded.
    ///
    /// - An execution's identity is its `trade_id`; a fee correction's is its `(corrects_trade_id,
    ///   correction_seq)`, its `trade_id` being empty. A row with neither names nothing and is never superseded.
    /// - Stream rows never supersede each other, and a row from before the stream with no stream twin stays: only
    ///   the stream's own resend after a re-identified journal could repeat one, and the datahub drops that at the
    ///   write.
    /// - `rows` is one series' (venue, symbol), as every reader of fills reads them; a reader holding several
    ///   reads [`Self::not_superseded`], which applies this per series.
    pub fn superseded(rows: &[ExecFillRow]) -> Vec<bool> {
        Self::superseded_in(rows.iter())
    }

    /// [`Self::superseded`] over one series' rows, by reference, so [`Self::not_superseded`] applies the one rule to
    /// each series of a mixed slice without copying a row.
    fn superseded_in<'r>(rows: impl Iterator<Item = &'r ExecFillRow> + Clone) -> Vec<bool> {
        let streamed: std::collections::HashSet<_> = rows
            .clone()
            .filter(|r| r.ownership.is_some())
            .filter_map(Self::execution_identity)
            .collect();
        rows.map(|r| {
            r.ownership.is_none()
                && Self::execution_identity(r).is_some_and(|id| streamed.contains(&id))
        })
        .collect()
    }

    /// **The rows of `rows` no stream row restates**, in `rows`' order: [`Self::superseded`]'s rule applied to each
    /// `(venue, symbol)` series of `rows` as that rule is written, so `rows` may hold any number of series (an
    /// account's page) and an execution the stream re-stored with its account is ONE row, never also a row
    /// "without an account".
    pub fn not_superseded(rows: &[ExecFillRow]) -> Vec<&ExecFillRow> {
        let mut series: std::collections::HashMap<(&str, &str), Vec<usize>> =
            std::collections::HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            series.entry((row.venue.as_str(), row.symbol.as_str())).or_default().push(index);
        }
        let mut gone = vec![false; rows.len()];
        for indices in series.values() {
            let verdicts = Self::superseded_in(indices.iter().map(|&index| &rows[index]));
            for (&index, superseded) in indices.iter().zip(verdicts) {
                gone[index] = superseded;
            }
        }
        rows.iter().zip(gone).filter(|(_, superseded)| !superseded).map(|(row, _)| row).collect()
    }

    /// **Whether this row is a FILL**, the one rule every count of fills reads: it restates no other row
    /// ([`Self::corrects_trade_id`] `None`) AND names its execution (a non-empty [`Self::trade_id`]), which are
    /// exactly the rows [`Self::fill_events`] folds as fills. A fee correction is part of its fill, not one
    /// more; a row with an empty id names no execution, and the fold skips it.
    pub fn is_fill(&self) -> bool {
        self.corrects_trade_id.is_none() && !self.trade_id.is_empty()
    }

    /// **Whether this row is another install's execution** ([`OWNERSHIP_FOREIGN`]): never a strategy's, but its
    /// account's. A row written before the stream (no [`Self::ownership`]) is not foreign: nothing said whose it is.
    pub fn is_foreign(&self) -> bool {
        self.ownership.as_deref() == Some(OWNERSHIP_FOREIGN)
    }

    /// **The execution a row states**, [`Self::superseded`]'s key and the datahub's first-contact one: a fill's
    /// `(trade_id, None, None)`, or a fee correction's `("", Some(corrects_trade_id), correction_seq)`. `None` for a
    /// row with neither, which names no execution.
    pub fn execution_identity(&self) -> Option<(&str, Option<&str>, Option<i64>)> {
        match self.corrects_trade_id.as_deref() {
            Some(corrects) => Some(("", Some(corrects), self.correction_seq)),
            None if !self.trade_id.is_empty() => Some((self.trade_id.as_str(), None, None)),
            None => None,
        }
    }

    /// **Whether this row's execution came AFTER a tradehub's position snapshot** journaled at `(generation,
    /// sequence)` (the datahub's journal generation, the record's sequence): the snapshot's book holds every
    /// execution journaled before it, so an account's window opens from the snapshot plus exactly the rows this
    /// answers `true` for. Journal order, never [`Self::ts`]: a fill the venue made while the tradehub was down
    /// carries a venue time before the snapshot and is journaled after it. A row written before the stream (no
    /// sequence) is before every snapshot.
    pub fn after_position_snapshot(&self, (generation, sequence): (i64, i64)) -> bool {
        match (self.journal_generation, self.journal_sequence) {
            (Some(g), Some(s)) => (g, s) > (generation, sequence),
            _ => false,
        }
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
    /// The MOUNT that minted the order — [`ExecFillRow::mount_id`]'s contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_id: Option<String>,
    /// [`ExecFillRow::ownership`]'s contract: `None` marks a row written before the execution stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<String>,
    /// The journal sequence of the report this row states: the TOTAL ORDER of one order's stream rows, which the
    /// stream writes one per applied event, so two of them can share a millisecond and their `ts` (a fill's venue
    /// time, otherwise the time vike recorded the event) need not rise with them. `None` on a row written before
    /// the stream. Additive, same contract as [`Self::account`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_sequence: Option<i64>,
    /// Which of the datahub's journals [`Self::journal_sequence`] counts in: the journal's place in the order the
    /// datahub first heard from each (`vike_secrets::execution_streams`' `generation`). A journal moved aside
    /// restarts its sequences, so one order's rows are ordered by `(journal_generation, journal_sequence)`, and a
    /// newer journal's row never loses to an older journal's higher sequence. `None` on a row written before the
    /// stream. Additive, same contract as [`Self::account`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_generation: Option<i64>,
    /// Why the order ended this way, in the words of whoever ended it, as the stream's report stated them: the risk
    /// gate's for `DENIED`, the venue's for `REJECTED`, the core's for a cancel or a reject it made up. `None` when the
    /// report carried none, and on every row written before the stream. Additive, same contract as [`Self::account`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ExecOrderRow {
    /// The stored [`Self::status`] word as the order lifecycle state it spells
    /// (`vike_model::OrderStatus::parse`, the inverse of the word its writer stored), or `None` for a
    /// word this build has no state for: a row a NEWER writer stored, which a reader skips and says so
    /// rather than reading as some other state.
    #[must_use]
    pub fn order_status(&self) -> Option<vike_model::OrderStatus> {
        vike_model::OrderStatus::parse(&self.status)
    }
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
            mount_id: Some("maker-a".into()),
            received_ms: Some(2_000),
            ownership: Some(OWNERSHIP_OURS.into()),
            journal_sequence: None,
            journal_generation: None,
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
        fill_no_mark.mount_id = None;
        fill_no_mark.received_ms = None;
        fill_no_mark.ownership = None;
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
            mount_id: Some("maker-a".into()),
            ownership: Some(OWNERSHIP_FOREIGN.into()),
            journal_sequence: Some(41),
            journal_generation: Some(2),
            reason: Some("the venue refused it".into()),
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
        assert_eq!((fill.correction_seq, fill.mount_id.as_deref()), (None, None));
        assert_eq!((fill.received_ms, fill.ownership.as_deref()), (None, None));
        assert_eq!(serde_json::to_string(&fill).unwrap(), old, "a None field adds no key");

        let old_order = r#"{"ts":1,"client_order_id":"c","venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"LIMIT","status":"FILLED","price":null,"trigger_price":null,"venue_order_id":null,"filled_qty":1.0,"avg_fill_px":2.0}"#;
        let order: ExecOrderRow = serde_json::from_str(old_order).unwrap();
        assert_eq!((order.account.as_deref(), order.mount_id.as_deref()), (None, None));
        assert_eq!((order.ownership.as_deref(), order.journal_sequence), (None, None));
        assert_eq!((order.journal_generation, order.reason.as_deref()), (None, None));
        assert_eq!(serde_json::to_string(&order).unwrap(), old_order);
    }

    /// A stored status word reads as the state it spells, every state's word round trips, and a word
    /// no state spells (a newer writer's) is `None`, never another state.
    #[test]
    fn a_stored_status_word_reads_as_its_order_status_or_none() {
        let old_order = r#"{"ts":1,"client_order_id":"c","venue":"binance","symbol":"BTCUSDT","side":1,"qty":1.0,"order_type":"LIMIT","status":"FILLED","price":null,"trigger_price":null,"venue_order_id":null,"filled_qty":1.0,"avg_fill_px":2.0}"#;
        let mut order: ExecOrderRow = serde_json::from_str(old_order).unwrap();
        assert_eq!(order.order_status(), Some(vike_model::OrderStatus::Filled));
        for status in [
            vike_model::OrderStatus::Initialized,
            vike_model::OrderStatus::Accepted,
            vike_model::OrderStatus::PartiallyFilled,
            vike_model::OrderStatus::Canceled,
            vike_model::OrderStatus::Released,
        ] {
            order.status = status.as_str().to_string();
            assert_eq!(order.order_status(), Some(status));
        }
        for unknown in ["SOME_FUTURE_STATUS", "filled", ""] {
            order.status = unknown.to_string();
            assert_eq!(order.order_status(), None, "{unknown:?}");
        }
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
            mount_id: None,
            received_ms: None,
            ownership: None,
            journal_sequence: None,
            journal_generation: None,
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
        ExecFillRow::fee_correction_row(&a, seq, 20, None, None)
    }

    /// **The plain fields write the event's row**: a writer that holds the correction as text gets the very row
    /// the journal door's event gives.
    #[test]
    fn a_correction_from_its_plain_fields_is_the_events_row() {
        let plain = FeeCorrectionFields {
            corrects_trade_id: "a",
            order_client_id: "c1",
            venue: "binance",
            symbol: "BTCUSDT",
            ts: 10,
            price: 100.0,
            commission: 1.25,
            commission_asset: "BNB",
        };
        let stated = |row: ExecFillRow| ExecFillRow {
            venue_order_id: Some("v1".into()),
            account: Some("ALT".into()),
            ..row
        };
        let from_fields =
            ExecFillRow::fee_correction_row(plain, 7, 20, Some("v1".into()), Some("ALT".into()));
        assert_eq!(from_fields, stated(correction("a", 1.25, 7)));
    }

    /// **A position snapshot splits a series by JOURNAL order, never by time**: a row journaled after the snapshot is
    /// after it even when its venue time is earlier (a fill made while the tradehub was down), a row of a later
    /// journal is after it whatever its sequence, and a row from before the stream is before every snapshot.
    #[test]
    fn a_position_snapshot_splits_rows_by_journal_order() {
        let at = |generation, sequence, ts| ExecFillRow {
            journal_generation: Some(generation),
            journal_sequence: Some(sequence),
            ts,
            ..fill_row("t")
        };
        let snapshot = (2, 40);
        assert!(
            at(2, 41, 1).after_position_snapshot(snapshot),
            "journaled after, venue time before"
        );
        assert!(!at(2, 40, 9_999).after_position_snapshot(snapshot), "the snapshot's own sequence");
        assert!(!at(2, 39, 9_999).after_position_snapshot(snapshot), "journaled before");
        assert!(at(3, 1, 1).after_position_snapshot(snapshot), "a later journal");
        assert!(!at(1, 99, 9_999).after_position_snapshot(snapshot), "an earlier journal");
        assert!(!fill_row("t").after_position_snapshot(snapshot), "a row from before the stream");
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

    /// **A stream row supersedes a row from before the stream** (the materializer's, which carries no account):
    /// per `trade_id`, and per `(corrects_trade_id, correction_seq)` for a fee correction, never across two
    /// executions. A row from before the stream with no stream twin stays, and two stream rows both stay.
    #[test]
    fn a_stream_row_supersedes_a_row_from_before_the_stream() {
        let streamed = |row: ExecFillRow| ExecFillRow {
            account: Some("ALT".into()),
            ownership: Some(OWNERSHIP_OURS.into()),
            ..row
        };
        let old_a = fill_row("a");
        let new_a = ExecFillRow { commission: 0.2, ..streamed(fill_row("a")) };
        let old_b = fill_row("b");
        let foreign_c = ExecFillRow { ownership: Some(OWNERSHIP_FOREIGN.into()), ..fill_row("c") };
        let old_fix = correction("a", 1.00, 5);
        let new_fix = streamed(correction("a", 1.00, 5));
        let other_fix = correction("a", 1.25, 9);
        let rows = [
            old_a.clone(),
            old_b,
            new_a,
            foreign_c.clone(),
            foreign_c,
            old_fix,
            new_fix,
            other_fix,
            fill_row(""),
        ];
        assert_eq!(
            ExecFillRow::superseded(&rows),
            [true, false, false, false, false, true, false, false, false],
            "only the old row of an execution the stream stored is superseded"
        );

        let fills = ExecFillRow::fill_events(&rows);
        let ids: Vec<&str> = fills.iter().map(|f| f.trade_id.as_str()).collect();
        assert_eq!(ids, ["b", "a", "c", "c"], "`a` once, as the stream stated it");
        assert_eq!(fills[1].commission, 0.2 + 1.25, "the stream's fee, then the last correction");

        let before_the_stream = [old_a, fill_row("b")];
        assert_eq!(ExecFillRow::superseded(&before_the_stream), [false, false]);
        assert_eq!(
            ExecFillRow::fill_events(&before_the_stream),
            before_the_stream.iter().filter_map(ExecFillRow::to_fill_event).collect::<Vec<_>>(),
            "rows written before the stream read as they always did"
        );
    }

    /// **A fill restates no row and names its execution** (E21): a fee correction is no fill, and neither is a
    /// row with an empty id, which the fold skips; so the rows `is_fill` counts are the fills `fill_events` folds.
    #[test]
    fn a_fill_is_a_row_the_fold_folds_as_one() {
        let rows = [fill_row("a"), fill_row(""), correction("a", 0.25, 3), fill_row("b")];
        let fills: Vec<bool> = rows.iter().map(ExecFillRow::is_fill).collect();
        assert_eq!(fills, [true, false, false, true]);
        assert_eq!(
            fills.iter().filter(|f| **f).count(),
            ExecFillRow::fill_events(&rows).len(),
            "one rule for what is counted and what is folded"
        );
    }

    /// A row is foreign only when the stream said so; a row of ours and a row from before the stream are not.
    #[test]
    fn a_row_is_foreign_only_when_the_stream_said_so() {
        let owned = |ownership: Option<&str>| ExecFillRow {
            ownership: ownership.map(Into::into),
            ..fill_row("a")
        };
        assert!(owned(Some(OWNERSHIP_FOREIGN)).is_foreign());
        assert!(!owned(Some(OWNERSHIP_OURS)).is_foreign());
        assert!(!owned(None).is_foreign(), "a row from before the stream says nothing");
    }

    /// **The supersede rule applied per series, in the rows' order**: the stream's row of `a` supersedes the old
    /// row of `a` in ITS series only (the other symbol's `a` is another execution), and a one-series slice keeps
    /// exactly what `superseded` keeps.
    #[test]
    fn not_superseded_applies_the_rule_per_series_in_the_rows_order() {
        let streamed = |row: ExecFillRow| ExecFillRow {
            account: Some("ALT".into()),
            ownership: Some(OWNERSHIP_OURS.into()),
            ..row
        };
        let other_symbol = |row: ExecFillRow| ExecFillRow { symbol: "ETHUSDT".into(), ..row };
        let rows = [
            fill_row("a"),
            other_symbol(fill_row("a")),
            streamed(fill_row("a")),
            other_symbol(fill_row("b")),
            fill_row("c"),
        ];
        let kept: Vec<(&str, &str, bool)> = ExecFillRow::not_superseded(&rows)
            .into_iter()
            .map(|r| (r.symbol.as_str(), r.trade_id.as_str(), r.ownership.is_some()))
            .collect();
        assert_eq!(
            kept,
            [
                ("ETHUSDT", "a", false),
                ("BTCUSDT", "a", true),
                ("ETHUSDT", "b", false),
                ("BTCUSDT", "c", false)
            ],
            "only BTCUSDT's old `a` is gone, and the rest keep their order"
        );

        let one_series = [fill_row("a"), streamed(fill_row("a")), fill_row("b")];
        let by_rule: Vec<&ExecFillRow> = one_series
            .iter()
            .zip(ExecFillRow::superseded(&one_series))
            .filter(|(_, superseded)| !superseded)
            .map(|(row, _)| row)
            .collect();
        assert_eq!(ExecFillRow::not_superseded(&one_series), by_rule);
    }

    /// **What a build older than the columns does with a correction row**: it decodes (an unknown
    /// JSON key is ignored) and, the id being empty, it is the row that build already skips.
    #[test]
    fn a_correction_row_is_the_row_an_older_reader_skips() {
        let row = correction("a", 1.25, 3);
        assert_eq!((row.trade_id.as_str(), row.qty), ("", 0.0));
        assert_eq!(
            (row.ts, row.received_ms),
            (10, Some(20)),
            "its fill's time, and its own receipt"
        );
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
