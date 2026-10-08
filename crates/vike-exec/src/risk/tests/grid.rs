//! The per-symbol price/size grid: `SymbolGrid` overrides, `grid_for`, the venue-grid builders.

use super::*;
use crate::risk::types::SymbolGrid;

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
