use super::*;

#[test]
fn pm_curve_registry_is_opt_in_and_default_compatible() {
    // The DEFAULT registry is untouched: polymarket stays Free (byte-identical consumers —
    // paper fill path, snapshot cost display, `vike_tradehub_core::resolve_fee_schedule`'s static
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
/// `every_roster_venue_has_a_fee_schedule` below ignores them, exactly as `venue_time_in_force`'s roster
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
        "ctrader-perp",
        "ibkr-stock",
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
        FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_percent: 0.125 }
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
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_percent: 0.01 }
    );
    assert_eq!(fee_schedule_for("ibkr_cpapi"), fee_schedule_for("ibkr"));
    assert_eq!(
        fee_schedule_for("ctrader"),
        FeeSchedule::PercentMakerTaker { maker_bps: 0.3, taker_bps: 0.3 }
    );
    assert_eq!(
        fee_schedule_for("dukascopy"),
        FeeSchedule::PercentMakerTaker { maker_bps: 0.3, taker_bps: 0.3 }
    );
    for free in ["polymarket", "oanda", "ig", "fxcm", "alpaca", "nasdaq"] {
        assert_eq!(fee_schedule_for(free), FeeSchedule::Free, "{free} is free/unknown");
    }
}

/// The commission audit of 2026-11-08 (ratio = this table ÷ the venue, on real demo fills): each row
/// it found wrong, pinned to what the venue charged. Red on the table before it: dukascopy was `Free`,
/// ctrader a per-unit 0.00003 USD, and the deribit futures, deribit spot, okx spot and both ibkr lanes
/// had no row (a lane key with no row rides the `Free` fallback).
#[test]
fn the_commission_audit_rows_charge_what_the_venue_charged() {
    let close = |got: f64, want: f64, what: &str| {
        assert!((got - want).abs() < 1e-12, "{what}: {got}, the venue charged {want}");
    };
    // dukascopy: USDJPY 1,000 units at 150 is 150,000 JPY of value, 1,000 USD: the venue charged 0.03
    // USD, 4.5 JPY. $30 a million either way.
    close(fee_schedule_for("dukascopy").commission(false, 1_000.0, 150.0), 4.5, "dukascopy USDJPY");
    close(fee_schedule_for("dukascopy").commission(true, 1_000.0, 1.0), 0.03, "dukascopy maker");
    // ctrader: $30 a million on FX and metals, nothing on a CFD.
    close(
        fee_schedule_for("ctrader").commission(false, 1_000.0, 1.0),
        0.03,
        "ctrader USD-based FX",
    );
    close(
        fee_schedule_for("ctrader").commission(false, 1.0, 2_600.0),
        0.078,
        "ctrader XAUUSD 1 oz",
    );
    assert_eq!(fee_schedule_for("ctrader-cfd"), FeeSchedule::Free);
    // deribit: a dated future and a spot pair at the measured 5 bps taker, not the options 3.
    for lane in ["deribit-futures", "deribit-spot"] {
        assert_eq!(
            fee_schedule_for(lane),
            FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 5.0 },
            "{lane}"
        );
    }
    // okx spot: the regular user's 8 / 10 bps, not the SWAP 2 / 5.
    assert_eq!(
        fee_schedule_for("okx-spot"),
        FeeSchedule::PercentMakerTaker { maker_bps: 8.0, taker_bps: 10.0 }
    );
    // ibkr forex: 0.2 bps of the value, 2.00 an order at least; a 25,000 EUR order pays the minimum,
    // where the stock row charged $0.005 a unit (125.00).
    let fx = fee_schedule_for("ibkr-fx");
    assert_eq!(fx, FeeSchedule::BpsWithFloor { bps: 0.2, min: 2.0 });
    assert_eq!(fx.commission(false, 25_000.0, 1.08), 2.0);
    close(fx.commission(false, 2_000_000.0, 1.08), 43.2, "ibkr 2M EUR.USD");
    assert_eq!(fee_schedule_for("ibkr").commission(false, 25_000.0, 1.08), 125.0);
    // ibkr futures: 0.85 a contract, never the multiplier's.
    let fut = fee_schedule_for("ibkr-futures");
    assert_eq!(fut, FeeSchedule::PerShareWithFloor { per_share: 0.85, min: 0.0, max_percent: 0.0 });
    close(
        fut.commission_on(false, 2.0, 6_000.0, vike_model::ContractValue::linear(50.0)),
        1.7,
        "2 ES",
    );
}

/// FEES-AUDIT 2026-10-26 §4.11: the static rows that disagreed with what the node measured, each
/// pinned to its measurement. The deribit and hyperliquid LANES are resolved by
/// `crate::fee_lane` (its `a_shape_lane_prices_apart_from_the_bare_row` is the
/// other half of this pin); the cTrader row is the venue's own, keyed by the bare id.
#[test]
fn the_shape_lanes_are_pinned() {
    // deribit perpetual: the node's two ETH-PERPETUAL taker fills were 0.0000021 ETH each, 5.0 bps of
    // the 10 USD contract. Flat percent, NOT the options row (3 bps of the underlying, capped).
    assert_eq!(
        fee_schedule_for("deribit-futures"),
        FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 5.0 }
    );
    let taker_on_10_usd = fee_schedule_for("deribit-futures").commission(false, 10.0, 1.0);
    assert!(
        (taker_on_10_usd - 0.005).abs() < 1e-12,
        "5.0 bps of a 10 USD contract: {taker_on_10_usd}"
    );
    assert_eq!(
        fee_schedule_for("deribit-futures").commission(true, 10.0, 1.0),
        0.0,
        "maker: published 0"
    );
    assert_ne!(fee_schedule_for("deribit-futures"), fee_schedule_for("deribit"));
    assert_ne!(fee_schedule_for("deribit-spot"), fee_schedule_for("deribit"));

    // hyperliquid spot: the published base tier, apart from the perp row.
    assert_eq!(
        fee_schedule_for("hyperliquid-spot"),
        FeeSchedule::PercentMakerTaker { maker_bps: 4.0, taker_bps: 7.0 }
    );
    assert_ne!(fee_schedule_for("hyperliquid-spot"), fee_schedule_for("hyperliquid"));

    // bybit spot (a pair spelled `PAIR.S`, decision 0231): 10 / 10 bps at VIP0 (taker measured on the demo host:
    // `feeRate 0.001`), apart from the 2 / 5.5 perpetual row the bare id keeps.
    assert_eq!(
        fee_schedule_for("bybit-spot"),
        FeeSchedule::PercentMakerTaker { maker_bps: 10.0, taker_bps: 10.0 }
    );
    assert_ne!(fee_schedule_for("bybit-spot"), fee_schedule_for("bybit"));

    // ctrader: $30 per million USD of value per side (commission audit 2026-11-08), so 0.01 lot
    // (1,000 units) of a USD-based pair pays 0.03 USD, and the CFD lane is apart from it.
    let ctrader = fee_schedule_for("ctrader");
    let one_fill = ctrader.commission(false, 1_000.0, 1.0);
    assert!((one_fill - 0.03).abs() < 1e-12, "0.01 lot of USD value: {one_fill}");
    assert_ne!(fee_schedule_for("ctrader-cfd"), ctrader);

    // okx spot and the two ibkr lanes, apart from their bare rows.
    assert_ne!(fee_schedule_for("okx-spot"), fee_schedule_for("okx"));
    assert_ne!(fee_schedule_for("ibkr-fx"), fee_schedule_for("ibkr"));
    assert_ne!(fee_schedule_for("ibkr-futures"), fee_schedule_for("ibkr"));
}

/// Completeness vs the canonical roster (`vike_model::VENUES`): every roster venue resolves
/// to a DELIBERATE fee outcome — either an explicit, non-`Free` published schedule arm
/// (`WITH_SCHEDULE`, pinned verbatim in `registry_has_real_published_defaults` above) or a
/// DOCUMENTED-`Free` venue (`KNOWN_FREE` — its NAMED arm returns `Free` on purpose). A roster
/// venue in NEITHER set fails here, so a NEW venue cannot silently inherit `Free` from the `_`
/// fallback: it must be classified one way or the other. Same shape as tif's
/// `every_roster_venue_is_classified` (#498).
#[test]
fn every_roster_venue_has_a_fee_schedule() {
    // roster venues with an explicit, non-Free published schedule arm
    const WITH_SCHEDULE: &[&str] = &[
        "binance",
        "bybit",
        "okx",
        "deribit",
        "aster",
        "hyperliquid",
        "ibkr",
        "ctrader",
        "dukascopy",
    ];
    // roster venues that DELIBERATELY resolve to Free, each with a NAMED arm documenting why:
    // polymarket (CLOB V2 dropped feeRateBps), oanda/ig/fxcm (FX/CFD spread-based),
    // alpaca (US-equities cash). (ctrader left this list on 2026-10-27 and dukascopy on 2026-11-08:
    // each charges a measured commission.) A venue here is a CLASSIFIED Free, never a silent
    // `_`-fallback Free.
    #[rustfmt::skip]
        const KNOWN_FREE: &[&str] = &[
            "polymarket", "oanda", "ig", "fxcm", "alpaca",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): move to WITH_SCHEDULE the moment a real arm lands
        ];
    assert_eq!(
        WITH_SCHEDULE.len() + KNOWN_FREE.len(),
        vike_model::VENUES.len(),
        "every roster venue classified exactly once (explicit schedule or documented Free)"
    );
    for &v in vike_model::VENUES {
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
