//! **No two of the shim's named return codes share a value.** The Rust side turns a code into words (`sys.rs`'s
//! `placement_failure`, `subscription.rs`'s verdicts, `event_mapper::outcome_is_unknown`), so a second meaning for a number
//! some other export already returns reads as the WRONG refusal, silently: two branches of 2026-10-26 each took -16 for a
//! different permission (`server/fxcm-native-stops`'s exit and `server/fxcm-exotic-pairs`' order creation) and only a merge
//! rehearsal saw it. Gated on a box with no ForexConnect SDK like `fxcm_shim_close.rs`: the source is what is read.
//!
//! The rule it pins: every code below -10 (the `-11`..`-NN` family: a sent request with no usable answer, an ambiguous lookup,
//! a refusal that never reached the venue) is a `#define FC_...` in `fcshim.cpp`, one value per name, and a bare
//! `return -NN;` of that family is never written, because only a `#define` is visible to this test.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// `fcshim.cpp` with `//` comments removed (its prose names codes it does not define).
fn shim_code() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/shim/fcshim.cpp");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the shim source {}: {e}", path.display()));
    src.lines()
        .map(|l| match l.find("//") {
            Some(at) if !l[..at].contains('"') => &l[..at],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `#define FC_<NAME> (-<N>)` of the shim, in file order.
fn defined_codes() -> Vec<(String, i32)> {
    shim_code()
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("#define ")?;
            let (name, value) = rest.split_once(char::is_whitespace)?;
            let value = value.trim().strip_prefix("(-")?.strip_suffix(')')?;
            Some((name.to_string(), -value.trim().parse::<i32>().ok()?))
        })
        .collect()
}

#[test]
fn no_two_named_codes_share_a_value_and_no_name_is_defined_twice() {
    let codes = defined_codes();
    let mut by_value: BTreeMap<i32, Vec<&str>> = BTreeMap::new();
    let mut by_name: BTreeMap<&str, Vec<i32>> = BTreeMap::new();
    for (name, value) in &codes {
        by_value.entry(*value).or_default().push(name);
        by_name.entry(name).or_default().push(*value);
    }
    for (value, names) in &by_value {
        assert_eq!(
            names.len(),
            1,
            "the code {value} has {} meanings: {names:?}. The Rust side maps a code to words, so one of them would read as the \
             other's refusal; take the next free value",
            names.len()
        );
    }
    for (name, values) in &by_name {
        assert_eq!(values.len(), 1, "{name} is defined {} times: {values:?}", values.len());
    }
}

/// A parse that finds nothing would pass the test above for ever.
#[test]
fn the_codes_this_test_reads_are_the_ones_the_shim_defines() {
    let codes: BTreeMap<String, i32> = defined_codes().into_iter().collect();
    for (name, value) in [
        ("FC_SENT_NO_ANSWER", -11),
        ("FC_SENT_NO_ORDER_ID", -12),
        ("FC_LOOKUP_AMBIGUOUS", -13),
        ("FC_NOT_REGULAR_FOREX", -14),
        ("FC_CLOSE_NOT_PERMITTED", -15),
        ("FC_EXIT_NOT_PERMITTED", -17),
    ] {
        assert_eq!(codes.get(name), Some(&value), "`{name}` is not defined as {value}: {codes:?}");
    }
}

/// `return -16;` would be invisible to the test above. Below -10 a code is named.
#[test]
fn no_code_below_minus_ten_is_returned_as_a_bare_number() {
    let code = shim_code();
    let mut rest = code.as_str();
    while let Some(at) = rest.find("return -") {
        let tail = &rest[at + "return -".len()..];
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse::<i32>() {
            assert!(
                n <= 10,
                "`return -{n};` is a bare code below -10: give it a `#define FC_...` name"
            );
        }
        rest = tail;
    }
}
