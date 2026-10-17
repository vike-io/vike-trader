use super::*;

#[test]
fn the_two_switches_land_on_their_keys_as_booleans() {
    let e = explain_data_override();
    assert_eq!(e.key, "data.explain");
    assert_eq!(e.value, toml::Value::Boolean(true));
    let r = require_coverage_override();
    assert_eq!(r.key, "data.require_coverage");
    assert_eq!(r.value, toml::Value::Boolean(true));
}

/// A mixed-case value from a runbook is the same choice on both routes.
#[test]
fn on_gap_and_universe_are_case_insensitive_and_normalise_to_lowercase() {
    assert_eq!(
        on_gap_override("Warn").expect("warn is valid").value,
        toml::Value::String("warn".to_string())
    );
    assert_eq!(
        universe_override("  STRICT ").expect("strict is valid").value,
        toml::Value::String("strict".to_string())
    );
}

#[test]
fn a_misspelled_disposition_is_a_local_usage_error_naming_the_set() {
    let e = on_gap_override("refues").expect_err("a typo must not reach the far side");
    assert!(e.contains("refuse | warn | run"), "the valid set is named: {e}");
    let u = universe_override("survivors").expect_err("a typo must not reach the far side");
    assert!(u.contains("declared | covered | strict"), "the valid set is named: {u}");
    assert!(u.contains("drops a member"), "the message corrects the expectation: {u}");
}

/// The span GRAMMAR belongs to the far side; a BLANK belongs here, because it is a typo of
/// nothing and its far-side refusal would name a grammar nobody wrote.
#[test]
fn max_gap_passes_a_span_through_verbatim_and_refuses_only_a_blank() {
    assert_eq!(
        max_gap_override("1d").expect("a span is passed through").value,
        toml::Value::String("1d".to_string())
    );
    assert_eq!(
        max_gap_override(" 4h ").expect("trimmed, not parsed").value,
        toml::Value::String("4h".to_string())
    );
    // A bar count and a calendar month are REFUSED — by `DataCfg::max_gap_ms`, not here, so
    // this side must let them through rather than growing a second grammar.
    assert!(max_gap_override("200bars").is_ok(), "the grammar is the far side's to refuse");
    assert!(max_gap_override("   ").is_err());
}

#[test]
fn every_key_is_under_the_data_table() {
    for key in [KEY_EXPLAIN, KEY_REQUIRE_COVERAGE, KEY_MAX_GAP, KEY_ON_GAP, KEY_UNIVERSE] {
        assert!(key.starts_with("data."), "{key} is not a [data] key");
    }
}
