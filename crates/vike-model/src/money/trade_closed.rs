//! [`TradeClosedRow`], one row of a run's closed-trade ledger: the part of one entry one fill closed;
//! and [`TradeClosedOpenLot`], one lot that ledger still holds open, as it is persisted.
//!
//! The ledger that books it is `vike_trade_ledger::TradeClosedLedger` (one lot per entry
//! fill, first in first out, each row net of its share of both commissions). The row lives here, in
//! the domain vocabulary, so the order seam can hand a strategy its own closed trades
//! (`crate::OrderBroker::closed_trades`) as the engine booked them, with no second booking.
//!
//! ⚠ **Its serde shape is a JOURNAL format**: the live core's `Snapshot` (`vike_journal::JournalSnapshotClosedTrades::rows`)
//! and the closed-trade archive (`vike_journal::TradeClosedArchive`, a file never compacted) store this row as it
//! is, by these field names, and an empty `tag` writes no key. A field renamed here breaks the reading of
//! every journal and archive already written; a field added must be `#[serde(default)]`.

/// One closed trade: the part of one lot one fill closed. Money is the account's.
///
/// ⚠ `#[repr(C)]` in this declaration order: a strategy file reads the host's rows in place, under
/// `crates/vike-rust-strategy/src/abi.rs`'s `layout`. Never reorder the fields.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[repr(C)]
pub struct TradeClosedRow {
    pub symbol: String,
    /// The name the lot was booked under (the entry's), empty when it has none (and then no key is written).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tag: String,
    /// The lot's side: `+1` long, `-1` short.
    pub side: i32,
    pub entry_timestamp: i64,
    pub entry_bar: usize,
    pub entry_price: f64,
    pub exit_timestamp: i64,
    pub exit_bar: usize,
    pub exit_price: f64,
    pub qty: f64,
    /// Price P&L, before commission.
    pub pnl: f64,
    /// The entry share plus the exit share.
    pub commission: f64,
    /// `pnl - entry share - exit share`, in that order.
    pub net: f64,
}

impl TradeClosedRow {
    /// Bars from entry to exit, as the reference's list counts them.
    pub fn bars(&self) -> usize {
        self.exit_bar.saturating_sub(self.entry_bar)
    }
}

/// **One OPEN lot of a closed-trade ledger, as it is persisted**: what the ledger's next closes read
/// of it, the entry, the quantity left, the entry commission per unit it still carries and the
/// contract value its entry was booked at. The live ledger's own lot is
/// `vike_trade_ledger::OpenLot`, which keeps its excursions beside these: its `state` writes this, and
/// its `resumed` rebuilds a lot from these fields.
///
/// ⚠ **Its serde shape is a JOURNAL format**, the same as [`TradeClosedRow`]'s: the live core's
/// `Snapshot` stores it in `vike_journal::JournalSnapshotClosedTrades::lots`, by these field names, in
/// this order, and an empty `tag` writes no key. A field renamed here breaks the reading of every
/// journal already written; a field added must be `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TradeClosedOpenLot {
    pub symbol: String,
    /// The name the lot was booked under (the entry's), empty when it has none (and then no key is written).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tag: String,
    /// `+1` long, `-1` short.
    pub side: i32,
    pub entry_timestamp: i64,
    /// The caller's bar index of the entry fill.
    pub entry_bar: usize,
    pub price: f64,
    /// The quantity still open.
    pub qty: f64,
    /// The entry commission per unit the lot still carries, in the account's money.
    pub fee_per_unit: f64,
    /// What one unit of the lot's contract is worth in the account's money, as its entry was booked.
    pub value: crate::ContractValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lot(tag: &str) -> TradeClosedOpenLot {
        TradeClosedOpenLot {
            symbol: "BTCUSDT".to_string(),
            tag: tag.to_string(),
            side: -1,
            entry_timestamp: 1_700_000_000_000,
            entry_bar: 7,
            price: 100.5,
            qty: 0.25,
            fee_per_unit: 0.01,
            value: crate::ContractValue::default(),
        }
    }

    /// The journal's bytes: the nine keys in declaration order, an empty tag writing no key, and the
    /// lot reading back as it was written.
    #[test]
    fn an_open_lot_writes_the_journal_keys_and_reads_back() {
        let value = serde_json::to_string(&crate::ContractValue::default()).expect("serializes");
        let tagged = serde_json::to_string(&lot("breakout")).expect("serializes");
        assert_eq!(
            tagged,
            format!(
                r#"{{"symbol":"BTCUSDT","tag":"breakout","side":-1,"entry_timestamp":1700000000000,"entry_bar":7,"price":100.5,"qty":0.25,"fee_per_unit":0.01,"value":{value}}}"#
            )
        );
        let untagged = serde_json::to_string(&lot("")).expect("serializes");
        assert!(!untagged.contains("\"tag\""), "{untagged}");
        for l in [lot("breakout"), lot("")] {
            let back: TradeClosedOpenLot =
                serde_json::from_str(&serde_json::to_string(&l).expect("serializes"))
                    .expect("reads back");
            assert_eq!(back, l);
        }
    }
}
