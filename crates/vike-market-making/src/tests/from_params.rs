//! [`rearm_fee_floor`], the mount's re-arm of the preset's break-even floor. The reader itself
//! (`SpreadMaker::from_params`) and its tests moved with the maker to the spread maker file
//! (`strategies/market_making/spread_maker.rs`).

use super::*;

// --- the presets, the key set and the fee re-arm (`preset.rs`) --------------------------------

fn table(src: &str) -> Params {
    toml::from_str(src).unwrap()
}

/// The break-even floor moves to the account's own rate only when it MOVES; an account nobody
/// answered for, a fee shape with no flat fraction, or the rate already held keeps the maker.
#[test]
fn the_fee_floor_is_rearmed_only_when_it_moves() {
    let held = table("preset = \"crypto\"\nround_trip_fee_rate = 0.0004");
    use vike_model::FeeSchedule;
    let pct = |maker_bps: f64| FeeSchedule::PercentMakerTaker { maker_bps, taker_bps: 5.5 };
    let moved = rearm_fee_floor(&held, Some(pct(1.0)), "v", "s").expect("2 bps differs from 4");
    assert_eq!(moved.get("round_trip_fee_rate").and_then(ParamValue::as_f64), Some(2e-4));
    assert_eq!(moved.get("preset"), held.get("preset"), "every other key kept");
    assert_eq!(rearm_fee_floor(&held, Some(pct(2.0)), "v", "s"), None, "the rate it holds");
    assert_eq!(rearm_fee_floor(&held, None, "v", "s"), None, "no engine answered");
    assert_eq!(rearm_fee_floor(&held, Some(FeeSchedule::Free), "v", "s"), None, "no flat rate");
    let unarmed = table("preset = \"outcome_token\"");
    assert_eq!(
        rearm_fee_floor(&unarmed, Some(pct(1.0)), "v", "s")
            .and_then(|p| p.get("round_trip_fee_rate").and_then(ParamValue::as_f64)),
        Some(2e-4),
        "a maker with no floor is armed by a flat rate"
    );
}
