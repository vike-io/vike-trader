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

/// One resting price level as venues push it. `qty == 0` ⇒ remove the level.
///
/// ⚠ **The named fields ARE this type's reason to exist — it was a `pub type BookLevel = (f64, f64)`
/// alias until 2026-09-16.** Every venue bridge builds these out of a DIFFERENT venue's JSON array
/// (binance's `parse_levels`, bybit's and okx's `levels`, deribit's `book_levels`, hyperliquid's
/// `side_levels`, ibkr's market-feed pump), and under the alias `(qty, price)` was exactly as valid
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

    // ---- walk-the-book helpers (pre-trade impact estimates; see module doc) ----

    /// The one walk core: consume `levels` (already ordered best-first) until `qty` is
    /// filled. `qty <= 0` is vacuously complete (nothing to fill); NaN qty fills nothing
    /// and stays incomplete. When a level covers the residual need it takes exactly the
    /// residual (`need`) and stops — so a completed walk reports `complete` without float
    /// dust deciding termination.
    fn walk_levels(
        levels: impl Iterator<Item = BookLevel>,
        qty: f64,
        mut fills: Option<&mut Vec<BookLevel>>,
    ) -> Walk {
        let mut w = Walk { filled: 0.0, notional: 0.0, worst_px: None, levels: 0, complete: false };
        if qty.is_nan() || qty <= 0.0 {
            w.complete = qty <= 0.0; // NaN → incomplete; zero/negative → vacuously complete
            return w;
        }
        for BookLevel { price: px, qty: lvl_qty } in levels {
            let need = qty - w.filled;
            let take = if lvl_qty >= need { need } else { lvl_qty };
            w.filled += take;
            w.notional += px * take;
            w.worst_px = Some(px);
            w.levels += 1;
            if let Some(f) = fills.as_deref_mut() {
                f.push(BookLevel::new(px, take));
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
    fn walk(&self, side: i32, qty: f64, fills: Option<&mut Vec<BookLevel>>) -> Walk {
        if side > 0 {
            Self::walk_levels(
                self.asks.iter().map(|(&t, &q)| BookLevel::new(self.price_of(t), q)),
                qty,
                fills,
            )
        } else if side < 0 {
            Self::walk_levels(
                self.bids.iter().rev().map(|(&t, &q)| BookLevel::new(self.price_of(t), q)),
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
        if w.complete && w.filled > 0.0 { Some(w.notional / w.filled) } else { None }
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
        if w.complete { 1.0 } else { w.filled / qty }
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
    pub fills: Vec<BookLevel>,
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
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    /// instrument id — empty for single-symbol paths (same convention as `QuoteTick`)
    #[serde(default)]
    pub symbol: String,
}

/// THE taker-price law: what `qty` units of `side` actually cost against the displayed book,
/// walking levels best-first, refusing anything worse than `limit`.
///
/// It lives HERE, beside [`L2Book`], and not in the backtest engine, because it is a property of an
/// order book rather than of a simulation — and because BOTH sides of the system must agree on it.
/// The backtest's `L2BookFillModel` prices fills with it; `Broker::quote_vwap` — a layer up, in
/// `crates/vike-model/src/strategy/mod.rs` — answers a strategy's pre-trade "what would this cost
/// me?" with it; a live sizer or a paper executor on a live feed calls it directly. Those callers
/// span crates that must not depend on each other
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
/// `Broker::depth_within_price` read (`crates/vike-model/src/strategy/mod.rs`) exists precisely so
/// a strategy can.
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

#[path = "orderbook_tests.rs"]
#[cfg(test)]
mod orderbook_tests;
