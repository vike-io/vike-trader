//! **The order `status` words of the binance family, each with ONE reading.** Every reader of a venue order row
//! (`recon`'s report, `confirm`'s by-order-client-id answer, `history`'s two replays, `crate::execution::rest::spot`'s `settle_replace`) parses the
//! word here and matches the result with no `_` arm, so a word cannot mean an end to one reader and a live order to
//! another, and a new variant fails to compile until each of them has said what it does with it. They used to spell
//! the words out, and two words read differently depending on who read them:
//!
//! * `EXPIRED_IN_MATCH` (self-trade prevention ended the order) was an end to the others and nothing to the replays,
//!   so an order that ended inside a reconnect gap stayed open on the node.
//! * `PENDING_CANCEL` was live to the others and an `OrderCanceled` to the replays, and the FSM has no
//!   `Canceled -> Filled` edge, so the wrap of a fill that beat the cancel was refused. It is not an end: the row after
//!   it says how the order ended.
//!
//! `tests/offline/order_words.rs` holds the word-by-reader table and folds the late fill through a real engine. An
//! `executionReport`'s or an `ORDER_TRADE_UPDATE`'s `x` is an EXECUTION type and an algo row's `algoStatus` is its own
//! list, so neither is read here, except that `crate::execution::rest::algo`'s `lookup_algo` hands its word to `confirm` on purpose:
//! `TRIGGERING`, `TRIGGERED` and `FINISHED` are [`OrderWord::Unknown`] there, which `confirm` reads as live.

/// One `status` word, as [`OrderWord::parse`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderWord {
    /// `NEW`: resting.
    New,
    /// `PENDING_NEW` (spot: an order-list leg the matching engine holds until its list activates), `NEW_INSURANCE` and
    /// `NEW_ADL` (fapi: the exchange's own liquidation orders, which this platform never placed). Live, not resting.
    LiveOther,
    /// `PARTIALLY_FILLED`: live, part of it done.
    PartiallyFilled,
    /// `FILLED`.
    Filled,
    /// `CANCELED`.
    Canceled,
    /// `PENDING_CANCEL`: live until a later row says otherwise.
    PendingCancel,
    /// `REJECTED`.
    Rejected,
    /// `EXPIRED` and `EXPIRED_IN_MATCH`: both are non-fill ends.
    Expired,
    /// An empty, absent or newer word. Live to every reader, the conservative direction: no reader ends an order on
    /// a word it does not know.
    Unknown,
}

impl OrderWord {
    /// The one reading of a raw `status` word.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "NEW" => OrderWord::New,
            "PENDING_NEW" | "NEW_INSURANCE" | "NEW_ADL" => OrderWord::LiveOther,
            "PARTIALLY_FILLED" => OrderWord::PartiallyFilled,
            "FILLED" => OrderWord::Filled,
            "CANCELED" => OrderWord::Canceled,
            "PENDING_CANCEL" => OrderWord::PendingCancel,
            "REJECTED" => OrderWord::Rejected,
            "EXPIRED" | "EXPIRED_IN_MATCH" => OrderWord::Expired,
            _ => OrderWord::Unknown,
        }
    }
}
