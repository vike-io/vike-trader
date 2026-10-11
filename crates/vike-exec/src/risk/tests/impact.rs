//! The opt-in pre-trade impact veto through `check_with_book`. The pure book walks it calls are
//! tested beside them, in `crates/vike-model/src/pricing/impact_tests.rs`.

use super::*;
use vike_model::{TakeScope, take_scope};

#[test]
fn check_with_book_none_is_identical_to_check() {
    let lim = RiskLimits {
        max_slippage_bps: Some(0.0), // armed, but no book ⇒ inert
        require_fillable: true,
        ..RiskLimits::new()
    };
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let a = RiskGate::new(lim.clone()).check(&market(1, 5.0), &ctx);
    let b = RiskGate::new(lim).check_with_book(&market(1, 5.0), &ctx, None);
    assert!(a.ok && b.ok, "unarmed-by-absent-book must pass: {a:?} / {b:?}");
}

#[test]
fn check_with_book_unarmed_limits_pass_with_a_book() {
    // book present but both knobs off ⇒ no veto even for a size the book cannot fill
    let mut gate = RiskGate::new(RiskLimits::new());
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let v = gate.check_with_book(&market(1, 1_000.0), &ctx, Some(&book()));
    assert!(v.ok, "got {v:?}");
}

#[test]
fn check_with_book_denies_over_budget_and_passes_within() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let b = book();
    // within budget: 1 unit at the top of book (~50.25 bps) under a 200 bps budget
    let mut ok_gate =
        RiskGate::new(RiskLimits { max_slippage_bps: Some(200.0), ..RiskLimits::new() });
    let ok = ok_gate.check_with_book(&market(1, 1.0), &ctx, Some(&b));
    assert!(ok.ok, "got {ok:?}");
    // over budget: 3 units (~117 bps) under a 60 bps budget
    let mut deny_gate =
        RiskGate::new(RiskLimits { max_slippage_bps: Some(60.0), ..RiskLimits::new() });
    let d = deny_gate.check_with_book(&market(1, 3.0), &ctx, Some(&b));
    assert!(!d.ok && d.reason == "impact-over-slippage-budget", "got {d:?}");
}

#[test]
fn check_with_book_require_fillable_is_its_own_knob() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let b = book();
    let mut gate = RiskGate::new(RiskLimits { require_fillable: true, ..RiskLimits::new() });
    // 6 units is exactly the displayed ask depth ⇒ fillable, no budget set ⇒ passes
    assert!(gate.check_with_book(&market(1, 6.0), &ctx, Some(&b)).ok);
    let v = gate.check_with_book(&market(1, 6.5), &ctx, Some(&b));
    assert!(!v.ok && v.reason == "impact-not-fillable", "got {v:?}");
}

/// The IMPACT bypass reads `is_covered_reduce`, like the margin bypass: anti-stranding is a
/// statement about a POSITION, so on a flat book a mis-tagged order must not skip the veto.
#[test]
fn impact_veto_bypass_requires_position_coverage() {
    let b = book();
    let armed = || RiskLimits { require_fillable: true, ..RiskLimits::new() };
    let ro = |side, qty| OrderRequest { reduce_only: true, ..market(side, qty) };
    // FLAT book + the flag, size beyond the displayed depth ⇒ vetoed.
    let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let v = RiskGate::new(armed()).check_with_book(&ro(1, 6.5), &flat, Some(&b));
    assert!(!v.ok && v.reason == "impact-not-fillable", "got {v:?}");
    // COVERED reduce of the same size ⇒ still bypasses (anti-stranding preserved): the
    // exit must go through exactly when the book looks worst.
    let long = RiskContext { position_size: 10.0, mark_price: 100.0, ..RiskContext::default() };
    let c = RiskGate::new(armed()).check_with_book(&ro(-1, 6.5), &long, Some(&b));
    assert!(c.ok, "covered reduce must still bypass the impact veto; got {c:?}");
}

/// A PASSIVE limit pays no slippage and must never be impact-vetoed: walking it as a market
/// taker would deny every `SpreadMaker` quote under an armed budget.
#[test]
fn passive_limit_is_never_impact_vetoed() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let b = book();
    let armed = RiskLimits {
        max_slippage_bps: Some(1.0), // brutally tight
        require_fillable: true,      // and a depth floor the quote size blows through
        ..RiskLimits::new()
    };
    // BUY 99 (at the best bid, joining the queue) and SELL 100 — both rest, neither takes.
    for req in [limit(1, 5.0, 99.0), limit(-1, 5.0, 100.0), limit(1, 1e6, 98.0)] {
        let v = RiskGate::new(armed.clone()).check_with_book(&req, &ctx, Some(&b));
        assert!(v.ok, "passive limit must pass an armed gate: {req:?} -> {v:?}");
    }
    // stops/take-profits fire against a future book — also never judged on today's depth
    let mut stop = market(1, 1e6);
    stop.order_type = "stop".to_string();
    stop.trigger_price = Some(105.0);
    let v = RiskGate::new(armed).check_with_book(&stop, &ctx, Some(&b));
    assert!(v.ok, "stop must not be impact-vetoed on the current book: {v:?}");
}

/// A CROSSING limit does take — but only at its limit or better, and any remainder rests.
#[test]
fn crossing_limit_is_judged_only_at_its_limit_or_better() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let b = book();
    assert_eq!(take_scope(&b, 1, "limit", Some(100.0)), TakeScope::Crossing(100.0));
    assert_eq!(take_scope(&b, 1, "limit", Some(99.5)), TakeScope::Passive);
    assert_eq!(take_scope(&b, -1, "limit", Some(99.0)), TakeScope::Crossing(99.0));
    assert_eq!(take_scope(&b, 1, "market", None), TakeScope::Market);

    // BUY 3 crossing at 101 takes 100×1 + 101×2 ⇒ ~117 bps ⇒ over a 60 bps budget
    let mut g = RiskGate::new(RiskLimits { max_slippage_bps: Some(60.0), ..RiskLimits::new() });
    let d = g.check_with_book(&limit(1, 3.0, 101.0), &ctx, Some(&b));
    assert!(!d.ok && d.reason == "impact-over-slippage-budget", "got {d:?}");
    // the SAME size crossing only at 100 can take just 1 unit there (~50 bps); the other 2
    // rest, so the budget arm sees only the takeable slice and passes.
    let mut g = RiskGate::new(RiskLimits { max_slippage_bps: Some(60.0), ..RiskLimits::new() });
    let v = g.check_with_book(&limit(1, 3.0, 100.0), &ctx, Some(&b));
    assert!(v.ok, "the unfillable remainder rests, it does not pay impact: {v:?}");
    // but `require_fillable` is an explicit fill-this-size-now floor and still denies it
    let mut g = RiskGate::new(RiskLimits { require_fillable: true, ..RiskLimits::new() });
    let v = g.check_with_book(&limit(1, 3.0, 100.0), &ctx, Some(&b));
    assert!(!v.ok && v.reason == "impact-not-fillable", "got {v:?}");
}

/// A closing order must never be stranded by the impact gate.
#[test]
fn pure_reduce_bypasses_the_impact_veto() {
    let b = book();
    let armed =
        RiskLimits { require_fillable: true, max_slippage_bps: Some(0.1), ..RiskLimits::new() };
    // long 10, flatten with a market sell of 10 into 6 units of displayed bid depth
    let ctx = RiskContext { mark_price: 100.0, position_size: 10.0, ..RiskContext::default() };
    let mut req = market(-1, 10.0);
    req.reduce_only = true;
    let v = RiskGate::new(armed.clone()).check_with_book(&req, &ctx, Some(&b));
    assert!(v.ok, "an explicit reduce_only exit must go through: {v:?}");
    // the implicit form (opposing side, size within the position) too
    let v = RiskGate::new(armed.clone()).check_with_book(&market(-1, 10.0), &ctx, Some(&b));
    assert!(v.ok, "an implicit close must go through: {v:?}");
    // and the same order is still vetoed when it OPENS (flat book-side depth, no position)
    let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let v = RiskGate::new(armed).check_with_book(&market(-1, 10.0), &flat, Some(&b));
    assert!(!v.ok && v.reason == "impact-not-fillable", "opening must still veto: {v:?}");
}
