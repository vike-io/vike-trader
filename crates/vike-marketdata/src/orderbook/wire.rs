//! The wire types: one resting price level ([`BookLevel`]), and the recorded L2 book event a feed
//! emits ([`BookUpdate`], with its [`BookUpdateKind`]).

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use super::L2Book;

// Engineering notes (not rendered): ⚠ **The named fields ARE this type's reason to exist — it was a
// `pub type BookLevel = (f64, f64)` alias until 2026-09-16.** Every venue bridge builds these out
// of a DIFFERENT venue's JSON array (`vike_bridge_core::depth::parse_levels` — binance's, bybit's
// and okx's own copies until they were hoisted there — deribit's `book_levels`, hyperliquid's
// `side_levels`, ibkr's market-feed pump), and under the alias `(qty, price)` was exactly as valid
// as `(price, qty)` at every one of them: an alias creates no type, so the pairing was held by
// nothing but two lines of code agreeing about the order. A venue that serves `[size, price]`
// would have filled the book with prices in the qty field — no compile error, no serde error, and
// no test failure until something priced a fill with it.
//
// **The wire is unchanged, and that is load-bearing.** [`BookUpdate`] rides the command journal,
// so `#[serde(from/into)]` keeps a level a two-element array on disk exactly as the tuple was — a
// journal written before this type existed still replays. The conversion costs nothing: `Copy`,
// 16 bytes, the same layout the tuple had.
/// One resting price level as venues push it. `qty == 0` means "remove the level".
///
/// The fields are named so price and quantity cannot be swapped by accident. It serialises as a
/// two-element `[price, qty]` array, the shape most venues use on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(from = "(f64, f64)", into = "(f64, f64)")]
pub struct BookLevel {
    /// Level price.
    pub price: f64,
    /// Resting quantity at that price (`0.0` in an update removes the level).
    pub qty: f64,
}

impl BookLevel {
    /// Build a level from a price and a quantity, in that order.
    pub const fn new(price: f64, qty: f64) -> Self {
        Self { price, qty }
    }
}

// Engineering note: the serde bridge — and deliberately NOT a convenience for call sites. Reaching
// for `.into()` on a bare pair puts the ordering hazard back exactly where this type removed it;
// construct with [`BookLevel::new`] or the field names.
impl From<(f64, f64)> for BookLevel {
    fn from((price, qty): (f64, f64)) -> Self {
        Self { price, qty }
    }
}

impl From<BookLevel> for (f64, f64) {
    fn from(l: BookLevel) -> Self {
        (l.price, l.qty)
    }
}

// Engineering notes (not rendered): the disk/lane twin of what the live feed does (book-recording
// plan, docs/superpowers/plans/2026-07-11-book-recording-replay.md). The three §B stream-health
// kinds mirror `vike_model::StreamStatus`: they make the gap-sentinel's disclosure part of the
// recorded stream, so replay integrity checking is the SAME rule the live consumer saw, not a
// reconstruction. Net-hardening §B names: `GapStart`, `Stale`, `Live`.
/// Kind of one recorded L2 book event: a depth delta, a full snapshot, or one of three
/// stream-health markers that make a data gap visible in the recording itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BookUpdateKind {
    /// Incremental depth delta (upsert; qty 0 removes) — folds via [`L2Book::apply_delta`].
    Delta,
    /// Full-state anchor (a venue snapshot frame or a periodic anchor synthesized by the feed) —
    /// folds via [`L2Book::apply_snapshot`]. A replay can start from any snapshot.
    Snapshot,
    /// Stream-health marker: the transport was lost from `ts`, so data until the next `Snapshot`
    /// is MISSING. Carries no levels.
    GapStart,
    /// Stream-health marker: the transport is alive but data stopped flowing.
    Stale,
    /// Stream-health marker: the stream recovered; the re-seeding `Snapshot` follows.
    LiveResume,
}

// Engineering note: `ts` is the same clock as `QuoteTick::ts`; `symbol` follows the same
// convention as `QuoteTick`.
/// One recorded L2 book event: the RAW wire-shaped update (levels as the venue pushed them), NOT
/// the folded [`L2Book`] state. Keeping the raw event is what makes delta-recording and exact
/// replay possible. `tick_size` rides on every event so a replay rebuilds the book on the SAME
/// price grid even when the venue changes tick size mid-stream (for example prediction-market
/// prices near 0 or 1). Status kinds carry empty levels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookUpdate {
    /// Venue (frame) time in epoch milliseconds (UTC).
    pub ts: i64,
    /// Local receive time in epoch milliseconds (UTC); `0` means "not stamped".
    #[serde(default)]
    pub local_ts: i64,
    /// Per-feed monotonic sequence number; contiguity is the replay integrity check (see
    /// [`SeqPolicy::Strict`](crate::SeqPolicy::Strict)). `0` for status kinds.
    pub seq: u64,
    /// What this event is: snapshot, delta or a stream-health marker.
    pub kind: BookUpdateKind,
    /// The price grid in force for this event; see [`L2Book::new`].
    pub tick_size: f64,
    /// Bid levels, as the venue sent them (empty for status kinds).
    pub bids: Vec<BookLevel>,
    /// Ask levels, as the venue sent them (empty for status kinds).
    pub asks: Vec<BookLevel>,
    /// instrument id — empty for single-symbol paths (same convention as `QuoteTick`)
    #[serde(default)]
    pub symbol: String,
}

#[path = "wire_tests.rs"]
#[cfg(test)]
mod wire_tests;
