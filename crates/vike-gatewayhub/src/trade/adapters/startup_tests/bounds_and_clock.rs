//! The probe bounds, the clock read on a start's inputs, the credential retries, the network reading.

use super::*;

/// An off-roster string is the OTHER gap: it says nothing about a venue, so it must not read as
/// a declaration.
#[test]
fn an_unknown_venue_is_not_a_declaration() {
    let vars = HashMap::new();
    match venue_server_time_ms(&[], "not-a-venue", &vars, AccountTier::Demo) {
        Err(ServerTimeGap::Unreachable(e)) => assert!(e.contains("not-a-venue"), "{e}"),
        other => panic!("an unknown venue must not read as declared: {other:?}"),
    }
}

/// The credential leg over an EMPTY probe map errors, never silently passes (what makes deriving
/// the venue list from the map load-bearing).
#[test]
fn the_authed_read_probe_errors_for_an_unbuilt_venue() {
    let e = authed_read_probe(CredentialProbeFunctions::new())("binance")
        .expect_err("no client was built");
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
    let probe: CredentialProbeFn = Arc::new(|| {
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
    let ok: CredentialProbeFn = Arc::new(|| Ok(()));
    assert_eq!(bounded_probe(&ok, CREDENTIAL_PROBE_TIMEOUT, CREDENTIAL_PROBE_THREAD), Ok(Ok(())));
    let refused: CredentialProbeFn =
        Arc::new(|| Err(ReconcileFetchError::unclassified("401 invalid api key")));
    assert_eq!(
        bounded_probe(&refused, CREDENTIAL_PROBE_TIMEOUT, CREDENTIAL_PROBE_THREAD),
        Ok(Err(ReconcileFetchError::unclassified("401 invalid api key")))
    );
}

/// THE CLOCK leg's half: a read that never answers is ABANDONED at [`CLOCK_PROBE_TIMEOUT`].
/// The fake parks far longer (a wedged `getaddrinfo`, the ONE case
/// `crate::trade::adapters::server_time::CLOCK_READ_TIMEOUT` cannot preempt); asserted on WALL CLOCK. Offline.
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
        CLOCK_PROBE_TIMEOUT > crate::trade::adapters::server_time::CLOCK_READ_TIMEOUT,
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

// ---- a contract row's clock is read on the inputs its start is handed ---------------------------

/// The server time [`SETTING_CLOCK`] answers once it can see its configured host.
const PLANTED_SERVER_MS: i64 = 1_700_000_000_000;

/// What [`SETTING_CLOCK`] reads: answers only when its inputs carry the host setting.
fn setting_clock_read(inputs: &AdapterInputs<'_>) -> Result<i64, String> {
    let clock_host = vike_venues::venue_fields::VenueFieldName::normalised("clock_host");
    match inputs.settings.get(vike_secrets::venue_setting::SettingTier::Any, &clock_host) {
        Some("planted-clock-host") => Ok(PLANTED_SERVER_MS),
        other => Err(format!("no clock at host {other:?}")),
    }
}

/// A contract row whose clock HOST is a venue setting (a configurable clock endpoint).
static SETTING_CLOCK: PlantedAdapter = PlantedAdapter {
    declaration: VenueAdapterDeclaration {
        clock: ClockDeclaration::Wired {
            endpoint: "GET {CLOCK_HOST}/time (a planted, configurable host)",
            auth: ClockAuthentication::Public,
            risk: ClockRisk::NoTimestamp,
        },
        ..PLANTED_DECLARATION
    },
    server_time: Some(setting_clock_read),
    ..PlantedAdapter::new("okx", ArmingResolution::NotRunning(ArmingNotRunningCause::NoLiveArm))
};
static SETTING_CLOCK_REG: [crate::trade::adapters::registry::VenueAdapterRow; 1] =
    [crate::trade::adapters::registry::VenueAdapterRow::Adapter(&SETTING_CLOCK)];

/// A policy whose `venue_setting` snapshot configures [`SETTING_CLOCK`]'s host.
fn policy_with_a_clock_host() -> vike_tradehub_core::AdapterPolicy {
    let row = vike_secrets::VenueSettingRow {
        venue: "okx".to_string(),
        tier: None,
        field: "CLOCK_HOST".to_string(),
        value: "planted-clock-host".to_string(),
    };
    vike_tradehub_core::AdapterPolicy {
        venue_settings: std::collections::BTreeMap::from([(
            "okx".to_string(),
            vike_secrets::venue_setting::VenueSettings::from_rows("okx", &[row]),
        )]),
        ..all_venues_at(AccountTier::Demo)
    }
}

/// ⚠ **THE CLOCK IS READ ON THE START'S INPUTS**: the policy's venue settings, account table and
/// process facts, as `contract_parts` hands the bridge's `start`, so a setting-sourced clock host
/// is measured where the start binds. Handed EMPTY settings (as it once was), this bridge
/// measured nothing; without the policy it still does — the other half.
#[test]
fn a_contract_rows_clock_is_read_on_the_inputs_its_start_is_handed() {
    let vars = Arc::new(HashMap::new());
    let policy = Arc::new(Some(snapshot_of(&policy_with_a_clock_host())));
    assert_eq!(
        bounded_server_time_ms(&SETTING_CLOCK_REG, "okx", &vars, AccountTier::Demo, &policy),
        Ok(PLANTED_SERVER_MS),
        "the clock read must see the venue settings the mount sees"
    );
    let unset = bounded_server_time_ms(
        &SETTING_CLOCK_REG,
        "okx",
        &vars,
        AccountTier::Demo,
        &Arc::new(None),
    );
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
    let mut probes = CredentialProbeFunctions::new();
    probes.insert(
        "okx".to_string(),
        Arc::new(move || {
            seen.fetch_add(1, Ordering::Relaxed);
            Err(ReconcileFetchError::new(FailureOutcome::Refused, "401 invalid api key"))
        }) as CredentialProbeFn,
    );
    assert_eq!(
        authed_read_probe(probes)("okx"),
        Err(CredentialGap::Rejected("401 invalid api key".to_string())),
        "an answering venue is Rejected, in its words"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        CREDENTIAL_PROBE_ATTEMPTS,
        "a demotion must be CONFIRMED — one refusal is a blip, not a verdict"
    );
}

/// A venue the probe could not REACH (`NotSent`) is `Unanswered` after its retries, never `Rejected` (owner
/// 2026-11-16): it says nothing about the keys, so the account is not stopped for the session and its start's own
/// connect decides.
#[test]
fn an_unreachable_venue_is_unanswered_not_rejected() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let mut probes = CredentialProbeFunctions::new();
    probes.insert(
        "okx".to_string(),
        Arc::new(move || {
            seen.fetch_add(1, Ordering::Relaxed);
            Err(ReconcileFetchError::new(
                FailureOutcome::NotSent,
                "venue error 0: network error: Connection refused",
            ))
        }) as CredentialProbeFn,
    );
    match authed_read_probe(probes)("okx") {
        Err(CredentialGap::Unanswered { detail, .. }) => {
            assert!(detail.contains("no verdict on the keys"), "{detail}");
        }
        other => panic!("an unreachable venue must be Unanswered, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::Relaxed), CREDENTIAL_PROBE_ATTEMPTS, "tried as a refusal is");
}

/// The credential leg over a probe answering `answer` every time.
fn credential_verdict(answer: ReconcileFetchError) -> Result<(), CredentialGap> {
    let mut probes = CredentialProbeFunctions::new();
    probes
        .insert("binance".to_string(), Arc::new(move || Err(answer.clone())) as CredentialProbeFn);
    authed_read_probe(probes)("binance")
}

/// **D12: a venue that answered "not now" is `Unanswered`, never `Rejected`, whatever its words** (deploy 2,
/// 2026-11-17: binance's demo spot host answered 502 at the start and the account was stopped until a restart; and a
/// Bybit Cloudflare 403, a Bybit `10000`, a Binance `-1007`/`-1008` or an OKX `50013`/`50026` read as a refused key
/// while the verdict was taken off the text by the central table). The verdict is the bridge's `outcome` alone: the
/// words below would read as an auth refusal to a text reader. Only `Refused` is `Rejected`.
#[test]
fn a_venue_not_now_is_unanswered_and_only_a_refusal_is_rejected() {
    for (outcome, words) in [
        (FailureOutcome::NotNow, "GET /v5/account/wallet-balance: venue error 403 (empty message)"),
        (FailureOutcome::OutcomeUnknown, "venue error 10000: Server Timeout"),
        (FailureOutcome::OutcomeUnknown, "venue error -1007: Timeout waiting for response"),
        (FailureOutcome::NotNow, "venue error -1008: Server is currently overloaded"),
        (FailureOutcome::NotNow, "venue error 50013: Systems are busy. Please try again later."),
        (FailureOutcome::NotSent, "venue error 0: network error: Connection refused"),
    ] {
        match credential_verdict(ReconcileFetchError::new(outcome, words)) {
            Err(CredentialGap::Unanswered { waited_ms: 0, detail }) => {
                assert!(detail.contains(words), "the venue's words are kept: {detail}");
            }
            other => panic!("{outcome:?} ({words}) must be Unanswered, got {other:?}"),
        }
    }
    assert_eq!(
        credential_verdict(ReconcileFetchError::new(
            FailureOutcome::Refused,
            "venue error -2015: Invalid API-key, IP, or permissions for action."
        )),
        Err(CredentialGap::Rejected(
            "venue error -2015: Invalid API-key, IP, or permissions for action.".to_string()
        )),
        "a refusal of the keys is still Rejected"
    );
}

/// The "not now" answer is ASKED AGAIN (`retry_on_pauses`): a venue that is busy at the first try and answers the
/// second is accepted, so a blip at the start costs one pause, not the account.
#[test]
fn a_venue_not_now_is_asked_again_and_a_later_answer_wins() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let mut probes = CredentialProbeFunctions::new();
    probes.insert(
        "bybit".to_string(),
        Arc::new(move || match seen.fetch_add(1, Ordering::Relaxed) {
            0 => Err(ReconcileFetchError::new(FailureOutcome::NotNow, "venue error 403")),
            _ => Ok(()),
        }) as CredentialProbeFn,
    );
    assert_eq!(authed_read_probe(probes)("bybit"), Ok(()));
    assert_eq!(calls.load(Ordering::Relaxed), 2, "asked once more after the pause");
}

/// …and a venue that never answers is `Unanswered` (never demotes). ⚠ NOT retried: re-waiting
/// the bound doubles the leg's worst case to learn nothing twice — what the bound exists to cap.
#[test]
fn a_silent_venue_is_unanswered_and_is_not_retried() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let mut probes = CredentialProbeFunctions::new();
    probes.insert(
        "alpaca".to_string(),
        Arc::new(move || {
            seen.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(CREDENTIAL_PROBE_TIMEOUT * 6);
            Ok(())
        }) as CredentialProbeFn,
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

// ---- the network leg's reading of a REAL probe handle ----------------------------------------

/// The epoch-ms a scripted round is stamped at.
const NET_NOW: i64 = 1_700_000_000_000;

/// A NetProbe whose ONE completed round observed `reachable` — injected resolver, no network.
fn net_probe(reachable: bool) -> vike_bridge_core::NetProbe {
    let config = vike_bridge_core::NetProbeConfig {
        hosts: vec!["scripted-host".to_string()],
        interval: vike_bridge_core::DEFAULT_PROBE_INTERVAL,
        failures_before_down: 1,
    };
    let p = vike_bridge_core::NetProbe::new(config).expect("non-empty host list");
    let _ = p.probe_once_with(|_: &str| reachable, NET_NOW);
    p
}

/// An unprobed handle's optimistic `internet_up() == true` is NOT a measurement: it reads
/// `Unknown`, which the report renders as a WARN rather than a pass.
#[test]
fn an_unprobed_net_probe_reads_unknown() {
    let p = vike_bridge_core::NetProbe::with_defaults();
    let h = p.handle();
    assert!(h.internet_up(), "precondition: optimistic");
    assert!(!h.has_probed(), "precondition: unmeasured");
    assert_eq!(net_reading_of(&h), vike_model::NetReading::Unknown);
}

/// A measured-up probe reads `Alive` with the rounds it completed — the number the report prints.
#[test]
fn a_measured_up_net_probe_reads_alive_with_its_rounds() {
    let p = net_probe(true);
    let h = p.handle();
    assert_eq!(net_reading_of(&h), vike_model::NetReading::Alive { rounds: Some(h.checks()) });
    assert!(h.checks() >= 1, "the scripted round completed");
}

/// A measured-down probe reads `Dead` since the epoch-ms the flag flipped, and the reading is a
/// value: the handle is Arc-shared state, so it never depends on the probe's lifetime.
#[test]
fn a_measured_down_net_probe_reads_dead_and_outlives_its_probe() {
    let p = net_probe(false);
    let h = p.handle();
    drop(p);
    assert_eq!(net_reading_of(&h), vike_model::NetReading::Dead { since_ms: h.last_down_ms() });
}
