//! The wire types: one resting price level, and the recorded L2 book event a feed emits.

use serde::{Deserialize, Serialize};

#[cfg(doc)]
use super::L2Book;

/// One resting price level as venues push it. `qty == 0` ⇒ remove the level.
///
/// ⚠ **The named fields ARE this type's reason to exist — it was a `pub type BookLevel = (f64, f64)`
/// alias until 2026-09-16.** Every venue bridge builds these out of a DIFFERENT venue's JSON array
/// (`vike_bridge_core::depth::parse_levels` — binance's, bybit's and okx's own copies until they were
/// hoisted there — deribit's `book_levels`, hyperliquid's `side_levels`, ibkr's market-feed pump),
/// and under the alias `(qty, price)` was exactly as valid
/// as `(price, qty)` at every one of them: an alias creates no type, so the pairing was held by
/// nothing but two lines of code agreeing about the order. A venue that serves `[size, price]`
/// would have filled the book with prices in the qty field — no compile error, no serde error, and
/// no test failure until something priced a fill with it.
///
/// **The wire is unchanged, and that is load-bearing.** [`BookUpdate`] rides the command journal,
/// so `#[serde(from/into)]` keeps a level a two-element array on disk exactly as the tuple was — a
/// journal written before this type existed still replays. The conversion costs nothing: `Copy`,
/// 16 bytes, the same layout the tuple had.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(from = "(f64, f64)", into = "(f64, f64)")]
pub struct BookLevel {
    pub price: f64,
    pub qty: f64,
}

impl BookLevel {
    /// The terse constructor, for the many sites that build a level from two values in hand.
    /// Positional like the tuple was — but it NAMES itself at the call site, and what it fills is
    /// named at the definition, so a reader who suspects an inversion has somewhere to look.
    pub const fn new(price: f64, qty: f64) -> Self {
        Self { price, qty }
    }
}

/// The serde bridge — and deliberately NOT a convenience for call sites. Reaching for `.into()` on
/// a bare pair puts the ordering hazard back exactly where this type removed it; construct with
/// [`BookLevel::new`] or the field names.
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

/// Kind of one recorded L2 book event — the disk/lane twin of what the live feed does
/// (book-recording plan, docs/superpowers/plans/2026-07-11-book-recording-replay.md).
/// The three §B stream-health kinds mirror `vike_data::StreamStatus`: they make the
/// gap-sentinel's disclosure part of the recorded stream, so replay integrity checking
/// is the SAME rule the live consumer saw, not a reconstruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BookUpdateKind {
    /// Incremental depth delta (upsert; qty 0 removes) — folds via [`L2Book::apply_delta`].
    Delta,
    /// Full-state anchor (venue snapshot frame OR a feed-synthesized periodic anchor) —
    /// folds via [`L2Book::apply_snapshot`]. Replay seeks start here.
    Snapshot,
    /// Stream-health marker: transport lost from `ts` — data until the next `Snapshot`
    /// is MISSING (net-hardening §B `StreamStatus::GapStart`). Carries no levels.
    GapStart,
    /// Stream-health marker: transport alive but data stopped flowing (§B `Stale`).
    Stale,
    /// Stream-health marker: stream recovered (§B `Live`); the re-seed `Snapshot` follows.
    LiveResume,
}

/// One recorded L2 book event: the RAW wire-shaped update (levels as the venue pushed
/// them), NOT the folded [`L2Book`] state — this is what makes delta-recording and exact
/// replay possible. `tick_size` rides on every event so replay rebuilds the book on the
/// SAME price grid even when the venue changes tick size mid-stream (point-in-time by
/// construction — load-bearing for Polymarket near 0/1). Status kinds carry empty levels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookUpdate {
    /// venue/frame epoch-ms (same clock as `QuoteTick::ts`)
    pub ts: i64,
    /// machine receive epoch-ms (dual-timestamp capture; 0 = not stamped)
    #[serde(default)]
    pub local_ts: i64,
    /// per-feed monotonic sequence; contiguity is the replay integrity check. 0 for status kinds.
    pub seq: u64,
    pub kind: BookUpdateKind,
    pub tick_size: f64,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    /// instrument id — empty for single-symbol paths (same convention as `QuoteTick`)
    #[serde(default)]
    pub symbol: String,
}

#[path = "wire_tests.rs"]
#[cfg(test)]
mod wire_tests;
