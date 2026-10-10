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
        !ACCOUNT_TIERS.contains(&SIM_KEY_TOKEN),
        "…and it is NOT an account tier — the whole point of §4.4 is that the two vocabularies \
             are different words, not one word in two cases"
    );
}

/// **Every credential-key tier token maps onto an [`ACCOUNT_TIERS`] member**, which is the
/// property `AccountResolver`'s [`SchemaRefusal::UnknownTier`] would otherwise fire on for a
/// perfectly ordinary key. Written as a loop over the real table so a FOURTH tier token cannot
/// be added upstream without this going red.
#[test]
fn every_credential_tier_token_names_an_account_tier() {
    for token in vike_model::credential_keys::CREDENTIAL_TIERS {
        let tier = account_tier_of_key_token(token);
        assert!(
            ACCOUNT_TIERS.contains(&tier.as_str()),
            "`{token}` maps to {tier:?}, which the `account` CHECK refuses"
        );
    }
}

/// **The two maps are inverses over the whole vocabulary**, which is what keeps a
/// `venue_setting` row's legacy credential NAME renderable from the tier the row stores. The
/// `paper` <-> `SIM` pair is the only one where they are not a case change, and it is the one
/// that silently broke a round trip before this landed.
#[test]
fn the_tier_and_the_key_token_round_trip() {
    for tier in ACCOUNT_TIERS {
        let token = key_token_of_account_tier(tier);
        assert_eq!(
            account_tier_of_key_token(&token),
            tier,
            "`{tier}` -> `{token}` -> … must come back to itself"
        );
        assert!(
            vike_model::credential_keys::CREDENTIAL_TIERS.contains(&token.as_str()),
            "`{tier}` renders the key token `{token}`, which no credential key is spelled with"
        );
    }
    assert_eq!(key_token_of_account_tier(PAPER_TIER), SIM_KEY_TOKEN, "the ONE non-case pair");
}

/// **The legacy INPUT spelling still classifies**, which is the migration for a dotted
/// `venue_setting` key an operator typed before the rename. It answers the CANONICAL word, so
/// `venue.ibkr.sim.backend` and `venue.ibkr.paper.backend` address the same row rather than
/// two.
#[test]
fn the_pre_rename_spelling_still_names_the_paper_tier() {
    for word in ["sim", "SIM", "Sim", "paper", "PAPER"] {
        assert_eq!(account_tier_named(word), Some(PAPER_TIER), "{word:?} must name the paper tier");
    }
    assert_eq!(account_tier_named("demo"), Some("demo"));
    assert_eq!(account_tier_named("live"), Some("live"));
    // …and a word that is not a tier answers `None` rather than being lowercased into one,
    // which is what lets the venue-settings grammar tell a tier segment from a FIELD.
    assert_eq!(account_tier_named("backend"), None);
    assert_eq!(account_tier_named("testnet"), None, "aster's own token is NOT a tier here");
}

// -----------------------------------------------------------------------------------------
// §5.2 step 7 — the `'any'` word and its two boundaries
// -----------------------------------------------------------------------------------------

/// **The two boundaries are inverses over every Rust tier**, and BOTH spellings of "no tier" read
/// as `None` — the stored `'any'`, and a NULL. A read boundary that passed `'any'` through is the
/// trap step 7 was split off stage 4a for.
#[test]
fn the_stored_tier_and_the_rust_tier_round_trip() {
    for tier in [None, Some("paper"), Some("demo"), Some("live")] {
        let stored = stored_venue_setting_tier(tier).expect("every Rust tier has a stored word");
        assert_eq!(
            venue_setting_tier_of_stored(Some(stored.to_string())).as_deref(),
            tier,
            "{tier:?} -> {stored:?} -> … must come back to itself"
        );
    }
    assert_eq!(
        stored_venue_setting_tier(None).expect("no tier"),
        ANY_TIER,
        "no tier is STORED as the word"
    );
    assert_eq!(venue_setting_tier_of_stored(None), None, "a NULL still reads None");
    // …and the word is reached from `None` ALONE. A `Some("any")` passed through would file a
    // "tier" onto the machine-scoped row — the write boundary's own doc carries how that was
    // found — so the two directions are inverses only while this is refused.
    assert!(
        stored_venue_setting_tier(Some(ANY_TIER)).is_err(),
        "`Some(\"any\")` must be refused, not stored as the machine-scoped row's word"
    );
}

/// **`'any'` is a STORED word, never a tier** — so it can collide with neither vocabulary
/// the dotted-key grammar classifies by, and an account can never be filed under it.
#[test]
fn the_any_word_is_not_a_tier() {
    assert!(!ACCOUNT_TIERS.contains(&ANY_TIER), "`'any'` must not be an account tier");
    assert_eq!(
        account_tier_named(ANY_TIER),
        None,
        "`venue.<v>.any.<f>` must not be read as a tier-scoped key — `any` stays in the FIELD"
    );
}
