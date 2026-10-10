use super::*;

fn cfg(default_utilization: f64, overrides: &[(&str, f64)]) -> RateLimitConfig {
    RateLimitConfig {
        default_utilization,
        per_venue: overrides.iter().map(|(v, u)| ((*v).to_string(), *u)).collect(),
    }
}

/// The measured optimum is the default, and it is what an unconfigured venue resolves to — this
/// is the "nothing needs configuring" contract, so the number is pinned bit-for-bit.
#[test]
fn default_is_forty_percent() {
    let c = RateLimitConfig::default();
    assert_eq!(c.default_utilization.to_bits(), 0.40_f64.to_bits());
    assert!(c.per_venue.is_empty());
    assert_eq!(c.utilization_for("binance").to_bits(), 0.40_f64.to_bits());
    // An unknown venue string still resolves (the pacer asks by name; it never panics).
    assert_eq!(c.utilization_for("not-a-venue").to_bits(), 0.40_f64.to_bits());
    c.validate().expect("the shipped default must validate");
}

#[test]
fn a_per_venue_override_wins_over_the_default() {
    let c = cfg(0.40, &[("binance", 0.75)]);
    assert_eq!(c.utilization_for("binance").to_bits(), 0.75_f64.to_bits());
    // …and only for the venue it names.
    assert_eq!(c.utilization_for("bybit").to_bits(), 0.40_f64.to_bits());
    c.validate().expect("a roster venue with an in-range value is valid");
}

/// The GUI-slider guarantee: nothing an operator can send produces a pacer that never progresses
/// (`0`) or one that outspends the venue's budget (`> 1`). Both ends, both levels.
#[test]
fn utilization_is_clamped_at_both_ends() {
    let c = cfg(5.0, &[("binance", 0.0), ("bybit", 3.0), ("okx", -1.0)]);
    assert_eq!(c.utilization_for("binance"), MIN_UTILIZATION);
    assert_eq!(c.utilization_for("okx"), MIN_UTILIZATION);
    assert_eq!(c.utilization_for("bybit"), MAX_UTILIZATION);
    // The DEFAULT is clamped too — an out-of-range default must not leak through an
    // un-overridden venue.
    assert_eq!(c.utilization_for("deribit"), MAX_UTILIZATION);
    // Infinities carry a direction, so they clamp rather than falling back.
    let inf = cfg(0.40, &[("binance", f64::INFINITY), ("bybit", f64::NEG_INFINITY)]);
    assert_eq!(inf.utilization_for("binance"), MAX_UTILIZATION);
    assert_eq!(inf.utilization_for("bybit"), MIN_UTILIZATION);
    // Exactly the bounds survive untouched.
    let edge = cfg(0.40, &[("binance", MIN_UTILIZATION), ("bybit", MAX_UTILIZATION)]);
    assert_eq!(edge.utilization_for("binance"), MIN_UTILIZATION);
    assert_eq!(edge.utilization_for("bybit"), MAX_UTILIZATION);
}

/// NaN carries no intent, so it FALLS BACK rather than clamping to an arbitrary end — at both
/// levels, and the resolution chain terminates in the compiled-in constant.
#[test]
fn nan_falls_back_to_the_default() {
    let c = cfg(0.30, &[("binance", f64::NAN)]);
    assert_eq!(c.utilization_for("binance").to_bits(), 0.30_f64.to_bits());
    // A NaN default too: the chain ends at DEFAULT_UTILIZATION, never at NaN.
    let both = cfg(f64::NAN, &[("binance", f64::NAN)]);
    assert_eq!(both.utilization_for("binance").to_bits(), DEFAULT_UTILIZATION.to_bits());
    assert_eq!(both.utilization_for("bybit").to_bits(), DEFAULT_UTILIZATION.to_bits());
    // Whatever the config, the resolved value is a usable fraction.
    for c in [&c, &both] {
        for &v in crate::VENUES {
            let u = c.utilization_for(v);
            assert!(in_range(u), "{v}: {u} escaped the accepted range");
        }
    }
}

/// The typo case — an unknown key changes NOTHING at runtime, so it must be an error the
/// operator can read, naming the offending key.
#[test]
fn an_unknown_venue_key_fails_validate_by_name() {
    let err = cfg(0.40, &[("binanace", 0.5)]).validate().unwrap_err();
    assert!(err.contains("binanace"), "the offending key must be named: {err}");
    // Case matters: ids are canonical lowercase (see `crate::venues`), so a mis-cased key is a
    // typo, not a near-miss that silently applies.
    let err = cfg(0.40, &[("Binance", 0.5)]).validate().unwrap_err();
    assert!(err.contains("Binance"), "{err}");
    assert_eq!(cfg(0.40, &[("Binance", 0.5)]).utilization_for("binance"), 0.40);
}

/// Every offending entry is reported in ONE pass — one edit fixes the whole file.
#[test]
fn validate_reports_every_offender_at_once() {
    let c = cfg(2.0, &[("binance", 9.0), ("nope", 0.5), ("bybit", 0.5)]);
    let err = c.validate().unwrap_err();
    assert!(err.contains("default_utilization"), "{err}");
    assert!(err.contains("binance"), "{err}");
    assert!(err.contains("nope"), "{err}");
    assert!(!err.contains("bybit"), "a valid row must not be reported: {err}");
    // NaN is not a valid setting even though `utilization_for` can survive it.
    assert!(cfg(f64::NAN, &[]).validate().is_err());
    assert!(cfg(0.40, &[("binance", f64::NAN)]).validate().is_err());
    // The inclusive bounds are legal settings.
    cfg(MIN_UTILIZATION, &[("binance", MAX_UTILIZATION)]).validate().expect("bounds are legal");
    // Every roster venue is an accepted key.
    let all: Vec<(&str, f64)> = crate::VENUES.iter().map(|v| (*v, 0.5)).collect();
    cfg(0.40, &all).validate().expect("every roster venue is a valid key");
}

/// Round-trips through the TOML-shaped JSON the workspace config layer uses (serde_json is the
/// in-tree serde witness; the field/table shape is what a `[rate_limits]` TOML table produces).
#[test]
fn serde_round_trip() {
    let c = cfg(0.25, &[("binance", 0.6), ("okx", 0.1)]);
    let json = serde_json::to_string(&c).expect("serialize");
    let back: RateLimitConfig = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.default_utilization.to_bits(), 0.25_f64.to_bits());
    assert_eq!(back.per_venue.len(), 2);
    assert_eq!(back.utilization_for("binance").to_bits(), 0.6_f64.to_bits());
    assert_eq!(back.utilization_for("okx").to_bits(), 0.1_f64.to_bits());
    // Insertion order survives the round trip (the IndexMap contract this module relies on for
    // deterministic error text).
    let keys: Vec<&str> = back.per_venue.keys().map(String::as_str).collect();
    assert_eq!(keys, ["binance", "okx"]);
}

/// The `#[serde(default)]` contract: an ABSENT table, an empty one, and a partial one are all
/// valid config, and each falls back to the measured optimum for whatever it did not say.
#[test]
fn absent_fields_fall_back_to_defaults() {
    let empty: RateLimitConfig = serde_json::from_str("{}").expect("an empty table is valid");
    assert_eq!(empty.default_utilization.to_bits(), DEFAULT_UTILIZATION.to_bits());
    assert!(empty.per_venue.is_empty());

    let only_default: RateLimitConfig =
        serde_json::from_str(r#"{"default_utilization":0.8}"#).expect("partial table");
    assert_eq!(only_default.default_utilization.to_bits(), 0.8_f64.to_bits());
    assert!(only_default.per_venue.is_empty());

    let only_overrides: RateLimitConfig =
        serde_json::from_str(r#"{"per_venue":{"bybit":0.9}}"#).expect("partial table");
    assert_eq!(only_overrides.default_utilization.to_bits(), DEFAULT_UTILIZATION.to_bits());
    assert_eq!(only_overrides.utilization_for("bybit").to_bits(), 0.9_f64.to_bits());
    assert_eq!(only_overrides.utilization_for("binance").to_bits(), DEFAULT_UTILIZATION.to_bits());
    only_overrides.validate().expect("a partial table is valid config");
}

// ----- the measured-pace half ---------------------------------------------------------------
//
// `PaceSample` is all that is left of it: the `PaceBook`/`MeasuredPace` persistence these tests
// used to drive went with the one-shot kline programs (docs/decisions/0094). What the sample itself
// promises — a usable shape, a unit conversion, an all-zero default that can never seed a pacer —
// is still asserted here, directly rather than through a book.

/// A realistic sample: the MEASURED binance fapi shape (2400/min budget, weight-5 `/klines`,
/// ~280 ms round trip from the CI box).
fn sample() -> PaceSample {
    PaceSample { request_ms: 280, per_request_weight: 5.0, budget_per_min: Some(2400), samples: 1 }
}

/// The good shape is usable, and the pacer's unit conversion is exact on it.
#[test]
fn a_measured_sample_is_usable_and_converts_to_seconds() {
    let good = sample();
    assert!(good.is_usable());
    assert_eq!(good.request_secs().to_bits(), 0.280_f64.to_bits());
}

/// A sample that measured nothing — or measured something that would poison a pace — is not
/// usable: a zero, negative or non-finite weight (a zero divisor is a zero-delay hammer, a NaN
/// panics inside `Duration::from_secs_f64`), a zero `request_ms` (nothing was timed) or zero
/// `samples` (nothing was observed).
#[test]
fn an_unusable_sample_is_refused_by_shape() {
    let good = sample();
    for bad in [
        PaceSample { samples: 0, ..good },    // nothing observed
        PaceSample { request_ms: 0, ..good }, // nothing timed
        PaceSample { per_request_weight: 0.0, ..good },
        PaceSample { per_request_weight: -1.0, ..good },
        PaceSample { per_request_weight: f64::NAN, ..good },
        PaceSample { per_request_weight: f64::INFINITY, ..good },
    ] {
        assert!(!bad.is_usable(), "{bad:?} must not be usable");
    }
    assert!(good.is_usable());
}

/// The `#[serde(default)]` contract: a bare sample defaults to the unusable all-zero shape rather
/// than a plausible-looking one, so a sample that says nothing can never seed a pacer.
#[test]
fn an_empty_sample_defaults_to_the_unusable_all_zero_shape() {
    let s: PaceSample = serde_json::from_str("{}").expect("an empty sample is valid");
    assert_eq!(s, PaceSample::default());
    assert!(!s.is_usable());
}
