use super::*;
use crate::transition::LEGACY_REGISTRY;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::preflight::{CHECK_CLOCK_SKEW, CHECK_CREDENTIALS, CHECK_NETWORK, CheckStatus};
use vike_config::VenueMode;

/// **The widest arming ceiling — every roster venue at `live`.**
///
/// ⚠ Nearly every test below needs this, and passing `None` instead would make most of them
/// VACUOUS rather than red: `None` reads all-`paper`, so a test asserting "absent credentials
/// mean no probe" would pass on a box where the credentials are present and the CEILING is
/// what suppressed them. The credential gate and the ceiling gate produce the same empty map,
/// so a scenario that leaves the ceiling closed cannot tell the two apart — and a test that
/// cannot tell them apart is not testing the one it names. Every test that is about
/// CREDENTIALS therefore holds the ceiling wide open, and the ceiling gets its own tests below.
fn all_live() -> crate::MountPolicy {
    all_venues_at(VenueMode::Live)
}

/// Every roster venue DECLARED at one ceiling — the knob the tests below sweep.
fn all_venues_at(mode: VenueMode) -> crate::MountPolicy {
    let mut venues = vike_config::VenuePolicy::default();
    for venue in vike_model::VENUES {
        venues = venues.declare(venue, mode);
    }
    crate::MountPolicy { venues, ..crate::MountPolicy::default() }
}

/// The widest ceiling with ONE venue capped to `paper` — the operator saying "not this one".
fn all_live_except(paper: &str) -> crate::MountPolicy {
    let mut policy = all_live();
    policy.venues = policy.venues.declare(paper, VenueMode::Paper);
    policy
}

fn vars_of(kv: &[(&str, &str)]) -> HashMap<String, String> {
    kv.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// A credential map that arms a real spread of the roster through each arm's OWN loader — the
/// same set `a_withheld_venue_would_no_longer_mount_live` drives over, for the same reason: a
/// ceiling test whose scenario arms one venue proves almost nothing.
fn credentialled() -> HashMap<String, String> {
    vars_of(&[
        // Decision 0095: LIVE-tier for binance/bybit/okx — `all_live()` caps every venue at
        // `Live`, and those three now require LIVE-tier keys specifically to reach anything but
        // Paper under a `live` ceiling (a mainnet host is never signed with demo keys). Deribit's
        // network is never the ceiling, so it stays DEMO.
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

/// Every row's venue is on the canonical roster, and its symbol is non-empty — the same
/// tie-in `vike_run::WIRED_MARKETS`' own test uses, so a typo here cannot silently check
/// nothing.
#[test]
fn authed_read_markets_are_on_the_canonical_roster() {
    for &(venue, symbol) in AUTHED_READ_MARKETS {
        assert!(
            vike_model::VENUES.contains(&venue),
            "AUTHED_READ_MARKETS names {venue}, which is not in vike_model::VENUES"
        );
        assert!(!symbol.is_empty(), "{venue} must name the symbol its recon client scopes to");
    }
}

/// Every listed venue must actually be one `build_recon_client` can build for — otherwise the
/// credential leg would FAIL a venue purely because this table over-claims.
#[test]
fn every_authed_read_market_yields_a_recon_client_when_credentialed() {
    let creds = vike_bridge_core::Credentials {
        api_key: "test-key".to_string(),
        api_secret: "test-secret".to_string(),
        passphrase: Some("test-pass".to_string()),
    };
    for &(venue, symbol) in AUTHED_READ_MARKETS {
        assert!(
            crate::build_recon_client(
                venue,
                symbol,
                &creds,
                crate::fallback::OKX_FALLBACK_CTVAL,
                // Demo tier: this asserts the table/factory agree, and construction is pure —
                // the tier only picks a host, which no assertion here reads.
                false,
            )
            .is_some(),
            "{venue} is listed for the credential leg but builds no ReconClient"
        );
    }
}

/// Absent credentials ⇒ no client, hence no checked venue — the live gate, unchanged. Pure:
/// client construction never touches the network.
#[test]
fn no_credentials_means_no_authed_read_clients() {
    assert!(authed_read_clients(LEGACY_REGISTRY, &HashMap::new(), Some(&all_live())).is_empty());
}

/// A venue with NO clock leg reports the DECLARED gap (③) carrying its reason — not a fake
/// time, and not the "did not answer" gap that means something is wrong. Every venue exercised
/// here is a `NotWired` row, so this touches no network. (The wired venues are deliberately not
/// exercised: those are real network reads.)
#[test]
fn a_declared_clock_venue_reports_its_reason_rather_than_a_fake_time() {
    let vars = HashMap::new();
    for venue in ["ctrader", "oanda", "alpaca"] {
        match venue_server_time_ms(LEGACY_REGISTRY, venue, &vars, false) {
            Err(ServerTimeGap::NotChecked(reason)) => {
                assert!(!reason.is_empty(), "{venue} declares no reason");
            }
            other => panic!("{venue} must report a DECLARED gap, got {other:?}"),
        }
    }
}

/// An off-roster string is the OTHER gap: it says nothing about a venue, so it must not read as
/// a declaration.
#[test]
fn an_unknown_venue_is_not_a_declaration() {
    let vars = HashMap::new();
    match venue_server_time_ms(LEGACY_REGISTRY, "not-a-venue", &vars, false) {
        Err(ServerTimeGap::Unreachable(e)) => assert!(e.contains("not-a-venue"), "{e}"),
        other => panic!("an unknown venue must not read as declared: {other:?}"),
    }
}

/// THE decoupling, at the wiring site: the clock list is derived from live INTENT over the
/// canonical roster, so it is not confined to the venues `build_recon_client` can build for —
/// which is what kept every non-CEX venue's clock unmeasured no matter what was wired.
#[test]
fn the_clock_venue_list_is_not_confined_to_the_authed_read_markets() {
    // A pure, network-free live-intent map for a venue that has NO authed-read client here.
    let vars: HashMap<String, String> =
        [("IG_DEMO_API_KEY", "k"), ("IG_DEMO_IDENTIFIER", "id"), ("IG_DEMO_PASSWORD", "pw")]
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
    assert!(clock_venues(LEGACY_REGISTRY, &vars, Some(&all_live())).contains(&"ig".to_string()));
    assert!(
        !AUTHED_READ_MARKETS.iter().any(|(v, _)| *v == "ig"),
        "precondition: ig has no authed-read client, so the old shared list excluded it"
    );
    assert!(
        authed_read_clients(LEGACY_REGISTRY, &vars, Some(&all_live())).is_empty(),
        "…and still does"
    );
    // …and every listed venue carries a policy iff its clock is actually wired.
    let policies =
        clock_policies(LEGACY_REGISTRY, &clock_venues(LEGACY_REGISTRY, &vars, Some(&all_live())));
    assert_eq!(
        policies.get("ig").copied(),
        crate::server_time::clock_policy(LEGACY_REGISTRY, "ig")
    );
    assert_eq!(
        policies["ig"].fail_ms, None,
        "ig's auth stamps no timestamp, so its clock leg may never degrade it to paper"
    );
}

/// No credentials ⇒ no live intent ⇒ no clock legs, so the offline property survives the
/// decoupling.
#[test]
fn no_credentials_means_no_clock_venues() {
    assert!(clock_venues(LEGACY_REGISTRY, &HashMap::new(), Some(&all_live())).is_empty());
}

/// The credential leg over an EMPTY probe map errors rather than silently passing — the
/// property that makes deriving the venue list from the map load-bearing.
#[test]
fn the_authed_read_probe_errors_for_an_unbuilt_venue() {
    let e = authed_read_probe(CredentialProbes::new())("binance").expect_err("no client was built");
    // …and it is the CONFIRMED half: an absent probe is a defect in our own wiring, and must
    // not be able to read as the "we never heard back" gap, which never demotes a venue.
    match e {
        CredentialGap::Rejected(msg) => assert!(msg.contains("binance"), "{msg}"),
        other => panic!("an unbuilt venue must be Rejected, not {other:?}"),
    }
}

/// Every inline row names a canonical-roster venue and a non-empty symbol, and `symbol_for`
/// resolves it — the tie-in that stops a typo from silently scoping a probe to `""`.
#[test]
fn every_inline_authed_read_market_is_tabled() {
    for &(venue, symbol) in INLINE_AUTHED_READ_MARKETS {
        assert!(
            vike_model::VENUES.contains(&venue),
            "INLINE_AUTHED_READ_MARKETS names {venue}, which is not in vike_model::VENUES"
        );
        assert!(!symbol.is_empty(), "{venue} must name the symbol its recon client scopes to");
        assert_eq!(symbol_for(venue), symbol, "symbol_for must resolve {venue}");
    }
    // …and the two tables are disjoint: a venue built BOTH ways would insert twice and the
    // second write would silently win.
    for &(venue, _) in INLINE_AUTHED_READ_MARKETS {
        assert!(
            !AUTHED_READ_MARKETS.iter().any(|(v, _)| *v == venue),
            "{venue} is in both probe tables"
        );
    }
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

    let probes = authed_read_probes(LEGACY_REGISTRY, &vars, Some(&all_live()));
    assert!(probes.contains_key("alpaca"), "alpaca must get a credential row");
    assert!(probes.contains_key("ctrader"), "ctrader must get a credential row");
    // …and neither is reachable through `build_recon_client`, which is why they needed their
    // own construction rather than a row in AUTHED_READ_MARKETS.
    assert!(!AUTHED_READ_MARKETS.iter().any(|(v, _)| *v == "alpaca" || *v == "ctrader"));
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

    let probes = authed_read_probes(LEGACY_REGISTRY, &vars, Some(&all_live()));
    assert!(
        probes.contains_key("ctrader"),
        "a refused grant must still be CHECKED — an absent row is the silent failure this leg \
             exists to remove"
    );
}

/// THE BOUND, and the thing that was missing for the credential leg's whole life: a probe that
/// never answers is ABANDONED at [`CREDENTIAL_PROBE_TIMEOUT`], not waited out.
///
/// The fake probe parks for far longer than the bound — the shape of the ~20 s geo-blocked
/// alpaca TCP connect measured on the dev box — and the assertion is on WALL CLOCK: the call
/// must return in about the bound, not in about the probe's own duration.
#[test]
fn an_unresponsive_credential_probe_is_abandoned_at_the_bound() {
    let probe: CredentialProbe = Arc::new(|| {
        std::thread::sleep(CREDENTIAL_PROBE_TIMEOUT * 6);
        Ok(())
    });
    let t0 = Instant::now();
    let outcome = bounded_probe(&probe, CREDENTIAL_PROBE_TIMEOUT, CREDENTIAL_PROBE_THREAD);
    let waited = t0.elapsed();
    assert!(outcome.is_err(), "a probe that did not answer must not report a verdict");
    assert!(
        waited < CREDENTIAL_PROBE_TIMEOUT * 3,
        "the mount waited {waited:?}, i.e. it waited the PROBE out rather than its own bound"
    );
}

/// …and the other half: a probe that answers inside the bound is not disturbed by it.
#[test]
fn a_prompt_credential_probe_answers_through_the_bound() {
    let ok: CredentialProbe = Arc::new(|| Ok(()));
    assert_eq!(bounded_probe(&ok, CREDENTIAL_PROBE_TIMEOUT, CREDENTIAL_PROBE_THREAD), Ok(Ok(())));
    let refused: CredentialProbe = Arc::new(|| Err("401 invalid api key".to_string()));
    assert_eq!(
        bounded_probe(&refused, CREDENTIAL_PROBE_TIMEOUT, CREDENTIAL_PROBE_THREAD),
        Ok(Err("401 invalid api key".to_string()))
    );
}

/// THE CLOCK leg's half of the same bound: a read that never answers is ABANDONED at
/// [`CLOCK_PROBE_TIMEOUT`], not waited out.
///
/// The fake probe parks far longer than the bound — the shape of a wedged `getaddrinfo`, which
/// is the ONE case `crate::server_time::CLOCK_READ_TIMEOUT` structurally cannot preempt — and
/// the assertion is on WALL CLOCK: the call must return in about the mount's bound, not in
/// about the read's own duration. Offline: the probe is a sleep, not a fetch.
#[test]
fn an_unresponsive_clock_read_is_abandoned_at_the_bound() {
    let probe: BoundedProbe<Result<i64, ServerTimeGap>> = Arc::new(|| {
        std::thread::sleep(CLOCK_PROBE_TIMEOUT * 6);
        Ok(0)
    });
    let t0 = Instant::now();
    let outcome = bounded_probe(&probe, CLOCK_PROBE_TIMEOUT, CLOCK_PROBE_THREAD);
    let waited = t0.elapsed();
    assert!(outcome.is_err(), "a read that did not answer must not report a reading");
    assert!(
        waited < CLOCK_PROBE_TIMEOUT * 3,
        "the mount waited {waited:?}, i.e. it waited the READ out rather than its own bound"
    );
}

/// …and the abandoned read reports outcome ② — a WARN that degrades nothing — never a
/// DECLARATION. `NotChecked` would render NOT-APPLICABLE, printing "nothing to check here" over
/// a venue whose clock we failed to reach; the whole point of the split is that those two are
/// opposite facts.
#[test]
fn an_abandoned_clock_read_is_unreachable_and_never_a_declaration() {
    let waited_ms = 4_000;
    assert_eq!(
        u128::from(waited_ms),
        CLOCK_PROBE_TIMEOUT.as_millis(),
        "the bound this row reports must be the one the leg actually waits"
    );
    match abandoned_clock_gap(waited_ms) {
        ServerTimeGap::Unreachable(msg) => {
            assert!(msg.contains("abandoned"), "the row must say what happened: {msg}");
            assert!(msg.contains("4000"), "…and name the bound it waited: {msg}");
        }
        other => panic!("an abandoned read must be Unreachable, got {other:?}"),
    }
}

/// The abandon ceiling sits ABOVE the transport's own timeout, and that gap is load-bearing
/// rather than slack: at equal values this bound — which starts marginally earlier — would
/// abandon essentially every ordinary timeout a hair before ureq reported it, trading the
/// venue's own error text for a generic "did not answer" on every timed-out read.
#[test]
fn the_clock_abandon_ceiling_sits_above_the_transports_own_timeout() {
    assert!(
        CLOCK_PROBE_TIMEOUT > crate::server_time::CLOCK_READ_TIMEOUT,
        "a read ureq CAN bound must report itself before the mount walks away from it"
    );
}

/// The thread names exist so a stack dump can say WHICH leg's abandoned worker is parked, and
/// that claim is only true on Linux if each name fits [`THREAD_NAME_MAX_BYTES`]: `std`
/// truncates a longer one silently, and the shipped credential name plus the clock name's first
/// draft truncated onto the SAME prefix. Distinctness is asserted too — two names that fit but
/// collide would fail the same reader.
#[test]
fn probe_thread_names_survive_linux_truncation() {
    for name in [CREDENTIAL_PROBE_THREAD, CLOCK_PROBE_THREAD] {
        assert!(
            name.len() <= THREAD_NAME_MAX_BYTES,
            "{name:?} is {} bytes; Linux keeps {THREAD_NAME_MAX_BYTES}, so a stack dump would \
                 not show it",
            name.len()
        );
    }
    assert_ne!(
        CREDENTIAL_PROBE_THREAD, CLOCK_PROBE_THREAD,
        "the two legs' workers must be tellable apart in a dump"
    );
}

/// A DECLARED venue is answered from the table, unchanged — no probe, so no thread. This is
/// what keeps the offline property exactly as it was: the bound is paid only by a read that can
/// actually block. Offline by construction (every venue here is a `NotWired` row).
#[test]
fn a_declared_clock_venue_is_answered_without_the_probe() {
    let vars = Arc::new(HashMap::new());
    for venue in ["ctrader", "oanda", "alpaca"] {
        assert_eq!(
            bounded_server_time_ms(LEGACY_REGISTRY, venue, &vars, false),
            venue_server_time_ms(LEGACY_REGISTRY, venue, &vars, false),
            "{venue} has no wired endpoint, so bounding it must change nothing"
        );
        assert!(
            !matches!(
                crate::server_time::clock_decl(LEGACY_REGISTRY, venue),
                Some(vike_bridge_core::venue_mount::ClockDecl::Wired { .. })
            ),
            "precondition: {venue} must be a DECLARED row for this test to mean anything"
        );
    }
}

/// THE confirmed-vs-transient split, at the leg: the two `Err` shapes are what decide whether a
/// venue is demoted, so they must never be reachable from each other's cause.
///
/// A venue that ANSWERS and refuses is `Rejected` — and it is only reported after the RETRY, so
/// a demotion needs the venue to refuse twice.
#[test]
fn an_answering_venue_is_rejected_and_is_retried_before_it_is_believed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let mut probes = CredentialProbes::new();
    probes.insert(
        "okx".to_string(),
        Arc::new(move || {
            seen.fetch_add(1, Ordering::Relaxed);
            Err("401 invalid api key".to_string())
        }) as CredentialProbe,
    );
    match authed_read_probe(probes)("okx") {
        Err(CredentialGap::Rejected(msg)) => assert!(msg.contains("401"), "{msg}"),
        other => panic!("an answering venue must be Rejected, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        CREDENTIAL_PROBE_ATTEMPTS,
        "a demotion must be CONFIRMED — one refusal is a blip, not a verdict"
    );
}

/// …and the venue that never answers is `Unanswered`, which never demotes. ⚠ It must ALSO not
/// be retried: re-waiting the bound would double the leg's worst case to learn the same nothing
/// twice, which is the arithmetic the whole bound exists to cap.
#[test]
fn a_silent_venue_is_unanswered_and_is_not_retried() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let mut probes = CredentialProbes::new();
    probes.insert(
        "alpaca".to_string(),
        Arc::new(move || {
            seen.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(CREDENTIAL_PROBE_TIMEOUT * 6);
            Ok(())
        }) as CredentialProbe,
    );
    match authed_read_probe(probes)("alpaca") {
        Err(CredentialGap::Unanswered { waited_ms, detail }) => {
            assert!(waited_ms > 0, "the row must state its own bound");
            assert!(!detail.is_empty());
        }
        other => panic!("a silent venue must be Unanswered, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::Relaxed), 1, "a timeout is never retried");
}

/// THE ENFORCEMENT MECHANISM, closed at the only layer that can see it: withholding a venue's
/// credentials makes `would_mount_live` FALSE for it — which is what actually turns
/// `make_engine` onto the paper fallback. `vike-run` asserts the map transformation; only this
/// crate can assert what the map transformation MEANS.
///
/// Driven over every canonical-roster venue that a plausible credential set can arm, so a venue
/// whose key spelling escapes the `{VENUE}_` prefix rule would redden this rather than mounting
/// live after being demoted.
#[test]
fn a_withheld_venue_would_no_longer_mount_live() {
    // Every live-arm gate this build compiles, spelled as its own loader wants it.
    let vars = credentialled();

    let armed: Vec<&str> = vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| crate::would_mount_live(LEGACY_REGISTRY, v, &vars))
        .collect();
    assert!(
        armed.len() >= 5,
        "precondition: this map must arm a real set of venues, armed = {armed:?}"
    );

    for venue in &armed {
        let mut demoted = vars.clone();
        let withheld = withhold_venue_credentials(&mut demoted, venue);
        assert!(withheld > 0, "{venue} was armed, so it must have had keys to withhold");
        assert!(
            !crate::would_mount_live(LEGACY_REGISTRY, venue, &demoted),
            "{venue} still reads as live-intent after its credentials were withheld — the \
                 preflight's demotion would be announced and then not happen"
        );
        // …and ONLY that venue moved: preflight demotes one venue, never a neighbour.
        for other in armed.iter().filter(|o| *o != venue) {
            assert!(
                crate::would_mount_live(LEGACY_REGISTRY, other, &demoted),
                "withholding {venue} also demoted {other}"
            );
        }
    }
}

/// THE DISK LEG, armed: the probe returns a real number for a real directory, and an ERROR
/// (which the leg renders as a WARN, never a fake PASS) for one that is not there.
///
/// ⚠ On Windows both halves take the declared-gap path, which is the honest answer there rather
/// than a skipped test — so this asserts the CONTRACT (`Ok` is plausible, `Err` names a reason)
/// on both platforms and the measurement only where it can be taken. What it cannot see is the
/// `f_bavail`-vs-`f_bfree` choice: on an unreserved filesystem the two are equal, so that
/// remains a documented judgement (see [`free_space_bytes`]) rather than a pinned one.
#[test]
fn the_disk_probe_measures_a_real_directory_and_declares_a_missing_one() {
    let here = std::env::temp_dir();
    let measured = free_space_bytes(&here);
    // ⚠ The two arms are `#[cfg]`-selected rather than branched on `cfg!(…)`: a runtime `if`
    // over a compile-time constant makes clippy's `assertions_on_constants` fire under the
    // `-D warnings` gate, and it would also let the WRONG arm compile-check into nothing.
    #[cfg(unix)]
    {
        let free = measured.expect("unix must MEASURE, not declare");
        assert!(free > 0, "a writable temp dir reporting ZERO free bytes is not a reading");
        // A directory that does not exist is an ERROR, so `check_disk_headroom` WARNs naming
        // it — never a silent absence, and never a PASS.
        let missing = here.join("vike-preflight-no-such-dir-8f3a1c");
        assert!(free_space_bytes(&missing).is_err(), "an absent directory cannot be measured");
    }
    #[cfg(not(unix))]
    {
        let reason = measured.expect_err("this platform cannot measure, so it must DECLARE");
        assert!(!reason.is_empty(), "a declared gap must carry its reason");
    }
}

/// The watched set is deduplicated by PATH, not by label: a journal written inside the store
/// root is ONE filesystem, and two rows for it would double every finding without adding a fact.
#[test]
fn the_disk_dirs_are_deduplicated_by_path() {
    let p = PathBuf::from("/srv/vike/data");
    let out = disk_dirs(&[
        ("journal".to_string(), p.clone()),
        ("hist-store".to_string(), p.clone()),
        ("other".to_string(), PathBuf::from("/srv/vike/other")),
    ]);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(out[0].0, "journal", "the FIRST label wins, so the order is deterministic");
}

/// Absent credentials ⇒ no probe for either venue, so the offline/paper property is intact.
#[test]
fn no_credentials_means_no_credential_probes() {
    assert!(authed_read_probes(LEGACY_REGISTRY, &HashMap::new(), Some(&all_live())).is_empty());
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
        !authed_read_probes(LEGACY_REGISTRY, &alpaca_partial, Some(&all_live()))
            .contains_key("alpaca")
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
        !authed_read_probes(LEGACY_REGISTRY, &ctrader_partial, Some(&all_live()))
            .contains_key("ctrader")
    );
}

/// THE default path: an empty credentials map runs the preflight with ZERO network — no venue
/// is checked (no creds ⇒ no authed-read client) and no `NetProbe` is spawned (no venue would
/// mount live). With nothing live, the single network row is DECLARED not-applicable rather
/// than WARNing a developer's TODO at an operator on every paper start — and it still never
/// claims a measurement it did not take. This is the shape `vike_run::build_node` sees on
/// every paper/CI mount.
#[test]
fn an_empty_credentials_map_preflights_offline() {
    // ⚠ The WIDEST ceiling, deliberately: the claim is that ABSENT CREDENTIALS keep this
    // offline, and `None` (all-`paper`) would keep it offline for the other reason — the same
    // empty report from a different cause, which is not what this test's name says.
    let report = run_startup_preflight(LEGACY_REGISTRY, &HashMap::new(), &[], Some(&all_live()));
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

// ─── THE ARMING CEILING reaches the preflight ──────────────────────────────────────────────
//
// Every test in this block drives the SAME credential map (`credentialled()`, which arms a real
// spread of the roster) and varies ONLY the ceiling, so a green result can be attributed to the
// ceiling and to nothing else. Offline by construction: every predicate under test is one of
// the pure, network-free live-intent probes, and no probe closure is ever CALLED.

/// **THE DEFECT, as a test.** A venue with valid credentials that the operator capped to
/// `paper` is not contacted by ANY leg — it is absent from the clock list and from the
/// credential probe map.
///
/// The measured incident: a box whose `policy.toml` said `ig = "paper"` still had the preflight
/// reach out to IG at every start, and IG's clock row is the one CREDENTIALED read in
/// `crate::server_time`'s table (`X-IG-API-KEY`). `paper` means "do not touch this account".
///
/// ⚠ The precondition is the half that makes this non-vacuous: under the WIDEST ceiling the
/// very same map DOES contact ig, so the assertions below are about the ceiling rather than
/// about a map that never armed anything.
#[test]
fn a_paper_capped_venue_with_credentials_is_never_contacted() {
    let vars = credentialled();

    let wide = all_live();
    assert!(
        clock_venues(LEGACY_REGISTRY, &vars, Some(&wide)).contains(&"ig".to_string()),
        "precondition: these credentials DO arm ig, so capping it is what changes the answer"
    );

    let capped = all_live_except("ig");
    assert!(
        !clock_venues(LEGACY_REGISTRY, &vars, Some(&capped)).contains(&"ig".to_string()),
        "ig is capped to `paper` and the preflight still reads its clock — with its API key"
    );
    assert!(
        !authed_read_probes(LEGACY_REGISTRY, &vars, Some(&capped)).contains_key("ig"),
        "a capped venue must not get a credential probe either"
    );
}

/// …and the same for a venue whose probe SIGNS a balance read, which is the account-scoped leg.
/// binance is capped; bybit and okx, credentialled identically and left armed, must be
/// untouched — a ceiling that disarmed a neighbour would be its own defect.
#[test]
fn capping_one_venue_leaves_its_neighbours_checked() {
    let vars = credentialled();
    let capped = all_live_except("binance");

    let probes = authed_read_probes(LEGACY_REGISTRY, &vars, Some(&capped));
    assert!(!probes.contains_key("binance"), "binance is `paper`: nothing may be signed for it");
    assert!(probes.contains_key("bybit"), "bybit was left armed and must still be checked");
    assert!(probes.contains_key("okx"), "okx was left armed and must still be checked");

    let clocks = clock_venues(LEGACY_REGISTRY, &vars, Some(&capped));
    assert!(!clocks.contains(&"binance".to_string()));
    assert!(clocks.contains(&"bybit".to_string()));
    assert!(clocks.contains(&"okx".to_string()));
}

/// **An armed venue is checked EXACTLY as before.** The `live` ceiling reproduces the sets the
/// uncapped predicate produced, venue for venue — so this lane narrows what a disarmed
/// deployment contacts and changes nothing for an armed one.
#[test]
fn an_armed_venue_is_checked_exactly_as_before() {
    let vars = credentialled();
    let wide = all_live();

    let before: Vec<String> = vike_model::VENUES
        .iter()
        .filter(|v| crate::would_mount_live(LEGACY_REGISTRY, v, &vars))
        .map(|v| (*v).to_string())
        .collect();
    assert_eq!(
        clock_venues(LEGACY_REGISTRY, &vars, Some(&wide)),
        before,
        "the widest ceiling must be a no-op"
    );

    let mut probed: Vec<String> =
        authed_read_probes(LEGACY_REGISTRY, &vars, Some(&wide)).into_keys().collect();
    probed.sort();
    assert!(
        probed.iter().any(|v| v == "binance") && probed.iter().any(|v| v == "ctrader"),
        "precondition: both probe SHAPES are exercised, eager and lazy — {probed:?}"
    );
    assert!(
        any_venue_would_mount_live(LEGACY_REGISTRY, &vars, Some(&wide)),
        "…and the net probe still arms"
    );
}

/// **THE PROPERTY, asserted rather than assumed: the ceiling can only ever NARROW what is
/// contacted.** For every ceiling — including the widest — each derived set is a SUBSET of the
/// uncapped answer, and every element of it is a venue the mount would itself arm at that
/// ceiling.
///
/// A subset check is the honest shape here. "Fewer venues" would pass for a fix that dropped
/// the wrong venue, and "equal under `live`" alone would say nothing about `demo`.
#[test]
fn the_ceiling_can_only_narrow_what_is_contacted() {
    let vars = credentialled();
    let uncapped: Vec<&str> = vike_model::VENUES
        .iter()
        .copied()
        .filter(|v| crate::would_mount_live(LEGACY_REGISTRY, v, &vars))
        .collect();
    assert!(uncapped.len() >= 5, "precondition: a real set must be armed, {uncapped:?}");

    for mode in [VenueMode::Paper, VenueMode::Demo, VenueMode::Live] {
        let policy = all_venues_at(mode);

        for venue in clock_venues(LEGACY_REGISTRY, &vars, Some(&policy)) {
            assert!(
                uncapped.contains(&venue.as_str()),
                "{mode:?}: the clock leg reached {venue}, which the uncapped answer excluded"
            );
            assert!(
                crate::would_mount_live_under_policy(LEGACY_REGISTRY, &venue, &vars, Some(&policy)),
                "{mode:?}: {venue} is checked but this mount would not arm it"
            );
        }
        for venue in authed_read_probes(LEGACY_REGISTRY, &vars, Some(&policy)).keys() {
            assert!(
                uncapped.contains(&venue.as_str()),
                "{mode:?}: a credential probe was built for {venue}, which is not even armed \
                     at the widest ceiling"
            );
            assert!(
                crate::would_mount_live_under_policy(LEGACY_REGISTRY, venue, &vars, Some(&policy)),
                "{mode:?}: {venue} would be signed for but this mount would not arm it"
            );
        }
        if mode == VenueMode::Paper {
            assert!(!any_venue_would_mount_live(LEGACY_REGISTRY, &vars, Some(&policy)));
        }
    }
}

/// **The offline property, STRENGTHENED rather than merely preserved:** no credentials means no
/// network call under EVERY ceiling — the widest included, which is the one that could have
/// regressed. (The narrow ceilings hold it for a second, independent reason, and that
/// redundancy is the point of the lane.)
#[test]
fn no_credentials_means_no_network_call_under_every_ceiling() {
    let empty = HashMap::new();
    for mode in [VenueMode::Paper, VenueMode::Demo, VenueMode::Live] {
        let policy = all_venues_at(mode);
        assert!(clock_venues(LEGACY_REGISTRY, &empty, Some(&policy)).is_empty(), "{mode:?}");
        assert!(authed_read_probes(LEGACY_REGISTRY, &empty, Some(&policy)).is_empty(), "{mode:?}");
        assert!(authed_read_clients(LEGACY_REGISTRY, &empty, Some(&policy)).is_empty(), "{mode:?}");
        assert!(!any_venue_would_mount_live(LEGACY_REGISTRY, &empty, Some(&policy)), "{mode:?}");
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
/// ⚠ The skip is NOT silent: the mount's own `crate::report_capped_to_paper` WARNs per venue
/// whose credentials would have armed it, and `crate::venue_arming_migration` says it once with
/// a paste-ready fix. This test asserts the preflight's half — that being told nothing was
/// checked is never dressed up as a PASS.
#[test]
fn an_all_paper_box_preflights_cleanly() {
    let report = run_startup_preflight(LEGACY_REGISTRY, &credentialled(), &[], None);
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

/// **The TIER half of the ceiling, at the one leg that SIGNS.** `probe_mainnet` is
/// `make_engine`'s own `ceiling_selects_mainnet && ceiling_permits_live`; before decision 0095,
/// this module read the `{VENUE}_MAINNET` flag alone, so a `demo`-capped binance was sent a
/// signed **MAINNET** balance read while the mount bound the demo host.
#[test]
fn a_demo_ceiling_never_signs_against_the_real_money_tier() {
    assert!(probe_mainnet("binance", Some(&all_live())), "a `live` ceiling permits it");

    let demo = all_venues_at(VenueMode::Demo);
    assert!(
        !probe_mainnet("binance", Some(&demo)),
        "a `demo` ceiling never signs against the LIVE account"
    );
    assert!(
        !probe_mainnet("binance", None),
        "and no policy at all is the safe end, not the wide one"
    );
    assert!(!probe_mainnet("deribit", Some(&all_live())), "deribit has no mainnet exec");

    // …and the venue is still CHECKED at the demo ceiling — narrowing the tier must not
    // silently drop the row, which would trade one blind spot for another.
    let vars = vars_of(&[
        ("BINANCE_LIVE_API_KEY", "lk"),
        ("BINANCE_LIVE_API_SECRET", "ls"),
        ("BINANCE_DEMO_API_KEY", "dk"),
        ("BINANCE_DEMO_API_SECRET", "ds"),
    ]);
    assert!(authed_read_probes(LEGACY_REGISTRY, &vars, Some(&demo)).contains_key("binance"));
}
