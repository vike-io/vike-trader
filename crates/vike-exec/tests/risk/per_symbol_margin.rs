//! The gate's buying-power lane with a per-symbol initial-margin rate (`SymbolAdmission::im_requirement`
//! writes `RiskLimits::im_by_symbol`, which `RiskLimits::im_for` reads ahead of the engine-wide
//! `im_requirement`): a 1x spot-like symbol beside a 4x perp is margined at each one's OWN rate, in the
//! single-order lane and across the legs of a combo (each admitted leg commits margin at its own
//! rate). The engine-level cases (admission, `ctx.margin_used`, the margin fold, restore) are
//! `tests/engine/quote_margin.rs`.

use vike_exec::{RiskContext, RiskGate};
use vike_model::{ComboLeg, OrderRequest, OrderType, RiskLimits};

fn limits(engine_wide: Option<f64>, rows: &[(&str, f64)]) -> RiskLimits {
    let mut l = RiskLimits { im_requirement: engine_wide, ..RiskLimits::new() };
    for (symbol, im) in rows {
        l.im_by_symbol.insert((*symbol).to_string(), *im);
    }
    l
}

fn ctx(equity: f64) -> RiskContext {
    RiskContext { mark_price: 100.0, equity, ..RiskContext::default() }
}

/// A limit buy of `qty` at 100, so `qty * 100` of notional.
fn buy(symbol: &str, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: "c1".into(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    }
}

fn reason(l: &RiskLimits, symbol: &str, qty: f64, equity: f64) -> Option<String> {
    let v = RiskGate::new(l.clone()).check(&buy(symbol, qty), &ctx(equity));
    (!v.ok).then_some(v.reason)
}

/// A 600 notional order against 500 of equity: refused at 1x (600 of margin), passed at 4x (150).
#[test]
fn a_symbols_own_rate_decides_the_verdict_not_the_engine_wide_one() {
    let l = limits(Some(0.25), &[("SPOT", 1.0)]);
    assert_eq!(reason(&l, "SPOT", 6.0, 500.0).as_deref(), Some("insufficient-margin"));
    assert_eq!(reason(&l, "PERP", 6.0, 500.0), None, "no row: the engine-wide 4x");

    // the other way round: the engine-wide rate is 1x and one symbol is a 4x perp
    let l = limits(Some(1.0), &[("PERP", 0.25)]);
    assert_eq!(reason(&l, "PERP", 6.0, 500.0), None);
    assert_eq!(reason(&l, "SPOT", 6.0, 500.0).as_deref(), Some("insufficient-margin"));

    // the 4x symbol is margined, not exempt: 2,100 of notional is 525 of margin against 500
    let l = limits(Some(0.25), &[("PERP", 0.25)]);
    assert_eq!(reason(&l, "PERP", 21.0, 500.0).as_deref(), Some("insufficient-margin"));
}

/// The comparison is `<=`: an order whose margin is exactly the free buying power goes.
#[test]
fn margin_exactly_equal_to_the_free_buying_power_passes() {
    let l = limits(Some(0.25), &[("SPOT", 1.0)]);
    assert_eq!(reason(&l, "SPOT", 5.0, 500.0), None, "500 of margin against 500");
    assert!(reason(&l, "SPOT", 5.01, 500.0).is_some());
}

/// A symbol's row arms the lane for it even where there is no engine-wide rate (the lane is off for
/// every symbol without one).
#[test]
fn a_row_arms_the_lane_for_its_symbol_alone() {
    let l = limits(None, &[("SPOT", 1.0)]);
    assert_eq!(reason(&l, "SPOT", 6.0, 500.0).as_deref(), Some("insufficient-margin"));
    assert_eq!(reason(&l, "PERP", 6.0, 500.0), None, "no rate for PERP: the lane is off");
}

fn combo(legs: &[&str]) -> OrderRequest {
    OrderRequest {
        client_order_id: "cb1".into(),
        venue: "binance".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(-5.0),
        combo_legs: legs.iter().map(|s| ComboLeg { symbol: (*s).into(), ratio: 1 }).collect(),
        ..Default::default()
    }
}

/// Each admitted leg commits margin at ITS OWN rate for the legs after it: SPOT commits 600 and PERP
/// 150, so with 700 of equity the second leg of either order finds too little left.
#[test]
fn a_combo_commits_each_legs_margin_at_that_legs_own_rate() {
    let l = limits(Some(0.25), &[("SPOT", 1.0)]);
    let run = |legs: &[&str], equity: f64, l: &RiskLimits| {
        let leg_ctx =
            move |_: &str| RiskContext { mark_price: 600.0, equity, ..RiskContext::default() };
        RiskGate::new(l.clone()).check_combo(&combo(legs), &RiskContext::default(), leg_ctx)
    };

    // SPOT first commits 600 (not 150): PERP then has 100 of 700 and needs 150
    let v = run(&["SPOT", "PERP"], 700.0, &l);
    assert!(!v.ok && v.reason == "leg PERP: insufficient-margin", "{v:?}");
    // PERP first commits 150: SPOT then has 550 of 700 and needs 600
    let v = run(&["PERP", "SPOT"], 700.0, &l);
    assert!(!v.ok && v.reason == "leg SPOT: insufficient-margin", "{v:?}");
    // with 800 both orders fit: 600 + 150 and 150 + 600
    assert!(run(&["SPOT", "PERP"], 800.0, &l).ok);
    assert!(run(&["PERP", "SPOT"], 800.0, &l).ok);

    // control: SPOT without its row is margined at the engine-wide 4x, and the very first case passes
    let no_rows = limits(Some(0.25), &[]);
    assert!(run(&["SPOT", "PERP"], 700.0, &no_rows).ok, "150 + 150 fits in 700");
}
