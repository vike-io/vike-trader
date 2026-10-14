//! `DataDelay` — how far behind the market a live stream's data is, as the VENUE says so for the data
//! it actually sent (`docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`, Decision
//! item 2, "Data rides the session at the entitlement it has").
//!
//! # What it is, and what it is not
//!
//! It is a LABEL on a stream, read off the data as it arrives (an IBKR socket feed reads it off
//! each tick's type, `DelayedBid` against `Bid`; a Client Portal feed off each row's availability
//! marker). It is never inferred from what was REQUESTED: an account asking for delayed data is
//! served real-time for a class it is entitled to, so only the data can say which it got.
//!
//! It is not stream HEALTH. [`crate::StreamStatus`] and [`crate::FeedStatus`] describe transitions
//! (a gap opened, the data went stale, the stream recovered); a delayed stream can be perfectly
//! healthy, and a real-time one can be gapped. The two travel separately and neither implies the
//! other.
//!
//! # The label is information, never a gate
//!
//! The owner's ruling (decision 0140, "An order MAY be priced off delayed data"): nothing refuses or
//! holds an order, or blocks arming a strategy, because a stream is [`DataDelay::Delayed`] or
//! [`DataDelay::Frozen`]. The label travels so that a 15-minute-old stock quote is never mistaken
//! for a live one; a consumer SHOWS and RECORDS it and decides nothing on it.
//!
//! # The serde shape
//!
//! The default externally tagged form: `"RealTime"`, `"Frozen"` and `{"Delayed":{"lag_ms":…}}`.
//! `crates/vike-model/src/data_delay_tests.rs` pins the bytes, because anything that ships the label
//! between processes ships these.

use serde::{Deserialize, Serialize};

/// How far behind the market one live stream's data is. `Copy` and allocation-free, so a feed can
/// carry it per tick and a consumer compare it per message at no cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataDelay {
    /// The venue served this stream at its real-time entitlement.
    RealTime,
    /// The venue served this stream DELAYED (no real-time entitlement for its class).
    Delayed {
        /// The lag the venue STATED, in milliseconds, when it states one. `None` when it does not
        /// (an IBKR socket `Delayed*` tick says "delayed" and no figure; IB's documentation puts
        /// it at 15-20 minutes depending on the exchange, which is not something to write here as
        /// if measured).
        lag_ms: Option<u64>,
    },
    /// The venue served the LAST value it had, not a moving market (IBKR's "frozen" and
    /// "delayed-frozen" market-data types, typically outside trading hours).
    Frozen,
}

impl DataDelay {
    /// A delayed label with no stated lag — what a venue that marks "delayed" without a figure
    /// sends.
    pub const DELAYED: DataDelay = DataDelay::Delayed { lag_ms: None };

    /// Whether this stream is at the real-time entitlement.
    pub const fn is_real_time(self) -> bool {
        matches!(self, DataDelay::RealTime)
    }

    /// The one short word for a log line or a GUI badge: `"real-time"`, `"delayed"` or `"frozen"`.
    /// The stated lag, when there is one, is the caller's to add.
    pub const fn word(self) -> &'static str {
        match self {
            DataDelay::RealTime => "real-time",
            DataDelay::Delayed { .. } => "delayed",
            DataDelay::Frozen => "frozen",
        }
    }

    /// The inverse of [`Self::word`], for a wire that carries the label as its word (the tradehub
    /// order row: its wire crate mirrors no vike type, so it spells the label). A stated lag does
    /// not survive the word, so `"delayed"` reads back as [`Self::DELAYED`]. An unknown word is
    /// `None`: a newer node's label this reader cannot name is shown as no label, never guessed.
    pub fn from_word(word: &str) -> Option<DataDelay> {
        match word {
            "real-time" => Some(DataDelay::RealTime),
            "delayed" => Some(DataDelay::DELAYED),
            "frozen" => Some(DataDelay::Frozen),
            _ => None,
        }
    }
}

#[path = "data_delay_tests.rs"]
#[cfg(test)]
mod data_delay_tests;
