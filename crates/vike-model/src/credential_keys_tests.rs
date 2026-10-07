use super::*;

/// **Every mechanised venue's attribution VAR is one of the two declared spellings, and it is
/// the one its mechanic implies** — the property `attribution_var_for` exists to hold, stated
/// where the classification lives rather than at the one caller that would otherwise have to
/// re-derive it.
///
/// ⚠ Both directions: an unmechanised venue must answer `None`, or a form would offer an
/// affiliate field for a venue whose `attribution_code_from` returns before it builds a key.
#[test]
fn the_attribution_var_follows_the_mechanic_for_every_roster_venue() {
    let mut mechanised = 0usize;
    for venue in VENUES {
        let mech = attribution_for(venue);
        match attribution_var_for(venue) {
            None => assert!(mech.is_none(), "{venue} has a mechanic and no var"),
            Some(var) => {
                assert!(!mech.is_none(), "{venue} has no mechanic and a var: {var}");
                mechanised += 1;
                assert!(var.starts_with(&venue.to_uppercase()), "{var} must be {venue}'s");
                let want = if matches!(
                    mech,
                    crate::venues::attribution::AttributionMechanic::SignedBuilder { .. }
                ) {
                    BUILDER_CODE_SUFFIX
                } else {
                    BROKER_CODE_SUFFIX
                };
                assert!(var.ends_with(want), "{var} must end with {want}");
                // ...and it is a name the reader would really look up.
                assert!(attribution_keys().contains(&var), "{var} is outside the grid");
            }
        }
    }
    assert!(mechanised >= 6, "only {mechanised} venue(s) classified — the fold has gone quiet");
}

/// **`key_owner` is TOTAL over the grid and answers for nothing else** — both directions, so the
/// membership half of `vike-cli secrets set`'s validation cannot go quietly wrong in either
/// one. A key the workspace can read that this classifier rejects is a key an operator would be
/// refused by name; a name outside the grid that it accepts is a key nothing will ever read,
/// written into the store with a success message.
#[test]
fn key_owner_classifies_exactly_the_lookup_grid() {
    for key in lookup_keys() {
        let (venue, tier) = key_owner(&key)
            .unwrap_or_else(|| panic!("{key} is in lookup_keys() and must be classified"));
        assert!(VENUES.contains(&venue), "{key}: {venue} is not a roster venue");
        assert!(key.starts_with(&venue.to_uppercase()), "{key} must be {venue}'s");
        match tier {
            // A credential key names a tier, and the name really carries that spelling —
            // including the legacy one, which is NOT normalized here (see the doc).
            Some(t) => assert!(
                CREDENTIAL_TIERS.contains(&t) || LEGACY_CREDENTIAL_TIERS.contains(&t),
                "{key}: {t} is no tier"
            ),
            // …and the tier-less answer is exactly the attribution family.
            None => assert!(
                key.ends_with(BROKER_CODE_SUFFIX) || key.ends_with(BUILDER_CODE_SUFFIX),
                "{key} answered with no tier but is not an attribution key"
            ),
        }
    }
    // The other direction: near-misses, a bespoke key the grid deliberately excludes, and an
    // unmechanised venue's attribution code all answer `None`.
    //
    // ⚠ **The near-misses are COMPOSED, never spelled**, and that is not style.
    // `crates/vike-ops/tests/settings/settings_registry.rs`'s literal harvest reads ANY string literal
    // with env-var shape and a known prefix as evidence that this crate READS that variable,
    // and then demands a `vike_ops::settings::SETTINGS` row for it — so spelling
    // `{VENUE}_{TIER}_API_KEYS` here as test data would demand a registry row for a key nothing
    // reads, which is precisely what that registry exists to refuse. Composing them keeps the
    // fixtures out of the harvest while leaving them exactly as near a miss.
    let real = credential_key(VENUES[0], CREDENTIAL_TIERS[0], API_KEY_SUFFIX);
    let outsiders = [
        format!("{real}_"),
        format!("_{real}"),
        format!("{real}S"),
        real.replace(CREDENTIAL_TIERS[0], "PROD"),
        format!("NOTAVENUE_{}", &real[real.find('_').unwrap() + 1..]),
        // Two BESPOKE shapes, likewise composed: real keys their own bridge's `config.rs`
        // reads with a literal, and which this grid deliberately does not cover.
        format!("FXCM_{}", "DEMO_USER"),
        format!("VIKE_{}", "TRADEHUB_CONTROL_KEY"),
        String::new(),
    ];
    for outsider in &outsiders {
        assert!(key_owner(outsider).is_none(), "{outsider} must not be classified");
    }
    // …and an attribution code for a venue with NO order-level mechanic is nobody's key, the
    // same narrowing `attribution_keys` applies.
    let unmechanised = VENUES
        .iter()
        .find(|v| attribution_for(v).is_none())
        .expect("some roster venue has no attribution mechanic");
    assert!(key_owner(&attribution_key(unmechanised, BROKER_CODE_SUFFIX)).is_none());
}
/// The grid is exactly the product it claims to be — DERIVED from the roster's length, never a
/// pinned count. A count checked against itself is the failure
/// `crate::venues`' `roster_matches_the_bridge_crates` was rewritten to stop shipping.
#[test]
fn the_credential_grid_is_the_roster_times_the_tiers_times_the_suffixes() {
    let tiers = CREDENTIAL_TIERS.len() + LEGACY_CREDENTIAL_TIERS.len();
    assert_eq!(credential_keys().len(), VENUES.len() * tiers * CREDENTIAL_SUFFIXES.len());
}

/// …and the attribution half is the MECHANIC venues times the suffixes, with the roster's
/// unmechanised venues genuinely absent (not merely unlisted).
#[test]
fn the_attribution_grid_covers_exactly_the_mechanised_venues() {
    let mechanised = VENUES.iter().filter(|v| !attribution_for(v).is_none()).count();
    assert_eq!(attribution_keys().len(), mechanised * ATTRIBUTION_SUFFIXES.len());
    assert!(mechanised > 0 && mechanised < VENUES.len(), "both arms must be non-empty");
    let keys = attribution_keys();
    for venue in VENUES.iter().filter(|v| attribution_for(v).is_none()) {
        for sfx in ATTRIBUTION_SUFFIXES {
            let key = attribution_key(venue, sfx);
            assert!(!keys.contains(&key), "{key} is never looked up — it must not be declared");
        }
    }
}

/// The SPELLING, pinned against a name written out in full. Everything else in this file
/// composes the same constants the code under test composes, which would keep agreeing after a
/// change to either; this one assertion is the anchor that says WHICH strings those are.
///
/// The literal is a declared grid key, so it is safe for
/// `crates/vike-ops/tests/settings/settings_registry.rs`'s literal sweep to observe. Do not spell a name
/// here that the grid does not contain — an env-shaped literal with no registry row fails that
/// gate's direction 1, wherever in the tree it sits.
#[test]
fn a_credential_key_is_venue_then_tier_then_suffix() {
    assert_eq!(credential_key("binance", "DEMO", API_KEY_SUFFIX), "BINANCE_DEMO_API_KEY");
    assert_eq!(attribution_key("okx", BROKER_CODE_SUFFIX), "OKX_BROKER_CODE");
}

/// Every key is uppercase/underscore/digit shaped and carries one of the declared suffixes —
/// the property the registry's own name predicate (`vike_ops::scan`'s `is_env_name`) demands
/// before it will even consider a string an environment variable.
#[test]
fn every_key_is_env_shaped_and_carries_a_declared_suffix() {
    for key in lookup_keys() {
        assert!(
            key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "{key} is not env-var shaped"
        );
        assert!(
            CREDENTIAL_SUFFIXES.iter().chain(ATTRIBUTION_SUFFIXES.iter()).any(|s| key.ends_with(s)),
            "{key} ends in no declared suffix"
        );
    }
}

/// Sorted and deduplicated, so a gate diffing this against a committed table prints one line
/// per change rather than a reordering.
#[test]
fn the_grid_is_sorted_and_unique() {
    for grid in [credential_keys(), attribution_keys(), lookup_keys()] {
        let mut sorted = grid.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(grid, sorted);
    }
}

/// **The platform table and the venue grid do not intersect, in either direction** — the
/// property every one of [`PLATFORM_KEYS`]' three "NOT unioned into" clauses rests on.
///
/// A platform key that `key_owner` classified would become settable through
/// `vike-cli secrets set` the moment somebody widened one function, silently reversing the
/// "generating is the fix; accepting is not" verdict; a grid key that answered `true` here
/// would let a node-key writer overwrite a venue's live signing credential.
#[test]
fn platform_keys_are_outside_the_venue_grid() {
    for key in PLATFORM_KEYS {
        assert!(
            key_owner(key).is_none(),
            "{key} is a PLATFORM key and must belong to no venue — `secrets set` refuses it"
        );
        assert!(!lookup_keys().contains(&key.to_string()), "{key} must not be in the grid");
    }
    for key in lookup_keys() {
        assert!(!is_platform_key(&key), "{key} is a venue key and is not a platform key");
    }
}

/// The table is two DISTINCT, env-shaped names, and the membership test answers for them and
/// for nothing near them.
///
/// ⚠ The near-misses are COMPOSED for the reason [`PLATFORM_KEYS`]' own doc gives and
/// `key_owner_classifies_exactly_the_lookup_grid` gives above: a whole env-shaped literal in
/// this file is read by the settings registry's harvest as a READ, and demands a row for a
/// variable this crate does not read.
#[test]
fn the_platform_table_is_two_distinct_env_shaped_names() {
    assert_ne!(PLATFORM_KEYS[0], PLATFORM_KEYS[1], "the two node keys are separate credentials");
    for key in PLATFORM_KEYS {
        assert!(
            key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "{key} is not env-var shaped"
        );
        assert!(is_platform_key(key), "{key} must answer its own membership test");
    }
    for outsider in
        [format!("{}_", PLATFORM_KEYS[0]), PLATFORM_KEYS[0].to_lowercase(), String::new()]
    {
        assert!(!is_platform_key(&outsider), "{outsider:?} must not be a platform key");
    }
}

/// **The two SERVICES partition [`PLATFORM_KEYS`]** — every name belongs to exactly one of
/// them, neither claims a name outside the table, and neither is empty.
///
/// ⚠ **This asserted it of the two PAIR PREDICATES until 2026-09-20, and that was only ever
/// true while every platform key was half of a pair.** `VIKE_TRADEHUB_ADMIN_KEY` is a tradehub
/// name that is NOT half of the tradehub pair, so the two are now different questions and the
/// partition belongs to [`platform_key_service`], which is the one that answers *whose key is
/// this*. Asserting it of the predicates instead would force the admin key into
/// [`is_tradehub_node_key`] — and that predicate's own doc records what widening it costs: it is
/// handed to `vike_secrets::resolve_node_keys` to decide WHICH FILE answers for the pair, and a
/// `node.env` holding only the admin key would become that answer while a working pair sat in
/// the settings database. Client signs with an empty key, node says `bad mac`, nothing warns.
///
/// The safety argument the partition carries is UNCHANGED and still lands on the predicates —
/// it is just stated over the right set now. Disjointness is what makes it impossible to stitch
/// a pair across the two credential files; `the_pair_predicates_are_the_pairs_and_nothing_else`
/// below is where exhaustiveness-over-the-pairs is asserted, since the pairs no longer exhaust
/// the table.
#[test]
fn the_two_service_families_partition_the_platform_table() {
    let mut tradehub = 0;
    let mut datahub = 0;
    for key in PLATFORM_KEYS {
        let t = platform_key_service(key) == Some(TRADEHUB_SERVICE);
        let d = platform_key_service(key) == Some(DATAHUB_SERVICE);
        assert!(t ^ d, "{key} belongs to both services or to neither");
        tradehub += usize::from(t);
        datahub += usize::from(d);
    }
    assert_eq!(tradehub + datahub, PLATFORM_KEYS.len(), "a platform key belongs to no service");
    assert!(tradehub > 0 && datahub > 0, "a service that owns nothing is not one");
    for key in lookup_keys() {
        assert!(!is_tradehub_node_key(&key), "{key} is a venue key");
        assert!(!is_datahub_node_key(&key), "{key} is a venue key");
    }
    // …and the service names are the ones the classifier answers with, not a fourth copy.
    assert_eq!(platform_key_service(PLATFORM_KEYS[0]), Some(TRADEHUB_SERVICE));
    assert_eq!(platform_key_service(PLATFORM_KEYS[2]), Some(DATAHUB_SERVICE));
}

/// **The PAIR predicates match the two PAIRS and nothing else** — the half that used to ride on
/// the partition test above and cannot any more, now that a platform key exists which belongs to
/// a service without belonging to its pair.
///
/// ⚠ **The load-bearing assertion is the NEGATIVE one.** `is_tradehub_node_key` is what
/// `vike_secrets::resolve_node_keys` probes a file with to decide where the observe/control pair
/// is read from, so every name it matches is a name that can make a file *the answer*. The admin
/// key must never be one: a `node.env` holding it alone would win the probe and a working pair
/// in the settings database would be dropped — the measured `bad mac` shape that predicate's own
/// doc describes, reproduced by a third name instead of by a second service.
#[test]
fn the_pair_predicates_are_the_pairs_and_nothing_else() {
    assert!(is_tradehub_node_key(PLATFORM_KEYS[0]) && is_tradehub_node_key(PLATFORM_KEYS[1]));
    assert!(is_datahub_node_key(PLATFORM_KEYS[2]) && is_datahub_node_key(PLATFORM_KEYS[3]));

    let admin = PLATFORM_KEYS[4];
    assert!(
        !is_tradehub_node_key(admin),
        "{admin} must not decide where the tradehub PAIR is read from"
    );
    assert!(!is_datahub_node_key(admin), "{admin} is not the datahub's at all");
    // …while still being a platform name a writer may write, and still routing an operator to
    // the command that owns it. Three predicates, three questions.
    assert!(is_platform_key(admin));
    assert_eq!(platform_key_service(admin), Some(TRADEHUB_SERVICE));

    // The pairs are exactly two each, so a name added to either predicate fails here rather than
    // silently widening a file probe.
    let pair_matches = PLATFORM_KEYS.iter().filter(|k| is_tradehub_node_key(k)).count();
    assert_eq!(pair_matches, 2, "the tradehub PAIR is two names");
    assert_eq!(PLATFORM_KEYS.iter().filter(|k| is_datahub_node_key(k)).count(), 2);
}

/// The two uppercasings the workspace used before this table existed agree on every roster id,
/// which is what makes [`attribution_key`]'s switch from `to_ascii_uppercase` a no-op.
#[test]
fn the_two_uppercasings_agree_on_every_roster_venue() {
    for venue in VENUES {
        assert_eq!(venue.to_uppercase(), venue.to_ascii_uppercase(), "{venue}");
    }
}
