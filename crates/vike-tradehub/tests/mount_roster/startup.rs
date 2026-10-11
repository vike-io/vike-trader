//! The startup preflight's clock and credential legs driven over the REAL registry — which venues
//! the preflight contacts, under which account tier, with which credentials. They lived in
//! `crates/vike-mount/src/startup_tests/mod.rs`, which keeps the tests of the probe bounds and of
//! planted contract rows, until the venue mount contract finished (docs/decisions/0096). The
//! account-table helpers below plant one active DEFAULT-account row per roster venue
//! (decision 0119: the row's tier is the only arming there is).

use std::collections::HashMap;
use std::sync::Arc;

use vike_bridge_core::venue_mount::{CredentialProbe, Tier};
use vike_bridge_core::venue_mount_fixture::MountFixture;
use vike_config::VenueMode;
use vike_model::accounts::account_keys::AccountLabel;
use vike_mount::VenueRow;
use vike_mount::preflight::{
    CHECK_CLOCK_SKEW, CHECK_CREDENTIALS, CHECK_NETWORK, CheckStatus, ServerTimeGap,
};
use vike_mount::startup::{
    any_venue_would_mount_live, authed_read_probes, bounded_server_time_ms, clock_policies,
    clock_venues, run_startup_preflight, venue_server_time_ms, withhold_venue_credentials,
};
use vike_tradehub::registry::REGISTRY;

/// The tier each roster venue's [`credentialled`] key shapes arm at: LIVE for the four venues
/// whose network IS the tier (decision 0095; `credentialled` carries their LIVE keys), DEMO for
/// every other venue (its keys are the demo/sandbox shapes).
///
/// ⚠ There is no "widest" tier any more: an account trades at EXACTLY its own tier (the owner's
/// no-downgrade ruling on decision 0119), so a `live` row over demo keys is PAPER, never demo. The
/// armed policy is therefore per venue, the tier its keys can honour.
fn arming_tier(venue: &str) -> VenueMode {
    if matches!(venue, "binance" | "bybit" | "okx" | "hyperliquid") {
        VenueMode::Live
    } else {
        VenueMode::Demo
    }
}

/// **Every roster venue's DEFAULT account armed — an active row at [`arming_tier`].**
///
/// ⚠ Nearly every test below needs this, and passing `None` instead would make most of them
/// VACUOUS rather than red: `None` reads all-`paper`, so a test asserting "absent credentials
/// mean no probe" would pass on a box where the credentials are present and the ACCOUNT TABLE is
/// what suppressed them. The credential gate and the tier gate produce the same empty map, so a
/// scenario that leaves every account at paper cannot tell the two apart — and a test that cannot
/// tell them apart is not testing the one it names. Every test that is about CREDENTIALS
/// therefore arms every account, and the tier gets its own tests below.
fn all_armed() -> vike_mount::MountPolicy {
    vike_model::VENUES.iter().fold(vike_mount::MountPolicy::default(), |p, venue| {
        p.with_account(venue, &AccountLabel::Default, arming_tier(venue))
    })
}

/// Every roster venue's DEFAULT account as an active row at ONE tier — the knob the tests below
/// sweep.
fn all_venues_at(mode: VenueMode) -> vike_mount::MountPolicy {
    vike_model::VENUES.iter().fold(vike_mount::MountPolicy::default(), |p, venue| {
        p.with_account(venue, &AccountLabel::Default, mode)
    })
}

/// [`all_armed`] with ONE venue's row at `paper` — the operator saying "not this one".
fn all_armed_except(paper: &str) -> vike_mount::MountPolicy {
    vike_model::VENUES.iter().fold(vike_mount::MountPolicy::default(), |p, venue| {
        let tier = if *venue == paper { VenueMode::Paper } else { arming_tier(venue) };
        p.with_account(venue, &AccountLabel::Default, tier)
    })
}

/// The venues [`credentialled`] arms at their own tier — the per-venue probe every set below
/// is compared with.
fn armed_at_own_tier(vars: &HashMap<String, String>) -> Vec<&'static str> {
    vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| vike_mount::would_mount_live_under(REGISTRY, v, vars, arming_tier(v)))
        .collect()
}

fn vars_of(kv: &[(&str, &str)]) -> HashMap<String, String> {
    kv.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// A credential map that arms a real spread of the roster through each arm's OWN loader — the
/// same set `a_withheld_venue_would_no_longer_mount_live` drives over, for the same reason: a
/// tier test whose scenario arms one venue proves almost nothing.
fn credentialled() -> HashMap<String, String> {
    vars_of(&[
        // Decision 0095: LIVE-tier for binance/bybit/okx — `arming_tier` gives those three a
        // `live` row, and a `live` account requires LIVE-tier keys to reach anything but Paper
        // (a mainnet host is never signed with demo keys). Every other venue here is DEMO-shaped
        // and armed by a `demo` row.
        ("BINANCE_LIVE_API_KEY", "k"),
        ("BINANCE_LIVE_API_SECRET", "s"),
        ("BYBIT_LIVE_API_KEY", "k"),
        ("BYBIT_LIVE_API_SECRET", "s"),
        ("OKX_LIVE_API_KEY", "k"),
        ("OKX_LIVE_API_SECRET", "s"),
        ("OKX_LIVE_API_PASSPHRASE", "p"),
        ("DERIBIT_DEMO_API_KEY", "k"),
        ("DERIBIT_DEMO_API_SECRET", "s"),
        ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
        ("ALPACA_SANDBOX_CLIENT_SECRET", "csec"),
        ("ALPACA_SANDBOX_ACCOUNT_ID", "acct-1"),
        ("CTRADER_CLIENT_ID", "app"),
        ("CTRADER_CLIENT_SECRET", "app-secret"),
        ("CTRADER_DEMO_ACCESS_TOKEN", "AT"),
        ("CTRADER_DEMO_REFRESH_TOKEN", "RT"),
        ("IG_DEMO_API_KEY", "k"),
        ("IG_DEMO_IDENTIFIER", "id"),
        ("IG_DEMO_PASSWORD", "pw"),
    ])
}

/// A venue with NO clock leg reports the DECLARED gap (③) carrying its reason — not a fake
/// time, and not the "did not answer" gap that means something is wrong. Every venue exercised
/// here is a `NotWired` row, so this touches no network. (The wired venues are deliberately not
/// exercised: those are real network reads.)
#[test]
fn a_declared_clock_venue_reports_its_reason_rather_than_a_fake_time() {
    let vars = HashMap::new();
    for venue in ["ctrader", "oanda", "alpaca"] {
        match venue_server_time_ms(REGISTRY, venue, &vars, false) {
            Err(ServerTimeGap::NotChecked(reason)) => {
                assert!(!reason.is_empty(), "{venue} declares no reason");
            }
            other => panic!("{venue} must report a DECLARED gap, got {other:?}"),
        }
    }
}

/// THE decoupling, at the wiring site: the clock list is derived from live INTENT over the
/// canonical roster, so it is not confined to the venues that offer a credential probe —
/// which is what kept every non-CEX venue's clock unmeasured no matter what was wired.
#[test]
fn the_clock_venue_list_is_not_confined_to_the_credential_probes() {
    // A pure, network-free live-intent map for a venue that has NO authed-read client here.
    let vars: HashMap<String, String> =
        [("IG_DEMO_API_KEY", "k"), ("IG_DEMO_IDENTIFIER", "id"), ("IG_DEMO_PASSWORD", "pw")]
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
    assert!(clock_venues(REGISTRY, &vars, Some(&all_armed())).contains(&"ig".to_string()));
    assert!(
        !authed_read_probes(REGISTRY, &vars, Some(&all_armed())).contains_key("ig"),
        "precondition: ig offers no credential probe, so the credential leg never lists it"
    );
    // …and every listed venue carries a policy iff its clock is actually wired.
    let policies = clock_policies(REGISTRY, &clock_venues(REGISTRY, &vars, Some(&all_armed())));
    assert_eq!(policies.get("ig").copied(), vike_mount::server_time::clock_policy(REGISTRY, "ig"));
    assert_eq!(
        policies["ig"].fail_ms, None,
        "ig's auth stamps no timestamp, so its clock leg may never degrade it to paper"
    );
}

/// No credentials ⇒ no live intent ⇒ no clock legs, so the offline property survives the
/// decoupling.
#[test]
fn no_credentials_means_no_clock_venues() {
    assert!(clock_venues(REGISTRY, &HashMap::new(), Some(&all_armed())).is_empty());
}

/// THE Finding-B fix, offline: credentialed alpaca and ctrader each get a credential probe, so
/// each gets a preflight ROW. Before this, neither venue was in `credential_venues` at all —
/// the mount announced `exec="LIVE" network="SANDBOX"` while every Alpaca host was unreachable
/// and nothing had asked whether the keys worked
/// (the alpaca+ctrader live rehearsal (PR #1407), Finding B).
///
/// Network-free BY CONSTRUCTION, which is also the claim: building the probes must touch
/// nothing. alpaca's client is a pure `TokenSource`/`AlpacaRest` assembly and ctrader's is
/// deferred into its closure, so this test runs offline with bogus credentials.
#[test]
fn alpaca_and_ctrader_get_a_credential_probe_when_credentialed() {
    let vars: HashMap<String, String> = [
        ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
        ("ALPACA_SANDBOX_CLIENT_SECRET", "csec"),
        ("ALPACA_SANDBOX_ACCOUNT_ID", "acct-1"),
        ("CTRADER_CLIENT_ID", "app"),
        ("CTRADER_CLIENT_SECRET", "app-secret"),
        ("CTRADER_DEMO_ACCESS_TOKEN", "AT"),
        ("CTRADER_DEMO_REFRESH_TOKEN", "RT"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();

    let probes = authed_read_probes(REGISTRY, &vars, Some(&all_armed()));
    assert!(probes.contains_key("alpaca"), "alpaca must get a credential row");
    assert!(probes.contains_key("ctrader"), "ctrader must get a credential row");
}

/// ⚠ The laziness that makes ctrader's row exist AT ALL. Its `ReconClient` authenticates during
/// construction, so an eagerly-built probe would return `None` for a REFUSED grant — and a
/// venue absent from the probe map gets NO ROW, turning an expired token back into silence.
/// Here the grant is nonsense and the host is never dialled, yet the row is present.
#[test]
fn a_ctrader_grant_that_could_not_connect_still_yields_a_row() {
    let vars: HashMap<String, String> = [
        ("CTRADER_CLIENT_ID", "app"),
        ("CTRADER_CLIENT_SECRET", "app-secret"),
        ("CTRADER_DEMO_ACCESS_TOKEN", "definitely-expired"),
        ("CTRADER_DEMO_REFRESH_TOKEN", "also-expired"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();

    let probes = authed_read_probes(REGISTRY, &vars, Some(&all_armed()));
    assert!(
        probes.contains_key("ctrader"),
        "a refused grant must still be CHECKED — an absent row is the silent failure this leg \
             exists to remove"
    );
}

/// A DECLARED venue is answered from the table, unchanged — no probe, so no thread. This is
/// what keeps the offline property exactly as it was: the bound is paid only by a read that can
/// actually block. Offline by construction (every venue here is a `NotWired` row).
#[test]
fn a_declared_clock_venue_is_answered_without_the_probe() {
    let vars = Arc::new(HashMap::new());
    let policy = Arc::new(None);
    for venue in ["ctrader", "oanda", "alpaca"] {
        assert_eq!(
            bounded_server_time_ms(REGISTRY, venue, &vars, false, &policy),
            venue_server_time_ms(REGISTRY, venue, &vars, false),
            "{venue} has no wired endpoint, so bounding it must change nothing"
        );
        assert!(
            !matches!(
                vike_mount::server_time::clock_decl(REGISTRY, venue),
                Some(vike_bridge_core::venue_mount::ClockDecl::Wired { .. })
            ),
            "precondition: {venue} must be a DECLARED row for this test to mean anything"
        );
    }
}

/// THE ENFORCEMENT MECHANISM, closed at the only layer that can see it: withholding a venue's
/// credentials makes `would_mount_live_under_policy` FALSE for it — which is what actually turns
/// `make_engine` onto the paper fallback. `crates/vike-tradehub/tests/build_node_paper.rs`'s
/// `a_per_venue_preflight_fail_withholds_that_venues_credentials` asserts the map transformation
/// (it was vike-run's); only this crate can assert what the map transformation MEANS.
///
/// Driven over every canonical-roster venue that a plausible credential set can arm, each at its
/// own account's tier, so a venue whose key spelling escapes the `{VENUE}_` prefix rule would
/// redden this rather than mounting live after being demoted.
#[test]
fn a_withheld_venue_would_no_longer_mount_live() {
    // Every live-arm gate this build compiles, spelled as its own loader wants it.
    let vars = credentialled();
    let policy = all_armed();
    let live = |venue: &str, map: &HashMap<String, String>| {
        vike_mount::would_mount_live_under_policy(REGISTRY, venue, map, Some(&policy))
    };

    let armed = armed_at_own_tier(&vars);
    assert!(
        armed.len() >= 5,
        "precondition: this map must arm a real set of venues, armed = {armed:?}"
    );

    for &venue in &armed {
        assert!(live(venue, &vars), "precondition: {venue} is armed by its own row");
        let mut demoted = vars.clone();
        let withheld = withhold_venue_credentials(&mut demoted, venue);
        assert!(withheld > 0, "{venue} was armed, so it must have had keys to withhold");
        assert!(
            !live(venue, &demoted),
            "{venue} still reads as live-intent after its credentials were withheld — the \
                 preflight's demotion would be announced and then not happen"
        );
        // …and ONLY that venue moved: preflight demotes one venue, never a neighbour.
        for &other in armed.iter().filter(|o| **o != venue) {
            assert!(live(other, &demoted), "withholding {venue} also demoted {other}");
        }
    }
}

/// Absent credentials ⇒ no probe for either venue, so the offline/paper property is intact.
#[test]
fn no_credentials_means_no_credential_probes() {
    assert!(authed_read_probes(REGISTRY, &HashMap::new(), Some(&all_armed())).is_empty());
}

/// Partial credentials are the live gate, not a half-armed probe: alpaca needs all three of
/// client id/secret/account, ctrader needs both halves of the grant.
#[test]
fn partial_credentials_yield_no_probe() {
    let alpaca_partial: HashMap<String, String> =
        [("ALPACA_SANDBOX_CLIENT_ID", "cid"), ("ALPACA_SANDBOX_CLIENT_SECRET", "csec")]
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
    assert!(
        !authed_read_probes(REGISTRY, &alpaca_partial, Some(&all_armed())).contains_key("alpaca")
    );

    let ctrader_partial: HashMap<String, String> = [
        ("CTRADER_CLIENT_ID", "app"),
        ("CTRADER_CLIENT_SECRET", "sec"),
        ("CTRADER_DEMO_ACCESS_TOKEN", "AT"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();
    assert!(
        !authed_read_probes(REGISTRY, &ctrader_partial, Some(&all_armed())).contains_key("ctrader")
    );
}

/// THE default path: an empty credentials map runs the preflight with ZERO network — no venue
/// is checked (no creds ⇒ no authed-read client) and no `NetProbe` is spawned (no venue would
/// mount live). With nothing live, the single network row is DECLARED not-applicable rather
/// than WARNing a developer's TODO at an operator on every paper start — and it still never
/// claims a measurement it did not take. This is the shape `vike_mount::build_node` sees on
/// every paper/CI mount.
#[test]
fn an_empty_credentials_map_preflights_offline() {
    // ⚠ EVERY account armed, deliberately: the claim is that ABSENT CREDENTIALS keep this
    // offline, and `None` (all-`paper`) would keep it offline for the other reason — the same
    // empty report from a different cause, which is not what this test's name says.
    let report = run_startup_preflight(REGISTRY, &HashMap::new(), &[], Some(&all_armed()));
    // A developer box could have the skip flag exported; then the report is empty by design.
    if report.skipped {
        assert!(report.checks.is_empty());
        return;
    }
    assert_eq!(report.checks.len(), 1, "only the global network leg runs: {:?}", report.lines());
    assert_eq!(report.checks[0].name, CHECK_NETWORK);
    assert_eq!(report.checks[0].status, CheckStatus::NotApplicable);
    assert_ne!(report.checks[0].status, CheckStatus::Pass, "never a fake PASS");
    assert!(report.go(), "nothing here is a no-go");
    assert!(report.degraded_venues().is_empty(), "nothing was checked, so nothing degrades");
    assert!(!report.checks.iter().any(|c| c.name == CHECK_CLOCK_SKEW));
    assert!(!report.checks.iter().any(|c| c.name == CHECK_CREDENTIALS));
}

/// **THE DEFECT, as a test.** A venue with valid credentials whose account the operator holds at
/// `paper` is not contacted by ANY leg — it is absent from the clock list and from the
/// credential probe map.
///
/// The measured incident: a box whose settings said `ig = "paper"` still had the preflight
/// reach out to IG at every start, and IG's clock row is the one CREDENTIALED read (it was then a
/// row of `vike_mount::server_time`'s table, `X-IG-API-KEY`; it is the ig bridge's declared clock
/// now). `paper` means "do not touch this account".
///
/// ⚠ The precondition is the half that makes this non-vacuous: with ig's row ARMED the very same
/// map DOES contact ig, so the assertions below are about the account's tier rather than about a
/// map that never armed anything.
#[test]
fn a_paper_capped_venue_with_credentials_is_never_contacted() {
    let vars = credentialled();

    let wide = all_armed();
    assert!(
        clock_venues(REGISTRY, &vars, Some(&wide)).contains(&"ig".to_string()),
        "precondition: these credentials DO arm ig, so holding it at paper is what changes the \
         answer"
    );

    let capped = all_armed_except("ig");
    assert!(
        !clock_venues(REGISTRY, &vars, Some(&capped)).contains(&"ig".to_string()),
        "ig's account is `paper` and the preflight still reads its clock — with its API key"
    );
    assert!(
        !authed_read_probes(REGISTRY, &vars, Some(&capped)).contains_key("ig"),
        "a paper-held venue must not get a credential probe either"
    );
}

/// …and the same for a venue whose probe SIGNS a balance read, which is the account-scoped leg.
/// binance is held at paper; bybit and okx, credentialled identically and left armed, must be
/// untouched — a paper row that disarmed a neighbour would be its own defect.
#[test]
fn capping_one_venue_leaves_its_neighbours_checked() {
    let vars = credentialled();
    let capped = all_armed_except("binance");

    let probes = authed_read_probes(REGISTRY, &vars, Some(&capped));
    assert!(!probes.contains_key("binance"), "binance is `paper`: nothing may be signed for it");
    assert!(probes.contains_key("bybit"), "bybit was left armed and must still be checked");
    assert!(probes.contains_key("okx"), "okx was left armed and must still be checked");

    let clocks = clock_venues(REGISTRY, &vars, Some(&capped));
    assert!(!clocks.contains(&"binance".to_string()));
    assert!(clocks.contains(&"bybit".to_string()));
    assert!(clocks.contains(&"okx".to_string()));
}

/// **An armed venue is checked EXACTLY as before.** With every account armed at the tier its keys
/// honour, the clock list is the per-venue probe's answer, venue for venue — so this lane narrows
/// what a paper deployment contacts and changes nothing for an armed one.
#[test]
fn an_armed_venue_is_checked_exactly_as_before() {
    let vars = credentialled();
    let wide = all_armed();

    let before: Vec<String> = armed_at_own_tier(&vars).into_iter().map(str::to_string).collect();
    assert_eq!(
        clock_venues(REGISTRY, &vars, Some(&wide)),
        before,
        "arming every account at its own tier must be a no-op"
    );

    let mut probed: Vec<String> =
        authed_read_probes(REGISTRY, &vars, Some(&wide)).into_keys().collect();
    probed.sort();
    assert!(
        probed.iter().any(|v| v == "binance") && probed.iter().any(|v| v == "ctrader"),
        "precondition: both probe SHAPES are exercised, eager and lazy — {probed:?}"
    );
    assert!(
        any_venue_would_mount_live(REGISTRY, &vars, Some(&wide)),
        "…and the net probe still arms"
    );
}

/// **THE PROPERTY, asserted rather than assumed: a tier can only ever NARROW what is
/// contacted.** For every uniform tier each derived set is a SUBSET of the venues these keys arm
/// at their own tier, and every element of it is a venue the mount would itself arm at that tier.
///
/// A subset check is the honest shape here. "Fewer venues" would pass for a fix that dropped
/// the wrong venue, and "equal under `live`" alone would say nothing about `demo`. Under the
/// no-downgrade rule the uniform `live` set is the four switched venues' (their LIVE keys) and
/// the uniform `demo` set is everyone else's: neither reaches a venue its keys cannot honour.
#[test]
fn the_tier_can_only_narrow_what_is_contacted() {
    let vars = credentialled();
    let uncapped = armed_at_own_tier(&vars);
    assert!(uncapped.len() >= 5, "precondition: a real set must be armed, {uncapped:?}");

    for mode in [VenueMode::Paper, VenueMode::Demo, VenueMode::Live] {
        let policy = all_venues_at(mode);

        for venue in clock_venues(REGISTRY, &vars, Some(&policy)) {
            assert!(
                uncapped.contains(&venue.as_str()),
                "{mode:?}: the clock leg reached {venue}, which no account tier arms"
            );
            assert!(
                vike_mount::would_mount_live_under_policy(REGISTRY, &venue, &vars, Some(&policy)),
                "{mode:?}: {venue} is checked but this mount would not arm it"
            );
        }
        for venue in authed_read_probes(REGISTRY, &vars, Some(&policy)).keys() {
            assert!(
                uncapped.contains(&venue.as_str()),
                "{mode:?}: a credential probe was built for {venue}, which is not even armed \
                     at its own tier"
            );
            assert!(
                vike_mount::would_mount_live_under_policy(REGISTRY, venue, &vars, Some(&policy)),
                "{mode:?}: {venue} would be signed for but this mount would not arm it"
            );
        }
        if mode == VenueMode::Paper {
            assert!(!any_venue_would_mount_live(REGISTRY, &vars, Some(&policy)));
        }
    }
}

/// **The offline property, STRENGTHENED rather than merely preserved:** no credentials means no
/// network call under EVERY account tier — `live` included, which is the one that could have
/// regressed. (`paper` holds it for a second, independent reason, and that redundancy is the
/// point of the lane.)
#[test]
fn no_credentials_means_no_network_call_under_every_tier() {
    let empty = HashMap::new();
    for mode in [VenueMode::Paper, VenueMode::Demo, VenueMode::Live] {
        let policy = all_venues_at(mode);
        assert!(clock_venues(REGISTRY, &empty, Some(&policy)).is_empty(), "{mode:?}");
        assert!(authed_read_probes(REGISTRY, &empty, Some(&policy)).is_empty(), "{mode:?}");
        assert!(!any_venue_would_mount_live(REGISTRY, &empty, Some(&policy)), "{mode:?}");
    }
}

/// **An ALL-PAPER box preflights cleanly** — a clean no-op, not a failure, and not a silent
/// skip. Full credential store, no policy at all (`None` ⇒ every venue `paper`, which is the
/// fresh-box default `MountPolicy::default()` carries): the report is exactly the one global
/// network row, NOT-APPLICABLE, `go()` is true and nothing is degraded.
///
/// Offline by construction — not one leg has a venue, so nothing is dialled, which is also why
/// this can be a unit test at all.
///
/// ⚠ The skip is NOT silent: the mount's own `vike_mount::report_capped_to_paper` WARNs per
/// venue whose credentials would have armed it, naming the account verbs that arm it. This test
/// asserts the preflight's half — that being told nothing was checked is never dressed up as a
/// PASS.
#[test]
fn an_all_paper_box_preflights_cleanly() {
    let report = run_startup_preflight(REGISTRY, &credentialled(), &[], None);
    if report.skipped {
        assert!(report.checks.is_empty(), "a skipped preflight runs no check at all");
        return;
    }
    assert_eq!(
        report.checks.len(),
        1,
        "an all-paper box checks no venue at all: {:?}",
        report.lines()
    );
    assert_eq!(report.checks[0].name, CHECK_NETWORK);
    assert_eq!(report.checks[0].status, CheckStatus::NotApplicable);
    assert_ne!(report.checks[0].status, CheckStatus::Pass, "never a fake PASS");
    assert!(report.go(), "an all-paper box is not a no-go");
    assert!(report.degraded_venues().is_empty(), "nothing was checked, so nothing degrades");
    assert!(!report.checks.iter().any(|c| c.name == CHECK_CLOCK_SKEW));
    assert!(!report.checks.iter().any(|c| c.name == CHECK_CREDENTIALS));
}

/// Both key sets, for each switched venue whose mount offers a credential probe.
const BOTH_TIERS: [(&str, &[(&str, &str)]); 3] = [
    (
        "binance",
        &[
            ("BINANCE_DEMO_API_KEY", "demo-key"),
            ("BINANCE_DEMO_API_SECRET", "demo-secret"),
            ("BINANCE_LIVE_API_KEY", "live-key"),
            ("BINANCE_LIVE_API_SECRET", "live-secret"),
        ],
    ),
    (
        "bybit",
        &[
            ("BYBIT_DEMO_API_KEY", "demo-key"),
            ("BYBIT_DEMO_API_SECRET", "demo-secret"),
            ("BYBIT_LIVE_API_KEY", "live-key"),
            ("BYBIT_LIVE_API_SECRET", "live-secret"),
        ],
    ),
    (
        "okx",
        &[
            ("OKX_DEMO_API_KEY", "demo-key"),
            ("OKX_DEMO_API_SECRET", "demo-secret"),
            ("OKX_DEMO_API_PASSPHRASE", "demo-pass"),
            ("OKX_LIVE_API_KEY", "live-key"),
            ("OKX_LIVE_API_SECRET", "live-secret"),
            ("OKX_LIVE_API_PASSPHRASE", "live-pass"),
        ],
    ),
];

/// **A `demo` account still probes the switched venues, and at the DEMO tier.** For binance, bybit
/// and okx — venues whose account tier chooses their network (decision 0095) — a store holding
/// BOTH tiers' keys under a `demo` row arms the venue, so the startup credential leg must build its
/// probe: a venue with no probe is never checked, so never demoted. That PRESENCE half goes through
/// the fold (`authed_read_probes` over the real registry).
///
/// ⚠ The TIER half does not. It is each bridge's own answer to an input this test hands it: the
/// registry row's `credential_probe`, called here with `live_permitted = false`, binds
/// `Tier::Demo`. Whether the fold HANDS a `demo` row `false` is not visible from here — the probe
/// the fold builds is a closure, and its identity record lands only when it reads the venue — so
/// that half is proven over planted tier-following rows in `vike-mount`'s own startup tests
/// (`crates/vike-mount/src/startup_tests/`), a `demo` account and its `live` twin.
///
/// The fourth switched venue, hyperliquid, is not in the loop: its mount offers no credential
/// probe (`crates/bridges/hyperliquid/src/mount.rs`'s `HyperliquidVenueMount` keeps the contract's
/// default `credential_probe`, which answers `None`), so the credential leg never contacts it at
/// any tier.
///
/// Each bridge also pins its own half under
/// `a_demo_ceiling_never_signs_against_the_real_money_tier`.
#[test]
fn a_demo_ceiling_still_probes_the_switched_venues_at_the_demo_tier() {
    let demo = all_venues_at(VenueMode::Demo);
    for (venue, keys) in BOTH_TIERS {
        let vars = vars_of(keys);
        assert!(
            vike_mount::would_mount_live_under_policy(REGISTRY, venue, &vars, Some(&demo)),
            "precondition: {venue}'s demo keys arm its `demo` account"
        );
        assert!(
            authed_read_probes(REGISTRY, &vars, Some(&demo)).contains_key(venue),
            "{venue}: a venue a `demo` row arms must be probed, or the credential leg never \
             checks it"
        );

        let row = REGISTRY
            .iter()
            .find_map(|r| match r {
                VenueRow::Mount(m) if m.venue() == venue => Some(*m),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{venue} is a contract row in the default build"));
        let fixture = MountFixture::new(keys);
        assert!(
            matches!(
                row.credential_probe(&fixture.inputs(false)),
                Some(CredentialProbe::RecordsIdentity { bound_tier: Tier::Demo, .. })
            ),
            "{venue}: handed `live_permitted = false`, the bridge's probe must bind the DEMO keys, \
             never the LIVE ones"
        );
    }
}
