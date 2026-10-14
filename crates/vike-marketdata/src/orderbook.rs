//! L2 order book — the in-core reducer (R8 HFT track): venue tasks decode depth deltas,
//! this folds them into a top-of-book the strategy reads via `on_order_book`.
//!
//! Design: prices are quantized to integer ticks (`price / tick` rounded) so levels are a
//! dense `BTreeMap<i64, f64>` keyed by tick — exact equality, no float-key hazard, ordered
//! for O(log n) best-bid/ask. A delta with qty == 0 REMOVES the level (venue convention).
//! The reducer is allocation-light on the steady state (map slots are reused); a full
//! snapshot clears and rebuilds. All arithmetic is {+,−,×,÷,cmp} — bit-parity eligible.
//!
//! ## The one delta-apply law ([`L2Book::delta_decision`])
//!
//! Whether an incoming delta seq folds, is dropped as stale, or means dropped frames is ONE
//! decision with two policies ([`SeqPolicy`]), so live consumers and replay can never diverge:
//!
//! - [`SeqPolicy::Monotonic`] — venue streams whose per-event seq legitimately JUMPS between
//!   consecutive frames (binance-grammar `u` spans; any venue where the bridge already proved
//!   contiguity upstream). Any `seq > last_seq` applies; `seq <= last_seq` (except the
//!   "venue supplied no seq" sentinel `0`, which always applies) is stale. No gap arm — a jump
//!   is normal here, so gap detection (if any) is the bridge's own upstream rule (binance
//!   `U`/`u`, okx `prevSeqId`). [`L2Book::apply_delta`] consults exactly this policy.
//! - [`SeqPolicy::Strict`] — streams whose seq increments by exactly 1 per applied event:
//!   bybit `orderbook.*` `u`, and the recorded [`BookUpdate`] chain every feed emits
//!   (feed-local contiguous seq — the replay integrity rule in vike-backtest's
//!   `apply_book_event`). `last_seq + 1` applies; `== last_seq` is a stale duplicate; anything
//!   else — a forward jump (dropped frames) OR a regression (a venue restart that reset the
//!   counter) — is [`DeltaDecision::Gap`], and the consumer must resync (live: re-seed a fresh
//!   snapshot; replay: distrust the book until the next recorded Snapshot re-anchors).
//!
//! Walk-the-book helpers ([`L2Book::avg_px_for_quantity`], [`L2Book::quantity_for_price`],
//! [`L2Book::simulate_fill`], [`L2Book::can_fill`], [`L2Book::fill_ratio`]) are pure
//! read-only pre-trade impact estimates against DISPLAYED liquidity — an upper bound on
//! fill quality (hidden flow, latency, and queue dynamics only make the real fill worse),
//! never an execution guarantee. Intended consumers: RiskGate veto thresholds and the DOM
//! cost-to-fill display (that wiring is a follow-up, not this module). Sums are naive
//! best-first folds; `side` follows the codebase i32 convention (+1 buy consumes asks
//! low→high, −1 sell consumes bids high→low; 0 fills nothing). Degenerate inputs
//! (NaN/non-positive qty, crossed books) degrade gracefully — no panics.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A depth-limited L2 book keyed by integer ticks. Serde derives (journal-replay Task 1): the
/// L2/tick lane's `BookUpdate` carries this through `Ingest`, so it must round-trip through the
/// command journal like every other lane payload — `BTreeMap<i64, f64>` round-trips through
/// serde_json via its stringified-integer-key map form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2Book {
    pub tick_size: f64,
    /// tick → qty, ascending; the best bid is the LAST key
    bids: BTreeMap<i64, f64>,
    /// tick → qty, ascending; the best ask is the FIRST key
    asks: BTreeMap<i64, f64>,
    /// monotonically increasing venue sequence (gap detection = [`L2Book::delta_decision`],
    /// consulted by the venue task / replay under its stream's [`SeqPolicy`])
    pub last_seq: u64,
}

impl L2Book {
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

    /// Replace the whole book (venue depth SNAPSHOT). Clears both sides first.
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

    /// Apply an incremental depth delta (upsert; qty 0 removes). Seq must not regress —
    /// a stale/duplicate seq is dropped (returns false). Consults [`L2Book::delta_decision`]
    /// under [`SeqPolicy::Monotonic`]; a strict-stream consumer asks `delta_decision` itself
    /// first (for the [`DeltaDecision::Gap`] arm this method cannot see) and only then folds.
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

    /// (price, qty) of the best bid (highest bid tick).
    pub fn best_bid(&self) -> Option<BookLevel> {
        self.bids.iter().next_back().map(|(&t, &q)| BookLevel::new(self.price_of(t), q))
    }

    /// (price, qty) of the best ask (lowest ask tick).
    pub fn best_ask(&self) -> Option<BookLevel> {
        self.asks.iter().next().map(|(&t, &q)| BookLevel::new(self.price_of(t), q))
    }

    pub fn mid(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(b), Some(a)) => Some((b.price + a.price) / 2.0),
            _ => None,
        }
    }

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

    pub fn bid_levels(&self) -> usize {
        self.bids.len()
    }

    pub fn ask_levels(&self) -> usize {
        self.asks.len()
    }

    /// Resting qty at exactly `price` on the BID side (`0.0` when the level is absent).
    /// Price is quantized to the book's tick grid, same as every apply path. Additive read
    /// for queue-position estimation (vike-backtest `queue_model`).
    pub fn bid_qty_at(&self, price: f64) -> f64 {
        self.bids.get(&Self::tick_at(self.tick_size, price)).copied().unwrap_or(0.0)
    }

    /// Resting qty at exactly `price` on the ASK side (`0.0` when the level is absent).
    pub fn ask_qty_at(&self, price: f64) -> f64 {
        self.asks.get(&Self::tick_at(self.tick_size, price)).copied().unwrap_or(0.0)
    }

    /// The N best (price, qty) on each side (bids high→low, asks low→high) — the render/
    /// strategy view.
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
