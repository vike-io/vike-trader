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

#[test]
fn pm_curve_registry_is_opt_in_and_default_compatible() {
    // The DEFAULT registry is untouched: polymarket stays Free (byte-identical consumers —
    // paper fill path, snapshot cost display, `vike_mount::resolve_fee_schedule`'s static
    // default all read this, NOT the opt-in seam).
    assert_eq!(fee_schedule_for("polymarket"), FeeSchedule::Free);
    // The opt-in twin hands out the verified V2 fee curve for polymarket (the VALUE change of
    // this workstream) — NOT the all-zero shape template.
    assert_eq!(fee_schedule_for_with_pm_curve("polymarket"), POLYMARKET_V2_FEE_CURVE);
    assert_ne!(fee_schedule_for_with_pm_curve("polymarket"), FeeSchedule::Free);
    // …and delegates every other venue to the default registry unchanged.
    for v in ["binance", "bybit", "okx", "deribit", "hyperliquid", "ibkr", "oanda", "nope"] {
        assert_eq!(fee_schedule_for_with_pm_curve(v), fee_schedule_for(v), "{v}");
    }
}

/// OFF/default-path byte-identity: the all-zero [`POLYMARKET_PROB_CURVE`] shape template MUST
/// stay numerically identical to [`FeeSchedule::Free`] at every price/side. Three `vike-backtest`
/// consumers depend on this (the zero-rate regression guard
/// `cheap_np_profile::the_zero_rate_polymarket_curve_still_charges_nothing`, plus the
/// `sim_broker`/`cheap_np_run` doc claims that its rates still charge zero); it must NEVER pick
/// up the V2 rates.
#[test]
fn zero_shape_template_stays_byte_identical_to_free() {
    assert_eq!(
        POLYMARKET_PROB_CURVE,
        FeeSchedule::ProbabilityScaled {
            taker_rate: 0.0,
            maker_rate: 0.0,
            maker_rebate_share: 0.0,
        }
    );
    for p in [0.0, 0.1, 0.25, 0.5, 0.9, 1.0] {
        assert_eq!(POLYMARKET_PROB_CURVE.commission(false, 100.0, p), 0.0, "taker p={p}");
        assert_eq!(POLYMARKET_PROB_CURVE.commission(true, 100.0, p), 0.0, "maker p={p}");
        assert_eq!(
            POLYMARKET_PROB_CURVE.commission(false, 100.0, p),
            FeeSchedule::Free.commission(false, 100.0, p),
            "identical to Free at p={p}"
        );
    }
}

/// The verified 2026 Polymarket V2 regime resolves through the opt-in curve: sports taker `0.05`,
/// a `15 %` maker rebate as a NEGATIVE commission, no separate maker fee. Pins the numbers AND
/// the rebate sign/shape.
#[test]
fn polymarket_v2_curve_has_the_verified_regime_numbers() {
    assert_eq!(
        POLYMARKET_V2_FEE_CURVE,
        FeeSchedule::ProbabilityScaled {
            taker_rate: 0.05,
            maker_rate: 0.0,
            maker_rebate_share: 0.15,
        }
    );
    // Taker pays qty × 0.05 × p(1−p) — strictly positive in the curve's interior.
    let taker = POLYMARKET_V2_FEE_CURVE.commission(false, 100.0, 0.25);
    assert_eq!(taker, 100.0 * 0.05 * (0.25 * (1.0 - 0.25)));
    assert!(taker > 0.0, "taker fee must be positive inside the curve support");
    // The maker side is a REBATE: a negative commission of exactly −15 % of the equivalent
    // taker fee at the same fill (the per-market rebate, modelled per-fill).
    let maker = POLYMARKET_V2_FEE_CURVE.commission(true, 100.0, 0.25);
    assert!(maker < 0.0, "maker rebate must be a negative commission, got {maker}");
    assert_eq!(maker, -(0.15 * taker));
    // Fee vanishes at the certainty bounds on both sides — a rebate must not manufacture cost
    // (or income) at p=0 / p=1.
    for is_maker in [false, true] {
        assert_eq!(POLYMARKET_V2_FEE_CURVE.commission(is_maker, 100.0, 0.0), 0.0);
        assert_eq!(POLYMARKET_V2_FEE_CURVE.commission(is_maker, 100.0, 1.0), 0.0);
    }
}

/// The LANE sub-key rows, pinned verbatim. These are NOT roster ids (so
/// `every_roster_venue_has_a_fee_schedule` below ignores them, exactly as `venue_tif`'s roster
/// gate ignores `"binance-perp"`); they exist because binance's and aster's `exec.rs` route
/// SPOT vs PERP on the `.P` suffix and the lanes are priced differently.
///
/// BOTH pairs are genuinely two-priced now. Binance: the SPOT row must stay 10/10 and the PERP
/// row 2/5 — collapsing them, which is what the table did before these rows existed, charges a
/// `BTCUSDT.P` paper/backtest mount 5x maker and 2x taker. Aster joined them on 2026-08-05,
/// when both of its rows were finally sourced from the venue's published fee pages and turned
/// out to differ (0.5 bps maker on spot vs 0 on perp). vike-catalog's
/// `a_dual_lane_venue_prices_its_two_lanes_apart` is the gate that catches a collapse through
/// the REAL resolver; this test pins the VALUES the resolver hands back.
///
/// ⚠ **A venue can have more than two lanes.** Aster has THREE: the same 2026-08-05 sweep that
/// confirmed its two rows also found its ONE perp order API charging three taker rates by
/// contract class, so `"aster-perp-usd1"` is pinned here alongside them. The pin is not
/// decorative — that row is 8x cheaper than `"aster-perp"`, so a collapse would flatter every
/// USD1-settled backtest rather than merely over-charge it.
#[test]
fn lane_rows_are_pinned() {
    // binance: the two lanes are genuinely different schedules.
    assert_eq!(
        fee_schedule_for("binance"),
        FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 },
        "binance BARE id is the SPOT lane"
    );
    assert_eq!(
        fee_schedule_for("binance-perp"),
        FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 },
        "binance-perp is the USDⓈ-M futures lane"
    );
    assert_ne!(
        fee_schedule_for("binance"),
        fee_schedule_for("binance-perp"),
        "the whole point of the lane split — a perp mount must not pay spot fees"
    );
    // aster: GENUINELY DUAL-PRICED as of 2026-08-05. Both rows now come from Aster's own
    // published fee pages (cited on the arms), and they DIFFER — the spot lane charges a 0.5
    // bps maker fee while the USDⓈ-M perp lane charges none. Until then both rows carried one
    // unsourced Binance-perp-shaped assumption and this block asserted them EQUAL; the
    // assertion is inverted rather than deleted, and vike-catalog's aster classification moved
    // LANE_NAMED_SAME_PRICE -> LANE_PRICED to match.
    assert_eq!(
        fee_schedule_for("aster"),
        FeeSchedule::PercentMakerTaker { maker_bps: 0.5, taker_bps: 4.0 },
        "aster BARE id is the SPOT lane — docs.asterdex.com/trading/spot/spot-fee-structure"
    );
    assert_eq!(
        fee_schedule_for("aster-perp"),
        FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 4.0 },
        "aster-perp is the USDT-Perpetual lane — \
             docs.asterdex.com/trading/perpetuals/fees-and-specs/fees"
    );
    assert_ne!(
        fee_schedule_for("aster"),
        fee_schedule_for("aster-perp"),
        "aster's two lanes are priced apart (0.5 bps maker on spot, 0 on perp) — collapsing \
             them back onto one row re-introduces the unsourced assumption this replaced"
    );
    // aster's THIRD lane: its perp order API is priced by CONTRACT CLASS, and the USD1-settled
    // contracts take an 8x cheaper taker leg. Published (USD1-Perpetual 0%/0.005%) AND measured
    // on all three live USD1 contracts, 2026-08-05 — see the arm for both citations.
    assert_eq!(
        fee_schedule_for("aster-perp-usd1"),
        FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.5 },
        "aster-perp-usd1 is the USD1-Perpetual lane — \
             docs.asterdex.com/trading/perpetuals/fees-and-specs/fees"
    );
    assert_ne!(
        fee_schedule_for("aster-perp-usd1"),
        fee_schedule_for("aster-perp"),
        "the USD1 perp lane collapsed back onto the crypto perp row — a USD1-settled mount is \
             being charged 8x its real taker fee"
    );
    // A lane key belongs to its OWN venue only — no other venue grew one by accident.
    for stray in [
        "bybit-perp",
        "okx-perp",
        "hyperliquid-perp",
        "deribit-perp",
        // The class sub-key convention must not be assumed to generalise: only the lanes
        // actually declared above exist. `aster-perp-stock` in particular is the measured but
        // deliberately-unencoded equity class — a future PR that adds it must add the ARM, not
        // just the key, and this row is what makes forgetting that loud.
        "aster-perp-stock",
        "binance-perp-usd1",
    ] {
        assert_eq!(
            fee_schedule_for(stray),
            FeeSchedule::Free,
            "{stray} is not a declared lane key — it must ride the fail-safe fallback"
        );
    }
}

#[test]
fn registry_has_real_published_defaults() {
    assert_eq!(
        fee_schedule_for("binance"),
        FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 }
    );
    assert_eq!(
        fee_schedule_for("bybit"),
        FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 }
    );
    assert_eq!(
        fee_schedule_for("okx"),
        FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 }
    );
    assert_eq!(
        fee_schedule_for("deribit"),
        FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 }
    );
    assert_eq!(
        fee_schedule_for("hyperliquid"),
        FeeSchedule::PercentMakerTaker { maker_bps: 1.5, taker_bps: 4.5 }
    );
    assert_eq!(
        fee_schedule_for("aster"),
        FeeSchedule::PercentMakerTaker { maker_bps: 0.5, taker_bps: 4.0 }
    );
    assert_eq!(
        fee_schedule_for("ibkr"),
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 }
    );
    assert_eq!(fee_schedule_for("ibkr_cpapi"), fee_schedule_for("ibkr"));
    for free in ["polymarket", "oanda", "ig", "fxcm", "dukascopy", "alpaca", "nasdaq"] {
        assert_eq!(fee_schedule_for(free), FeeSchedule::Free, "{free} is free/unknown");
    }
}

/// Completeness vs the canonical roster (`crate::venues::VENUES`): every roster venue resolves
/// to a DELIBERATE fee outcome — either an explicit, non-`Free` published schedule arm
/// (`WITH_SCHEDULE`, pinned verbatim in `registry_has_real_published_defaults` above) or a
/// DOCUMENTED-`Free` venue (`KNOWN_FREE` — its NAMED arm returns `Free` on purpose). A roster
/// venue in NEITHER set fails here, so a NEW venue cannot silently inherit `Free` from the `_`
/// fallback: it must be classified one way or the other. Same shape as tif's
/// `every_roster_venue_is_classified` (#498).
#[test]
fn every_roster_venue_has_a_fee_schedule() {
    // roster venues with an explicit, non-Free published schedule arm
    const WITH_SCHEDULE: &[&str] =
        &["binance", "bybit", "okx", "deribit", "aster", "hyperliquid", "ibkr"];
    // roster venues that DELIBERATELY resolve to Free, each with a NAMED arm documenting why:
    // polymarket (CLOB V2 dropped feeRateBps), oanda/ig/fxcm/dukascopy (FX/CFD spread-based),
    // alpaca (US-equities cash), ctrader (broker-dependent — conservative spread-based, TODO).
    // A venue here is a CLASSIFIED Free, never a silent `_`-fallback Free.
    #[rustfmt::skip]
        const KNOWN_FREE: &[&str] = &[
            "polymarket", "oanda", "ig", "fxcm", "dukascopy", "alpaca", "ctrader",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): move to WITH_SCHEDULE the moment a real arm lands
        ];
    assert_eq!(
        WITH_SCHEDULE.len() + KNOWN_FREE.len(),
        crate::venues::VENUES.len(),
        "every roster venue classified exactly once (explicit schedule or documented Free)"
    );
    for &v in crate::venues::VENUES {
        let scheduled = WITH_SCHEDULE.contains(&v);
        let known_free = KNOWN_FREE.contains(&v);
        assert!(
            scheduled ^ known_free,
            "roster venue {v} must be classified exactly once (explicit schedule arm or \
                 documented KNOWN_FREE)"
        );
        let fee = fee_schedule_for(v);
        if scheduled {
            assert_ne!(fee, FeeSchedule::Free, "{v}: declared a real schedule, must not be Free");
        } else {
            assert_eq!(fee, FeeSchedule::Free, "{v}: KNOWN_FREE venue must resolve to Free");
        }
    }
}
