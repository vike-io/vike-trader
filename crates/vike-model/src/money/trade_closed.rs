//! [`TradeClosedRow`], one row of a run's closed-trade ledger: the part of one entry one fill closed.
//!
//! The ledger that books it is `vike_analytics::closed_trades::TradeClosedLedger` (one lot per entry
//! fill, first in first out, each row net of its share of both commissions). The row lives here, in
//! the domain vocabulary, so the order seam can hand a strategy its own closed trades
//! (`crate::OrderBroker::closed_trades`) as the engine booked them, with no second booking.
//!
//! ⚠ **Its serde shape is a JOURNAL format**: the live core's `Snapshot` (`vike_journal::JournalSnapshotClosedTrades::rows`)
//! and the closed-trade archive (`vike_journal::TradeClosedArchive`, a file never compacted) store this row as it
//! is, by these field names, and an empty `tag` writes no key. A field renamed here breaks the reading of
//! every journal and archive already written; a field added must be `#[serde(default)]`.

/// One closed trade: the part of one lot one fill closed. Money is the account's.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
