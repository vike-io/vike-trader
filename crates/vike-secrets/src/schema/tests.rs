use vike_model::AccountTier;

use super::tiers::ANY_TIER;
use super::*;

/// **The two boundaries are inverses over every Rust tier**, and BOTH spellings of "no tier" read
/// as `None` — the stored `'any'`, and a NULL. A read boundary that passed `'any'` through is the
/// trap step 7 was split off stage 4a for.
#[test]
fn the_stored_tier_and_the_rust_tier_round_trip() {
    for tier in [None, Some(AccountTier::Paper), Some(AccountTier::Demo), Some(AccountTier::Live)] {
        let stored = stored_venue_setting_tier(tier);
        assert_eq!(
            venue_setting_tier_of_stored(0, Some(stored.to_string())).expect("a stored word"),
            tier,
            "{tier:?} -> {stored:?} -> … must come back to itself"
        );
    }
    assert_eq!(stored_venue_setting_tier(None), ANY_TIER, "no tier is STORED as the word");
    assert_eq!(venue_setting_tier_of_stored(0, None).expect("a NULL"), None, "a NULL reads None");
    // …and a stored word that is no tier fails the read rather than being guessed into one.
    assert!(venue_setting_tier_of_stored(0, Some("sim".to_string())).is_err());
}

/// **`'any'` is a STORED word, never a tier** — so it can collide with neither vocabulary
/// the dotted-key grammar classifies by, and an account can never be filed under it.
#[test]
fn the_any_word_is_not_a_tier() {
    assert!(
        ANY_TIER.parse::<AccountTier>().is_err(),
        "`venue.<v>.any.<f>` must not be read as a tier-scoped key — `any` stays in the FIELD"
    );
}
