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

/// The row spelling round-trips through serde, and `as_str` is the same set the parser takes.
#[test]
fn the_three_spellings_round_trip() {
    for mode in VenueMode::ALL {
        let text = format!("m = {:?}\n", mode.as_str());
        #[derive(Deserialize)]
        struct One {
            m: VenueMode,
        }
        assert_eq!(toml::from_str::<One>(&text).expect("parses").m, mode);
        assert!(legal_modes().contains(mode.as_str()), "{mode} missing from {}", legal_modes());
    }
}

/// [`roster_id`] is exact and case-SENSITIVE: a lookup that quietly accepted a near miss would be
/// repairing the caller's spelling for them.
#[test]
fn the_roster_lookup_is_exact() {
    assert_eq!(roster_id("bybit"), Some("bybit"));
    assert_eq!(roster_id("Bybit"), None, "ids are lowercase and the lookup does not repair");
    assert_eq!(roster_id("bybitt"), None);
    assert_eq!(roster_id(""), None);
    for venue in VENUES {
        assert_eq!(roster_id(venue), Some(*venue), "every roster id finds itself");
    }
}
