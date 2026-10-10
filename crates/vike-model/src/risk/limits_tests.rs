//! `RiskLimits`' pure lookups and builders, judged without a gate: the per-symbol grid
//! (`grid_for`, the two `from_properties` builders), the initial-margin override (`im_for`) and the
//! account-ceiling fold. The gate's lanes that read them are tested in vike-exec, beside the gate.

use super::*;

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
    use crate::SymbolProperties;
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
    use crate::SymbolProperties;
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
    use crate::SymbolProperties;
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
fn im_for_prefers_per_symbol_then_default() {
    let mut lim = RiskLimits::new();
    lim.im_requirement = Some(0.5); // venue default 2x
    lim.im_by_symbol.insert("BTCUSDT".to_string(), 0.1); // BTC override 10x
    assert_eq!(lim.im_for("BTCUSDT"), Some(0.1));
    assert_eq!(lim.im_for("ETHUSDT"), Some(0.5)); // falls back to default
    assert_eq!(RiskLimits::new().im_for("BTCUSDT"), None); // gate off
}

/// **THE ARMING FOLD NARROWS AND NEVER WIDENS**: `RiskLimits::narrow_account_exposure`, the one
/// operation `vike_mount::make_engine_for_account` and its paper twin arm this ceiling through, so
/// "it can only ever REFUSE" is a property of the operation, not of nobody else writing the field.
/// `min` when both sides carry a number, in BOTH argument orders (a fold taking any incoming value
/// passes a one-directional test), and never `None` over an existing `Some`
/// (`vike_config::VenueMode::cap` is the precedent).
#[test]
fn the_account_ceiling_fold_narrows_and_never_widens() {
    let narrow = |held: Option<f64>, incoming: Option<f64>| {
        let mut lim = RiskLimits { max_account_exposure: held, ..RiskLimits::new() };
        lim.narrow_account_exposure(incoming);
        lim.max_account_exposure
    };
    assert_eq!(narrow(None, None), None, "no ceiling anywhere ⇒ the axis stays off");
    assert_eq!(
        narrow(None, Some(100.0)),
        Some(100.0),
        "the operator's file arms an unarmed engine"
    );
    assert_eq!(
        narrow(Some(100.0), None),
        Some(100.0),
        "a policy that says nothing may not DISARM a ceiling something else already set"
    );
    assert_eq!(narrow(Some(100.0), Some(250.0)), Some(100.0), "the looser incoming value loses");
    assert_eq!(narrow(Some(250.0), Some(100.0)), Some(100.0), "…and the tighter one wins");
}
