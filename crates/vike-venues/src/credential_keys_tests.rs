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
                    crate::attribution::AttributionMechanic::SignedBuilder { .. }
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

/// **`attribution_key_owner` is TOTAL over the attribution grid and answers for nothing else** —
/// both directions, so the membership half of `vike-cli secrets set`'s validation cannot go quietly
/// wrong in either one.
#[test]
fn attribution_key_owner_classifies_exactly_the_attribution_grid() {
    for key in attribution_keys() {
        let venue = attribution_key_owner(&key)
            .unwrap_or_else(|| panic!("{key} is in attribution_keys() and must be classified"));
        assert!(VENUES.contains(&venue), "{key}: {venue} is not a roster venue");
        assert!(key.starts_with(&venue.to_uppercase()), "{key} must be {venue}'s");
        assert!(
            key.ends_with(BROKER_CODE_SUFFIX) || key.ends_with(BUILDER_CODE_SUFFIX),
            "{key} is not an attribution key"
        );
    }
    // The other direction. ⚠ The near-misses are COMPOSED, never spelled: the settings registry's
    // literal harvest reads ANY env-shaped string literal with a known prefix as a READ by this
    // crate, and would demand a registry row for a key nothing reads.
    let real = attribution_keys()[0].clone();
    let outsiders = [
        format!("{real}_"),
        format!("_{real}"),
        format!("{real}S"),
        format!("NOTAVENUE{}", BROKER_CODE_SUFFIX),
        format!("VIKE_{}", "TRADEHUB_CONTROL_KEY"),
        String::new(),
    ];
    for outsider in &outsiders {
        assert!(attribution_key_owner(outsider).is_none(), "{outsider} must not be classified");
    }
    // …and an attribution code for a venue with NO order-level mechanic is nobody's key, the
    // same narrowing `attribution_keys` applies.
    let unmechanised = VENUES
        .iter()
        .find(|v| attribution_for(v).is_none())
        .expect("some roster venue has no attribution mechanic");
    assert!(attribution_key_owner(&attribution_key(unmechanised, BROKER_CODE_SUFFIX)).is_none());
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
/// The literal is a declared attribution key, so it is safe for
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s literal sweep to observe.
#[test]
fn an_attribution_key_is_venue_then_suffix() {
    assert_eq!(attribution_key("okx", BROKER_CODE_SUFFIX), "OKX_BROKER_CODE");
}

/// Every attribution key is uppercase/underscore/digit shaped and carries one of the declared
/// suffixes — the property the registry's own name predicate (`vike_model::scan`'s
/// `is_env_name`) demands before it will even consider a string an environment variable.
#[test]
fn every_key_is_env_shaped_and_carries_a_declared_suffix() {
    for key in attribution_keys() {
        assert!(
            key.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "{key} is not env-var shaped"
        );
        assert!(
            ATTRIBUTION_SUFFIXES.iter().any(|s| key.ends_with(s)),
            "{key} ends in no declared suffix"
        );
    }
}

/// The generic HMAC set's three FIELD words are the store's `credential.field` spellings: upper
/// case, no tier, no separator at either end.
#[test]
fn the_api_field_words_carry_no_tier_and_no_separator() {
    for field in [API_KEY, API_SECRET, API_PASSPHRASE] {
        assert!(!field.starts_with('_') && !field.ends_with('_'), "{field}");
        assert!(field.chars().all(|c| c.is_ascii_uppercase() || c == '_'), "{field}");
        for token in ["SIM", "DEMO", "LIVE", "PAPER"] {
            assert!(!field.split('_').any(|w| w == token), "{field} carries a tier token");
        }
    }
}

/// Sorted and deduplicated, so a gate diffing this against a committed table prints one line
/// per change rather than a reordering.
#[test]
fn the_grid_is_sorted_and_unique() {
    let grid = attribution_keys();
    let mut sorted = grid.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(grid, sorted);
}

/// **The platform table and the venue grid do not intersect, in either direction** — the
/// property every one of [`PLATFORM_KEYS`]' three "NOT unioned into" clauses rests on.
///
/// A platform key that `attribution_key_owner` classified would become settable through
/// `vike-cli secrets set` the moment somebody widened one function, silently reversing the
/// "generating is the fix; accepting is not" verdict; a grid key that answered `true` here
/// would let a hub-key writer overwrite a venue's live signing credential.
#[test]
fn platform_keys_are_outside_the_venue_grid() {
    for key in PLATFORM_KEYS {
        assert!(
            attribution_key_owner(key).is_none(),
            "{key} is a PLATFORM key and must belong to no venue — `secrets set` refuses it"
        );
        assert!(!attribution_keys().contains(&key.to_string()), "{key} must not be in the grid");
    }
    for key in attribution_keys() {
        assert!(!is_platform_key(&key), "{key} is a venue key and is not a platform key");
    }
}

/// The table is two DISTINCT, env-shaped names, and the membership test answers for them and
/// for nothing near them.
///
/// ⚠ The near-misses are COMPOSED for the reason [`PLATFORM_KEYS`]' own doc gives and
/// `attribution_key_owner_classifies_exactly_the_attribution_grid` gives above: a whole env-shaped
/// literal in
/// this file is read by the settings registry's harvest as a READ, and demands a row for a
/// variable this crate does not read.
#[test]
fn the_platform_table_is_two_distinct_env_shaped_names() {
    assert_ne!(PLATFORM_KEYS[0], PLATFORM_KEYS[1], "the two hub keys are separate credentials");
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

/// **The SERVICES partition [`PLATFORM_KEYS`]** (two when this test was written and cited by
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` under this name, three since the
/// gatewayhub's pair joined) — every name belongs to exactly one of them, none claims a name
/// outside the table, and none is empty.
///
/// ⚠ **This asserted it of the two PAIR PREDICATES until 2026-09-20, and that was only ever
/// true while every platform key was half of a pair.** `VIKE_TRADEHUB_ADMIN_KEY` is a tradehub
/// name that is NOT half of the tradehub pair, so the two are now different questions and the
/// partition belongs to [`platform_key_service`], which is the one that answers *whose key is
/// this*. Asserting it of the predicates instead would force the admin key into
/// [`is_tradehub_pair_key`] — and that predicate's own doc records what widening it costs: it is
/// the scope `vike_secrets::CredentialStore::hub_keys` hands the CLI and the desktop, and the admin key in
/// it would put the key that writes key material in every reader of the pair.
///
/// The safety argument the partition carries is UNCHANGED and still lands on the predicates —
/// it is just stated over the right set now. Disjointness is what keeps one service's reader from
/// holding the other service's keys; `the_pair_predicates_are_the_pairs_and_nothing_else`
/// below is where exhaustiveness-over-the-pairs is asserted, since the pairs no longer exhaust
/// the table.
#[test]
fn the_two_service_families_partition_the_platform_table() {
    let mut tradehub = 0;
    let mut datahub = 0;
    let mut gatewayhub = 0;
    for key in PLATFORM_KEYS {
        let t = platform_key_service(key) == Some(TRADEHUB_SERVICE);
        let d = platform_key_service(key) == Some(DATAHUB_SERVICE);
        let g = platform_key_service(key) == Some(GATEWAYHUB_SERVICE);
        assert_eq!(
            usize::from(t) + usize::from(d) + usize::from(g),
            1,
            "{key} belongs to more than one service or to none"
        );
        tradehub += usize::from(t);
        datahub += usize::from(d);
        gatewayhub += usize::from(g);
    }
    assert_eq!(
        tradehub + datahub + gatewayhub,
        PLATFORM_KEYS.len(),
        "a platform key belongs to no service"
    );
    assert!(
        tradehub > 0 && datahub > 0 && gatewayhub > 0,
        "a service that owns nothing is not one"
    );
    for key in attribution_keys() {
        assert!(!is_tradehub_pair_key(&key), "{key} is a venue key");
        assert!(!is_datahub_pair_key(&key), "{key} is a venue key");
        assert!(!is_gatewayhub_daemon_key(&key), "{key} is a venue key");
        assert!(!is_datahub_gatewayhub_client_key(&key), "{key} is a venue key");
    }
    // …and the service names are the ones the classifier answers with, not a fourth copy.
    assert_eq!(platform_key_service(PLATFORM_KEYS[0]), Some(TRADEHUB_SERVICE));
    assert_eq!(platform_key_service(PLATFORM_KEYS[2]), Some(DATAHUB_SERVICE));
    assert_eq!(platform_key_service(PLATFORM_KEYS[5]), Some(GATEWAYHUB_SERVICE));
    // The three names are three DIFFERENT strings, or the classifier's arms would shadow each other.
    assert_ne!(TRADEHUB_SERVICE, DATAHUB_SERVICE);
    assert_ne!(TRADEHUB_SERVICE, GATEWAYHUB_SERVICE);
    assert_ne!(DATAHUB_SERVICE, GATEWAYHUB_SERVICE);
}

/// **The PAIR predicates match the two PAIRS and nothing else** — the half that used to ride on
/// the partition test above and cannot any more, now that a platform key exists which belongs to
/// a service without belonging to its pair.
///
/// ⚠ **The load-bearing assertion is the NEGATIVE one.** `is_tradehub_pair_key` is the scope
/// `vike_secrets::CredentialStore::hub_keys` hands the CLI and the desktop, so every name it matches is a
/// name those processes hold. The admin key must never be one: it is the key that writes key
/// material, and decision 0065 keeps it off every reader of the pair.
#[test]
fn the_pair_predicates_are_the_pairs_and_nothing_else() {
    assert!(is_tradehub_pair_key(PLATFORM_KEYS[0]) && is_tradehub_pair_key(PLATFORM_KEYS[1]));
    assert!(is_datahub_pair_key(PLATFORM_KEYS[2]) && is_datahub_pair_key(PLATFORM_KEYS[3]));

    let admin = PLATFORM_KEYS[4];
    assert!(
        !is_tradehub_pair_key(admin),
        "{admin} must not decide where the tradehub PAIR is read from"
    );
    assert!(!is_datahub_pair_key(admin), "{admin} is not the datahub's at all");
    assert!(!is_datahub_daemon_key(admin), "{admin} is not the datahub's at all");
    // …while still being a platform name a writer may write, and still routing an operator to
    // the command that owns it. Three predicates, three questions.
    assert!(is_platform_key(admin));
    assert_eq!(platform_key_service(admin), Some(TRADEHUB_SERVICE));

    // The pairs are exactly two each, so a name added to either predicate fails here rather than
    // silently widening a file probe.
    let pair_matches = PLATFORM_KEYS.iter().filter(|k| is_tradehub_pair_key(k)).count();
    assert_eq!(pair_matches, 2, "the tradehub PAIR is two names");
    assert_eq!(PLATFORM_KEYS.iter().filter(|k| is_datahub_pair_key(k)).count(), 2);

    // ⚠ The datahub's strategy-lane key is the datahub's FAMILY and not its PAIR: filing `[9]`
    // under the datahub must not hand it to a reader of the pair (the desktop, the backtesthub, the
    // CLI, the `study` verb, the tearsheet).
    let lane = PLATFORM_KEYS[9];
    assert_eq!(platform_key_service(lane), Some(DATAHUB_SERVICE));
    assert!(
        !is_datahub_pair_key(lane),
        "{lane} must not decide where the datahub PAIR is read from"
    );
    assert!(is_datahub_daemon_key(lane), "the datahub root verifies {lane}");
}

/// **The datahub DAEMON predicate is the datahub pair PLUS the strategy-lane key, and nothing
/// else** — `is_tradehub_daemon_key`'s twin: a drift in either direction fails here. Dropping `[9]`
/// silently disarms the strategy lane on every box; adding a tradehub or gatewayhub name hands the
/// datahub root another service's keys.
#[test]
fn the_datahub_daemon_predicate_is_the_datahub_pair_plus_the_strategy_lane_key() {
    for index in [2, 3, 9] {
        assert!(
            is_datahub_daemon_key(PLATFORM_KEYS[index]),
            "{} is a name the datahub daemon reads",
            PLATFORM_KEYS[index]
        );
    }
    for index in [0, 1, 4, 5, 6, 7, 8] {
        assert!(
            !is_datahub_daemon_key(PLATFORM_KEYS[index]),
            "{} is another service's and the datahub root never holds it",
            PLATFORM_KEYS[index]
        );
    }
    assert_eq!(PLATFORM_KEYS.iter().filter(|k| is_datahub_daemon_key(k)).count(), 3);
    for key in PLATFORM_KEYS {
        assert_eq!(
            is_datahub_daemon_key(key),
            is_datahub_pair_key(key) || key == PLATFORM_KEYS[9],
            "{key}: the daemon scope is the pair scope plus the strategy-lane key"
        );
    }
    for key in attribution_keys() {
        assert!(!is_datahub_daemon_key(&key), "{key} is a venue key");
    }
    assert!(!is_datahub_daemon_key(""));
    assert!(!is_datahub_daemon_key(&PLATFORM_KEYS[9].to_lowercase()));
}

/// **The DAEMON predicate is the tradehub pair PLUS the admin key, and nothing else.**
///
/// It is what `vike_tradehub::node`'s `start_observe_server` hands `CredentialStore::hub_keys`, and the
/// daemon arms account administration only when the admin key is in the map that returns. The
/// pair-only [`is_tradehub_pair_key`] cannot be that predicate (the desktop and the CLI use it and
/// must not carry the admin key), so this one is separate, and a drift in either direction fails
/// here: dropping `[4]` silently disarms the capability on every box, adding a datahub name hands
/// the daemon another service's keys.
#[test]
fn the_daemon_predicate_is_the_tradehub_pair_plus_the_admin_key() {
    for index in [0, 1, 4] {
        assert!(
            is_tradehub_daemon_key(PLATFORM_KEYS[index]),
            "{} is a name the tradehub daemon reads",
            PLATFORM_KEYS[index]
        );
    }
    for index in [2, 3, 5, 6, 7, 8, 9] {
        assert!(
            !is_tradehub_daemon_key(PLATFORM_KEYS[index]),
            "{} is another service's and the tradehub daemon never holds it",
            PLATFORM_KEYS[index]
        );
    }
    for key in attribution_keys() {
        assert!(!is_tradehub_daemon_key(&key), "{key} is a venue key");
    }
    assert!(!is_tradehub_daemon_key(""));
    assert!(!is_tradehub_daemon_key(&PLATFORM_KEYS[4].to_lowercase()));
    assert_eq!(PLATFORM_KEYS.iter().filter(|k| is_tradehub_daemon_key(k)).count(), 3);

    // The daemon's scope CONTAINS the pair scope and differs from it by exactly the admin key, so
    // the two predicates cannot drift apart unnoticed.
    for key in PLATFORM_KEYS {
        assert_eq!(
            is_tradehub_daemon_key(key),
            is_tradehub_pair_key(key) || key == PLATFORM_KEYS[4],
            "{key}: the daemon scope is the pair scope plus the admin key"
        );
    }
}

/// **The gatewayhub's pair is APPENDED after the five that came before it, and its spelling is
/// pinned against a name written out in full** — the anchor `PLATFORM_KEYS`' own doc promises,
/// because no server crate spells these two names as a constant of its own (so
/// `crates/vike-cli/tests/node_cli.rs` has no reference spelling to hold the table equal to).
///
/// ⚠ The names are COMPOSED, never spelled, for the reason the neighbouring tests give: an
/// env-shaped literal with a known prefix in this crate would read to the settings registry's
/// harvest as `vike-venues` READING the variable, and it reads none.
#[test]
fn the_gatewayhub_pair_is_appended_after_the_five_and_spelled_as_pinned() {
    assert!(PLATFORM_KEYS.len() >= 7, "five that existed, plus the gatewayhub's pair");
    // The first five keep their indices: the index assertions in `vike-cli` and `vike-tradehub`
    // pin `[0..=4]`.
    let first_five = [
        format!("VIKE_{}", "TRADEHUB_OBSERVE_KEY"),
        format!("VIKE_{}", "TRADEHUB_CONTROL_KEY"),
        format!("VIKE_{}", "DATAHUB_OBSERVE_KEY"),
        format!("VIKE_{}", "DATAHUB_CONTROL_KEY"),
        format!("VIKE_{}", "TRADEHUB_ADMIN_KEY"),
    ];
    for (index, name) in first_five.iter().enumerate() {
        assert_eq!(PLATFORM_KEYS[index], name, "[{index}] moved");
    }
    assert_eq!(PLATFORM_KEYS[5], format!("VIKE_{}", "GATEWAYHUB_OBSERVE_KEY"), "observe first");
    assert_eq!(PLATFORM_KEYS[6], format!("VIKE_{}", "GATEWAYHUB_CONTROL_KEY"), "control second");
    // The named accessor is those two names, in that order: every reader of the pair calls it.
    assert_eq!(gatewayhub_key_pair(), [PLATFORM_KEYS[5], PLATFORM_KEYS[6]]);
    // Every name in the table is a DIFFERENT credential: no key is shared across services, and a
    // duplicate would route two services' clients to one secret.
    for (i, a) in PLATFORM_KEYS.iter().enumerate() {
        for b in &PLATFORM_KEYS[i + 1..] {
            assert_ne!(a, b, "two platform keys share a name");
        }
    }
}

/// **The gatewayhub's predicates, and the three daemons' scopes do not overlap.**
///
/// The blast-radius rule is stated over the DAEMON scopes because those are what
/// `vike_secrets::CredentialStore::hub_keys` hands a process: the tradehub daemon, the datahub root
/// and the gatewayhub daemon each admit their own names and no other service's, so every platform key
/// is in exactly one of the three. The datahub's CLIENT predicate for the gatewayhub is a fourth
/// question: the PAIR, held by the other side, contained in the gatewayhub daemon's scope. The
/// gatewayhub daemon's FAMILY is wider than the pair since wave 2 (`[7]` the `Trade` key and `[8]` the
/// admin key), and the datahub's client predicate stays the pair.
#[test]
fn the_gatewayhub_predicates_admit_the_pair_and_the_three_daemon_scopes_do_not_overlap() {
    for index in [5, 6] {
        let key = PLATFORM_KEYS[index];
        assert!(is_platform_key(key), "{key}");
        assert_eq!(platform_key_service(key), Some(GATEWAYHUB_SERVICE), "{key}");
        assert!(is_gatewayhub_daemon_key(key), "{key} is a name the gatewayhub daemon verifies");
        assert!(
            is_datahub_gatewayhub_client_key(key),
            "{key} is a name the datahub holds to dial the gatewayhub"
        );
        // The existing pair predicates do not reach it: the CLI and the desktop resolve those two
        // pairs and must never carry the gatewayhub's.
        assert!(!is_tradehub_pair_key(key) && !is_tradehub_daemon_key(key), "{key}");
        assert!(!is_datahub_pair_key(key), "{key}: the datahub's SERVER pair is [2]/[3] only");
        assert!(!is_datahub_daemon_key(key), "{key}");
    }
    // The daemon's family is the pair plus the Trade and Admin keys, and the datahub's client
    // predicate holds none of the last two: the order-scope key is the tradehub's to hold.
    for index in [7, 8] {
        let key = PLATFORM_KEYS[index];
        assert_eq!(platform_key_service(key), Some(GATEWAYHUB_SERVICE), "{key}");
        assert!(is_gatewayhub_daemon_key(key), "{key} is a name the gatewayhub daemon verifies");
        assert!(
            !is_datahub_gatewayhub_client_key(key),
            "{key}: the datahub must never gain a gatewayhub name by the family filing"
        );
        assert!(!is_tradehub_pair_key(key) && !is_tradehub_daemon_key(key), "{key}");
        assert!(!is_datahub_pair_key(key) && !is_datahub_daemon_key(key), "{key}");
    }
    for index in [0, 1, 2, 3, 4, 9] {
        let key = PLATFORM_KEYS[index];
        assert!(
            !is_gatewayhub_daemon_key(key),
            "{key} is another service's and the gatewayhub never holds it"
        );
        assert!(
            !is_datahub_gatewayhub_client_key(key),
            "{key}: the datahub dials the gatewayhub with [5]/[6] and nothing else"
        );
    }
    // Exact counts, so a name added to either predicate fails here rather than silently widening a
    // hub-key probe.
    assert_eq!(PLATFORM_KEYS.iter().filter(|k| is_gatewayhub_daemon_key(k)).count(), 4);
    assert_eq!(PLATFORM_KEYS.iter().filter(|k| is_datahub_gatewayhub_client_key(k)).count(), 2);
    // Every platform key is in exactly one daemon's scope.
    for key in PLATFORM_KEYS {
        let owners = [
            is_tradehub_daemon_key(key),
            is_datahub_daemon_key(key),
            is_gatewayhub_daemon_key(key),
        ];
        assert_eq!(owners.iter().filter(|o| **o).count(), 1, "{key}: {owners:?}");
    }
    // The client scope is CONTAINED in the daemon's, so the datahub can never hold a gatewayhub
    // name the gatewayhub does not verify.
    for key in PLATFORM_KEYS {
        assert!(
            !is_datahub_gatewayhub_client_key(key) || is_gatewayhub_daemon_key(key),
            "{key}: a client name the daemon does not verify"
        );
    }
    // Narrow, exact and case-sensitive.
    for outsider in [
        String::new(),
        format!("{}_", PLATFORM_KEYS[5]),
        PLATFORM_KEYS[5].to_lowercase(),
        format!("{}_", PLATFORM_KEYS[7]),
        PLATFORM_KEYS[7].to_lowercase(),
        format!("VIKE_{}", "GATEWAYHUB_TRADE_KEYS"),
    ] {
        assert!(!is_gatewayhub_daemon_key(&outsider), "{outsider:?}");
        assert!(!is_datahub_gatewayhub_client_key(&outsider), "{outsider:?}");
        assert!(!is_platform_key(&outsider), "{outsider:?}");
    }
    for key in attribution_keys() {
        assert!(
            !is_gatewayhub_daemon_key(&key) && !is_datahub_gatewayhub_client_key(&key),
            "{key}"
        );
    }
}

/// **Wave 2's three names are APPENDED after the gatewayhub's pair, spelled as pinned, and filed
/// under the service that VERIFIES them.** The anchor for `[7]`/`[8]`, which have no second spelling,
/// and the table half of `[9]`, whose second spelling (`vike_hub_proto::auth::DATAHUB_TRADE_KEY_ENV`)
/// is held equal by `crates/vike-cli/tests/node_cli.rs`.
///
/// ⚠ COMPOSED names, for the reason the neighbouring tests give.
#[test]
fn the_wave_two_keys_are_appended_after_the_gatewayhub_pair_and_filed_under_their_verifier() {
    assert_eq!(PLATFORM_KEYS.len(), 10, "seven that existed, plus the three of wave 2");
    assert_eq!(PLATFORM_KEYS[7], format!("VIKE_{}", "GATEWAYHUB_TRADE_KEY"));
    assert_eq!(PLATFORM_KEYS[8], format!("VIKE_{}", "GATEWAYHUB_ADMIN_KEY"));
    assert_eq!(PLATFORM_KEYS[9], format!("VIKE_{}", "DATAHUB_TRADE_KEY"));
    // The index pins of the earlier waves did not move: the accessor still answers `[5]`/`[6]`.
    assert_eq!(gatewayhub_key_pair(), [PLATFORM_KEYS[5], PLATFORM_KEYS[6]]);
    assert_eq!(gatewayhub_trade_key(), PLATFORM_KEYS[7]);
    assert_eq!(datahub_trade_key(), PLATFORM_KEYS[9]);
    assert_eq!(platform_key_service(PLATFORM_KEYS[7]), Some(GATEWAYHUB_SERVICE));
    assert_eq!(platform_key_service(PLATFORM_KEYS[8]), Some(GATEWAYHUB_SERVICE));
    assert_eq!(platform_key_service(PLATFORM_KEYS[9]), Some(DATAHUB_SERVICE));
    // No key is shared across services, and no wave-2 name is anyone's attribution code.
    for key in &PLATFORM_KEYS[7..] {
        assert!(is_platform_key(key), "{key}");
        assert!(attribution_key_owner(key).is_none(), "{key}");
    }
}

/// **The predicate matrix: every predicate that can scope a `CredentialStore::hub_keys` read, over
/// all ten names, admits exactly the documented set.** The desktop's and the CLI's predicates
/// (`is_tradehub_pair_key`, `is_datahub_pair_key`) admit none of `[7]`, `[8]` and `[9]`; the two
/// tradehub CLIENT predicates are single names; the three DAEMON predicates partition the table.
///
/// The table is written by INDEX, so a reorder of `PLATFORM_KEYS` fails here and not in a daemon.
#[test]
fn the_predicate_matrix_admits_exactly_the_documented_names() {
    type Row = (&'static str, fn(&str) -> bool, &'static [usize]);
    let matrix: [Row; 9] = [
        ("is_platform_key", is_platform_key, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]),
        ("is_tradehub_pair_key", is_tradehub_pair_key, &[0, 1]),
        ("is_tradehub_daemon_key", is_tradehub_daemon_key, &[0, 1, 4]),
        ("is_datahub_pair_key", is_datahub_pair_key, &[2, 3]),
        ("is_datahub_daemon_key", is_datahub_daemon_key, &[2, 3, 9]),
        ("is_gatewayhub_daemon_key", is_gatewayhub_daemon_key, &[5, 6, 7, 8]),
        ("is_datahub_gatewayhub_client_key", is_datahub_gatewayhub_client_key, &[5, 6]),
        ("is_tradehub_gatewayhub_trade_client_key", is_tradehub_gatewayhub_trade_client_key, &[7]),
        ("is_tradehub_datahub_trade_client_key", is_tradehub_datahub_trade_client_key, &[9]),
    ];
    for (name, predicate, admitted) in matrix {
        for (index, key) in PLATFORM_KEYS.iter().enumerate() {
            assert_eq!(
                predicate(key),
                admitted.contains(&index),
                "{name} over [{index}] {key}: the admitted set is {admitted:?}"
            );
        }
    }
    // The desktop's and the CLI's scope holds none of the order-scope or admin keys.
    for index in [7, 8, 9] {
        let key = PLATFORM_KEYS[index];
        assert!(!is_tradehub_pair_key(key) && !is_datahub_pair_key(key), "{key}");
    }
}

/// The two uppercasings the workspace used before this table existed agree on every roster id,
/// which is what makes [`attribution_key`]'s switch from `to_ascii_uppercase` a no-op.
#[test]
fn the_two_uppercasings_agree_on_every_roster_venue() {
    for venue in VENUES {
        assert_eq!(venue.to_uppercase(), venue.to_ascii_uppercase(), "{venue}");
    }
}

/// **[`STORE_KEYS_OUTSIDE_THE_GRID`] is sorted, duplicate-free, and outside BOTH other tables** —
/// a name in the grid is settable already, and a hub key is never settable by hand, so a row that
/// overlapped either would be a second answer to a question one table already owns. Its agreement
/// with the settings REGISTRY is held where both can be seen, in
/// `crates/vike-cli/src/cmd/secrets/tests/set_tests.rs`'s `the_settable_list_and_the_registry_agree_both_ways`.
#[test]
fn the_store_keys_outside_the_grid_are_sorted_unique_and_disjoint() {
    let names: Vec<&str> = STORE_KEYS_OUTSIDE_THE_GRID.iter().map(|(n, _)| *n).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(names, sorted, "sorted by name and duplicate-free");
    let grid = attribution_keys();
    for (name, reader) in STORE_KEYS_OUTSIDE_THE_GRID {
        assert!(!reader.is_empty(), "{name}: every row names the crate that reads it");
        assert!(
            attribution_key_owner(name).is_none() && !grid.iter().any(|g| g == name),
            "{name} is a grid key"
        );
        assert!(!is_platform_key(name), "{name} is a hub key — never settable by hand");
        assert!(is_store_key_outside_the_grid(name), "{name}: the membership test disagrees");
    }
    assert!(!is_store_key_outside_the_grid(PLATFORM_KEYS[0]));
    assert!(!is_store_key_outside_the_grid(&grid[0]));
}
