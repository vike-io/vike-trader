//! A level-2 order book and the reducer that folds venue depth updates into it.
//!
//! [`L2Book`] keeps price levels on a tick grid: prices are quantized to integer ticks
//! (`price / tick_size`, rounded half to even), so equality is exact and there is no float-key
//! hazard. Levels are ordered, giving O(log n) best bid/ask. An update with quantity `0` REMOVES
//! the level (the usual venue convention). A full snapshot clears the book and rebuilds it.
//!
//! # Feeding the book
//!
//! - [`L2Book::apply_snapshot`] replaces the whole book; [`L2Book::apply_delta`] upserts levels.
//! - [`BookLevel`] is one `(price, qty)` level and [`BookUpdate`] is one recorded, wire-shaped
//!   book event (snapshot, delta or a stream-health marker) that a feed can store and replay.
//! - Reads: [`L2Book::best_bid`], [`L2Book::best_ask`], [`L2Book::mid`], [`L2Book::spread`],
//!   [`L2Book::imbalance`], [`L2Book::top_n`], [`L2Book::bid_qty_at`] and friends.
//!
//! # Sequence numbers: the delta-apply law ([`L2Book::delta_decision`])
//!
//! Whether an incoming delta's sequence number folds, is dropped as stale, or means dropped frames
//! is ONE decision with two policies ([`SeqPolicy`]), so a live consumer and a replay never diverge:
//!
//! - [`SeqPolicy::Monotonic`] - for streams whose per-event sequence legitimately JUMPS between
//!   consecutive frames (for example Binance-style `u` spans, or any stream whose contiguity was
//!   already checked upstream). Any `seq > last_seq` applies; `seq <= last_seq` is stale, except
//!   the "venue supplied no seq" sentinel `0`, which always applies. There is no gap arm, because a
//!   jump is normal here. [`L2Book::apply_delta`] uses exactly this policy.
//! - [`SeqPolicy::Strict`] - for streams whose sequence increments by exactly 1 per applied event
//!   (for example Bybit's `orderbook.*` `u`, and a recorded [`BookUpdate`] chain with a feed-local
//!   contiguous seq). `last_seq + 1` applies; `== last_seq` is a stale duplicate; anything else, a
//!   forward jump (dropped frames) OR a regression (a venue restart that reset its counter), is
//!   [`DeltaDecision::Gap`], and the consumer must resync: re-seed from a fresh snapshot, or in a
//!   replay distrust the book until the next recorded snapshot re-anchors it.
//!
//! # Walk-the-book estimates
//!
//! [`L2Book::avg_px_for_quantity`], [`L2Book::quantity_for_price`], [`L2Book::simulate_fill`],
//! [`L2Book::can_fill`], [`L2Book::fill_ratio`] and [`book_taker_price`] are pure, read-only
//! pre-trade estimates against DISPLAYED liquidity. They are an upper bound on fill quality
//! (hidden orders, latency and queue dynamics only make a real fill worse), never an execution
//! guarantee. `side` is an `i32`: `+1` buys, consuming asks from low to high; `-1` sells,
//! consuming bids from high to low; `0` fills nothing. Degenerate inputs (NaN or non-positive
//! quantity, crossed books) degrade gracefully and never panic.
//
// Engineering notes (not rendered): the in-core reducer (R8 HFT track): venue tasks decode depth
// deltas, this folds them into a top-of-book the strategy reads via `on_order_book`. The reducer is
// allocation-light on the steady state (map slots are reused). All arithmetic is {+,−,×,÷,cmp} —
// bit-parity eligible. Levels are a dense `BTreeMap<i64, f64>` keyed by tick.
//
// Delta-apply law, per stream: `Monotonic` is binance-grammar `u` spans and any venue where the
// bridge already proved contiguity upstream; gap detection (if any) is then the bridge's own
// upstream rule (binance `U`/`u`, okx `prevSeqId`). `Strict` is bybit `orderbook.*` `u` and the
// recorded `BookUpdate` chain every feed emits (the replay integrity rule in vike-backtest's
// `apply_book_event`).
//
// Walk-the-book helpers: intended consumers are RiskGate veto thresholds and the DOM cost-to-fill
// display (that wiring is a follow-up, not this module). Sums are naive best-first folds; `side`
// follows the codebase i32 convention.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// Engineering note: the serde derives came with journal-replay Task 1: the L2/tick lane's
// `BookUpdate` carries this through `Ingest`, so it must round-trip through the command journal
// like every other lane payload — `BTreeMap<i64, f64>` round-trips through serde_json via its
// stringified-integer-key map form.
/// A depth-limited level-2 order book keyed by integer ticks.
///
/// Build one with [`L2Book::new`], seed it with [`L2Book::apply_snapshot`], keep it current with
/// [`L2Book::apply_delta`], and read it through [`L2Book::best_bid`], [`L2Book::top_n`] and the
/// other accessors. It derives `Serialize`/`Deserialize` (levels are stored internally as
/// tick-index maps, which serde_json writes with stringified integer keys).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2Book {
    /// The price grid: every price is rounded to a multiple of this. [`L2Book::new`] replaces a
    /// non-positive value with `1.0`.
    pub tick_size: f64,
    /// tick → qty, ascending; the best bid is the LAST key
    bids: BTreeMap<i64, f64>,
    /// tick → qty, ascending; the best ask is the FIRST key
    asks: BTreeMap<i64, f64>,
    /// The sequence number of the last snapshot or delta folded in (`0` for a fresh book).
    /// [`L2Book::delta_decision`] compares an incoming delta against it under a [`SeqPolicy`].
    pub last_seq: u64,
}

impl L2Book {
    /// An empty book on a `tick_size` price grid (a non-positive `tick_size` falls back to `1.0`).
    pub fn new(tick_size: f64) -> Self {
        L2Book {
            tick_size: if tick_size > 0.0 { tick_size } else { 1.0 },
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_seq: 0,
        }
    }

    /// Quantize a price to its tick index (round-half-to-even for a stable mapping).
    /// Free function over the scalar so it never co-borrows `self` with the level maps.
    fn tick_at(tick_size: f64, price: f64) -> i64 {
        (price / tick_size).round_ties_even() as i64
    }

    fn price_of(&self, tick: i64) -> f64 {
        tick as f64 * self.tick_size
    }

    fn apply_side(side: &mut BTreeMap<i64, f64>, tick: i64, qty: f64) {
        if qty == 0.0 {
            side.remove(&tick);
        } else {
            side.insert(tick, qty);
        }
    }

    /// Replace the whole book with a venue depth SNAPSHOT: both sides are cleared first, then the
    /// given levels are inserted (a level with `qty == 0` is skipped) and `last_seq` is set to
    /// `seq`.
    pub fn apply_snapshot(&mut self, seq: u64, bids: &[BookLevel], asks: &[BookLevel]) {
        self.bids.clear();
        self.asks.clear();
        let ts = self.tick_size;
        for &BookLevel { price: px, qty } in bids {
            Self::apply_side(&mut self.bids, Self::tick_at(ts, px), qty);
        }
        for &BookLevel { price: px, qty } in asks {
            Self::apply_side(&mut self.asks, Self::tick_at(ts, px), qty);
        }
        self.last_seq = seq;
    }

    /// Apply an incremental depth delta: each level is an upsert, and `qty == 0` removes the
    /// level. The sequence number must not regress: a stale or duplicate `seq` is dropped and
    /// `false` is returned (`true` means the delta was applied and `last_seq` is now `seq`).
    ///
    /// This uses [`SeqPolicy::Monotonic`] (see [`L2Book::delta_decision`]). A consumer of a strict,
    /// contiguous stream should ask [`L2Book::delta_decision`] with [`SeqPolicy::Strict`] first,
    /// because only that can report a [`DeltaDecision::Gap`], and fold only on
    /// [`DeltaDecision::Apply`].
    pub fn apply_delta(&mut self, seq: u64, bids: &[BookLevel], asks: &[BookLevel]) -> bool {
        if self.delta_decision(seq, SeqPolicy::Monotonic) != DeltaDecision::Apply {
            return false; // stale/replayed — the venue task handles resync
        }
        let ts = self.tick_size;
        for &BookLevel { price: px, qty } in bids {
            Self::apply_side(&mut self.bids, Self::tick_at(ts, px), qty);
        }
        for &BookLevel { price: px, qty } in asks {
            Self::apply_side(&mut self.asks, Self::tick_at(ts, px), qty);
        }
        self.last_seq = seq;
        true
    }

    /// The best bid (highest bid price) and its resting quantity, or `None` when the bid side is
    /// empty.
    pub fn best_bid(&self) -> Option<BookLevel> {
        self.bids.iter().next_back().map(|(&t, &q)| BookLevel::new(self.price_of(t), q))
    }

    /// The best ask (lowest ask price) and its resting quantity, or `None` when the ask side is
    /// empty.
    pub fn best_ask(&self) -> Option<BookLevel> {
        self.asks.iter().next().map(|(&t, &q)| BookLevel::new(self.price_of(t), q))
    }

    /// Mid price `(best_bid + best_ask) / 2`, or `None` when either side is empty.
    pub fn mid(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some((b.price + a.price) / 2.0),
            _ => None,
        }
    }

    /// Spread `best_ask - best_bid` in price units (negative if the book is crossed), or `None`
    /// when either side is empty.
    pub fn spread(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some(a.price - b.price),
            _ => None,
        }
    }

    /// Top-of-book size imbalance in [-1, 1]: (bidQ − askQ) / (bidQ + askQ). None if
    /// either side is empty or both sizes are zero.
    pub fn imbalance(&self) -> Option<f64> {
        let bq = self.best_bid()?.qty;
        let aq = self.best_ask()?.qty;
        let denom = bq + aq;
        if denom == 0.0 { None } else { Some((bq - aq) / denom) }
    }

    /// Number of distinct price levels on the bid side.
    pub fn bid_levels(&self) -> usize {
        self.bids.len()
    }

    /// Number of distinct price levels on the ask side.
    pub fn ask_levels(&self) -> usize {
        self.asks.len()
    }

    // Engineering note: an additive read for queue-position estimation (vike-backtest `queue_model`).
    /// Resting qty at exactly `price` on the BID side (`0.0` when the level is absent).
    /// Price is quantized to the book's tick grid, same as every apply path. Useful for
    /// queue-position estimation.
    pub fn bid_qty_at(&self, price: f64) -> f64 {
        self.bids.get(&Self::tick_at(self.tick_size, price)).copied().unwrap_or(0.0)
    }

    /// Resting qty at exactly `price` on the ASK side (`0.0` when the level is absent). Price is
    /// quantized to the book's tick grid, like [`L2Book::bid_qty_at`].
    pub fn ask_qty_at(&self, price: f64) -> f64 {
        self.asks.get(&Self::tick_at(self.tick_size, price)).copied().unwrap_or(0.0)
    }

    /// The `n` best levels on each side, returned as `(bids, asks)`: bids from highest to lowest
    /// price, asks from lowest to highest. A side with fewer than `n` levels returns all it has.
    pub fn top_n(&self, n: usize) -> (Vec<BookLevel>, Vec<BookLevel>) {
        let bids = self
            .bids
            .iter()
            .rev()
            .take(n)
            .map(|(&t, &q)| BookLevel::new(self.price_of(t), q))
            .collect();
        let asks =
            self.asks.iter().take(n).map(|(&t, &q)| BookLevel::new(self.price_of(t), q)).collect();
        (bids, asks)
    }
}

mod seq;
mod walk;
mod wire;

pub use seq::{DeltaDecision, SeqPolicy};
pub use walk::{FillSim, book_taker_price};
pub use wire::{BookLevel, BookUpdate, BookUpdateKind};

#[path = "orderbook/core_tests.rs"]
#[cfg(test)]
mod core_tests;
