//! PURE pre-trade market-impact primitives — the walk-the-book veto, hoisted here from
//! `vike_exec::risk` (which re-exports every item, so this move is a no-op for its callers).
//!
//! Nothing in this module holds state, owns a bus, or does I/O: each fn is a total function of
//! an [`L2Book`] plus scalars, which is exactly why it belongs in the common ancestor crate.
//! `vike_exec::RiskGate::check_with_book` is the gate-side consumer; vike-chart's DOM readout
//! is the other natural one (it currently hand-derives the same predicate behind a
//! sync-by-comment block, because it could not reach vike-exec — it CAN reach here, but that
//! rewire is deliberately its own PR).
//!
//! **What the veto judges:** only the part of an order that can take DISPLAYED liquidity right
//! now ([`take_scope`]). Passive limits, stops and take-profits are never vetoed — a resting
//! quote pays no slippage. Displayed depth only UPPER-BOUNDS fill quality: these are estimates,
//! not guarantees.

use crate::L2Book;

/// Typed reason for a pre-trade impact veto. `as_str` is the stable wire string that reaches
/// `Event::OrderDenied.reason` (existing reasons are kebab-case, these follow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImpactDeny {
    /// projected slippage vs mid exceeded `max_slippage_bps`
    OverSlippageBudget,
    /// displayed depth cannot cover the full order size
    NotFillable,
}

impl ImpactDeny {
    pub fn as_str(self) -> &'static str {
        match self {
            ImpactDeny::OverSlippageBudget => "impact-over-slippage-budget",
            ImpactDeny::NotFillable => "impact-not-fillable",
        }
    }
}

/// PURE pre-trade market-impact veto against DISPLAYED liquidity (module doc: estimates only —
/// displayed depth upper-bounds fill quality).
///
/// `budget` is the slippage allowance in bps vs mid; `None` ⇒ never denies (no book walk at
/// all). With a budget set:
/// * `qty <= 0` ⇒ no veto (vacuous, matching [`fillable_veto`]);
/// * the walk cannot cover `qty` ⇒ [`ImpactDeny::NotFillable`] — the true slippage past the
///   displayed book is UNBOUNDED, so a partial-walk VWAP would understate it;
/// * slippage is unmeasurable (one-sided book ⇒ no mid, mid == 0, or nothing filled) ⇒ no veto
///   (this fn refuses to invent a number; use `require_fillable` for a depth floor);
/// * otherwise deny iff `slippage_bps_vs_mid > budget` (a budget-EQUAL fill passes).
///
/// Allocation note: `simulate_fill` collects per-level fills, so this is not free — it runs only
/// when a budget is set, which is off by default.
pub fn impact_veto(book: &L2Book, side: i32, qty: f64, budget: Option<f64>) -> Option<ImpactDeny> {
    let budget = budget?;
    // `qty <= 0` is vacuously fillable — MATCHES `fillable_veto`. Without this the
    // `total_filled <= 0.0` arm below would deny a zero-size walk while `fillable_veto` passes
    // it, and both fns are public exports. NaN still falls through to NotFillable in both.
    if qty <= 0.0 {
        return None;
    }
    let sim = book.simulate_fill(side, qty);
    if sim.remaining > 0.0 || sim.total_filled <= 0.0 {
        return Some(ImpactDeny::NotFillable);
    }
    match sim.slippage_bps_vs_mid {
        Some(slip) if slip > budget => Some(ImpactDeny::OverSlippageBudget),
        _ => None,
    }
}

/// How much of the DISPLAYED book an order can take RIGHT NOW — the scope the impact veto is
/// allowed to judge it against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TakeScope {
    /// The order cannot take displayed liquidity against the current book, so pre-trade impact
    /// is not a thing that happens to it: a passive (non-crossing) limit, a stop/take_profit
    /// that triggers against a FUTURE book, or a limit with no price. No walk, no veto.
    Passive,
    /// Unpriced taker — walks the whole opposing side.
    Market,
    /// Crossing limit at this price — only levels priced at `.0` or better are takeable; any
    /// remainder RESTS (it never pays slippage), so an incomplete walk is not a denial here.
    Crossing(f64),
}

/// PURE: classify an order against the current book (see [`TakeScope`]). Only `"market"` is
/// treated as an unpriced taker; a priced order takes only when it crosses the opposing best.
/// An empty opposing side ⇒ nothing to take ⇒ [`TakeScope::Passive`].
pub fn take_scope(book: &L2Book, side: i32, order_type: &str, price: Option<f64>) -> TakeScope {
    if order_type == "market" {
        return TakeScope::Market;
    }
    let Some(px) = price else { return TakeScope::Passive };
    if px.is_nan() {
        return TakeScope::Passive;
    }
    // stop / take_profit carry a trigger, not a live marketable price — they fire against a book
    // nobody can see yet, so judging them on today's depth would be fiction.
    if order_type != "limit" {
        return TakeScope::Passive;
    }
    let crosses = if side > 0 {
        book.best_ask().is_some_and(|(a, _)| px >= a)
    } else {
        book.best_bid().is_some_and(|(b, _)| px <= b)
    };
    if crosses {
        TakeScope::Crossing(px)
    } else {
        TakeScope::Passive
    }
}

/// PURE scoped impact veto — the form the gate uses. Applies [`fillable_veto`] /
/// [`impact_veto`] ONLY to the portion of the order that actually takes displayed liquidity
/// ([`take_scope`]):
/// * [`TakeScope::Passive`] ⇒ never denies, and does NOT touch the book;
/// * [`TakeScope::Market`] ⇒ the plain market-taker semantics of the two fns above;
/// * [`TakeScope::Crossing`] ⇒ depth is measured at the limit price or better
///   (`quantity_for_price`), and only the takeable slice is walked for slippage — an
///   unfillable remainder rests rather than paying unbounded impact, so the budget arm never
///   returns `NotFillable` for a crossing limit (`require_fillable` still can: it is an
///   explicit "I want this size filled now" floor).
pub fn scoped_impact_veto(
    book: &L2Book,
    scope: TakeScope,
    side: i32,
    qty: f64,
    budget: Option<f64>,
    require_fillable: bool,
) -> Option<ImpactDeny> {
    match scope {
        TakeScope::Passive => None,
        TakeScope::Market => {
            if require_fillable {
                if let Some(d) = fillable_veto(book, side, qty) {
                    return Some(d);
                }
            }
            impact_veto(book, side, qty, budget)
        }
        TakeScope::Crossing(px) => {
            let takeable = book.quantity_for_price(side, px);
            if require_fillable && takeable < qty {
                return Some(ImpactDeny::NotFillable);
            }
            budget?; // no budget ⇒ nothing left to judge (the depth floor ran above)
                     // walk only what is takeable at the limit or better; that slice lies entirely
                     // within the limit, so `simulate_fill` over it IS the limit-constrained walk.
            let slice = if takeable < qty { takeable } else { qty };
            impact_veto(book, side, slice, budget)
        }
    }
}

/// PURE depth floor: deny when displayed liquidity cannot cover the full `qty`. Cheap
/// (`can_fill` walks without collecting fills). `qty <= 0` is vacuously fillable.
pub fn fillable_veto(book: &L2Book, side: i32, qty: f64) -> Option<ImpactDeny> {
    if book.can_fill(side, qty) {
        None
    } else {
        Some(ImpactDeny::NotFillable)
    }
}
