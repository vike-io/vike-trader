//! Property-based tests for the position-sizing framework (`vike_analytics::sizing`) — testing-arch
//! plan Phase 6, target P1. `Sizer::size` had **ZERO direct tests** (only one indirect `r4_parity`
//! golden run touched it), making it the biggest money-path coverage hole in the workspace. These
//! properties assert the invariants the eight sizers must hold over RANDOM but business-VALID
//! inputs (positive prices/multipliers, non-negative equity, fractional pcts), where the curated
//! golden series never looks.
//!
//! Ranges are deliberately generous but bounded to the real domain: a sizer is never asked to size
//! against a negative price or a negative equity, and a "pct" knob is a fraction in `[0, 1]`. A
//! violation here is a real sizing bug, not a widened tolerance — the two `<= cap` properties carry
//! only a few-ULP round-trip epsilon (documented inline), never a fudge factor.

use proptest::prelude::*;
use vike_analytics::sizing::{
    DrawdownThrottleSizer, FixedDollarSizer, FixedSharesSizer, MaxRiskPctSizer, PctEquitySizer,
    PctVolatilitySizer, PortfolioHeatSizer, PositionSizer, SizeContext,
};

/// A neutral, VALID context the field-override properties start from.
fn base_ctx() -> SizeContext {
    SizeContext {
        symbol: "SYM".to_string(),
        side: 1,
        intent: 0.0,
        basis_price: 100.0,
        equity: 10_000.0,
        cash: 10_000.0,
        multiplier: 1.0,
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
        let ctx = SizeContext {
            symbol: "SYM".to_string(),
            side,
            intent,
            basis_price,
            equity,
            cash,
            multiplier,
            atr,
            drawdown,
            risk_stop,
            open_risk,
        };
        // PassThrough is intentionally NOT in this list: it returns `ctx.intent` verbatim, so its
        // non-negativity is the caller's contract on `intent`, not the sizer's own math. It IS
        // covered by generating intent >= 0 and asserting it below.
        let sizers: Vec<(&str, Box<dyn PositionSizer>)> = vec![
            ("FixedDollar", Box::new(FixedDollarSizer { amount })),
            ("FixedShares", Box::new(FixedSharesSizer { shares })),
            ("PctEquity", Box::new(PctEquitySizer { pct })),
            ("PctVolatility", Box::new(PctVolatilitySizer { pct })),
            ("MaxRiskPct", Box::new(MaxRiskPctSizer { pct })),
            (
                "PortfolioHeat",
                Box::new(PortfolioHeatSizer { base: Box::new(PctEquitySizer { pct }), max_heat }),
            ),
            (
                "DrawdownThrottle",
                Box::new(DrawdownThrottleSizer {
                    base: Box::new(FixedSharesSizer { shares }),
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
        prop_assert_eq!(vike_analytics::sizing::PassThroughSizer.size(&ctx), intent);
    }

    /// `PctEquity` targets `pct` of equity as NOTIONAL, so the resulting position's notional
    /// (`qty · basis · mult`) never exceeds equity for `pct <= 1`. The only slack is the
    /// divide-then-multiply round trip (<= a couple ULP); the few-ULP relative epsilon covers that
    /// and NOTHING else — the exact math result is `pct·equity <= equity`, never a real overshoot.
    #[test]
    fn pct_equity_notional_never_exceeds_equity(
        pct in 0.0f64..1.0,
        basis_price in 0.001f64..1_000_000.0,
        equity in 0.0f64..1_000_000_000.0,
        multiplier in 0.001f64..1_000.0,
    ) {
        let ctx = SizeContext { basis_price, equity, multiplier, ..base_ctx() };
        let qty = PctEquitySizer { pct }.size(&ctx);
        let notional = vike_model::order_notional(qty, basis_price, multiplier);
        prop_assert!(
            notional <= equity * (1.0 + 1e-9) + 1e-9,
            "PctEquity notional {notional} exceeds equity {equity} (pct={pct}, qty={qty})"
        );
    }

    /// `DrawdownThrottle` scales the base size by `max(floor, 1 - sens·dd)`, which is non-increasing
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
        let make = || DrawdownThrottleSizer {
            base: Box::new(FixedSharesSizer { shares }),
            sensitivity,
            floor,
        };
        let (dd_lo, dd_hi) = if dd_a <= dd_b { (dd_a, dd_b) } else { (dd_b, dd_a) };
        let q_lo = make().size(&SizeContext { drawdown: dd_lo, ..base_ctx() });
        let q_hi = make().size(&SizeContext { drawdown: dd_hi, ..base_ctx() });
        prop_assert!(
            q_hi <= q_lo,
            "throttle not monotonic: dd {dd_lo} -> {q_lo}, deeper dd {dd_hi} -> {q_hi}"
        );
    }

    /// `MaxRiskPct` solves the size so the position's stop-distance risk (`qty · |basis-stop| ·
    /// mult`) equals `pct` of equity — the cap. So the realized position risk never exceeds
    /// `pct·equity` (round-trip epsilon only; a no-stop or basis==stop case sizes 0). Same
    /// documented few-ULP slack as the notional cap above.
    #[test]
    fn max_risk_pct_respects_the_risk_cap(
        pct in 0.0f64..1.0,
        basis_price in 0.001f64..1_000_000.0,
        stop in 0.001f64..1_000_000.0,
        equity in 0.0f64..1_000_000_000.0,
        multiplier in 0.001f64..1_000.0,
    ) {
        let ctx =
            SizeContext { basis_price, equity, multiplier, risk_stop: Some(stop), ..base_ctx() };
        let qty = MaxRiskPctSizer { pct }.size(&ctx);
        prop_assert!(qty.is_finite() && qty >= 0.0, "qty {qty}");
        let risk_per_unit = (basis_price - stop).abs() * multiplier;
        let position_risk = qty * risk_per_unit;
        let cap = pct * equity;
        prop_assert!(
            position_risk <= cap * (1.0 + 1e-9) + 1e-9,
            "MaxRiskPct position risk {position_risk} exceeds cap {cap} (pct={pct}, qty={qty})"
        );
    }
}
