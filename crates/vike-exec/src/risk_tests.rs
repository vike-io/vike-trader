use super::*;
use vike_model::BookLevel;

fn market(side: i32, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "t1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        order_type: "market".to_string(),
        side,
        qty,
        ..Default::default()
    }
}

// ---- per-symbol price/size grid ----

fn market_in(symbol: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest { symbol: symbol.to_string(), ..market(side, qty) }
}

/// THE BUG THE GRID FIXES. An engine's scalars come from ONE symbol's `SymbolProperties`, so a
/// coarse mount lot is applied to a DIFFERENT symbol's order too: `round_to(0.5, Some(1.0))`
/// is `0.0`, and the gate then denies a perfectly valid order as `"non-positive-size"` — a
/// reason that names nothing about the real cause.
#[test]
fn a_coarse_mount_lot_destroys_a_finer_grid_order_without_an_override() {
    let lim = RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
    let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
    assert!(!v.ok, "0.5 rounds to 0.0 on a lot of 1.0");
    assert_eq!(v.reason, "non-positive-size");
}

/// With ITS OWN grid declared, the same order survives: rounded onto 0.001 rather than 1.0.
#[test]
fn a_declared_symbol_is_rounded_onto_its_own_lot() {
    let mut lim = RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
    lim.grid_by_symbol.insert(
        "ETHUSDT".to_string(),
        SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
    );
    let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
    assert!(v.ok, "the finer lot must admit it: {v:?}");
    let admitted = v.request.expect("an admitted verdict carries the request");
    assert!((admitted.qty - 0.5).abs() < 1e-12, "qty {} != 0.5", admitted.qty);
}

/// An override is per SYMBOL, not global: the engine's own symbol keeps the scalars.
#[test]
fn an_override_does_not_leak_to_other_symbols() {
    let mut lim = RiskLimits { lot_size: Some(1.0), ..RiskLimits::new() };
    lim.grid_by_symbol.insert(
        "ETHUSDT".to_string(),
        SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
    );
    let v = RiskGate::new(lim).check(&market(1, 0.5), &RiskContext::default());
    assert!(!v.ok, "BTCUSDT still rounds on the 1.0 scalar");
}

/// Fallback is FIELD-BY-FIELD: an override that pins only `lot_size` must still inherit the
/// engine's `min_qty`. A half-specified grid must not become a way to switch a floor off.
#[test]
fn a_partial_override_still_inherits_the_engines_floors() {
    let mut lim = RiskLimits { lot_size: Some(1.0), min_qty: Some(10.0), ..RiskLimits::new() };
    lim.grid_by_symbol.insert(
        "ETHUSDT".to_string(),
        SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
    );
    let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
    assert!(!v.ok, "0.5 clears the finer lot but not the inherited min_qty of 10");
    assert_eq!(v.reason, "below-min-qty");
}

/// A symbol's own floors apply when it declares them.
#[test]
fn a_declared_symbol_uses_its_own_min_qty() {
    let mut lim = RiskLimits { lot_size: Some(1.0), min_qty: Some(10.0), ..RiskLimits::new() };
    lim.grid_by_symbol.insert(
        "ETHUSDT".to_string(),
        SymbolGrid { lot_size: Some(0.001), min_qty: Some(0.1), ..SymbolGrid::default() },
    );
    let v = RiskGate::new(lim).check(&market_in("ETHUSDT", 1, 0.5), &RiskContext::default());
    assert!(v.ok, "its own 0.1 floor admits 0.5: {v:?}");
}

/// `SymbolGrid::from_properties` and `RiskLimits::from_properties` must read ONE
/// `SymbolProperties` the same way — otherwise a mount's own symbol and its declared leg would
/// be judged on two different readings of the same venue payload, which is a worse failure than
/// the missing-grid one the map exists to fix.
///
/// NON-VACUOUS: the four fields carry four DISTINCT values, so a swapped pair (the realistic
/// drift — `step_size` feeds `lot_size`, not `tick_size`) fails; a wholesale `Default` in either
/// builder fails; and adding a fifth mapped field to one builder alone fails as soon as it is
/// asserted here. An all-zero fixture would pass against almost any wrong mapping, so the
/// values are deliberately unequal.
#[test]
fn a_symbol_grid_matches_the_scalar_builder_field_for_field() {
    use vike_model::SymbolProperties;
    let f = SymbolProperties {
        tick_size: 0.5,
        step_size: 0.1,
        min_qty: 0.01,
        min_notional: 5.0,
        ..Default::default()
    };
    let scalars = RiskLimits::from_properties(&f);
    let g = SymbolGrid::from_properties(&f);
    assert_eq!(g.tick_size, scalars.tick_size);
    assert_eq!(g.lot_size, scalars.lot_size);
    assert_eq!(g.min_qty, scalars.min_qty);
    assert_eq!(g.min_notional, scalars.min_notional);
    // …and the mapping itself, spelled out, so this cannot pass by both builders being wrong
    // in the same direction.
    assert_eq!(
        (g.tick_size, g.lot_size, g.min_qty, g.min_notional),
        (Some(0.5), Some(0.1), Some(0.01), Some(5.0))
    );
}

/// THE DECLARED RESIDUAL (see [`SymbolGrid::from_properties`]'s ⚠): a venue field of `0.0`
/// means UNCONSTRAINED, but `nz_step` folds it to `None` and `None` in a `SymbolGrid` means
/// INHERIT — so this leg still rounds on the mounted symbol's lot. Pinned, not fixed: closing it
/// needs a spelling for "explicitly unconstrained", which changes the serialized `RiskLimits`
/// shape that feeds the journal determinism fence.
///
/// NON-VACUOUS: it asserts the INHERITED `0.001`, not merely `is_none()` on the override — a
/// future `from_properties` that mapped `0.0` to a real "no rounding" answer would return
/// `None` from `grid_for` here and fail, which is exactly the signal wanted if somebody closes
/// this without deleting the pin.
#[test]
fn a_zero_field_from_the_venue_inherits_the_mount_scalar() {
    use vike_model::SymbolProperties;
    let mut lim = RiskLimits { lot_size: Some(0.001), ..RiskLimits::new() };
    // the venue publishes NO lot for this leg
    let leg = SymbolProperties { tick_size: 0.01, step_size: 0.0, ..Default::default() };
    lim.grid_by_symbol.insert("ETHUSDT".to_string(), SymbolGrid::from_properties(&leg));
    let g = lim.grid_for("ETHUSDT");
    assert_eq!(g.tick_size, Some(0.01), "the leg's own tick is applied");
    assert_eq!(
        g.lot_size,
        Some(0.001),
        "an unconstrained leg lot INHERITS the mount scalar — the declared residual"
    );
}

/// An EMPTY map is the identity: every verdict is exactly what it was before the grid existed.
#[test]
fn an_empty_grid_map_is_byte_identical() {
    let lim = RiskLimits { lot_size: Some(0.01), min_qty: Some(0.05), ..RiskLimits::new() };
    assert!(lim.grid_by_symbol.is_empty());
    let g = lim.grid_for("ANYTHING");
    assert_eq!(g.lot_size, lim.lot_size);
    assert_eq!(g.tick_size, lim.tick_size);
    assert_eq!(g.min_qty, lim.min_qty);
    assert_eq!(g.min_notional, lim.min_notional);
}

#[test]
fn from_filters_maps_with_zero_as_none() {
    use vike_model::SymbolProperties;
    let f = SymbolProperties {
        tick_size: 0.5,
        step_size: 0.1,
        min_qty: 0.01,
        min_notional: 5.0,
        ..Default::default()
    };
    let l = RiskLimits::from_properties(&f);
    assert_eq!(l.tick_size, Some(0.5));
    assert_eq!(l.lot_size, Some(0.1)); // step_size -> lot_size
    assert_eq!(l.min_qty, Some(0.01));
    assert_eq!(l.min_notional, Some(5.0));
    // all-0.0 -> all None
    let z = RiskLimits::from_properties(&SymbolProperties::default());
    assert_eq!((z.tick_size, z.lot_size, z.min_qty, z.min_notional), (None, None, None, None));
    assert_eq!(z.window_ms, 1000); // inherits RiskLimits::new() defaults
}

#[test]
fn check_rejects_below_min_qty() {
    let mut gate = RiskGate::new(RiskLimits { min_qty: Some(1.0), ..RiskLimits::new() });
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };

    let verdict = gate.check(&market(1, 0.5), &ctx);
    assert!(!verdict.ok && verdict.reason == "below-min-qty", "got {:?}", verdict);

    let verdict_ok = gate.check(&market(1, 1.0), &ctx);
    assert!(verdict_ok.ok || verdict_ok.reason != "below-min-qty", "got {:?}", verdict_ok);
}

/// LIVE-VS-BACKTEST DIVERGENCE FIX (finding A): a pure reduce/close below `min_qty` was
/// DENIED live while `SimBroker::apply_fill` fills it in backtest ("a closing fill must
/// ALWAYS execute so a position is never stranded below-min") — stranding exactly the
/// position that rule protects. The gate now applies its own `pure_reduce` bypass — the one
/// margin and impact already used — to the min floors. A NON-reducing below-min order still
/// denies exactly as before.
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

/// Finding A, min-notional twin: same anti-stranding exemption, same non-reduce pin.
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

/// B9 FIX (fail-before/pass-after): the `max_total_exposure` lane folds `ctx.multiplier` into
/// its projected exposure, exactly like its `min_notional`/per-order-cap and `initial_margin`
/// siblings in the SAME gate. For a multiplier != 1 instrument (options, inverse perps) the
/// pre-fix line under-counted projected exposure by the multiplier factor. Multiplier-1 stays
/// bit-identical, so this test arms a multiplier of 10 to prove the fold is present.
#[test]
fn max_total_exposure_includes_the_contract_multiplier() {
    // flat book, BUY 2 @ mark 100 with a x10 multiplier ⇒ projected exposure = 2 * 100 * 10 =
    // 2_000. Pre-fix the multiplier was dropped ⇒ projected = 200.
    let ctx = RiskContext { mark_price: 100.0, multiplier: 10.0, ..RiskContext::default() };
    // cap 1_000 sits BETWEEN the two: 200 (pre-fix, would PASS) and 2_000 (post-fix, DENIES).
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
// The law: a halt stops OPENING risk and must never TRAP the operator in a position. Until
// this block existed `Halted` denied every order, so `market-exit`'s flatten legs came back
// `OrderDenied` and the panic button was disarmed exactly in the situations that reach
// `Halted` on their own. `docs/ops/kill-switches.md` is the operator-facing statement.

/// The FIX itself: the shape `OrderIntent::Flatten` mints — a `reduce_only` MARKET for exactly
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

/// #600-P4 FIX: a `reduce_only`-tagged SAME-DIRECTION ADD (exposure-INCREASING but
/// magnitude-covered — long 5, BUY 2 tagged reduce_only) is NOT a covered reduce. The gate
/// must treat it as the OPENING order it is on ALL THREE bypasses that read
/// `is_covered_reduce` — the min floors, buying power, and the impact veto — matching
/// `SimBroker::apply_fill`'s direction-only opening split and the real venues (binance/bybit)
/// that reject a reduce_only order which would increase the position. Pre-fix the coverage-only
/// flag arm laundered it past all three.
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
    // order margin = 2 * 100 * 0.1 = 20; equity 10 ⇒ DENY (pre-fix: bypassed, admitted).
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

/// LIVE-VS-BACKTEST DIVERGENCE FIX (finding B): the gate's notional was `qty × price` with
/// NO contract multiplier, while `SimBroker` gates fills on `rounded × price × multiplier`
/// and the gate's OWN margin calc already used `ctx.multiplier` — the same `min_notional`
/// gated differently live vs backtest for multiplier != 1 instruments.
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
    // COMPAT PIN: multiplier 1.0 — the default, i.e. nearly every instrument — reproduces the
    // exact pre-fix verdicts (the same boundary cases pinned in
    // `ordinary_positive_price_verdicts_are_unchanged_by_the_abs`; `x * 1.0` is bit-exact).
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

/// A limit order whose `price` is a SIGNED combo net (`ComboSpec::net_limit`) — the shape a
/// `Combo` takes through the gate once PR-2 lowers it.
fn combo_limit(side: i32, qty: f64, net: f64) -> OrderRequest {
    use vike_model::ComboLeg;
    OrderRequest {
        client_order_id: "c1".to_string(),
        venue: "deribit".to_string(),
        symbol: String::new(),
        order_type: "limit".to_string(),
        side,
        qty,
        price: Some(net),
        combo_legs: vec![
            ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
            ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
        ],
        ..Default::default()
    }
}

#[test]
fn notional_is_a_magnitude_so_credit_combos_gate_like_debit_ones() {
    // REGRESSION: `notional = qty.abs() * ref_price` (SIGNED) went NEGATIVE for a credit
    // combo, so (a) `notional < min_notional` denied EVERY credit combo, and (b)
    // `notional > cap` could never trip, letting an arbitrarily large one escape the cap.
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

// ---- pre-trade impact veto (opt-in) ----

/// asks 100@1, 101@2, 102@3 ; bids 99@1, 98@2, 97@3 ⇒ mid = 99.5, tick 1.0
fn book() -> L2Book {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(
        1,
        &[BookLevel::new(99.0, 1.0), BookLevel::new(98.0, 2.0), BookLevel::new(97.0, 3.0)],
        &[BookLevel::new(100.0, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)],
    );
    b
}

#[test]
fn impact_veto_none_budget_never_denies() {
    let b = book();
    // even a size the book cannot fill at all passes when the knob is off
    assert_eq!(impact_veto(&b, 1, 1e9, None), None);
    assert_eq!(impact_veto(&b, -1, 1e9, None), None);
    assert_eq!(impact_veto(&L2Book::new(1.0), 1, 5.0, None), None);
}

#[test]
fn impact_veto_empty_book_not_fillable() {
    let empty = L2Book::new(1.0);
    assert_eq!(impact_veto(&empty, 1, 1.0, Some(1e9)), Some(ImpactDeny::NotFillable));
    assert_eq!(impact_veto(&empty, -1, 1.0, Some(1e9)), Some(ImpactDeny::NotFillable));
    assert_eq!(fillable_veto(&empty, 1, 1.0), Some(ImpactDeny::NotFillable));
    // qty 0 is vacuously fillable
    assert_eq!(fillable_veto(&empty, 1, 0.0), None);
}

#[test]
fn impact_veto_partial_walk_is_not_fillable() {
    let b = book();
    // 6 units exhausts each side exactly; 7 cannot fill ⇒ slippage is unbounded ⇒ deny
    assert_eq!(fillable_veto(&b, 1, 6.0), None);
    assert_eq!(impact_veto(&b, 1, 7.0, Some(1e9)), Some(ImpactDeny::NotFillable));
    assert_eq!(impact_veto(&b, -1, 7.0, Some(1e9)), Some(ImpactDeny::NotFillable));
    assert_eq!(fillable_veto(&b, -1, 7.0), Some(ImpactDeny::NotFillable));
}

#[test]
fn impact_veto_exact_fill_both_sides_and_budget_boundary() {
    let b = book();
    // BUY 3 → 100×1 + 101×2 = 302 / 3 = 100.6667 ; mid 99.5 ⇒ +117.25 bps
    let buy = b.simulate_fill(1, 3.0).slippage_bps_vs_mid.unwrap();
    assert!(buy > 117.0 && buy < 118.0, "buy slippage {buy}");
    // SELL 3 → 99×1 + 98×2 = 295 / 3 = 98.3333 ; below mid ⇒ positive (worse for taker)
    let sell = b.simulate_fill(-1, 3.0).slippage_bps_vs_mid.unwrap();
    assert!(sell > 117.0 && sell < 118.0, "sell slippage {sell}");

    // budget-EQUAL passes (strict >), a hair under denies
    assert_eq!(impact_veto(&b, 1, 3.0, Some(buy)), None);
    assert_eq!(impact_veto(&b, 1, 3.0, Some(buy - 1e-9)), Some(ImpactDeny::OverSlippageBudget));
    assert_eq!(impact_veto(&b, -1, 3.0, Some(sell)), None);
    assert_eq!(impact_veto(&b, -1, 3.0, Some(sell - 1e-9)), Some(ImpactDeny::OverSlippageBudget));
    // touching only the top of book is cheapest and passes a tight budget
    assert_eq!(impact_veto(&b, 1, 1.0, Some(51.0)), None);
}

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

/// The IMPACT bypass moved onto `is_covered_reduce` together with the margin bypass: the
/// anti-stranding rationale that justifies it is a statement about a POSITION, so a flat
/// book has nothing to protect and a mis-tagged order must not skip the veto.
#[test]
fn impact_veto_bypass_requires_position_coverage() {
    let b = book();
    let armed = || RiskLimits { require_fillable: true, ..RiskLimits::new() };
    let ro = |side, qty| OrderRequest { reduce_only: true, ..market(side, qty) };
    // FLAT book + the flag, size beyond the displayed depth ⇒ vetoed (was: bypassed).
    let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let v = RiskGate::new(armed()).check_with_book(&ro(1, 6.5), &flat, Some(&b));
    assert!(!v.ok && v.reason == "impact-not-fillable", "got {v:?}");
    // COVERED reduce of the same size ⇒ still bypasses (anti-stranding preserved): the
    // exit must go through exactly when the book looks worst.
    let long = RiskContext { position_size: 10.0, mark_price: 100.0, ..RiskContext::default() };
    let c = RiskGate::new(armed()).check_with_book(&ro(-1, 6.5), &long, Some(&b));
    assert!(c.ok, "covered reduce must still bypass the impact veto; got {c:?}");
}

fn limit(side: i32, qty: f64, px: f64) -> OrderRequest {
    OrderRequest { order_type: "limit".to_string(), price: Some(px), ..market(side, qty) }
}

/// REGRESSION (review major #2): a PASSIVE limit pays no slippage and must never be
/// impact-vetoed — the pre-fix gate walked it as if it were a market taker, which denied
/// every `SpreadMaker` quote under an armed budget.
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

/// REGRESSION (review major #3): a closing order must never be stranded by the impact gate.
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

/// The two public pure fns must agree on degenerate size (review minor).
#[test]
fn impact_and_fillable_agree_on_non_positive_qty() {
    let b = book();
    for side in [1, -1] {
        assert_eq!(fillable_veto(&b, side, 0.0), None);
        assert_eq!(impact_veto(&b, side, 0.0, Some(0.0)), None);
        assert_eq!(fillable_veto(&b, side, -1.0), None);
        assert_eq!(impact_veto(&b, side, -1.0, Some(0.0)), None);
    }
}

// ---- combo crossing (spec §5: atomic per-leg, ONE throttle slot) ----

const LEG_A: &str = "BTC-27MAR26-100000-C";
const LEG_B: &str = "BTC-27MAR26-120000-C";

/// per-leg marks: LEG_A 50, LEG_B 30 ⇒ a 1×/−1× call spread nets +20 (debit)
fn leg_marks(sym: &str) -> RiskContext {
    let mark = match sym {
        LEG_A => 50.0,
        LEG_B => 30.0,
        _ => 0.0,
    };
    RiskContext { mark_price: mark, ..RiskContext::default() }
}

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
    // MAJOR: each leg gets a FRESH lctx, so without accumulation N legs each fit in the same
    // unchanged free BP. im 0.1, equity 100k, marks 50/30, 12_000 units:
    //   leg A margin = 12_000×50×0.1 = 60_000, leg B = 12_000×30×0.1 = 36_000.
    // Individually both fit in 100k; together they need 96_000 — which still fits, so push
    // leg A to 1.6k units... use a size where the PAIR overflows but each leg alone does not.
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
    // `ComboSpec::validate` does NOT reject a duplicate leg symbol, and each leg got a fresh
    // lctx, so the exposure cap measured the SAME 0-position twice instead of the sum.
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
    // MAJOR: `leg_req` inherited `reduce_only`, and `pure_reduce` is `req.reduce_only || ..`,
    // so EVERY leg skipped buying power and passed the `Reducing` gate — including a leg
    // opening a brand-new position in an instrument never held.
    let mut req = combo_limit(1, 100.0, 20.0);
    req.reduce_only = true;
    // long LEG_A (the +1 leg genuinely reduces nothing — it BUYS more), flat LEG_B.
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
    // a 0 ratio used to surface as the misleading `leg X: invalid-side`
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

// ---- fat-finger price collar (OPT-IN) ----

/// `pct` fraction + absolute floor, as the venue-wide default (no per-symbol rows).
fn collared(pct: f64, abs_floor: f64) -> RiskLimits {
    RiskLimits { price_collar: Some(PriceCollar { pct, abs_floor }), ..RiskLimits::new() }
}

/// A stop/take-profit carries `trigger_price` and NO `price` — the collar must judge it too.
fn trigger_order(side: i32, qty: f64, trigger: f64) -> OrderRequest {
    OrderRequest {
        order_type: "stop".to_string(),
        trigger_price: Some(trigger),
        ..market(side, qty)
    }
}

#[test]
fn price_collar_denies_a_fat_finger_in_both_directions() {
    // mark 100, band = max(0.05 * 100, 0.0) = 5.0
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    // 10x UP — the classic mis-scale. EVERY pre-existing axis admits it (no cap is set, and
    // its notional would fit under an ordinary one anyway): this is the hole the axis closes.
    let mut g = RiskGate::new(collared(0.05, 0.0));
    let v = g.check(&limit(1, 1.0, 1_000.0), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a 10x-up limit must deny: {v:?}");
    // 10x DOWN — a sell at a tenth of the mark is the same fat finger
    let v = g.check(&limit(-1, 1.0, 10.0), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a 10x-down limit must deny: {v:?}");
    // a hair outside the band on each side
    let v = g.check(&limit(1, 1.0, 105.01), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "just above the band: {v:?}");
    let v = g.check(&limit(-1, 1.0, 94.99), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "just below the band: {v:?}");
    // a TRIGGER price is judged the same way (a stop carries price = None)
    let v = g.check(&trigger_order(1, 1.0, 200.0), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a fat-finger trigger must deny: {v:?}");
    // a non-finite price is denied outright, never silently admitted (`NaN > band` is false)
    let v = g.check(&limit(1, 1.0, f64::NAN), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a NaN price must deny: {v:?}");

    // a collar denial must not burn a rate slot — the axis runs BEFORE the throttle, like
    // the margin and impact vetoes.
    let mut lim = collared(0.05, 0.0);
    lim.max_orders_per_window = Some(1);
    let mut t = RiskGate::new(lim);
    assert!(!t.check(&limit(1, 1.0, 1_000.0), &ctx).ok);
    assert!(t.throttle_times().is_empty(), "a collared order must not burn a rate slot");
    assert!(t.check(&limit(1, 1.0, 100.0), &ctx).ok, "the slot must still be available");
}

#[test]
fn price_collar_allows_inside_the_band() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let mut g = RiskGate::new(collared(0.05, 0.0));
    for px in [96.0, 100.0, 104.0] {
        let v = g.check(&limit(1, 1.0, px), &ctx);
        assert!(v.ok, "price {px} is inside the +/-5 band: {v:?}");
    }
    // band-EQUAL passes on both edges (the comparison is strict `>`)
    let v = g.check(&limit(1, 1.0, 105.0), &ctx);
    assert!(v.ok, "the upper band edge must pass: {v:?}");
    let v = g.check(&limit(-1, 1.0, 95.0), &ctx);
    assert!(v.ok, "the lower band edge must pass: {v:?}");
    // an in-band trigger passes too
    let v = g.check(&trigger_order(-1, 1.0, 96.0), &ctx);
    assert!(v.ok, "an in-band trigger must pass: {v:?}");
    // a MARKET order carries neither price nor trigger => never collared, at any band
    let mut zero = RiskGate::new(collared(0.0, 0.0));
    let v = zero.check(&market(1, 1.0), &ctx);
    assert!(v.ok, "a market order has no price to collar: {v:?}");
}

/// Why BOTH halves of the band are required: on a cheap instrument (the Polymarket shape — a
/// 0.02 mark) a pure percentage collar denies ordinary quoting, so the absolute floor must
/// dominate. Above it the percentage takes back over.
#[test]
fn price_collar_absolute_floor_dominates_at_a_tiny_mark() {
    let ctx = RiskContext { mark_price: 0.02, ..RiskContext::default() };
    // pct alone: 10% of 0.02 = 0.002, so an ordinary 0.025 quote would be DENIED
    let v = RiskGate::new(collared(0.10, 0.0)).check(&limit(1, 100.0, 0.025), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a pct-only collar is too tight: {v:?}");
    // with a 0.01 absolute floor the band is max(0.002, 0.01) = 0.01 => 0.025 is admitted
    let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(1, 100.0, 0.025), &ctx);
    assert!(v.ok, "the absolute floor must widen the band at a tiny mark: {v:?}");
    // ...and the mis-scale this axis exists for is STILL caught: 0.55 -> 5.5 in miniature,
    // here 0.02 -> 0.055, which is 0.035 away
    let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(1, 100.0, 0.055), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a mis-scaled 0.055 must deny: {v:?}");
    // the DOWN mis-scale (0.02 -> 0.002) likewise
    let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(-1, 100.0, 0.002), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "a mis-scaled 0.002 must deny: {v:?}");
    // symmetrically, on an EXPENSIVE mark the percentage dominates that same 0.01 floor:
    // band = max(0.10 * 50_000, 0.01) = 5_000, so 52_000 is admitted.
    let rich = RiskContext { mark_price: 50_000.0, ..RiskContext::default() };
    let v = RiskGate::new(collared(0.10, 0.01)).check(&limit(1, 1.0, 52_000.0), &rich);
    assert!(v.ok, "the percentage must govern an expensive mark: {v:?}");
    // the band fn itself, pinned at both ends
    let c = PriceCollar { pct: 0.10, abs_floor: 0.01 };
    assert_eq!(c.band(50_000.0), 5_000.0);
    assert_eq!(c.band(0.02), 0.01);
}

/// An UNPRICED mark skips the axis entirely — the mount's readiness gate guarantees
/// priced-before-order-flow, so denying here would be a pure false positive.
#[test]
fn price_collar_skips_an_unpriced_mark() {
    for mark in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let ctx = RiskContext { mark_price: mark, ..RiskContext::default() };
        let v = RiskGate::new(collared(0.05, 0.0)).check(&limit(1, 1.0, 1_000.0), &ctx);
        assert!(v.ok, "an unpriced mark ({mark}) must skip the collar: {v:?}");
    }
}

/// A COMBO's `price` is the SIGNED NET across legs, not a price in any single instrument's
/// mark units — collaring it would deny every combo (a credit net is NEGATIVE). Its LEGS are
/// not silently un-checked either: `check_combo` clears each leg's price/trigger, so a leg
/// carries nothing to collar and prices off its own mark exactly as it does today.
#[test]
fn price_collar_never_judges_a_combo_net_price() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    // a +20 debit net and a -20 credit net are both far outside a +/-5 band around 100
    let v = RiskGate::new(collared(0.05, 0.0)).check(&combo_limit(1, 2.0, 20.0), &ctx);
    assert!(v.ok, "a debit combo net must not be collared: {v:?}");
    let v = RiskGate::new(collared(0.05, 0.0)).check(&combo_limit(-1, 2.0, -20.0), &ctx);
    assert!(v.ok, "a credit combo net must not be collared: {v:?}");
    // the combo entry point is likewise unaffected
    let mut g = RiskGate::new(collared(0.05, 0.0));
    let v = g.check_combo(&combo_limit(1, 2.0, 20.0), &RiskContext::default(), leg_marks);
    assert!(v.ok, "check_combo must be unaffected by an armed collar: {v:?}");
}

/// Per-symbol override, mirroring `im_by_symbol`/`im_for`: the map wins where it has a row,
/// the venue default covers everything else.
#[test]
fn price_collar_per_symbol_override_mirrors_im_by_symbol() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let row = PriceCollar { pct: 0.05, abs_floor: 0.0 };
    let tight = PriceCollar { pct: 0.0, abs_floor: 0.0 };
    let mut map: indexmap::IndexMap<String, PriceCollar> = indexmap::IndexMap::new();
    map.insert("BTCUSDT".to_string(), row);

    // no venue default at all — only a BTCUSDT row (the symbol `market()`/`limit()` build)
    let only_btc = RiskLimits { collar_by_symbol: map.clone(), ..RiskLimits::new() };
    assert_eq!(only_btc.collar_for("BTCUSDT"), Some(row));
    assert_eq!(only_btc.collar_for("ETHUSDT"), None);
    let v = RiskGate::new(only_btc.clone()).check(&limit(1, 1.0, 1_000.0), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "the BTC row must arm the axis: {v:?}");
    // an uncovered symbol with no venue default stays unarmed
    let mut eth = limit(1, 1.0, 1_000.0);
    eth.symbol = "ETHUSDT".to_string();
    let v = RiskGate::new(only_btc).check(&eth, &ctx);
    assert!(v.ok, "a symbol with no row and no default must be unarmed: {v:?}");

    // the per-symbol row OVERRIDES a brutally tight venue default...
    let mixed =
        RiskLimits { price_collar: Some(tight), collar_by_symbol: map, ..RiskLimits::new() };
    assert_eq!(mixed.collar_for("BTCUSDT"), Some(row));
    assert_eq!(mixed.collar_for("ETHUSDT"), Some(tight));
    let v = RiskGate::new(mixed.clone()).check(&limit(1, 1.0, 104.0), &ctx);
    assert!(v.ok, "the per-symbol row must override the venue default: {v:?}");
    // ...while the tight default still governs every OTHER symbol
    let mut eth2 = limit(1, 1.0, 104.0);
    eth2.symbol = "ETHUSDT".to_string();
    let v = RiskGate::new(mixed).check(&eth2, &ctx);
    assert!(!v.ok && v.reason == "price-collar", "the default must govern ETH: {v:?}");
}

/// THE OFF TEST — the byte-identical-when-unset guarantee, both halves:
/// (a) with the axis `None`/empty an order that WOULD trip it is admitted, and
/// (b) neither new key reaches the canonical JSON, so `EngineSnapshot`'s `state_hash` (the
///     journal determinism fence) is unchanged — see
///     `engine_snapshot::tests::state_hash_of_a_default_limits_snapshot_is_pinned`.
#[test]
fn price_collar_off_is_byte_identical() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let off = RiskLimits::new();
    assert_eq!(off.price_collar, None);
    assert!(off.collar_by_symbol.is_empty());
    assert_eq!(off.collar_for("BTCUSDT"), None);
    // a 100x limit, a 100x trigger and a NaN price — all admitted with the axis off
    let cases = [limit(1, 1.0, 10_000.0), trigger_order(1, 1.0, 10_000.0), limit(1, 1.0, f64::NAN)];
    for req in cases {
        let v = RiskGate::new(RiskLimits::new()).check(&req, &ctx);
        assert!(v.ok, "the OFF path must admit exactly as before: {req:?} -> {v:?}");
    }
    // the serialized shape is untouched: no key, not even a null
    let json = serde_json::to_string(&RiskLimits::new()).unwrap();
    assert!(
        !json.contains("price_collar") && !json.contains("collar_by_symbol"),
        "an off collar must not reach the canonical JSON: {json}"
    );
    // `Default` (not just `new()`) is off too — it is what serde reconstructs a pre-collar
    // journal record into, and the round trip must land back on the same value.
    let d = RiskLimits::default();
    assert_eq!(d.price_collar, None);
    assert!(d.collar_by_symbol.is_empty());
    let round: RiskLimits = serde_json::from_str(&json).unwrap();
    assert_eq!(round, RiskLimits::new());
}

/// REGRESSION (review major #1): an ARMED COLLAR MUST NOT DENY A PROTECTIVE BRACKET EXIT.
/// `vike_model::build_bracket` emits its stop-loss (`order_type: "stop"`, `trigger_price`) and
/// take-profit (`limit`, `price`) legs DELIBERATELY far from the mark — that distance IS the
/// protection — so collaring them would veto exactly the order that limits the loss. The
/// covered-reduce bypass (the codebase's standing anti-stranding rule) is what prevents it.
#[test]
fn price_collar_never_denies_a_protective_bracket_exit() {
    use vike_model::BracketSpec;
    // long 1.0 at a mark of 100, under a brutally tight +/-2 collar
    let held = RiskContext { position_size: 1.0, mark_price: 100.0, ..RiskContext::default() };
    // a MARKET entry (no price to collar), a stop 10 below the mark and a take-profit 20
    // above it — both exits 5x/10x outside the +/-2 band, BY DESIGN.
    let spec = BracketSpec {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        entry_price: None,
        stop_loss: 90.0,
        take_profit: 120.0,
    };
    let [entry, sl, tp] = vike_model::build_bracket(&spec, "e1", "s1", "t1");
    // the exits are reduce-only, opposite-side and covered by the held position
    assert!(sl.reduce_only && sl.trigger_price == Some(90.0) && sl.order_type == "stop");
    assert!(tp.reduce_only && tp.price == Some(120.0) && tp.order_type == "limit");
    for leg in [&entry, &sl, &tp] {
        let v = RiskGate::new(collared(0.02, 0.0)).check(leg, &held);
        assert!(v.ok, "an armed collar must not deny a bracket leg: {leg:?} -> {v:?}");
    }

    // ...and the bypass is POSITION-COVERED, not flag-trusting: on a FLAT book the very same
    // legs open exposure (nothing server-side reduces either), so the collar still bites.
    let flat = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    for leg in [&sl, &tp] {
        let v = RiskGate::new(collared(0.02, 0.0)).check(leg, &flat);
        assert!(
            !v.ok && v.reason == "price-collar",
            "a flat-book 'reduce_only' leg is an OPENING order and must stay collared: {v:?}"
        );
    }

    // the OPENING leg is never bypassed either: a fat-fingered LIMIT entry still denies.
    let mut far_entry = spec.clone();
    far_entry.entry_price = Some(1_000.0);
    let [bad_entry, _, _] = vike_model::build_bracket(&far_entry, "e2", "s2", "t2");
    let v = RiskGate::new(collared(0.02, 0.0)).check(&bad_entry, &held);
    assert!(!v.ok && v.reason == "price-collar", "a fat-finger entry must deny: {v:?}");
}

/// A garbage collar config must FAIL OPEN, never closed. Pre-fix, `band()` propagated a
/// negative/`NaN` band straight into `|p - mark| > band` — always true — so the opt-in
/// fat-finger knob silently became a TOTAL KILL SWITCH denying every priced order.
#[test]
fn price_collar_garbage_config_is_never_a_kill_switch() {
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let garbage = [
        PriceCollar { pct: f64::NAN, abs_floor: 0.0 },
        PriceCollar { pct: -0.5, abs_floor: 0.0 },
        PriceCollar { pct: 0.0, abs_floor: f64::NAN },
        PriceCollar { pct: 0.0, abs_floor: -1.0 },
        PriceCollar { pct: f64::NAN, abs_floor: f64::NAN },
        PriceCollar { pct: -0.5, abs_floor: -1.0 },
        PriceCollar { pct: f64::NEG_INFINITY, abs_floor: f64::NEG_INFINITY },
    ];
    for c in garbage {
        let band = c.band(100.0);
        assert!(band.is_finite() && band >= 0.0, "{c:?} must yield a sane band, got {band}");
        let lim = RiskLimits { price_collar: Some(c), ..RiskLimits::new() };
        // an AT-THE-MARK limit — the order a kill switch would have denied — is admitted
        let v = RiskGate::new(lim.clone()).check(&limit(1, 1.0, 100.0), &ctx);
        assert!(v.ok, "{c:?} must not deny an at-the-mark limit: {v:?}");
        // so is an at-the-mark trigger, and a market order (no price at all)
        let v = RiskGate::new(lim.clone()).check(&trigger_order(-1, 1.0, 100.0), &ctx);
        assert!(v.ok, "{c:?} must not deny an at-the-mark trigger: {v:?}");
        let v = RiskGate::new(lim).check(&market(1, 1.0), &ctx);
        assert!(v.ok, "{c:?} must not deny a market order: {v:?}");
    }
    // a garbage half does not poison a VALID half: the surviving component still governs
    assert_eq!(PriceCollar { pct: f64::NAN, abs_floor: 0.02 }.band(100.0), 0.02);
    assert_eq!(PriceCollar { pct: -0.5, abs_floor: 0.02 }.band(100.0), 0.02);
    // ...and the two WHOLLY-garbage shapes — the ones that pre-fix produced a `NaN` and a
    // NEGATIVE band, i.e. the actual kill switch — collapse to the TIGHTEST LEGAL band (0.0),
    // exactly what an explicit `PriceCollar { pct: 0.0, abs_floor: 0.0 }` already means here.
    assert_eq!(PriceCollar { pct: f64::NAN, abs_floor: f64::NAN }.band(100.0), 0.0);
    assert_eq!(PriceCollar { pct: -0.5, abs_floor: -1.0 }.band(100.0), 0.0);
}
