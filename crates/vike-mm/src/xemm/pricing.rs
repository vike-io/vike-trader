//! The PURE CORE of a cross-exchange market maker (xEMM) — ports the pricing arithmetic of
//! Hummingbot's cross-exchange market making strategy (Apache-2.0,
//! `hummingbot/strategy/cross_exchange_market_making`). The maker quotes on venue A (the "maker"
//! venue) at prices DERIVED from venue B's (the "taker" venue) touch plus a required edge, so that
//! any maker fill on A can be IMMEDIATELY hedged on B at a profit no smaller than that edge.
//!
//! Four pure functions and one value type, no state and no I/O — the whole price of one tick:
//! - [`xemm_maker_quotes`] turns venue B's `(bid, ask)` into the `(maker_bid, maker_ask)` to rest on
//!   venue A, backing each side off B's touch by the required profitability PLUS the round-trip fee.
//!   THE HEDGE IDENTITY LIVES HERE AND NOWHERE ELSE — every later stage may only move a quote AWAY
//!   from the reference touch, never toward it;
//! - [`hedge_qty`] sizes the taker hedge to fire on B after a maker fill on A;
//! - [`passive_clamp`] holds each side strictly INSIDE venue A's own touch, which is what keeps the
//!   maker leg passive across a persistent A-vs-B basis (its doc carries the module's one genuinely
//!   non-obvious decision, and the obvious alternative it rejects is a real trap);
//! - [`snap_down`]/[`snap_up`] put the two prices on venue A's grid DIRECTIONALLY, because the
//!   crate's usual half-to-even `vike_model::round_to_step` could round a clamped bid back UP
//!   through the clamp and make it marketable.
//!
//! House style: naïve f64 folds (no `mul_add`), `pub(crate)` pure fns, in-file `#[cfg(test)]`.
//! [`xemm_maker_quotes`]/[`hedge_qty`] and their four tests moved here VERBATIM from the former
//! `src/xemm.rs` when it was promoted to a module directory — that verbatim move IS the regression
//! proof for the promotion. The `#[allow(dead_code)]` those functions once needed is gone: the live
//! two-venue mount ([`crate::XemmMaker`]) now calls every item in this file.

/// Derive the two prices to REST on the maker venue (A) from the taker venue (B)'s touch, backed off
/// by the required edge plus the round-trip fee so a maker fill can always be hedged on B at a
/// profit `>= min_profitability`:
///
/// - `maker_bid = taker_bid · (1 − min_profitability − total_fee)` — buy on A strictly below B's
///   bid, so the position can be sold into B's bid for the edge;
/// - `maker_ask = taker_ask · (1 + min_profitability + total_fee)` — sell on A strictly above B's
///   ask, so the position can be bought back from B's ask for the edge.
///
/// `min_profitability` is the required edge and `total_fee` the round-trip cost (maker fee + taker
/// fee), both as fractions of price (e.g. `0.001` = 10 bps). They only ever ADD, so the maker spread
/// widens monotonically in each. With BOTH at `0.0` the maker quotes collapse onto the taker touch
/// exactly (`maker_bid == taker_bid`, `maker_ask == taker_ask`), bit-for-bit. Naïve folds, no
/// `mul_add`. PURE.
pub(crate) fn xemm_maker_quotes(
    taker_bid: f64,
    taker_ask: f64,
    min_profitability: f64,
    total_fee: f64,
) -> (f64, f64) {
    let maker_bid = taker_bid * (1.0 - min_profitability - total_fee);
    let maker_ask = taker_ask * (1.0 + min_profitability + total_fee);
    (maker_bid, maker_ask)
}

/// The size to taker-hedge on venue B after a maker fill on venue A: `net_filled_base · hedge_ratio`.
/// `net_filled_base` is the net base quantity just filled on the maker side (signed by the caller if
/// it tracks direction; magnitude here), `hedge_ratio` the fraction of it to offset on B (typically
/// `1.0` for a fully-hedged book; `< 1.0` leaves a deliberate residual inventory). Linear in both,
/// so a zero fill hedges nothing. PURE.
pub(crate) fn hedge_qty(net_filled_base: f64, hedge_ratio: f64) -> f64 {
    net_filled_base * hedge_ratio
}

/// One venue's last observed L1 touch, with the EVENT ts it was observed at — the maker's whole
/// view of a book. `Copy` so it is cheap to read out of `self` before a `&mut self` call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Touch {
    pub(crate) bid: f64,
    pub(crate) ask: f64,
    /// EVENT ts (epoch-ms) — never wall-clock, so every freshness bound is deterministic.
    pub(crate) ts: i64,
}

impl Touch {
    /// Midpoint. Only meaningful on a sane touch (see [`Touch::is_sane`]).
    pub(crate) fn mid(&self) -> f64 {
        0.5 * (self.bid + self.ask)
    }

    /// Both sides finite, strictly positive, and not crossed — the
    /// `vike_model::spread_quote::legs_are_sane` discipline: an unknowable book is never treated as
    /// a free one.
    pub(crate) fn is_sane(&self) -> bool {
        self.bid.is_finite()
            && self.ask.is_finite()
            && self.bid > 0.0
            && self.ask > 0.0
            && self.ask >= self.bid
    }
}

/// Clamp the hedge-guaranteed maker quotes so NEITHER SIDE IS EVER MARKETABLE on the maker venue:
///
/// ```text
///   maker_bid = min(raw_bid, a_ask − standoff)      maker_ask = max(raw_ask, a_bid + standoff)
/// ```
///
/// `raw_bid`/`raw_ask` come from [`xemm_maker_quotes`]; `a_touch` is the MAKER venue's own touch
/// (not the reference venue's); `standoff` is the minimum distance to hold inside that touch, in
/// price units (the caller computes it as `tick · min_edge_ticks`).
///
/// # Why `min`/`max` — strictly conservative on BOTH axes at once
///
/// `min`/`max` can only move a quote AWAY from the reference touch, so:
/// - the result is never above (bid) / below (ask) the price [`xemm_maker_quotes`] guaranteed
///   hedgeable, i.e. **the edge identity is preserved BY CONSTRUCTION** and `xemm_maker_quotes`
///   itself is untouched;
/// - the result is never through venue A's own touch, so the quote cannot be filled aggressively
///   and cannot be charged an unbudgeted TAKER fee. This is the substitute for the post-only that
///   **no roster venue supports** (`vike_model::venue_caps` pins `supports_post_only: false` on all
///   14 rows), and `vike_model::fees::xemm_round_trip_fee`'s maker-rate default depends on it.
///
/// # ⚠ WHY NOT A BASIS TERM IN THE PRICE (the trap this rejects)
///
/// The obvious fix for a persistent A-vs-B basis is to rebase the reference touch by an estimated
/// `mid_A/mid_B`. That **destroys the hedge identity**: with A 10 bp below B and `edge + fee` also
/// 10 bp, the rebased ask lands ON `ask_B`, so selling on A and buying back on B is a guaranteed
/// loss net of fees — and every such fill LOOKS hedged. Flooring the rebase back against the raw
/// quote restores the identity but pins the disadvantaged side at a price that can never fill,
/// leaving a one-sided accumulator quoting off an estimate whose error is unbounded between the
/// fill and the hedge. This clamp needs NO estimate at all: venue A's own book IS the ground truth
/// about where A trades, and the mount already receives it.
///
/// # CONSEQUENCE, stated so nobody reads it as a bug
///
/// On a pair whose persistent basis EXCEEDS the required edge, one side rests deep and inert while
/// the other sits at A's touch and fills. That is the economic truth of the pair, not a modelling
/// artifact — a two-sided xEMM across a wide basis is a fiction. `super::basis::BasisEwma`'s band is
/// what turns "this pair has no two-sided edge" into an operator-visible halt instead of a silent
/// accumulation.
///
/// # `None` — unknowable, never free
///
/// `None` on: no maker-venue touch at all (`a_touch == None`), an insane touch
/// ([`Touch::is_sane`]), a non-finite `raw_*`, a `standoff` that is not finite and strictly
/// positive (without a tick grid there is no price this function can certify non-marketable), or a
/// clamped result that came out CROSSED. It never silently degrades to the unclamped price — the
/// caller PULLS on `None`.
pub(crate) fn passive_clamp(
    raw_bid: f64,
    raw_ask: f64,
    a_touch: Option<Touch>,
    standoff: f64,
) -> Option<(f64, f64)> {
    let a = a_touch?;
    if !a.is_sane() || !raw_bid.is_finite() || !raw_ask.is_finite() {
        return None;
    }
    // A non-positive/non-finite standoff cannot certify anything: `min(raw_bid, a_ask)` sits AT the
    // ask and is marketable. Refuse rather than emit a quote whose passivity is unproven.
    if !(standoff.is_finite() && standoff > 0.0) {
        return None;
    }
    let bid = raw_bid.min(a.ask - standoff);
    let ask = raw_ask.max(a.bid + standoff);
    // A crossed result means the maker venue's own spread is narrower than twice the standoff (or
    // the reference is wildly dislocated) — there is no two-sided quote to place.
    if ask <= bid {
        return None;
    }
    Some((bid, ask))
}

/// Snap a BID down onto the `tick` grid — `(px / tick).floor() · tick`.
///
/// DIRECTIONAL on purpose. `vike_model::round_to_step` is half-to-EVEN, so it can round a clamped
/// bid UP; one such rounding puts the quote back at (or through) the maker venue's own ask and the
/// whole passivity argument of [`passive_clamp`] collapses. Flooring can only move the bid further
/// from the touch, which is the safe direction on both axes (still hedgeable, still passive).
///
/// A non-positive/non-finite `tick`, or a non-finite `px`, is IDENTITY — a maker on an unknown grid
/// emits the price it computed rather than a fabricated one. (`passive_clamp` has already refused
/// in that case, so this is belt-and-braces.)
///
/// FLOATING-POINT BOUNDARY NOTE, stated rather than engineered around: `px / tick` for a price that
/// is exactly on the grid can land a fraction below the integer, in which case `floor` drops it one
/// tick. That wobble is always in the SAFE direction on both sides — the bid rests one tick deeper,
/// the ask one tick higher, i.e. still hedgeable and still passive, only marginally less
/// competitive. Correcting it would need a relative epsilon, and an epsilon that can move a price
/// UP is exactly the hazard this function exists to remove.
pub(crate) fn snap_down(px: f64, tick: f64) -> f64 {
    if !(tick.is_finite() && tick > 0.0 && px.is_finite()) {
        return px;
    }
    (px / tick).floor() * tick
}

/// Snap an ASK up onto the `tick` grid — `(px / tick).ceil() · tick`. The mirror of
/// [`snap_down`]; see its doc for why the direction is load-bearing.
pub(crate) fn snap_up(px: f64, tick: f64) -> f64 {
    if !(tick.is_finite() && tick > 0.0 && px.is_finite()) {
        return px;
    }
    (px / tick).ceil() * tick
}

#[cfg(test)]
mod tests {
    use super::*;

    // A representative venue-B touch used across the pricing tests.
    const TAKER_BID: f64 = 100.0;
    const TAKER_ASK: f64 = 100.5;

    #[test]
    fn a_positive_edge_backs_the_maker_off_both_sides_of_the_taker_touch() {
        // With a required edge (and/or fee) the maker bids BELOW B's bid and asks ABOVE B's ask, so
        // either fill can be hedged into B at a profit.
        let (bid, ask) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.01, 0.0);
        assert!(bid < TAKER_BID, "maker_bid {bid} must sit below taker_bid {TAKER_BID}");
        assert!(ask > TAKER_ASK, "maker_ask {ask} must sit above taker_ask {TAKER_ASK}");
        // exact arithmetic: 100·0.99 and 100.5·1.01.
        assert_eq!(bid.to_bits(), (TAKER_BID * 0.99).to_bits());
        assert_eq!(ask.to_bits(), (TAKER_ASK * 1.01).to_bits());
    }

    #[test]
    fn edge_and_fee_widen_the_maker_spread_monotonically() {
        // Raising EITHER the required edge or the fee pushes the bid further down and the ask further
        // up — the maker spread only ever widens, never narrows.
        let (b0, a0) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.0, 0.0);
        let (b1, a1) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.01, 0.0);
        let (b2, a2) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.01, 0.005);
        assert!(b1 < b0 && a1 > a0, "a larger edge widens the spread");
        assert!(b2 < b1 && a2 > a1, "adding fee on top widens it further");
        // edge and fee enter symmetrically (they only ADD): 0.01 edge == 0.01 fee.
        let (b_fee, a_fee) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.0, 0.01);
        assert_eq!(b1.to_bits(), b_fee.to_bits(), "edge and fee are interchangeable on the bid");
        assert_eq!(a1.to_bits(), a_fee.to_bits(), "edge and fee are interchangeable on the ask");
    }

    #[test]
    fn zero_edge_and_zero_fee_reproduce_the_taker_touch_bit_for_bit() {
        // No required edge and no fee ⇒ the maker quotes ARE the taker touch, exactly.
        let (bid, ask) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.0, 0.0);
        assert_eq!(bid.to_bits(), TAKER_BID.to_bits(), "maker_bid collapses onto taker_bid");
        assert_eq!(ask.to_bits(), TAKER_ASK.to_bits(), "maker_ask collapses onto taker_ask");
    }

    #[test]
    fn hedge_qty_is_linear_and_zero_at_zero() {
        // ratio 1.0 hedges the whole fill; a fraction leaves a residual; zero fill hedges nothing.
        assert_eq!(hedge_qty(5.0, 1.0).to_bits(), 5.0_f64.to_bits(), "full hedge at ratio 1.0");
        assert_eq!(hedge_qty(5.0, 0.5).to_bits(), 2.5_f64.to_bits(), "half hedge at ratio 0.5");
        assert_eq!(hedge_qty(0.0, 1.0).to_bits(), 0.0_f64.to_bits(), "a zero fill hedges nothing");
        // linear: doubling the fill doubles the hedge.
        assert_eq!(hedge_qty(10.0, 0.5).to_bits(), (2.0 * hedge_qty(5.0, 0.5)).to_bits());
    }

    // --- passive_clamp -------------------------------------------------------------------------

    fn touch(bid: f64, ask: f64) -> Touch {
        Touch { bid, ask, ts: 0 }
    }

    /// THE §6 CORE CLAIM: the clamp can only move a quote AWAY from the reference touch, so the
    /// edge `xemm_maker_quotes` guaranteed always survives it. Swept over a grid of maker-venue
    /// touch positions — far below the reference, straddling it, far above — because the whole
    /// point is that it holds for ANY basis, including a basis wider than the edge.
    #[test]
    fn passive_clamp_never_returns_a_quote_worse_than_the_hedge_guaranteed_price() {
        let (raw_bid, raw_ask) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.001, 0.00065);
        for a_mid in [80.0, 95.0, 99.0, 100.25, 101.0, 105.0, 130.0] {
            let a = touch(a_mid - 0.05, a_mid + 0.05);
            let Some((bid, ask)) = passive_clamp(raw_bid, raw_ask, Some(a), 0.01) else {
                continue; // a refusal is always safe; this test is about what it DOES return
            };
            assert!(bid <= raw_bid, "a_mid {a_mid}: bid {bid} rose above the hedgeable {raw_bid}");
            assert!(ask >= raw_ask, "a_mid {a_mid}: ask {ask} fell below the hedgeable {raw_ask}");
        }
    }

    /// The other half of the same sweep: the clamped quote is never MARKETABLE on the maker venue —
    /// which is what stands in for the post-only no roster venue supports, and what makes the
    /// maker-rate assumption in `xemm_round_trip_fee` defensible.
    #[test]
    fn passive_clamp_never_returns_a_marketable_quote() {
        let (raw_bid, raw_ask) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.001, 0.00065);
        for a_mid in [80.0, 95.0, 99.0, 100.25, 101.0, 105.0, 130.0] {
            let a = touch(a_mid - 0.05, a_mid + 0.05);
            let Some((bid, ask)) = passive_clamp(raw_bid, raw_ask, Some(a), 0.01) else {
                continue;
            };
            assert!(bid < a.ask, "a_mid {a_mid}: bid {bid} would lift the maker venue's own ask");
            assert!(ask > a.bid, "a_mid {a_mid}: ask {ask} would hit the maker venue's own bid");
            assert!(
                bid <= a.ask - 0.01 && ask >= a.bid + 0.01,
                "a_mid {a_mid}: the standoff must be honoured, not merely the strict inequality"
            );
        }
    }

    /// THE HYPERLIQUID SHAPE, explicitly: venue A sits persistently BELOW venue B (the measured
    /// HYPE basis). The bid is pinned at A's own touch minus the standoff — where it can genuinely
    /// fill — while the ask stays out at the hedge-guaranteed price, deep and inert. That
    /// one-sidedness is the economic truth of the pair, and this test pins it as INTENDED rather
    /// than leaving it to be rediscovered as a bug.
    #[test]
    fn a_below_b_pins_the_bid_at_a_s_touch_and_leaves_the_ask_inert() {
        // Reference (B) at 100.0/100.5; maker venue (A) 1% below at 99.0/99.1.
        let (raw_bid, raw_ask) = xemm_maker_quotes(100.0, 100.5, 0.001, 0.0);
        let a = touch(99.0, 99.1);
        let (bid, ask) = passive_clamp(raw_bid, raw_ask, Some(a), 0.01).expect("a sane touch");
        assert_eq!(
            bid.to_bits(),
            (99.1_f64 - 0.01).to_bits(),
            "the raw bid ({raw_bid}) is ABOVE A's ask, so the clamp pins it one standoff inside"
        );
        assert_eq!(
            ask.to_bits(),
            raw_ask.to_bits(),
            "the ask is already far above A's bid, so the clamp leaves it at the hedgeable price"
        );
        assert!(ask > a.ask, "and it therefore rests deep — inert until the basis closes");
    }

    /// No maker-venue touch ⇒ `None`, never a silently unclamped quote. The caller pulls; a maker
    /// that cannot see its own book has no way to know whether its quote would cross.
    #[test]
    fn no_maker_touch_returns_none() {
        let (raw_bid, raw_ask) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.001, 0.0);
        assert_eq!(passive_clamp(raw_bid, raw_ask, None, 0.01), None);
    }

    /// Unknowable is never free: a crossed/zero/non-finite maker touch, a non-finite raw price, a
    /// non-positive standoff, and a clamped result that came out crossed all REFUSE.
    #[test]
    fn insane_inputs_and_a_crossed_result_return_none() {
        let (rb, ra) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.001, 0.0);
        assert_eq!(passive_clamp(rb, ra, Some(touch(101.0, 100.0)), 0.01), None, "crossed touch");
        assert_eq!(passive_clamp(rb, ra, Some(touch(0.0, 100.0)), 0.01), None, "zero bid");
        assert_eq!(passive_clamp(rb, ra, Some(touch(f64::NAN, 100.0)), 0.01), None, "NaN bid");
        assert_eq!(passive_clamp(f64::NAN, ra, Some(touch(99.0, 99.1)), 0.01), None, "NaN raw bid");
        assert_eq!(passive_clamp(rb, ra, Some(touch(99.0, 99.1)), 0.0), None, "no standoff");
        assert_eq!(passive_clamp(rb, ra, Some(touch(99.0, 99.1)), -1.0), None, "negative standoff");
    }

    /// The `ask > bid` guard exists for TOTALITY, not for a reachable case: with a sane touch and a
    /// positive standoff the clamp can only WIDEN, so a wide standoff produces a wider quote rather
    /// than an inverted one. Pinned so a future edit that makes the clamp narrow anything is caught
    /// here rather than by an inverted live order.
    #[test]
    fn a_wide_standoff_widens_rather_than_inverting() {
        let (rb, ra) = xemm_maker_quotes(TAKER_BID, TAKER_ASK, 0.001, 0.0);
        // A standoff far wider than the maker venue's own 2-cent spread.
        let (bid, ask) = passive_clamp(rb, ra, Some(touch(100.0, 100.02)), 5.0).expect("widened");
        assert!(ask > bid, "the clamp widens: {bid} / {ask}");
        assert_eq!(bid.to_bits(), (100.02_f64 - 5.0).to_bits(), "bid pushed a full standoff down");
        assert_eq!(ask.to_bits(), (100.0_f64 + 5.0).to_bits(), "ask pushed a full standoff up");
    }

    // --- directional snapping ------------------------------------------------------------------

    /// THE HALF-EVEN TRAP: `vike_model::round_to_step` is half-to-even, so a clamped bid sitting
    /// just under a grid line rounds UP through the clamp and becomes marketable. `snap_down`
    /// cannot: it only ever moves the bid further from the touch.
    #[test]
    fn snap_down_never_rounds_up_through_the_clamp() {
        // 99.096 is 0.004 under the 99.10 line: half-even rounds it UP to 99.10 == A's ask.
        let a = touch(99.0, 99.10);
        let clamped = 99.096_f64;
        assert!(clamped < a.ask, "precondition: the clamped bid is inside A's ask");
        assert!(
            vike_model::round_to_step(clamped, 0.01) >= a.ask,
            "precondition: the crate's half-even snap lands AT OR ABOVE A's ask — the trap"
        );
        let snapped = snap_down(clamped, 0.01);
        assert!(snapped < a.ask, "snap_down keeps the bid strictly inside: {snapped} vs {}", a.ask);
        assert!(snapped <= clamped, "and never moves it toward the touch");
    }

    /// The ask mirror, plus the shared degenerate contract: a non-positive/non-finite grid, or a
    /// non-finite price, is IDENTITY rather than a fabricated value.
    #[test]
    fn snap_up_mirrors_it_and_a_degenerate_grid_is_identity() {
        assert!(snap_up(99.104, 0.01) >= 99.104, "the ask only ever moves up");
        assert_eq!(snap_up(99.104, 0.01).to_bits(), snap_up(99.11, 0.01).to_bits());
        for tick in [0.0, -0.01, f64::NAN, f64::INFINITY] {
            assert_eq!(snap_down(99.096, tick).to_bits(), 99.096_f64.to_bits(), "tick {tick}");
            assert_eq!(snap_up(99.104, tick).to_bits(), 99.104_f64.to_bits(), "tick {tick}");
        }
        assert!(snap_down(f64::NAN, 0.01).is_nan(), "a NaN price passes through untouched");
    }

    /// A price already ON the grid is unmoved by either direction — so a well-configured maker
    /// pays no snapping drift at all.
    #[test]
    fn an_on_grid_price_is_unmoved_in_either_direction() {
        assert_eq!(snap_down(99.10, 0.01).to_bits(), snap_up(99.10, 0.01).to_bits());
    }
}
