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

/// One resting price level as venues push it: (price, qty). qty == 0 ⇒ remove.
pub type Level = (f64, f64);

/// The contiguity a consumer expects of an incoming delta's seq — see the module doc's
/// "one delta-apply law" section for which stream uses which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqPolicy {
    /// seq increments by exactly 1 per applied event (bybit `u`; the recorded `BookUpdate`
    /// chain). Anything but `last_seq + 1` / `last_seq` is a gap.
    Strict,
    /// seq only needs to increase (binance-grammar `u` spans) — jumps are normal, never a gap.
    Monotonic,
}

/// What [`L2Book::delta_decision`] says to do with an incoming delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaDecision {
    /// In sequence — fold it ([`L2Book::apply_delta`] will accept it).
    Apply,
    /// Already reflected (duplicate/replayed) — drop it, the book stays trustworthy.
    Stale,
    /// Frames were dropped (forward jump) or the venue restarted its counter (regression) —
    /// the book can no longer be trusted; the consumer must resync from a fresh snapshot.
    /// Only the [`SeqPolicy::Strict`] policy can return this.
    Gap,
}

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
    pub fn apply_snapshot(&mut self, seq: u64, bids: &[Level], asks: &[Level]) {
        self.bids.clear();
        self.asks.clear();
        let ts = self.tick_size;
        for &(px, qty) in bids {
            Self::apply_side(&mut self.bids, Self::tick_at(ts, px), qty);
        }
        for &(px, qty) in asks {
            Self::apply_side(&mut self.asks, Self::tick_at(ts, px), qty);
        }
        self.last_seq = seq;
    }

    /// The one delta-apply decision (see the module doc): given an incoming delta's `seq` and
    /// the stream's contiguity policy, say whether it folds, is stale, or means dropped frames.
    /// `seq == 0` is the "venue supplied no seq" sentinel: it always applies under
    /// [`SeqPolicy::Monotonic`] (matching [`L2Book::apply_delta`]'s long-standing convention);
    /// under [`SeqPolicy::Strict`] it is judged as a plain number (a strict stream that stops
    /// carrying its seq has, by definition, lost its integrity chain).
    pub fn delta_decision(&self, seq: u64, policy: SeqPolicy) -> DeltaDecision {
        match policy {
            SeqPolicy::Monotonic => {
                if seq != 0 && seq <= self.last_seq {
                    DeltaDecision::Stale
                } else {
                    DeltaDecision::Apply
                }
            }
            SeqPolicy::Strict => {
                // wrapping_add: last_seq == u64::MAX must not panic in debug builds. At that
                // edge the wrap target is 0, so an incoming seq == 0 would actually read as
                // Apply here (folding via this sentinel and resetting last_seq to 0), not
                // Gap/Stale — a real divergence from the "no true next seq" intent. Accepted:
                // no real venue seq counter reaches u64::MAX, so the case is unreachable in
                // practice; wrapping_add exists purely to keep debug builds panic-free.
                if seq == self.last_seq.wrapping_add(1) {
                    DeltaDecision::Apply
                } else if seq == self.last_seq {
                    DeltaDecision::Stale
                } else {
                    DeltaDecision::Gap
                }
            }
        }
    }

    /// Apply an incremental depth delta (upsert; qty 0 removes). Seq must not regress —
    /// a stale/duplicate seq is dropped (returns false). Consults [`L2Book::delta_decision`]
    /// under [`SeqPolicy::Monotonic`]; a strict-stream consumer asks `delta_decision` itself
    /// first (for the [`DeltaDecision::Gap`] arm this method cannot see) and only then folds.
    pub fn apply_delta(&mut self, seq: u64, bids: &[Level], asks: &[Level]) -> bool {
        if self.delta_decision(seq, SeqPolicy::Monotonic) != DeltaDecision::Apply {
            return false; // stale/replayed — the venue task handles resync
        }
        let ts = self.tick_size;
        for &(px, qty) in bids {
            Self::apply_side(&mut self.bids, Self::tick_at(ts, px), qty);
        }
        for &(px, qty) in asks {
            Self::apply_side(&mut self.asks, Self::tick_at(ts, px), qty);
        }
        self.last_seq = seq;
        true
    }

    /// (price, qty) of the best bid (highest bid tick).
    pub fn best_bid(&self) -> Option<Level> {
        self.bids.iter().next_back().map(|(&t, &q)| (self.price_of(t), q))
    }

    /// (price, qty) of the best ask (lowest ask tick).
    pub fn best_ask(&self) -> Option<Level> {
        self.asks.iter().next().map(|(&t, &q)| (self.price_of(t), q))
    }

    pub fn mid(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some((b, _)), Some((a, _))) => Some((b + a) / 2.0),
            _ => None,
        }
    }

    pub fn spread(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some((b, _)), Some((a, _))) => Some(a - b),
            _ => None,
        }
    }

    /// Top-of-book size imbalance in [-1, 1]: (bidQ − askQ) / (bidQ + askQ). None if
    /// either side is empty or both sizes are zero.
    pub fn imbalance(&self) -> Option<f64> {
        let (_, bq) = self.best_bid()?;
        let (_, aq) = self.best_ask()?;
        let denom = bq + aq;
        if denom == 0.0 {
            None
        } else {
            Some((bq - aq) / denom)
        }
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
    pub fn top_n(&self, n: usize) -> (Vec<Level>, Vec<Level>) {
        let bids = self.bids.iter().rev().take(n).map(|(&t, &q)| (self.price_of(t), q)).collect();
        let asks = self.asks.iter().take(n).map(|(&t, &q)| (self.price_of(t), q)).collect();
        (bids, asks)
    }

    // ---- walk-the-book helpers (pre-trade impact estimates; see module doc) ----

    /// The one walk core: consume `levels` (already ordered best-first) until `qty` is
    /// filled. `qty <= 0` is vacuously complete (nothing to fill); NaN qty fills nothing
    /// and stays incomplete. When a level covers the residual need it takes exactly the
    /// residual (`need`) and stops — so a completed walk reports `complete` without float
    /// dust deciding termination.
    fn walk_levels(
        levels: impl Iterator<Item = Level>,
        qty: f64,
        mut fills: Option<&mut Vec<Level>>,
    ) -> Walk {
        let mut w = Walk { filled: 0.0, notional: 0.0, worst_px: None, levels: 0, complete: false };
        if qty.is_nan() || qty <= 0.0 {
            w.complete = qty <= 0.0; // NaN → incomplete; zero/negative → vacuously complete
            return w;
        }
        for (px, lvl_qty) in levels {
            let need = qty - w.filled;
            let take = if lvl_qty >= need { need } else { lvl_qty };
            w.filled += take;
            w.notional += px * take;
            w.worst_px = Some(px);
            w.levels += 1;
            if let Some(f) = fills.as_deref_mut() {
                f.push((px, take));
            }
            if lvl_qty >= need {
                w.complete = true;
                break;
            }
        }
        w
    }

    /// Dispatch a best-first walk to the side `side` consumes: +1 buy → asks low→high,
    /// −1 sell → bids high→low, 0 → nothing.
    fn walk(&self, side: i32, qty: f64, fills: Option<&mut Vec<Level>>) -> Walk {
        if side > 0 {
            Self::walk_levels(self.asks.iter().map(|(&t, &q)| (self.price_of(t), q)), qty, fills)
        } else if side < 0 {
            Self::walk_levels(
                self.bids.iter().rev().map(|(&t, &q)| (self.price_of(t), q)),
                qty,
                fills,
            )
        } else {
            Self::walk_levels(std::iter::empty(), qty, fills)
        }
    }

    /// VWAP to fill `qty` against displayed liquidity, walking levels best-first.
    /// `None` when the book cannot fill the full `qty` (or side is 0, or qty is not a
    /// positive finite number) — an estimate upper-bounding real fill quality, for
    /// RiskGate thresholds / DOM cost-to-fill, not a guarantee.
    pub fn avg_px_for_quantity(&self, side: i32, qty: f64) -> Option<f64> {
        let w = self.walk(side, qty, None);
        if w.complete && w.filled > 0.0 {
            Some(w.notional / w.filled)
        } else {
            None
        }
    }

    /// Total displayed size available at `limit_px` or better (buy: asks priced at or
    /// below; sell: bids priced at or above). The limit is snapped to the book's tick
    /// grid with the SAME round-half-even rule level prices use — exact-equality at the
    /// boundary, no float-compare hazard (callers pass on-grid prices in practice).
    /// 0.0 for side 0 / NaN limit.
    pub fn quantity_for_price(&self, side: i32, limit_px: f64) -> f64 {
        if limit_px.is_nan() {
            return 0.0;
        }
        let lt = Self::tick_at(self.tick_size, limit_px);
        if side > 0 {
            self.asks.range(..=lt).map(|(_, &q)| q).sum::<f64>()
        } else if side < 0 {
            self.bids.range(lt..).map(|(_, &q)| q).sum::<f64>()
        } else {
            0.0
        }
    }

    /// Full pre-trade walk with per-level detail — the DOM cost-to-fill / RiskGate view.
    /// Walks displayed levels best-first until `qty` is filled or the side is exhausted;
    /// slippage is signed vs the current mid (positive = worse than mid for the taker)
    /// and `None`-safe when either book side is empty (no mid), mid is 0, or nothing
    /// filled. Estimates only: displayed depth upper-bounds real fill quality.
    pub fn simulate_fill(&self, side: i32, qty: f64) -> FillSim {
        let mut fills = Vec::new();
        let w = self.walk(side, qty, Some(&mut fills));
        let avg_px = if w.filled > 0.0 { Some(w.notional / w.filled) } else { None };
        let slippage_bps_vs_mid = match (avg_px, self.mid()) {
            (Some(avg), Some(mid)) if mid != 0.0 => {
                let sign = if side > 0 { 1.0 } else { -1.0 };
                Some(sign * (avg - mid) / mid * 10_000.0)
            }
            _ => None,
        };
        let remaining = if w.complete { 0.0 } else { qty - w.filled };
        FillSim {
            fills,
            total_filled: w.filled,
            remaining,
            avg_px,
            worst_px: w.worst_px,
            slippage_bps_vs_mid,
            levels_consumed: w.levels,
        }
    }

    /// True when displayed liquidity covers the full `qty` (qty ≤ 0 is vacuously true).
    /// Displayed depth is an upper bound — a `true` here is necessary, not sufficient,
    /// for the real fill.
    pub fn can_fill(&self, side: i32, qty: f64) -> bool {
        self.walk(side, qty, None).complete
    }

    /// Fraction of `qty` the displayed book can fill, in [0, 1]. Exactly 1.0 when the
    /// walk completes (no float-dust ratios); qty ≤ 0 → 1.0 (vacuous); NaN qty → 0.0.
    pub fn fill_ratio(&self, side: i32, qty: f64) -> f64 {
        if qty.is_nan() {
            return 0.0;
        }
        if qty <= 0.0 {
            return 1.0;
        }
        let w = self.walk(side, qty, None);
        if w.complete {
            1.0
        } else {
            w.filled / qty
        }
    }
}

/// Accumulator for one best-first book walk (private to [`L2Book`]'s helpers).
struct Walk {
    /// quantity consumed so far (naive fold)
    filled: f64,
    /// Σ price×take over consumed levels (naive fold, best-first)
    notional: f64,
    /// deepest price touched
    worst_px: Option<f64>,
    /// levels touched
    levels: usize,
    /// the walk covered the requested qty
    complete: bool,
}

/// Result of [`L2Book::simulate_fill`] — a pre-trade estimate against DISPLAYED
/// liquidity. Upper bound on fill quality, not an execution guarantee: intended for
/// RiskGate veto thresholds and DOM cost-to-fill display.
#[derive(Debug, Clone, PartialEq)]
pub struct FillSim {
    /// (price, qty consumed) per touched level, best-first; the last entry may be a
    /// partial take of that level.
    pub fills: Vec<Level>,
    /// total quantity the displayed book could fill (≤ requested)
    pub total_filled: f64,
    /// requested − filled; exactly 0.0 when the walk completed
    pub remaining: f64,
    /// VWAP over what filled; `None` when nothing filled
    pub avg_px: Option<f64>,
    /// deepest (worst) price touched; `None` when nothing filled
    pub worst_px: Option<f64>,
    /// signed cost vs mid in basis points — POSITIVE = worse than mid for the taker
    /// (buy above / sell below); `None` when either side is empty (no mid), mid == 0,
    /// or nothing filled
    pub slippage_bps_vs_mid: Option<f64>,
    /// number of price levels touched (== `fills.len()`)
    pub levels_consumed: usize,
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
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    /// instrument id — empty for single-symbol paths (same convention as `QuoteTick`)
    #[serde(default)]
    pub symbol: String,
}

/// THE taker-price law: what `qty` units of `side` actually cost against the displayed book,
/// walking levels best-first, refusing anything worse than `limit`.
///
/// It lives HERE, beside [`L2Book`], and not in the backtest engine, because it is a property of an
/// order book rather than of a simulation — and because BOTH sides of the system must agree on it.
/// The backtest's `L2BookFillModel` prices fills with it; [`crate::Broker::quote_vwap`] answers a
/// strategy's pre-trade "what would this cost me?" with it; a live sizer or a paper executor on a
/// live feed calls it directly. Those callers span crates that must not depend on each other
/// (`vike-backtest` is a SIBLING of the live path, not below it — a bridge reaching into it would
/// break the down-only layering), so a shared home is the only way they can share one definition.
/// **A second copy of this rule is how a live-vs-backtest divergence gets born**: the backtest would
/// go on describing a system the live path no longer is, and no test would notice.
///
/// `None` — deliberately, in every case where the displayed book cannot honour the order:
///
/// * `side == 0`, `qty <= 0`, or a NaN input;
/// * the displayed depth (within `limit`, when given) does not COVER `qty`.
///
/// The second case is the important one: it means "not fillable here", NOT "fill it anyway at the
/// last price you saw". Displayed depth already upper-bounds real fill quality (it ignores queue
/// position, hidden size and the taker's own market impact), so filling beyond it would be
/// inventing liquidity twice over. A caller that wants a partial fill must size DOWN first — the
/// [`crate::Broker::depth_within_price`] read exists precisely so a strategy can.
pub fn book_taker_price(book: &L2Book, side: i32, qty: f64, limit: Option<f64>) -> Option<f64> {
    // NaN first, so the `<=` below is a total comparison rather than a silently-false one.
    if side == 0 || qty.is_nan() || qty <= 0.0 {
        return None;
    }
    if let Some(lim) = limit {
        if lim.is_nan() {
            return None;
        }
        // Within-limit depth must cover the order. Because the walk is best-first, that check is
        // exactly what makes the uncapped `avg_px_for_quantity` below equal the CAPPED walk: every
        // level it can reach is inside the limit.
        if book.quantity_for_price(side, lim) < qty {
            return None;
        }
    }
    book.avg_px_for_quantity(side, qty)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Moved here with the law itself (it was in vike-backtest's `fill_model.rs`): the rule is a
    /// property of the book, so its proof belongs beside it — and `vike-model` is the one crate
    /// both the backtest engine and the live path can reach.
    #[test]
    fn book_taker_price_refuses_rather_than_inventing_liquidity() {
        let mut b = L2Book::new(0.01);
        // asks 0.30 x100, 0.31 x100, 0.35 x1000 ; bids 0.28 x100, 0.27 x100
        b.apply_snapshot(
            1,
            &[(0.28, 100.0), (0.27, 100.0)],
            &[(0.30, 100.0), (0.31, 100.0), (0.35, 1000.0)],
        );
        assert_eq!(book_taker_price(&b, 0, 10.0, None), None, "no side");
        assert_eq!(book_taker_price(&b, 1, 0.0, None), None, "no size");
        assert_eq!(book_taker_price(&b, 1, -5.0, None), None, "negative size");
        assert_eq!(book_taker_price(&b, 1, f64::NAN, None), None);
        assert_eq!(book_taker_price(&b, 1, 10.0, Some(f64::NAN)), None);
        assert_eq!(book_taker_price(&b, 1, 5_000.0, None), None, "beyond displayed depth");
        assert_eq!(book_taker_price(&L2Book::new(0.01), 1, 1.0, None), None, "empty book");
        // the capped walk equals the uncapped one when the cap is not binding
        assert_eq!(
            book_taker_price(&b, 1, 150.0, Some(0.31)),
            book_taker_price(&b, 1, 150.0, None)
        );
    }

    #[test]
    fn qty_at_reads_exact_levels_and_zero_when_absent() {
        let mut b = L2Book::new(0.01);
        b.apply_snapshot(1, &[(0.45, 100.0), (0.44, 20.0)], &[(0.46, 50.0)]);
        assert_eq!(b.bid_qty_at(0.45), 100.0);
        assert_eq!(b.bid_qty_at(0.44), 20.0);
        assert_eq!(b.ask_qty_at(0.46), 50.0);
        // absent levels (either side) read 0.0
        assert_eq!(b.bid_qty_at(0.43), 0.0);
        assert_eq!(b.ask_qty_at(0.47), 0.0);
        assert_eq!(b.bid_qty_at(0.46), 0.0); // wrong side reads 0 too
                                             // a qty-0 delta removes the level → 0.0
        b.apply_delta(2, &[(0.45, 0.0)], &[]);
        assert_eq!(b.bid_qty_at(0.45), 0.0);
    }

    #[test]
    fn book_update_serde_roundtrip_and_defaults() {
        let u = BookUpdate {
            ts: 1_000,
            local_ts: 1_002,
            seq: 7,
            kind: BookUpdateKind::Delta,
            tick_size: 0.01,
            bids: vec![(0.45, 100.0)],
            asks: vec![(0.46, 50.0)],
            symbol: "TOK".to_string(),
        };
        let s = serde_json::to_string(&u).unwrap();
        let back: BookUpdate = serde_json::from_str(&s).unwrap();
        assert_eq!(back.seq, 7);
        assert_eq!(back.kind, BookUpdateKind::Delta);
        assert_eq!(back.bids[0].0.to_bits(), 0.45f64.to_bits());
        // additive-serde contract: local_ts and symbol absent in old payloads → defaults
        let old: BookUpdate = serde_json::from_str(
            r#"{"ts":1,"seq":1,"kind":"Snapshot","tick_size":0.01,"bids":[],"asks":[]}"#,
        )
        .unwrap();
        assert_eq!(old.local_ts, 0);
        assert!(old.symbol.is_empty());
    }

    // ---- walk-the-book helpers ----

    /// Tick 0.5 keeps every price/tick round-trip binary-exact (n/2 grid), so the walk
    /// asserts below are bit-exact, not tolerance-based.
    fn book(bids: &[Level], asks: &[Level]) -> L2Book {
        let mut b = L2Book::new(0.5);
        b.apply_snapshot(1, bids, asks);
        b
    }

    /// bids 99.5×5, 99.0×10 | asks 100.5×5, 101.0×10 — mid exactly 100.0, 15 per side.
    fn two_level_book() -> L2Book {
        book(&[(99.5, 5.0), (99.0, 10.0)], &[(100.5, 5.0), (101.0, 10.0)])
    }

    #[test]
    fn walk_empty_book_is_all_none_and_unfillable() {
        let b = L2Book::new(0.5);
        assert_eq!(b.avg_px_for_quantity(1, 1.0), None);
        assert_eq!(b.avg_px_for_quantity(-1, 1.0), None);
        assert_eq!(b.quantity_for_price(1, 100.0), 0.0);
        assert_eq!(b.quantity_for_price(-1, 100.0), 0.0);
        assert!(!b.can_fill(1, 1.0));
        assert!(!b.can_fill(-1, 1.0));
        assert_eq!(b.fill_ratio(1, 1.0), 0.0);
        let sim = b.simulate_fill(1, 3.0);
        assert!(sim.fills.is_empty());
        assert_eq!(sim.total_filled, 0.0);
        assert_eq!(sim.remaining, 3.0);
        assert_eq!(sim.avg_px, None);
        assert_eq!(sim.worst_px, None);
        assert_eq!(sim.slippage_bps_vs_mid, None);
        assert_eq!(sim.levels_consumed, 0);
        // qty 0 is vacuously fillable even on an empty book
        assert!(b.can_fill(1, 0.0));
        assert_eq!(b.fill_ratio(1, 0.0), 1.0);
    }

    #[test]
    fn walk_zero_qty_on_populated_book_is_vacuous() {
        let b = two_level_book();
        assert_eq!(b.avg_px_for_quantity(1, 0.0), None); // no VWAP of nothing
        assert!(b.can_fill(1, 0.0));
        assert_eq!(b.fill_ratio(1, 0.0), 1.0);
        let sim = b.simulate_fill(1, 0.0);
        assert!(sim.fills.is_empty());
        assert_eq!(sim.total_filled, 0.0);
        assert_eq!(sim.remaining, 0.0);
        assert_eq!(sim.avg_px, None);
        assert_eq!(sim.worst_px, None);
        assert_eq!(sim.slippage_bps_vs_mid, None);
        assert_eq!(sim.levels_consumed, 0);
    }

    #[test]
    fn walk_one_level_exact_qty() {
        let b = book(&[], &[(100.5, 5.0)]);
        assert_eq!(b.avg_px_for_quantity(1, 5.0), Some(100.5));
        assert!(b.can_fill(1, 5.0));
        assert_eq!(b.fill_ratio(1, 5.0), 1.0);
        let sim = b.simulate_fill(1, 5.0);
        assert_eq!(sim.fills, vec![(100.5, 5.0)]);
        assert_eq!(sim.total_filled, 5.0);
        assert_eq!(sim.remaining, 0.0);
        assert_eq!(sim.avg_px, Some(100.5));
        assert_eq!(sim.worst_px, Some(100.5));
        assert_eq!(sim.slippage_bps_vs_mid, None); // bid side empty → no mid
        assert_eq!(sim.levels_consumed, 1);
    }

    #[test]
    fn walk_one_level_partial_when_qty_exceeds_book() {
        let b = book(&[], &[(100.5, 5.0)]);
        assert_eq!(b.avg_px_for_quantity(1, 6.0), None); // can't fill fully
        assert!(!b.can_fill(1, 6.0));
        assert_eq!(b.fill_ratio(1, 6.0), 5.0 / 6.0);
        let sim = b.simulate_fill(1, 6.0);
        assert_eq!(sim.fills, vec![(100.5, 5.0)]);
        assert_eq!(sim.total_filled, 5.0);
        assert_eq!(sim.remaining, 1.0);
        assert_eq!(sim.avg_px, Some(100.5)); // VWAP of what DID fill
        assert_eq!(sim.worst_px, Some(100.5));
        assert_eq!(sim.levels_consumed, 1);
    }

    #[test]
    fn walk_multi_level_buy_vwap_and_detail() {
        let b = two_level_book();
        // buy 7: 5 @ 100.5 + 2 @ 101.0 (best-first on asks, low→high)
        let expect_avg = (5.0 * 100.5 + 2.0 * 101.0) / 7.0;
        assert_eq!(b.avg_px_for_quantity(1, 7.0), Some(expect_avg));
        let sim = b.simulate_fill(1, 7.0);
        assert_eq!(sim.fills, vec![(100.5, 5.0), (101.0, 2.0)]);
        assert_eq!(sim.total_filled, 7.0);
        assert_eq!(sim.remaining, 0.0);
        assert_eq!(sim.avg_px, Some(expect_avg));
        assert_eq!(sim.worst_px, Some(101.0));
        assert_eq!(sim.levels_consumed, 2);
    }

    #[test]
    fn walk_multi_level_sell_walks_bids_high_to_low() {
        let b = two_level_book();
        // sell 7: 5 @ 99.5 + 2 @ 99.0 (best-first on bids, high→low)
        let expect_avg = (5.0 * 99.5 + 2.0 * 99.0) / 7.0;
        assert_eq!(b.avg_px_for_quantity(-1, 7.0), Some(expect_avg));
        let sim = b.simulate_fill(-1, 7.0);
        assert_eq!(sim.fills, vec![(99.5, 5.0), (99.0, 2.0)]);
        assert_eq!(sim.worst_px, Some(99.0));
        assert_eq!(sim.levels_consumed, 2);
    }

    #[test]
    fn walk_exact_boundary_consumes_whole_side() {
        let b = two_level_book();
        // exactly the total displayed on each side
        for side in [1, -1] {
            assert!(b.can_fill(side, 15.0));
            assert_eq!(b.fill_ratio(side, 15.0), 1.0);
            let sim = b.simulate_fill(side, 15.0);
            assert_eq!(sim.total_filled, 15.0);
            assert_eq!(sim.remaining, 0.0);
            assert_eq!(sim.levels_consumed, 2);
            // one drop more than displayed → unfillable
            assert!(!b.can_fill(side, 15.0 + 1e-9));
            assert_eq!(b.avg_px_for_quantity(side, 15.0 + 1e-9), None);
        }
    }

    #[test]
    fn walk_partial_multi_level_ratio_and_remaining() {
        let b = two_level_book();
        let sim = b.simulate_fill(1, 20.0);
        assert_eq!(sim.total_filled, 15.0);
        assert_eq!(sim.remaining, 5.0);
        assert_eq!(sim.avg_px, Some((5.0 * 100.5 + 10.0 * 101.0) / 15.0));
        assert_eq!(sim.worst_px, Some(101.0));
        assert_eq!(sim.levels_consumed, 2);
        assert_eq!(b.fill_ratio(1, 20.0), 15.0 / 20.0);
    }

    #[test]
    fn quantity_for_price_at_or_better_both_sides() {
        let b = two_level_book();
        // buy: asks at or below the limit
        assert_eq!(b.quantity_for_price(1, 100.0), 0.0); // below best ask
        assert_eq!(b.quantity_for_price(1, 100.5), 5.0); // exact best-ask boundary included
        assert_eq!(b.quantity_for_price(1, 101.0), 15.0);
        assert_eq!(b.quantity_for_price(1, 200.0), 15.0);
        // sell: bids at or above the limit
        assert_eq!(b.quantity_for_price(-1, 100.0), 0.0); // above best bid
        assert_eq!(b.quantity_for_price(-1, 99.5), 5.0); // exact best-bid boundary included
        assert_eq!(b.quantity_for_price(-1, 99.0), 15.0);
        assert_eq!(b.quantity_for_price(-1, 1.0), 15.0);
    }

    #[test]
    fn quantity_for_price_snaps_limit_to_tick_grid() {
        // realistic 0.01 grid: the exact-boundary compare goes through tick keys, so a
        // limit equal to a level's price always includes that level (no float-compare
        // hazard on prices like 0.1 that aren't binary-exact).
        let mut b = L2Book::new(0.01);
        b.apply_snapshot(1, &[(0.29, 7.0)], &[(0.3, 4.0), (0.31, 6.0)]);
        assert_eq!(b.quantity_for_price(1, 0.3), 4.0);
        assert_eq!(b.quantity_for_price(1, 0.31), 10.0);
        assert_eq!(b.quantity_for_price(-1, 0.29), 7.0);
        // off-grid limit snaps with the same round-half-even rule levels use
        assert_eq!(b.quantity_for_price(1, 0.302), 4.0);
    }

    #[test]
    fn slippage_sign_positive_means_worse_than_mid_both_sides() {
        let b = two_level_book(); // mid exactly 100.0
        let buy = b.simulate_fill(1, 10.0); // avg 100.75 > mid → cost
        let avg_b = (5.0 * 100.5 + 5.0 * 101.0) / 10.0;
        assert_eq!(buy.avg_px, Some(avg_b));
        let slip_b = buy.slippage_bps_vs_mid.unwrap();
        assert!((slip_b - 75.0).abs() < 1e-9, "buy slippage {slip_b} != ~75bps");
        assert!(slip_b > 0.0);
        let sell = b.simulate_fill(-1, 10.0); // avg 99.25 < mid → cost, sign flipped
        let avg_s = (5.0 * 99.5 + 5.0 * 99.0) / 10.0;
        assert_eq!(sell.avg_px, Some(avg_s));
        let slip_s = sell.slippage_bps_vs_mid.unwrap();
        assert!((slip_s - 75.0).abs() < 1e-9, "sell slippage {slip_s} != ~75bps");
        assert!(slip_s > 0.0);
    }

    #[test]
    fn slippage_none_when_one_side_empty_but_fill_still_reported() {
        let b = book(&[], &[(100.5, 5.0), (101.0, 10.0)]);
        let sim = b.simulate_fill(1, 7.0);
        assert_eq!(sim.total_filled, 7.0);
        assert!(sim.avg_px.is_some());
        assert_eq!(sim.slippage_bps_vs_mid, None); // no bid → no mid → None-safe
    }

    #[test]
    fn walk_degenerate_crossed_book_tolerated() {
        // crossed input (bid above ask) — reducer stores it as pushed; walks must not
        // panic and slippage reads as price IMPROVEMENT (negative) vs the crossed mid.
        let b = book(&[(101.0, 1.0)], &[(100.0, 1.0)]); // mid 100.5
        let buy = b.simulate_fill(1, 1.0);
        assert_eq!(buy.avg_px, Some(100.0));
        assert!(buy.slippage_bps_vs_mid.unwrap() < 0.0);
        let sell = b.simulate_fill(-1, 1.0);
        assert_eq!(sell.avg_px, Some(101.0));
        assert!(sell.slippage_bps_vs_mid.unwrap() < 0.0);
        assert!(b.can_fill(1, 1.0) && b.can_fill(-1, 1.0));
    }

    #[test]
    fn walk_side_zero_fills_nothing() {
        let b = two_level_book();
        assert_eq!(b.avg_px_for_quantity(0, 1.0), None);
        assert_eq!(b.quantity_for_price(0, 100.5), 0.0);
        assert!(!b.can_fill(0, 1.0));
        assert!(b.can_fill(0, 0.0)); // still vacuous at qty 0
        assert_eq!(b.fill_ratio(0, 1.0), 0.0);
        let sim = b.simulate_fill(0, 1.0);
        assert!(sim.fills.is_empty());
        assert_eq!(sim.total_filled, 0.0);
        assert_eq!(sim.remaining, 1.0);
    }

    #[test]
    fn walk_degenerate_qty_inputs_graceful() {
        let b = two_level_book();
        // negative qty: nothing to fill → vacuously complete
        assert!(b.can_fill(1, -1.0));
        assert_eq!(b.fill_ratio(1, -1.0), 1.0);
        assert_eq!(b.avg_px_for_quantity(1, -1.0), None);
        let sim = b.simulate_fill(1, -1.0);
        assert!(sim.fills.is_empty());
        assert_eq!(sim.total_filled, 0.0);
        assert_eq!(sim.remaining, 0.0);
        // NaN qty: unfillable, nothing consumed, no panic
        assert!(!b.can_fill(1, f64::NAN));
        assert_eq!(b.fill_ratio(1, f64::NAN), 0.0);
        assert_eq!(b.avg_px_for_quantity(1, f64::NAN), None);
        let sim = b.simulate_fill(1, f64::NAN);
        assert!(sim.fills.is_empty());
        assert_eq!(sim.total_filled, 0.0);
        assert_eq!(sim.avg_px, None);
        // NaN limit price: no size
        assert_eq!(b.quantity_for_price(1, f64::NAN), 0.0);
    }
}
