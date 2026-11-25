//! Property-based tests for the position-sizing framework (`vike_portfolio::sizing`) — testing-arch
//! plan Phase 6, target P1. `Sizer::size` had **ZERO direct tests** (only one indirect `r4_parity`
//! golden run touched it), making it the biggest money-path coverage hole in the workspace. These
//! properties assert the invariants the eight sizers must hold over RANDOM but business-VALID
//! inputs (positive prices/multipliers, non-negative equity, fractional percents), where the curated
//! golden series never looks.
//!
//! Ranges are deliberately generous but bounded to the real domain: a sizer is never asked to size
//! against a negative price or a negative equity, and a "pct" knob is a fraction in `[0, 1]`. A
//! violation here is a real sizing bug, not a widened tolerance — the two `<= cap` properties carry
//! only a few-ULP round-trip epsilon (documented inline), never a fudge factor.

use proptest::prelude::*;
use vike_model::{ContractValue, floor_to_step};
use vike_portfolio::sizing::{
    PositionSizer, PositionSizerContext, PositionSizerDrawdownThrottle, PositionSizerFixedDollar,
    PositionSizerFixedShares, PositionSizerMaxRiskPercent, PositionSizerPercentEquity,
    PositionSizerPercentVolatility, PositionSizerPortfolioHeat,
};

/// A neutral, VALID context the field-override properties start from.
fn base_context() -> PositionSizerContext {
    PositionSizerContext {
        symbol: "SYM".to_string(),
        side: 1,
        intent: 0.0,
        basis_price: 100.0,
        equity: 10_000.0,
        cash: 10_000.0,
        contract_value: ContractValue::linear(1.0),
        atr: 0.0,
        drawdown: 0.0,
        risk_stop: None,
        open_risk: 0.0,
    }
}

proptest! {
    /// EVERY sizer returns a quantity that is finite (no NaN/inf) and non-negative, for every
    /// valid context — the `size(..) -> f64` "Return the quantity (>= 0)" contract in `sizing.rs`.
    /// The two wrapping sizers are exercised over concrete (non-negative) bases so the whole
    /// registry is covered in one property.
    #[test]
    fn all_sizers_return_nonneg_finite(
        side in prop_oneof![Just(1i32), Just(-1i32)],
        intent in 0.0f64..1_000_000.0,
        basis_price in 0.001f64..1_000_000.0,
        equity in 0.0f64..1_000_000_000.0,
        cash in 0.0f64..1_000_000_000.0,
        multiplier in 0.001f64..1_000.0,
        // atr == 0 is the documented "unavailable" fallback branch of PctVolatility — include it.
        atr in prop_oneof![Just(0.0f64), 0.0001f64..100_000.0],
        drawdown in 0.0f64..1.0,
        risk_stop in prop_oneof![Just(None), (0.001f64..1_000_000.0).prop_map(Some)],
        open_risk in 0.0f64..1_000_000_000.0,
        // sizer parameters (all non-negative — a valid configuration)
        pct in 0.0f64..1.0,
        amount in 0.0f64..1_000_000.0,
        shares in 0.0f64..1_000_000.0,
        max_heat in 0.0f64..1.0,
        sensitivity in 0.0f64..10.0,
        floor in 0.0f64..1.0,
    ) {
        let ctx = PositionSizerContext {
            symbol: "SYM".to_string(),
            side,
            intent,
            basis_price,
            equity,
            cash,
            contract_value: ContractValue::linear(multiplier),
            atr,
            drawdown,
            risk_stop,
            open_risk,
        };
        // PassThrough is intentionally NOT in this list: it returns `ctx.intent` verbatim, so its
        // non-negativity is the caller's contract on `intent`, not the sizer's own math. It IS
        // covered by generating intent >= 0 and asserting it below.
        let sizers: Vec<(&str, Box<dyn PositionSizer>)> = vec![
            ("FixedDollar", Box::new(PositionSizerFixedDollar { amount, ..Default::default() })),
            ("FixedShares", Box::new(PositionSizerFixedShares { shares })),
            ("PctEquity", Box::new(PositionSizerPercentEquity { percent: pct, ..Default::default() })),
            ("PctVolatility", Box::new(PositionSizerPercentVolatility { percent: pct })),
            ("MaxRiskPct", Box::new(PositionSizerMaxRiskPercent { percent: pct })),
            (
                "PortfolioHeat",
                Box::new(PositionSizerPortfolioHeat {
                    base: Box::new(PositionSizerPercentEquity { percent: pct, ..Default::default() }),
                    max_heat,
                }),
            ),
            (
                "DrawdownThrottle",
                Box::new(PositionSizerDrawdownThrottle {
                    base: Box::new(PositionSizerFixedShares { shares }),
                    sensitivity,
                    floor,
                }),
            ),
        ];
        for (name, s) in &sizers {
            let q = s.size(&ctx);
            prop_assert!(q.is_finite(), "{name} produced non-finite qty {q}");
            prop_assert!(q >= 0.0, "{name} produced negative qty {q}");
        }
        // PassThrough contract: intent flows through unchanged (>= 0 by generation).
        prop_assert_eq!(vike_portfolio::sizing::PositionSizerPassThrough.size(&ctx), intent);
    }

    /// `PctEquity` targets `pct` of equity as NOTIONAL, so the resulting position's notional
    /// (`qty · basis · mult`) never exceeds equity for `pct <= 1`. The only slack is the
    /// divide-then-multiply round trip (<= a couple ULP); the few-ULP relative epsilon covers that
    /// and NOTHING else — the exact math result is `pct·equity <= equity`, never a real overshoot.
    #[test]
    fn percent_equity_notional_never_exceeds_equity(
        pct in 0.0f64..1.0,
        basis_price in 0.001f64..1_000_000.0,
        equity in 0.0f64..1_000_000_000.0,
        multiplier in 0.001f64..1_000.0,
    ) {
        let contract_value = ContractValue::linear(multiplier);
        let ctx = PositionSizerContext { basis_price, equity, contract_value, ..base_context() };
        let qty = PositionSizerPercentEquity { percent: pct, ..Default::default() }.size(&ctx);
        let notional = vike_model::order_notional(qty, basis_price, multiplier);
        prop_assert!(
            notional <= equity * (1.0 + 1e-9) + 1e-9,
            "PctEquity notional {notional} exceeds equity {equity} (pct={pct}, qty={qty})"
        );
    }

    /// `DrawdownThrottle` scales the base size by `max(floor, 1 - sens·drawdown)`, which is non-increasing
    /// in drawdown for `sens >= 0` — so a DEEPER drawdown never yields a LARGER size. The base here
    /// is a constant (`FixedShares`) so all drawdown-dependence is the throttle factor itself. Float
    /// multiplication is order-preserving for a non-negative base, so this holds EXACTLY (no
    /// tolerance).
    #[test]
    fn drawdown_throttle_is_monotonic_non_increasing(
        dd_a in 0.0f64..1.0,
        dd_b in 0.0f64..1.0,
        sensitivity in 0.0f64..10.0,
        floor in 0.0f64..1.0,
        shares in 0.0f64..1_000_000.0,
    ) {
        let make = || PositionSizerDrawdownThrottle {
            base: Box::new(PositionSizerFixedShares { shares }),
            sensitivity,
            floor,
        };
        let (drawdown_lo, drawdown_hi) = if dd_a <= dd_b { (dd_a, dd_b) } else { (dd_b, dd_a) };
        let q_lo = make().size(&PositionSizerContext { drawdown: drawdown_lo, ..base_context() });
        let q_hi = make().size(&PositionSizerContext { drawdown: drawdown_hi, ..base_context() });
        prop_assert!(
            q_hi <= q_lo,
            "throttle not monotonic: dd {drawdown_lo} -> {q_lo}, deeper dd {drawdown_hi} -> {q_hi}"
        );
    }

    /// `MaxRiskPct` solves the size so the position's stop-distance risk (`qty · |basis-stop| ·
    /// mult`) equals `pct` of equity — the cap. So the realized position risk never exceeds
    /// `pct·equity` (round-trip epsilon only; a no-stop or basis==stop case sizes 0). Same
    /// documented few-ULP slack as the notional cap above.
    #[test]
    fn max_risk_percent_respects_the_risk_cap(
        pct in 0.0f64..1.0,
        basis_price in 0.001f64..1_000_000.0,
        stop in 0.001f64..1_000_000.0,
        equity in 0.0f64..1_000_000_000.0,
        multiplier in 0.001f64..1_000.0,
    ) {
        let contract_value = ContractValue::linear(multiplier);
        let ctx = PositionSizerContext {
            basis_price,
            equity,
            contract_value,
            risk_stop: Some(stop),
            ..base_context()
        };
        let qty = PositionSizerMaxRiskPercent { percent: pct }.size(&ctx);
        prop_assert!(qty.is_finite() && qty >= 0.0, "qty {qty}");
        let risk_per_unit = (basis_price - stop).abs() * multiplier;
        let position_risk = qty * risk_per_unit;
        let cap = pct * equity;
        prop_assert!(
            position_risk <= cap * (1.0 + 1e-9) + 1e-9,
            "MaxRiskPct position risk {position_risk} exceeds cap {cap} (pct={pct}, qty={qty})"
        );
    }

    /// With every option at its default, `PctEquity` and `FixedDollar` are the quotient they always were, bit for bit, on a
    /// linear and on an inverse value: the zero fee terms are IEEE no-ops and no step floors anything.
    #[test]
    fn the_money_sizers_with_no_options_are_the_old_quotient_bit_for_bit(
        pct in 0.0f64..1.0,
        amount in 0.0f64..1_000_000.0,
        basis_price in 0.001f64..1_000_000.0,
        equity in 0.0f64..1_000_000_000.0,
        per_unit in 0.001f64..1_000.0,
        inverse in any::<bool>(),
    ) {
        let contract_value =
            if inverse { ContractValue::inverse(per_unit) } else { ContractValue::linear(per_unit) };
        let ctx = PositionSizerContext { basis_price, equity, contract_value, ..base_context() };
        let unit = contract_value.unit_value(basis_price, 1.0).expect("par always answers");
        let sized = PositionSizerPercentEquity { percent: pct, ..Default::default() }.size(&ctx);
        prop_assert_eq!(sized.to_bits(), ((pct * equity) / unit).to_bits());
        let sized = PositionSizerFixedDollar { amount, ..Default::default() }.size(&ctx);
        prop_assert_eq!(sized.to_bits(), (amount / unit).to_bits());
    }

    /// With a commission stated, `PctEquity`'s size and the commission it pays spend `pct` of equity together: the value,
    /// its rate, the per-unit fee on every unit and the per-order fee add back to the target (round-trip epsilon only).
    #[test]
    fn percent_equity_with_a_commission_spends_the_target(
        pct in 0.0f64..1.0,
        basis_price in 0.001f64..1_000_000.0,
        equity in 0.0f64..1_000_000_000.0,
        multiplier in 0.001f64..1_000.0,
        fee_rate in 0.0f64..0.01,
        fee_per_unit in 0.0f64..10.0,
        fee_per_order in 0.0f64..10.0,
    ) {
        let contract_value = ContractValue::linear(multiplier);
        let ctx = PositionSizerContext { basis_price, equity, contract_value, ..base_context() };
        let sizer = PositionSizerPercentEquity { percent: pct, fee_rate, fee_per_unit, fee_per_order, step: None };
        let qty = sizer.size(&ctx);
        let unit = basis_price * multiplier;
        let spent = qty * unit * (1.0 + fee_rate) + qty * fee_per_unit + fee_per_order;
        let target = pct * equity;
        prop_assert!(
            (spent - target).abs() <= (target + fee_per_order) * 1e-9 + 1e-9,
            "spent {spent} against the target {target} (qty={qty})"
        );
    }

    /// On a decimal step `1/n` (a venue's 0.00001, 0.001 or 1) `floor_to_step` is `(qty · n).floor() / n` bit for bit, the
    /// floor the strategy lab measured against its reference, through `PctEquity`'s and `FixedDollar`'s `step` too.
    #[test]
    fn floor_to_step_on_a_decimal_step_is_the_whole_count_over_n(
        qty in 0.0f64..10_000_000.0,
        decimals in 0usize..9,
    ) {
        let n = DECIMAL_STEPS[decimals];
        let want = ((qty * n).floor() / n).to_bits();
        prop_assert_eq!(floor_to_step(qty, 1.0 / n).to_bits(), want);
        // one unit worth 1 and an equity of `qty`: both sizers answer `qty` before the floor
        let ctx = PositionSizerContext { basis_price: 1.0, equity: qty, ..base_context() };
        let step = Some(1.0 / n);
        let all_in = PositionSizerPercentEquity { percent: 1.0, step, ..Default::default() };
        prop_assert_eq!(all_in.size(&ctx).to_bits(), want);
        prop_assert_eq!(PositionSizerFixedDollar { amount: qty, step }.size(&ctx).to_bits(), want);
    }
}

/// `1/n` for every decimal step from 1 to 0.00000001.
const DECIMAL_STEPS: [f64; 9] =
    [1.0, 10.0, 100.0, 1_000.0, 10_000.0, 100_000.0, 1_000_000.0, 10_000_000.0, 100_000_000.0];

/// A step that is not one over a whole number keeps whole multiples of itself, read as their decimal (three steps of 0.3
/// are 0.9, where `3.0 * 0.3` is 0.8999999999999999: P5.1, `vike_marketdata::TickFraction`), the decimal steps land on their
/// decimals, and a step that is not a positive number floors nothing.
#[test]
fn floor_to_step_on_other_steps_and_on_no_step() {
    assert_eq!(floor_to_step(12.0, 5.0), 10.0);
    assert_eq!(floor_to_step(4.9, 5.0), 0.0);
    assert_eq!(floor_to_step(1.0, 0.3), 0.9);
    assert_ne!(3.0 * 0.3, 0.9, "the count times the step");
    assert_eq!(floor_to_step(0.464_990_001, 0.00001), 0.46499);
    assert_eq!(floor_to_step(0.123_49, 0.0001), 0.1234);
    assert_eq!(floor_to_step(2.7, 1.0), 2.0);
    assert_eq!(floor_to_step(2.7, 0.5), 2.5);
    for step in [0.0, -0.001, f64::NAN] {
        assert_eq!(floor_to_step(2.7, step), 2.7, "step {step}");
    }
}

/// `ContractValue::move_risk` (once `vike_analytics::sizing::move_risk`) on a linear value is the
/// stop-distance product the risk sizers always spelled, bit for bit, both signs of the move and any unit.
#[test]
fn move_risk_on_a_linear_value_is_the_old_product_bit_for_bit() {
    for (basis, to) in [(100.0, 95.0), (100.0, 104.5), (0.3, 0.27), (65_700.0, 64_000.25)] {
        for u in [1.0, 50.0, 0.0001, 10_000.0] {
            let got = ContractValue::linear(u).move_risk(basis, to);
            assert_eq!(got.to_bits(), ((basis - to).abs() * u).to_bits(), "{basis}->{to} x {u}");
        }
    }
}

/// The inverse worked example, deribit `BTC-PERPETUAL`: 1 BTC of equity at 60,000 (60,000 dollars in the
/// context), long, stop 57,000, risk 1%. One dollar contract loses `1·(1/57,000 − 1/60,000)` BTC to the
/// stop, 0.0526 dollars at the 60,000 the equity is valued at, so the sizer opens 11,400 contracts, and
/// their loss at the stop is 0.01 BTC: 1% of the coin. The linear law on the same numbers opened 0.2.
#[test]
fn the_risk_sizers_price_an_inverse_stop_by_its_coin_pnl() {
    let inverse = PositionSizerContext {
        basis_price: 60_000.0,
        equity: 60_000.0,
        contract_value: ContractValue::inverse(1.0),
        risk_stop: Some(57_000.0),
        ..base_context()
    };
    let qty = PositionSizerMaxRiskPercent { percent: 0.01 }.size(&inverse);
    assert!((qty - 11_400.0).abs() < 1e-6, "{qty}");
    let coin_loss = qty * (1.0 / 57_000.0 - 1.0 / 60_000.0);
    assert!((coin_loss - 0.01).abs() < 1e-12, "1% of the coin: {coin_loss}");
    let linear =
        PositionSizerContext { contract_value: ContractValue::linear(1.0), ..inverse.clone() };
    assert!((PositionSizerMaxRiskPercent { percent: 0.01 }.size(&linear) - 0.2).abs() < 1e-12);

    // one ATR of 3,000 against the long is the same move
    let atr = PositionSizerContext { atr: 3_000.0, risk_stop: None, ..inverse.clone() };
    assert!((PositionSizerPercentVolatility { percent: 0.01 }.size(&atr) - 11_400.0).abs() < 1e-6);
    // ...and against a short it is the move UP to 63,000: `1/60,000 − 1/63,000` a contract
    let short = PositionSizerContext { side: -1, ..atr };
    let want = 600.0 / ((1.0 / 60_000.0 - 1.0 / 63_000.0) * 60_000.0);
    assert!((PositionSizerPercentVolatility { percent: 0.01 }.size(&short) - want).abs() < 1e-6);

    // the heat cap is that risk too: 100% of equity is 60,000 contracts, capped to 11,400
    let all_in = || PositionSizerPercentEquity { percent: 1.0, ..Default::default() };
    let heat = PositionSizerPortfolioHeat { base: Box::new(all_in()), max_heat: 0.01 };
    assert!((all_in().size(&inverse) - 60_000.0).abs() < 1e-9, "the face");
    assert!((heat.size(&inverse) - 11_400.0).abs() < 1e-6);
}
