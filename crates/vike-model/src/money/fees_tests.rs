use super::*;

#[test]
fn percent_maker_taker_distinguishes_sides() {
    let s = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 };
    // maker: 2 bps of 1000 notional = 0.2; taker: 5.5 bps = 0.55
    assert_eq!(s.commission(true, 10.0, 100.0), 10.0 * 100.0 * (2.0 / 10_000.0));
    assert_eq!(s.commission(false, 10.0, 100.0), 10.0 * 100.0 * (5.5 / 10_000.0));
}

#[test]
fn commission_is_the_broker_sim_primitive_bit_for_bit() {
    // Guards the paper byte-path: commission == qty*px*(bps/1e4) in the SAME associativity as
    // `broker_sim::fee(size, px, rate, 1.0)` where rate = bps/1e4.
    let s = FeeSchedule::PercentMakerTaker { maker_bps: 7.0, taker_bps: 7.0 };
    let (q, p) = (0.375, 29550.1234);
    let rate = 7.0 / 10_000.0;
    assert_eq!(s.commission(false, q, p), q * p * rate);
}

#[test]
fn per_share_floor_and_cap() {
    let s = FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 };
    // tiny order: 10 shares * $0.005 = $0.05 -> floored to $1.00 min
    assert_eq!(s.commission(false, 10.0, 100.0), 1.0);
    // mid order: 1000 shares * $0.005 = $5.00, cap = 0.5% * 1000 * $50 = $250 -> $5.00
    assert_eq!(s.commission(false, 1000.0, 50.0), 5.0);
    // penny-stock huge order: 100000 * $0.005 = $500 raw, cap = 0.5% * 100000 * $0.20 = $100
    assert_eq!(s.commission(false, 100_000.0, 0.20), 100.0);
    // is_maker is ignored for the per-share shape
    assert_eq!(s.commission(true, 10.0, 100.0), s.commission(false, 10.0, 100.0));
}

#[test]
fn percent_of_underlying_commission_degrades_to_bounded_premium_approx() {
    // The paper site (premium-only): min(bps × premium, cap × premium). bps ≪ cap, so the bps
    // term wins and the number matches the old PercentMakerTaker{3,3} paper figure bit-for-bit.
    let d = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 };
    let (qty, premium) = (2.0, 3000.0);
    assert_eq!(d.commission(false, qty, premium), qty * premium * (3.0 / 10_000.0));
    // identical to the schedule this replaced, at the paper site
    let old = FeeSchedule::PercentMakerTaker { maker_bps: 3.0, taker_bps: 3.0 };
    assert_eq!(d.commission(false, qty, premium), old.commission(false, qty, premium));
    // maker == taker for the options shape
    assert_eq!(d.commission(true, qty, premium), d.commission(false, qty, premium));
}

#[test]
fn percent_of_underlying_accurate_uses_underlying_with_premium_cap() {
    let d = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 };
    // ATM: 0.03% of underlying (18) < 12.5% of premium (375) → underlying term wins.
    assert_eq!(d.commission_with_underlying(1.0, 3000.0, 60_000.0), 1.0 * 60_000.0 * 0.0003);
    // Cheap deep-OTM: 0.03% of underlying (18) > 12.5% of premium (12.5) → the premium CAP binds
    // (the buyer-protecting rule the flat approximation could never express).
    assert_eq!(d.commission_with_underlying(1.0, 100.0, 60_000.0), 1.0 * 100.0 * 0.125);
    // The accurate path exceeds the premium-only approximation whenever underlying > premium
    // (the honest correction the paper site cannot make without an index price).
    assert!(d.commission_with_underlying(1.0, 3000.0, 60_000.0) > d.commission(false, 1.0, 3000.0));
}

#[test]
fn commission_with_underlying_delegates_for_non_option_shapes() {
    // Non-PercentOfUnderlying shapes ignore the underlying — delegate to the premium commission.
    let p = FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 };
    assert_eq!(p.commission_with_underlying(2.0, 100.0, 9_999.0), p.commission(false, 2.0, 100.0));
    assert_eq!(FeeSchedule::Free.commission_with_underlying(2.0, 100.0, 9_999.0), 0.0);
}

#[test]
fn percent_of_underlying_reports_maker_taker_fraction() {
    let d = FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 };
    assert_eq!(d.maker_taker_rates(), (3.0 / 10_000.0, 3.0 / 10_000.0));
}

#[test]
fn free_is_zero() {
    assert_eq!(FeeSchedule::Free.commission(true, 10.0, 100.0), 0.0);
    assert_eq!(FeeSchedule::Free.commission(false, 10.0, 100.0), 0.0);
}

#[test]
fn maker_taker_rates_only_for_percent() {
    let s = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 };
    assert_eq!(s.maker_taker_rates(), (2.0 / 10_000.0, 5.0 / 10_000.0));
    assert_eq!(FeeSchedule::Free.maker_taker_rates(), (0.0, 0.0));
    assert_eq!(
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 }
            .maker_taker_rates(),
        (0.0, 0.0)
    );
}

#[test]
fn from_fractions_round_trips_to_commission() {
    let s = FeeSchedule::from_fractions(0.001, 0.001); // 10 bps / 10 bps
    assert_eq!(s, FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 });
    assert_eq!(s.commission(false, 2.0, 100.0), 2.0 * 100.0 * (10.0 / 10_000.0));
    // from_binance_rates is the same construction
    assert_eq!(FeeSchedule::from_binance_rates(0.001, 0.001), s);
}

#[test]
fn probability_scaled_taker_curve_peaks_at_half() {
    let s = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.02,
        maker_rate: 0.0,
        maker_rebate_share: 0.0,
    };
    // p = 0.5 is the curve peak: fee = qty * rate * 0.25.
    assert_eq!(s.commission(false, 100.0, 0.5), 100.0 * 0.02 * (0.5 * (1.0 - 0.5)));
    assert_eq!(s.commission(false, 100.0, 0.5), 0.5);
    // symmetric: p and 1−p charge the same fee (analytically; f64 rounding differs by ulps —
    // 0.2·0.8 evaluates as 0.2·(1−0.2) vs 0.8·(1−0.8), which are not the same bit pattern)
    let (lo, hi) = (s.commission(false, 100.0, 0.2), s.commission(false, 100.0, 0.8));
    assert!((lo - hi).abs() < 1e-12, "curve symmetry: {lo} vs {hi}");
    // every off-peak price charges strictly less than the peak
    for p in [0.01, 0.1, 0.3, 0.49, 0.51, 0.9, 0.99] {
        assert!(s.commission(false, 100.0, p) < s.commission(false, 100.0, 0.5), "p={p}");
    }
}

#[test]
fn probability_scaled_fee_vanishes_at_certainty_bounds() {
    let s = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.02,
        maker_rate: 0.01,
        maker_rebate_share: 0.5,
    };
    for is_maker in [false, true] {
        assert_eq!(s.commission(is_maker, 100.0, 0.0), 0.0);
        assert_eq!(s.commission(is_maker, 100.0, 1.0), 0.0);
        // out-of-domain prices clamp into [0,1] — never a negative curve
        assert_eq!(s.commission(is_maker, 100.0, -0.25), 0.0);
        assert_eq!(s.commission(is_maker, 100.0, 1.75), 0.0);
    }
    // p→0 / p→1: the fee tends to zero
    assert!(s.commission(false, 100.0, 1e-9) < 1e-8);
    assert!(s.commission(false, 100.0, 1.0 - 1e-9) < 1e-8);
}

#[test]
fn probability_scaled_maker_rebate_is_negative_commission() {
    // maker_rate 0 + a rebate share: the maker EARNS `share × taker fee` (negative commission,
    // the module's sign convention), while the taker side is unaffected.
    let s = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.02,
        maker_rate: 0.0,
        maker_rebate_share: 0.25,
    };
    let taker = s.commission(false, 100.0, 0.5);
    assert_eq!(taker, 100.0 * 0.02 * (0.5 * (1.0 - 0.5)));
    let maker = s.commission(true, 100.0, 0.5);
    assert!(maker < 0.0, "maker commission must be a rebate, got {maker}");
    assert_eq!(maker, -(0.25 * taker));
    // with a nonzero maker_rate and no rebate the maker pays the (positive) maker curve
    let paying = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.02,
        maker_rate: 0.01,
        maker_rebate_share: 0.0,
    };
    assert_eq!(paying.commission(true, 100.0, 0.5), 100.0 * 0.01 * (0.5 * (1.0 - 0.5)));
    // rebate nets against a nonzero maker curve: maker_fee − share × taker_fee
    let netted = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.02,
        maker_rate: 0.01,
        maker_rebate_share: 0.5,
    };
    let curve = 0.5 * (1.0 - 0.5);
    assert_eq!(
        netted.commission(true, 100.0, 0.5),
        100.0 * 0.01 * curve - 0.5 * (100.0 * 0.02 * curve)
    );
}

#[test]
fn probability_scaled_has_no_flat_rate_and_delegates_underlying() {
    let s = FeeSchedule::ProbabilityScaled {
        taker_rate: 0.02,
        maker_rate: 0.01,
        maker_rebate_share: 0.5,
    };
    // price-dependent — not expressible as a flat fraction
    assert_eq!(s.maker_taker_rates(), (0.0, 0.0));
    // no underlying concept: delegates to the plain (taker) commission
    assert_eq!(s.commission_with_underlying(100.0, 0.5, 9_999.0), s.commission(false, 100.0, 0.5));
}
