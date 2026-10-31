//! The startup preflight's clock and credential legs driven over the REAL registry — which venues
//! the preflight contacts, under which account tier, with which credentials. They lived in
//! `crates/vike-gatewayhub/src/trade/adapters/startup_tests/mod.rs`, which keeps the tests of the probe bounds and of
//! planted contract rows, until the venue-adapter contract finished (docs/decisions/0096). The
//! account-table helpers below plant one active DEFAULT-account row per roster venue
//! (decision 0119: the row's tier is the only arming there is), and file each account's
//! credentials, BY FIELD, against its own row (decision 0156 item 6: a credential belongs to its
//! account). The deployment map every leg is handed is empty: no credential rides in it.

use std::collections::HashMap;
use std::sync::Arc;

use vike_bridge_core::venue_adapter::CredentialProbe;
use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::trade::adapters::probe::ServerTimeGap;
use vike_gatewayhub::trade::adapters::registry::VenueRow;
use vike_gatewayhub::trade::adapters::startup::venue_server_time_ms;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::preflight::{
    CHECK_CLOCK_SKEW, CHECK_CREDENTIALS, CHECK_NETWORK, CheckStatus,
};
use vike_tradehub_core::startup::withhold_venue_credentials;

use crate::account_book::{book, creds, with_keyed_account};
use crate::local::{
    any_venue_would_go_live, authed_read_probes, bounded_server_time_ms, clock_policies,
    clock_venues, run_startup_preflight,
};

/// One venue's DEFAULT account fields, per venue — a store, by FIELD.
type Store = [(&'static str, &'static [(&'static str, &'static str)])];

/// The tier each roster venue's [`credentialled`] fields arm at: LIVE for the four venues whose
/// network IS the tier (decision 0095), DEMO for every other venue (a demo-only arm binds no other).
///
/// ⚠ There is no "widest" tier any more: an account trades at EXACTLY its own tier (the owner's
/// no-downgrade ruling on decision 0119), so a `live` row of a demo-only arm is PAPER, never demo.
/// The armed policy is therefore per venue, the tier its arm can honour.
fn arming_tier(venue: &str) -> AccountTier {
    if matches!(venue, "binance" | "bybit" | "okx" | "hyperliquid") {
        AccountTier::Live
    } else {
        AccountTier::Demo
    }
}

/// The fields `store` holds for `venue`'s DEFAULT account — none for a venue it does not name.
fn fields_of(store: &Store, venue: &str) -> &'static [(&'static str, &'static str)] {
    store.iter().find(|(v, _)| *v == venue).map_or(&[], |(_, kv)| *kv)
}

/// **Every roster venue's DEFAULT account as an active row at `tier_of(venue)`, each holding its
/// fields from `store`.**
///
/// ⚠ Nearly every test below arms every account, and passing `None` instead would make most of them
/// VACUOUS rather than red: `None` reads all-`paper`, so a test asserting "absent credentials mean
/// no probe" would pass on a box where the credentials are present and the ACCOUNT TABLE is what
/// suppressed them. The credential gate and the tier gate produce the same empty map, so a
/// scenario that leaves every account at paper cannot tell the two apart — and a test that cannot
/// tell them apart is not testing the one it names. Every test that is about CREDENTIALS
/// therefore arms every account, and the tier gets its own tests below.
fn accounts(
    store: &Store,
    tier_of: impl Fn(&str) -> AccountTier,
) -> vike_tradehub_core::AdapterPolicy {
    vike_model::VENUES.iter().copied().fold(
        vike_tradehub_core::AdapterPolicy::default(),
        |p, venue| {
            let tier = tier_of(venue);
            with_keyed_account(p, venue, &AccountLabel::Default, tier, fields_of(store, venue))
        },
    )
}

/// Every account armed at [`arming_tier`], holding nothing.
fn all_armed() -> vike_tradehub_core::AdapterPolicy {
    accounts(&[], arming_tier)
}

/// Every account armed at [`arming_tier`], holding `store`'s fields.
fn all_armed_holding(store: &Store) -> vike_tradehub_core::AdapterPolicy {
    accounts(store, arming_tier)
}

/// Every account as an active row at ONE tier, holding `store`'s fields — the knob the tests below
/// sweep.
fn all_venues_at(mode: AccountTier, store: &Store) -> vike_tradehub_core::AdapterPolicy {
    accounts(store, |_| mode)
}

/// [`all_armed_holding`] with ONE venue's row at `paper` (still holding its fields) — the operator
/// saying "not this one".
fn all_armed_except(paper: &str, store: &Store) -> vike_tradehub_core::AdapterPolicy {
    accounts(store, |venue| if venue == paper { AccountTier::Paper } else { arming_tier(venue) })
}

/// The venues `store` arms at their own tier — through the POLICY-LESS per-venue probe (each
/// venue's fields handed as its account's credentials), so every set below is compared with an
/// answer the policy path did not compute.
fn armed_at_own_tier(store: &Store) -> Vec<&'static str> {
    vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| {
            let held = book(v, 1, fields_of(store, v));
            vike_gatewayhub::trade::adapters::arming::would_go_live_under(
                REGISTRY,
                v,
                &HashMap::new(),
                creds(&held, v, 1),
                arming_tier(v),
            )
        })
        .collect()
}

/// A store that arms a real spread of the roster through each arm's OWN loader — the same set
/// `a_withheld_venue_would_no_longer_start_live` drives over, for the same reason: a tier test
/// whose scenario arms one venue proves almost nothing.
fn credentialled() -> &'static Store {
    &[
        // binance/bybit/okx are armed by a `live` row (`arming_tier`); every other venue here by a
        // `demo` row. The fields are the account's own and carry no tier.
        ("binance", &[("API_KEY", "k"), ("API_SECRET", "s")]),
        ("bybit", &[("API_KEY", "k"), ("API_SECRET", "s")]),
        ("okx", &[("API_KEY", "k"), ("API_SECRET", "s"), ("API_PASSPHRASE", "p")]),
        ("deribit", &[("API_KEY", "k"), ("API_SECRET", "s")]),
        ("alpaca", &[("CLIENT_ID", "cid"), ("CLIENT_SECRET", "csec"), ("ACCOUNT_ID", "acct-1")]),
        // The app registration pair is the VENUE's (filed against the venue); the grant is the
        // account's.
        (
            "ctrader",
            &[
                ("CLIENT_ID", "app"),
                ("CLIENT_SECRET", "app-secret"),
                ("ACCESS_TOKEN", "AT"),
                ("REFRESH_TOKEN", "RT"),
            ],
        ),
        ("ig", &[("API_KEY", "k"), ("IDENTIFIER", "id"), ("PASSWORD", "pw")]),
    ]
}

/// A venue with NO clock leg reports the DECLARED gap (③) carrying its reason — not a fake
/// time, and not the "did not answer" gap that means something is wrong. Every venue exercised
/// here is a `NotWired` row, so this touches no network. (The wired venues are deliberately not
/// exercised: those are real network reads.)
#[test]
fn a_declared_clock_venue_reports_its_reason_rather_than_a_fake_time() {
    let vars = HashMap::new();
    for venue in ["ctrader", "oanda", "alpaca"] {
        match venue_server_time_ms(REGISTRY, venue, &vars, AccountTier::Demo) {
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
    // A pure, network-free live-intent account for a venue that has NO authed-read client here.
    let ig: &Store = &[("ig", &[("API_KEY", "k"), ("IDENTIFIER", "id"), ("PASSWORD", "pw")])];
    let policy = all_armed_holding(ig);
    let vars = HashMap::new();
    assert!(clock_venues(REGISTRY, &vars, Some(&policy)).contains(&"ig".to_string()));
    assert!(
        !authed_read_probes(REGISTRY, &vars, Some(&policy)).contains_key("ig"),
        "precondition: ig offers no credential probe, so the credential leg never lists it"
    );
    // …and every listed venue carries a policy iff its clock is actually wired.
    let policies = clock_policies(REGISTRY, &clock_venues(REGISTRY, &vars, Some(&policy)));
    assert_eq!(policies.get("ig").copied(), crate::local::clock_policy(REGISTRY, "ig"));
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
/// the start announced `exec="LIVE" network="SANDBOX"` while every Alpaca host was unreachable
/// and nothing had asked whether the keys worked
/// (the alpaca+ctrader live rehearsal (PR #1407), Finding B).
///
/// Network-free BY CONSTRUCTION, which is also the claim: building the probes must touch
/// nothing. alpaca's client is a pure `TokenSource`/`AlpacaRest` assembly and ctrader's is
/// deferred into its closure, so this test runs offline with bogus credentials.
#[test]
fn alpaca_and_ctrader_get_a_credential_probe_when_credentialed() {
    let store: &Store = &[
        ("alpaca", &[("CLIENT_ID", "cid"), ("CLIENT_SECRET", "csec"), ("ACCOUNT_ID", "acct-1")]),
        (
            "ctrader",
            &[
                ("CLIENT_ID", "app"),
                ("CLIENT_SECRET", "app-secret"),
                ("ACCESS_TOKEN", "AT"),
                ("REFRESH_TOKEN", "RT"),
            ],
        ),
    ];
    let probes = authed_read_probes(REGISTRY, &HashMap::new(), Some(&all_armed_holding(store)));
    assert!(probes.contains_key("alpaca"), "alpaca must get a credential row");
    assert!(probes.contains_key("ctrader"), "ctrader must get a credential row");
}

/// ⚠ The laziness that makes ctrader's row exist AT ALL. Its `ReconClient` authenticates during
/// construction, so an eagerly-built probe would return `None` for a REFUSED grant — and a
/// venue absent from the probe map gets NO ROW, turning an expired token back into silence.
/// Here the grant is nonsense and the host is never dialled, yet the row is present.
#[test]
fn a_ctrader_grant_that_could_not_connect_still_yields_a_row() {
    let store: &Store = &[(
        "ctrader",
        &[
            ("CLIENT_ID", "app"),
            ("CLIENT_SECRET", "app-secret"),
            ("ACCESS_TOKEN", "definitely-expired"),
            ("REFRESH_TOKEN", "also-expired"),
        ],
    )];
    let probes = authed_read_probes(REGISTRY, &HashMap::new(), Some(&all_armed_holding(store)));
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
            bounded_server_time_ms(REGISTRY, venue, &vars, AccountTier::Demo, &policy),
            venue_server_time_ms(REGISTRY, venue, &vars, AccountTier::Demo),
            "{venue} has no wired endpoint, so bounding it must change nothing"
        );
        assert!(
            !matches!(
                vike_gatewayhub::trade::adapters::server_time::clock_decl(REGISTRY, venue),
                Some(vike_bridge_core::venue_adapter::ClockDecl::Wired { .. })
            ),
            "precondition: {venue} must be a DECLARED row for this test to mean anything"
        );
    }
}

/// THE ENFORCEMENT MECHANISM, closed at the only layer that can see it: withholding a venue's
/// credentials makes `would_go_live_under_policy` FALSE for it — which is what actually turns
/// `make_engine` onto the paper fallback. `crates/vike-tradehub/tests/assemble_runtime_paper.rs`'s
/// `a_per_venue_preflight_fail_withholds_that_venues_credentials` asserts the transformation
/// (it was vike-run's); only this crate can assert what the transformation MEANS.
///
/// Driven over every canonical-roster venue that a plausible credential set can arm, each at its
/// own account's tier, so a venue whose credentials escape the withhold (an application field the
/// venue holds for every account, cTrader's pair) would redden this rather than starting live
/// after being demoted. The credentials are the ACCOUNTS' (decision 0156 item 6), so the withhold
/// takes them out of the account directory the start reads, not out of the deployment map.
#[test]
fn a_withheld_venue_would_no_longer_start_live() {
    // Every live-arm gate this build compiles, spelled as its own loader wants it.
    let policy = all_armed_holding(credentialled());
    let no_vars = HashMap::new();
    let live = |venue: &str, policy: &vike_tradehub_core::AdapterPolicy| {
        crate::local::would_go_live_under_policy(REGISTRY, venue, &no_vars, Some(policy))
    };

    let armed = armed_at_own_tier(credentialled());
    assert!(
        armed.len() >= 5,
        "precondition: this store must arm a real set of venues, armed = {armed:?}"
    );

    for &venue in &armed {
        assert!(live(venue, &policy), "precondition: {venue} is armed by its own row");
        let (kept, withheld) = withhold_venue_credentials(&policy.accounts, venue);
        assert!(withheld > 0, "{venue} was armed, so it must have had fields to withhold");
        let demoted = vike_tradehub_core::AdapterPolicy { accounts: kept, ..policy.clone() };
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
    let alpaca_partial: &Store = &[("alpaca", &[("CLIENT_ID", "cid"), ("CLIENT_SECRET", "csec")])];
    assert!(
        !authed_read_probes(REGISTRY, &HashMap::new(), Some(&all_armed_holding(alpaca_partial)))
            .contains_key("alpaca")
    );

    let ctrader_partial: &Store =
        &[("ctrader", &[("CLIENT_ID", "app"), ("CLIENT_SECRET", "sec"), ("ACCESS_TOKEN", "AT")])];
    assert!(
        !authed_read_probes(REGISTRY, &HashMap::new(), Some(&all_armed_holding(ctrader_partial)))
            .contains_key("ctrader")
    );
}

/// THE default path: no credentials runs the preflight with ZERO network — no venue is checked
/// (no creds ⇒ no authed-read client) and no `NetProbe` is spawned (no venue would go live).
/// With nothing live, the single network row is DECLARED not-applicable rather than WARNing a
/// developer's TODO at an operator on every paper start — and it still never claims a measurement
/// it did not take. This is the shape `vike_tradehub_core::assemble_runtime` sees on every paper/CI start.
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
/// row of `vike_tradehub_core::server_time`'s table, `X-IG-API-KEY`; it is the ig bridge's declared clock
/// now). `paper` means "do not touch this account".
///
/// ⚠ The precondition is the half that makes this non-vacuous: with ig's row ARMED the very same
/// fields DO contact ig, so the assertions below are about the account's tier rather than about a
/// store that never armed anything.
#[test]
fn a_paper_capped_venue_with_credentials_is_never_contacted() {
    let vars = HashMap::new();

    let wide = all_armed_holding(credentialled());
    assert!(
        clock_venues(REGISTRY, &vars, Some(&wide)).contains(&"ig".to_string()),
        "precondition: these credentials DO arm ig, so holding it at paper is what changes the \
         answer"
    );

    let capped = all_armed_except("ig", credentialled());
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
    let vars = HashMap::new();
    let capped = all_armed_except("binance", credentialled());

    let probes = authed_read_probes(REGISTRY, &vars, Some(&capped));
    assert!(!probes.contains_key("binance"), "binance is `paper`: nothing may be signed for it");
    assert!(probes.contains_key("bybit"), "bybit was left armed and must still be checked");
    assert!(probes.contains_key("okx"), "okx was left armed and must still be checked");

    let clocks = clock_venues(REGISTRY, &vars, Some(&capped));
    assert!(!clocks.contains(&"binance".to_string()));
    assert!(clocks.contains(&"bybit".to_string()));
    assert!(clocks.contains(&"okx".to_string()));
}

/// **An armed venue is checked EXACTLY as before.** With every account armed at the tier its arm
/// honours, the clock list is the per-venue probe's answer, venue for venue — so this lane narrows
/// what a paper deployment contacts and changes nothing for an armed one.
#[test]
fn an_armed_venue_is_checked_exactly_as_before() {
    let vars = HashMap::new();
    let wide = all_armed_holding(credentialled());

    let before: Vec<String> =
        armed_at_own_tier(credentialled()).into_iter().map(str::to_string).collect();
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
    assert!(any_venue_would_go_live(REGISTRY, &vars, Some(&wide)), "…and the net probe still arms");
}

/// **THE PROPERTY, asserted rather than assumed: a tier can only ever NARROW what is
/// contacted.** For every uniform tier each derived set is a SUBSET of the venues these fields arm
/// at their own tier, and every element of it is a venue the start would itself arm at that tier.
///
/// A subset check is the honest shape here. "Fewer venues" would pass for a fix that dropped
/// the wrong venue, and "equal under `live`" alone would say nothing about `demo`. Under the
/// no-downgrade rule the uniform `live` set is the switched venues' alone (a demo-only arm's `live`
/// account is refused by name), and the uniform `demo` set is every venue whose arm has a demo
/// tier: neither reaches a venue its fields cannot arm.
#[test]
fn the_tier_can_only_narrow_what_is_contacted() {
    let vars = HashMap::new();
    let uncapped = armed_at_own_tier(credentialled());
    assert!(uncapped.len() >= 5, "precondition: a real set must be armed, {uncapped:?}");

    for mode in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        let policy = all_venues_at(mode, credentialled());

        for venue in clock_venues(REGISTRY, &vars, Some(&policy)) {
            assert!(
                uncapped.contains(&venue.as_str()),
                "{mode:?}: the clock leg reached {venue}, which no account tier arms"
            );
            assert!(
                crate::local::would_go_live_under_policy(REGISTRY, &venue, &vars, Some(&policy)),
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
                crate::local::would_go_live_under_policy(REGISTRY, venue, &vars, Some(&policy)),
                "{mode:?}: {venue} would be signed for but this mount would not arm it"
            );
        }
        if mode == AccountTier::Paper {
            assert!(!any_venue_would_go_live(REGISTRY, &vars, Some(&policy)));
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
    for mode in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        let policy = all_venues_at(mode, &[]);
        assert!(clock_venues(REGISTRY, &empty, Some(&policy)).is_empty(), "{mode:?}");
        assert!(authed_read_probes(REGISTRY, &empty, Some(&policy)).is_empty(), "{mode:?}");
        assert!(!any_venue_would_go_live(REGISTRY, &empty, Some(&policy)), "{mode:?}");
    }
}

/// **An ALL-PAPER box preflights cleanly** — a clean no-op, not a failure, and not a silent
/// skip. Two all-paper boxes, each the same answer: every account a `paper` row HOLDING its
/// credentials, and no policy at all (`None` ⇒ every venue `paper`, the fresh-box default
/// `AdapterPolicy::default()` carries). The report is exactly the one global network row,
/// NOT-APPLICABLE, `go()` is true and nothing is degraded.
///
/// Offline by construction — not one leg has a venue, so nothing is dialled, which is also why
/// this can be a unit test at all.
///
/// ⚠ The skip is NOT silent: the start's own `vike_tradehub_core::report_capped_to_paper` WARNs per
/// venue whose credentials would have armed it, naming the account verbs that arm it. This test
/// asserts the preflight's half — that being told nothing was checked is never dressed up as a
/// PASS.
#[test]
fn an_all_paper_box_preflights_cleanly() {
    let held_at_paper = all_venues_at(AccountTier::Paper, credentialled());
    for (shape, policy) in [("paper rows holding keys", Some(&held_at_paper)), ("no policy", None)]
    {
        let report = run_startup_preflight(REGISTRY, &HashMap::new(), &[], policy);
        if report.skipped {
            assert!(report.checks.is_empty(), "{shape}: a skipped preflight runs no check at all");
            return;
        }
        assert_eq!(
            report.checks.len(),
            1,
            "{shape}: an all-paper box checks no venue at all: {:?}",
            report.lines()
        );
        assert_eq!(report.checks[0].name, CHECK_NETWORK);
        assert_eq!(report.checks[0].status, CheckStatus::NotApplicable);
        assert_ne!(report.checks[0].status, CheckStatus::Pass, "{shape}: never a fake PASS");
        assert!(report.go(), "{shape}: an all-paper box is not a no-go");
        assert!(report.degraded_venues().is_empty(), "{shape}: nothing was checked");
        assert!(!report.checks.iter().any(|c| c.name == CHECK_CLOCK_SKEW));
        assert!(!report.checks.iter().any(|c| c.name == CHECK_CREDENTIALS));
    }
}

/// One `demo` account's key set, for each switched venue whose start offers a credential probe.
const SWITCHED: [(&str, &[(&str, &str)]); 3] = [
    ("binance", &[("API_KEY", "demo-key"), ("API_SECRET", "demo-secret")]),
    ("bybit", &[("API_KEY", "demo-key"), ("API_SECRET", "demo-secret")]),
    (
        "okx",
        &[("API_KEY", "demo-key"), ("API_SECRET", "demo-secret"), ("API_PASSPHRASE", "demo-pass")],
    ),
];

/// **A `demo` account still probes the switched venues, and at the DEMO tier.** For binance, bybit
/// and okx — venues whose account tier chooses their network (decision 0095) — a `demo` row holding
/// its key set arms the venue, so the startup credential leg must build its probe: a venue with no
/// probe is never checked, so never demoted. That PRESENCE half goes through the fold
/// (`authed_read_probes` over the real registry).
///
/// ⚠ The TIER half does not. It is each bridge's own answer to an input this test hands it: the
/// registry row's `credential_probe`, called here at the `demo` tier, binds
/// `AccountTier::Demo`. Whether the fold HANDS a `demo` row that tier is not visible from here — the probe
/// the fold builds is a closure, and its identity record lands only when it reads the venue — so
/// that half is proven over planted tier-following rows in `vike-tradehub-core`'s own startup tests
/// (`crates/vike-gatewayhub/src/trade/adapters/startup_tests/`), a `demo` account and its `live` twin.
///
/// The fourth switched venue, hyperliquid, is not in the loop: its start offers no credential
/// probe (`crates/bridges/hyperliquid/src/adapter.rs`'s `HyperliquidVenueAdapter` keeps the contract's
/// default `credential_probe`, which answers `None`), so the credential leg never contacts it at
/// any tier.
///
/// Each bridge also pins its own half under
/// `a_demo_ceiling_never_signs_against_the_real_money_tier`.
#[test]
fn a_demo_ceiling_still_probes_the_switched_venues_at_the_demo_tier() {
    for (venue, keys) in SWITCHED {
        let store: &Store = &[(venue, keys)];
        let demo = all_venues_at(AccountTier::Demo, store);
        let vars = HashMap::new();
        assert!(
            crate::local::would_go_live_under_policy(REGISTRY, venue, &vars, Some(&demo)),
            "precondition: {venue}'s key set arms its `demo` account"
        );
        assert!(
            authed_read_probes(REGISTRY, &vars, Some(&demo)).contains_key(venue),
            "{venue}: a venue a `demo` row arms must be probed, or the credential leg never \
             checks it"
        );

        let row = REGISTRY
            .iter()
            .find_map(|r| match r {
                VenueRow::Adapter(m) if m.venue() == venue => Some(*m),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{venue} is a contract row in the default build"));
        let fixture = AdapterFixture::new(keys);
        assert!(
            matches!(
                row.credential_probe(&fixture.inputs(AccountTier::Demo)),
                Some(CredentialProbe::RecordsIdentity { bound_tier: AccountTier::Demo, .. })
            ),
            "{venue}: handed the `demo` tier, the bridge's probe must bind the DEMO network, \
             never the LIVE one"
        );
    }
}
