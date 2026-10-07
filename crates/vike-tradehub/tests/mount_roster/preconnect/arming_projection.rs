//! THE ARMING PROJECTION (`venue_arming_under` / `venue_arming`) agrees with the real mount.

use std::collections::HashMap;

use vike_config::VenuePolicy;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::registry::REGISTRY;

use super::*;
use crate::support::vars;

// ===========================================================================================
// THE ARMING PROJECTION (`venue_arming_under` / `venue_arming`) — the Data Manager's Venues
// tab reads its Effective column from here, so these gate that the column cannot disagree with
// the mount.
//
// ⚠ The agreement is asserted against `make_engine_with_legs` ITSELF wherever a network-free
// observable exists for it, not against a restatement of the rows: the tier a mount reaches is
// visible offline only as PAPER-vs-not (the pre-connect budget refusal fires from
// `would_mount_live_under`), so that half is driven through the real mount, and the
// Live-vs-Demo half is driven against the SAME per-venue tier resolvers the arms call.
// ===========================================================================================

/// **THE column's gate: for every roster venue, at every ceiling, over the real credential
/// fixtures, the projection's PAPER-vs-not verdict is what the mount actually does.**
///
/// ⚠ **What this proves, stated exactly.** The agreement is BY CONSTRUCTION — `make_engine`
/// consults `would_mount_live_under`, which is now `venue_arming_under` projected onto
/// paper-vs-not — so this test cannot catch the two functions disagreeing (they are one
/// function). What it DOES catch is the construction coming apart end-to-end: the mount
/// consulting a different ceiling than the one it was handed, the seam moving back below the
/// credential read, or the projection answering above its own ceiling. It drives the REAL
/// `make_engine_with_legs` with a REAL `MountPolicy` and reads its only offline observable, the
/// pre-connect budget refusal. The INDEPENDENT half — is the projected TIER the tier the arm
/// would dial — is
/// [`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`],
/// which pins, per switched venue and key set, the tier the projection reaches.
///
/// Anti-vacuity is built in two ways: the armed maps are asserted to arm SOMETHING at the live
/// ceiling before the comparison runs, and every venue is driven with an EMPTY map too, so a
/// projection that had degenerated into "always paper" would still have to agree with a mount
/// that has not.
#[test]
fn the_arming_projection_agrees_with_the_real_mount_for_every_roster_venue() {
    use vike_config::VenueMode as Mode;

    let armed_for: std::collections::HashMap<&str, HashMap<String, String>> =
        live_arming_cases().iter().map(|(v, kv)| (*v, vars(kv))).collect();
    // ANTI-VACUITY: at least one fixture really does arm, or every agreement below is between
    // two functions that both always say paper.
    assert!(
        armed_for.iter().any(|(v, map)| vike_mount::venue_arming_under(
            REGISTRY,
            v,
            map,
            Mode::Live
        )
        .0 != Mode::Paper),
        "no fixture arms anything — this test would prove nothing"
    );

    // ⚠ The THIRD map per venue, and it is the one that makes this test able to fail for the
    // venues it covers. Most `live_arming_cases` fixtures still arm at the DEMO tier, so for
    // those a `demo` ceiling leaves the capped and the UNCAPPED probes agreeing — a mount that
    // consulted `would_mount_live` instead of `would_mount_live_under` at its seam would sail
    // through them undetected. Decision 0095 moved binance/bybit/okx/hyperliquid's OWN rows in
    // `live_arming_cases` to LIVE-tier keys (their arms now require it — see that fn's doc), so
    // those four already create the divergence through `armed_for` alone; `mainnet_arming_cases`
    // is what still creates it for every OTHER row `live_arming_cases` cannot — aster above all,
    // whose `live_arming_cases` row is deliberately TESTNET-shaped.
    let mainnet_for: std::collections::HashMap<&str, HashMap<String, String>> =
        mainnet_arming_cases().iter().map(|(v, kv)| (*v, vars(kv))).collect();
    let empty = HashMap::new();
    for venue in vike_model::VENUES {
        for map in [
            armed_for.get(venue).unwrap_or(&empty),
            mainnet_for.get(venue).unwrap_or(&empty),
            &empty,
        ] {
            for cap in Mode::ALL {
                let (effective, block) = vike_mount::venue_arming_under(REGISTRY, venue, map, cap);

                // 1. A ceiling can only ever REFUSE. The screen renders `effective` beside
                //    `ceiling`, so a projection that promoted would render a lie.
                assert!(
                    effective <= cap,
                    "{venue} @ {cap}: projected {effective}, ABOVE the ceiling"
                );
                // 2. `block` and `effective` cannot contradict, in ONE direction: a CAPPED row
                //    must carry a reason, and a CLEAR block must be at its ceiling.
                //
                //    ⚠ Deliberately not an `assert_eq!` of the two. The converse is false and
                //    correctly so: a `paper` ceiling reports `Disarmed` and a mount-less build
                //    reports `NoMountInThisBuild` — both at their ceiling, both with something
                //    to say. A block is "what is holding this row where it is", which is
                //    information even when nothing is being refused.
                if effective < cap {
                    assert!(
                        !block.is_clear(),
                        "{venue} @ {cap}: capped to {effective} with NO reason — the Effective \
                             column would render a demotion the operator cannot explain"
                    );
                }
                if block.is_clear() {
                    assert_eq!(
                        effective, cap,
                        "{venue} @ {cap}: a clear block must mean the row is at its ceiling"
                    );
                }
                // 3. THE agreement, through the REAL mount.
                let policy = ceiling(venue, cap);
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
                            "{venue} @ {cap}: a mount that armed live must have hit the \
                                 pre-connect budget refusal first"
                        );
                        false
                    }
                };
                assert_eq!(
                    effective != Mode::Paper,
                    mount_is_live,
                    "{venue} @ {cap}: the screen would say `{effective}` ({block:?}) while the \
                         mount {} — the Effective column exists precisely so these cannot differ",
                    if mount_is_live { "reaches its live arm" } else { "stays paper" }
                );
            }
        }
    }
}

/// The credential shapes that arm a venue at its **LIVE** tier — [`live_arming_cases`]'s
/// dangerous twin, and the only maps on which a `demo` ceiling and no ceiling at all give
/// DIFFERENT answers.
///
/// Only the five venues whose arm can reach live from a map: the four venues whose network is
/// their ceiling (a LIVE key set) and aster, which is switchless and picks its tier from WHICH key
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
    for (venue, kv) in live_arming_cases() {
        let armed = vars(kv);
        for map in [&armed, &empty] {
            for cap in Mode::ALL {
                assert_eq!(
                    vike_mount::would_mount_live_under(REGISTRY, venue, map, cap),
                    vike_mount::venue_arming_under(REGISTRY, venue, map, cap).0 != Mode::Paper,
                    "{venue} @ {cap}"
                );
            }
        }
    }
}

/// One `(venue, LIVE-tier keys, DEMO-tier keys)` row per switched venue (decision 0095: binance,
/// bybit, okx, hyperliquid) — deliberately its OWN table rather than a reuse of
/// [`live_arming_cases`], which for these four venues carries LIVE-tier keys only (its own doc
/// explains why: `would_mount_live` is an implicit `Live`-ceiling probe, so a DEMO-shaped row
/// there would probe false). Shared by the pure-projection test
/// ([`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`]) and its real-mount
/// twin ([`the_ceiling_alone_chooses_the_network_through_the_real_mount_too`]) so the two matrices
/// cannot silently drift apart, and so BOTH tiers are independently driven through the REAL mount
/// for every switched venue — not only the pure projection.
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

/// **Decision 0095: the ceiling alone chooses the network** for binance, bybit, okx and
/// hyperliquid, and the row says what is missing. No variable is consulted — the map below carries
/// none, and the process environment is not read on this path any more.
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
            "{venue}: a `live` ceiling with only demo keys is PAPER — a mainnet host is never \
             signed with demo keys"
        );
        assert_eq!(
            vike_mount::venue_arming_under(REGISTRY, venue, &live, Mode::Demo),
            (Mode::Paper, Block::NoCredentials),
            "{venue}: a `demo` ceiling never reaches the LIVE key set"
        );
        assert_eq!(
            vike_mount::venue_arming_under(REGISTRY, venue, &demo, Mode::Demo),
            (Mode::Demo, Block::None),
            "{venue}"
        );
        let row = vike_config::VenueArming {
            venue: vike_config::roster_id(venue).expect("a roster venue"),
            label: AccountLabel::Default,
            ceiling: Mode::Live,
            effective: Mode::Paper,
            block: Block::LiveCredentialsAbsent,
        };
        assert!(row.why().contains("LIVE") && row.why().contains("paper"), "{}", row.why());
    }
    // deribit mounts through its own bridge and is testnet-only: a live ceiling still reaches demo.
    let deribit = vars(&[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")]);
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "deribit", &deribit, Mode::Live),
        (Mode::Demo, Block::DemoOnlyArm)
    );
}

/// **The REAL-MOUNT twin of [`the_ceiling_alone_chooses_the_network_and_the_row_says_what_is_missing`].**
///
/// That test proves the pure projection (`venue_arming_under`) agrees that "a `live` ceiling with
/// only demo keys is PAPER" for the four switched venues — but proving the PROJECTION agrees with
/// itself is not the same evidence as proving the mount does. `live_arming_cases()` (the fixture
/// most tests in this suite draw from) deliberately carries LIVE-tier keys, not DEMO-tier, for
/// these four venues specifically so `would_mount_live`'s implicit `Live`-ceiling probe stays true
/// (decision 0095's own doc on that fn) — which means no map in that shared table can any longer
/// witness "DEMO-tier keys reach the REAL mount's paper path under a `live` ceiling" for
/// binance/bybit/okx/hyperliquid. This test drives [`mount`] — the real
/// `make_engine_with_legs` — directly with [`switched_venue_tier_cases`]'s own LIVE- and DEMO-tier
/// maps, all four (venue, key-tier, ceiling) combinations, so that property is witnessed by real
/// code and not only by the function under test agreeing with itself.
#[test]
fn the_ceiling_alone_chooses_the_network_through_the_real_mount_too() {
    use vike_config::VenueMode as Mode;

    for SwitchedVenueTiers { venue, live_keys, demo_keys } in switched_venue_tier_cases() {
        let venue = *venue;
        let (live, demo) = (vars(live_keys), vars(demo_keys));

        // LIVE keys, LIVE ceiling -> reaches the live path (the pre-connect budget refusal fires,
        // proving the mount classified this as live-intent rather than merely not-erroring).
        assert!(
            matches!(
                mount(venue, &live, Some(&ceiling(venue, Mode::Live))).0,
                Err(vike_mount::MountError::MissingRiskBudget { .. })
            ),
            "{venue}: LIVE-tier keys under a `live` ceiling must reach the live path"
        );

        // DEMO keys, LIVE ceiling -> PAPER. THE safety property this task exists to guard,
        // witnessed through the real mount rather than the pure projection alone: a `live`
        // ceiling never falls back to a venue's demo credentials.
        let (out, live_set) = mount(venue, &demo, Some(&ceiling(venue, Mode::Live)));
        out.unwrap_or_else(|e| {
            panic!(
                "{venue}: a `live` ceiling with only DEMO-tier keys must mount PAPER, not refuse \
                 to start: {e}"
            )
        });
        assert!(
            live_set.is_empty(),
            "{venue}: DEMO-tier keys armed a live exec client under a `live` ceiling"
        );

        // LIVE keys, DEMO ceiling -> PAPER (the real-mount twin of
        // `a_demo_ceiling_never_reaches_the_live_key_set`, over all four switched venues rather
        // than binance alone).
        let (out, live_set) = mount(venue, &live, Some(&ceiling(venue, Mode::Demo)));
        out.unwrap_or_else(|e| {
            panic!(
                "{venue}: a `demo` ceiling with only LIVE-tier keys must mount PAPER, not refuse \
                 to start: {e}"
            )
        });
        assert!(
            live_set.is_empty(),
            "{venue}: LIVE-tier keys armed an exec client under a `demo` ceiling"
        );

        // DEMO keys, DEMO ceiling -> reaches the demo path (still non-paper, still budget-gated —
        // the control that proves the two PAPER results above are the ceiling's doing and not a
        // fixture that never arms anything).
        assert!(
            matches!(
                mount(venue, &demo, Some(&ceiling(venue, Mode::Demo))).0,
                Err(vike_mount::MountError::MissingRiskBudget { .. })
            ),
            "{venue}: DEMO-tier keys under a `demo` ceiling must reach the demo path"
        );
    }
}

/// A venue this BUILD cannot mount reports [`vike_config::ArmingBlock::FeatureAbsent`] rather
/// than "no credentials" — the row renders the disagreement between a portable `policy.venues`
/// row and a binary's compiled features instead of hiding it behind a credential complaint.
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
/// carrying the ceiling it was asked about and the answer the per-venue call gives.
///
/// ⚠ **With an empty credential map and no `[accounts]` table there is exactly one row per
/// roster venue — the DEFAULT account's** — which is the table this projection returned before
/// it knew about accounts at all. That equality is the whole "a box with no accounts behaves
/// exactly as today" claim, asserted here at the projection rather than argued in prose.
#[test]
fn the_roster_projection_covers_every_venue_and_carries_its_ceiling() {
    use vike_config::VenueMode as Mode;
    let policy = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("bybit", Mode::Live).declare("binance", Mode::Demo),
        ..Default::default()
    };
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
    assert_eq!(by("bybit").ceiling, Mode::Live);
    assert_eq!(by("binance").ceiling, Mode::Demo);
    assert_eq!(by("okx").ceiling, Mode::Paper, "an unnamed venue keeps the safe default");
    // …and every row agrees with the per-venue call it is built from.
    for row in &rows {
        assert_eq!(
            (row.effective, row.block),
            vike_mount::venue_arming_under(REGISTRY, row.venue, &HashMap::new(), row.ceiling),
        );
    }
}

/// **A LABELLED account in the credential store gets a row of its own** — and, with no
/// `[accounts]` line naming it, that row is PAPER with the block that says which line to write.
#[test]
fn a_labelled_account_in_the_store_gets_its_own_unarmed_row() {
    use vike_config::{ArmingBlock as Block, VenueMode as Mode};
    // `Demo`, not `Live`: `bybit_store_with_alt` plants DEMO-tier keys only, and decision 0095
    // means a `live` ceiling would no longer arm the default account from them (a mainnet host is
    // never signed with demo keys) — this test's subject is the LABELLED row, not the tier, so the
    // ceiling that actually matches the fixture's credentials is the one that keeps the default
    // row's "it IS armed" precondition true.
    let policy = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("bybit", Mode::Demo),
        ..Default::default()
    };
    let vars = bybit_store_with_alt();

    let rows = vike_mount::venue_account_arming(REGISTRY, "bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "the default account and ALT: {rows:?}");
    assert!(rows[0].is_default_account(), "the default account sorts FIRST");
    assert_ne!(rows[0].effective, Mode::Paper, "its credentials arm it, exactly as before");

    let alt = &rows[1];
    assert_eq!(alt.label.text(), Some("ALT"));
    assert_eq!(alt.effective, Mode::Paper, "a labelled account is not armed by the venue line");
    assert_eq!(alt.block, Block::AccountNotNamed);
    assert_eq!(alt.key(), "policy.accounts.bybit.ALT", "the row names the line to write");
    assert_eq!(alt.route_key(), "bybit#ALT");
    assert_eq!(rows[0].route_key(), "bybit", "the default account's routing does not move");
}

// ⚠ **THE HEADLINE — two armed accounts of one venue, and the SYMBOL reaching no arming
// decision — is asserted in `crates/vike-tradehub/tests/shared_book_report.rs`**, not here, and
// the reason is a gate rather than a preference: `crates/vike-ops/tests/settings/settings_registry.rs`
// HARVESTS STRING LITERALS out of `src/` to find undeclared environment reads, and a fixture
// planting `HYPERLIQUID_DEMO_ACCOUNT_ADDRESS__ALT` here reads to that scanner as this library
// reading a variable nothing declares. A `tests/` file is outside its scan by construction, so
// a credential-shaped fixture belongs there — which is also where the rule's own unit tests
// live (`crates/vike-config/tests/venue_accounts_table.rs`).
//
// The test this replaced was `two_armed_accounts_on_one_symbol_refuse_the_labelled_one`, and it
// asserted the opposite of what is true: long BTC on the default account and short BTC on `ALT`
// is an ordinary spread, and the two accounts are two wallets holding two positions.
