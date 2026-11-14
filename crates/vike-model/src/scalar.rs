//! Small shared scalar primitives for order/price arithmetic — hoisted here (the common
//! vike-model ancestor) so vike-execution and vike-market-making/vike-core stop each carrying their own copy,
//! while the down-only crate layering holds (vike-market-making depends on vike-model ONLY, so it can't
//! reach into vike-execution's `risk::round_to`). No I/O; pure `f64`/`i32` math.

/// Round `value` onto a `step` grid using round-half-to-EVEN (banker's rounding): the count
/// `(value / step).round_ties_even()`, read back as a price by
/// [`vike_marketdata::TickFraction`], the rule `L2Book` reads its levels by. So an order snapped
/// here and the book's level at the same tick are the same number: 35 ticks of 0.01 is 0.35, where
/// `count * step` was 0.35000000000000003 (owner ruling 2026-11-13, P5.1 point 2).
///
/// Half-to-EVEN, hence `round_ties_even()` and **never** `round()` (which is half-away-from-zero
/// and would move every `.5` boundary); that half of the old `exec/risk.py` `_round_to` parity
/// stands. [`round_to`] and the self-consistent `vike_market_making::avellaneda::snap_to_tick` compute
/// through here; both keep their own `step > 0.0` / `tick > 0.0` guard.
///
/// This fn intentionally does NOT guard `step` — callers own that. A `0.0` step yields the raw
/// `inf * 0.0 = NaN` (or `NaN` for a `NaN` step), and a negative step the plain product, exactly
/// as `count * step` did ([`vike_marketdata::TickFraction`]'s third rule).
#[inline]
pub fn round_to_step(value: f64, step: f64) -> f64 {
    vike_marketdata::TickFraction::new(step).price_of((value / step).round_ties_even())
}

/// Guarded [`round_to_step`]: `None`, a `0.0` step, or a NEGATIVE step all fall through to
/// identity. This is the form every SymbolProperties-driven call site wants — hoisted from
/// `vike_execution::risk` (PR #481) so vike-simulator's `OrderBrokerSimulator` stops
/// reaching UP into vike-execution for four lines of arithmetic.
///
/// PARITY: the `step > 0.0` guard is load-bearing and is NOT the same as calling
/// [`round_to_step`] directly — [`nz_step`] admits a negative step (it only rejects `0.0`), and
/// a negative step here must round to identity, not compute.
#[inline]
pub fn round_to(value: f64, step: Option<f64>) -> f64 {
    match step {
        Some(s) if s > 0.0 => round_to_step(value, s), // Python round() = half-even
        _ => value,
    }
}

/// A level this many ticks off the grid counts as on it: a level a percentage lands exactly on the
/// grid may come out of f64 a few ulps past it, and without this it would snap a whole tick away.
const SNAP_GUARD_TICKS: f64 = 1e-9;

/// A raw level keeps four decimals of a tick, cut toward zero, before it snaps, so a level within a
/// ten-thousandth of a tick of the grid stays on it and one further off snaps, at most one tick.
/// MEASURED only at a 0.01 tick (ETHUSDT), where it is 6 decimals of price, and picked by 12 rows of
/// the strategy lab's set A, with three other readings fitting those rows equally well; at any other
/// tick it is an assumption with no reference list behind it. The owner ruled it ONE rule wherever an
/// order runs (decision 0269, row 27), so an order's price never depends on where it runs.
const TICK_CUT: f64 = 10_000.0;

/// Snapshot a STOP's level onto the `tick` grid by its side: a buy stop UP, a sell stop DOWN, so it never
/// fires before the level stated. [`round_to`] rounds to nearest, which is not this. `None`, a `0.0`
/// or a negative tick is identity, as [`round_to`]'s guard is. A stop-limit's TRIGGER snaps as a
/// limit ([`snap_limit`]), and its limit too.
#[inline]
pub fn snap_stop(side: i32, price: f64, tick: Option<f64>) -> f64 {
    snap_by_side(price, tick, side > 0)
}

/// Snapshot a LIMIT's level onto the `tick` grid by its side: a buy limit DOWN, a sell limit UP, so it
/// never pays more than the level stated. The guard and the cut are [`snap_stop`]'s.
#[inline]
pub fn snap_limit(side: i32, price: f64, tick: Option<f64>) -> f64 {
    snap_by_side(price, tick, side <= 0)
}

/// The one side-directed snap: the level in ticks, cut ([`TICK_CUT`]) with the guard added first,
/// then ceiled or floored with the guard. The guard goes in first, or f64 noise takes a level on the
/// grid under its line and a down-snap then loses a tick: a plain cut does that to 8,356 of the
/// 400,001 exact prices from 1000 to 5000 at 0.01. It grows with the level once f64's own noise
/// outgrows [`SNAP_GUARD_TICKS`], past about 1.1e6 ticks. A tick that divides one unit of price (0.01,
/// 0.25) counts the ticks in one unit; any other tick (5, 0.3) counts in the tick. The snapped count
/// reads back through [`vike_marketdata::TickFraction`], so a snapped level is the double nearest
/// its decimal value (3 ticks of 0.01 is 0.03, not 0.030000000000000002; 3 ticks of 0.3 is 0.9).
///
/// shortcut: past about 1e10 ticks that noise reaches the decimals kept, so a level 3 ulps off the
/// grid can lose a tick (exact grid prices still hold); no list is near it, and only decimal
/// arithmetic would cure it.
fn snap_by_side(price: f64, tick: Option<f64>, up: bool) -> f64 {
    let Some(tick) = tick.filter(|t| *t > 0.0) else {
        return price;
    };
    let in_one = (1.0 / tick).round();
    let divides = in_one >= 1.0 && (in_one * tick - 1.0).abs() < 1e-9;
    let ticks = if divides { price * in_one } else { price / tick };
    let guard = SNAP_GUARD_TICKS.max(ticks.abs() * f64::EPSILON * 4.0);
    let cut = ((ticks + guard) * TICK_CUT).trunc() / TICK_CUT;
    let snapped = if up { (cut - guard).ceil() } else { (cut + guard).floor() };
    vike_marketdata::TickFraction::new(tick).price_of(snapped)
}

/// `qty` floored to a whole number of `step`s, as a venue's quantity filter takes a size worked out from money. A step that is
/// one over a whole number `n` (0.00001, 0.5, 1) counts by multiplying by `n`: `qty · n` rounds before the floor, so a `qty` a
/// few ulps under a whole count lands on it. Any other step (5, 0.3) counts by dividing by itself. The count reads back through
/// [`vike_marketdata::TickFraction`], so the answer is the double nearest its decimal, which the count times the step,
/// rounded twice, can miss. A step that is not a positive number leaves `qty` as it is.
pub fn floor_to_step(qty: f64, step: f64) -> f64 {
    if step.is_nan() || step <= 0.0 {
        return qty;
    }
    let per_unit = (1.0 / step).round();
    let count = if per_unit >= 1.0 && (per_unit * step - 1.0).abs() <= f64::EPSILON {
        (qty * per_unit).floor()
    } else {
        (qty / step).floor()
    };
    vike_marketdata::TickFraction::new(step).price_of(count)
}

/// The quantity that spends `share` of `equity` with its commission: `(share · equity − per_order) / (unit · (1 + rate) +
/// per_unit)`, `unit` being what one unit of quantity is worth at the entry's price
/// ([`crate::ContractValue::unit_value`]), `rate` the commission as a fraction of the value, `per_unit` per unit of
/// quantity and `per_order` per order, both in the money of `equity`. A zero fee subtracts, scales and adds nothing (an
/// IEEE no-op on each term), so with none it is `share · equity / unit` bit for bit; `0.0` when the denominator is.
/// The one percent-of-equity law: `vike_analytics::sizing::PositionSizerPercentEquity` and `vike_tactics::TacticsSizing` both size by it.
#[inline]
pub fn share_quantity(
    share: f64,
    equity: f64,
    unit: f64,
    rate: f64,
    per_unit: f64,
    per_order: f64,
) -> f64 {
    let denominator = unit * (1.0 + rate) + per_unit;
    if denominator != 0.0 { (share * equity - per_order) / denominator } else { 0.0 }
}

/// The `SymbolProperties` → [`round_to`] convention: `0.0` means UNCONSTRAINED, so it becomes
/// `None`; every other value (including a nonsensical negative, which [`round_to`]'s own guard
/// then neutralizes) passes through as `Some`.
#[inline]
pub fn nz_step(x: f64) -> Option<f64> {
    (x != 0.0).then_some(x)
}

/// Order notional as a MAGNITUDE: `|qty| · |ref_price| · |multiplier|`.
///
/// All three factors enter ABSOLUTE, deliberately. A COMBO's price is the SIGNED net limit
/// (`OrderCombo::net_limit`) and goes negative for a credit structure; a signed factor would make
/// a `notional < min_notional` floor spuriously trip (denying every credit combo) while a
/// `notional > cap` check could never trip — so an arbitrarily large credit combo would escape
/// the per-order cap entirely. The multiplier is part of the notional for the same reason it is
/// part of [`crate::money::margin::initial_margin`] and of `OrderBrokerSimulator::apply_fill`'s fill gate: omitting
/// it made the same floor judge multiplier != 1 instruments (options, inverse perps) differently
/// live vs backtest. `multiplier` is 1.0 for ordinary instruments, and `x * 1.0` is an IEEE-754
/// no-op, so those notionals are bit-identical either way.
#[inline]
pub fn order_notional(qty: f64, ref_price: f64, multiplier: f64) -> f64 {
    qty.abs() * ref_price.abs() * multiplier.abs()
}

/// SIGNED position notional: `size · price · multiplier`, sign and all — a long contributes `+`,
/// a short `−`, so a hedged pair NETS to (near) zero.
///
/// This is the THIRD notional shape in the workspace and it is deliberately NOT
/// [`order_notional`]: this one values a HELD position (net exposure, equity marks, the cash a
/// fill moves), where the sign IS the answer. Stripping it would make a net exposure report gross,
/// `equity_now` count a short as a credit, and `cash -= delta · price · mult` move cash the wrong
/// way on a sell.
///
/// The factor ORDER is load-bearing: f64 multiplication is commutative but NOT associative, so
/// this computes `(size · price) · multiplier` exactly as every hand-rolled site it replaces did.
/// A `1.0` multiplier is an IEEE-754 no-op, so ordinary instruments are bit-identical either way.
#[inline]
pub fn signed_notional(size: f64, price: f64, multiplier: f64) -> f64 {
    size * price * multiplier
}

/// GROSS position notional: `|size| · price · multiplier` — magnitude in the SIZE ONLY.
///
/// The SECOND notional shape, and the one most easily confused with the other two. It differs
/// from [`signed_notional`] in stripping the position's DIRECTION (a hedged pair sums to its two
/// legs' notionals, never zero — the gross-exposure / margin-base / notional-at-risk fold) and
/// from [`order_notional`] in leaving `price` and `multiplier` SIGNED. That asymmetry is not an
/// oversight: [`order_notional`] prices an ORDER whose reference price may be a signed combo net
/// limit, whereas the price feeding a position fold is a venue MARK — already positive — and every
/// hand-rolled site this replaces (`Account::gross_notional_priced`, the liquidation notional
/// folds) passes marks through unmodified. Wrapping them in `.abs()` here would be a silent
/// behavior change on any negative-price instrument, so the hoist preserves exactly what was there.
///
/// Factor order is load-bearing for the same reason as [`signed_notional`]:
/// `(|size| · price) · multiplier`.
#[inline]
pub fn gross_notional(size: f64, price: f64, multiplier: f64) -> f64 {
    size.abs() * price * multiplier
}

/// The DIRECTION half of the reduce predicate: the order pushes against a non-zero position
/// (`side · position_size < 0.0`). Says nothing about whether the position COVERS the order —
/// a reversal that flips through flat satisfies this too.
///
/// This is deliberately its own fn because the two consumers genuinely differ:
/// [`is_covered_reduce`] adds a coverage requirement, while `OrderBrokerSimulator::apply_fill`'s
/// opening/closing split is direction-ONLY (a flip is its closing fill, and only a below-min one is
/// cut down to the close). Naming the
/// shared half lets both share code without pretending they are the same rule.
#[inline]
pub fn is_reducing_direction(side: i32, position_size: f64) -> bool {
    side as f64 * position_size < 0.0
}

/// The pure-reduce predicate (LEAN's) every bypass of the gate reads, DERIVED FROM STATE ONLY: the
/// order opposes a non-zero position AND that position covers it, i.e. it shrinks `abs(position)`
/// without flipping through flat.
///
/// It takes no `reduce_only` flag, and that is the rule, not an omission. The flag is
/// caller-asserted; it may not widen this predicate, and both requirements stand with or without it:
///
/// * COVERAGE (`|position| >= |qty|`): `OrderBrokerSimulator::apply_fill` (the reference for the anti-stranding
///   rule) never consults a flag and derives closing-ness from the ACTUAL position. A flat-book
///   "reduce_only" order is an OPENING order and must stay floor-gated, or a buggy strategy tagging
///   entries `reduce_only` would put sub-floor opening orders on the wire.
/// * DIRECTION (`is_reducing_direction`): a `reduce_only`-tagged SAME-DIRECTION ADD — one that
///   INCREASES exposure but happens to be magnitude-covered (long 5, BUY 2 tagged reduce_only) —
///   is NOT a reduce. Without this guard the coverage check alone admitted it, laundering an
///   exposure-increasing order past the floor/margin/impact bypasses (a live-vs-backtest
///   asymmetry: `apply_fill`'s direction-only opening split floor-gates the very same order).
///   Real venues (binance/bybit) REJECT a reduce_only order that would increase the position, so
///   trusting the flag to reduce when the direction says it adds is a contradiction.
#[inline]
pub fn is_covered_reduce(side: i32, position_size: f64, qty: f64) -> bool {
    is_reducing_direction(side, position_size) && position_size.abs() >= qty.abs()
}

/// The side (`+1` / `-1`) of the order that CLOSES a signed `position_size`: `-1` for a long
/// (`> 0.0`), `+1` for a short — and `+1` for a flat (`0.0`) position too. Matches the three
/// ported sites byte-for-byte (`Account::apply_liquidation`, the `margin_call` liquidation
/// intent, and the runtime `Flatten` intent), including the flat/`0.0 → +1` edge.
#[inline]
pub fn closing_side(position_size: f64) -> i32 {
    if position_size > 0.0 { -1 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_to_step_is_half_to_even() {
        // The load-bearing parity property: ties round to the nearest EVEN multiple
        // (Python round() / round_ties_even), NOT half-away-from-zero.
        assert_eq!(round_to_step(0.5, 1.0), 0.0);
        assert_eq!(round_to_step(1.5, 1.0), 2.0);
        assert_eq!(round_to_step(2.5, 1.0), 2.0);
        assert_eq!(round_to_step(3.5, 1.0), 4.0);
        // same property on a 0.5 grid (every intermediate value is exact in f64)
        assert_eq!(round_to_step(2.25, 0.5), 2.0); // 4.5 -> 4 (even)
        assert_eq!(round_to_step(2.75, 0.5), 3.0); // 5.5 -> 6 (even)
    }

    #[test]
    fn covered_reduce_requires_direction_and_coverage() {
        // A REVERSAL opposes the position but is NOT covered by it.
        assert!(!is_covered_reduce(-1, 1.0, 3.0));
        // A capped flatten, and SELL 2 into a long 5, oppose it and are covered.
        assert!(is_covered_reduce(-1, 3.0, 1.0));
        assert!(is_covered_reduce(-1, 5.0, 2.0));
        // A FLAT book covers nothing: an order on it is OPENING however it is tagged.
        assert!(!is_covered_reduce(1, 0.0, 2.0), "flat is still opening");
        // A SAME-DIRECTION ADD (BUY 2 into a long 5) is NOT a reduce, even though the position
        // magnitude-covers it: the direction half refuses it, so it is the opening order it is.
        // (This was the latent asymmetry: the gate admitted it while `OrderBrokerSimulator::apply_fill`
        // floor-gated it as an opening fill; binance/bybit reject a reduce_only order that would
        // increase the position.)
        assert!(!is_covered_reduce(1, 5.0, 2.0), "same-direction add is not a covered reduce");
    }

    #[test]
    fn reducing_direction_matches_every_spelling_it_replaced() {
        // Each form was a hand-spelled site. `is_reducing_direction` must reproduce it EXACTLY over
        // the reachable domain (side is always +/-1; position is finite): this pins the hoist as
        // behavior-preserving. Outside that domain the forms part ways (a side of 0, a NaN size).
        for side in [1i32, -1] {
            for p in [0.0f64, -0.0, 1.0, -1.0, 1e-12, -1e-12, 1e18, -1e18] {
                let reducing = is_reducing_direction(side, p);
                let opening = p == 0.0 || (p > 0.0) == (side > 0); // the sim's opening splits
                assert_eq!(!reducing, opening, "side={side} p={p}");
                let closing = p != 0.0 && (p > 0.0) != (side > 0); // `cap_to_leverage`, the min-hold
                assert_eq!(reducing, closing, "side={side} p={p}");
                let credited = p != 0.0 && (side as f64 * p) < 0.0; // `reduces`, the reversal credits
                assert_eq!(reducing, credited, "side={side} p={p}");
                let door = p != 0.0 && (side > 0) == (p < 0.0); // the control door's Modify reads
                assert_eq!(reducing, door, "side={side} p={p}");
            }
        }
    }

    #[test]
    fn order_notional_is_a_magnitude() {
        assert_eq!(order_notional(2.0, 3.0, 1.0), 6.0);
        // a credit combo's SIGNED net limit must still yield a positive notional, or the floor
        // trips spuriously while the cap can never trip
        assert_eq!(order_notional(2.0, -3.0, 1.0), 6.0);
        assert_eq!(order_notional(-2.0, 3.0, -1.0), 6.0);
        // multiplier 1.0 is an IEEE-754 no-op, so ordinary instruments are bit-identical
        assert_eq!(order_notional(0.1, 0.2, 1.0), 0.1f64.abs() * 0.2f64.abs());
    }

    #[test]
    fn the_three_notional_shapes_are_genuinely_different() {
        // A SHORT position (size < 0) at a positive mark is where all three part ways — the exact
        // divergence that makes flattening them into one call a silent risk-arithmetic bug.
        assert_eq!(signed_notional(-2.0, 3.0, 1.0), -6.0); // net exposure: direction kept
        assert_eq!(gross_notional(-2.0, 3.0, 1.0), 6.0); // gross exposure: direction stripped
        assert_eq!(order_notional(-2.0, 3.0, 1.0), 6.0); // order gate: magnitude

        // A NEGATIVE price (a credit combo's signed net limit) is where the remaining two part
        // ways: only the order gate strips the price's sign.
        assert_eq!(gross_notional(2.0, -3.0, 1.0), -6.0);
        assert_eq!(order_notional(2.0, -3.0, 1.0), 6.0);
        // A negative multiplier likewise survives both position folds and dies in the order gate.
        assert_eq!(signed_notional(2.0, 3.0, -1.0), -6.0);
        assert_eq!(gross_notional(2.0, 3.0, -1.0), -6.0);
        assert_eq!(order_notional(2.0, 3.0, -1.0), 6.0);
    }

    #[test]
    fn notional_helpers_keep_left_to_right_association() {
        // f64 multiplication is NOT associative, so the hoisted helpers must fold in the same
        // order the hand-rolled sites did — `(size · price) · multiplier`, never `size · (price ·
        // multiplier)`. These operands are chosen so the two groupings genuinely disagree.
        let (s, p, m) = (0.1_f64, 0.2_f64, 0.3_f64);
        assert_eq!(signed_notional(s, p, m), (s * p) * m);
        assert_eq!(gross_notional(s, p, m), (s.abs() * p) * m);
        assert_eq!(order_notional(s, p, m), (s.abs() * p.abs()) * m.abs());
    }

    #[test]
    fn nz_step_and_round_to_guard_together() {
        assert_eq!(nz_step(0.0), None);
        assert_eq!(nz_step(0.5), Some(0.5));
        assert_eq!(round_to(1.23, None), 1.23); // unconstrained -> identity
        assert_eq!(round_to(1.23, Some(0.0)), 1.23); // 0 step -> identity, NOT NaN
        // nz_step admits a negative; round_to's own `step > 0.0` guard neutralizes it
        assert_eq!(round_to(1.23, nz_step(-0.5)), 1.23);
        assert_eq!(round_to(2.5, Some(1.0)), 2.0); // still half-to-EVEN
    }

    #[test]
    fn a_stop_snaps_away_from_the_market_and_a_limit_toward_its_own_side() {
        let t = Some(0.01);
        assert_eq!(snap_stop(1, 1234.561, t), 1234.57, "a buy stop rounds up");
        assert_eq!(snap_stop(-1, 1234.569, t), 1234.56, "a sell stop rounds down");
        assert_eq!(snap_limit(1, 1234.569, t), 1234.56, "a buy limit rounds down");
        assert_eq!(snap_limit(-1, 1234.561, t), 1234.57, "a sell limit rounds up");
        // a level on the grid stays, either side, either kind; three ticks of 0.01 is the double 0.03
        for side in [1, -1] {
            assert_eq!(snap_stop(side, 1234.56, t), 1234.56);
            assert_eq!(snap_limit(side, 1234.56, t), 1234.56);
            assert_eq!(snap_stop(side, 0.03, t), 0.03);
        }
        // 1.1 × 100 is 110 on paper and 110.00000000000001 in f64: the guard and the cut keep a level
        // a few ulps past the grid from snapping a whole tick away
        assert_eq!(1.1 * 100.0, 110.00000000000001);
        assert_eq!(snap_stop(1, 1.1 * 100.0, t), 110.0);
        assert_eq!(snap_limit(-1, 1.1 * 100.0, t), 110.0);
    }

    #[test]
    fn the_cut_keeps_four_decimals_of_a_tick() {
        let t = Some(0.01);
        // 1e-5 of a tick over the grid is cut away: it stays on the grid
        assert_eq!(snap_stop(1, 100.0000001, t), 100.0);
        // 1e-3 of a tick over survives the cut: a buy stop moves up one tick, no more
        assert_eq!(snap_stop(1, 100.00001, t), 100.01);
        assert_eq!(snap_stop(-1, 100.00999, t), 100.0);
    }

    #[test]
    fn a_tick_that_does_not_divide_one_unit_snaps_in_the_tick_itself() {
        let five = Some(5.0);
        assert_eq!(snap_stop(1, 101.0, five), 105.0);
        assert_eq!(snap_stop(-1, 101.0, five), 100.0);
        assert_eq!(snap_limit(1, 104.0, five), 100.0);
        assert_eq!(snap_limit(-1, 100.0, five), 100.0);
        let quarter = Some(0.25);
        assert_eq!(snap_stop(1, 10.1, quarter), 10.25);
        assert_eq!(snap_limit(1, 10.1, quarter), 10.0);
    }

    #[test]
    fn no_tick_is_identity() {
        for tick in [None, Some(0.0), Some(-0.01)] {
            assert_eq!(snap_stop(1, 1234.567, tick), 1234.567);
            assert_eq!(snap_limit(-1, 1234.567, tick), 1234.567);
        }
    }

    #[test]
    fn closing_side_flips_by_sign() {
        assert_eq!(closing_side(3.0), -1); // long closes with a sell
        assert_eq!(closing_side(-3.0), 1); // short closes with a buy
        assert_eq!(closing_side(0.0), 1); // flat -> +1 (matches the ported sites)
    }
}
