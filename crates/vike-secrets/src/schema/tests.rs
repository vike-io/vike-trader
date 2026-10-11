use vike_model::VenueMode;

use super::tiers::ANY_TIER;
use super::*;

/// **The one duplicated word, gated by MEMBERSHIP rather than by position.**
///
/// [`SIM_KEY_TOKEN`] is spelled out here and also lives in
/// `vike_model::credential_keys::CREDENTIAL_TIERS`. Taking it as `CREDENTIAL_TIERS[0]` would
/// read as *the first tier*, which is not what it is, and would silently re-point at `DEMO` if
/// that table were ever reordered. This asserts what is actually true — that the token is one
/// of the credential-key tiers — so a reorder is invisible and a REMOVAL is red.
#[test]
fn the_paper_tier_is_spelled_sim_in_a_credential_key() {
    assert!(
        vike_model::credential_keys::CREDENTIAL_TIERS.contains(&SIM_KEY_TOKEN),
        "`SIM_KEY_TOKEN` must be a real credential-key tier: {:?}",
        vike_model::credential_keys::CREDENTIAL_TIERS
    );
    assert!(
        SIM_KEY_TOKEN.parse::<VenueMode>().is_err(),
        "…and it is NOT an account tier — the whole point of §4.4 is that the two vocabularies \
             are different words, not one word in two cases"
    );
}

/// **Every credential-key tier token maps onto a tier**, which a classifier filing an ordinary key
/// relies on. Written as a loop over the real table so a FOURTH tier token cannot be added upstream
/// without this going red.
#[test]
fn every_credential_tier_token_names_an_account_tier() {
    for token in vike_model::credential_keys::CREDENTIAL_TIERS {
        assert!(
            account_tier_of_key_token(token).is_some(),
            "`{token}` maps to no tier, so a key spelled with it would be filed nowhere"
        );
    }
}

/// **The two maps are inverses over the whole vocabulary**, which is what keeps a
/// `venue_setting` row's legacy credential NAME renderable from the tier the row stores. The
/// `paper` <-> `SIM` pair is the only one where they are not a case change, and it is the one
/// that silently broke a round trip before this landed.
#[test]
fn the_tier_and_the_key_token_round_trip() {
    for tier in VenueMode::ALL {
        let token = key_token_of_account_tier(tier);
        assert_eq!(
            account_tier_of_key_token(token),
            Some(tier),
            "`{tier}` -> `{token}` -> … must come back to itself"
        );
        assert!(
            vike_model::credential_keys::CREDENTIAL_TIERS.contains(&token),
            "`{tier}` renders the key token `{token}`, which no credential key is spelled with"
        );
    }
    assert_eq!(key_token_of_account_tier(VenueMode::Paper), SIM_KEY_TOKEN, "the ONE non-case pair");
}

/// **The key-token map is EXACT**: it maps the three tokens an operator types into a key NAME and
/// nothing else. A tier word is not a token (`paper` is not `SIM`), and a lowercased token is not
/// one either — the map never repairs a spelling, and it is never the tier parser's input.
#[test]
fn the_key_token_map_takes_exactly_the_three_tokens() {
    for word in ["sim", "paper", "PAPER", "demo", "live", "Live", "TESTNET", "DEMO1", ""] {
        assert_eq!(account_tier_of_key_token(word), None, "{word:?} is no credential-key token");
    }
}

// -----------------------------------------------------------------------------------------
// §5.2 step 7 — the `'any'` word and its two boundaries
// -----------------------------------------------------------------------------------------

/// **The two boundaries are inverses over every Rust tier**, and BOTH spellings of "no tier" read
/// as `None` — the stored `'any'`, and a NULL. A read boundary that passed `'any'` through is the
/// trap step 7 was split off stage 4a for.
#[test]
fn the_stored_tier_and_the_rust_tier_round_trip() {
    for tier in [None, Some(VenueMode::Paper), Some(VenueMode::Demo), Some(VenueMode::Live)] {
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
        ANY_TIER.parse::<VenueMode>().is_err(),
        "`venue.<v>.any.<f>` must not be read as a tier-scoped key — `any` stays in the FIELD"
    );
    assert_eq!(account_tier_of_key_token(ANY_TIER), None, "nor is it a credential-key token");
}
