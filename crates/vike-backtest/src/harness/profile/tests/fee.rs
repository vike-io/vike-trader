//! The `[engine.fee]` schedule: every fee shape, the venue lookup, and their refusals.

use super::*;

// --- G7: the fee schedule -----------------------------------------------------------------

#[test]
fn fee_is_absent_unless_configured() {
    assert!(BacktestProfile::from_toml_str(BAR_TOML).unwrap().engine.fee.is_none());
}

#[test]
fn probability_scaled_fee_builds_the_curve() {
    let p = BacktestProfile::from_toml_str(CROSS_VENUE_TOML).unwrap();
    let schedule = p.engine.fee.as_ref().unwrap().build().unwrap();
    assert_eq!(
        schedule,
        FeeSchedule::ProbabilityScaled {
            taker_rate: 0.072,
            maker_rate: 0.0,
            maker_rebate_share: 0.0,
        }
    );
    // The number the live bot pays: 0.072·p·(1−p) per share, at the fill's own price.
    assert_eq!(schedule.commission(false, 1.0, 0.25), 0.072 * 0.25 * 0.75);
}

#[test]
fn an_unknown_fee_kind_is_rejected() {
    let toml = CROSS_VENUE_TOML.replace("\"probability_scaled\"", "\"prob_scaled\"");
    refused_at_load(&toml, HarnessError::Validation, &["unknown engine.fee.kind"]);
}

#[test]
fn a_fee_schedule_plus_a_flat_fee_rate_is_rejected() {
    let toml = CROSS_VENUE_TOML.replace("cash = 1000.0", "cash = 1000.0\nfee_rate = 0.001");
    refused_at_load(&toml, HarnessError::Validation, &["two different cost models"]);
}

// --- realism/fee-shape-family + realism/venue-fee-schedule-lookup -------------------------
//
// The G7 table above reached ONE of `vike_model::FeeSchedule`'s five shapes. These cover the
// rest, and `kind = "venue"` — the venue's own published schedule, resolved through the pair
// the paper mount resolves it through.

/// An `[engine.fee]` body, parsed on its own.
fn fee_cfg(body: &str) -> FeeCfg {
    toml::from_str(body).expect("the [engine.fee] body parses")
}

/// A resolved data slice, as `(venue, symbol)` rows.
fn fee_series(rows: &[(&str, &str)]) -> Vec<SeriesRef> {
    rows.iter().map(|(v, s)| SeriesRef::new(*v, *s)).collect()
}

/// The headline of the per-share family: the FLOOR reaches the fill. `maker_taker_rates()`
/// answers `(0.0, 0.0)` for this shape, so the pre-existing flatten-everything-but-the-curve
/// routing would have charged a configured IBKR schedule exactly NOTHING — which is why the
/// second assertion is here rather than left implicit.
#[test]
fn the_per_share_floor_shape_builds_and_its_floor_is_what_a_flat_rate_cannot_express() {
    let schedule =
        fee_cfg("kind = \"per_share_with_floor\"\nper_share = 0.005\nmin = 1.0\nmax_pct = 0.005\n")
            .build()
            .expect("the IBKR-shaped schedule builds");
    assert_eq!(
        schedule,
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 1.0, max_pct: 0.005 }
    );
    // 10 shares × $0.005 is $0.05 of per-share fee, and the order pays the $1.00 minimum —
    // the cost a small-size high-frequency configuration is eaten by live.
    assert_eq!(schedule.commission(false, 10.0, 100.0), 1.0);
    // ...and the reason the engine must not flatten it.
    assert_eq!(schedule.maker_taker_rates(), (0.0, 0.0));
}

/// The floor is REQUIRED, because a per-share fee with no minimum is a flat rate wearing a
/// different name — the configuration the shape exists to escape.
#[test]
fn a_per_share_shape_with_no_floor_is_refused() {
    let err = fee_cfg("kind = \"per_share_with_floor\"\nper_share = 0.005\n")
        .build()
        .expect_err("a floorless floor shape is refused");
    match err {
        HarnessError::Validation(m) => {
            assert!(m.contains("needs BOTH per_share and min"), "{m}")
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// `min = 0.0` is accepted — the zero is then an ASSERTION that there is no floor rather than
/// an omission, which is the whole difference the refusal above buys.
#[test]
fn an_explicit_zero_floor_is_accepted() {
    let schedule = fee_cfg("kind = \"per_share_with_floor\"\nper_share = 0.005\nmin = 0.0\n")
        .build()
        .expect("an explicit zero floor is a legal assertion");
    assert_eq!(
        schedule,
        FeeSchedule::PerShareWithFloor { per_share: 0.005, min: 0.0, max_pct: 0.0 }
    );
}

/// Deribit's shape, and the trap that makes its cap REQUIRED: `commission` is
/// `min(bps × premium, cap × premium)`, so a zero cap zeroes the whole commission.
#[test]
fn the_percent_of_underlying_shape_builds_and_a_zero_cap_is_refused() {
    let schedule =
        fee_cfg("kind = \"percent_of_underlying\"\nbps = 3.0\npremium_cap_pct = 0.125\n")
            .build()
            .expect("the Deribit-shaped schedule builds");
    assert_eq!(schedule, FeeSchedule::PercentOfUnderlying { bps: 3.0, premium_cap_pct: 0.125 });
    // The cap binds for a cheap deep-OTM option, which is what a flat rate could not say.
    assert_eq!(schedule.commission_with_underlying(1.0, 100.0, 60_000.0), 100.0 * 0.125);

    let err = fee_cfg("kind = \"percent_of_underlying\"\nbps = 3.0\npremium_cap_pct = 0.0\n")
        .build()
        .expect_err("a zero cap charges nothing and is refused");
    match err {
        HarnessError::Validation(m) => {
            assert!(m.contains("premium_cap_pct must be > 0"), "{m}");
            assert!(m.contains("no fee at all"), "the refusal says what it costs: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The two-sided flat shape, which `engine.fee_rate` can only charge to both sides at once.
#[test]
fn the_percent_maker_taker_shape_says_the_two_sides_apart() {
    let schedule = fee_cfg("kind = \"percent_maker_taker\"\nmaker_bps = 2.0\ntaker_bps = 5.0\n")
        .build()
        .expect("the two-sided flat shape builds");
    assert_eq!(schedule, FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 });
    // A table naming NEITHER side describes no cost, which `kind = "free"` already says.
    let err = fee_cfg("kind = \"percent_maker_taker\"\n")
        .build()
        .expect_err("a sideless percent shape is refused");
    assert!(matches!(err, HarnessError::Validation(_)));
    assert_eq!(fee_cfg("kind = \"free\"\n").build().unwrap(), FeeSchedule::Free);
}

/// A knob written under the wrong `kind` is REFUSED rather than ignored — the
/// "a key nothing reads is worse than an unimplemented feature" rule, applied inside one
/// table, where the cost of ignoring it is a run priced at something nobody wrote.
#[test]
fn a_knob_belonging_to_another_kind_is_refused_by_name() {
    let err = fee_cfg("kind = \"free\"\nper_share = 0.005\n")
        .build()
        .expect_err("a per-share knob under kind = free is refused");
    match err {
        HarnessError::Validation(m) => {
            assert!(m.contains("engine.fee.per_share"), "it names the knob: {m}");
            assert!(m.contains("per_share_with_floor"), "...and the kind that reads it: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
    // The three pre-existing curve rates are plain defaulted scalars, so only a NON-ZERO one
    // is detectable — and a written zero configures nothing under either kind anyway.
    assert!(fee_cfg("kind = \"free\"\ntaker_rate = 0.0\n").build().is_ok());
    assert!(fee_cfg("kind = \"free\"\ntaker_rate = 0.07\n").build().is_err());
}

/// ⚠ **The venue lookup's whole point: the `.P` LANE.** A `BTCUSDT.P` run on binance is
/// costed at the PERP row, which is priced completely differently from the venue's spot row —
/// and the answer is the one `crates/vike-mount/src/engine.rs`'s `make_engine` computes for its
/// own `static_default`, asserted here as that same expression rather than as a copied
/// number.
#[test]
fn the_venue_kind_resolves_the_perp_lane_from_the_runs_own_symbol() {
    let cfg = fee_cfg("kind = \"venue\"\nvenue = \"binance\"\n");
    let perp =
        cfg.build_for(&fee_series(&[("binance", "BTCUSDT.P")])).expect("the perp lane resolves");
    assert_eq!(perp, vike_model::fee_schedule_for(vike_catalog::fee_lane("binance", "BTCUSDT.P")));
    let spot =
        cfg.build_for(&fee_series(&[("binance", "BTCUSDT")])).expect("the spot lane resolves");
    assert_eq!(spot, vike_model::fee_schedule_for("binance"));
    assert_ne!(perp, spot, "the two lanes are priced apart — that IS the lookup");
    // The slice-free door (`build`, which `refusals` drives at load) answers the BARE lane.
    assert_eq!(cfg.build().unwrap(), spot);
}

/// A venue the run does not trade has no symbol to read a lane from, and answering with the
/// bare-venue row would charge a perp run at spot fees — the mispricing the lane key ended.
#[test]
fn a_venue_the_run_does_not_load_is_refused() {
    let err = fee_cfg("kind = \"venue\"\nvenue = \"binance\"\n")
        .build_for(&fee_series(&[("polymarket", "0xTOK")]))
        .expect_err("a venue the slice does not trade is refused");
    match err {
        HarnessError::Validation(m) => {
            assert!(m.contains("does not load"), "{m}");
            assert!(m.contains("polymarket"), "it names what the run DOES trade: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// One `EngineParams::fee_schedule` cannot serve two lanes, so a run holding both is refused
/// rather than half-mispriced — and `engine.fee.symbol` is the named way out.
#[test]
fn a_run_straddling_two_fee_lanes_is_refused_and_an_explicit_symbol_resolves_it() {
    let cfg = fee_cfg("kind = \"venue\"\nvenue = \"binance\"\n");
    let both = fee_series(&[("binance", "BTCUSDT"), ("binance", "ETHUSDT.P")]);
    match cfg.build_for(&both).expect_err("two lanes, one schedule") {
        HarnessError::Validation(m) => {
            assert!(m.contains("straddle"), "{m}");
            assert!(m.contains("engine.fee.symbol"), "it names the way out: {m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
    let pinned = fee_cfg("kind = \"venue\"\nvenue = \"binance\"\nsymbol = \"ETHUSDT.P\"\n");
    assert_eq!(
        pinned.build_for(&both).expect("an explicit symbol names the lane"),
        vike_model::fee_schedule_for("binance-perp")
    );
}

/// `pm_curve` exists because `fee_schedule_for("polymarket")` is a deliberate `Free`, so the
/// venue lookup would otherwise cost a prediction-market run at exactly zero. On any other
/// venue the flag reaches a function that delegates, so it would configure nothing — refused.
#[test]
fn pm_curve_opts_into_the_v2_regime_and_is_refused_off_polymarket() {
    let poly = fee_series(&[("polymarket", "0xTOK")]);
    let plain = fee_cfg("kind = \"venue\"\nvenue = \"polymarket\"\n");
    assert_eq!(plain.build_for(&poly).unwrap(), FeeSchedule::Free);
    let curved = fee_cfg("kind = \"venue\"\nvenue = \"polymarket\"\npm_curve = true\n");
    assert_eq!(curved.build_for(&poly).unwrap(), vike_model::POLYMARKET_V2_FEE_CURVE);
    let err = fee_cfg("kind = \"venue\"\nvenue = \"binance\"\npm_curve = true\n")
        .build()
        .expect_err("pm_curve is polymarket-only");
    match err {
        HarnessError::Validation(m) => assert!(m.contains("polymarket-only"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The unknown-`kind` refusal is BUILT from [`FeeCfg::KINDS`], so an operator who typos is
/// never shown a roster the code does not accept. This is the `engine.sizer` rule
/// (`profile_surface`'s `the_unknown_sizer_kind_message_names_every_arm`) asserted on this
/// side, where the roster is a const rather than a match this crate can parse.
#[test]
fn the_unknown_fee_kind_message_names_every_accepted_kind() {
    let err = fee_cfg("kind = \"prob_scaled\"\n").build().expect_err("a typo is refused");
    let HarnessError::Validation(msg) = err else { panic!("expected a validation error") };
    assert!(msg.contains("unknown engine.fee.kind"), "{msg}");
    for kind in FeeCfg::KINDS {
        assert!(msg.contains(kind), "the refusal does not name {kind:?}: {msg}");
        // ...and every named kind is one `build` actually accepts, or the roster is a lie.
        assert!(
            fee_cfg(&format!("kind = {kind:?}\n")).validate_shape().is_err()
                || fee_cfg(&format!("kind = {kind:?}\n")).build().is_ok(),
            "{kind:?} is neither buildable bare nor refused with its own reason"
        );
    }
}

/// A `kind = "venue"` table with no venue is refused at LOAD, through the door
/// `BacktestProfile::refusals` drives — so the one venue-kind rule that CAN be answered
/// without the data slice is answered there.
#[test]
fn the_venue_kind_needs_a_venue_and_says_so_at_load() {
    let toml = BAR_TOML.replace("fee_rate = 0.001", "[engine.fee]\nkind = \"venue\"");
    refused_at_load(&toml, HarnessError::Validation, &["needs engine.fee.venue"]);
}
