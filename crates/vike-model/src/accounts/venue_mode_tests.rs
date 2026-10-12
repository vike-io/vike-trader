use super::*;

/// **The ordering the tier vocabulary carries.** Pinned as a full comparison chain rather than as
/// two `assert!(a < b)` lines, because every "reached below the account's tier" check depends on
/// the TOTAL order.
#[test]
fn paper_is_below_demo_is_below_live() {
    assert!(VenueMode::Paper < VenueMode::Demo);
    assert!(VenueMode::Demo < VenueMode::Live);
    assert!(VenueMode::Paper < VenueMode::Live);
    // …and `ALL` is that order, so a menu and a comparison cannot disagree.
    let mut sorted = VenueMode::ALL;
    sorted.sort_unstable();
    assert_eq!(sorted, VenueMode::ALL);
    assert_eq!(VenueMode::default(), VenueMode::Paper, "the safe end is the default");
}

/// The row spelling round-trips through serde AND through the parser, and `as_str` is the set both
/// take.
#[test]
fn the_three_spellings_round_trip() {
    for mode in VenueMode::ALL {
        let json = serde_json::to_string(&mode).expect("serializes");
        assert_eq!(json, format!("\"{}\"", mode.as_str()));
        assert_eq!(serde_json::from_str::<VenueMode>(&json).expect("parses"), mode);
        assert_eq!(mode.as_str().parse::<VenueMode>(), Ok(mode));
        assert_eq!(mode.to_string(), mode.as_str());
        assert!(
            VenueMode::legal_words().contains(mode.as_str()),
            "{mode} missing from {}",
            VenueMode::legal_words()
        );
    }
}

/// **Case and surrounding whitespace do not matter** — `ibc-start --tier DEMO` and
/// `account add --tier LIVE` worked before the type existed and still do.
#[test]
fn the_parser_ignores_case_and_surrounding_whitespace() {
    for (word, mode) in [
        ("Paper", VenueMode::Paper),
        ("DEMO", VenueMode::Demo),
        ("Live", VenueMode::Live),
        (" live", VenueMode::Live),
        ("demo ", VenueMode::Demo),
    ] {
        assert_eq!(word.parse::<VenueMode>().expect(word), mode, "{word:?}");
    }
}

/// **Anything that is not one of the three words is refused.** `sim` was the old spelling of
/// `paper` and is refused (decision 0129).
#[test]
fn the_parser_refuses_every_other_word() {
    for word in ["sim", "SIM", "", "any", "testnet", "mainnet", "de mo"] {
        let err = word.parse::<VenueMode>().expect_err(word);
        assert_eq!(err.word, word);
        assert!(err.to_string().contains("paper / demo / live"), "{err}");
    }
    assert!(serde_json::from_str::<VenueMode>("\"sim\"").is_err(), "serde is as strict");
}
