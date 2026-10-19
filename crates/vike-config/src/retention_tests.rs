use super::*;

/// The refusal for `value`, which must be one.
fn refusal(value: &str) -> String {
    parse_retention_days_by_kind(value).expect_err(value)
}

#[test]
fn the_documented_example_parses_in_the_order_written() {
    assert_eq!(
        parse_retention_days_by_kind("book:14,depth:14,trade:90").unwrap(),
        vec![("book", 14), ("depth", 14), ("trade", 90)]
    );
}

#[test]
fn the_empty_row_is_todays_behaviour() {
    assert_eq!(parse_retention_days_by_kind("").unwrap(), vec![]);
}

#[test]
fn one_entry_and_every_accepted_kind_parse() {
    assert_eq!(parse_retention_days_by_kind("quote:1").unwrap(), vec![("quote", 1)]);
    let all = RETENTION_KINDS.map(|k| format!("{k}:7")).join(",");
    let parsed = parse_retention_days_by_kind(&all).unwrap();
    assert_eq!(parsed.len(), RETENTION_KINDS.len());
    assert!(parsed.iter().all(|(_, days)| *days == 7));
}

#[test]
fn the_most_days_a_count_holds_is_taken() {
    assert_eq!(
        parse_retention_days_by_kind(&format!("bar:{}", u32::MAX)).unwrap(),
        vec![("bar", u32::MAX)]
    );
}

#[test]
fn every_execution_kind_is_refused_by_name_with_its_reason() {
    for kind in RETENTION_REFUSED_KINDS {
        let msg = refusal(&format!("{kind}:30"));
        assert!(msg.contains(kind), "{kind}: it names the kind: {msg}");
        assert!(msg.contains("execution history"), "{kind}: it says why: {msg}");
        assert!(msg.contains("Leave it out"), "{kind}: it says what to do: {msg}");
    }
    // Refused wherever it stands in the row, not only first.
    assert!(refusal("trade:90,exec_fill:30").contains("exec_fill"));
}

#[test]
fn an_unknown_kind_is_refused_and_the_accepted_kinds_are_listed() {
    let msg = refusal("bok:14");
    assert!(msg.contains("`bok`"), "{msg}");
    assert!(msg.contains("not a store kind"), "{msg}");
    for kind in RETENTION_KINDS {
        assert!(msg.contains(kind), "{kind} is listed: {msg}");
    }
    assert!(!msg.contains("lower case"), "no case hint for a word that is just wrong: {msg}");
}

#[test]
fn a_kind_word_in_the_wrong_case_is_refused_with_the_case_named() {
    for bad in ["Book:14", "BOOK:14", "Trade:1"] {
        let msg = refusal(bad);
        assert!(msg.contains("lower case"), "{bad}: {msg}");
    }
}

#[test]
fn zero_days_is_refused_naming_the_kind_and_the_consequence() {
    let msg = refusal("book:14,trade:0");
    assert!(msg.contains("`trade:0`"), "{msg}");
    assert!(msg.contains("older than NOW"), "{msg}");
    assert!(msg.contains("1 day"), "{msg}");
}

#[test]
fn a_days_value_that_is_not_a_plain_count_is_refused() {
    for bad in ["book:", "book:-3", "book:+3", "book:1.5", "book:1e3", "book:abc", "book:0x10"] {
        let msg = refusal(bad);
        assert!(msg.contains("whole number written in digits"), "{bad}: {msg}");
        assert!(msg.contains("`book`"), "{bad}: it names the kind: {msg}");
    }
}

#[test]
fn a_days_value_past_u32_is_refused_not_wrapped() {
    let msg = refusal("book:4294967296");
    assert!(msg.contains("do not fit"), "{msg}");
    assert!(msg.contains(&u32::MAX.to_string()), "{msg}");
}

#[test]
fn a_kind_named_twice_is_refused_even_with_the_same_days() {
    for dup in ["book:14,book:14", "book:14,trade:90,book:30"] {
        let msg = refusal(dup);
        assert!(msg.contains("`book` is named twice"), "{dup}: {msg}");
    }
}

#[test]
fn a_malformed_entry_is_refused_with_the_entry_quoted() {
    for bad in ["book", "book14", "book;14"] {
        let msg = refusal(bad);
        assert!(msg.contains("not `kind:days`"), "{bad}: {msg}");
        assert!(msg.contains(bad), "{bad}: the entry is quoted: {msg}");
    }
    // A second colon leaves a days part that is not digits.
    assert!(refusal("book:14:3").contains("digits"));
}

#[test]
fn empty_entries_and_whitespace_are_refused_not_repaired() {
    for bad in [",", "book:14,", ",book:14", "book:14,,trade:90"] {
        assert!(refusal(bad).contains("empty entry"), "{bad:?}");
    }
    for bad in [" ", "book:14, trade:90", " book:14", "book:14 ", "book :14", "book:\t14", "\n"] {
        assert!(refusal(bad).contains("whitespace"), "{bad:?}");
    }
}

#[test]
fn a_long_offender_is_cut_in_the_refusal() {
    let long = "x".repeat(200);
    let msg = refusal(&long);
    assert!(msg.contains('…'), "{msg}");
    assert!(!msg.contains(&long), "the whole value must not be printed: {msg}");
}

#[test]
fn the_kind_lists_are_sorted_disjoint_and_the_minimum_is_one() {
    assert!(RETENTION_KINDS.is_sorted(), "alphabetical, like StoreKindId");
    assert!(RETENTION_REFUSED_KINDS.is_sorted());
    for kind in RETENTION_REFUSED_KINDS {
        assert!(!RETENTION_KINDS.contains(&kind), "{kind} is on both lists");
        assert!(kind.starts_with("exec_"), "{kind}: the refused set is the execution kinds");
    }
    assert!(RETENTION_KINDS.iter().all(|k| !k.starts_with("exec_")));
    assert_eq!(MIN_RETENTION_DAYS, 1);
}
