//! THE ARMING PROJECTION (`venue_arming_under` / `venue_arming`) agrees with the real mount.

use std::collections::HashMap;

use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::registry::REGISTRY;

use super::*;
use crate::support::vars;

// ===========================================================================================
// THE ARMING PROJECTION (`venue_arming_under` / `venue_arming`) — what `vike-backend venues` and
// `crates/vike-mount/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts` read, so these gate
// that the projection cannot disagree with the mount.
//
// ⚠ The agreement is asserted against `make_engine_with_legs` ITSELF wherever a network-free
// observable exists for it, not against a restatement of the rows: the tier a mount reaches is
// visible offline only as PAPER-vs-not (the pre-connect budget refusal fires from
// `would_mount_live_under`), so that half is driven through the real mount, and the
// Live-vs-Demo half is driven against the SAME per-venue tier resolvers the arms call.
// ===========================================================================================

/// **THE projection's gate: for every roster venue, at every account tier, over the real
/// credential fixtures, the projection's PAPER-vs-not verdict is what the mount actually does.**
///
/// ⚠ **What this proves, stated exactly.** The agreement is BY CONSTRUCTION — `make_engine`
/// consults `would_mount_live_under`, which is `venue_arming_under` projected onto paper-vs-not —
/// so this test cannot catch the two functions disagreeing (they are one function). What it DOES
/// catch is the construction coming apart end-to-end: the mount consulting a different tier than
/// the account row it was handed, the seam moving back below the credential read, or the
/// projection answering at a tier other than the account's own. It drives the REAL
/// `make_engine_with_legs` with a REAL `MountPolicy` and reads its only offline observable, the
/// pre-connect budget refusal. The INDEPENDENT half — is the projected TIER the tier the arm
/// would dial — is [`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`], which
/// pins, per switched venue and key set, the tier the projection reaches.
///
/// ⚠ **The no-downgrade rule is asserted here over the whole roster** (decision 0119, the
/// owner's ruling): an account trades at EXACTLY its own tier, so every projected `effective` is
/// either `Paper` or the account's tier — never a lower non-paper tier (aster's or ibkr's old demo
/// fallback under `live`), and never a higher one.
///
/// Anti-vacuity is built in two ways: the armed maps are asserted to arm SOMETHING at their own
/// tier before the comparison runs, and every venue is driven with an EMPTY map too, so a
/// projection that had degenerated into "always paper" would still have to agree with a mount
/// that has not.
#[test]
fn the_arming_projection_agrees_with_the_real_mount_for_every_roster_venue() {
    use vike_config::VenueMode as Mode;

    let armed_for: std::collections::HashMap<&str, HashMap<String, String>> =
        live_arming_cases().iter().map(|(v, _, kv)| (*v, vars(kv))).collect();
    // ANTI-VACUITY: at least one fixture really does arm, or every agreement below is between
    // two functions that both always say paper.
    assert!(
        live_arming_cases().iter().any(|(v, tier, kv)| vike_mount::venue_arming_under(
            REGISTRY,
            v,
            &vars(kv),
            *tier
        )
        .0 != Mode::Paper),
        "no fixture arms anything — this test would prove nothing"
    );

    // ⚠ The THIRD map per venue, and it is the one that makes this test able to fail for the
    // venues it covers. Most `live_arming_cases` fixtures arm at the DEMO tier, so for those a
    // `live` account must reach PAPER (the no-downgrade rule) while a `demo` one arms — but only a
    // LIVE-tier map shows that a `demo` account never reaches the live key set.
    // binance/bybit/okx/hyperliquid's OWN rows in `live_arming_cases` are LIVE-tier keys already
    // (decision 0095), so those four create the divergence through `armed_for` alone;
    // `mainnet_arming_cases` is what still creates it for every OTHER row `live_arming_cases`
    // cannot — aster above all, whose `live_arming_cases` row is deliberately TESTNET-shaped.
    let mainnet_for: std::collections::HashMap<&str, HashMap<String, String>> =
        mainnet_arming_cases().iter().map(|(v, kv)| (*v, vars(kv))).collect();
    let empty = HashMap::new();
    for venue in vike_model::VENUES {
        for map in [
            armed_for.get(venue).unwrap_or(&empty),
            mainnet_for.get(venue).unwrap_or(&empty),
            &empty,
        ] {
            for tier in Mode::ALL {
                let (effective, block) = vike_mount::venue_arming_under(REGISTRY, venue, map, tier);

                // 1. THE NO-DOWNGRADE RULE: an account trades at exactly its own tier or not at
                //    all. A projection that promoted, or that fell back to a lower non-paper tier,
                //    would describe an account trading somewhere its row never named.
                assert!(
                    effective == Mode::Paper || effective == tier,
                    "{venue} @ {tier}: projected {effective} — an account trades at its own tier \
                     or on paper, never another"
                );
                // 2. `block` and `effective` cannot contradict, in ONE direction: a row held
                //    below its tier must carry a reason, and a CLEAR block must be at its tier.
                //
                //    ⚠ Deliberately not an `assert_eq!` of the two. The converse is false and
                //    correctly so: a `paper` tier reports `PaperTier` — at its tier, with
                //    something to say. A block is "what is holding this row where it is", which
                //    is information even when nothing is being refused.
                if effective < tier {
                    assert!(
                        !block.is_clear(),
                        "{venue} @ {tier}: held to {effective} with NO reason — the projection \
                             would render a demotion the operator cannot explain"
                    );
                }
                if block.is_clear() {
                    assert_eq!(
                        effective, tier,
                        "{venue} @ {tier}: a clear block must mean the row is at its tier"
                    );
                }
                // 3. THE agreement, through the REAL mount.
                let policy = account_at(venue, tier);
                let (out, live_set) = mount(venue, map, Some(&policy));
                let mount_is_live = match out {
                    Err(vike_mount::MountError::MissingRiskBudget { venue: named, .. }) => {
                        assert_eq!(named.as_str(), *venue, "the refusal must name the venue");
                        true
                    }
                    // ⚠ No catch-all `Err` arm: `MountError` has exactly one variant today, so
                    // one would be an unreachable pattern (`-D warnings`). A NEW variant makes
                    // this match non-exhaustive, which is a compile error naming this site —
                    // the right way round, since a new failure mode needs a decision here.
                    Ok(_) => {
                        assert!(
                            live_set.is_empty(),
                            "{venue} @ {tier}: a mount that armed live must have hit the \
                                 pre-connect budget refusal first"
                        );
                        false
                    }
                };
                assert_eq!(
                    effective != Mode::Paper,
                    mount_is_live,
                    "{venue} @ {tier}: the projection would say `{effective}` ({block:?}) while \
                         the mount {} — the projection exists precisely so these cannot differ",
                    if mount_is_live { "reaches its live arm" } else { "stays paper" }
                );
            }
        }
    }
}

/// The credential shapes that arm a venue at its **LIVE** tier — [`live_arming_cases`]'s
/// dangerous twin, and the only maps on which a `demo` account and a `live` one give DIFFERENT
/// answers for the venues whose `live_arming_cases` row is demo-shaped.
///
/// Only the five venues whose arm can reach live from a map: the four venues whose network is
/// their tier (a LIVE key set) and aster, which is switchless and picks its tier from WHICH key
/// set exists. Every other roster venue's arm hardcodes its demo endpoint, so there is no
/// live-tier map to write for it.
fn mainnet_arming_cases() -> &'static [(&'static str, &'static [(&'static str, &'static str)])] {
    &[
        ("binance", &[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")]),
        ("bybit", &[("BYBIT_LIVE_API_KEY", "k"), ("BYBIT_LIVE_API_SECRET", "s")]),
        (
            "okx",
            &[
                ("OKX_LIVE_API_KEY", "k"),
                ("OKX_LIVE_API_SECRET", "s"),
                ("OKX_LIVE_API_PASSPHRASE", "p"),
            ],
        ),
        ("hyperliquid", &[("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xkey")]),
        ("aster", &[("ASTER_LIVE_USER", "0xUser"), ("ASTER_LIVE_PRIVATE_KEY", "0xkey")]),
    ]
}

/// The boolean probe is the projection's own answer, not a parallel implementation — asserted
/// over the same matrix, because the pre-connect budget refusal rides on it and a divergence
/// there is a live-money defect rather than a cosmetic one.
#[test]
fn the_live_intent_probe_is_exactly_the_projection_above_paper() {
    use vike_config::VenueMode as Mode;
    let empty = HashMap::new();
    for (venue, _, kv) in live_arming_cases() {
        let armed = vars(kv);
        for map in [&armed, &empty] {
            for tier in Mode::ALL {
                assert_eq!(
                    vike_mount::would_mount_live_under(REGISTRY, venue, map, tier),
                    vike_mount::venue_arming_under(REGISTRY, venue, map, tier).0 != Mode::Paper,
                    "{venue} @ {tier}"
                );
            }
        }
    }
}

/// One `(venue, LIVE-tier keys, DEMO-tier keys)` row per switched venue (decision 0095: binance,
/// bybit, okx, hyperliquid) — deliberately its OWN table rather than a reuse of
/// [`live_arming_cases`], which for these four venues carries LIVE-tier keys only. Shared by the
/// pure-projection test ([`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`])
/// and its real-mount twin ([`the_ceiling_alone_chooses_the_network_through_the_real_mount_too`]) so
/// the two matrices cannot silently drift apart, and so BOTH tiers are independently driven
/// through the REAL mount for every switched venue — not only the pure projection.
struct SwitchedVenueTiers {
    venue: &'static str,
    live_keys: &'static [(&'static str, &'static str)],
    demo_keys: &'static [(&'static str, &'static str)],
}

fn switched_venue_tier_cases() -> &'static [SwitchedVenueTiers] {
    &[
        SwitchedVenueTiers {
            venue: "binance",
            live_keys: &[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")],
            demo_keys: &[("BINANCE_DEMO_API_KEY", "k"), ("BINANCE_DEMO_API_SECRET", "s")],
        },
        SwitchedVenueTiers {
            venue: "bybit",
            live_keys: &[("BYBIT_LIVE_API_KEY", "k"), ("BYBIT_LIVE_API_SECRET", "s")],
            demo_keys: &[("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")],
        },
        SwitchedVenueTiers {
            venue: "okx",
            live_keys: &[
                ("OKX_LIVE_API_KEY", "k"),
                ("OKX_LIVE_API_SECRET", "s"),
                ("OKX_LIVE_API_PASSPHRASE", "p"),
            ],
            demo_keys: &[
                ("OKX_DEMO_API_KEY", "k"),
                ("OKX_DEMO_API_SECRET", "s"),
                ("OKX_DEMO_API_PASSPHRASE", "p"),
            ],
        },
        SwitchedVenueTiers {
            venue: "hyperliquid",
            live_keys: &[("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xkey")],
            demo_keys: &[("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xkey")],
        },
    ]
}

/// **Decision 0095, restated by 0119: the account's tier alone chooses the network** for binance,
/// bybit, okx and hyperliquid, and the row says what is missing. No variable is consulted — the
/// map below carries none, and the process environment is not read on this path any more.
#[test]
fn the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing() {
    use vike_config::ArmingBlock as Block;
    use vike_config::VenueMode as Mode;

    for SwitchedVenueTiers { venue, live_keys, demo_keys } in switched_venue_tier_cases() {
        let venue = *venue;
        let (live, demo) = (vars(live_keys), vars(demo_keys));
        assert_eq!(
            vike_mount::venue_arming_under(REGISTRY, venue, &live, Mode::Live),
            (Mode::Live, Block::None),
            "{venue}"
        );
        assert_eq!(
            vike_mount::venue_arming_under(REGISTRY, venue, &demo, Mode::Live),
            (Mode::Paper, Block::LiveCredentialsAbsent),
            "{venue}: a `live` account with only demo keys is PAPER — a mainnet host is never \
             signed with demo keys"
        );
        assert_eq!(
            vike_mount::venue_arming_under(REGISTRY, venue, &live, Mode::Demo),
            (Mode::Paper, Block::NoCredentials),
            "{venue}: a `demo` account never reaches the LIVE key set"
        );
        assert_eq!(
            vike_mount::venue_arming_under(REGISTRY, venue, &demo, Mode::Demo),
            (Mode::Demo, Block::None),
            "{venue}"
        );
        let row = vike_config::VenueArming {
            venue: vike_config::roster_id(venue).expect("a roster venue"),
            label: AccountLabel::Default,
            tier: Mode::Live,
            effective: Mode::Paper,
            block: Block::LiveCredentialsAbsent,
            account_ids: vec![7],
        };
        assert!(row.why().contains("LIVE") && row.why().contains("paper"), "{}", row.why());
    }
    // deribit mounts through its own bridge and is testnet-only. Its arm holds a `live` request
    // to its demo endpoint (`DemoOnlyArm`), which used to TRADE demo under a `live` ceiling; the
    // no-downgrade rule makes a `live` deribit account PAPER, and only a `demo` one reaches demo.
    let deribit = vars(&[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")]);
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "deribit", &deribit, Mode::Live),
        (Mode::Paper, Block::LiveCredentialsAbsent),
        "a `live` deribit account must never trade its demo endpoint"
    );
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "deribit", &deribit, Mode::Demo),
        (Mode::Demo, Block::None)
    );
}

/// **The REAL-MOUNT twin of [`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`].**
///
/// That test proves the pure projection (`venue_arming_under`) agrees that "a `live` account with
/// only demo keys is PAPER" for the four switched venues — but proving the PROJECTION agrees with
/// itself is not the same evidence as proving the mount does. This test drives [`mount`] — the
/// real `make_engine_with_legs` — directly with [`switched_venue_tier_cases`]'s own LIVE- and
/// DEMO-tier maps, all four (venue, key-tier, account-tier) combinations, so that property is
/// witnessed by real code and not only by the function under test agreeing with itself.
#[test]
fn the_ceiling_alone_chooses_the_network_through_the_real_mount_too() {
    use vike_config::VenueMode as Mode;

    for SwitchedVenueTiers { venue, live_keys, demo_keys } in switched_venue_tier_cases() {
        let venue = *venue;
        let (live, demo) = (vars(live_keys), vars(demo_keys));

        // LIVE keys, LIVE account -> reaches the live path (the pre-connect budget refusal fires,
        // proving the mount classified this as live-intent rather than merely not-erroring).
        assert!(
            matches!(
                mount(venue, &live, Some(&account_at(venue, Mode::Live))).0,
                Err(vike_mount::MountError::MissingRiskBudget { .. })
            ),
            "{venue}: LIVE-tier keys for a `live` account must reach the live path"
        );

        // DEMO keys, LIVE account -> PAPER. THE safety property this table exists to guard,
        // witnessed through the real mount rather than the pure projection alone: a `live`
        // account never falls back to a venue's demo credentials.
        let (out, live_set) = mount(venue, &demo, Some(&account_at(venue, Mode::Live)));
        out.unwrap_or_else(|e| {
            panic!(
                "{venue}: a `live` account with only DEMO-tier keys must mount PAPER, not refuse \
                 to start: {e}"
            )
        });
        assert!(
            live_set.is_empty(),
            "{venue}: DEMO-tier keys armed a live exec client for a `live` account"
        );

        // LIVE keys, DEMO account -> PAPER (the real-mount twin of
        // `a_demo_tier_never_reaches_the_live_key_set`, over all four switched venues rather
        // than binance alone).
        let (out, live_set) = mount(venue, &live, Some(&account_at(venue, Mode::Demo)));
        out.unwrap_or_else(|e| {
            panic!(
                "{venue}: a `demo` account with only LIVE-tier keys must mount PAPER, not refuse \
                 to start: {e}"
            )
        });
        assert!(
            live_set.is_empty(),
            "{venue}: LIVE-tier keys armed an exec client for a `demo` account"
        );

        // DEMO keys, DEMO account -> reaches the demo path (still non-paper, still budget-gated —
        // the control that proves the two PAPER results above are the tier's doing and not a
        // fixture that never arms anything).
        assert!(
            matches!(
                mount(venue, &demo, Some(&account_at(venue, Mode::Demo))).0,
                Err(vike_mount::MountError::MissingRiskBudget { .. })
            ),
            "{venue}: DEMO-tier keys for a `demo` account must reach the demo path"
        );
    }
}

/// **A LIVE ACCOUNT NEVER TRADES DEMO, through the real mount, on the venues whose arm used to
/// fall back** (the owner's no-downgrade ruling on decision 0119). Aster's switchless chain and
/// deribit's testnet-only arm both resolved a DEMO session for a `live` request when only demo
/// keys were stored (`held_below_live`); an account whose row says `live` must mount PAPER with
/// `LiveCredentialsAbsent` instead, and the same keys under a `demo` row still arm — so the
/// refusal is the tier rule, not a broken fixture.
#[test]
fn a_live_account_whose_arm_would_bind_demo_mounts_paper_never_demo() {
    use vike_config::{ArmingBlock as Block, VenueMode as Mode};

    for (venue, demo_keys) in [
        ("aster", vars(&[("ASTER_TESTNET_USER", "0xUser"), ("ASTER_TESTNET_PRIVATE_KEY", "0xk")])),
        ("deribit", vars(&[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")])),
    ] {
        let live_account = account_at(venue, Mode::Live);
        let rows =
            vike_mount::venue_account_arming(REGISTRY, venue, &demo_keys, Some(&live_account));
        let row = rows.first().expect("the default account's row");
        assert_eq!(
            (row.tier, row.effective, row.block),
            (Mode::Live, Mode::Paper, Block::LiveCredentialsAbsent),
            "{venue}: a `live` account with only demo keys must project PAPER, never demo"
        );
        let (out, live_set) = mount(venue, &demo_keys, Some(&live_account));
        out.unwrap_or_else(|e| panic!("{venue}: a paper mount never refuses to start: {e}"));
        assert!(live_set.is_empty(), "{venue}: a `live` account bound its DEMO keys");

        // CONTROL: the same keys under a `demo` row reach the demo path.
        assert!(
            matches!(
                mount(venue, &demo_keys, Some(&account_at(venue, Mode::Demo))).0,
                Err(vike_mount::MountError::MissingRiskBudget { .. })
            ),
            "{venue}: the demo keys must arm a `demo` account, or the refusal above proves nothing"
        );
    }
}

/// A venue this BUILD cannot mount reports [`vike_config::ArmingBlock::FeatureAbsent`] rather
/// than "no credentials" — the row renders the disagreement between a portable `account` row and
/// a binary's compiled features instead of hiding it behind a credential complaint.
///
/// The default build this module runs in (`mount_roster.rs`'s crate-level `cfg`) registers ibkr,
/// fxcm and polymarket `FeatureAbsent`, so these assertions hold wherever it runs; each venue's
/// feature-on half is in `crates/vike-tradehub/tests/ibkr_mount.rs`,
/// `crates/vike-tradehub/tests/fxcm_mount.rs` and `crates/vike-tradehub/tests/polymarket_mount.rs`.
#[test]
fn a_venue_this_build_cannot_mount_says_so_instead_of_blaming_credentials() {
    use vike_config::ArmingBlock as Block;
    use vike_config::VenueMode as Mode;
    let empty = HashMap::new();

    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "polymarket", &empty, Mode::Live),
        (Mode::Paper, Block::FeatureAbsent),
    );

    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "ibkr", &empty, Mode::Live),
        (Mode::Paper, Block::FeatureAbsent),
    );
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "fxcm", &empty, Mode::Live),
        (Mode::Paper, Block::FeatureAbsent),
    );

    // ⚠ dukascopy answered `NoLiveArm` here until 2026-09-09 — a fact this test kept apart from
    // "a feature is off" precisely because the two look identical to an operator. It has an arm
    // now, so with an EMPTY map the honest block is `NoCredentials`: the venue is mountable and
    // this box simply has no login for it. Keeping the old answer would have been the very
    // confusion the comment above guards against, one step down the ladder.
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "dukascopy", &empty, Mode::Live),
        (Mode::Paper, Block::NoCredentials),
    );
}

/// [`vike_mount::venue_arming`] is the projection over the WHOLE roster: one row per venue, each
/// carrying the tier its `account` rows state and the answer the per-venue call gives.
///
/// ⚠ **With an empty credential map and no labelled row there is exactly one row per roster venue
/// — the DEFAULT account's** — which is the table this projection returned before it knew about
/// accounts at all. That equality is the whole "a box with no labelled accounts behaves exactly as
/// today" claim, asserted here at the projection rather than argued in prose.
#[test]
fn the_roster_projection_covers_every_venue_and_carries_its_tier() {
    use vike_config::{ArmingBlock as Block, VenueMode as Mode};
    let policy = vike_mount::MountPolicy::default()
        .with_account("bybit", &AccountLabel::Default, Mode::Live)
        .with_account("binance", &AccountLabel::Default, Mode::Demo);
    let rows = vike_mount::venue_arming(REGISTRY, &HashMap::new(), &policy);

    assert_eq!(rows.len(), vike_model::VENUES.len(), "one row per roster venue");
    let seen: Vec<&str> = rows.iter().map(|r| r.venue).collect();
    for venue in vike_model::VENUES {
        assert!(seen.contains(venue), "{venue} has no row");
    }
    for row in &rows {
        assert!(row.is_default_account(), "{}: an empty store names no second account", row.venue);
    }
    let by = |v: &str| rows.iter().find(|r| r.venue == v).expect("row").clone();
    assert_eq!(by("bybit").tier, Mode::Live);
    assert_eq!(by("binance").tier, Mode::Demo);
    assert_eq!(by("bybit").account_ids.len(), 1, "the row names the one account that states it");
    assert_eq!(by("binance").account_ids.len(), 1);
    assert_ne!(by("bybit").account_ids, by("binance").account_ids, "two rows, two ids");
    let okx = by("okx");
    assert_eq!(
        (okx.tier, okx.effective, okx.block),
        (Mode::Paper, Mode::Paper, Block::NoAccountRow),
        "a venue with no row keeps the safe default, and says why"
    );
    assert!(okx.account_ids.is_empty(), "no row, no id to name");
    // …and every row a row ARMS agrees with the per-venue call it is built from. (A paper row is
    // decided by the table before any bridge is asked, so its block names the table's cause.)
    for row in rows.iter().filter(|r| r.tier != Mode::Paper) {
        assert_eq!(
            (row.effective, row.block),
            vike_mount::venue_arming_under(REGISTRY, row.venue, &HashMap::new(), row.tier),
        );
    }
}

/// **A LABELLED account in the credential store gets a row of its own** — and, with no `account`
/// row naming its label, that row is PAPER with the block that says no row states a tier. Given an
/// ACTIVE row of its own it arms exactly as the default account does.
#[test]
fn a_labelled_account_in_the_store_gets_its_own_unarmed_row() {
    use vike_config::{ArmingBlock as Block, VenueMode as Mode};
    // `Demo`, not `Live`: `bybit_store_with_alt` plants DEMO-tier keys only, and a `live` account
    // never binds demo keys — this test's subject is the LABELLED row, not the tier, so the tier
    // that matches the fixture's credentials is the one that keeps the default row's "it IS
    // armed" precondition true.
    let policy = vike_mount::MountPolicy::default().with_account(
        "bybit",
        &AccountLabel::Default,
        Mode::Demo,
    );
    let vars = bybit_store_with_alt();

    let rows = vike_mount::venue_account_arming(REGISTRY, "bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "the default account and ALT: {rows:?}");
    assert!(rows[0].is_default_account(), "the default account sorts FIRST");
    assert_ne!(rows[0].effective, Mode::Paper, "its credentials arm it, exactly as before");

    let alt = &rows[1];
    assert_eq!(alt.label.text(), Some("ALT"));
    assert_eq!(alt.effective, Mode::Paper, "a labelled account is not armed by the default's row");
    assert_eq!(alt.block, Block::NoAccountRow);
    assert!(alt.account_ids.is_empty(), "no row states ALT's tier, so there is no id to name");
    assert_eq!(alt.route_key(), "bybit#ALT");
    assert_eq!(rows[0].route_key(), "bybit", "the default account's routing does not move");

    // …and an active `ALT` row of its own arms it at that row's tier.
    let alt_label = AccountLabel::parse("ALT").expect("a legal label");
    let both = policy.with_account("bybit", &alt_label, Mode::Demo);
    let rows = vike_mount::venue_account_arming(REGISTRY, "bybit", &vars, Some(&both));
    assert_eq!(
        (rows[1].tier, rows[1].effective, rows[1].block),
        (Mode::Demo, Mode::Demo, Block::None),
        "ALT's own active row arms it: {rows:?}"
    );
    assert_eq!(rows[1].account_ids.len(), 1);
}

// ⚠ **THE HEADLINE — two armed accounts of one venue, and the SYMBOL reaching no arming
// decision — is asserted in `crates/vike-tradehub/tests/shared_book_report.rs`**, not here, and
// the reason is a gate rather than a preference: `crates/vike-ops/tests/settings_secrets/settings_registry.rs`
// HARVESTS STRING LITERALS out of `src/` to find undeclared environment reads, and a fixture
// planting `HYPERLIQUID_DEMO_ACCOUNT_ADDRESS__ALT` here reads to that scanner as this library
// reading a variable nothing declares. A `tests/` file is outside its scan by construction, so
// a credential-shaped fixture belongs there — which is also where the rule's own unit tests
// live (`crates/vike-config/tests/shared_book_table.rs`).
//
// The test this replaced was `two_armed_accounts_on_one_symbol_refuse_the_labelled_one`, and it
// asserted the opposite of what is true: long BTC on the default account and short BTC on `ALT`
// is an ordinary spread, and the two accounts are two wallets holding two positions.
