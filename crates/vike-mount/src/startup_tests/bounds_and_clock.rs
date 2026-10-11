//! The probe bounds, the clock read on a mount's inputs, the credential retries, the disk leg.

use super::*;

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
    assert_matches!(
        unset,
        Err(ServerTimeGap::Unreachable(_)),
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
