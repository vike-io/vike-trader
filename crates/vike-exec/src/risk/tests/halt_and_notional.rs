//! The min floors, the HALTED/reduce-only ladder, and notional with the contract multiplier.

use super::*;

#[test]
fn check_rejects_below_min_qty() {
    let mut gate = RiskGate::new(RiskLimits { min_qty: Some(1.0), ..RiskLimits::new() });
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };

    let verdict = gate.check(&market(1, 0.5), &ctx);
    assert!(!verdict.ok && verdict.reason == "below-min-qty", "got {:?}", verdict);

    let verdict_ok = gate.check(&market(1, 1.0), &ctx);
    assert!(verdict_ok.ok || verdict_ok.reason != "below-min-qty", "got {:?}", verdict_ok);
}

/// A covered reduce/close below `min_qty` is EXEMPT, as `SimBroker::apply_fill` fills it in
/// backtest ("a closing fill must ALWAYS execute so a position is never stranded below-min").
/// A NON-reducing below-min order still denies.
#[test]
fn pure_reduce_dust_flatten_passes_min_qty_gate() {
    let lim = || RiskLimits { min_qty: Some(0.01), ..RiskLimits::new() };
    // long 0.005 — dust below the 0.01 floor — flattened with a sell of 0.005
    let long_dust =
        RiskContext { mark_price: 100.0, position_size: 0.005, ..RiskContext::default() };
    let mut req = market(-1, 0.005);
    req.reduce_only = true;
    let v = RiskGate::new(lim()).check(&req, &long_dust);
    assert!(v.ok, "an explicit reduce_only dust flatten must pass the min-qty floor: {v:?}");
    // the implicit form (opposite a position that fully covers it) too
    let v = RiskGate::new(lim()).check(&market(-1, 0.005), &long_dust);
    assert!(v.ok, "an implicit dust close must pass the min-qty floor: {v:?}");
    // the SAME order with no reduce intent (flat account) still denies exactly as today
    let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let v = RiskGate::new(lim()).check(&market(-1, 0.005), &flat);
    assert!(!v.ok && v.reason == "below-min-qty", "opening dust must still deny: {v:?}");
    // and a REVERSAL (qty > |position|) is NOT a pure reduce — it opens the far side
    let v = RiskGate::new(lim()).check(&market(-1, 0.008), &long_dust);
    assert!(!v.ok && v.reason == "below-min-qty", "a below-min reversal must deny: {v:?}");
    // the reduce_only FLAG alone buys nothing at the floor: with a FLAT book it is an
    // OPENING order (SimBroker derives closing-ness from the position, never a flag) —
    // a buggy strategy tagging entries reduce_only must not put sub-floor orders on the wire
    let mut flagged_open = market(-1, 0.005);
    flagged_open.reduce_only = true;
    let v = RiskGate::new(lim()).check(&flagged_open, &flat);
    assert!(
        !v.ok && v.reason == "below-min-qty",
        "flat-book reduce_only must stay floor-gated: {v:?}"
    );
}

/// The min-notional twin: same anti-stranding exemption, same non-reduce pin.
#[test]
fn pure_reduce_dust_flatten_passes_min_notional_gate() {
    let lim = || RiskLimits { min_notional: Some(5.0), ..RiskLimits::new() };
    // 0.02 @ mark 100 = 2.0 notional, under the 5.0 floor
    let long_dust =
        RiskContext { mark_price: 100.0, position_size: 0.02, ..RiskContext::default() };
    let mut req = market(-1, 0.02);
    req.reduce_only = true;
    let v = RiskGate::new(lim()).check(&req, &long_dust);
    assert!(v.ok, "a reduce_only dust flatten must pass the min-notional floor: {v:?}");
    let v = RiskGate::new(lim()).check(&market(-1, 0.02), &long_dust);
    assert!(v.ok, "an implicit dust close must pass the min-notional floor: {v:?}");
    // no reduce intent ⇒ denies exactly as today
    let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let v = RiskGate::new(lim()).check(&market(-1, 0.02), &flat);
    assert!(!v.ok && v.reason == "below-min-notional", "opening dust must still deny: {v:?}");
    // a reversal (0.03 > the 0.02 position) is not a pure reduce ⇒ still denied
    let v = RiskGate::new(lim()).check(&market(-1, 0.03), &long_dust);
    assert!(!v.ok && v.reason == "below-min-notional", "reversal must deny: {v:?}");
}

/// The `max_total_exposure` lane folds `ctx.multiplier` into its projected exposure, like its
/// `min_notional`/per-order-cap and `initial_margin` siblings; a multiplier of 10 proves the fold
/// is present (multiplier 1 is bit-identical).
#[test]
fn max_total_exposure_includes_the_contract_multiplier() {
    // flat book, BUY 2 @ mark 100 with a x10 multiplier ⇒ projected exposure = 2 * 100 * 10 =
    // 2_000 (200 without the multiplier).
    let ctx = RiskContext { mark_price: 100.0, multiplier: 10.0, ..RiskContext::default() };
    // cap 1_000 sits BETWEEN the two: 200 would PASS, 2_000 DENIES.
    let v = RiskGate::new(RiskLimits { max_total_exposure: Some(1_000.0), ..RiskLimits::new() })
        .check(&market(1, 2.0), &ctx);
    assert!(
        !v.ok && v.reason == "over-max-exposure",
        "the multiplier must enter projected exposure (2*100*10 = 2_000 > 1_000): {v:?}"
    );
    // a cap above the true multiplied exposure still passes (2_000 <= 2_500).
    let v = RiskGate::new(RiskLimits { max_total_exposure: Some(2_500.0), ..RiskLimits::new() })
        .check(&market(1, 2.0), &ctx);
    assert!(v.ok, "a cap above the true multiplied exposure must pass: {v:?}");
    // multiplier 1 (the default) is unchanged: 2 * 100 * 1 = 200 <= 1_000 ⇒ passes.
    let ctx1 = RiskContext { mark_price: 100.0, multiplier: 1.0, ..RiskContext::default() };
    let v = RiskGate::new(RiskLimits { max_total_exposure: Some(1_000.0), ..RiskLimits::new() })
        .check(&market(1, 2.0), &ctx1);
    assert!(v.ok, "multiplier-1 exposure is bit-identical and must still pass: {v:?}");
}

// ── HALTED admits a position-covered reduce, and NOTHING else ────────────────────────────
//
// The law: a halt stops OPENING risk and must never TRAP the operator in a position
// (`docs/ops/kill-switches.md`).

/// The shape `OrderIntent::Flatten` mints — a `reduce_only` MARKET for exactly
/// `|position|`, opposite the position — must pass the kill switch.
#[test]
fn halted_admits_the_flatten_shape_that_market_exit_mints() {
    let long = RiskContext {
        mark_price: 100.0,
        position_size: 2.0,
        trading_state: TradingState::Halted,
        ..RiskContext::default()
    };
    // exactly what `Flatten` builds: closing_side(pos) = -1, qty = |pos|, reduce_only.
    let mut flat_leg = market(-1, 2.0);
    flat_leg.reduce_only = true;
    let v = RiskGate::new(RiskLimits::new()).check(&flat_leg, &long);
    assert!(v.ok, "a halt must not trap the operator in a position: {v:?}");

    // the SHORT twin (short 2, BUY 2 to close)
    let short = RiskContext { position_size: -2.0, ..long };
    let mut flat_short = market(1, 2.0);
    flat_short.reduce_only = true;
    assert!(RiskGate::new(RiskLimits::new()).check(&flat_short, &short).ok);

    // a PARTIAL exit is covered too — it shrinks abs(position) without crossing zero.
    assert!(RiskGate::new(RiskLimits::new()).check(&market(-1, 1.0), &long).ok);

    // ...and the IMPLICIT form: a genuine exit is admitted whether or not the caller
    // remembered the flag, because the predicate reads the POSITION, not the flag.
    assert!(RiskGate::new(RiskLimits::new()).check(&market(-1, 2.0), &long).ok);
}

/// THE MUTATION SENTINEL, and the reason the predicate is `is_covered_reduce` rather than
/// `request.reduce_only`. A gate that trusted the caller-asserted flag would pass the test
/// above AND admit both of these — each of which OPENS risk under a halt.
#[test]
fn halted_refuses_a_reduce_only_flag_that_does_not_actually_reduce() {
    let halted = |pos: f64| RiskContext {
        mark_price: 100.0,
        position_size: pos,
        trading_state: TradingState::Halted,
        ..RiskContext::default()
    };

    // (1) FLAT BOOK: there is no position to reduce, so a `reduce_only` tag is an OPENING
    // order — and no venue catches it server-side either, since there is nothing to cap it
    // against. This is the shape a strategy bug that tags its entries `reduce_only` produces.
    let mut flagged_open = market(-1, 2.0);
    flagged_open.reduce_only = true;
    let v = RiskGate::new(RiskLimits::new()).check(&flagged_open, &halted(0.0));
    assert!(
        !v.ok && v.reason == "halted",
        "a flat-book reduce_only order is an OPENING order and the halt must refuse it: {v:?}"
    );

    // (2) REVERSAL: long 2, `reduce_only` SELL 5 flips to SHORT 3 of brand-new exposure.
    let mut overshoot = market(-1, 5.0);
    overshoot.reduce_only = true;
    let v = RiskGate::new(RiskLimits::new()).check(&overshoot, &halted(2.0));
    assert!(
        !v.ok && v.reason == "halted",
        "a reduce_only order that FLIPS the position opens risk under a halt: {v:?}"
    );

    // (3) SAME-DIRECTION ADD tagged reduce_only (long 5, BUY 2): magnitude-covered but
    // exposure-INCREASING. `is_covered_reduce`'s direction arm is what refuses it.
    let mut add = market(1, 2.0);
    add.reduce_only = true;
    let v = RiskGate::new(RiskLimits::new()).check(&add, &halted(5.0));
    assert!(!v.ok && v.reason == "halted", "a reduce_only ADD must not pass a halt: {v:?}");

    // (4) and the ordinary opening order, which is what a halt is FOR.
    let v = RiskGate::new(RiskLimits::new()).check(&market(1, 2.0), &halted(0.0));
    assert!(!v.ok && v.reason == "halted", "an opening order must still be halted: {v:?}");
}

/// The LOT-ROUNDING edge the post-normalization re-check exists for. The kill-switch arm at
/// the top of `check_inner` judges the RAW qty (it runs before the grid is resolved), and
/// rounding is half-to-EVEN, so it can round a qty UP across the coverage boundary: 1.6 on a
/// 1.0 lot becomes 2.0, which against a 1.8 long FLIPS the position short 0.2.
///
/// ⚠ MUTATION-CHECK THIS ONE by deleting the re-check next to `covered_reduce` — the raw-qty
/// arm admits it (1.8 >= 1.6) and this test is the only thing that catches it.
#[test]
fn halted_refuses_a_reduce_whose_lot_rounding_would_flip_the_position() {
    let long = RiskContext {
        mark_price: 100.0,
        position_size: 1.8,
        trading_state: TradingState::Halted,
        ..RiskContext::default()
    };
    let lim = || RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
    let mut req = market(-1, 1.6); // covered RAW (1.8 >= 1.6); rounds to 2.0 on the wire
    req.reduce_only = true;
    let v = RiskGate::new(lim()).check(&req, &long);
    assert!(
        !v.ok && v.reason == "halted",
        "the halt verdict must be re-taken against the size actually sent: {v:?}"
    );
    // the same order on a grid that does NOT round it up is still admitted.
    let mut fine = market(-1, 1.6);
    fine.reduce_only = true;
    let v =
        RiskGate::new(RiskLimits { lot_size: Some(0.1), ..RiskLimits::new() }).check(&fine, &long);
    assert!(v.ok, "a genuinely covered reduce must still get out: {v:?}");
}

/// The three states are a strict LADDER — `Halted` ⊂ `Reducing` ⊂ `Active` — and the halt
/// exemption must never widen `Halted` past `Reducing`. `Reducing` keeps the looser,
/// flag-trusting `RiskGate::reduces` on purpose: it is the state you are meant to be able to
/// trade out of, whereas `Halted` is the kill switch.
#[test]
fn halted_admits_strictly_less_than_reducing_which_admits_less_than_active() {
    let at = |state, pos: f64| RiskContext {
        mark_price: 100.0,
        position_size: pos,
        trading_state: state,
        ..RiskContext::default()
    };
    let mut flagged_open = market(-1, 2.0);
    flagged_open.reduce_only = true;

    // the flag-only order (flat book): Active yes, Reducing yes (it trusts the flag), Halted NO.
    assert!(
        RiskGate::new(RiskLimits::new()).check(&flagged_open, &at(TradingState::Active, 0.0)).ok
    );
    assert!(
        RiskGate::new(RiskLimits::new()).check(&flagged_open, &at(TradingState::Reducing, 0.0)).ok
    );
    assert!(
        !RiskGate::new(RiskLimits::new()).check(&flagged_open, &at(TradingState::Halted, 0.0)).ok
    );

    // a genuine covered exit: admitted by all three.
    let exit = market(-1, 2.0);
    for st in [TradingState::Active, TradingState::Reducing, TradingState::Halted] {
        assert!(
            RiskGate::new(RiskLimits::new()).check(&exit, &at(st, 2.0)).ok,
            "a covered exit must be admitted in every state, including {st:?}"
        );
    }

    // a plain opening order: Active only.
    let open = market(1, 2.0);
    assert!(RiskGate::new(RiskLimits::new()).check(&open, &at(TradingState::Active, 0.0)).ok);
    assert!(!RiskGate::new(RiskLimits::new()).check(&open, &at(TradingState::Reducing, 0.0)).ok);
    assert!(!RiskGate::new(RiskLimits::new()).check(&open, &at(TradingState::Halted, 0.0)).ok);
}

/// A COMBO stays denied wholesale under `Halted` — `check_combo`'s own kill-switch arm returns
/// before any leg is examined, so the single-order exemption above cannot leak into it. This is
/// a decision, not an oversight: `market_exit_flatten_legs` mints per-symbol `Flatten` intents
/// (single orders), never a combo, so nothing on the exit path needs this; and a combo is ONE
/// new multi-leg venue order whose legs derive their `reduce_only` from a PROJECTED book rather
/// than a settled one. Widening it would need each leg proven covered against real state.
#[test]
fn halted_still_denies_a_combo_wholesale() {
    let halted = RiskContext {
        trading_state: TradingState::Halted,
        position_size: 2.0,
        ..RiskContext::default()
    };
    let v = RiskGate::new(RiskLimits::new()).check_combo(
        &combo_limit(-1, 1.0, 20.0),
        &halted,
        leg_marks,
    );
    assert!(!v.ok && v.reason == "halted", "a combo is refused as one unit under halt: {v:?}");
}

/// A `reduce_only`-tagged SAME-DIRECTION ADD (long 5, BUY 2: magnitude-covered but
/// exposure-INCREASING) is NOT a covered reduce, so every bypass that reads `is_covered_reduce`
/// — the min floors, buying power, the impact veto — treats it as the OPENING order it is,
/// matching `SimBroker::apply_fill`'s direction-only split and the venues (binance/bybit) that
/// reject such a reduce_only order.
#[test]
fn reduce_only_same_direction_add_is_not_a_covered_reduce() {
    // FLOOR lane: long 5, BUY 2 tagged reduce_only, below a 10.0 min_qty floor ⇒ DENY.
    let long = RiskContext { mark_price: 100.0, position_size: 5.0, ..RiskContext::default() };
    let mut add = market(1, 2.0);
    add.reduce_only = true;
    let v =
        RiskGate::new(RiskLimits { min_qty: Some(10.0), ..RiskLimits::new() }).check(&add, &long);
    assert!(
        !v.ok && v.reason == "below-min-qty",
        "a reduce_only same-direction ADD must face the min floor: {v:?}"
    );
    // the short twin (short 5, SELL 2 tagged reduce_only)
    let short = RiskContext { mark_price: 100.0, position_size: -5.0, ..RiskContext::default() };
    let mut add_s = market(-1, 2.0);
    add_s.reduce_only = true;
    let v = RiskGate::new(RiskLimits { min_qty: Some(10.0), ..RiskLimits::new() })
        .check(&add_s, &short);
    assert!(!v.ok && v.reason == "below-min-qty", "short-side add must face the floor: {v:?}");

    // MARGIN lane: the add opens real exposure, so it must face buying power. IM 0.1,
    // order margin = 2 * 100 * 0.1 = 20; equity 10 ⇒ DENY.
    let poor = RiskContext {
        mark_price: 100.0,
        position_size: 5.0,
        equity: 10.0,
        multiplier: 1.0,
        ..RiskContext::default()
    };
    let v = RiskGate::new(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() })
        .check(&add, &poor);
    assert!(
        !v.ok && v.reason == "insufficient-margin",
        "a reduce_only same-direction ADD must face buying power: {v:?}"
    );

    // GENUINE covered reduce is untouched: SELL 2 into the long 5, below the same floor, still
    // bypasses (anti-stranding preserved).
    let mut reduce = market(-1, 2.0);
    reduce.reduce_only = true;
    let v = RiskGate::new(RiskLimits { min_qty: Some(10.0), ..RiskLimits::new() })
        .check(&reduce, &long);
    assert!(v.ok, "a genuine covered reduce must still bypass the floor: {v:?}");
}

/// The gate's notional includes the contract multiplier, as `SimBroker` gates fills on
/// `rounded × price × multiplier` and the gate's own margin calc does, so `min_notional` gates
/// identically live and backtest for multiplier != 1 instruments.
#[test]
fn notional_includes_the_contract_multiplier() {
    let lim = || RiskLimits {
        min_notional: Some(5.0),
        max_notional_per_order: Some(100.0),
        ..RiskLimits::new()
    };
    let m10 = RiskContext { mark_price: 1.0, multiplier: 10.0, ..RiskContext::default() };
    // qty×price = 2 (under the 5.0 floor) but ×10 multiplier = 20 ⇒ passes now
    let v = RiskGate::new(lim()).check(&market(1, 2.0), &m10);
    assert!(v.ok, "multiplier-inclusive notional must clear the floor: {v:?}");
    // vice versa: qty×price = 20 (inside the 100 cap) but ×10 = 200 ⇒ over-max-notional now
    let v = RiskGate::new(lim()).check(&market(1, 20.0), &m10);
    assert!(!v.ok && v.reason == "over-max-notional", "cap must see the multiplier: {v:?}");
    // still under the floor even WITH the multiplier: 0.2 × 1 × 10 = 2 < 5
    let v = RiskGate::new(lim()).check(&market(1, 0.2), &m10);
    assert!(!v.ok && v.reason == "below-min-notional", "{v:?}");
    // COMPAT PIN: multiplier 1.0 (the default) reproduces the multiplier-free verdicts (the
    // boundary cases of `ordinary_positive_price_verdicts_are_unchanged_by_the_abs`).
    let m1 = RiskContext { mark_price: 100.0, multiplier: 1.0, ..RiskContext::default() };
    assert!(RiskGate::new(lim()).check(&market(1, 0.5), &m1).ok);
    assert_eq!(RiskGate::new(lim()).check(&market(1, 0.01), &m1).reason, "below-min-notional");
    assert_eq!(RiskGate::new(lim()).check(&market(-1, 2.0), &m1).reason, "over-max-notional");
}

/// The combo path routes every leg through the SAME `check_inner` (it has no notional line of
/// its own), so the multiplier-inclusive notional flows through `leg_ctx`'s per-symbol
/// `multiplier` automatically — pinned here so a future combo-side notional never forks.
#[test]
fn combo_leg_notional_uses_the_leg_multiplier() {
    // 0.1 units: leg A 0.1×50 = 5.0 (at the floor), leg B 0.1×30 = 3.0 — denied at ×1
    // (the `one_failing_leg…` case), but as a ×10 contract 3.0×10 = 30 ⇒ passes.
    let lim = || RiskLimits { min_notional: Some(5.0), ..RiskLimits::new() };
    let ctx = RiskContext::default();
    let v = RiskGate::new(lim()).check_combo(&combo_limit(1, 0.1, 20.0), &ctx, leg_marks);
    assert_eq!(v.reason, format!("leg {LEG_B}: below-min-notional"));
    let with_mult = |sym: &str| {
        let mut c = leg_marks(sym);
        if sym == LEG_B {
            c.multiplier = 10.0;
        }
        c
    };
    let v = RiskGate::new(lim()).check_combo(&combo_limit(1, 0.1, 20.0), &ctx, with_mult);
    assert!(v.ok, "the leg multiplier must enter the leg's notional: {v:?}");
}

#[test]
fn notional_is_a_magnitude_so_credit_combos_gate_like_debit_ones() {
    // A SIGNED notional goes NEGATIVE for a credit combo: `notional < min_notional` would deny
    // EVERY credit combo and `notional > cap` could never trip.
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let limits = || RiskLimits {
        min_notional: Some(5.0),
        max_notional_per_order: Some(100.0),
        ..RiskLimits::new()
    };

    // DEBIT (+20 net, 2 units => |notional| 40): inside both bounds, passes. Unchanged.
    let mut gate = RiskGate::new(limits());
    let v = gate.check(&combo_limit(1, 2.0, 20.0), &ctx);
    assert!(v.ok, "debit combo should pass: {v:?}");

    // CREDIT (-20 net, same magnitude): must gate IDENTICALLY to the debit twin.
    let mut gate = RiskGate::new(limits());
    let v = gate.check(&combo_limit(-1, 2.0, -20.0), &ctx);
    assert!(v.ok, "credit combo must NOT be denied below-min-notional: {v:?}");

    // ...and the per-order cap must still bite on the credit side (|−60| * 2 = 120 > 100).
    let mut gate = RiskGate::new(limits());
    let v = gate.check(&combo_limit(-1, 2.0, -60.0), &ctx);
    assert!(!v.ok && v.reason == "over-max-notional", "credit cap must bite: {v:?}");
    // symmetric with the debit twin
    let mut gate = RiskGate::new(limits());
    let v = gate.check(&combo_limit(1, 2.0, 60.0), &ctx);
    assert!(!v.ok && v.reason == "over-max-notional", "{v:?}");

    // a genuinely tiny credit is still below-min-notional (the check is not disabled)
    let mut gate = RiskGate::new(limits());
    let v = gate.check(&combo_limit(-1, 1.0, -1.0), &ctx);
    assert!(!v.ok && v.reason == "below-min-notional", "{v:?}");
}

#[test]
fn ordinary_positive_price_verdicts_are_unchanged_by_the_abs() {
    // The abs() must be a NO-OP for every ordinary (non-negative price) order — the
    // byte-identical-when-off guarantee.
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let lim = || RiskLimits {
        min_notional: Some(5.0),
        max_notional_per_order: Some(100.0),
        ..RiskLimits::new()
    };
    let mut gate = RiskGate::new(lim());
    // market order, priced off the mark: 0.5 * 100 = 50 => ok
    assert!(gate.check(&market(1, 0.5), &ctx).ok);
    // 0.01 * 100 = 1 < 5 => below-min-notional
    let mut gate = RiskGate::new(lim());
    assert_eq!(gate.check(&market(1, 0.01), &ctx).reason, "below-min-notional");
    // 2 * 100 = 200 > 100 => over-max-notional
    let mut gate = RiskGate::new(lim());
    assert_eq!(gate.check(&market(-1, 2.0), &ctx).reason, "over-max-notional");
}
