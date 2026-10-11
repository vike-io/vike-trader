//! THE ACCOUNT TIER (decision 0119): a `paper`, absent or inactive account never reaches a live client.

use std::collections::HashMap;

use vike_config::VenueMode;
use vike_tradehub::registry::REGISTRY;

use super::*;
use crate::support::vars;

// ===========================================================================================
// THE ACCOUNT TIER (decision 0119: the `account` row's `tier` + `active` replaced the ceiling)
//
// ⚠ Every test below drives the REAL `make_engine_with_legs` with REAL live-arming credential
// shapes, and every one of them stays OFFLINE — because the tier holds. A regression makes
// this suite dial ten venues, which is the same stance (and the same wording) as the OKX
// passphrase and OANDA live-tier gates above.
//
// ⚠ The LIVE side is asserted through the pre-connect BUDGET REFUSAL rather than through
// `live_venues`, and that is not a weaker check dressed up — it is the only observable a
// network-free test HAS. Reaching `live_venues.insert` means the venue's arm ran, i.e. a
// blocking instrument fetch and a dialing exec thread; `MountError::MissingRiskBudget` fires
// one step earlier, from `would_mount_live_under`, and firing it PROVES the mount classified
// the venue as live-intent at that tier. The paper side, where nothing dials, is asserted on
// `live_venues` directly.
// ===========================================================================================

/// **THE PROPERTY, over the WHOLE roster**: a venue whose DEFAULT account has no active
/// non-paper row never gets a live exec client, no matter what its credentials say — and the very
/// same credentials under an active row at the arming tier do reach the live path, so the refusal
/// is the account table and not a broken fixture.
///
/// Four paper shapes, each a different `account_tier` answer: no policy at all, an UNREAD
/// directory (`MountPolicy::default()`), an active `paper`-tier row, and an INACTIVE row at the very
/// tier these keys arm.
///
/// ⚠ The `None`-policy leg is not redundant with the `MountPolicy::default()` one, and it is
/// the leg that catches the fail-safe default being weakened: `policy.map_or(Paper, …)` and
/// `policy.map_or(Live, …)` agree on every `Some`, and differ only here.
#[test]
fn a_paper_moded_venue_never_gets_a_live_exec_client() {
    for (venue, tier, kv) in live_arming_cases() {
        let armed = vars(kv);
        // ANTI-VACUITY: these credentials really would arm this venue at this tier. Without this
        // line every assertion below could pass against an empty map.
        assert!(
            vike_mount::would_mount_live_under(REGISTRY, venue, &armed, *tier),
            "{venue}: the fixture must be a map that ARMS, or this test proves nothing"
        );

        let unread = vike_mount::MountPolicy::default();
        let paper_row = account_at(venue, VenueMode::Paper);
        let inactive =
            vike_mount::MountPolicy::default().with_account_row(account_row(venue, *tier, false));
        for (shape, policy) in [
            ("no policy", None),
            ("unread directory", Some(&unread)),
            ("paper-tier row", Some(&paper_row)),
            ("inactive row", Some(&inactive)),
        ] {
            let (out, live) = mount(venue, &armed, policy);
            let (engine, recon) = out.unwrap_or_else(|e| {
                panic!("{venue} ({shape}): a paper account is PAPER and must never refuse: {e}")
            });
            assert!(live.is_empty(), "{venue} ({shape}): a live exec client was armed");
            assert!(recon.is_none(), "{venue}: a paper venue never gets a reconcile handle");
            assert_eq!(
                engine.fee_schedule,
                Some(vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, "BTCUSDT"))),
                "{venue} ({shape}): the paper mount is still tagged with the venue's fee schedule"
            );
        }
        assert!(
            !vike_mount::would_mount_live_under(REGISTRY, venue, &armed, VenueMode::Paper),
            "{venue}: a paper-tier account has no live arm to probe"
        );

        // …and the CONTROL, which is what makes the refusals above the ACCOUNT TABLE's doing: the
        // same map under an active row at its tier reaches the live path. Observed as the
        // pre-connect budget refusal — no venue session is established, so this stays offline.
        let permitted = account_at(venue, *tier);
        match mount(venue, &armed, Some(&permitted)).0 {
            Err(vike_mount::MountError::MissingRiskBudget { venue: refused, .. }) => {
                assert_eq!(refused.as_str(), *venue, "the refusal must name the venue")
            }
            Ok(_) => panic!(
                "{venue}: an active `{tier}` row over these credentials did NOT reach the live \
                 path — the pre-connect budget refusal never fired, so the account table is \
                 refusing everything and the paper assertions above prove nothing"
            ),
        }

        // EXACTLY that venue: the same one-row table leaves every OTHER armed venue on paper, so
        // a row arms its own account rather than flipping a global switch.
        for (other, _, other_kv) in live_arming_cases() {
            if other == venue {
                continue;
            }
            let (out, live) = mount(other, &vars(other_kv), Some(&permitted));
            out.unwrap_or_else(|e| panic!("{other} has no row and must not refuse: {e}"));
            assert!(live.is_empty(), "{venue}'s row must not arm {other}");
        }
    }
}

/// **`demo` NEVER REACHES THE LIVE KEY SET.** Decision 0095: a `demo` account resolves the DEMO
/// tier regardless of which keys are present, so LIVE keys alone, with no demo keys, leave the
/// venue on PAPER — never a mainnet host signed with the live key set a `demo` account declined
/// to use.
#[test]
fn a_demo_tier_never_reaches_the_live_key_set() {
    let armed = vars(&[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")]);
    // CONTROL FIRST, so the refusal below cannot be a fixture that never armed anything: for a
    // `live` account this exact map reaches the live path (the pre-connect budget refusal fires).
    assert!(vike_mount::would_mount_live_under(REGISTRY, "binance", &armed, VenueMode::Live));
    assert!(matches!(
        mount("binance", &armed, Some(&account_at("binance", VenueMode::Live))).0,
        Err(vike_mount::MountError::MissingRiskBudget { .. })
    ));

    // …and for a `demo` account the same map arms nothing at all.
    assert!(
        !vike_mount::would_mount_live_under(REGISTRY, "binance", &armed, VenueMode::Demo),
        "a `demo` account resolves the DEMO tier, and there are no demo keys here"
    );
    let (out, live) = mount("binance", &armed, Some(&account_at("binance", VenueMode::Demo)));
    out.expect("a demo account with only LIVE keys is PAPER, and paper never refuses");
    assert!(live.is_empty(), "the LIVE key set armed a `demo` account");
}

/// **THE MEASURED HOLE.** Aster has never had a `{VENUE}_MAINNET`-shaped switch, and its arm tried
/// `Environment::Live` FIRST. An `ASTER_LIVE_*` pair in the credential store was therefore by
/// itself an authenticated MAINNET session on a daemon that never asked for one (observed on the CI box,
/// inside a nine-venue live set).
///
/// Under anything below `live` the Live attempt is DELETED from the chain, so the pair resolves
/// nothing and the venue is paper.
#[test]
fn demo_mode_deletes_the_live_first_attempt_on_a_switchless_venue() {
    let armed = vars(&[("ASTER_LIVE_USER", "0xUser"), ("ASTER_LIVE_PRIVATE_KEY", "0xkey")]);
    // CONTROL: the pair genuinely arms this venue under `live` — mainnet, first attempt, today.
    assert!(vike_mount::would_mount_live_under(REGISTRY, "aster", &armed, VenueMode::Live));
    assert!(matches!(
        mount("aster", &armed, Some(&account_at("aster", VenueMode::Live))).0,
        Err(vike_mount::MountError::MissingRiskBudget { .. })
    ));

    for capped in [VenueMode::Demo, VenueMode::Paper] {
        assert!(
            !vike_mount::would_mount_live_under(REGISTRY, "aster", &armed, capped),
            "{capped}: LIVE aster credentials must arm nothing — there is no flag to refuse \
                 them, so the account's tier is the only refusal that exists"
        );
        let (out, live) = mount("aster", &armed, Some(&account_at("aster", capped)));
        out.expect("a below-live aster mount is PAPER, and paper never refuses to start");
        assert!(live.is_empty(), "{capped}: ASTER_LIVE_* armed a real mainnet account");
    }

    // …and the TESTNET pair still arms a `demo` account, so what the tier deleted is the LIVE
    // attempt and not the venue.
    let testnet = vars(&[("ASTER_TESTNET_USER", "0xUser"), ("ASTER_TESTNET_PRIVATE_KEY", "0xkey")]);
    assert!(vike_mount::would_mount_live_under(REGISTRY, "aster", &testnet, VenueMode::Demo));

    // …and the NO-DOWNGRADE half (the owner's ruling on decision 0119): the same TESTNET pair
    // under a `live` account is PAPER, never the demo fallback (`held_below_live`) this venue
    // once took. A live account never trades demo; the block says its live set is absent.
    assert_eq!(
        vike_mount::venue_arming_under(REGISTRY, "aster", &testnet, VenueMode::Live),
        (VenueMode::Paper, vike_config::ArmingBlock::LiveCredentialsAbsent),
        "a `live` aster account with only TESTNET keys must project PAPER, never demo"
    );
    let (out, live) = mount("aster", &testnet, Some(&account_at("aster", VenueMode::Live)));
    out.expect("a live aster account with only TESTNET keys is PAPER, and paper never refuses");
    assert!(live.is_empty(), "a `live` aster account fell back to its TESTNET keys");
}

/// An account held at paper must be the SAME engine an uncredentialled one gets — the account
/// table refuses an arming, it does not invent a third mode.
///
/// This is the guard on `paper_engine` being a separate assembly from the post-match tail: the
/// two are compared by OUTPUT (`vike_exec::state_hash` over the real snapshot, plus the fee
/// schedule, which the snapshot does not carry), so a step added to the tail and not here is
/// caught rather than merely commented about.
#[test]
fn a_capped_venue_is_the_same_engine_an_uncredentialled_one_gets() {
    for (venue, tier, kv) in live_arming_cases() {
        let capped = mount(venue, &vars(kv), Some(&vike_mount::MountPolicy::default()))
            .0
            .unwrap_or_else(|e| panic!("{venue} held at paper: {e}"))
            .0;
        // The SAME venue with an EMPTY credential map, and an active row at the tier its keys
        // would arm — so the only reason it is paper is the pre-existing absent-credentials gate.
        let unarmed = mount(venue, &HashMap::new(), Some(&account_at(venue, *tier)))
            .0
            .unwrap_or_else(|e| panic!("{venue} uncredentialled: {e}"))
            .0;
        assert_eq!(
            vike_exec::state_hash(&[capped.snapshot_state()]),
            vike_exec::state_hash(&[unarmed.snapshot_state()]),
            "{venue}: the paper-held mount and the uncredentialled one are different engines"
        );
        assert_eq!(capped.fee_schedule, unarmed.fee_schedule, "{venue}");
    }
}

/// **The regression the D1 fix exists for, on the account's own figure.** Until 2026-09-27
/// `paper_engine` took no `account` and so could not read a per-account exposure figure — the ONE
/// limits line the same-engine test above cannot catch, because neither `MountPolicy` it builds
/// carries one. This plants `account.max_exposure` on the DEFAULT account's row and asserts BOTH
/// paper routes — held by a `paper`-tier row, and uncredentialled under an active row at the
/// arming tier — apply it, mirroring the live twin's own fold in `make_engine_for_account`'s
/// post-match tail.
///
/// The second half pins the fold's DIRECTION on the paper side: the row's figure is a `min` with
/// the box-wide `policy.max_account_exposure`, so it narrows a looser box figure and never
/// widens a tighter one.
#[test]
fn a_paper_engine_applies_the_per_account_exposure_ceiling() {
    let cap = 42_000.0;
    let with_cap = |venue: &str, tier: VenueMode, box_wide: Option<f64>| {
        let row = vike_secrets::Account {
            max_exposure: vike_secrets::MaxExposure::new(cap),
            ..account_row(venue, tier, true)
        };
        vike_mount::MountPolicy {
            max_account_exposure: box_wide,
            ..vike_mount::MountPolicy::default()
        }
        .with_account_row(row)
    };
    for (venue, tier, kv) in live_arming_cases() {
        let capped = mount(venue, &vars(kv), Some(&with_cap(venue, VenueMode::Paper, None)))
            .0
            .unwrap_or_else(|e| panic!("{venue} paper-tier: {e}"))
            .0;
        assert_eq!(
            capped.gate.limits.max_account_exposure,
            Some(cap),
            "{venue}: a paper-tier engine must apply the DEFAULT account's `max_exposure`, \
                 exactly as the live twin's post-match tail does"
        );

        let unarmed = mount(venue, &HashMap::new(), Some(&with_cap(venue, *tier, None)))
            .0
            .unwrap_or_else(|e| panic!("{venue} uncredentialled: {e}"))
            .0;
        assert_eq!(
            unarmed.gate.limits.max_account_exposure,
            Some(cap),
            "{venue}: an uncredentialled-paper engine must apply the SAME ceiling — a paper \
                 mount must never be the more permissive side, whichever reason made it paper"
        );

        for (box_wide, expected) in [(cap * 2.0, cap), (cap / 2.0, cap / 2.0)] {
            let engine =
                mount(venue, &vars(kv), Some(&with_cap(venue, VenueMode::Paper, Some(box_wide))))
                    .0
                    .unwrap_or_else(|e| panic!("{venue} paper-tier, box-wide {box_wide}: {e}"))
                    .0;
            assert_eq!(
                engine.gate.limits.max_account_exposure,
                Some(expected),
                "{venue}: box-wide {box_wide} and the row's {cap} must fold to the TIGHTER one"
            );
        }
    }
}
