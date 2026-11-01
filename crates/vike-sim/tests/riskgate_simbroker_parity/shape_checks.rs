//! The gate SHAPE tests: deny-whole by default, the opt-in clamp, and the shared predicates.

use vike_exec::{RiskContext, RiskGate};
use vike_model::{OrderRequest, RiskLimits};

use super::sides::{live_verdict, sim_outcome};
use super::{Row, SYM, VENUE};

// ---------------------------------------------------------------------------------------------
// The gate SHAPE tests (deny-vs-clamp phases 1+2). The matrix asserts DECISIONS; these pin the
// two outcome shapes behind them: the default DENY-WHOLE through the live gate (zero fill, the
// GATE's own reason string) and the opt-in clamp (the pre-phase-1 truncation, verbatim).
// ---------------------------------------------------------------------------------------------

/// A Row for the shape lane: 1_000 units against a 100-unit leverage room
/// (`1.0 * 10_000 / 100`), the over-budget order the matrix's
/// `margin/over-budget-both-deny-whole` row decides on.
fn over_budget_row(clamp: bool) -> Row {
    Row {
        name: "shape/over-budget",
        qty: 1_000.0,
        im: Some(1.0),
        leverage: Some(1.0),
        equity: 10_000.0,
        clamp,
        ..Row::default()
    }
}

/// DEFAULT SHAPE: the order is DENIED WHOLE — zero fill, nothing resized — and the strategy can
/// OBSERVE the denial: `SimBroker::dropped` records the GATE's own reason with the full
/// requested size. UNIFIED AS OF PHASE 2: the string is `"insufficient-margin"` — the SAME
/// string, from the SAME `RiskGate::check`, that live publishes in `OrderDenied` — because the
/// backtest now IS the live gate; the phase-1 stand-in `"insufficient-leverage-room"` (kept
/// distinct back then precisely because the formulas differed) no longer exists.
#[test]
fn margin_over_budget_denies_whole_by_default() {
    let row = over_budget_row(false);
    let (live_ok, reason) = live_verdict(&row);
    assert!(!live_ok && reason == "insufficient-margin", "live must deny outright: {reason:?}");

    let (_, _, pos_after, dropped) = sim_outcome(&row);
    assert_eq!(pos_after, 0.0, "deny-whole must fill NOTHING (no silent resize), got {pos_after}");
    assert_eq!(
        dropped,
        vec![(SYM.to_string(), "insufficient-margin".to_string(), 1_000.0, 0.0)],
        "the denial must be strategy-observable: full requested size, the LIVE reason string"
    );
}

/// OPT-IN SHAPE (`clamp_to_leverage: true`): the pre-phase-1 `cap_to_leverage` truncation,
/// verbatim — the order shrinks to the remaining room and executes silently (nothing in
/// `dropped`). This is the escape hatch, not the default.
#[test]
fn clamp_knob_restores_the_truncate_shape_verbatim() {
    let (_, _, pos_after, dropped) = sim_outcome(&over_budget_row(true));
    // room = leverage*equity / (price*multiplier) = 1.0*10_000 / 100 = 100 units.
    assert!(
        (pos_after - 100.0).abs() <= 1e-9,
        "clamp_to_leverage must TRUNCATE to the remaining room (100 units), got {pos_after}"
    );
    assert!(dropped.is_empty(), "the clamp shape is silent (the historical behavior): {dropped:?}");
}

/// BOUNDARY: an order that EXACTLY fills the margin budget passes WHOLE on both shapes — the
/// deny path is the live gate itself and `has_sufficient_margin` is `<=` (order margin == free
/// buying power admits), while the clamp path's `size <= room` boundary is inclusive too, so
/// deny-mode and clamp-mode fills are identical here.
#[test]
fn exactly_fitting_order_passes_whole_on_both_shapes() {
    for clamp in [false, true] {
        let row = Row {
            name: "shape/exactly-fits",
            qty: 100.0, // = the whole room: 1.0 * 10_000 / 100
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            clamp,
            ..Row::default()
        };
        let (live_ok, reason) = live_verdict(&row);
        assert!(live_ok, "live admits the exact boundary (order margin == free BP): {reason:?}");
        let (_, _, pos_after, dropped) = sim_outcome(&row);
        assert_eq!(pos_after, 100.0, "clamp={clamp}: the exact-fit order must fill whole");
        assert!(dropped.is_empty(), "clamp={clamp}: nothing to refuse at the boundary");
    }
}

/// ANTI-STRANDING: a REDUCING order is NEVER denied by the default deny path (nor capped by the
/// clamp), even with the leverage room fully exhausted — the same never-strand-a-position rule
/// as `apply_fill`'s closing-fill floor bypass and this week's #458 fix. Position 100 units IS
/// the whole room, so an opening order would be refused here; the full flatten still executes.
#[test]
fn reducing_order_is_never_denied_even_with_zero_room() {
    let row = Row {
        name: "shape/reduce-with-zero-room",
        position: 100.0, // exactly the room: setup passes the gate as an exact fit
        side: -1,
        qty: 100.0,
        im: Some(1.0),
        leverage: Some(1.0),
        equity: 10_000.0,
        ..Row::default()
    };
    let (pos_before, _other, pos_after, dropped) = sim_outcome(&row);
    assert_eq!(pos_before, 100.0, "harness: the setup leg must land whole");
    assert_eq!(pos_after, 0.0, "the flatten must execute in full");
    assert!(dropped.is_empty(), "a reducing order must never be denied: {dropped:?}");
}

/// FIXED (was a pinned known bug): `max_total_exposure` now values the projected position at
/// `|position + side*qty| * mark_price * ctx.multiplier`, folding the contract multiplier exactly
/// like the `order_notional` (min/max notional) and `initial_margin` (buying power) lanes of the
/// same gate. Before the fix it omitted `ctx.multiplier`, so on a mult=10 instrument the exposure
/// cap measured a TENTH of the real exposure and admitted an order ten times too large.
///
/// `SimBroker` has no exposure cap at all, so this never surfaces as a live/backtest DISAGREEMENT
/// — which is precisely why it keeps its own pin here: the matrix above cannot see it. Per the
/// pin's standing instruction, the fix INVERTS this assertion rather than deleting the test.
#[test]
fn max_total_exposure_includes_the_contract_multiplier() {
    let limits = RiskLimits { max_total_exposure: Some(500.0), ..RiskLimits::new() };
    let ctx = RiskContext {
        mark_price: 100.0,
        contract_value: vike_model::ContractValue::linear(10.0),
        ..RiskContext::default()
    };
    let req = OrderRequest {
        client_order_id: "exposure".to_string(),
        venue: VENUE.to_string(),
        symbol: SYM.to_string(),
        order_type: vike_model::OrderType::Market,
        side: 1,
        qty: 2.0,
        ..Default::default()
    };

    // TRUE exposure is 2 * 100 * 10 = 2_000, four times the 500 cap.
    assert_eq!(ctx.contract_value.order_notional(req.qty, ctx.mark_price), 2_000.0);
    // the gate now measures the full 2_000 (multiplier folded in) and DENIES. <-- fixed.
    let v = RiskGate::new(limits).check(&req, &ctx);
    assert!(
        !v.ok && v.reason == "over-max-exposure",
        "max_total_exposure must fold ctx.multiplier: 2*100*10 = 2_000 > 500 cap must DENY: {v:?}"
    );
}

/// The shared #481 predicates are the vocabulary the matrix speaks; pin that each side's real
/// rule is the one the rows assume, so a change to either predicate's meaning breaks here loudly
/// rather than silently re-labelling scenarios.
#[test]
fn the_two_sides_use_the_two_distinct_shared_predicates() {
    // The gate's FLOOR bypass is `is_covered_reduce` (coverage required, flag not trusted alone).
    assert!(vike_model::is_covered_reduce(false, -1, 5.0, 2.0), "covered reduce");
    assert!(!vike_model::is_covered_reduce(true, 1, 0.0, 2.0), "flat + flag is still opening");
    assert!(!vike_model::is_covered_reduce(false, -1, 0.005, 0.008), "a reversal is not covered");

    // `SimBroker::apply_fill`'s opening split is DIRECTION-ONLY — which is exactly why the
    // reversal rows diverge: the same reversal is "not covered" above but "not opening" here.
    assert!(vike_model::is_reducing_direction(-1, 0.005), "the reversal is still a closing fill");
    assert!(!vike_model::is_reducing_direction(1, 0.0), "flat is opening in either direction");
}
