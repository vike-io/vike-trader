//! **The live tokens of the rolling families a datahub records**: the answer shape of
//! [`Request::LiveFamilyTokens`](crate::proto::Request::LiveFamilyTokens).
//!
//! A Polymarket up/down family (`btc-updown-5m`) is a new market every few minutes, so its outcome tokens rotate. The
//! datahub's recorder resolves each family its profile names to the family's live windows on every tick and reads each
//! new token's facts row; the verb answers what it holds of them, from memory, with no venue call on the request. A
//! client (the desktop's Polymarket cockpit) follows the rotation by asking again when the window it shows ends.

use serde::{Deserialize, Serialize};

/// One outcome token of a family's live window, as the datahub's recorder holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveFamilyToken {
    /// The family the recorder profile names (`btc-updown-5m`).
    pub family: String,
    /// The outcome token's id: the one name its book, an order and a fill carry (decision 0217).
    pub token: String,
    /// The outcome as the venue labels it (`Up`, `Down`).
    pub outcome: String,
    /// When the token's market ends (its window's end), epoch ms; `None` when the venue states no end.
    pub end_ms: Option<i64>,
}
