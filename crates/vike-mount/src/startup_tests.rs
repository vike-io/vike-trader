use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_bridge_core::venue_mount::{
    ClockAuth, ClockDecl, ClockRisk, MountInputs, PaperCause, Resolution, Tier, VenueDeclaration,
};
use vike_bridge_core::venue_mount_fixture::{PLANTED_DECLARATION, PlantedMount};
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

// ---- a contract row's clock is read on the inputs its mount is handed ---------------------------

/// The server time [`SETTING_CLOCK`] answers once it can see its configured host.
const PLANTED_SERVER_MS: i64 = 1_700_000_000_000;

/// What [`SETTING_CLOCK`] reads: it answers only when its inputs carry the host setting, the way a
/// real read fails at the wrong host.
fn setting_clock_read(inputs: &MountInputs<'_>) -> Result<i64, String> {
    match inputs.settings.get(vike_secrets::venue_setting::SettingTier::Any, "CLOCK_HOST") {
        Some("planted-clock-host") => Ok(PLANTED_SERVER_MS),
        other => Err(format!("no clock at host {other:?}")),
    }
}

/// A contract row whose clock HOST is a venue setting — the shape a bridge takes once its clock
/// endpoint is configurable.
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

/// ⚠ **THE CLOCK IS READ ON THE MOUNT'S INPUTS.** A contract row's clock read gets the policy's
/// venue settings — with its account table and the process facts, exactly what `contract_parts`
/// hands the same bridge's `mount` — so a clock host that comes from a setting is measured where
/// the mount will bind. The read used to be handed EMPTY settings, and this bridge then measured
/// nothing; without the policy it still does, which is the other half.
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

// ─── THE ARMING CEILING reaches the preflight ──────────────────────────────────────────────
//
// Every test in this block drives the SAME credential map (`credentialled()`, which arms a real
// spread of the roster) and varies ONLY the ceiling, so a green result can be attributed to the
// ceiling and to nothing else. Offline by construction: every predicate under test is one of
// the pure, network-free live-intent probes, and no probe closure is ever CALLED.

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

/// The probe records AFTER the balance read and only when it answered — the verdict is the balance
/// alone — and it records at the tier it was given, for the default account, with the client that
/// answered.
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

/// What every planted probe row below resolves to: armed at `DEMO` whatever its inputs say. Each
/// row declares what [`PLANTED_DECLARATION`] declares — a venue that addresses no named account,
/// names no book and reads no clock, nothing the credential leg's tests look at — and offers the
/// startup probe its `credential_probe` builds.
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

/// Branch 4, `RecordsIdentity`: a contract row's probe reads the balance and records at the tier
/// its credentials BIND — here DEMO under a `live` ceiling, so a record keyed on the ceiling would
/// be the wrong row.
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

/// Branch 4, `ReadOnly`: the bridge's own read goes into the map as it is, and nothing is recorded
/// — behind the same ceiling gate as a `RecordsIdentity` row.
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

/// The probe of a contract row that binds the tier it is HANDED, the shape every CEX bridge's
/// `credential_probe` has: `MountInputs::live_permitted` picks the LIVE key set and `Tier::Live`,
/// anything else the DEMO set and `Tier::Demo`. Each call records the key set it chose in
/// [`HANDED_KEYS`], so a test reads what branch 4 HANDED the bridge rather than what a planted
/// constant says.
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

/// Branch 4 under a `demo` ceiling: a row the ceiling ARMS below `live` is still probed, it is
/// handed the DEMO tier, and its record lands at DEMO. Every other presence assertion here runs
/// under a `live` ceiling, so a branch 4 that skipped every row below `live` would pass them all —
/// and on a demo box it would switch the credential leg off without a trace, because a venue with
/// no probe is never checked and so never demoted. And a branch 4 that handed every row
/// `live_permitted = true` would send a demo box's signed balance read to the real-money account
/// with the LIVE keys — the defect decision 0095 closed — which the planted row's tier-following
/// probe makes visible here: it binds what it is handed, as every CEX bridge's does.
///
/// It replaces the last assertion of the deleted
/// `a_demo_ceiling_never_signs_against_the_real_money_tier`, which drove the same branch through
/// okx's row in the transitional registry. Its `live` half, which keeps this one from being
/// vacuous, is [`a_live_ceiling_hands_the_probe_the_live_tier`]; the PRESENCE half over the REAL
/// registry, with both key sets stored, is `crates/vike-tradehub/tests/mount_roster/startup.rs`'s
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

/// The other half of [`an_armed_venue_is_still_probed_under_a_demo_ceiling`], so that one cannot
/// pass by a row that answers DEMO whatever it is handed: under a `live` ceiling the same
/// tier-following row is handed `live_permitted = true`, binds LIVE, and its record lands at LIVE.
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
/// A row that arms below `live` and then offers a probe BOUND to the LIVE tier whatever it is
/// handed — a bridge that ignores `MountInputs::live_permitted` when it picks its key set.
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

/// THE CEILING INTERLOCK, startup-probe half: a `RecordsIdentity` probe bound to the LIVE tier under
/// a ceiling below `live` is never built — the bridge's client does the balance read, so a probe in
/// the map is a signed read against the real-money account the ceiling forbids. `resolve` answered
/// DEMO here and the interlock's other halves watch `resolve` and `mount`, which this leg reaches
/// neither of. No venue is in the map and nothing is read or recorded.
///
/// The control is the same row under a `live` ceiling: probed, read once, recorded at LIVE — so the
/// refusal is the ceiling's and not a probe that never builds.
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
