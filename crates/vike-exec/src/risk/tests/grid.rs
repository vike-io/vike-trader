//! The per-symbol price/size grid through the gate: `SymbolGrid` overrides as `check` applies them.
//! The pure lookups and builders (`grid_for`, the two `from_properties`) are tested beside them, in
//! `crates/vike-model/src/risk/limits_tests.rs`.

use super::*;
use vike_model::SymbolGrid;

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
