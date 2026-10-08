use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_bridge_core::venue_mount::{
    ClockAuth, ClockDecl, ClockRisk, MountInputs, PaperCause, Resolution, Tier, VenueDeclaration,
};
use vike_bridge_core::venue_mount_fixture::{PLANTED_DECLARATION, PlantedMount};
use vike_config::VenueMode;

/// **The widest arming ceiling — every roster venue at `live`.**
///
/// ⚠ `None` instead would make most tests below VACUOUS, not red: `None` reads all-`paper`, and
/// the credential gate and the ceiling gate produce the same empty map, so "absent credentials
/// mean no probe" would pass where the CEILING suppressed them. CREDENTIAL tests hold the
/// ceiling wide open; the ceiling gets its own tests.
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

/// An off-roster string is the OTHER gap: it says nothing about a venue, so it must not read as
/// a declaration.
#[test]
fn an_unknown_venue_is_not_a_declaration() {
    let vars = HashMap::new();
    match venue_server_time_ms(&[], "not-a-venue", &vars, false) {
        Err(ServerTimeGap::Unreachable(e)) => assert!(e.contains("not-a-venue"), "{e}"),
        other => panic!("an unknown venue must not read as declared: {other:?}"),
    }
}

/// The credential leg over an EMPTY probe map errors, never silently passes (what makes deriving
/// the venue list from the map load-bearing).
#[test]
fn the_authed_read_probe_errors_for_an_unbuilt_venue() {
    let e = authed_read_probe(CredentialProbes::new())("binance").expect_err("no client was built");
    // …the CONFIRMED half: an absent probe is our wiring defect, never the "we never heard
    // back" gap (which never demotes).
    match e {
        CredentialGap::Rejected(msg) => assert!(msg.contains("binance"), "{msg}"),
        other => panic!("an unbuilt venue must be Rejected, not {other:?}"),
    }
}

/// THE BOUND: a probe that never answers is ABANDONED at [`CREDENTIAL_PROBE_TIMEOUT`], not
/// waited out. The fake parks far longer (the ~20 s geo-blocked alpaca TCP connect measured on
/// the dev box); the assertion is on WALL CLOCK: about the bound, not the probe's duration.
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

/// THE CLOCK leg's half: a read that never answers is ABANDONED at [`CLOCK_PROBE_TIMEOUT`].
/// The fake parks far longer (a wedged `getaddrinfo`, the ONE case
/// `crate::server_time::CLOCK_READ_TIMEOUT` cannot preempt); asserted on WALL CLOCK. Offline.
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

/// …and the abandoned read is outcome ② (a WARN that degrades nothing), never a DECLARATION:
/// `NotChecked` would print "nothing to check here" over a clock we failed to reach.
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

/// The abandon ceiling sits ABOVE the transport's timeout, load-bearing: at equal values this
/// bound (starting marginally earlier) would abandon every ordinary timeout a hair before ureq
/// reported it, trading the venue's error text for a generic "did not answer".
#[test]
fn the_clock_abandon_ceiling_sits_above_the_transports_own_timeout() {
    assert!(
        CLOCK_PROBE_TIMEOUT > crate::server_time::CLOCK_READ_TIMEOUT,
        "a read ureq CAN bound must report itself before the mount walks away from it"
    );
}

/// Thread names let a stack dump say WHICH leg's abandoned worker is parked — on Linux only if
/// each fits [`THREAD_NAME_MAX_BYTES`] (`std` truncates silently; the credential name and the
/// clock name's first draft truncated onto the SAME prefix). Distinctness is asserted too.
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

// ---- a contract row's clock is read on the inputs its mount is handed ---------------------------

/// The server time [`SETTING_CLOCK`] answers once it can see its configured host.
const PLANTED_SERVER_MS: i64 = 1_700_000_000_000;

/// What [`SETTING_CLOCK`] reads: answers only when its inputs carry the host setting.
fn setting_clock_read(inputs: &MountInputs<'_>) -> Result<i64, String> {
    match inputs.settings.get(vike_secrets::venue_setting::SettingTier::Any, "CLOCK_HOST") {
        Some("planted-clock-host") => Ok(PLANTED_SERVER_MS),
        other => Err(format!("no clock at host {other:?}")),
    }
}

/// A contract row whose clock HOST is a venue setting (a configurable clock endpoint).
static SETTING_CLOCK: PlantedMount = PlantedMount {
    declaration: VenueDeclaration {
        clock: ClockDecl::Wired {
            endpoint: "GET {CLOCK_HOST}/time (a planted, configurable host)",
            auth: ClockAuth::Public,
            risk: ClockRisk::NoTimestamp,
        },
        ..PLANTED_DECLARATION
    },
    server_time: Some(setting_clock_read),
    ..PlantedMount::new("okx", Resolution::Paper(PaperCause::NoLiveArm))
};
static SETTING_CLOCK_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&SETTING_CLOCK)];

/// A policy whose `venue_setting` snapshot configures [`SETTING_CLOCK`]'s host.
fn policy_with_a_clock_host() -> crate::MountPolicy {
    let row = vike_secrets::VenueSettingRow {
        venue: "okx".to_string(),
        tier: None,
        field: "CLOCK_HOST".to_string(),
        value: "planted-clock-host".to_string(),
    };
    crate::MountPolicy {
        venue_settings: std::collections::BTreeMap::from([(
            "okx".to_string(),
            vike_secrets::venue_setting::VenueSettings::from_rows("okx", &[row]),
        )]),
        ..all_venues_at(VenueMode::Demo)
    }
}

/// ⚠ **THE CLOCK IS READ ON THE MOUNT'S INPUTS**: the policy's venue settings, account table and
/// process facts, as `contract_parts` hands the bridge's `mount`, so a setting-sourced clock host
/// is measured where the mount binds. Handed EMPTY settings (as it once was), this bridge
/// measured nothing; without the policy it still does — the other half.
#[test]
fn a_contract_rows_clock_is_read_on_the_inputs_its_mount_is_handed() {
    let vars = Arc::new(HashMap::new());
    let policy = Arc::new(Some(policy_with_a_clock_host()));
    assert_eq!(
        bounded_server_time_ms(&SETTING_CLOCK_REG, "okx", &vars, false, &policy),
        Ok(PLANTED_SERVER_MS),
        "the clock read must see the venue settings the mount sees"
    );
    let unset = bounded_server_time_ms(&SETTING_CLOCK_REG, "okx", &vars, false, &Arc::new(None));
    assert!(
        matches!(unset, Err(ServerTimeGap::Unreachable(_))),
        "with no policy the setting is absent and the read fails: {unset:?}"
    );
}

/// THE confirmed-vs-transient split: the two `Err` shapes decide demotion, so neither may be
/// reachable from the other's cause. A venue that ANSWERS and refuses is `Rejected`, reported
/// only after the RETRY (a demotion needs two refusals).
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

/// …and a venue that never answers is `Unanswered` (never demotes). ⚠ NOT retried: re-waiting
/// the bound doubles the leg's worst case to learn nothing twice — what the bound exists to cap.
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

/// THE DISK LEG, armed: a real number for a real directory, an ERROR (rendered WARN, never a
/// fake PASS) for a missing one.
///
/// ⚠ Windows takes the declared-gap path (honest, not skipped): the CONTRACT is asserted on both
/// platforms, the measurement only where it can be taken. Unseen: `f_bavail` vs `f_bfree` (equal
/// on an unreserved filesystem), a documented judgement in [`free_space_bytes`], not pinned.
#[test]
fn the_disk_probe_measures_a_real_directory_and_declares_a_missing_one() {
    let here = std::env::temp_dir();
    let measured = free_space_bytes(&here);
    // ⚠ `#[cfg]`-selected, not `if cfg!(…)`: that fires clippy's `assertions_on_constants`
    // under `-D warnings` and lets the WRONG arm compile-check into nothing.
    #[cfg(unix)]
    {
        let free = measured.expect("unix must MEASURE, not declare");
        assert!(free > 0, "a writable temp dir reporting ZERO free bytes is not a reading");
        // A missing directory is an ERROR, so `check_disk_headroom` WARNs naming it.
        let missing = here.join("vike-preflight-no-such-dir-8f3a1c");
        assert!(free_space_bytes(&missing).is_err(), "an absent directory cannot be measured");
    }
    #[cfg(not(unix))]
    {
        let reason = measured.expect_err("this platform cannot measure, so it must DECLARE");
        assert!(!reason.is_empty(), "a declared gap must carry its reason");
    }
}

/// Deduplicated by PATH, not label: a journal inside the store root is ONE filesystem; two rows
/// would double every finding.
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

// ---- THE ARMING CEILING reaches the preflight: tested where the registry is ----
//
// The tests that drive ONE credential map arming a real spread of the roster and vary ONLY the
// ceiling are `crates/vike-tradehub/tests/mount_roster/startup.rs`'s, over its `credentialled`.
// The ceiling tests here (the last three below) drive PLANTED rows over an empty map.

// ---- the identity-recording probe, and branch 4's two probe shapes ------------------------------

/// A record an identity recorder was handed: `(venue, account, tier, a client came with it)`.
type Recorded = (String, vike_model::accounts::account_keys::AccountLabel, VenueMode, bool);

/// Every record [`capture`] was handed, by every test that passes it.
static RECORDED: Mutex<Vec<Recorded>> = Mutex::new(Vec::new());

/// An [`IdentityRecorder`] that captures what it is handed instead of asking the venue.
fn capture(
    venue: &str,
    label: &vike_model::accounts::account_keys::AccountLabel,
    tier: VenueMode,
    recon: Option<&dyn ReconClient>,
    _directory: &vike_bridge_core::account_directory::AccountDirectory,
) {
    RECORDED.lock().expect("the capture").push((
        venue.to_string(),
        label.clone(),
        tier,
        recon.is_some(),
    ));
}

/// What [`capture`] recorded for `venue` — each test records under its own venue name.
fn recorded_for(venue: &str) -> Vec<Recorded> {
    RECORDED.lock().expect("the capture").iter().filter(|r| r.0 == venue).cloned().collect()
}

/// A reconcile client whose balance read answers `balance`, counting its reads.
struct BalanceOnly {
    balance: Result<Option<f64>, &'static str>,
    reads: &'static AtomicUsize,
}

impl ReconClient for BalanceOnly {
    fn fetch_order_status_reports(
        &self,
        _since: i64,
    ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
        Ok(vec![])
    }
    fn fetch_position_status_reports(
        &self,
    ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.balance.map_err(str::to_string)
    }
}

/// The probe records AFTER an answered balance read only (the balance alone is the verdict), at
/// the tier it was given, for the default account, with the client that answered.
#[test]
fn the_identity_probe_records_after_an_answered_balance_and_only_then() {
    static READS: AtomicUsize = AtomicUsize::new(0);
    let answered = identity_recording_probe(
        "identity-probe-answered".to_string(),
        Box::new(BalanceOnly { balance: Ok(Some(1.0)), reads: &READS }),
        VenueMode::Demo,
        vike_bridge_core::account_directory::AccountDirectory::default(),
        capture,
    );
    assert_eq!(answered(), Ok(()));
    assert_eq!(READS.load(Ordering::SeqCst), 1, "one balance read");
    assert_eq!(
        recorded_for("identity-probe-answered"),
        vec![(
            "identity-probe-answered".to_string(),
            vike_model::accounts::account_keys::AccountLabel::Default,
            VenueMode::Demo,
            true
        )]
    );

    static REFUSED_READS: AtomicUsize = AtomicUsize::new(0);
    let refused = identity_recording_probe(
        "identity-probe-refused".to_string(),
        Box::new(BalanceOnly { balance: Err("401 refused"), reads: &REFUSED_READS }),
        VenueMode::Demo,
        vike_bridge_core::account_directory::AccountDirectory::default(),
        capture,
    );
    assert_eq!(refused(), Err("401 refused".to_string()), "the balance read is the verdict");
    assert!(recorded_for("identity-probe-refused").is_empty(), "a refused key records nothing");
}

/// Every planted probe row below resolves armed at `DEMO` whatever its inputs say, declares
/// [`PLANTED_DECLARATION`] (no named account, no book, no clock) and offers the probe its
/// `credential_probe` builds.
const ARMED_DEMO: Resolution = Resolution::Armed { tier: Tier::Demo, held_below_live: None };

static BOUND_DEMO_READS: AtomicUsize = AtomicUsize::new(0);
static BOUND_DEMO: PlantedMount = PlantedMount {
    credential_probe: Some(|_| {
        Some(vike_bridge_core::venue_mount::CredentialProbe::RecordsIdentity {
            client: Box::new(BalanceOnly { balance: Ok(Some(1.0)), reads: &BOUND_DEMO_READS }),
            bound_tier: Tier::Demo,
        })
    }),
    ..PlantedMount::new("okx", ARMED_DEMO)
};
static BOUND_DEMO_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&BOUND_DEMO)];

/// Branch 4, `RecordsIdentity`: records at the tier the credentials BIND — DEMO under a `live`
/// ceiling, so a record keyed on the ceiling would be the wrong row.
#[test]
fn a_contract_rows_identity_probe_records_at_its_bound_tier_not_the_ceiling() {
    let probes =
        authed_read_probes_with(&BOUND_DEMO_REG, &HashMap::new(), Some(&all_live()), capture);
    let probe = probes.get("okx").expect("an armed contract row's probe is in the map");
    assert_eq!(probe(), Ok(()));
    assert_eq!(BOUND_DEMO_READS.load(Ordering::SeqCst), 1, "the balance was read");
    assert_eq!(
        recorded_for("okx"),
        vec![(
            "okx".to_string(),
            vike_model::accounts::account_keys::AccountLabel::Default,
            VenueMode::Demo,
            true
        )],
        "recorded at the BOUND tier"
    );
}

static READ_ONLY_CALLS: AtomicUsize = AtomicUsize::new(0);
static READ_ONLY: PlantedMount = PlantedMount {
    credential_probe: Some(|_| {
        Some(vike_bridge_core::venue_mount::CredentialProbe::ReadOnly(Arc::new(|| {
            READ_ONLY_CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })))
    }),
    ..PlantedMount::new("alpaca", ARMED_DEMO)
};
static READ_ONLY_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&READ_ONLY)];

/// Branch 4, `ReadOnly`: the bridge's own read goes into the map as is, nothing recorded, behind
/// the same ceiling gate.
#[test]
fn a_contract_rows_read_only_probe_is_the_bridges_own_read() {
    let probes =
        authed_read_probes_with(&READ_ONLY_REG, &HashMap::new(), Some(&all_live()), capture);
    assert_eq!(probes.get("alpaca").expect("the bridge's own read")(), Ok(()));
    assert_eq!(READ_ONLY_CALLS.load(Ordering::SeqCst), 1, "the map holds the bridge's closure");
    assert!(recorded_for("alpaca").is_empty(), "a read-only probe records nothing");

    let capped = all_live_except("alpaca");
    assert!(
        !authed_read_probes(&READ_ONLY_REG, &HashMap::new(), Some(&capped)).contains_key("alpaca"),
        "a `paper` ceiling yields no probe for a contract row either"
    );
}

/// `(venue, key set)` for every probe a [`tier_following_probe`] row built.
static HANDED_KEYS: Mutex<Vec<(&'static str, &'static str)>> = Mutex::new(Vec::new());

/// What [`HANDED_KEYS`] recorded for `venue` — each test plants its own venue name.
fn handed_keys_for(venue: &str) -> Vec<&'static str> {
    HANDED_KEYS
        .lock()
        .expect("the keys")
        .iter()
        .filter(|(v, _)| *v == venue)
        .map(|(_, k)| *k)
        .collect()
}

/// A probe binding the tier it is HANDED, like every CEX bridge's `credential_probe`:
/// `MountInputs::live_permitted` picks LIVE keys and `Tier::Live`, else DEMO. Each call records
/// its key set in [`HANDED_KEYS`], so a test reads what branch 4 HANDED the bridge.
fn tier_following_probe(
    venue: &'static str,
    reads: &'static AtomicUsize,
    inputs: &MountInputs<'_>,
) -> Option<vike_bridge_core::venue_mount::CredentialProbe> {
    let (keys, tier) =
        if inputs.live_permitted { ("LIVE", Tier::Live) } else { ("DEMO", Tier::Demo) };
    HANDED_KEYS.lock().expect("the keys").push((venue, keys));
    Some(vike_bridge_core::venue_mount::CredentialProbe::RecordsIdentity {
        client: Box::new(BalanceOnly { balance: Ok(Some(1.0)), reads }),
        bound_tier: tier,
    })
}

static DEMO_CEILING_READS: AtomicUsize = AtomicUsize::new(0);
static DEMO_CEILING: PlantedMount = PlantedMount {
    credential_probe: Some(|inputs| tier_following_probe("bybit", &DEMO_CEILING_READS, inputs)),
    ..PlantedMount::new("bybit", ARMED_DEMO)
};
static DEMO_CEILING_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&DEMO_CEILING)];

/// Branch 4 under a `demo` ceiling: a row ARMED below `live` is still probed, handed the DEMO
/// tier, recorded at DEMO. Every other presence assertion runs under `live`, so a branch 4
/// skipping rows below `live` would pass them all and silently switch a demo box's credential
/// leg off (no probe = never checked = never demoted). Handing every row `live_permitted = true`
/// would send a demo box's signed balance read to the real-money account with LIVE keys (the
/// defect decision 0095 closed); the tier-following probe makes that visible.
///
/// Successor of `a_demo_ceiling_never_signs_against_the_real_money_tier`'s last assertion here.
/// Its `live` half (non-vacuity) is [`a_live_ceiling_hands_the_probe_the_live_tier`]; the
/// PRESENCE half over the REAL registry is `crates/vike-tradehub/tests/mount_roster/startup.rs`'s
/// `a_demo_ceiling_still_probes_the_switched_venues_at_the_demo_tier`.
#[test]
fn an_armed_venue_is_still_probed_under_a_demo_ceiling() {
    let demo = all_venues_at(VenueMode::Demo);
    assert!(
        crate::would_mount_live_under_policy(
            &DEMO_CEILING_REG,
            "bybit",
            &HashMap::new(),
            Some(&demo)
        ),
        "precondition: the planted row arms at DEMO under a `demo` ceiling"
    );
    let probes = authed_read_probes_with(&DEMO_CEILING_REG, &HashMap::new(), Some(&demo), capture);
    let probe = probes.get("bybit").expect("a row a `demo` ceiling arms is probed");
    assert_eq!(
        handed_keys_for("bybit"),
        vec!["DEMO"],
        "a `demo` ceiling must hand the bridge the DEMO tier, never the LIVE keys"
    );
    assert_eq!(probe(), Ok(()));
    assert_eq!(DEMO_CEILING_READS.load(Ordering::SeqCst), 1, "the balance was read");
    assert_eq!(
        recorded_for("bybit"),
        vec![(
            "bybit".to_string(),
            vike_model::accounts::account_keys::AccountLabel::Default,
            VenueMode::Demo,
            true
        )],
        "recorded at the DEMO tier the credentials bind"
    );
}

static LIVE_CEILING_READS: AtomicUsize = AtomicUsize::new(0);
static LIVE_CEILING: PlantedMount = PlantedMount {
    credential_probe: Some(|inputs| tier_following_probe("binance", &LIVE_CEILING_READS, inputs)),
    ..PlantedMount::new("binance", ARMED_DEMO)
};
static LIVE_CEILING_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&LIVE_CEILING)];

/// The other half of [`an_armed_venue_is_still_probed_under_a_demo_ceiling`] (so a row answering
/// DEMO whatever it is handed cannot pass it): under `live` the same row binds and records LIVE.
#[test]
fn a_live_ceiling_hands_the_probe_the_live_tier() {
    let probes =
        authed_read_probes_with(&LIVE_CEILING_REG, &HashMap::new(), Some(&all_live()), capture);
    let probe = probes.get("binance").expect("a row a `live` ceiling arms is probed");
    assert_eq!(handed_keys_for("binance"), vec!["LIVE"], "a `live` ceiling hands the LIVE tier");
    assert_eq!(probe(), Ok(()));
    assert_eq!(LIVE_CEILING_READS.load(Ordering::SeqCst), 1, "the balance was read");
    assert_eq!(
        recorded_for("binance"),
        vec![(
            "binance".to_string(),
            vike_model::accounts::account_keys::AccountLabel::Default,
            VenueMode::Live,
            true
        )],
        "recorded at the LIVE tier the credentials bind"
    );
}

static BOUND_LIVE_READS: AtomicUsize = AtomicUsize::new(0);
/// Arms below `live`, then offers a probe BOUND to LIVE whatever it is handed (a bridge ignoring
/// `MountInputs::live_permitted`).
static BOUND_LIVE: PlantedMount = PlantedMount {
    credential_probe: Some(|_| {
        Some(vike_bridge_core::venue_mount::CredentialProbe::RecordsIdentity {
            client: Box::new(BalanceOnly { balance: Ok(Some(1.0)), reads: &BOUND_LIVE_READS }),
            bound_tier: Tier::Live,
        })
    }),
    ..PlantedMount::new("oanda", ARMED_DEMO)
};
static BOUND_LIVE_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&BOUND_LIVE)];

/// THE CEILING INTERLOCK, startup-probe half: a LIVE-bound `RecordsIdentity` probe under a ceiling
/// below `live` is never built — in the map it would be a signed read against the real-money
/// account the ceiling forbids. `resolve` answered DEMO, and the other halves watch `resolve` and
/// `mount`, neither reached by this leg. Nothing is mapped, read or recorded.
///
/// Control: the same row under `live` is probed, read once, recorded at LIVE (the ceiling refused).
#[test]
fn a_probe_bound_to_the_live_tier_under_a_demo_ceiling_is_never_built() {
    let demo = all_venues_at(VenueMode::Demo);
    assert!(
        crate::would_mount_live_under_policy(
            &BOUND_LIVE_REG,
            "oanda",
            &HashMap::new(),
            Some(&demo)
        ),
        "precondition: the planted row arms below `live`, so the ceiling gate lets it through"
    );
    let probes = authed_read_probes_with(&BOUND_LIVE_REG, &HashMap::new(), Some(&demo), capture);
    assert!(!probes.contains_key("oanda"), "a LIVE-bound probe under a demo ceiling is refused");
    assert_eq!(BOUND_LIVE_READS.load(Ordering::SeqCst), 0, "no balance was read");
    assert!(recorded_for("oanda").is_empty(), "nothing was recorded");

    let probes =
        authed_read_probes_with(&BOUND_LIVE_REG, &HashMap::new(), Some(&all_live()), capture);
    let probe = probes.get("oanda").expect("under a `live` ceiling the same probe is built");
    assert_eq!(probe(), Ok(()));
    assert_eq!(BOUND_LIVE_READS.load(Ordering::SeqCst), 1, "the balance was read");
    assert_eq!(
        recorded_for("oanda"),
        vec![(
            "oanda".to_string(),
            vike_model::accounts::account_keys::AccountLabel::Default,
            VenueMode::Live,
            true
        )],
        "recorded at the LIVE tier the credentials bind"
    );
}
