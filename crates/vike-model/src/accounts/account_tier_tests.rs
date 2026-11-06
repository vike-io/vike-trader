use super::*;

/// **The ordering the tier vocabulary carries.** Pinned as a full comparison chain rather than as
/// two `assert!(a < b)` lines, because every "reached below the account's tier" check depends on
/// the TOTAL order.
#[test]
fn paper_is_below_demo_is_below_live() {
    assert!(AccountTier::Paper < AccountTier::Demo);
    assert!(AccountTier::Demo < AccountTier::Live);
    assert!(AccountTier::Paper < AccountTier::Live);
    // …and `ALL` is that order, so a menu and a comparison cannot disagree.
    let mut sorted = AccountTier::ALL;
    sorted.sort_unstable();
    assert_eq!(sorted, AccountTier::ALL);
    assert_eq!(AccountTier::default(), AccountTier::Paper, "the safe end is the default");
}

/// The row spelling round-trips through serde AND through the parser, and `as_str` is the set both
/// take.
#[test]
fn the_three_spellings_round_trip() {
    for mode in AccountTier::ALL {
        let json = serde_json::to_string(&mode).expect("serializes");
        assert_eq!(json, format!("\"{}\"", mode.as_str()));
        assert_eq!(serde_json::from_str::<AccountTier>(&json).expect("parses"), mode);
        assert_eq!(mode.as_str().parse::<AccountTier>(), Ok(mode));
        assert_eq!(mode.to_string(), mode.as_str());
        assert!(
            AccountTier::legal_words().contains(mode.as_str()),
            "{mode} missing from {}",
            AccountTier::legal_words()
        );
    }
}

/// **Case and surrounding whitespace do not matter** — `ibc-start --tier DEMO` and
/// `account add --tier LIVE` worked before the type existed and still do.
#[test]
fn the_parser_ignores_case_and_surrounding_whitespace() {
    for (word, mode) in [
        ("Paper", AccountTier::Paper),
        ("DEMO", AccountTier::Demo),
        ("Live", AccountTier::Live),
        (" live", AccountTier::Live),
        ("demo ", AccountTier::Demo),
    ] {
        assert_eq!(word.parse::<AccountTier>().expect(word), mode, "{word:?}");
    }
}

/// **Anything that is not one of the three words is refused.** `sim` was the old spelling of
/// `paper` and is refused (decision 0129).
#[test]
fn the_parser_refuses_every_other_word() {
    for word in ["sim", "SIM", "", "any", "testnet", "mainnet", "de mo"] {
        let err = word.parse::<AccountTier>().expect_err(word);
        assert_eq!(err.word, word);
        assert!(err.to_string().contains("paper / demo / live"), "{err}");
    }
    assert!(serde_json::from_str::<AccountTier>("\"sim\"").is_err(), "serde is as strict");
}
