//! PURE pre-trade market-impact primitives — the walk-the-book veto. A private child of
//! `risk`: its one consumer is `RiskGate::check_with_book` (`risk.rs`). Hoisted out of
//! `vike_execution::risk` into vike-model by PR #481, it came back here on 2026-10-12, because no
//! other crate called it.
//!
//! Nothing in this module holds state, owns a bus, or does I/O: each fn is a total function of
//! an [`L2Book`] plus scalars.
//!
//! **What the veto judges:** only the part of an order that can take DISPLAYED liquidity right
//! now ([`take_scope`]). Passive limits, stops and take-profits are never vetoed — a resting
//! quote pays no slippage. Displayed depth only UPPER-BOUNDS fill quality: these are estimates,
//! not guarantees.

use vike_marketdata::L2Book;
use vike_model::{ContractValue, OrderPeg, OrderPegAnchor, OrderType};

/// Typed reason for a pre-trade impact veto. `denial` is its word in the gate's vocabulary, the stable
/// string that reaches `Event::OrderDenied.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImpactDeny {
    /// projected slippage vs mid exceeded `max_slippage_bps`
    OverSlippageBudget,
    /// displayed depth cannot cover the full order size
    NotFillable,
}

impl ImpactDeny {
    /// Its word in the gate's vocabulary.
    pub(super) fn denial(self) -> vike_model::OrderDenial {
        match self {
            ImpactDeny::OverSlippageBudget => vike_model::OrderDenial::ImpactOverSlippageBudget,
            ImpactDeny::NotFillable => vike_model::OrderDenial::ImpactNotFillable,
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
/// * otherwise deny iff the slippage of the walk's average vs mid, in bps signed worse-for-the-taker
///   (`L2BookFillSimulation::slippage_bps_vs_mid`'s expression), exceeds `budget` (a budget-EQUAL fill passes).
///
/// The average is `value`'s [`ContractValue::taker_price`]: the walk's VWAP on a linear contract, bit
/// for bit, and HARMONIC on an inverse one, whose arithmetic average sits above it, so an inverse buy
/// was judged a little worse than it fills and could be vetoed at the budget's edge. Allocation free
/// on both kinds (the walk collects nothing); it runs only when a budget is set, which is off by
/// default.
pub(super) fn impact_veto(
    book: &L2Book,
    side: i32,
    qty: f64,
    budget: Option<f64>,
    value: ContractValue,
) -> Option<ImpactDeny> {
    let budget = budget?;
    // `qty <= 0` is vacuously fillable — MATCHES `fillable_veto`. Without this the taker price's
    // refusal below would deny a zero-size walk while `fillable_veto` passes it, and the gate calls
    // both. NaN still falls through to NotFillable in both.
    if qty <= 0.0 {
        return None;
    }
    let Some(avg) = value.taker_price(book, side, qty, None) else {
        return Some(ImpactDeny::NotFillable);
    };
    match book.mid() {
        Some(mid) if mid != 0.0 => {
            let sign = if side > 0 { 1.0 } else { -1.0 };
            let slip = sign * (avg - mid) / mid * 10_000.0;
            (slip > budget).then_some(ImpactDeny::OverSlippageBudget)
        }
        _ => None,
    }
}

/// How much of the DISPLAYED book an order can take RIGHT NOW — the scope the impact veto is
/// allowed to judge it against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum TakeScope {
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

/// PURE: classify an order against the current book (see [`TakeScope`]). Only
/// [`OrderType::Market`] and a [`OrderType::Pegged`] order pegged to the FAR side of the touch
/// ([`OrderPegAnchor::Market`]: it sits at the opposing best, so it takes) are treated as unpriced
/// takers; every other peg is passive here, and a priced order takes only when it crosses the
/// opposing best. An empty opposing side ⇒ nothing to take ⇒ [`TakeScope::Passive`].
pub(super) fn take_scope(
    book: &L2Book,
    side: i32,
    order_type: &OrderType,
    price: Option<f64>,
    peg: Option<&OrderPeg>,
) -> TakeScope {
    let far_peg =
        *order_type == OrderType::Pegged && peg.is_some_and(|p| p.anchor == OrderPegAnchor::Market);
    if *order_type == OrderType::Market || far_peg {
        return TakeScope::Market;
    }
    let Some(px) = price else { return TakeScope::Passive };
    if px.is_nan() {
        return TakeScope::Passive;
    }
    // stop / take_profit carry a trigger, not a live marketable price — they fire against a book
    // nobody can see yet, so judging them on today's depth would be fiction. Every kind but
    // `Limit` (stop_limit and `Unrecognized` included) is passive here, as it always was.
    if *order_type != OrderType::Limit {
        return TakeScope::Passive;
    }
    let crosses = if side > 0 {
        book.best_ask().is_some_and(|a| px >= a.price)
    } else {
        book.best_bid().is_some_and(|b| px <= b.price)
    };
    if crosses { TakeScope::Crossing(px) } else { TakeScope::Passive }
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
pub(super) fn scoped_impact_veto(
    book: &L2Book,
    scope: TakeScope,
    side: i32,
    qty: f64,
    budget: Option<f64>,
    require_fillable: bool,
    value: ContractValue,
) -> Option<ImpactDeny> {
    match scope {
        TakeScope::Passive => None,
        TakeScope::Market => {
            if require_fillable && let Some(d) = fillable_veto(book, side, qty) {
                return Some(d);
            }
            impact_veto(book, side, qty, budget, value)
        }
        TakeScope::Crossing(px) => {
            let takeable = book.quantity_for_price(side, px);
            if require_fillable && takeable < qty {
                return Some(ImpactDeny::NotFillable);
            }
            budget?; // no budget ⇒ nothing left to judge (the depth floor ran above)
            // walk only what is takeable at the limit or better; that slice lies entirely
            // within the limit, so the walk over it IS the limit-constrained walk.
            let slice = if takeable < qty { takeable } else { qty };
            impact_veto(book, side, slice, budget, value)
        }
    }
}

/// PURE depth floor: deny when displayed liquidity cannot cover the full `qty`. Cheap
/// (`can_fill` walks without collecting fills). `qty <= 0` is vacuously fillable.
pub(super) fn fillable_veto(book: &L2Book, side: i32, qty: f64) -> Option<ImpactDeny> {
    if book.can_fill(side, qty) { None } else { Some(ImpactDeny::NotFillable) }
}

#[path = "impact_tests.rs"]
#[cfg(test)]
mod impact_tests;
