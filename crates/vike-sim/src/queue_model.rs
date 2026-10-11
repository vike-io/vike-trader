//! Queue-position models for resting maker LIMIT orders on the tick/book replay path.
//!
//! The frozen crossing model fills a resting limit the instant price touches it — as if the
//! order were always FIRST in the queue (see `StrategyEngine::fill_tagged`'s documented
//! limits). That over-fills makers: on a real venue an order at price P fills only after the
//! resting size AHEAD of it at P is consumed by trades. This module supplies that missing
//! queue-position estimate — an independent reimplementation of the mechanism popularized by
//! hftbacktest's queue models (no code taken from it).
//!
//! OPT-IN AND REPLAY-ONLY: activated by `EngineParams::queue_model = Some(kind)`; `None`
//! (the default) leaves every existing path byte-identical. When active it gates ONLY the
//! `run_ticks` tick/book replay lanes (untagged pending limits + the tagged HFT maker lane);
//! the bar path (`StrategyEngine::run`) never consults it.
//!
//! Composition rule (enforced by the engine wire-in, `engine.rs`):
//! - the EXISTING fill model still decides the price condition (limit crossed / touched);
//! - a STRICT price cross (trade or quote trades THROUGH the level) fills in full — everyone
//!   at the level, including the queue ahead, was consumed;
//! - a TOUCH (event exactly at the order's price) is queue-gated: a trade at P first consumes
//!   the estimated front, and only its EXCESS beyond the front fills the order (partial fills);
//!   a quote touch fills only once the front is already cleared;
//! - recorded L2 `BookUpdate`s drive `on_depth_change` at each tracked level, so cancellations
//!   ahead of the order shorten (or, for the probabilistic model, statistically shorten) the
//!   wait — exactly the depth-consumption channel the crossing model ignored.

use indexmap::IndexMap;
use vike_model::{L2Book, QuoteTick};

/// One resting order's queue estimate at its price level.
///
/// `front_qty` is the estimated resting qty AHEAD of the order (our simulated order itself is
/// never part of the market book). `cum_trade_qty` accumulates trade qty already applied to
/// `front_qty` so a later observed depth DECREASE at the level is not double-counted (the
/// venue's book update reflecting those same trades arrives after the trade prints). Only
/// [`ProbQueueModel`] consumes it; it is cleared once the echo is either netted out by a
/// decrease or SUPERSEDED by an increase (a level that grew past its pre-trade size has
/// demonstrably already absorbed the print — see [`ProbQueueModel::on_depth_change`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueueState {
    pub front_qty: f64,
    pub cum_trade_qty: f64,
}

impl QueueState {
    pub fn new(front_qty: f64) -> Self {
        QueueState { front_qty: front_qty.max(0.0), cum_trade_qty: 0.0 }
    }
}

/// The queue-position estimator seam. All verbs are pure f64 state folds — no I/O, no clock.
pub trait QueueModel {
    /// A new resting order joins the BACK of the level: everything currently resting
    /// (`price_level_qty`) is ahead of it.
    fn on_new_order(&self, price_level_qty: f64) -> QueueState;

    /// A trade printed at the order's price: it consumed liquidity from the FRONT of the level.
    fn on_trade(&self, st: &mut QueueState, trade_qty: f64);

    /// The observed resting qty at the order's price changed `old_qty` → `new_qty` (a recorded
    /// L2 book update). Increases join the back (front unchanged); decreases are attributed
    /// front-vs-back per the model, after netting out `cum_trade_qty` already applied.
    fn on_depth_change(&self, st: &mut QueueState, old_qty: f64, new_qty: f64);

    /// Nothing left ahead — the order is at the front of the queue.
    fn is_front_cleared(&self, st: &QueueState) -> bool {
        st.front_qty <= 0.0
    }
}

/// Conservative (risk-averse) estimator: the front shrinks ONLY on hard evidence — trades at
/// the order's price, or the level itself shrinking below the current front estimate (a depth
/// decrease can never leave more ahead of us than the whole level). Cancellations behind the
/// order are never credited to the front, so this is the pessimistic (slowest-fill) bound.
#[derive(Debug, Clone, Copy, Default)]
pub struct RiskAdverseQueueModel;

impl QueueModel for RiskAdverseQueueModel {
    fn on_new_order(&self, price_level_qty: f64) -> QueueState {
        QueueState::new(price_level_qty)
    }

    fn on_trade(&self, st: &mut QueueState, trade_qty: f64) {
        st.front_qty = (st.front_qty - trade_qty).max(0.0);
        st.cum_trade_qty += trade_qty;
    }

    fn on_depth_change(&self, st: &mut QueueState, _old_qty: f64, new_qty: f64) {
        // front ≤ level qty always; decreases clamp, increases are a no-op via min().
        st.front_qty = st.front_qty.min(new_qty.max(0.0));
    }
}

/// The pluggable decrease-attribution weight `f(x)` for [`ProbQueueModel`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProbFunc {
    /// `f(x) = x^n` — n = 1 splits a decrease pro-rata by size; larger n biases the
    /// attribution toward the (usually larger) back of the queue.
    Power(f64),
    /// `f(x) = ln(1 + x)` — a sub-linear weight (large backs saturate).
    Log,
}

impl ProbFunc {
    /// ⚠ `libm::pow`/`libm::log`, never `x.powf(n)`/`(…).ln()`. IEEE 754 requires `+ - * /` and
    /// `sqrt` correctly rounded and requires NOTHING of `pow`/`log`, so the method spellings call
    /// the PLATFORM's libm — MSVC's CRT on the Windows dev box, glibc on the the CI box Linux boxes —
    /// and the two disagree in the last bit. What that decides here is a FILL: `f` weights the
    /// front-vs-back attribution of a depth decrease, so it moves the estimated queue ahead of a
    /// resting maker, so it moves whether a trade's excess reaches the order at all. A backtest can
    /// fill on one box and not on the other.
    /// `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is the
    /// accepted verdict and carries the measurement.
    ///
    /// ⚠ Read the exposure honestly, because the arms differ. That record's rule is to classify a
    /// power by its BASE, and `x` here is a runtime `f64` queue quantity — the worst case. But the
    /// DEFAULT configuration escapes it: the bare `"prob_power"` profile spelling parses to
    /// `Power(1.0)` (`crates/vike-backtest/src/harness/profile/engine_cfg.rs`'s `QueueModelKind`),
    /// and `pow(x, 1.0)` returns `x` itself. ⚠ Stated from the SOURCE rather than from the
    /// standard, because the standard does not actually say it — C99 Annex F's `pow` special-case
    /// list covers `pow(1, y)` and `pow(x, ±0)` and has no `pow(x, 1)` row at all. What does say
    /// it is the implementation this crate now calls: `libm`'s `pow` takes a `y is +-1` branch that
    /// returns `x` unchanged for every non-NaN base, ahead of any arithmetic. So the UNCONDITIONAL
    /// divergence is the [`ProbFunc::Log`] arm (`"prob_log"`), plus `"prob_power:N"` for any
    /// `N != 1`. That is not a reason to leave the `Power` arm alone: `N` is an operator-supplied
    /// exponent, one profile line away from being anything, and a site that is portable only for
    /// one value of its parameter is portable by accident, not by design.
    fn f(&self, x: f64) -> f64 {
        match self {
            ProbFunc::Power(n) => libm::pow(x, *n),
            ProbFunc::Log => libm::log(1.0 + x),
        }
    }
}

/// Probabilistic estimator: a NET depth decrease `chg` at the order's price (observed decrease
/// minus `cum_trade_qty` already applied by trades) is split between the queue ahead (front)
/// and behind (back) of the order by `prob = f(back) / (f(back) + f(front))` — the probability
/// the cancellation came from BEHIND. The front estimate becomes
/// `front − (1 − prob)·chg + min(back − prob·chg, 0)` (the last term folds back-overflow —
/// change the back could not have supplied — into the front), clamped to `[0, new_level_qty]`.
#[derive(Debug, Clone, Copy)]
pub struct ProbQueueModel {
    pub func: ProbFunc,
}

impl ProbQueueModel {
    pub fn power(n: f64) -> Self {
        ProbQueueModel { func: ProbFunc::Power(n) }
    }

    pub fn log() -> Self {
        ProbQueueModel { func: ProbFunc::Log }
    }
}

impl QueueModel for ProbQueueModel {
    fn on_new_order(&self, price_level_qty: f64) -> QueueState {
        QueueState::new(price_level_qty)
    }

    fn on_trade(&self, st: &mut QueueState, trade_qty: f64) {
        st.front_qty = (st.front_qty - trade_qty).max(0.0);
        st.cum_trade_qty += trade_qty;
    }

    fn on_depth_change(&self, st: &mut QueueState, old_qty: f64, new_qty: f64) {
        let new_qty = new_qty.max(0.0);
        if new_qty > old_qty {
            // The level GREW past its pre-event size: any pending trade echo has already been
            // absorbed by the book (an update can only report the level's net state), so a
            // retained `cum_trade_qty` would wrongly excuse a LATER, unrelated cancellation
            // from front attribution. Drop it — the echo is superseded.
            st.cum_trade_qty = 0.0;
        }
        if new_qty < old_qty {
            let chg = old_qty - new_qty;
            // Net out trades already applied to the front so the book echo of those trades
            // is not double-counted; the remainder of the decrease is cancellations.
            let chg_net = chg - st.cum_trade_qty;
            st.cum_trade_qty = (st.cum_trade_qty - chg).max(0.0);
            if chg_net > 0.0 {
                let front = st.front_qty;
                let back = (old_qty - front).max(0.0);
                let (fb, ff) = (self.func.f(back), self.func.f(front));
                let denom = fb + ff;
                // Empty level both sides → nothing ahead; attribute the change to the back.
                let prob = if denom > 0.0 { fb / denom } else { 1.0 };
                let est_front = front - (1.0 - prob) * chg_net + (back - prob * chg_net).min(0.0);
                st.front_qty = est_front.clamp(0.0, new_qty);
                return;
            }
        }
        // Increase (joins the back) or a fully-trade-explained decrease: the front still can
        // never exceed the level.
        st.front_qty = st.front_qty.min(new_qty);
    }
}

/// The `EngineParams` selector — which queue model gates resting-limit fills in tick replay.
/// `Copy` so `EngineParams` construction sites stay `..Default::default()`-friendly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QueueModelKind {
    /// [`RiskAdverseQueueModel`] — the conservative bound.
    RiskAdverse,
    /// [`ProbQueueModel`] with `f(x) = x^n`.
    ProbPower(f64),
    /// [`ProbQueueModel`] with `f(x) = ln(1 + x)`.
    ProbLog,
}

impl QueueModelKind {
    /// Build the concrete model once (the tracker holds it for the whole replay).
    pub fn build(&self) -> Box<dyn QueueModel> {
        match self {
            QueueModelKind::RiskAdverse => Box::new(RiskAdverseQueueModel),
            QueueModelKind::ProbPower(n) => Box::new(ProbQueueModel::power(*n)),
            QueueModelKind::ProbLog => Box::new(ProbQueueModel::log()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Engine-side tracker (the wire-in state). pub(crate): only `engine::run_ticks` drives it.
// ---------------------------------------------------------------------------------------------

/// One resting order's queue entry: the level it was seeded at + the running estimate + the
/// engine-local identity (`WorkingOrder::qid`, or the tagged lane's stamped equivalent) it was
/// seeded FOR. Any of the three changing means the resting order is a DIFFERENT order — it
/// re-seeds at the back of its level rather than inheriting a priority it never earned.
pub(crate) struct OrderQueue {
    pub side: i32,
    pub price_bits: u64,
    pub qid: u64,
    pub state: QueueState,
}

impl OrderQueue {
    /// Does this entry still describe the same resting order at the same level?
    pub(crate) fn matches(&self, side: i32, price_bits: u64, qid: u64) -> bool {
        self.side == side && self.price_bits == price_bits && self.qid == qid
    }
}

/// Per-symbol queue bookkeeping.
///
/// BOTH lanes are keyed by REAL per-order identity, never by a `(side, price)` fingerprint:
/// untagged states are keyed by the order's `WorkingOrder::qid` (stamped once, monotonically,
/// when the queued pass first sees it), tagged states by tag WITH the tag's current `qid`
/// checked on every sync. That is what makes the classic maker re-quote — `cancel_all` (or
/// `cancel_tagged`) followed by a re-submit at the SAME price — correctly re-seed at the back
/// of the level: the replacement is a fresh `WorkingOrder`, so it carries a fresh identity and
/// cannot adopt the canceled order's advanced front estimate. States whose order is gone are
/// dropped by the next pass's rebuild. `IndexMap` for deterministic iteration.
#[derive(Default)]
pub(crate) struct SymQueue {
    /// qid → the order's queue entry
    pub pending: IndexMap<u64, OrderQueue>,
    pub tagged: IndexMap<String, OrderQueue>,
    /// last L1 quote `(bid, bid_size, ask, ask_size)` — the seed fallback when no book exists
    pub last_quote: Option<(f64, f64, f64, f64)>,
    /// tick size of the most recent book seen for this symbol — the grid the bookless quote
    /// seed quantizes to (see [`QueueTracker::seed`]). `None` until any book arrives.
    pub tick_size: Option<f64>,
}

/// The replay-scoped queue engine: ONE model + per-symbol state. Built by `StrategyEngine::new`
/// when `EngineParams::queue_model` is `Some`; absent otherwise (the byte-identical default).
pub(crate) struct QueueTracker {
    pub model: Box<dyn QueueModel>,
    /// seed depth when neither the replayed book nor a matching L1 quote knows the level qty
    pub seed_depth: f64,
    /// Minimum-hold floor (ms): a position-reducing fill is deferred until this long after the
    /// position opened (`0` = off). See [`EngineParams::queue_min_hold_ms`](crate::EngineParams).
    pub min_hold_ms: i64,
    pub sym: Vec<SymQueue>,
    /// monotonic identity source for `WorkingOrder::qid` (starts at 1; `0` = unstamped)
    next_qid: u64,
}

/// Do two prices name the SAME resting level? With a known tick grid this is the book's own
/// rule (quantize with round-half-to-even, compare tick indices) so a strategy price computed
/// arithmetically — `mid − k·tick` accumulating an ulp — still matches the level it sits on.
/// Without a grid (a purely quote-driven replay that never carried a book) fall back to a
/// few-ulp relative epsilon, which absorbs the same drift without merging distinct ticks.
pub(crate) fn same_level(a: f64, b: f64, tick_size: Option<f64>) -> bool {
    match tick_size {
        Some(t) if t > 0.0 => (a / t).round_ties_even() == (b / t).round_ties_even(),
        _ => a == b || (a - b).abs() <= a.abs().max(b.abs()) * 1e-9,
    }
}

/// Observed resting qty at `price` on the order's own side (`side > 0` rests on bids).
pub(crate) fn level_qty(book: Option<&L2Book>, side: i32, price: f64) -> Option<f64> {
    book.map(|b| if side > 0 { b.bid_qty_at(price) } else { b.ask_qty_at(price) })
}

impl SymQueue {
    /// Seed one new order's queue state from THIS symbol's view: the replayed book's level qty
    /// at its price → the matching side of the last L1 quote (tick-grid compare, see
    /// [`same_level`]) → `seed_depth`. Split out of [`QueueTracker::seed`] so a caller that
    /// already holds `&mut SymQueue` (the tagged lane's mid-pass re-seed) can reach it without
    /// re-borrowing the whole tracker.
    pub(crate) fn seed_with(
        &self,
        model: &dyn QueueModel,
        seed_depth: f64,
        side: i32,
        price: f64,
        book: Option<&L2Book>,
    ) -> QueueState {
        let depth = match level_qty(book, side, price) {
            Some(q) => q,
            None => {
                let tick = book.map(|b| b.tick_size).or(self.tick_size);
                match self.last_quote {
                    Some((bid, bid_sz, _, _)) if side > 0 && same_level(bid, price, tick) => bid_sz,
                    Some((_, _, ask, ask_sz)) if side < 0 && same_level(ask, price, tick) => ask_sz,
                    _ => seed_depth,
                }
            }
        };
        model.on_new_order(depth)
    }

    /// [`Self::seed_with`] wrapped as a whole entry for `qid` at `price`.
    pub(crate) fn new_entry_with(
        &self,
        model: &dyn QueueModel,
        seed_depth: f64,
        side: i32,
        price: f64,
        qid: u64,
        book: Option<&L2Book>,
    ) -> OrderQueue {
        OrderQueue {
            side,
            price_bits: price.to_bits(),
            qid,
            state: self.seed_with(model, seed_depth, side, price, book),
        }
    }
}

impl QueueTracker {
    pub(crate) fn new(
        kind: QueueModelKind,
        seed_depth: f64,
        min_hold_ms: i64,
        n_sym: usize,
    ) -> Self {
        QueueTracker {
            model: kind.build(),
            seed_depth,
            min_hold_ms,
            sym: (0..n_sym).map(|_| SymQueue::default()).collect(),
            next_qid: 1,
        }
    }

    /// Hand out the next engine-local order identity (see [`vike_model::WorkingOrder::qid`]).
    /// Monotonic for the whole replay, so a stamped id is never reused by a later order.
    pub(crate) fn next_qid(&mut self) -> u64 {
        let id = self.next_qid;
        self.next_qid += 1;
        id
    }

    /// Remember the freshest L1 quote (the bookless seed fallback).
    pub(crate) fn note_quote(&mut self, si: usize, q: &QuoteTick) {
        self.sym[si].last_quote = Some((q.bid, q.bid_size, q.ask, q.ask_size));
    }

    /// Seed a new order's queue state: book level qty at its price → the matching side/price of
    /// the last L1 quote → the configured `seed_depth`. The quote match is a TICK-GRID compare
    /// (see [`same_level`]), not bit-exact equality, so an ulp-drifted strategy price still
    /// seeds from the quote size instead of silently falling through to `seed_depth`.
    pub(crate) fn seed(
        &self,
        si: usize,
        side: i32,
        price: f64,
        book: Option<&L2Book>,
    ) -> QueueState {
        self.sym[si].seed_with(self.model.as_ref(), self.seed_depth, side, price, book)
    }

    /// Snapshot the tracked levels' resting qty BEFORE a book event is applied (the `old_qty`
    /// side of `on_depth_change`); `None` per level when no valid book exists yet.
    pub(crate) fn pre_depth(
        &self,
        si: usize,
        book: Option<&L2Book>,
    ) -> IndexMap<(i32, u64), Option<f64>> {
        let sq = &self.sym[si];
        let mut m: IndexMap<(i32, u64), Option<f64>> = IndexMap::new();
        for oq in sq.pending.values().chain(sq.tagged.values()) {
            m.entry((oq.side, oq.price_bits))
                .or_insert_with(|| level_qty(book, oq.side, f64::from_bits(oq.price_bits)));
        }
        m
    }

    /// Fold one applied book event into every tracked state: `old` from [`Self::pre_depth`],
    /// `new` from the post-apply book. A dropped book (`None` — gap/§B marker) leaves states
    /// untouched (no information); an unknown `old` with a known `new` applies the conservative
    /// clamp `front = min(front, new)` (a fresh anchor after a gap can only bound the front).
    pub(crate) fn apply_depth(
        &mut self,
        si: usize,
        pre: &IndexMap<(i32, u64), Option<f64>>,
        book: Option<&L2Book>,
    ) {
        let Some(book) = book else { return };
        let model = &self.model;
        let sq = &mut self.sym[si];
        // remember the grid: it is what the bookless quote seed quantizes to after a book drop
        sq.tick_size = Some(book.tick_size);
        for oq in sq.pending.values_mut().chain(sq.tagged.values_mut()) {
            let new = level_qty(Some(book), oq.side, f64::from_bits(oq.price_bits)).unwrap_or(0.0);
            let old = pre.get(&(oq.side, oq.price_bits)).copied().flatten();
            match old {
                Some(old) if old != new => model.on_depth_change(&mut oq.state, old, new),
                Some(_) => {}
                None => oq.state.front_qty = oq.state.front_qty.min(new.max(0.0)),
            }
        }
    }

    /// Sync the tagged side-table to the live tag registry before a tagged fill pass. A tag
    /// keeps its accumulated queue state ONLY when side, price AND identity (`qid`) all still
    /// match; anything else re-seeds at the back of its level. That covers every way a tag can
    /// name a DIFFERENT resting order than the one the state was built for:
    /// - a re-price (`modify_tagged` with a new price) — priority is forfeited on a real venue;
    /// - a re-submit over a LIVE tag (`submit_limit_tagged` REPLACES the resting order, so it
    ///   builds a fresh `WorkingOrder` with `qid == 0` → a fresh identity), including the
    ///   `cancel_tagged` + re-submit at the same price that makers re-quote with;
    /// - a `modify_tagged` qty INCREASE, which real venues treat as a new order at the back
    ///   (`SimBroker::modify_tagged` clears the `qid` for exactly this reason; an amend-DOWN
    ///   keeps both the identity and the priority, as venues do).
    ///
    /// `live` is `(tag, side, price, qid)` in the registry's insertion order.
    pub(crate) fn sync_tags(
        &mut self,
        si: usize,
        live: &[(String, i32, f64, u64)],
        book: Option<&L2Book>,
    ) {
        let mut old = std::mem::take(&mut self.sym[si].tagged);
        let mut next: IndexMap<String, OrderQueue> = IndexMap::with_capacity(live.len());
        for (tag, side, price, qid) in live {
            let pbits = price.to_bits();
            let entry = match old.shift_remove(tag) {
                Some(oq) if oq.matches(*side, pbits, *qid) => oq,
                _ => self.new_entry(si, *side, *price, *qid, book),
            };
            next.insert(tag.clone(), entry);
        }
        self.sym[si].tagged = next;
    }

    /// A fresh entry for an order joining the BACK of its level.
    pub(crate) fn new_entry(
        &self,
        si: usize,
        side: i32,
        price: f64,
        qid: u64,
        book: Option<&L2Book>,
    ) -> OrderQueue {
        OrderQueue {
            side,
            price_bits: price.to_bits(),
            qid,
            state: self.seed(si, side, price, book),
        }
    }
}

#[path = "queue_model_tests.rs"]
#[cfg(test)]
mod queue_model_tests;
