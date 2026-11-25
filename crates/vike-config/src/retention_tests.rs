use super::*;

use vike_model::StoreKindId::{Bar, Book, Depth, Quote, Trade};

/// The refusal for `value`, which must be one.
fn refusal(value: &str) -> String {
    parse_retention_days_by_kind(value).expect_err(value)
}

/// One parsed entry, spelled short.
fn entry(kind: StoreKindId, lane: Option<&'static str>, days: u32) -> RetentionEntry {
    RetentionEntry { kind, lane, days }
}

#[test]
fn the_documented_example_parses_in_the_order_written() {
    assert_eq!(
        parse_retention_days_by_kind("book:14,depth:14,trade:90").unwrap(),
        vec![entry(Book, None, 14), entry(Depth, None, 14), entry(Trade, None, 90)]
    );
}

#[test]
fn the_empty_row_is_todays_behaviour() {
    assert_eq!(parse_retention_days_by_kind("").unwrap(), vec![]);
}

#[test]
fn one_entry_and_every_market_kind_parse() {
    assert_eq!(parse_retention_days_by_kind("quote:1").unwrap(), vec![entry(Quote, None, 1)]);
    let all = retainable().map(|k| format!("{}:7", k.word())).collect::<Vec<_>>().join(",");
    let parsed = parse_retention_days_by_kind(&all).unwrap();
    assert_eq!(parsed, retainable().map(|k| entry(k, None, 7)).collect::<Vec<_>>());
}

/// A lane's word stands beside its kind's, each once, under the kind's rules. Only a word the table holds is
/// taken: a lane nobody declared, and a lane of an account kind, are unknown words.
#[test]
fn a_lane_word_is_its_own_entry_under_the_same_rules() {
    assert_eq!(
        parse_retention_days_by_kind("quote:365,quote@recorder:30").unwrap(),
        vec![entry(Quote, None, 365), entry(Quote, Some("recorder"), 30)]
    );
    assert!(
        refusal("quote@recorder:1,quote@recorder:2").contains("`quote@recorder` is named twice")
    );
    assert!(refusal("quote@recorder:0").contains("older than NOW"));
    // The recorder's closed live bars (`flags.record_bars`) take a word of their own the same way.
    assert_eq!(
        parse_retention_days_by_kind("bar@recorder:30,quote@recorder:30").unwrap(),
        vec![entry(Bar, Some("recorder"), 30), entry(Quote, Some("recorder"), 30)]
    );
    assert!(refusal("bar@recorder:0").contains("older than NOW"));
    for unknown in ["quote@nobody:1", "exec_fill@recorder:1", "quote@:1"] {
        assert!(refusal(unknown).contains("not a store kind"), "{unknown}");
    }
    assert!(refusal("Quote@Recorder:1").contains("lower case"));
}

/// Every lane word's kind half is a market kind: a lane of an account kind would be a way round the
/// refusal. (That the lane half is one the kind's producers declare is the data daemon's test.)
#[test]
fn every_lane_word_names_a_market_kind() {
    for word in RETENTION_LANES {
        let (kind, lane) = word.split_once('@').expect("kind@lane");
        let kind = StoreKindId::from_word(kind).unwrap_or_else(|| panic!("{word}: no store kind"));
        assert_eq!(kind.plane(), StorePlane::Market, "{word}");
        assert!(!lane.is_empty(), "{word}");
    }
}

#[test]
fn the_most_days_a_count_holds_is_taken() {
    assert_eq!(
        parse_retention_days_by_kind(&format!("bar:{}", u32::MAX)).unwrap(),
        vec![entry(Bar, None, u32::MAX)]
    );
}

/// Every ACCOUNT kind is refused by name with its reason, `equity` included (the owner's ruling of
/// 2026-11-21), and every MARKET kind is taken: the class decides, no list does.
#[test]
fn every_account_kind_is_refused_by_name_with_its_reason() {
    let account: Vec<StoreKindId> =
        StoreKindId::ALL.into_iter().filter(|k| k.plane() == StorePlane::Account).collect();
    assert!(account.contains(&StoreKindId::Equity), "equity is account data");
    for kind in &account {
        let word = kind.word();
        let msg = refusal(&format!("{word}:30"));
        assert!(msg.contains(&format!("`{word}`")), "{word}: it names the kind: {msg}");
        assert!(msg.contains("account data"), "{word}: it says why: {msg}");
        assert!(msg.contains("Leave it out"), "{word}: it says what to do: {msg}");
    }
    assert_eq!(
        account.len() + retainable().count(),
        StoreKindId::ALL.len(),
        "every kind is on a side"
    );
    // Refused wherever it stands in the row, not only first.
    assert!(refusal("trade:90,exec_fill:30").contains("exec_fill"));
    assert!(refusal("bar:7,equity:30").contains("`equity`"));
}

#[test]
fn an_unknown_kind_is_refused_and_the_accepted_kinds_are_listed() {
    let msg = refusal("bok:14");
    assert!(msg.contains("`bok`"), "{msg}");
    assert!(msg.contains("not a store kind"), "{msg}");
    let kinds = retainable().map(StoreKindId::word).collect::<Vec<_>>().join(", ");
    assert!(msg.contains(&format!("the kinds are {kinds}, and the lanes")), "{msg}");
    // The whole refusal: only the retainable kinds are offered (an account kind never is:
    // `every_account_kind_is_refused_by_name_with_its_reason` holds every kind to one side), and
    // there is no case hint for a word that is just wrong.
    assert_eq!(
        msg,
        format!(
            "`bok` is not a store kind a retention can name; the kinds are {kinds}, and the lanes \
             bar@recorder, quote@recorder"
        )
    );
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
    assert_eq!(
        msg,
        format!(
            "the entry `{}…` is not `kind:days` — every entry is a store kind, a colon and a \
             whole number of days, `book:14`",
            "x".repeat(24)
        ),
        "the whole value must not be printed"
    );
}

/// The smallest retention is one day.
#[test]
fn the_minimum_is_one_day() {
    assert_eq!(MIN_RETENTION_DAYS, 1);
}
