//! Small shared scalar primitives for order/price arithmetic — hoisted here (the common
//! vike-model ancestor) so vike-exec and vike-mm/vike-core stop each carrying their own copy,
//! while the down-only crate layering holds (vike-mm depends on vike-model ONLY, so it can't
//! reach into vike-exec's `risk::round_to`). No I/O; pure `f64`/`i32` math.

/// Round `value` onto a `step` grid using round-half-to-EVEN (banker's rounding):
/// `(value / step).round_ties_even() * step`.
///
/// PARITY: this is the scalar core of `exec/risk.py`'s `_round_to` — Python `round()` is
/// half-to-EVEN, hence `round_ties_even()` and **never** `round()` (which is half-away-from-zero
/// and would move every `.5` boundary). The oracle-parity-pinned `vike_exec::risk::round_to` and
/// the self-consistent `vike_mm::avellaneda::snap_to_tick` compute this identical expression;
/// both keep their own `step > 0.0` / `tick > 0.0` guard and delegate only the formula here.
///
/// This fn intentionally does NOT guard `step` — callers own that. A `0.0` step yields the raw
/// `inf * 0.0 = NaN` (or `NaN` for a `NaN` step), exactly as the inlined expression would.
#[inline]
pub fn round_to_step(value: f64, step: f64) -> f64 {
    (value / step).round_ties_even() * step
}

/// Guarded [`round_to_step`]: `None`, a `0.0` step, or a NEGATIVE step all fall through to
/// identity. This is the form every SymbolProperties-driven call site wants — hoisted from
/// `vike_exec::risk::round_to` (which re-exports it) so vike-backtest's `SimBroker` stops
/// reaching UP into vike-exec for four lines of arithmetic.
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
/// (`ComboSpec::net_limit`) and goes negative for a credit structure; a signed factor would make
/// a `notional < min_notional` floor spuriously trip (denying every credit combo) while a
/// `notional > cap` check could never trip — so an arbitrarily large credit combo would escape
/// the per-order cap entirely. The multiplier is part of the notional for the same reason it is
/// part of [`crate::margin::initial_margin`] and of `SimBroker::apply_fill`'s fill gate: omitting
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
/// fill moves), where the sign IS the answer. Stripping it would make `net_exposure` report gross,
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
/// hand-rolled site this replaces (`Account::gross_notional_priced`, `CoreSnapshot::
/// gross_exposure`, the liquidation notional folds) passes marks through unmodified. Wrapping them
/// in `.abs()` here would be a silent behavior change on any negative-price instrument, so the
/// hoist preserves exactly what was there.
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
/// [`is_implicit_reduce`] adds a coverage requirement, while `SimBroker::apply_fill`'s
/// opening/closing split is direction-ONLY (it executes a below-min flip whole). Naming the
/// shared half lets both share code without pretending they are the same rule.
#[inline]
pub fn is_reducing_direction(side: i32, position_size: f64) -> bool {
    side as f64 * position_size < 0.0
}

/// LEAN's pure-reduce predicate, DERIVED FROM STATE ONLY (no caller-asserted flag): the order
/// opposes a non-zero position AND that position covers it (`|position| >= |qty|`), i.e. the
/// order shrinks `abs(position)` without flipping through flat.
#[inline]
pub fn is_implicit_reduce(side: i32, position_size: f64, qty: f64) -> bool {
    is_reducing_direction(side, position_size) && position_size.abs() >= qty.abs()
}

/// The STRICTER reduce predicate the below-min FLOOR bypass uses: an implicit reduce, or an
/// explicit `reduce_only` flag on an order that BOTH opposes the position AND is covered by it.
///
/// The flag arm carries TWO requirements, both load-bearing — it must NOT be collapsed into a
/// plain `reduce_only || implicit`:
///
/// * COVERAGE (`|position| >= |qty|`): the flag alone is caller-asserted, whereas
///   `SimBroker::apply_fill` (the reference for the anti-stranding rule) never consults a flag and
///   derives closing-ness from the ACTUAL position. A flat-book "reduce_only" order is an OPENING
///   order and must stay floor-gated, or a buggy strategy tagging entries `reduce_only` would put
///   sub-floor opening orders on the wire.
/// * DIRECTION (`is_reducing_direction`): a `reduce_only`-tagged SAME-DIRECTION ADD — one that
///   INCREASES exposure but happens to be magnitude-covered (long 5, BUY 2 tagged reduce_only) —
///   is NOT a reduce. Without this guard the coverage check alone admitted it, laundering an
///   exposure-increasing order past the floor/margin/impact bypasses (a live-vs-backtest
///   asymmetry: `apply_fill`'s direction-only opening split floor-gates the very same order).
///   Real venues (binance/bybit) REJECT a reduce_only order that would increase the position, so
///   trusting the flag to reduce when the direction says it adds is a contradiction. With the
///   guard the flag arm implies `is_implicit_reduce`, so the two engines agree: the add faces the
///   opening-order checks on both sides.
#[inline]
pub fn is_covered_reduce(reduce_only: bool, side: i32, position_size: f64, qty: f64) -> bool {
    is_implicit_reduce(side, position_size, qty)
        || (reduce_only
            && is_reducing_direction(side, position_size)
            && position_size.abs() >= qty.abs())
}

/// The side (`+1` / `-1`) of the order that CLOSES a signed `position_size`: `-1` for a long
/// (`> 0.0`), `+1` for a short — and `+1` for a flat (`0.0`) position too. Matches the three
/// ported sites byte-for-byte (`Account::apply_liquidation`, the `margin_call` liquidation
/// intent, and the runtime `Flatten` intent), including the flat/`0.0 → +1` edge.
#[inline]
pub fn closing_side(position_size: f64) -> i32 {
    if position_size > 0.0 {
        -1
    } else {
        1
    }
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
    fn covered_reduce_requires_a_genuine_reduce_not_just_a_flag() {
        // A REVERSAL opposes the position but is NOT covered by it — neither predicate admits it.
        assert!(!is_implicit_reduce(-1, 1.0, 3.0));
        assert!(!is_covered_reduce(false, -1, 1.0, 3.0));
        // A capped flatten is BOTH.
        assert!(is_implicit_reduce(-1, 3.0, 1.0));
        assert!(is_covered_reduce(false, -1, 3.0, 1.0));
        // A `reduce_only` FLAG on a FLAT book is an OPENING order — the floor bypass stays gated.
        assert!(!is_implicit_reduce(1, 0.0, 2.0));
        assert!(!is_covered_reduce(true, 1, 0.0, 2.0), "flat + reduce_only flag is still opening");
        // The flag DOES bypass a GENUINE covered reduce — SELL 2 into a long 5 (reducing
        // direction, covered by the position).
        assert!(is_covered_reduce(true, -1, 5.0, 2.0));
        // ...but a `reduce_only`-tagged SAME-DIRECTION ADD (BUY 2 into a long 5) is NOT a reduce,
        // even though the position magnitude-covers it: the direction guard refuses it, so it is
        // treated as the opening order it is. The flag never launders an exposure-increasing
        // order past the bypass. (Pre-fix this was the latent asymmetry — the gate admitted it via
        // the coverage-only flag arm while `SimBroker::apply_fill` floor-gated it as an opening
        // fill; binance/bybit reject a reduce_only order that would increase the position.)
        assert!(
            !is_covered_reduce(true, 1, 5.0, 2.0),
            "same-direction add is not a covered reduce"
        );
        // and with the flag OFF it is likewise not a reduce — the guard makes the flag inert here,
        // so `is_covered_reduce` now agrees with `is_implicit_reduce` on this shape.
        assert!(!is_covered_reduce(false, 1, 5.0, 2.0));
    }

    #[test]
    fn reducing_direction_matches_the_sim_broker_opening_split() {
        // `SimBroker::apply_fill` derived `opening` as `p == 0.0 || (p > 0.0) == (side > 0)`.
        // `!is_reducing_direction` must reproduce it EXACTLY over the reachable domain
        // (side is always +/-1; position is finite) — this pins the hoist as behavior-preserving.
        for side in [1i32, -1] {
            for p in [0.0f64, -0.0, 1.0, -1.0, 1e-12, -1e-12, 1e18, -1e18] {
                let original = p == 0.0 || (p > 0.0) == (side > 0);
                assert_eq!(!is_reducing_direction(side, p), original, "side={side} p={p}");
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
    fn closing_side_flips_by_sign() {
        assert_eq!(closing_side(3.0), -1); // long closes with a sell
        assert_eq!(closing_side(-3.0), 1); // short closes with a buy
        assert_eq!(closing_side(0.0), 1); // flat -> +1 (matches the ported sites)
    }
}
