//! Walk-the-book: pre-trade impact estimates over displayed liquidity, and the taker-price law.

use super::{BookLevel, L2Book};

impl L2Book {
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

#[path = "walk_tests.rs"]
#[cfg(test)]
mod walk_tests;
