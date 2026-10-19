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

// ---- an order judged on its OWN symbol (decision 0173) ----

/// An `AdmitSymbol` row: the grid plus the `on_demand` marker the engine sets.
fn admitted_grid(tick: f64, lot: f64) -> SymbolGrid {
    SymbolGrid {
        tick_size: Some(tick),
        lot_size: Some(lot),
        on_demand: true,
        ..SymbolGrid::default()
    }
}

/// A stop carries `trigger_price` and no `price`: the collar judges the trigger, and it must round
/// it on the ORDER symbol's tick. On the engine-wide tick of 1.0 a 100.4 trigger rounded to 100.0
/// and slipped inside a 0.3 band that its own 0.01 tick puts it outside of.
#[test]
fn the_collar_rounds_a_trigger_on_the_order_symbols_tick() {
    let mut lim = RiskLimits {
        tick_size: Some(1.0),
        price_collar: Some(vike_model::PriceCollar { pct: 0.003, abs_floor: 0.0 }),
        ..RiskLimits::new()
    };
    lim.grid_by_symbol.insert("ETHUSDT".to_string(), admitted_grid(0.01, 0.001));
    let ctx = RiskContext { mark_price: 100.0, ..RiskContext::default() };
    let stop = |symbol: &str| OrderRequest {
        order_type: vike_model::OrderType::Stop,
        trigger_price: Some(100.4),
        ..market_in(symbol, 1, 1.0)
    };
    let mut g = RiskGate::new(lim);
    let v = g.check(&stop("ETHUSDT"), &ctx);
    assert!(!v.ok && v.reason == "price-collar", "0.4 off the mark on a 0.01 tick: {v:?}");
    // The mounted symbol keeps the scalar tick: 100.4 rounds to 100.0, inside the band.
    assert!(g.check(&stop("BTCUSDT"), &ctx).ok, "the mounted symbol is judged as before");
}

/// A market order carries no price, so on a symbol with no mark its notional is ZERO and every
/// price-based cap passes it. On an admitted symbol that is refused with a reason the operator can
/// act on; the cap it would have escaped is the point.
#[test]
fn a_priceless_order_on_an_admitted_symbol_needs_a_reference_price() {
    let mut lim = RiskLimits { max_notional_per_order: Some(1_000.0), ..RiskLimits::new() };
    lim.grid_by_symbol.insert("ETHUSDT".to_string(), admitted_grid(0.01, 0.001));
    let mut g = RiskGate::new(lim);
    let v = g.check(&market_in("ETHUSDT", 1, 5.0), &RiskContext::default());
    assert!(!v.ok && v.reason == "no-reference-price", "no mark, no price: {v:?}");
    // With a mark the notional is real and the cap applies as it always did.
    let ctx = RiskContext { mark_price: 3_000.0, ..RiskContext::default() };
    let v = g.check(&market_in("ETHUSDT", 1, 5.0), &ctx);
    assert!(!v.ok && v.reason == "over-max-notional", "5 x 3000 is over the cap: {v:?}");
    assert!(g.check(&market_in("ETHUSDT", 1, 0.1), &ctx).ok, "0.1 x 3000 fits");
    // A limit order carries its own price, but on an admitted symbol that is not a reference: with no
    // mark the collar and the exposure caps would judge it against itself, so it is denied too
    // (decision 0174); with a mark it goes.
    let v = g.check(&limit_in("ETHUSDT", 1, 0.1), &RiskContext::default());
    assert!(!v.ok && v.reason == "no-reference-price", "a limit order is not its own mark: {v:?}");
    assert!(g.check(&limit_in("ETHUSDT", 1, 0.1), &ctx).ok);
}

/// A covered reduce is never stranded by a missing price, as for every other floor.
#[test]
fn a_covered_reduce_on_an_admitted_symbol_needs_no_reference_price() {
    let mut lim = RiskLimits::new();
    lim.grid_by_symbol.insert("ETHUSDT".to_string(), admitted_grid(0.01, 0.001));
    let ctx = RiskContext { position_size: 1.0, ..RiskContext::default() };
    let reduce = OrderRequest { reduce_only: true, ..market_in("ETHUSDT", -1, 1.0) };
    assert!(RiskGate::new(lim).check(&reduce, &ctx).ok, "the close must reach the venue");
}

/// BYTE-IDENTICAL for everything that is not an admitted symbol: the mounted symbol, and a leg a
/// strategy declared at assembly (a grid row WITHOUT the marker) keep passing a priceless order.
#[test]
fn a_priceless_order_elsewhere_is_judged_as_before() {
    let mut lim = RiskLimits::new();
    lim.grid_by_symbol.insert(
        "LEGUSDT".to_string(),
        SymbolGrid { lot_size: Some(0.001), ..SymbolGrid::default() },
    );
    let mut g = RiskGate::new(lim);
    assert!(g.check(&market_in("BTCUSDT", 1, 1.0), &RiskContext::default()).ok, "mounted");
    assert!(g.check(&market_in("LEGUSDT", 1, 1.0), &RiskContext::default()).ok, "declared leg");
}

fn limit_in(symbol: &str, side: i32, qty: f64) -> OrderRequest {
    OrderRequest { symbol: symbol.to_string(), ..limit(side, qty, 3_000.0) }
}
