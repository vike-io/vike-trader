//! Per-token pump vocabulary: the subscription modes, one token's book state and its socket seat.

use std::sync::Arc;
use std::time::Duration;

use vike_bridge_core::stream_health::StreamHealth;
use vike_marketdata::L2Book;

use super::{FRESHNESS_THRESHOLD_BOOK, FRESHNESS_THRESHOLD_TRADES};

/// What one `Feeds` subscription pumps to the sink — see the module doc for the exact emission
/// rules per mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpMode {
    /// `subscribe_book`: book snapshots/deltas → `sink.book()` + derived-quote `sink.quote()`.
    Book,
    /// `subscribe_quotes`: same book bookkeeping as `Book`, but only the derived `sink.quote()`.
    Quotes,
    /// `subscribe_trades`: `last_trade` frames only → `sink.trade()`.
    Trades,
}

impl PumpMode {
    /// The `stream` key used in this pump's `LiveDataSink::stream_status` disclosures
    /// (net-hardening §B) — a stable per-mode label so a consumer can tell a dead quote stream from
    /// a dead trade stream on the same token.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            PumpMode::Book => "book",
            PumpMode::Quotes => "quotes",
            PumpMode::Trades => "trades",
        }
    }

    /// Per-mode data-freshness threshold (net-hardening §B). Book/Quotes 300s, Trades 1800s —
    /// measured floors from the 2026-07-11 Dublin sample, not placeholders (see the constants' doc).
    pub(super) fn freshness_threshold(self) -> Duration {
        match self {
            PumpMode::Book | PumpMode::Quotes => FRESHNESS_THRESHOLD_BOOK,
            PumpMode::Trades => FRESHNESS_THRESHOLD_TRADES,
        }
    }
}

/// One token's book-maintenance state, carried across reconnects within a single subscription's
/// lifetime. `pub book`/`pub last_top` so the scripted test seam can assert quantized tops
/// directly; `seq` stays private — it is a pure bookkeeping counter with no test-visible meaning
/// beyond what `book`'s own accessors already expose.
pub struct TokenState {
    /// The token's standing book, behind an [`Arc`] so `sink.book()` ships a refcount bump instead
    /// of deep-cloning both `BTreeMap`s on EVERY applied delta (perf audit finding #1 — see
    /// `vike_exec::BookUpdate`). Mutated through [`Arc::make_mut`]: O(1) while this pump holds the
    /// only handle, copy-on-write (on the PUMP thread, never the single-writer core) while a
    /// consumer still holds the last one emitted. Read-only uses (`best_bid`, `top_n`, `tick_size`,
    /// the scripted test's assertions) go through `Deref` unchanged.
    pub book: Arc<L2Book>,
    pub(super) seq: u64,
    pub last_top: Option<(f64, f64, f64, f64)>,
    /// Applied deltas since the last emitted anchor (Book mode) — drives the synthetic-anchor
    /// cadence ([`ANCHOR_EVERY_DELTAS`]); reset to 0 by any snapshot (venue or synthetic).
    pub(super) deltas_since_anchor: u64,
}

impl TokenState {
    pub fn new(tick_size: f64) -> Self {
        TokenState {
            book: Arc::new(L2Book::new(tick_size)),
            seq: 0,
            last_top: None,
            deltas_since_anchor: 0,
        }
    }
}

/// One token's seat on a shard's socket: everything the frame protocol mutates for that token —
/// its book fold ([`TokenState`], which owns the feed-local `seq` and `last_top`) and its OWN
/// §B [`StreamHealth`] (so a per-token `Stale` disclosure, and a `Gap`/`Live` pair per token, ride
/// the shared socket without collapsing into one socket-wide signal).
///
/// Slots are what makes batching per-token by construction: a frame is routed to the slot whose
/// `token_id` equals the frame's own `asset_id`, so token A's frames can never touch token B's book
/// or bump its `seq`.
pub struct TokenSlot {
    pub token_id: String,
    pub state: TokenState,
    pub health: StreamHealth,
}

impl TokenSlot {
    /// A fresh slot: an empty book quantized at `tick_size` and a fresh `StreamHealth` armed with
    /// `freshness_threshold_ms` (the mode's [`PumpMode::freshness_threshold`] in production).
    pub fn new(token_id: impl Into<String>, tick_size: f64, freshness_threshold_ms: i64) -> Self {
        TokenSlot {
            token_id: token_id.into(),
            state: TokenState::new(tick_size),
            health: StreamHealth::new(freshness_threshold_ms),
        }
    }
}
