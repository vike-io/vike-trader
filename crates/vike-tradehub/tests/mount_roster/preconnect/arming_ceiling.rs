//! THE ARMING CEILING (settings-unification stage 3): a `paper` ceiling never reaches a live client.

use std::collections::HashMap;

use vike_config::{VenueMode, VenuePolicy};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::registry::REGISTRY;

use super::*;
use crate::support::vars;

// ===========================================================================================
// THE ARMING CEILING (settings-unification stage 3)
//
// ⚠ Every test below drives the REAL `make_engine_with_legs` with REAL live-arming credential
// shapes, and every one of them stays OFFLINE — because the ceiling holds. A regression makes
// this suite dial ten venues, which is the same stance (and the same wording) as the OKX
// passphrase and OANDA live-tier gates above.
//
// ⚠ The LIVE side is asserted through the pre-connect BUDGET REFUSAL rather than through
// `live_venues`, and that is not a weaker check dressed up — it is the only observable a
// network-free test HAS. Reaching `live_venues.insert` means the venue's arm ran, i.e. a
// blocking instrument fetch and a dialing exec thread; `MountError::MissingRiskBudget` fires
// one step earlier, from `would_mount_live_under`, and firing it PROVES the mount classified
// the venue as live-intent under that ceiling. The paper side, where nothing dials, is asserted
// on `live_venues` directly.
// ===========================================================================================

/// **THE STAGE-3 PROPERTY, over the WHOLE roster**: a venue whose ceiling is `paper` never gets
/// a live exec client, no matter what its credentials say — and the very same credentials under
/// a `live` ceiling do reach the live path, so the refusal is the ceiling and not a broken
/// fixture.
///
/// ⚠ The `None`-policy leg is not redundant with the `MountPolicy::default()` one, and it is
/// the leg that catches the fail-safe default being weakened: `policy.map_or(Paper, …)` and
/// `policy.map_or(Live, …)` agree on every `Some`, and differ only here.
#[test]
fn a_paper_moded_venue_never_gets_a_live_exec_client() {
    for (venue, kv) in live_arming_cases() {
        let armed = vars(kv);
        // ANTI-VACUITY: these credentials really would arm this venue. Without this line every
        // assertion below could pass against an empty map.
        assert!(
            vike_mount::would_mount_live(REGISTRY, venue, &armed),
            "{venue}: the fixture must be a map that ARMS, or this test proves nothing"
        );

        let no_file = vike_mount::MountPolicy::default();
        for policy in [None, Some(&no_file)] {
            let (out, live) = mount(venue, &armed, policy);
            let (engine, recon) = out.unwrap_or_else(|e| {
                panic!("{venue}: a capped mount is PAPER and must never refuse to start: {e}")
            });
            assert!(
                live.is_empty(),
                "{venue}: a `paper` ceiling let a live exec client be armed (policy \
                     supplied: {})",
                policy.is_some()
            );
            assert!(recon.is_none(), "{venue}: a paper venue never gets a reconcile handle");
            assert_eq!(
                engine.fee_schedule,
                Some(vike_model::fee_schedule_for(vike_catalog::fee_lane(venue, "BTCUSDT"))),
                "{venue}: the capped mount is still tagged with the venue's fee schedule"
            );
        }
        assert!(
            !vike_mount::would_mount_live_under(REGISTRY, venue, &armed, VenueMode::Paper),
            "{venue}: a disarmed venue has no live arm to probe"
        );

        // …and the CONTROL, which is what makes the refusals above the CEILING's doing: the
        // same map under `live` reaches the live path. Observed as the pre-connect budget
        // refusal — no venue session is established, so this stays offline.
        assert!(
            vike_mount::would_mount_live_under(REGISTRY, venue, &armed, VenueMode::Live),
            "{venue}: the same credentials under a `live` ceiling must probe live"
        );
        let permitted = ceiling(venue, VenueMode::Live);
        match mount(venue, &armed, Some(&permitted)).0 {
            Err(vike_mount::MountError::MissingRiskBudget { venue: refused, .. }) => {
                assert_eq!(refused.as_str(), *venue, "the refusal must name the venue")
            }
            Ok(_) => panic!(
                "{venue}: a `live` ceiling over live credentials did NOT reach the live path — \
                     the pre-connect budget refusal never fired, so the ceiling is refusing \
                     everything and the paper assertions above prove nothing"
            ),
        }

        // EXACTLY that venue: the same one-venue `live` policy leaves every OTHER armed venue
        // on paper, so a ceiling is per-venue rather than a global switch.
        for (other, other_kv) in live_arming_cases() {
            if other == venue {
                continue;
            }
            let (out, live) = mount(other, &vars(other_kv), Some(&permitted));
            out.unwrap_or_else(|e| panic!("{other} is capped and must not refuse: {e}"));
            assert!(live.is_empty(), "{venue}=live must not arm {other}");
        }
    }
}

/// **`demo` NEVER REACHES THE LIVE KEY SET.** Decision 0095: under a `demo` ceiling the DEMO tier
/// is resolved regardless of which keys are present, so LIVE keys alone, with no demo keys, leave
/// the venue on PAPER — never a mainnet host signed with the live key set a `demo` ceiling
/// declined to use.
#[test]
fn a_demo_ceiling_never_reaches_the_live_key_set() {
    let armed = vars(&[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")]);
    // CONTROL FIRST, so the refusal below cannot be a fixture that never armed anything: under
    // `live` this exact map reaches the live path (the pre-connect budget refusal fires).
    assert!(vike_mount::would_mount_live_under(REGISTRY, "binance", &armed, VenueMode::Live));
    assert!(matches!(
        mount("binance", &armed, Some(&ceiling("binance", VenueMode::Live))).0,
        Err(vike_mount::MountError::MissingRiskBudget { .. })
    ));

    // …and under `demo` the same map arms nothing at all.
    assert!(
        !vike_mount::would_mount_live_under(REGISTRY, "binance", &armed, VenueMode::Demo),
        "a `demo` ceiling resolves the DEMO tier, and there are no demo keys here"
    );
    let (out, live) = mount("binance", &armed, Some(&ceiling("binance", VenueMode::Demo)));
    out.expect("a demo-capped venue with only LIVE keys is PAPER, and paper never refuses");
    assert!(live.is_empty(), "the LIVE key set armed a venue the ceiling capped at `demo`");
}

/// **THE MEASURED HOLE.** Aster has never had a `{VENUE}_MAINNET`-shaped switch, so there is no
/// `ASTER_MAINNET` for `vike_config::CREDENTIAL_FILE_ARMING_REFUSED` to carry a row for, and its
/// arm tried `Environment::Live` FIRST. An `ASTER_LIVE_*` pair in
/// the credential store was therefore by itself an authenticated MAINNET session on a daemon that
/// never asked for one (observed on the CI box, inside a nine-venue live set).
///
/// Under anything below `live` the Live attempt is DELETED from the chain, so the pair resolves
/// nothing and the venue is paper.
#[test]
fn demo_mode_deletes_the_live_first_attempt_on_a_switchless_venue() {
    let armed = vars(&[("ASTER_LIVE_USER", "0xUser"), ("ASTER_LIVE_PRIVATE_KEY", "0xkey")]);
    // CONTROL: the pair genuinely arms this venue under `live` — mainnet, first attempt, today.
    assert!(vike_mount::would_mount_live_under(REGISTRY, "aster", &armed, VenueMode::Live));
    assert!(matches!(
        mount("aster", &armed, Some(&ceiling("aster", VenueMode::Live))).0,
        Err(vike_mount::MountError::MissingRiskBudget { .. })
    ));

    for capped in [VenueMode::Demo, VenueMode::Paper] {
        assert!(
            !vike_mount::would_mount_live_under(REGISTRY, "aster", &armed, capped),
            "{capped}: LIVE aster credentials must arm nothing — there is no flag to refuse \
                 them, so the ceiling is the only refusal that exists"
        );
        let (out, live) = mount("aster", &armed, Some(&ceiling("aster", capped)));
        out.expect("a capped aster mount is PAPER, and paper never refuses to start");
        assert!(live.is_empty(), "{capped}: ASTER_LIVE_* armed a real mainnet account");
    }

    // …and the TESTNET pair still arms under `demo`, so what the ceiling deleted is the LIVE
    // attempt and not the venue.
    let testnet = vars(&[("ASTER_TESTNET_USER", "0xUser"), ("ASTER_TESTNET_PRIVATE_KEY", "0xkey")]);
    assert!(vike_mount::would_mount_live_under(REGISTRY, "aster", &testnet, VenueMode::Demo));
}

/// A capped venue must be the SAME engine an uncredentialled one gets — the ceiling refuses an
/// arming, it does not invent a third mode.
///
/// This is the guard on `paper_engine` being a separate assembly from the post-match tail: the
/// two are compared by OUTPUT (`vike_exec::state_hash` over the real snapshot, plus the fee
/// schedule, which the snapshot does not carry), so a step added to the tail and not here is
/// caught rather than merely commented about.
#[test]
fn a_capped_venue_is_the_same_engine_an_uncredentialled_one_gets() {
    for (venue, kv) in live_arming_cases() {
        let capped = mount(venue, &vars(kv), Some(&vike_mount::MountPolicy::default()))
            .0
            .unwrap_or_else(|e| panic!("{venue} capped: {e}"))
            .0;
        // The SAME venue with an EMPTY credential map, and a ceiling that permits everything —
        // so the only reason it is paper is the pre-existing absent-credentials gate.
        let unarmed = mount(venue, &HashMap::new(), Some(&ceiling(venue, VenueMode::Live)))
            .0
            .unwrap_or_else(|e| panic!("{venue} uncredentialled: {e}"))
            .0;
        assert_eq!(
            vike_exec::state_hash(&[capped.snapshot_state()]),
            vike_exec::state_hash(&[unarmed.snapshot_state()]),
            "{venue}: the capped mount and the uncredentialled one are different engines"
        );
        assert_eq!(capped.fee_schedule, unarmed.fee_schedule, "{venue}");
    }
}

/// **The regression this D1 fix exists for.** Until 2026-09-27 `paper_engine` took no `account`
/// and so could not read `policy.account_exposure.<venue>.<LABEL>` — the ONE limits line the
/// same-engine test above cannot catch, because neither `MountPolicy` it builds sets that table.
/// This plants a figure for the DEFAULT account (reachable on a single-account box, per
/// `vike_config::VenuePolicy::account_exposure`'s own doc) and asserts BOTH paper routes —
/// capped by the ceiling, and uncredentialled — apply it, mirroring the live twin's own fold in
/// `make_engine_for_account`'s post-match tail.
#[test]
fn a_paper_engine_applies_the_per_account_exposure_ceiling() {
    for (venue, kv) in live_arming_cases() {
        let cap = 42_000.0;
        let venues = VenuePolicy::default()
            .declare(venue, VenueMode::Paper)
            .declare_account_exposure(venue, &AccountLabel::Default, cap);
        let capped_policy =
            vike_mount::MountPolicy { venues, ..vike_mount::MountPolicy::default() };
        let capped = mount(venue, &vars(kv), Some(&capped_policy))
            .0
            .unwrap_or_else(|e| panic!("{venue} capped: {e}"))
            .0;
        assert_eq!(
            capped.gate.limits.max_account_exposure,
            Some(cap),
            "{venue}: a capped-paper engine must apply policy.account_exposure for the DEFAULT \
                 account, exactly as the live twin's post-match tail does"
        );

        let venues_unarmed = VenuePolicy::default()
            .declare(venue, VenueMode::Live)
            .declare_account_exposure(venue, &AccountLabel::Default, cap);
        let unarmed_policy = vike_mount::MountPolicy {
            venues: venues_unarmed,
            ..vike_mount::MountPolicy::default()
        };
        let unarmed = mount(venue, &HashMap::new(), Some(&unarmed_policy))
            .0
            .unwrap_or_else(|e| panic!("{venue} uncredentialled: {e}"))
            .0;
        assert_eq!(
            unarmed.gate.limits.max_account_exposure,
            Some(cap),
            "{venue}: an uncredentialled-paper engine must apply the SAME ceiling — a paper \
                 mount must never be the more permissive side, whichever reason made it paper"
        );
    }
}

// -- the migration warning ------------------------------------------------------------------

/// **The upgrade warning fires exactly once per box that needs it, and is silent otherwise.**
///
/// Four cases, and the third is the one that matters: an operator who wrote `[venues]` with
/// EVERY venue at `paper` has stated their arming, and a warning that keeps firing at them is
/// the "refusal list that fires on harmless lines" `vike_config::arming` names as the way
/// operators are taught to work around a check.
#[test]
fn the_migration_warning_fires_only_with_credentials_and_no_table() {
    let armed = vars(&[("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")]);

    // 1. credentials + no policy threaded at all → the warning.
    let msg = vike_mount::venue_arming_migration_message(REGISTRY, &armed, None)
        .expect("credentials and no `[venues]` table is exactly the upgrade case");
    // 2. …and the same for a real, loaded policy that simply never named a venue.
    assert_eq!(
        vike_mount::venue_arming_migration_message(
            REGISTRY,
            &armed,
            Some(&vike_mount::MountPolicy::default())
        ),
        Some(msg.clone()),
        "an undeclared policy is the same case as no policy"
    );

    // 3. SELF-SILENCING: an ALL-PAPER table produces the identical ceiling and silences it.
    let all_paper = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("bybit", VenueMode::Paper),
        ..vike_mount::MountPolicy::default()
    };
    assert_eq!(
        all_paper.venue_mode("bybit"),
        VenueMode::Paper,
        "the fixture must leave the CEILING unchanged, or it is silencing by widening"
    );
    assert_eq!(
        vike_mount::venue_arming_migration_message(REGISTRY, &armed, Some(&all_paper)),
        None,
        "a stated all-paper arming is a decision; warning at it trains people to ignore it"
    );

    // 4. No credentials → nothing was refused and there is nothing to paste.
    assert_eq!(vike_mount::venue_arming_migration_message(REGISTRY, &HashMap::new(), None), None);
}

/// The message must be ACTIONABLE and must leak nothing: the key, the venues that were refused,
/// a paste-ready `vike-cli config set` line per venue — and never a credential key NAME, let
/// alone a value. Same rule as `vike_config::refuse_credential_file_arming`, whose refusal
/// deliberately echoes no value from the credential store.
#[test]
fn the_migration_warning_names_the_command_and_only_venue_slugs() {
    let armed = vars(&[
        ("BYBIT_DEMO_API_KEY", "super-secret-key"),
        ("BYBIT_DEMO_API_SECRET", "super-secret-secret"),
        ("OANDA_DEMO_API_KEY", "another-secret"),
        ("OANDA_DEMO_ACCOUNT_ID", "101-004-1234567-001"),
    ]);
    let msg = vike_mount::venue_arming_migration_message(REGISTRY, &armed, None)
        .expect("the upgrade case");

    assert!(!msg.contains("settings/policy.toml"), "names no FILE any more: {msg}");
    assert!(msg.contains("policy.venues"), "names the ROW space: {msg}");
    assert!(
        // Decision 0095: bybit's stored keys are DEMO-only, and bybit is one of the four venues
        // whose `live` ceiling now means MAINNET — so the paste-ready remedy names `demo`, the
        // tier these keys actually reach, never `live` (which would be a no-op paste).
        msg.contains("vike-cli config set policy.venues.bybit demo"),
        "the line is paste-ready and names the tier these keys actually reach: {msg}"
    );
    assert!(
        msg.contains("vike-cli config set policy.venues.oanda live"),
        "…for EVERY refused venue: {msg}"
    );
    assert!(!msg.contains("binance"), "a venue with no credentials is not in the list: {msg}");
    assert!(msg.contains("paper"), "says what the box is doing right now: {msg}");

    for (key, value) in armed.iter() {
        assert!(!msg.contains(key.as_str()), "leaked a credential key NAME ({key}): {msg}");
        assert!(!msg.contains(value.as_str()), "leaked a credential VALUE: {msg}");
    }
}
