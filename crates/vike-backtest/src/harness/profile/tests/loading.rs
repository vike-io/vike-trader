//! The loader doors: the parsed text handed back, and `validate_all` accumulating diagnostics.

use super::*;
use std::assert_matches;

/// The text a profile was parsed FROM comes back with it, so the run record can store the bytes
/// that actually drove the run rather than re-reading a file that may have changed since.
#[test]
fn loading_a_profile_hands_back_the_exact_text_it_parsed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sma.toml");
    std::fs::write(&path, BAR_TOML).unwrap();

    let (profile, text) = BacktestProfile::from_path_with_text(&path).unwrap();

    assert_eq!(text, BAR_TOML, "byte for byte, comments and blank lines included");
    assert_eq!(profile.data.interval, "1h");
    assert!(profile.base_dir.is_some(), "and it still records the sidecar base directory");
}

/// The old door keeps working and keeps meaning the same thing — it has callers this plan does
/// not touch, and it must not grow a second parse.
#[test]
fn the_path_loader_still_answers_and_delegates_to_the_same_parse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sma.toml");
    std::fs::write(&path, BAR_TOML).unwrap();

    let only = BacktestProfile::from_path(&path).unwrap();
    let (both, _) = BacktestProfile::from_path_with_text(&path).unwrap();

    assert_eq!(only.data.interval, both.data.interval);
    assert_eq!(only.base_dir, both.base_dir);
}

/// A profile with mistakes in four `[engine]` keys and one in `[data]`, kept deliberately
/// PARSEABLE so every one of them is semantic: this is what the accumulating door exists for,
/// and the exact KEY SEQUENCE is asserted rather than a count, because the sequence is the
/// contract `BacktestProfile::validate` reads element zero of.
const FIVE_MISTAKES_TOML: &str = r#"
name = "five-mistakes"

[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2026-01-02T00"
to = "2026-01-01T00"

[engine]
cash = 0.0
feed_latency = true
maint_margin = -1.0
multiplier = 0.0

[strategy]
name = "sma_cross"
"#;

/// ⚠ **Five mistakes, ONE pass** — the whole point of the second door. Before it existed an
/// operator learned about `engine.cash`, fixed it, learned about `engine.feed_latency`, fixed
/// that, and paid five edit-and-rerun cycles for one file; on the remote route each of those
/// was also a dial to a compute daemon.
#[test]
fn validate_all_reports_every_mistake_in_one_pass() {
    let p: BacktestProfile =
        toml::from_str(FIVE_MISTAKES_TOML).expect("it PARSES — every mistake here is semantic");
    let all = p.validate_all();
    let keys: Vec<&str> = all.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "engine.cash",
            "engine.feed_latency",
            "engine.maint_margin",
            "engine.multiplier",
            "data.from",
        ],
        "every rule that refused, in the order the rules run: {all:?}"
    );
    assert!(
        all.iter().all(|d| d.severity == vike_model::Severity::Error),
        "a load-time refusal is all this validator has to say, so every row is an Error: \
             {all:?}"
    );
    assert!(
        all[3].message.contains("scales every position's notional"),
        "and each row carries the validator's OWN sentence, not a summary of it: {:?}",
        all[3].message
    );
}

/// ⚠ **The first diagnostic is the error the first-error door returns.** The two doors are one
/// walk, so they cannot disagree about which mistake governs — and if a future edit reorders a
/// rule, this is what goes red rather than the published surface (which sorts its refusal rows
/// by message text and would notice nothing).
#[test]
fn the_first_diagnostic_is_what_validate_itself_returns() {
    let p: BacktestProfile = toml::from_str(FIVE_MISTAKES_TOML).expect("it parses");
    let err = p.validate().expect_err("five mistakes is not zero mistakes");
    let all = p.validate_all();
    assert_eq!(all[0].key, "engine.cash");
    assert_eq!(all[0].message, err.message(), "same walk, same governing refusal");
}

/// ⚠ **The accumulator carries the real [`HarnessError`], which is why the VARIANT survives.**
/// An unparseable stamp is a [`HarnessError::Parse`] — `rejects_unparsable_timestamp` asserts
/// exactly that — and rebuilding an error from a `Diagnostic`'s text would have quietly turned
/// it into a `Validation`, changing what every existing caller sees for a profile nobody
/// edited.
#[test]
fn the_accumulating_door_does_not_change_the_error_variant() {
    let toml = BAR_TOML.replace("from = \"2026-01-01T00\"", "from = \"not-a-timestamp\"");
    let p: BacktestProfile = toml::from_str(&toml).expect("it parses; the stamp is semantic");
    let err = p.validate().expect_err("an unparseable stamp is refused");
    assert_matches!(err, HarnessError::Parse(_), "got {err:?}");
    let all = p.validate_all();
    assert_eq!(all.len(), 1, "one mistake, one diagnostic: {all:?}");
    assert_eq!(all[0].key, "data", "anchored on the table, since the range spans two keys");
    assert_eq!(all[0].message, err.message());
}

/// A profile that loads collects nothing — the emptiness is the success signal, so a caller
/// can drive the accumulating door alone and never ask the other one.
#[test]
fn a_profile_that_loads_collects_no_diagnostics() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("the fixture loads");
    assert!(p.validate_all().is_empty(), "{:?}", p.validate_all());
}

/// ⚠ A refused `[data]` slice SKIPS the `engine.multipliers` key check rather than answering
/// it from wreckage: that rule names the loaded symbols back, and a "the data slice is: "
/// listing built from a malformed slice would be a second, wrong refusal chasing the first.
/// One diagnostic, not two.
#[test]
fn a_refused_slice_does_not_produce_a_second_refusal_about_the_symbols_it_never_resolved() {
    let toml = BAR_TOML
        .replace("\"BTCUSDT\", \"ETHUSDT\"", "\"BTCUSDT\", \"BTCUSDT\"")
        .replace("fee_rate = 0.001", "fee_rate = 0.001\nmultipliers = { NOPE = 50.0 }");
    let p: BacktestProfile = toml::from_str(&toml).expect("it parses; the duplicate is semantic");
    let all = p.validate_all();
    assert_eq!(all.len(), 1, "the duplicate symbol alone: {all:?}");
    assert_eq!(all[0].key, "data");
    assert!(all[0].message.contains("duplicate symbol"), "{:?}", all[0].message);
}
