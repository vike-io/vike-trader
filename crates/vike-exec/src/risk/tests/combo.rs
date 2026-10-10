//! `check_combo`: the atomic per-leg crossing, leg accumulation and the ONE-slot throttle split.

use super::*;

// ---- combo crossing (spec §5: atomic per-leg, ONE throttle slot) ----

#[test]
fn combo_passes_when_every_leg_passes_and_burns_exactly_one_slot() {
    let mut gate = RiskGate::new(RiskLimits {
        min_notional: Some(5.0),
        max_orders_per_window: Some(1),
        ..RiskLimits::new()
    });
    let ctx = RiskContext::default();
    // 2 units: leg A 2×50 = 100, leg B 2×30 = 60 — both above min_notional
    let v = gate.check_combo(&combo_limit(1, 2.0, 20.0), &ctx, leg_marks);
    assert!(v.ok, "{v:?}");
    // the returned request is the COMBO verbatim — legs intact, SIGNED net untouched
    let req = v.request.unwrap();
    assert_eq!(req.combo_legs.len(), 2);
    assert_eq!(req.price, Some(20.0));
    // ONE slot for N legs, not N
    assert_eq!(gate.throttle_times().len(), 1, "a combo must consume exactly one slot");
    // ...and it really was consumed: the next combo is rate-limited
    let v = gate.check_combo(&combo_limit(1, 2.0, 20.0), &ctx, leg_marks);
    assert!(!v.ok && v.reason == "rate-limited", "{v:?}");
}

#[test]
fn credit_combo_gates_exactly_like_its_debit_twin() {
    // A short call spread is a CREDIT: net_limit NEGATIVE. Per-leg risk is identical to the
    // debit twin (same legs, same marks, same sizes) — nothing may treat the sign as size.
    let limits = || RiskLimits {
        min_notional: Some(5.0),
        max_notional_per_order: Some(1_000.0),
        ..RiskLimits::new()
    };
    let ctx = RiskContext::default();
    let debit = RiskGate::new(limits()).check_combo(&combo_limit(1, 2.0, 20.0), &ctx, leg_marks);
    let credit = RiskGate::new(limits()).check_combo(&combo_limit(-1, 2.0, -20.0), &ctx, leg_marks);
    assert!(debit.ok, "{debit:?}");
    assert!(credit.ok, "credit combo must not be denied: {credit:?}");
    // the negative net survives the gate un-clamped and un-absolute-valued
    assert_eq!(credit.request.unwrap().price, Some(-20.0));
}

#[test]
fn one_failing_leg_denies_the_whole_combo_and_names_it() {
    // LEG_B's mark is 30 ⇒ 0.1 units = 3.0 notional, under the 5.0 floor; LEG_A (5.0) passes.
    let mut gate = RiskGate::new(RiskLimits {
        min_notional: Some(5.0),
        max_orders_per_window: Some(4),
        ..RiskLimits::new()
    });
    let v = gate.check_combo(&combo_limit(1, 0.1, 20.0), &RiskContext::default(), leg_marks);
    assert!(!v.ok, "{v:?}");
    assert_eq!(v.reason, format!("leg {LEG_B}: below-min-notional"));
    assert!(v.request.is_none());
    // a DENIED combo burns no rate slot — the passing first leg must not have taken one either
    assert!(gate.throttle_times().is_empty(), "a denied combo consumed a slot: {v:?}");
}

#[test]
fn combo_leg_sides_follow_the_sign_law_for_reduce_only_state() {
    // Reducing state admits only position-reducing legs. Long LEG_A / flat LEG_B:
    // BUYING the combo (+1 ratio on A) is an ADD on A ⇒ denied, naming A.
    let per_leg = |sym: &str| match sym {
        LEG_A => RiskContext {
            mark_price: 50.0,
            position_size: 10.0,
            trading_state: TradingState::Reducing,
            ..RiskContext::default()
        },
        _ => RiskContext {
            mark_price: 30.0,
            trading_state: TradingState::Reducing,
            ..RiskContext::default()
        },
    };
    let ctx = RiskContext { trading_state: TradingState::Reducing, ..RiskContext::default() };
    let mut gate = RiskGate::new(RiskLimits::new());
    let v = gate.check_combo(&combo_limit(1, 1.0, 20.0), &ctx, per_leg);
    assert_eq!(v.reason, format!("leg {LEG_A}: reduce-only"));
    // SELLING the same combo flips leg A short (a reduce on the long) — leg B (+1 after the
    // flip of its −1 ratio) is the one that now adds, so the denial moves to B.
    let mut gate = RiskGate::new(RiskLimits::new());
    let v = gate.check_combo(&combo_limit(-1, 1.0, -20.0), &ctx, per_leg);
    assert_eq!(v.reason, format!("leg {LEG_B}: reduce-only"));
}

#[test]
fn combo_leg_qty_is_ratio_times_units() {
    // ratio 3 on leg A: 3 × 2 units = 6 @ 50 = 300 notional — over a 250 cap, under 350.
    let mut req = combo_limit(1, 2.0, 20.0);
    req.combo_legs[0].ratio = 3;
    let ctx = RiskContext::default();
    let v = RiskGate::new(RiskLimits { max_notional_per_order: Some(250.0), ..RiskLimits::new() })
        .check_combo(&req, &ctx, leg_marks);
    assert_eq!(v.reason, format!("leg {LEG_A}: over-max-notional"));
    let v = RiskGate::new(RiskLimits { max_notional_per_order: Some(350.0), ..RiskLimits::new() })
        .check_combo(&req, &ctx, leg_marks);
    assert!(v.ok, "{v:?}");
}

#[test]
fn combo_guards_halted_bad_side_bad_qty_and_the_not_a_combo_sentinel() {
    let mut gate = RiskGate::new(RiskLimits::new());
    let halted = RiskContext { trading_state: TradingState::Halted, ..RiskContext::default() };
    assert_eq!(gate.check_combo(&combo_limit(1, 1.0, 20.0), &halted, leg_marks).reason, "halted");
    let ctx = RiskContext::default();
    assert_eq!(
        gate.check_combo(&combo_limit(0, 1.0, 20.0), &ctx, leg_marks).reason,
        "invalid-side"
    );
    assert_eq!(
        gate.check_combo(&combo_limit(1, 0.0, 20.0), &ctx, leg_marks).reason,
        "non-positive-size"
    );
    assert_eq!(
        gate.check_combo(&combo_limit(1, f64::NAN, 20.0), &ctx, leg_marks).reason,
        "non-positive-size"
    );
    // an ordinary (non-combo) request routed here is the sentinel case, never a silent pass
    assert_eq!(gate.check_combo(&market(1, 1.0), &ctx, leg_marks).reason, "not-a-combo");
    assert!(gate.throttle_times().is_empty());
}

#[test]
fn account_reducing_is_authoritative_even_when_leg_ctx_says_active() {
    // CRITICAL: the natural caller closure fills only PER-SYMBOL facts and leaves
    // `trading_state` at its `RiskContext::default()` value (Active). The account ctx must
    // still win, or a fully risk-ADDING combo is admitted while the account is reduce-only.
    // NOTE this test deliberately does NOT hand-thread `Reducing` into the legs.
    let ctx = RiskContext { trading_state: TradingState::Reducing, ..RiskContext::default() };
    let mut gate = RiskGate::new(RiskLimits::new());
    let v = gate.check_combo(&combo_limit(1, 2.0, 20.0), &ctx, leg_marks);
    assert!(!v.ok, "a risk-adding combo must be denied while the account reduces: {v:?}");
    assert_eq!(v.reason, format!("leg {LEG_A}: reduce-only"));
    assert!(gate.throttle_times().is_empty());
    // sanity: the same combo passes when the account is Active
    assert!(gate.check_combo(&combo_limit(1, 2.0, 20.0), &RiskContext::default(), leg_marks).ok);
}

#[test]
fn a_leg_with_no_mark_is_denied_not_priced_at_zero() {
    // CRITICAL: `leg_ctx` is TOTAL, so an unknown symbol yields mark 0.0 — which makes
    // notional 0, exposure 0 and initial_margin 0, i.e. every price-based limit vacuous.
    // `min_notional` would catch it, but `from_properties` maps 0.0 → None, the normal case
    // for options — the exact asset class combos exist for.
    let unarmed = || RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() };
    let ctx = RiskContext::default();

    // 0.0 mark (unknown symbol): would otherwise pass buying power at ZERO equity.
    let zero_mark = |sym: &str| match sym {
        // deep-pocketed so leg A itself is never the denial under test
        LEG_A => RiskContext { mark_price: 50.0, equity: 1e18, ..RiskContext::default() },
        _ => RiskContext::default(), // mark 0.0 — "I don't know this symbol", equity 0
    };
    let v = RiskGate::new(unarmed()).check_combo(&combo_limit(1, 1e6, 20.0), &ctx, zero_mark);
    assert!(!v.ok, "a zero-mark leg must not be admitted: {v:?}");
    assert_eq!(v.reason, format!("leg {LEG_B}: no-mark"));

    // NaN mark: every comparison against it is false, so it silently passes every cap.
    let nan_mark = |sym: &str| match sym {
        LEG_A => RiskContext { mark_price: 50.0, equity: 1e18, ..RiskContext::default() },
        _ => RiskContext { mark_price: f64::NAN, ..RiskContext::default() },
    };
    let v = RiskGate::new(unarmed()).check_combo(&combo_limit(1, 1e6, 20.0), &ctx, nan_mark);
    assert_eq!(v.reason, format!("leg {LEG_B}: no-mark"));
}

#[test]
fn leg_margin_accumulates_so_a_combo_is_never_cheaper_than_its_naked_legs() {
    // Without accumulation N legs each fit in the same unchanged free BP. im 0.1, equity 100k,
    // marks 50/30: pick a size where the PAIR overflows but each leg alone does not.
    let limits = || RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() };
    let acct = |_: &str| RiskContext { equity: 100_000.0, ..RiskContext::default() };
    let marks_with_equity =
        move |sym: &str| RiskContext { mark_price: leg_marks(sym).mark_price, ..acct(sym) };
    let ctx = RiskContext::default();

    // 15_000 units: A = 75_000, B = 45_000. Each alone < 100_000; together 120_000 > 100_000.
    let v = RiskGate::new(limits()).check_combo(
        &combo_limit(1, 15_000.0, 20.0),
        &ctx,
        marks_with_equity,
    );
    assert!(!v.ok, "the second leg must see the first leg's committed margin: {v:?}");
    assert_eq!(v.reason, format!("leg {LEG_B}: insufficient-margin"));

    // proof it is the ACCUMULATION and not a per-leg cap: 10_000 units (50_000 + 30_000 =
    // 80_000 ≤ 100_000) still passes.
    let v = RiskGate::new(limits()).check_combo(
        &combo_limit(1, 10_000.0, 20.0),
        &ctx,
        marks_with_equity,
    );
    assert!(v.ok, "{v:?}");
}

#[test]
fn repeated_leg_symbol_accumulates_exposure_instead_of_double_measuring() {
    // `ComboSpec::validate` does NOT reject a duplicate leg symbol; without accumulation the
    // exposure cap would measure the SAME 0-position twice instead of the sum.
    let mut req = combo_limit(1, 10.0, 20.0);
    req.combo_legs[1].symbol = LEG_A.into();
    req.combo_legs[1].ratio = 1; // both legs BUY 10 of LEG_A ⇒ projected 20 @ 50 = 1_000
    let ctx = RiskContext::default();
    // cap 750: leg 1 alone projects 500 (passes), the pair projects 1_000 (must deny)
    let v = RiskGate::new(RiskLimits { max_total_exposure: Some(750.0), ..RiskLimits::new() })
        .check_combo(&req, &ctx, leg_marks);
    assert!(!v.ok, "the repeated symbol must accumulate: {v:?}");
    assert_eq!(v.reason, format!("leg {LEG_A}: over-max-exposure"));
    // 1_100 clears the accumulated projection
    let v = RiskGate::new(RiskLimits { max_total_exposure: Some(1_100.0), ..RiskLimits::new() })
        .check_combo(&req, &ctx, leg_marks);
    assert!(v.ok, "{v:?}");
}

#[test]
fn reduce_only_combo_does_not_launder_an_opening_leg() {
    // A leg that inherited the combo's `reduce_only` would skip buying power and pass the
    // `Reducing` gate — including a leg opening a brand-new position.
    let mut req = combo_limit(1, 100.0, 20.0);
    req.reduce_only = true;
    // short LEG_A (so the +1 BUY leg genuinely reduces), flat LEG_B.
    let per_leg = |sym: &str| match sym {
        LEG_A => RiskContext {
            mark_price: 50.0,
            position_size: -1_000.0, // short ⇒ the BUY leg really is a reduce
            equity: 10_000.0,
            ..RiskContext::default()
        },
        _ => RiskContext { mark_price: 30.0, equity: 10_000.0, ..RiskContext::default() },
    };
    // LEG_B is SOLD (ratio −1) into a FLAT book — a brand-new short, margin 100×30×0.1 = 300
    // against equity 10_000 ⇒ fits. Tighten equity so the opening leg cannot afford it: only
    // reachable at all if reduce_only is NOT inherited.
    let poor = move |sym: &str| RiskContext { equity: 100.0, ..per_leg(sym) };
    let v = RiskGate::new(RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() })
        .check_combo(&req, &RiskContext::default(), poor);
    assert!(!v.ok, "an opening leg inside a reduce_only combo must not bypass margin: {v:?}");
    assert_eq!(v.reason, format!("leg {LEG_B}: insufficient-margin"));

    // ...and the genuinely-reducing leg (A, buying back a short) is still treated as a reduce:
    // it is checked FIRST and did not deny.
    let v = RiskGate::new(RiskLimits {
        im_requirement: Some(0.1),
        block_reduce_only_overshoot: true,
        ..RiskLimits::new()
    })
    .check_combo(&req, &RiskContext::default(), poor);
    assert_eq!(v.reason, format!("leg {LEG_B}: insufficient-margin"), "{v:?}");
}

#[test]
fn combo_denies_infinite_qty_and_zero_ratio_with_its_own_reason() {
    let ctx = RiskContext::default();
    let mut gate = RiskGate::new(RiskLimits::new());
    // INFINITY is not caught by `<= 0.0`: an infinite leg notional sails through an UNARMED
    // notional cap, which is the default.
    assert_eq!(
        gate.check_combo(&combo_limit(1, f64::INFINITY, 20.0), &ctx, leg_marks).reason,
        "non-positive-size"
    );
    // a 0 ratio names itself, not the misleading `leg X: invalid-side`
    let mut req = combo_limit(1, 1.0, 20.0);
    req.combo_legs[0].ratio = 0;
    assert_eq!(gate.check_combo(&req, &ctx, leg_marks).reason, format!("leg {LEG_A}: zero-ratio"));
    assert!(gate.throttle_times().is_empty());
}

#[test]
fn plain_orders_are_untouched_by_the_throttle_split() {
    // The single-order path must stay byte-identical: same admissions, same window state.
    let lim = || RiskLimits { max_orders_per_window: Some(2), ..RiskLimits::new() };
    let ctx = RiskContext { mark_price: 100.0, now_ms: 500, ..RiskContext::default() };
    let mut gate = RiskGate::new(lim());
    assert!(gate.check(&market(1, 1.0), &ctx).ok);
    assert!(gate.check(&market(-1, 1.0), &ctx).ok);
    let v = gate.check(&market(1, 1.0), &ctx);
    assert!(!v.ok && v.reason == "rate-limited", "{v:?}");
    assert_eq!(gate.throttle_times(), vec![500, 500]);
    // the window still slides exactly as before (window_ms = 1000)
    let later = RiskContext { now_ms: 1600, ..ctx };
    assert!(gate.check(&market(1, 1.0), &later).ok);
    assert_eq!(gate.throttle_times(), vec![1600]);
}

#[test]
fn impact_denial_does_not_consume_a_throttle_slot() {
    let ctx = RiskContext { mark_price: 100.0, now_ms: 0, ..RiskContext::default() };
    let b = book();
    let mut gate = RiskGate::new(RiskLimits {
        max_slippage_bps: Some(60.0),
        max_orders_per_window: Some(1),
        ..RiskLimits::new()
    });
    let d = gate.check_with_book(&market(1, 3.0), &ctx, Some(&b));
    assert!(!d.ok && d.reason == "impact-over-slippage-budget");
    assert!(gate.throttle_times().is_empty(), "a vetoed order must not burn a rate slot");
    // the slot is still available to a within-budget order
    assert!(gate.check_with_book(&market(1, 1.0), &ctx, Some(&b)).ok);
}
