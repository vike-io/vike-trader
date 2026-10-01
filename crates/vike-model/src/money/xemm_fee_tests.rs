use super::*;

/// The v1 pair — hyperliquid maker (1.5 bps) resting, okx taker (5.0 bps) hedging. The exact
/// arithmetic is pinned bit-for-bit because this number IS the maker's break-even offset: a
/// silent change to either registry row moves every quote the maker rests.
#[test]
fn xemm_round_trip_fee_is_maker_plus_taker_by_default() {
    let fee = xemm_round_trip_fee(fee_schedule_for("hyperliquid"), fee_schedule_for("okx"), false)
        .expect("both legs are PercentMakerTaker");
    assert_eq!(
        fee.to_bits(),
        (1.5_f64 / 10_000.0 + 5.0 / 10_000.0).to_bits(),
        "hyperliquid maker + okx taker = 6.5 bps, naive fold"
    );
}

/// The conservative opt-in charges the MAKER venue's TAKER rate instead — strictly larger, so
/// it can only widen the maker spread, never narrow it below break-even.
#[test]
fn assume_taker_on_maker_leg_charges_the_taker_rate_on_both_legs() {
    let maker = fee_schedule_for("hyperliquid");
    let taker = fee_schedule_for("okx");
    let default = xemm_round_trip_fee(maker, taker, false).expect("some");
    let conservative = xemm_round_trip_fee(maker, taker, true).expect("some");
    assert_eq!(
        conservative.to_bits(),
        (4.5_f64 / 10_000.0 + 5.0 / 10_000.0).to_bits(),
        "hyperliquid TAKER + okx taker = 9.5 bps"
    );
    assert!(conservative > default, "the conservative reading can only widen");
}

/// The four shapes with no flat-fraction equivalent are REFUSED, on EITHER leg. A `0.0` here
/// would price a fee-bearing round trip as free — see the fn doc.
#[test]
fn shapes_without_a_flat_rate_are_refused_not_defaulted_to_zero() {
    let ok = FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 };
    let refused = [
        FeeSchedule::Free,           // oanda/ig/alpaca/polymarket/…
        fee_schedule_for("ibkr"),    // PerShareWithFloor
        POLYMARKET_V2_FEE_CURVE,     // ProbabilityScaled
        fee_schedule_for("deribit"), // PercentOfUnderlying
    ];
    for s in refused {
        assert_eq!(
            xemm_round_trip_fee(s, ok, false),
            None,
            "{s:?} on the MAKER leg has no flat fraction — must refuse, not default to 0.0"
        );
        assert_eq!(
            xemm_round_trip_fee(ok, s, false),
            None,
            "{s:?} on the TAKER leg has no flat fraction — must refuse"
        );
    }
}

/// The refusal is by SHAPE, not by VALUE: a genuine zero-rate percent schedule is a legitimate
/// answer of `0.0`, not a missing one. (Matching on the value would make a zero-fee venue
/// indistinguishable from an unknown one.)
#[test]
fn a_genuine_zero_bps_percent_schedule_is_accepted() {
    let zero = FeeSchedule::PercentMakerTaker { maker_bps: 0.0, taker_bps: 0.0 };
    assert_eq!(
        xemm_round_trip_fee(zero, zero, false).map(f64::to_bits),
        Some(0.0_f64.to_bits()),
        "a real 0-bps schedule resolves to 0.0, unlike the refused shapes"
    );
}

/// The maker-rate default is only defensible because NO roster venue supports post-only, which
/// is why the passive clamp (not an order flag) is what keeps the maker leg passive. If a venue
/// ever gains post-only this test BREAKS, forcing the default to be revisited rather than
/// silently inherited.
#[test]
fn the_maker_rate_default_is_justified_by_the_absence_of_post_only() {
    for &v in crate::venues::VENUES {
        assert!(
            !crate::venue_caps::caps_for(v).supports_post_only,
            "{v} now supports post-only: revisit `assume_taker_on_maker_leg`'s default and \
                 whether xEMM should send a post-only maker leg instead of relying on the clamp"
        );
    }
}

// --- maker_round_trip_fee (the SINGLE-VENUE twin) --------------------------------------------

/// Both legs are MAKER fills on the SAME venue, so the total is `maker + maker` — never
/// `maker + taker`, which is the xEMM shape and is strictly larger on every row where the two
/// differ. Pinned on the row the the CI box finding was measured against (bybit 2/5.5 bps).
#[test]
fn a_single_venue_round_trip_pays_the_maker_rate_twice() {
    let bybit = fee_schedule_for("bybit");
    assert_eq!(bybit, FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 });
    assert_eq!(
        maker_round_trip_fee(bybit).map(f64::to_bits),
        Some((2.0_f64 / 10_000.0 + 2.0_f64 / 10_000.0).to_bits()),
        "maker + maker = 4 bps round trip on bybit VIP0"
    );
    // ...and it is STRICTLY CHEAPER than the maker-in/taker-out bar, which is what makes it a
    // LOWER bound rather than the whole cost of getting flat (see the fn doc).
    let flatten = xemm_round_trip_fee(bybit, bybit, false).expect("percent shape");
    assert!(
        maker_round_trip_fee(bybit).unwrap() < flatten,
        "a taker exit costs more than a passive round trip; the floor covers only the latter"
    );
}

/// THE MEASUREMENT THIS SEAM EXISTS FOR: on bybit BTCUSDT at the mid the finding was taken at,
/// the per-side break-even half-spread is ~126 ticks — more than DOUBLE
/// `MakerMountConfig::crypto`'s 60-tick `max_half_spread_ticks` ceiling. The arithmetic lives
/// here so the claim is checkable, not merely asserted in prose.
#[test]
fn bybit_btcusdt_break_even_half_spread_exceeds_the_crypto_mount_cap() {
    let fee = maker_round_trip_fee(fee_schedule_for("bybit")).expect("percent shape");
    let (mid, tick, cap_ticks) = (63_050.0_f64, 0.1_f64, 60.0_f64);
    let break_even_half = 0.5 * fee * mid; // the ½ that lives at the consumer (see the fn doc)
    assert!(
        (break_even_half - 12.61).abs() < 0.01,
        "break-even half-spread ≈ $12.61, got {break_even_half}"
    );
    assert!(
        break_even_half / tick > 2.0 * cap_ticks,
        "126 ticks needed vs a 60-tick ceiling: {} ticks",
        break_even_half / tick
    );
}

/// The refusal is by SHAPE, for the identical reason as [`xemm_round_trip_fee`]'s: a `Free` FX
/// venue's maker is not free (it is charged through the spread), a per-share or p(1−p) fee has
/// no flat fraction, and deribit's flat fraction UNDERSTATES the real charge. Answering `0.0`
/// for any of them is the "unknown fee silently became zero" failure this whole seam is against.
#[test]
fn shapes_without_a_flat_rate_are_refused_rather_than_floored_at_zero() {
    for s in [
        FeeSchedule::Free,
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 },
        POLYMARKET_V2_FEE_CURVE,
        FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 },
    ] {
        assert_eq!(maker_round_trip_fee(s), None, "{s:?} must REFUSE, not answer 0.0");
    }
    // ...while a genuine, measured 0-bps percent schedule IS an answer (aster's perp maker row).
    assert_eq!(
        maker_round_trip_fee(fee_schedule_for("aster-perp")).map(f64::to_bits),
        Some(0.0_f64.to_bits()),
        "a measured 0-bps maker is a real zero, not a refusal"
    );
}
