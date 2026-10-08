use super::*;

/// A `RiskLimits` shaped like a real `from_properties` fetch: a populated instrument grid,
/// operator budget still at the `new()`/`Default` off-state.
fn venue_fetched_base() -> RiskLimits {
    RiskLimits {
        tick_size: Some(0.5),
        lot_size: Some(0.01),
        min_qty: Some(0.01),
        min_notional: Some(10.0),
        ..RiskLimits::new()
    }
}

/// A profile with ONLY operator-budget fields set (no venue-owned instrument fields).
fn operator_only_profile() -> ProfileRisk {
    ProfileRisk {
        max_notional_per_order: Some(25_000.0),
        max_total_exposure: Some(100_000.0),
        max_orders_per_window: Some(5),
        window_ms: 2000,
        max_leverage: Some(4.0),
        block_reduce_only_overshoot: true,
        required_free_bp_pct: 0.1,
        ..ProfileRisk::default()
    }
}

#[test]
fn apply_to_operator_only_leaves_venue_fields_byte_identical() {
    let base = venue_fetched_base();
    let profile = operator_only_profile();

    let got =
        profile.apply_to(base.clone(), GridSource::VenueFetched).expect("no venue field set -> Ok");

    assert_eq!(got.tick_size, base.tick_size);
    assert_eq!(got.lot_size, base.lot_size);
    assert_eq!(got.min_qty, base.min_qty);
    assert_eq!(got.min_notional, base.min_notional);
    assert_eq!(got.max_notional_per_order, Some(25_000.0));
    assert_eq!(got.max_total_exposure, Some(100_000.0));
    assert_eq!(got.max_orders_per_window, Some(5));
    assert_eq!(got.window_ms, 2000);
    assert_eq!(got.max_leverage, Some(4.0));
    assert!(got.block_reduce_only_overshoot);
    assert_eq!(got.im_requirement, Some(0.25), "4x leverage ⇒ 25% initial margin");
    assert_eq!(got.required_free_bp_pct, 0.1);
}

#[test]
fn apply_to_each_venue_owned_field_is_config_error_when_grid_fetched() {
    let cases: &[(&str, ProfileRisk)] = &[
        ("risk.tick_size", ProfileRisk { tick_size: Some(1.0), ..ProfileRisk::default() }),
        ("risk.lot_size", ProfileRisk { lot_size: Some(1.0), ..ProfileRisk::default() }),
        ("risk.min_qty", ProfileRisk { min_qty: Some(1.0), ..ProfileRisk::default() }),
        ("risk.min_notional", ProfileRisk { min_notional: Some(1.0), ..ProfileRisk::default() }),
    ];
    for (key, profile) in cases {
        let err = profile
            .apply_to(venue_fetched_base(), GridSource::VenueFetched)
            .expect_err(&format!("{key} set alongside a fetched grid must be Err"));
        match err {
            ProfileError::Validation(m) => {
                assert!(m.contains(key), "error must name `{key}`: {m}")
            }
            other => panic!("expected Validation error for {key}, got {other:?}"),
        }
    }
}

#[test]
fn apply_to_names_every_offending_field_in_one_error() {
    let profile =
        ProfileRisk { tick_size: Some(1.0), min_qty: Some(1.0), ..ProfileRisk::default() };
    let err = profile
        .apply_to(venue_fetched_base(), GridSource::VenueFetched)
        .expect_err("multiple venue fields set must still be Err");
    let ProfileError::Validation(m) = err else { panic!("expected Validation error") };
    assert!(m.contains("risk.tick_size"), "message: {m}");
    assert!(m.contains("risk.min_qty"), "message: {m}");
    assert!(!m.contains("risk.lot_size"), "message must not name an unset field: {m}");
}

#[test]
fn apply_to_no_grid_fetched_profile_supplies_instrument_fields() {
    let profile = ProfileRisk {
        tick_size: Some(0.25),
        lot_size: Some(0.5),
        min_qty: Some(0.75),
        min_notional: Some(2.5),
        ..ProfileRisk::default()
    };
    let got = profile
        .apply_to(RiskLimits::new(), GridSource::NoGridFetched)
        .expect("no venue grid was fetched -> profile instrument fields are allowed");
    assert_eq!(got.tick_size, Some(0.25));
    assert_eq!(got.lot_size, Some(0.5));
    assert_eq!(got.min_qty, Some(0.75));
    assert_eq!(got.min_notional, Some(2.5));
}

#[test]
fn apply_to_no_grid_fetched_ignores_bases_venue_fields() {
    let profile = ProfileRisk::default(); // no instrument fields set
    let got = profile
        .apply_to(venue_fetched_base(), GridSource::NoGridFetched)
        .expect("empty profile risk is always Ok");
    assert_eq!(got.tick_size, None, "NoGridFetched must use the profile's None, not base's Some");
    assert_eq!(got.lot_size, None);
    assert_eq!(got.min_qty, None);
    assert_eq!(got.min_notional, None);
}

#[test]
fn apply_to_matches_to_risk_limits_when_no_grid_fetched() {
    let p = ProfileRisk {
        max_notional_per_order: Some(50_000.0),
        max_total_exposure: Some(200_000.0),
        max_orders_per_window: Some(20),
        window_ms: 1000,
        max_leverage: Some(5.0),
        block_reduce_only_overshoot: true,
        required_free_bp_pct: 0.05,
        ..ProfileRisk::default()
    };
    let via_apply = p.apply_to(RiskLimits::new(), GridSource::NoGridFetched).unwrap();
    let via_to_risk_limits = p.to_risk_limits();
    assert_eq!(via_apply, via_to_risk_limits);
}

#[test]
fn apply_to_preserves_fields_neither_side_owns() {
    use crate::PriceCollar;
    let mut base = venue_fetched_base();
    base.im_by_symbol.insert("BTCUSDT".to_string(), 0.05);
    base.max_slippage_bps = Some(12.0);
    base.require_fillable = true;
    base.price_collar = Some(PriceCollar { pct: 0.1, abs_floor: 0.01 });
    base.collar_by_symbol.insert("ETHUSDT".to_string(), PriceCollar { pct: 0.2, abs_floor: 0.02 });

    let got = operator_only_profile().apply_to(base.clone(), GridSource::VenueFetched).unwrap();

    assert_eq!(got.im_by_symbol, base.im_by_symbol);
    assert_eq!(got.max_slippage_bps, base.max_slippage_bps);
    assert_eq!(got.require_fillable, base.require_fillable);
    assert_eq!(got.price_collar, base.price_collar);
    assert_eq!(got.collar_by_symbol, base.collar_by_symbol);
}

// ---------------------------------------------------------------------------------------
// `window_ms`'s serde default (0) must never silently clobber a real `window_ms` (e.g.
// `RiskLimits::new()`'s 1000) just because SOME profile was merged in.
// ---------------------------------------------------------------------------------------

/// A profile that arms `max_orders_per_window` but leaves `window_ms` at its serde default (0,
/// since a TOML profile that never mentions the key parses to that) must be REJECTED, not
/// silently merged with a zero window (cutoff == now ⇒ the throttle never trips even though
/// `max_orders_per_window` reads as armed).
#[test]
fn apply_to_rejects_armed_throttle_with_non_positive_window_ms() {
    let profile = ProfileRisk { max_orders_per_window: Some(5), ..ProfileRisk::default() };
    let err = profile
        .apply_to(venue_fetched_base(), GridSource::VenueFetched)
        .expect_err("max_orders_per_window set with window_ms <= 0 must be Err");
    match err {
        ProfileError::Validation(m) => {
            assert!(m.contains("window_ms"), "error must name window_ms: {m}");
            assert!(m.contains("max_orders_per_window"), "error must name the throttle: {m}");
        }
        other => panic!("expected Validation error, got {other:?}"),
    }
}

/// A profile that never mentions `max_orders_per_window` at all must NOT touch `window_ms` —
/// `base`'s own window (e.g. a real venue-fetched grid's `RiskLimits::new()`-derived 1000) is
/// carried through untouched, exactly like any other field neither side asked to change.
#[test]
fn apply_to_inherits_bases_window_ms_when_throttle_is_untouched() {
    let profile = ProfileRisk::default();
    let got = profile
        .apply_to(venue_fetched_base(), GridSource::VenueFetched)
        .expect("no max_orders_per_window is always Ok");
    assert_eq!(
        got.window_ms, 1000,
        "must inherit base's window_ms (1000), not the serde default 0"
    );
}

/// A profile that DOES arm the throttle with a valid positive window still wins over `base`'s
/// own window — the operator's explicit choice is respected, not silently ignored in favor of
/// the inherited default.
#[test]
fn apply_to_uses_profiles_window_ms_when_throttle_is_armed() {
    let profile =
        ProfileRisk { max_orders_per_window: Some(3), window_ms: 250, ..ProfileRisk::default() };
    let got = profile
        .apply_to(venue_fetched_base(), GridSource::VenueFetched)
        .expect("a positive window_ms alongside max_orders_per_window is Ok");
    assert_eq!(got.window_ms, 250);
    assert_eq!(got.max_orders_per_window, Some(3));
}

// ---------------------------------------------------------------------------------------
// `max_leverage` is the ONE operator-facing leverage name, and it is ENFORCED — it converts to
// the `im_requirement` the buying-power check actually reads.
// ---------------------------------------------------------------------------------------

/// The conversion itself, across the whole plausible range, on all three converters — the
/// point of the change: `max_leverage = N` must arm the buying-power check at `1/N`, not sit
/// inert the way `RiskLimits::max_leverage` always did.
#[test]
fn max_leverage_converts_to_the_enforced_im_requirement() {
    for (lev, im) in [(1.0, 1.0), (2.0, 0.5), (4.0, 0.25), (10.0, 0.1), (100.0, 0.01)] {
        let p = ProfileRisk { max_leverage: Some(lev), ..ProfileRisk::default() };
        assert_eq!(p.im_requirement(), Some(im), "{lev}x ⇒ im {im}");
        assert_eq!(p.to_risk_limits().im_requirement, Some(im));
        assert_eq!(
            p.apply_to(RiskLimits::new(), GridSource::NoGridFetched).unwrap().im_requirement,
            Some(im)
        );
        assert_eq!(p.apply_operator_budget_only(RiskLimits::new()).im_requirement, Some(im));
    }
}

/// An UNSET `max_leverage` must leave the buying-power lane disarmed — as an unset
/// `im_requirement` always did, and the precondition for `vike-mount`'s
/// conservative 1× rescue (`.or(Some(1.0))`) still firing on a profile that never mentions
/// leverage.
#[test]
fn absent_max_leverage_leaves_the_buying_power_check_disarmed() {
    let p = ProfileRisk::default();
    assert_eq!(p.im_requirement(), None);
    assert_eq!(p.to_risk_limits().im_requirement, None);
    assert_eq!(p.apply_operator_budget_only(RiskLimits::new()).im_requirement, None);
}

/// The guard. A leverage below 1.0 (or non-finite) is a config error `apply_to` REJECTS —
/// `0.0` would divide by zero and a negative would produce a NEGATIVE margin requirement, i.e.
/// unbounded buying power, which is the one direction this must never fail in.
#[test]
fn apply_to_rejects_a_max_leverage_below_one() {
    for lev in [0.0, 0.5, -2.0, f64::NAN, f64::INFINITY] {
        let p = ProfileRisk { max_leverage: Some(lev), ..ProfileRisk::default() };
        let err = p
            .apply_to(venue_fetched_base(), GridSource::VenueFetched)
            .expect_err(&format!("max_leverage = {lev} must be Err"));
        match err {
            ProfileError::Validation(m) => {
                assert!(m.contains("risk.max_leverage"), "error must name the key: {m}")
            }
            other => panic!("expected Validation error for {lev}, got {other:?}"),
        }
    }
}

/// The infallible converters cannot return an error, so they must degrade CONSERVATIVELY on
/// the same nonsense input: 1× (`im 1.0`), never `None` (gate silently off) and never a
/// negative fraction (unbounded buying power).
#[test]
fn infallible_converters_degrade_a_bad_max_leverage_to_one_x() {
    for lev in [0.0, 0.5, -2.0, f64::NAN, f64::INFINITY] {
        let p = ProfileRisk { max_leverage: Some(lev), ..ProfileRisk::default() };
        assert_eq!(p.im_requirement(), Some(1.0), "{lev} must degrade to 1x, not disarm");
        assert_eq!(p.to_risk_limits().im_requirement, Some(1.0));
        assert_eq!(p.apply_operator_budget_only(RiskLimits::new()).im_requirement, Some(1.0));
    }
}

// NOTE: the twin property — `[risk] im_requirement` is now a LOUD `deny_unknown_fields` parse
// error rather than a silently-ignored key — is pinned where the TOML actually gets parsed,
// in `vike_core::run_profile`'s tests (`retired_im_requirement_key_is_rejected_by_name`).
// This crate has no `toml` dependency and should not grow one for a single test.

// ---------------------------------------------------------------------------------------
// `apply_operator_budget_only` — the fallback merge `vike-mount`
// uses when `apply_to` rejects a profile, so an illegal venue-owned field never costs the
// operator their ENTIRE risk budget.
// ---------------------------------------------------------------------------------------

/// The operator-owned fields must ALL still arm even though this profile illegally also sets
/// a venue-owned field (the exact shape that makes `apply_to` return `Err` under
/// `VenueFetched`) — `apply_operator_budget_only` is the fallback that must never leave the
/// budget at "zero caps".
#[test]
fn apply_operator_budget_only_arms_the_budget_despite_an_illegal_venue_field() {
    let profile = ProfileRisk {
        tick_size: Some(999.0), // illegal under VenueFetched — must be dropped, not honored
        max_notional_per_order: Some(100.0),
        max_total_exposure: Some(500.0),
        max_orders_per_window: Some(5),
        window_ms: 2000,
        max_leverage: Some(5.0),
        required_free_bp_pct: 0.1,
        ..ProfileRisk::default()
    };
    let base = venue_fetched_base();
    // Confirm this profile really would be rejected by `apply_to` first (the scenario this
    // fallback exists for).
    assert!(profile.apply_to(base.clone(), GridSource::VenueFetched).is_err());

    let got = profile.apply_operator_budget_only(base.clone());
    assert_eq!(got.tick_size, base.tick_size, "venue field must be base's, not the profile's");
    assert_eq!(got.max_notional_per_order, Some(100.0));
    assert_eq!(got.max_total_exposure, Some(500.0));
    assert_eq!(got.max_orders_per_window, Some(5));
    assert_eq!(got.window_ms, 2000);
    assert_eq!(got.max_leverage, Some(5.0));
    assert_eq!(got.im_requirement, Some(0.2), "5x leverage ⇒ 20% initial margin");
    assert_eq!(got.required_free_bp_pct, 0.1);
}

/// A profile with an invalid `window_ms` (the serde default 0) that also arms
/// `max_orders_per_window` falls back to `base`'s window rather than zeroing it — and since
/// `base` here has a REAL positive window (`venue_fetched_base`'s inherited 1000, same as any
/// `RiskLimits::new()`/`from_properties` base ever is), arming with that inherited window is
/// safe: it is exactly the same window `apply_to` would have used had the profile simply not
/// mentioned `window_ms` at all.
#[test]
fn apply_operator_budget_only_falls_back_to_bases_window_when_profiles_own_is_invalid() {
    let profile = ProfileRisk {
        tick_size: Some(999.0),
        max_orders_per_window: Some(5),
        ..ProfileRisk::default() // window_ms stays the serde default (0)
    };
    let base = venue_fetched_base();
    let got = profile.apply_operator_budget_only(base.clone());
    assert_eq!(got.window_ms, base.window_ms, "must inherit base's real window, never 0");
    assert_eq!(
        got.max_orders_per_window,
        Some(5),
        "arming with the inherited (valid, positive) window is safe"
    );
}

/// The degenerate case the guard above exists for: if `base` ITSELF carries a non-positive
/// window (never true of a real `RiskLimits::new()`/`from_properties` base, but not something
/// this infallible fallback can rule out), falling back to it would reproduce the zero-window
/// hazard (armed count, zero-width window, cutoff == now). The fallback must
/// disarm the throttle instead of silently merging a zero window.
#[test]
fn apply_operator_budget_only_disarms_throttle_if_even_bases_window_is_non_positive() {
    let profile = ProfileRisk {
        tick_size: Some(999.0),
        max_orders_per_window: Some(5),
        ..ProfileRisk::default() // window_ms stays the serde default (0)
    };
    let base = RiskLimits { tick_size: Some(0.5), window_ms: 0, ..RiskLimits::default() };
    let got = profile.apply_operator_budget_only(base);
    assert_eq!(got.window_ms, 0);
    assert_eq!(
        got.max_orders_per_window, None,
        "an inconsistent throttle request must disarm, not silently merge a zero window"
    );
}
