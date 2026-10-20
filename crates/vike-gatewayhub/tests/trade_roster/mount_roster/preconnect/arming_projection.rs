//! THE ARMING PROJECTION (`venue_arming_under` / `venue_account_arming` / `venue_arming`) agrees
//! with the real start.

use std::collections::{HashMap, HashSet};

use vike_config::ArmingBlock;

use vike_gatewayhub::registry::trade::REGISTRY;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::AccountCredentials;

use super::*;
use crate::account_book::{keyed_policy, with_keyed_account, with_keyed_row};

// ===========================================================================================
// THE ARMING PROJECTION — what `vike-backend venues` and
// `crates/vike-tradehub-core/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts` read, so these gate
// that the projection cannot disagree with the start.
//
// ⚠ The agreement is asserted against `make_engine_with_legs` ITSELF wherever a network-free
// observable exists for it, not against a restatement of the rows: the tier a start reaches is
// visible offline as one of three outcomes — the pre-connect budget refusal (live intent, fired
// from the arming probe), a paper engine (a `paper` account), or NOT RUNNING with its block
// (decision 0156) — so that half is driven through the real start, and the Live-vs-Demo half is
// driven against the SAME per-venue resolvers the arms call.
// ===========================================================================================

/// What a real start did, as far as an offline test can see it.
#[derive(Debug, PartialEq)]
enum Mounted {
    /// It reached its live arm (the pre-connect budget refusal fired, naming the venue).
    Live,
    /// It built the paper simulator: a `paper` account.
    Paper,
    /// The account does not run, with this block.
    NotRunning(ArmingBlock),
}

/// Read a start's result as [`Started`].
fn classify(
    venue: &str,
    tier: AccountTier,
    out: Result<vike_tradehub_core::EngineAndRecon, vike_tradehub_core::EngineError>,
    live_set: &HashSet<String>,
) -> Mounted {
    match out {
        Err(vike_tradehub_core::EngineError::MissingRiskBudget { venue: named, .. }) => {
            assert_eq!(named.as_str(), venue, "the refusal must name the venue");
            Mounted::Live
        }
        Err(vike_tradehub_core::EngineError::NotRunning(stopped)) => {
            assert!(live_set.is_empty(), "{venue} @ {tier}: a stopped account armed live");
            Mounted::NotRunning(stopped.block)
        }
        // These starts carry no run profile, so the venue-owned-field refusal cannot arise. ⚠ The
        // arm is the decision a NEW `EngineError` variant forces here: it fails this test.
        Err(other) => panic!("{venue} @ {tier}: unexpected mount error: {other}"),
        Ok((engine, _)) => {
            assert!(
                live_set.is_empty(),
                "{venue} @ {tier}: a mount that armed live must have hit the pre-connect budget \n                 refusal first"
            );
            assert_eq!(engine.tier, AccountTier::Paper, "{venue} @ {tier}");
            Mounted::Paper
        }
    }
}

/// The DEFAULT account's row of the projection `make_engine_accounts` selects with
/// (`vike_tradehub_core::venue_account_arming`), over `policy` — `(effective, block)`.
fn default_row(
    venue: &str,
    policy: &vike_tradehub_core::AdapterPolicy,
) -> (Option<AccountTier>, vike_config::ArmingBlock) {
    let rows = crate::local::rows(REGISTRY, venue, &HashMap::new(), Some(policy));
    let row = rows.iter().find(|r| r.is_default_account()).expect("the default account's row");
    (row.effective, row.block)
}

/// **THE projection's gate: for every roster venue, at every account tier, over the real
/// credential fixtures, the projection's verdict — runs, runs on paper, or does not run — is what the start actually does.**
///
/// ⚠ **What this proves, stated exactly.** The agreement is BY CONSTRUCTION — `make_engine`
/// consults `account_arming_under`, which `venue_account_arming` projects — so this test cannot
/// catch the two functions disagreeing (they are one function). What it DOES catch is the
/// construction coming apart end-to-end: the start consulting a different tier than the account
/// row it was handed, the seam moving back below the credential read, the start reading a
/// different account's credentials than the row's, or the projection answering at a tier other
/// than the account's own. It drives the REAL `make_engine_with_legs` with a REAL `AdapterPolicy`
/// (the account's row holding its fields) and reads its only offline observable, the pre-connect
/// budget refusal. The INDEPENDENT half — is the projected TIER the tier the arm would dial — is
/// [`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`], which pins, per
/// switched venue, the tier the projection reaches.
///
/// ⚠ **The no-downgrade rule is asserted here over the whole roster** (decision 0119, the
/// owner's ruling): an account trades at EXACTLY its own tier, so every projected `effective` is
/// either `None` (it does not run) or the account's tier — never a lower non-paper tier, and never
/// a higher one.
///
/// ⚠ **The policy-less probe is held to the same answer** (`vike_tradehub_core::venue_arming_under`,
/// handed the SAME fields as the account's credentials) for every venue whose arm reads nothing
/// but those credentials and the tier — every venue but [`ROW_RESOLVED`]'s, whose arm resolves its
/// account ROW out of the table a policy-less probe does not carry.
///
/// Anti-vacuity is built in two ways: the armed fixtures are asserted to arm SOMETHING at their own
/// tier before the comparison runs, and every venue is driven holding NOTHING too, so a projection
/// that had degenerated into "never runs" would still have to agree with a start that has not.
#[test]
fn the_arming_projection_agrees_with_the_real_start_for_every_roster_venue() {
    use vike_model::AccountTier as Mode;

    let armed_for: HashMap<&str, &[(&str, &str)]> =
        live_arming_cases().iter().map(|(v, _, kv)| (*v, *kv)).collect();
    // ANTI-VACUITY: at least one fixture really does arm, or every agreement below is between
    // two functions that both always say "does not run".
    assert!(
        live_arming_cases().iter().any(|(v, tier, kv)| default_row(v, &keyed_policy(v, *tier, kv))
            .0
            .is_some_and(|t| t != Mode::Paper)),
        "no fixture arms anything — this test would prove nothing"
    );

    // ⚠ There is no tier-shaped "mainnet twin" fixture any more: an account's fields carry no tier,
    // so the SAME set is driven at EVERY tier below, and the divergence between a `demo` account
    // and a `live` one comes from the tier loop alone (decision 0156 item 6).
    let nothing: &[(&str, &str)] = &[];
    for venue in vike_model::VENUES {
        for kv in [armed_for.get(venue).copied().unwrap_or(nothing), nothing] {
            for tier in Mode::ALL {
                let policy = keyed_policy(venue, tier, kv);
                let (effective, block) = default_row(venue, &policy);

                // 1. THE NO-DOWNGRADE RULE: an account trades at exactly its own tier or not at
                //    all. A projection that promoted, or that fell back to a lower non-paper tier,
                //    would describe an account trading somewhere its row never named.
                assert!(
                    effective.is_none_or(|e| e == tier),
                    "{venue} @ {tier}: projected {effective:?} — an account trades at its own tier \
                     or does not run, never another"
                );
                // 2. `block` and `effective` cannot contradict, in ONE direction: a row held
                //    below its tier must carry a reason, and a CLEAR block must be at its tier.
                //
                //    ⚠ Deliberately not an `assert_eq!` of the two. The converse is false and
                //    correctly so: a `paper` tier reports `PaperTier` — at its tier, with
                //    something to say. A block is "what is holding this row where it is", which
                //    is information even when nothing is being refused.
                if effective != Some(tier) {
                    assert!(
                        !block.is_clear(),
                        "{venue} @ {tier}: held to {effective:?} with NO reason — the projection \
                             would render a demotion the operator cannot explain"
                    );
                }
                if block.is_clear() {
                    assert_eq!(
                        effective,
                        Some(tier),
                        "{venue} @ {tier}: a clear block must mean the row is at its tier"
                    );
                }
                // 3. THE agreement, through the REAL start: the projection's `effective` is `None`
                //    (the account does not run, with this block), `Paper` (the simulator) or a tier
                //    above it (the live arm, reached as the pre-connect budget refusal).
                let (out, live_set) = start_adapter(venue, Some(&policy));
                let expected = match effective {
                    None => Mounted::NotRunning(block),
                    Some(Mode::Paper) => Mounted::Paper,
                    Some(_) => Mounted::Live,
                };
                assert_eq!(
                    classify(venue, tier, out, &live_set),
                    expected,
                    "{venue} @ {tier}: the projection would say `{effective:?}` ({block:?}) — the \
                     projection exists precisely so it cannot differ from the mount"
                );
                // 4. …and the policy-less probe, handed the SAME fields, says the same.
                if !ROW_RESOLVED.contains(venue) {
                    assert_eq!(
                        projected(venue, tier, kv),
                        (effective, block),
                        "{venue} @ {tier}: the policy-less probe handed the account's own fields \
                         must answer what the account's row answers"
                    );
                }
            }
        }
    }
}

/// The boolean probe is the projection's own answer, not a parallel implementation — asserted
/// over the same matrix, because the pre-connect budget refusal rides on it and a divergence
/// there is a live-money defect rather than a cosmetic one.
#[test]
fn the_live_intent_probe_is_exactly_the_projection_above_paper() {
    use vike_model::AccountTier as Mode;
    let nothing: &[(&str, &str)] = &[];
    for (venue, _, kv) in live_arming_cases() {
        for fields in [*kv, nothing] {
            for tier in Mode::ALL {
                assert_eq!(
                    probe(venue, tier, fields),
                    projected(venue, tier, fields).0.is_some_and(|t| t != Mode::Paper),
                    "{venue} @ {tier}"
                );
            }
        }
    }
}

/// One `(venue, the account's key set)` row per venue whose network IS its account's tier
/// (decision 0095: binance, bybit, okx, hyperliquid; aster, whose arm follows the same rule since
/// decision 0156 item 6: its network is its account's tier and nothing falls back below it). Shared by the pure-projection test
/// ([`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`]) and its real-mount
/// twin ([`the_ceiling_alone_chooses_the_network_through_the_real_start_too`]) so the two cannot
/// silently drift apart, and so BOTH tiers are independently driven through the REAL start for
/// every switched venue — not only the pure projection.
fn switched_venue_cases() -> &'static [(&'static str, &'static [(&'static str, &'static str)])] {
    &[
        ("binance", &[("API_KEY", "k"), ("API_SECRET", "s")]),
        ("bybit", &[("API_KEY", "k"), ("API_SECRET", "s")]),
        ("okx", &[("API_KEY", "k"), ("API_SECRET", "s"), ("API_PASSPHRASE", "p")]),
        ("hyperliquid", &[("PRIVATE_KEY", "0xkey")]),
        ("aster", &[("USER", "0xUser"), ("PRIVATE_KEY", "0xkey")]),
    ]
}

/// **Decision 0095, restated by 0119 and 0156 item 6: the account's tier alone chooses the
/// network** for the switched venues, and the row says what is missing. The SAME key set arms the
/// LIVE network under a `live` account and the DEMO one under a `demo` account — the fields carry
/// no tier, so the tier the row states is the only thing that chose. No variable is consulted —
/// the deployment map below carries none, and the process environment is not read on this path.
#[test]
fn the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing() {
    use vike_config::ArmingBlock as Block;
    use vike_model::AccountTier as Mode;

    for (venue, keys) in switched_venue_cases() {
        let venue = *venue;
        assert_eq!(projected(venue, Mode::Live, keys), (Some(Mode::Live), Block::None), "{venue}");
        assert_eq!(
            projected(venue, Mode::Demo, keys),
            (Some(Mode::Demo), Block::None),
            "{venue}: the SAME key set under a `demo` account binds the DEMO network — the tier \
             chose, not the keys"
        );
        assert_eq!(
            projected(venue, Mode::Live, &[]),
            (None, Block::LiveCredentialsAbsent),
            "{venue}: a `live` account holding no key set does not run — it falls back to no other \
             tier and no other account's keys"
        );
        assert_eq!(
            projected(venue, Mode::Demo, &[]),
            (None, Block::NoCredentials),
            "{venue}: a `demo` account holding nothing is the ordinary unconfigured state"
        );
        let row = vike_config::VenueArming {
            venue: vike_model::venues::roster_id(venue).expect("a roster venue"),
            label: AccountLabel::Default,
            tier: Some(Mode::Live),
            effective: None,
            block: Block::LiveCredentialsAbsent,
            account_ids: vec![7],
        };
        assert!(row.why().contains("LIVE") && row.why().contains("does NOT RUN"), "{}", row.why());
    }
    // deribit starts through its own bridge and is testnet-only. A `live` deribit account holding
    // its keys is refused by NAME (`LiveTierNotWired`: present and unusable — the arm binds only
    // its demo endpoint), never the demo session it once traded under a `live` ceiling; only a
    // `demo` account reaches demo.
    let deribit: &[(&str, &str)] = &[("API_KEY", "k"), ("API_SECRET", "s")];
    assert_eq!(
        projected("deribit", Mode::Live, deribit),
        (None, Block::LiveTierNotWired),
        "a `live` deribit account must never trade its demo endpoint"
    );
    assert_eq!(projected("deribit", Mode::Demo, deribit), (Some(Mode::Demo), Block::None));
}

/// **The REAL-MOUNT twin of [`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`].**
///
/// That test proves the pure projection (`venue_arming_under`) — but proving the PROJECTION agrees
/// with itself is not the same evidence as proving the start does. This test drives [`start_adapter`] — the
/// real `make_engine_with_legs` — with [`switched_venue_cases`]' own key sets, under a `live` and a
/// `demo` account, and with each account holding NOTHING while a labelled account of the OTHER tier
/// beside it holds the set, so the safety property is witnessed by real code: an account signs
/// with its own key set, at its own tier, or not at all.
#[test]
fn the_ceiling_alone_chooses_the_network_through_the_real_start_too() {
    use vike_model::AccountTier as Mode;
    let alt = AccountLabel::parse("ALT").expect("a legal label");

    for (venue, keys) in switched_venue_cases() {
        let venue = *venue;

        // The key set under either tier's account reaches that tier's path (the pre-connect
        // budget refusal fires, proving the start classified this as live-intent rather than
        // merely not-erroring) — the controls for the two NOT RUNNING results below.
        for tier in [Mode::Live, Mode::Demo] {
            assert!(
                matches!(
                    start_adapter(venue, Some(&keyed_policy(venue, tier, keys))).0,
                    Err(vike_tradehub_core::EngineError::MissingRiskBudget { .. })
                ),
                "{venue}: its key set held by a `{tier}` account must reach the `{tier}` path"
            );
        }

        // A `live` account holding NOTHING beside a `demo` account holding the set -> NOT RUNNING.
        // THE safety property, witnessed through the real start: a `live` account never falls back
        // to another account's (demo) credentials.
        let live_bare =
            with_keyed_account(keyed_policy(venue, Mode::Live, &[]), venue, &alt, Mode::Demo, keys);
        let (out, live_set) = start_adapter(venue, Some(&live_bare));
        let stopped = not_running(venue, out);
        assert_eq!(
            stopped.block,
            ArmingBlock::LiveCredentialsAbsent,
            "{venue}: a `live` account holding no key set of its own does not run"
        );
        assert!(
            live_set.is_empty(),
            "{venue}: a `live` account armed off a `demo` account's key set"
        );

        // …and the mirror: a `demo` account holding NOTHING beside a `live` account holding the
        // set -> NOT RUNNING. A `demo` account never reaches a `live` account's keys.
        let demo_bare =
            with_keyed_account(keyed_policy(venue, Mode::Demo, &[]), venue, &alt, Mode::Live, keys);
        let (out, live_set) = start_adapter(venue, Some(&demo_bare));
        let stopped = not_running(venue, out);
        assert_eq!(
            stopped.block,
            ArmingBlock::NoCredentials,
            "{venue}: a `demo` account holding no key set of its own does not run"
        );
        assert!(
            live_set.is_empty(),
            "{venue}: a `demo` account armed off a `live` account's key set"
        );
    }
}

/// **A LIVE ACCOUNT NEVER TRADES DEMO, through the real start, on the venues whose arm used to
/// fall back** (the owner's no-downgrade ruling on decision 0119). deribit's testnet-only arm
/// resolved a DEMO session for a `live` request (`held_below_live`); a `live` deribit account
/// holding its keys does NOT RUN, with `LiveTierNotWired`, instead, and the same keys under a `demo`
/// row still arm — so the refusal is the tier rule, not a broken fixture. Aster's testnet fallback
/// is gone with its key-set chain (decision 0156 item 6): a `live` aster account holding no pair
/// of its own does not run, with `LiveCredentialsAbsent`, whatever a `demo` account beside it holds.
#[test]
fn a_live_account_whose_arm_would_bind_demo_starts_paper_never_demo() {
    use vike_config::ArmingBlock as Block;
    use vike_model::AccountTier as Mode;

    let deribit: &[(&str, &str)] = &[("API_KEY", "k"), ("API_SECRET", "s")];
    let live_account = keyed_policy("deribit", Mode::Live, deribit);
    let rows = crate::local::rows(REGISTRY, "deribit", &HashMap::new(), Some(&live_account));
    let row = rows.first().expect("the default account's row");
    assert_eq!(
        (row.tier, row.effective, row.block),
        (Some(Mode::Live), None, Block::LiveTierNotWired),
        "deribit: a `live` account holding its keys must not run, never on demo"
    );
    let (out, live_set) = start_adapter("deribit", Some(&live_account));
    let stopped = not_running("deribit", out);
    assert_eq!(stopped.block, Block::LiveTierNotWired, "deribit: a `live` account does not run");
    assert!(live_set.is_empty(), "deribit: a `live` account bound its DEMO endpoint");
    // CONTROL: the same keys under a `demo` row reach the demo path.
    assert!(
        matches!(
            start_adapter("deribit", Some(&keyed_policy("deribit", Mode::Demo, deribit))).0,
            Err(vike_tradehub_core::EngineError::MissingRiskBudget { .. })
        ),
        "deribit: the keys must arm a `demo` account, or the refusal above proves nothing"
    );

    let pair: &[(&str, &str)] = &[("USER", "0xUser"), ("PRIVATE_KEY", "0xk")];
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let beside =
        with_keyed_account(keyed_policy("aster", Mode::Live, &[]), "aster", &alt, Mode::Demo, pair);
    let rows = crate::local::rows(REGISTRY, "aster", &HashMap::new(), Some(&beside));
    let default = rows.iter().find(|r| r.is_default_account()).expect("the default account's row");
    assert_eq!(
        (default.tier, default.effective, default.block),
        (Some(Mode::Live), None, Block::LiveCredentialsAbsent),
        "aster: a `live` account holding no pair does not run, never on the demo account's"
    );
    let (out, live_set) = start_adapter("aster", Some(&beside));
    let stopped = not_running("aster", out);
    assert_eq!(
        stopped.block,
        Block::LiveCredentialsAbsent,
        "aster: a `live` account holding no pair"
    );
    assert!(live_set.is_empty(), "aster: a `live` account bound a `demo` account's pair");
    // CONTROL: the pair arms the `demo` account that holds it.
    let alt_row = rows.iter().find(|r| r.label == alt).expect("ALT's row");
    assert_eq!(
        (alt_row.tier, alt_row.effective, alt_row.block),
        (Some(Mode::Demo), Some(Mode::Demo), Block::None),
        "aster: the pair must arm the account it belongs to, or the refusal above proves nothing"
    );
}

/// A venue this BUILD cannot start reports [`vike_config::ArmingBlock::FeatureAbsent`] rather
/// than "no credentials" — the row renders the disagreement between a portable `account` row and
/// a binary's compiled features instead of hiding it behind a credential complaint.
///
/// The default build this module runs in (`mount_roster.rs`'s crate-level `cfg`) registers ibkr,
/// fxcm and polymarket `FeatureAbsent`, so these assertions hold wherever it runs; each venue's
/// feature-on half is in `crates/vike-gatewayhub/tests/trade_roster/ibkr_adapter.rs`,
/// `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` and `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`.
#[test]
fn a_venue_this_build_cannot_start_says_so_instead_of_blaming_credentials() {
    use vike_config::ArmingBlock as Block;
    use vike_model::AccountTier as Mode;
    let empty = HashMap::new();
    let none = AccountCredentials::none;

    assert_eq!(
        vike_gatewayhub::trade::adapters::arming::venue_arming_under(
            REGISTRY,
            "polymarket",
            &empty,
            none(),
            Mode::Live
        ),
        (None, Block::FeatureAbsent),
    );

    assert_eq!(
        vike_gatewayhub::trade::adapters::arming::venue_arming_under(
            REGISTRY,
            "ibkr",
            &empty,
            none(),
            Mode::Live
        ),
        (None, Block::FeatureAbsent),
    );
    assert_eq!(
        vike_gatewayhub::trade::adapters::arming::venue_arming_under(
            REGISTRY,
            "fxcm",
            &empty,
            none(),
            Mode::Live
        ),
        (None, Block::FeatureAbsent),
    );

    // ⚠ dukascopy answered `NoLiveArm` here until 2026-09-09 — a fact this test kept apart from
    // "a feature is off" precisely because the two look identical to an operator. It has an arm
    // now, so for an account row holding no login the honest block is `NoCredentials`: the venue
    // is startable and this box simply has no login for it. Keeping the old answer would have been
    // the very confusion the comment above guards against, one step down the ladder. (Asked over
    // the account's ROW: dukascopy's arm resolves its account out of the `account` table.)
    assert_eq!(
        default_row("dukascopy", &account_at("dukascopy", Mode::Live)),
        (None, Block::NoCredentials),
    );
}

/// [`vike_tradehub_core::venue_arming`] is the projection over the WHOLE roster: one row per venue, each
/// carrying the tier its `account` rows state and the answer the per-venue call gives.
///
/// ⚠ **With no credentials and no labelled row there is exactly one row per roster venue — the
/// DEFAULT account's** — which is the table this projection returned before it knew about
/// accounts at all. That equality is the whole "a box with no labelled accounts behaves exactly as
/// today" claim, asserted here at the projection rather than argued in prose.
#[test]
fn the_roster_projection_covers_every_venue_and_carries_its_tier() {
    use vike_config::ArmingBlock as Block;
    use vike_model::AccountTier as Mode;
    let policy = vike_tradehub_core::AdapterPolicy::default()
        .with_account("bybit", &AccountLabel::Default, Mode::Live)
        .with_account("binance", &AccountLabel::Default, Mode::Demo);
    let rows = crate::local::all_rows(REGISTRY, &HashMap::new(), &policy);

    assert_eq!(rows.len(), vike_model::VENUES.len(), "one row per roster venue");
    let seen: Vec<&str> = rows.iter().map(|r| r.venue).collect();
    for venue in vike_model::VENUES {
        assert!(seen.contains(venue), "{venue} has no row");
    }
    for row in &rows {
        assert!(row.is_default_account(), "{}: an empty store names no second account", row.venue);
    }
    let by = |v: &str| rows.iter().find(|r| r.venue == v).expect("row").clone();
    assert_eq!(by("bybit").tier, Some(Mode::Live));
    assert_eq!(by("binance").tier, Some(Mode::Demo));
    assert_eq!(by("bybit").account_ids.len(), 1, "the row names the one account that states it");
    assert_eq!(by("binance").account_ids.len(), 1);
    assert_ne!(by("bybit").account_ids, by("binance").account_ids, "two rows, two ids");
    let okx = by("okx");
    assert_eq!(
        (okx.tier, okx.effective, okx.block),
        (None, None, Block::NoAccountRow),
        "a venue with no row keeps the safe default, and says why"
    );
    assert!(okx.account_ids.is_empty(), "no row, no id to name");
    // …and every row a row ARMS agrees with the per-venue call it is built from. (A paper row is
    // decided by the table before any bridge is asked, so its block names the table's cause.)
    for (row, tier) in
        rows.iter().filter_map(|r| r.tier.filter(|t| *t != Mode::Paper).map(|t| (r, t)))
    {
        assert_eq!(
            (row.effective, row.block),
            vike_gatewayhub::trade::adapters::arming::venue_arming_under(
                REGISTRY,
                row.venue,
                &HashMap::new(),
                AccountCredentials::none(),
                tier
            ),
        );
    }
}

/// **A LABELLED account ROW gets an arming row of its own** — and it is armed ONLY by the fields
/// filed against it: switched off it reads `AccountInactive`; active and holding nothing it is
/// `NoCredentials` with the DEFAULT account's keys right beside it; holding its own fields it arms
/// exactly as the default account does.
#[test]
fn a_labelled_account_gets_its_own_row_armed_only_by_its_own_fields() {
    use vike_config::ArmingBlock as Block;
    use vike_model::AccountTier as Mode;
    // `Demo` for both accounts: this test's subject is the LABELLED row, not the tier.
    let default_keys: &[(&str, &str)] = &[("API_KEY", "k"), ("API_SECRET", "s")];
    let alt_keys: &[(&str, &str)] = &[("API_KEY", "k2"), ("API_SECRET", "s2")];
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let default_armed = keyed_policy("bybit", Mode::Demo, default_keys);

    // ALT's row INACTIVE, holding its own keys.
    let switched_off = with_keyed_row(
        default_armed.clone(),
        vike_secrets::Account {
            id: 2,
            venue: "bybit".to_string(),
            tier: Mode::Demo,
            label: Some("ALT".to_string()),
            venue_account_id: None,
            parent_id: None,
            active: false,
            last_verified_at: None,
            max_exposure: None,
            leverage: None,
        },
        alt_keys,
    );
    let rows = crate::local::rows(REGISTRY, "bybit", &HashMap::new(), Some(&switched_off));
    assert_eq!(rows.len(), 2, "the default account and ALT: {rows:?}");
    assert!(rows[0].is_default_account(), "the default account sorts FIRST");
    assert!(
        rows[0].effective.is_some_and(|t| t != Mode::Paper),
        "its own keys arm it, exactly as before"
    );
    let alt_row = &rows[1];
    assert_eq!(alt_row.label.text(), Some("ALT"));
    assert_eq!(
        (alt_row.effective, alt_row.block),
        (None, Block::AccountInactive),
        "a switched-off labelled account is listed, and arms nothing"
    );
    assert!(alt_row.account_ids.is_empty(), "no ACTIVE row states ALT's tier, so no id to name");
    assert_eq!(alt_row.route_key(), "bybit#ALT");
    assert_eq!(rows[0].route_key(), "bybit", "the default account's routing does not move");

    // An ACTIVE `ALT` row holding NOTHING is not armed by the default account's keys beside it.
    let bare = with_keyed_account(default_armed.clone(), "bybit", &alt, Mode::Demo, &[]);
    let rows = crate::local::rows(REGISTRY, "bybit", &HashMap::new(), Some(&bare));
    assert_eq!(
        (rows[1].tier, rows[1].effective, rows[1].block),
        (Some(Mode::Demo), None, Block::NoCredentials),
        "ALT armed off the DEFAULT account's keys: {rows:?}"
    );
    assert_eq!(rows[1].account_ids.len(), 1);

    // …and ALT's own fields on its own active row arm it at that row's tier.
    let both = with_keyed_account(default_armed, "bybit", &alt, Mode::Demo, alt_keys);
    let rows = crate::local::rows(REGISTRY, "bybit", &HashMap::new(), Some(&both));
    assert_eq!(
        (rows[1].tier, rows[1].effective, rows[1].block),
        (Some(Mode::Demo), Some(Mode::Demo), Block::None),
        "ALT's own active row and fields arm it: {rows:?}"
    );
    assert_eq!(rows[1].account_ids.len(), 1);
}

// ⚠ **THE HEADLINE — two armed accounts of one venue, and the SYMBOL reaching no arming
// decision — is asserted in `crates/vike-gatewayhub/tests/trade_roster/shared_book_report.rs`**, not here, and
// the reason was a gate rather than a preference: `crates/vike-ops/tests/settings_secrets/settings_registry.rs`
// HARVESTS STRING LITERALS out of `src/` to find undeclared environment reads, and a
// credential-shaped fixture in this suite's old home (`vike-tradehub-core`'s `src/`) read to that scanner
// as the library reading a variable nothing declares. A `tests/` file is outside its scan by
// construction, so a credential-shaped fixture belongs there — which is also where the rule's own
// unit tests live (`crates/vike-config/tests/shared_book_table.rs`).
//
// The test this replaced was `two_armed_accounts_on_one_symbol_refuse_the_labelled_one`, and it
// asserted the opposite of what is true: long BTC on the default account and short BTC on `ALT`
// is an ordinary spread, and the two accounts are two wallets holding two positions.
