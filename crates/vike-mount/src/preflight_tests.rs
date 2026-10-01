use super::*;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vike_bridge_core::{DEFAULT_PROBE_INTERVAL, NetProbe, NetProbeConfig};

/// A fixed local clock so skew arithmetic is exact.
const NOW: i64 = 1_700_000_000_000;

/// The env-map builder — same helper shape as `reconcile_config`'s tests.
fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A REAL reading, replayed: the local clock either side of one bybit `/v5/market/time` call
/// and bybit's own stamp, measured from the CI box on 2026-08-08 (rtt 193 ms, +11 ms of skew after
/// the midpoint correction). Used by the tests that simulate a drifted HOST clock by shifting
/// these local samples — the comparison input — rather than the box's clock.
const MEASURED_T0: i64 = 1_786_218_814_267;
const MEASURED_T1: i64 = 1_786_218_814_460;
const MEASURED_SERVER: i64 = 1_786_218_814_374;
/// The skew that reading actually produces: `374 - (267 + 460) / 2`.
const MEASURED_SKEW_MS: i64 = 11;

/// All four legs wired healthy: zero skew, accepted credentials, ample free space.
fn healthy() -> FnProbes {
    FnProbes::new()
        .with_now_ms(|| NOW)
        .with_venue_server_time_ms(|_: &str| Ok(NOW))
        .with_venue_authed_read(|_: &str| Ok::<(), CredentialGap>(()))
        .with_free_space_bytes(|_: &Path| Ok(DEFAULT_DISK_WARN_BYTES))
}

/// The measured bybit round trip above, replayed with the local clock shifted by
/// `host_offset_ms` (positive = this box reads AHEAD of real time).
fn replay_measured(host_offset_ms: i64) -> FnProbes {
    let samples = Arc::new(AtomicUsize::new(0));
    healthy()
        .with_now_ms(move || {
            let first = samples.fetch_add(1, Ordering::Relaxed).is_multiple_of(2);
            host_offset_ms + if first { MEASURED_T0 } else { MEASURED_T1 }
        })
        .with_venue_server_time_ms(|_: &str| Ok(MEASURED_SERVER))
}

/// A probe scripted with `(rtt_ms, skew_ms)` readings, consumed in order: the local clock
/// advances `rtt_ms` across each venue read, and the venue stamps `midpoint + skew_ms`, so the
/// check MEASURES exactly the scripted skew over exactly the scripted round trip.
fn scripted_samples(script: &[(i64, i64)]) -> FnProbes {
    let script: Vec<(i64, i64)> = script.to_vec();
    let for_server = script.clone();
    // (index of the current reading, whether the next clock read is its t0, that t0)
    let state = Arc::new(Mutex::new((0usize, true, NOW)));
    let for_now = Arc::clone(&state);
    let by_server = Arc::clone(&state);
    healthy()
        .with_now_ms(move || {
            let mut g = for_now.lock().unwrap();
            let (i, is_t0, t0) = *g;
            let rtt = script.get(i).map_or(0, |s| s.0);
            if is_t0 {
                *g = (i, false, t0);
                t0
            } else {
                *g = (i + 1, true, t0 + rtt);
                t0 + rtt
            }
        })
        .with_venue_server_time_ms(move |_: &str| {
            let (i, _, t0) = *by_server.lock().unwrap();
            let (rtt, skew) = for_server[i.min(for_server.len() - 1)];
            Ok(t0 + rtt / 2 + skew)
        })
}

/// A clock probe that declares this venue has no leg — outcome ③.
fn declared(reason: &'static str) -> FnProbes {
    healthy().with_venue_server_time_ms(move |_: &str| Err(ServerTimeGap::NotChecked(reason)))
}

/// A clock probe that declares no leg at a venue whose clock IS on the order path — outcome ④.
fn at_risk(reason: &'static str, at_stake: &'static str) -> FnProbes {
    healthy().with_venue_server_time_ms(move |_: &str| {
        Err(ServerTimeGap::UnmeasuredRisk { reason, at_stake })
    })
}

/// A clock probe whose venue publishes an endpoint that did not answer — outcome ②.
fn unreachable(why: &'static str) -> FnProbes {
    healthy()
        .with_venue_server_time_ms(move |_: &str| Err(ServerTimeGap::Unreachable(why.to_string())))
}

/// Healthy probes, except the venue server clock reads `ms` ahead of ours.
fn skewed(ms: i64) -> FnProbes {
    healthy().with_venue_server_time_ms(move |_: &str| Ok(NOW + ms))
}

/// `n` identical readings of `(rtt_ms, skew_ms)` — a venue whose behaviour does not change
/// between samples, which is what makes "resampling cannot rescue a genuinely bad clock"
/// testable.
fn repeated_sample(n: usize, rtt: i64, skew: i64) -> FnProbes {
    let script: Vec<(i64, i64)> = std::iter::repeat_n((rtt, skew), n).collect();
    scripted_samples(&script)
}

/// A server-time probe that is fatally skewed for `binance` and fine for everyone else.
fn binance_skew_only(venue: &str) -> Result<i64, ServerTimeGap> {
    if venue == "binance" { Ok(NOW + DEFAULT_CLOCK_FAIL_MS) } else { Ok(NOW) }
}

/// An authed-read probe that rejects `bybit` and accepts everyone else.
fn bybit_auth_fails(venue: &str) -> Result<(), String> {
    if venue == "bybit" { Err("403".to_string()) } else { Ok(()) }
}

/// An authed-read probe that always rejects — the dead-credentials fake.
fn auth_rejected(_venue: &str) -> Result<(), String> {
    Err("401 invalid api key".to_string())
}

/// A free-space probe that cannot answer at all.
fn disk_unqueryable(_dir: &Path) -> Result<u64, String> {
    Err("no such directory".to_string())
}

/// A config running BOTH venue legs over exactly `venues`, defaults everywhere else — the
/// shape a fully-credentialed CEX mount produces. The two lists are independent in general
/// (`the_clock_and_credential_venue_lists_are_independent` covers that).
fn cfg_for(venues: &[&str]) -> PreflightConfig {
    let venues: Vec<String> = venues.iter().map(|v| (*v).to_string()).collect();
    PreflightConfig {
        clock_venues: venues.clone(),
        credential_venues: venues,
        ..PreflightConfig::default()
    }
}

/// One journal dir plus one venue — the shape a real mount would use.
fn cfg_full() -> PreflightConfig {
    let dirs = vec![("journal".to_string(), PathBuf::from("/data/journal"))];
    PreflightConfig { dirs, ..cfg_for(&["binance"]) }
}

/// Defaults, plus ONE venue's declared clock policy.
fn cfg_with_policy(venue: &str, policy: ClockPolicy) -> PreflightConfig {
    let mut clock_policies = HashMap::new();
    clock_policies.insert(venue.to_string(), policy);
    PreflightConfig { clock_policies, ..PreflightConfig::default() }
}

/// A probe whose every local-clock read advances the wall clock by `step_ms` — the shape a
/// slow blocking REST read has — and whose venue stamp always lands exactly on the midpoint,
/// so a reading measures ZERO skew and the only thing under test is the leg's BUDGET.
fn ticking_clock(step_ms: i64) -> FnProbes {
    let now = Arc::new(AtomicI64::new(NOW));
    let for_now = Arc::clone(&now);
    let for_server = Arc::clone(&now);
    healthy()
        .with_now_ms(move || for_now.fetch_add(step_ms, Ordering::Relaxed))
        // Called between this read's t0 and t1, i.e. one step after t0: the midpoint is
        // `load - step/2`.
        .with_venue_server_time_ms(move |_: &str| {
            Ok(for_server.load(Ordering::Relaxed) - step_ms / 2)
        })
}

/// A NetProbe whose ONE completed round observed `reachable` — injected resolver, no network.
fn net_probe(reachable: bool) -> NetProbe {
    let cfg = NetProbeConfig {
        hosts: vec!["scripted-host".to_string()],
        interval: DEFAULT_PROBE_INTERVAL,
        failures_before_down: 1,
    };
    let p = NetProbe::new(cfg).expect("non-empty host list");
    let _ = p.probe_once_with(|_: &str| reachable, NOW);
    p
}

/// The disk check's status for `free` bytes under `cfg`.
fn disk_status(free: u64, cfg: &PreflightConfig) -> CheckStatus {
    let probes = FnProbes::new().with_free_space_bytes(move |_: &Path| Ok(free));
    check_disk_headroom("journal", Path::new("/j"), cfg, &probes).status
}

// ---- (a) clock skew: thresholds derived from the signers' recvWindow=5000 -------------------

#[test]
fn clock_skew_inside_the_warn_band_passes() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("binance", &cfg, &skewed(120), None);
    assert_eq!(r.status, CheckStatus::Pass);
    assert_eq!(r.venue.as_deref(), Some("binance"));
    assert_eq!(r.name, CHECK_CLOCK_SKEW);
    assert!(r.remediation.is_empty(), "a passing check carries no remediation");
}

/// The warn boundary is inclusive (`>=`).
#[test]
fn clock_skew_at_the_warn_threshold_warns() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("binance", &cfg, &skewed(DEFAULT_CLOCK_WARN_MS), None);
    assert_eq!(r.status, CheckStatus::Warn);
    assert!(!r.remediation.is_empty());
}

#[test]
fn clock_skew_just_below_the_fail_threshold_only_warns() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("binance", &cfg, &skewed(DEFAULT_CLOCK_FAIL_MS - 1), None);
    assert_eq!(r.status, CheckStatus::Warn);
}

#[test]
fn clock_skew_at_the_fail_threshold_fails() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("binance", &cfg, &skewed(DEFAULT_CLOCK_FAIL_MS), None);
    assert_eq!(r.status, CheckStatus::Fail);
    assert!(r.message.contains("2500"), "the measured skew is disclosed: {}", r.message);
}

/// A LAGGING local clock is just as fatal as a leading one — the check is on the magnitude.
#[test]
fn negative_clock_skew_is_measured_by_magnitude() {
    let cfg = PreflightConfig::default();
    let f = check_clock_skew("binance", &cfg, &skewed(-DEFAULT_CLOCK_FAIL_MS), None);
    assert_eq!(f.status, CheckStatus::Fail);
    let w = check_clock_skew("binance", &cfg, &skewed(-DEFAULT_CLOCK_WARN_MS), None);
    assert_eq!(w.status, CheckStatus::Warn);
}

/// The thresholds are config, not values baked into the logic.
#[test]
fn clock_thresholds_are_configurable() {
    let cfg =
        PreflightConfig { clock_warn_ms: 10, clock_fail_ms: 50, ..PreflightConfig::default() };
    assert_eq!(check_clock_skew("v", &cfg, &skewed(5), None).status, CheckStatus::Pass);
    assert_eq!(check_clock_skew("v", &cfg, &skewed(20), None).status, CheckStatus::Warn);
    assert_eq!(check_clock_skew("v", &cfg, &skewed(60), None).status, CheckStatus::Fail);
}

/// The RTT correction: a PERFECTLY-synced venue clock read over a slow round trip must still
/// measure ~zero skew. A pre-call-only local sample would have booked the whole 800 ms flight
/// as skew and warned; the midpoint cancels it.
#[test]
fn clock_skew_is_measured_against_the_round_trip_midpoint() {
    let cfg = PreflightConfig::default();
    let rtt = 800;
    assert!(rtt >= DEFAULT_CLOCK_WARN_MS, "precondition: an uncorrected RTT would warn");
    // The venue stamps its (identical) clock mid-flight, i.e. rtt/2 after our first sample.
    let r = check_clock_skew("binance", &cfg, &repeated_sample(1, rtt, 0), None);
    assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
    assert!(r.message.contains("clock skew 0 ms"), "{}", r.message);
}

/// ...and the correction does not hide a REAL skew riding on the same slow link. What the slow
/// link costs is RESOLUTION, not detection: 2500 ms measured over an 800 ms round trip proves
/// only 2100 ms, so it warns rather than failing, and the SAME drift over a tight link (the
/// venue next door on the same roster) proves the whole thing and fails.
#[test]
fn a_real_skew_is_still_detected_through_a_slow_round_trip() {
    let cfg = PreflightConfig::default();
    let probes = repeated_sample(DEFAULT_CLOCK_SAMPLES, 800, DEFAULT_CLOCK_FAIL_MS);
    let r = check_clock_skew("binance", &cfg, &probes, None);
    assert_eq!(r.status, CheckStatus::Warn, "{}", r.message);
    assert!(r.message.contains("clock skew 2500 ms"), "{}", r.message);
    assert!(r.message.contains("proven |skew| >= 2100 ms"), "{}", r.message);
    assert!(
        r.message.contains(&format!("best of {DEFAULT_CLOCK_SAMPLES} sample(s)")),
        "a reading whose band straddles the fail threshold is looked at again: {}",
        r.message
    );
    // The tight-link twin: the host clock is SHARED, so the venue with the sharpest round trip
    // is the one that resolves it — which is why this rule is not a hole.
    let tight = check_clock_skew("bybit", &cfg, &repeated_sample(1, 60, 2_600), None);
    assert_eq!(tight.status, CheckStatus::Fail, "{}", tight.message);
}

/// The local clock is sampled on BOTH sides of the venue read — that is what makes the
/// midpoint available at all.
#[test]
fn the_local_clock_is_sampled_on_both_sides_of_the_venue_read() {
    let cfg = PreflightConfig::default();
    let samples = Arc::new(AtomicUsize::new(0));
    let s = Arc::clone(&samples);
    let probes = healthy().with_now_ms(move || {
        s.fetch_add(1, Ordering::Relaxed);
        NOW
    });
    let r = check_clock_skew("binance", &cfg, &probes, None);
    assert_eq!(r.status, CheckStatus::Pass);
    assert_eq!(samples.load(Ordering::Relaxed), 2, "one sample each side of the venue read");
}

/// Not being able to MEASURE the skew is not evidence of a bad clock: warn, never fail, so it
/// can never degrade a venue on its own.
#[test]
fn unmeasurable_clock_skew_warns_and_does_not_degrade() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("binance", &cfg, &unreachable("t/o"), None);
    assert_eq!(r.status, CheckStatus::Warn);
    assert!(r.message.contains("t/o"));
}

// ---- the THREE outcomes: measured / unreachable / declared ---------------------------------

/// ③ A venue that publishes no clock is NOT-APPLICABLE and carries its declared reason. It is
/// not a warning, so it can never look like a fault on a healthy mount — the whole defect this
/// split exists to fix.
#[test]
fn a_declared_venue_is_not_applicable_and_states_its_reason() {
    let cfg = PreflightConfig::default();
    let why = "the Open API protobuf schema publishes no server time at all";
    let r = check_clock_skew("ctrader", &cfg, &declared(why), None);
    assert_eq!(r.status, CheckStatus::NotApplicable);
    assert_eq!(r.status.as_str(), "N/A", "it must not read as PASS/WARN/FAIL");
    assert!(r.message.contains(why), "{}", r.message);
    assert!(r.remediation.is_empty(), "there is nothing to remediate: {}", r.remediation);
}

/// ② A venue that DOES publish a clock and did not answer says exactly that — the fact that
/// used to be indistinguishable from ③.
#[test]
fn an_unreachable_venue_warns_and_says_the_venue_publishes_one() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("bybit", &cfg, &unreachable("connection timed out"), None);
    assert_eq!(r.status, CheckStatus::Warn);
    assert!(r.message.contains("publishes it"), "the row says the venue HAS one: {}", r.message);
    assert!(r.message.contains("connection timed out"), "{}", r.message);
    assert_eq!(r.remediation, REMEDY_CLOCK_UNREACHABLE);
}

/// ④ A venue with NO leg whose auth signs the clock into the order path is a WARN carrying what
/// is at stake — NOT the quiet not-applicable row. This is the polymarket shape: the roster's
/// one order-affecting clock gap was being printed as "nothing to check here".
#[test]
fn a_declared_venue_with_orders_at_stake_warns_and_says_what_is_at_stake() {
    let cfg = PreflightConfig::default();
    const REASON: &str = "its CLOB is reachable only through the SOCKS egress proxy";
    const AT_STAKE: &str = "polymarket signs POLY_TIMESTAMP into every authenticated request";
    let r = check_clock_skew("polymarket", &cfg, &at_risk(REASON, AT_STAKE), None);
    assert_eq!(r.status, CheckStatus::Warn, "an unmeasured HAZARD is not a shrug");
    assert!(r.message.contains(AT_STAKE), "{}", r.message);
    assert!(r.message.contains(REASON), "{}", r.message);
    assert_eq!(r.remediation, REMEDY_CLOCK_UNMEASURED);
    // …and it still degrades nothing: the gap is ours, the venue is not at fault.
    let cfg = PreflightConfig {
        clock_venues: vec!["polymarket".to_string()],
        ..PreflightConfig::default()
    };
    let report = run_preflight(&cfg, &at_risk(REASON, AT_STAKE), None);
    assert!(report.go());
    assert!(report.degraded_venues().is_empty());
    assert_eq!(report.venue_disposition("polymarket"), VenueDisposition::Live);
}

/// The THREE gaps must never render alike — status, message and remedy all differ. ③ and ④ are
/// the pair that used to be one, and they differ in the field an operator reads FIRST.
#[test]
fn the_declared_and_unreachable_gaps_are_distinguishable() {
    let cfg = PreflightConfig::default();
    let d =
        check_clock_skew("ctrader", &cfg, &declared("no server time exists in the schema"), None);
    let u = check_clock_skew("bybit", &cfg, &unreachable("connection timed out"), None);
    let a = check_clock_skew("polymarket", &cfg, &at_risk("no proxy yet", "orders at stake"), None);
    assert_ne!(d.status, u.status);
    assert_ne!(d.remediation, u.remediation);
    assert_ne!(d.message, u.message);
    assert_ne!(d.status, a.status, "③ is quiet, ④ is a warning — that IS the split");
    assert_ne!(a.remediation, u.remediation, "…and ④ is not the 'venue did not answer' line");
    assert_ne!(a.message, u.message);
}

/// A declared row raises NOTHING: not the worst status, not a degrade, not the go bit.
#[test]
fn a_declared_clock_leg_grounds_nothing() {
    let cfg =
        PreflightConfig { clock_venues: vec!["ctrader".to_string()], ..PreflightConfig::default() };
    let report = run_preflight(&cfg, &declared("no server time exists in the schema"), None);
    let clock: Vec<_> = report.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW).collect();
    assert_eq!(clock.len(), 1);
    assert_eq!(clock[0].status, CheckStatus::NotApplicable);
    assert!(report.go());
    assert!(report.degraded_venues().is_empty());
    assert_eq!(report.venue_disposition("ctrader"), VenueDisposition::Live);
    assert!(
        CheckStatus::NotApplicable < CheckStatus::Pass,
        "N/A must sort below PASS so it never becomes a run's worst status"
    );
}

// ---- sampling: the tightest round trip, and only when it matters ---------------------------

/// A conclusive first reading is NOT resampled — the ordinary case costs one call.
#[test]
fn a_conclusive_reading_is_not_resampled() {
    let cfg = PreflightConfig::default();
    // A second reading is scripted, and must never be reached.
    let r = check_clock_skew("bybit", &cfg, &scripted_samples(&[(200, 10), (200, 4_000)]), None);
    assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
    assert!(r.message.contains("best of 1 sample(s)"), "{}", r.message);
    assert!(r.message.contains("clock skew 10 ms"), "{}", r.message);
}

/// An INCONCLUSIVE reading (its ±rtt/2 band straddles a threshold) is resampled, and the
/// TIGHTEST round trip wins — the reading a slow, asymmetric hop cannot fake.
#[test]
fn an_inconclusive_reading_is_resampled_and_the_tightest_round_trip_wins() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("bybit", &cfg, &scripted_samples(&[(400, 400), (100, 100)]), None);
    assert_eq!(r.status, CheckStatus::Pass, "the tight sample decides: {}", r.message);
    assert!(r.message.contains("clock skew 100 ms"), "{}", r.message);
    assert!(r.message.contains("rtt 100 ms"), "{}", r.message);
    assert!(r.message.contains("best of 2 sample(s)"), "{}", r.message);
}

/// …and it is genuinely the TIGHTEST round trip that wins, not merely the last one. Three
/// inconclusive readings whose middle one is the fastest: keeping the latest instead would
/// report the 900 ms hop's 800 ms and WARN, which is the false alarm the whole rule exists to
/// prevent.
#[test]
fn the_tightest_round_trip_wins_even_when_it_is_not_the_last() {
    let cfg = PreflightConfig::default();
    let script = [(600, 400), (200, 450), (900, 800)];
    assert_eq!(cfg.clock_samples, script.len(), "every reading must be taken");
    let r = check_clock_skew("bybit", &cfg, &scripted_samples(&script), None);
    assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
    assert!(r.message.contains("clock skew 450 ms"), "{}", r.message);
    assert!(r.message.contains("rtt 200 ms"), "{}", r.message);
    assert!(r.message.contains("best of 3 sample(s)"), "{}", r.message);
}

/// …and resampling cannot hide a REAL skew: a genuinely-drifted clock survives every sample.
#[test]
fn resampling_does_not_hide_a_real_skew() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("bybit", &cfg, &scripted_samples(&[(400, 2_600), (100, 2_600)]), None);
    assert_eq!(r.status, CheckStatus::Fail, "{}", r.message);
    assert!(r.message.contains("clock skew 2600 ms"), "{}", r.message);
}

/// THE MEASURED HAZARD, replayed: bybit's demo host once read **182 ms of apparent skew over a
/// 549 ms round trip** (the CI box, 2026-08-08) while every other rep on that host read 13-23 ms.
/// That is path asymmetry surviving the midpoint correction, not clock error, and it must not
/// warn — 36% of the warn threshold from one sample is exactly how a check earns its reputation
/// for crying wolf.
#[test]
fn the_measured_round_trip_artifact_does_not_warn() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("bybit", &cfg, &scripted_samples(&[(549, 182)]), None);
    assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
}

/// The sample budget is config, not a value baked into the logic.
#[test]
fn the_sample_budget_is_configurable() {
    let cfg = PreflightConfig { clock_samples: 1, ..PreflightConfig::default() };
    // Inconclusive, but the budget forbids a second look, so the wide reading stands.
    let r = check_clock_skew("bybit", &cfg, &scripted_samples(&[(400, 400), (100, 100)]), None);
    assert!(r.message.contains("best of 1 sample(s)"), "{}", r.message);
    assert!(r.message.contains("clock skew 400 ms"), "{}", r.message);
}

/// A read that fails AFTER a good sample keeps the measurement already paid for, rather than
/// throwing it away for a "could not measure" warning.
#[test]
fn a_late_read_failure_keeps_the_sample_already_taken() {
    let cfg = PreflightConfig::default();
    let reads = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&reads);
    // The local clock advances 400 ms across each read, so the first sample is |skew| 600 over
    // a 400 ms round trip — a band of [400, 800] that straddles the warn threshold, hence
    // INCONCLUSIVE and resampled. The second read then fails.
    let ticks = Arc::new(AtomicI64::new(NOW));
    let t = Arc::clone(&ticks);
    let probes = healthy()
        .with_now_ms(move || t.fetch_add(400, Ordering::Relaxed))
        .with_venue_server_time_ms(move |_: &str| {
            if c.fetch_add(1, Ordering::Relaxed) == 0 {
                // midpoint (NOW + 200) + 600
                Ok(NOW + 800)
            } else {
                Err(ServerTimeGap::Unreachable("connection reset".to_string()))
            }
        });
    let r = check_clock_skew("bybit", &cfg, &probes, None);
    assert_eq!(reads.load(Ordering::Relaxed), 2, "the inconclusive reading WAS retaken");
    assert!(r.message.contains("clock skew 600 ms"), "{}", r.message);
    assert!(r.message.contains("best of 1 sample(s)"), "the failed read is not a sample");
    assert_eq!(r.status, CheckStatus::Pass, "600 ms over a 400 ms rtt proves only 400 ms");
}

// ---- the REAL reading, and a simulated host drift on top of it -----------------------------

/// The measured the CI box bybit reading passes — an actual `/v5/market/time` round trip against an
/// NTP-disciplined box, replayed through the real check.
#[test]
fn the_measured_bybit_reading_passes() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("bybit", &cfg, &replay_measured(0), None);
    assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
    let want = format!("clock skew {MEASURED_SKEW_MS} ms");
    assert!(r.message.contains(&want), "{}", r.message);
    assert!(r.message.contains("rtt 193 ms"), "{}", r.message);
}

/// …and the SAME reading with the host clock shifted warns, then fails, with the arithmetic
/// disclosed. This is the proof the check fires on a real drift rather than only on a
/// hand-built fixture: only the local samples move, exactly as a mis-set host clock would move
/// them.
#[test]
fn a_simulated_host_drift_on_the_measured_reading_warns_then_fails() {
    let cfg = PreflightConfig::default();

    // Host clock 1 s FAST: the venue now looks 989 ms behind us.
    let w = check_clock_skew("bybit", &cfg, &replay_measured(1_000), None);
    assert_eq!(w.status, CheckStatus::Warn, "{}", w.message);
    let want = format!("clock skew {} ms", MEASURED_SKEW_MS - 1_000);
    assert!(w.message.contains(&want), "{}", w.message);
    assert_eq!(w.remediation, REMEDY_CLOCK, "no per-venue remedy configured here");

    // Host clock 3 s SLOW: past half the recv-window budget, so the venue is a no-go.
    let f = check_clock_skew("bybit", &cfg, &replay_measured(-3_000), None);
    assert_eq!(f.status, CheckStatus::Fail, "{}", f.message);
    let want = format!("clock skew {} ms", MEASURED_SKEW_MS + 3_000);
    assert!(f.message.contains(&want), "{}", f.message);
}

// ---- the per-venue remedy ------------------------------------------------------------------

/// The remedy for a measured skew comes from the VENUE's row, so the report can never assert a
/// recv-window rejection at a venue whose auth stamps no timestamp.
#[test]
fn a_measured_skew_uses_the_venues_own_remedy() {
    const DERIBIT_REMEDY: &str = "this venue's auth stamps no timestamp";
    let cfg = cfg_with_policy(
        "deribit",
        ClockPolicy { warn_ms: DEFAULT_CLOCK_WARN_MS, fail_ms: None, remedy: DERIBIT_REMEDY },
    );

    let d = check_clock_skew("deribit", &cfg, &skewed(DEFAULT_CLOCK_WARN_MS), None);
    assert_eq!(d.status, CheckStatus::Warn);
    assert_eq!(d.remediation, DERIBIT_REMEDY);

    let b = check_clock_skew("bybit", &cfg, &skewed(DEFAULT_CLOCK_WARN_MS), None);
    assert_eq!(b.remediation, REMEDY_CLOCK, "an undeclared venue falls back to the generic line");
    assert!(
        !REMEDY_CLOCK.contains("recv"),
        "the FALLBACK must claim no rejection mechanism — it does not know the venue"
    );
}

/// THE per-venue FAIL rule: a venue whose declared policy carries no `fail_ms` tops out at a
/// WARN however far its clock reads, so the clock leg can never degrade it to paper. The
/// SAME reading at a recv-window venue fails. (`crate::server_time::clock_policy_of` is what
/// fills this in production; here the two policies are spelled out so the RULE is what is
/// under test.)
#[test]
fn a_venue_that_cannot_reject_an_order_over_drift_never_fails_its_clock_check() {
    let canary = cfg_with_policy(
        "deribit",
        ClockPolicy { warn_ms: CANARY_CLOCK_WARN_MS, fail_ms: None, remedy: "canary" },
    );
    // Twenty times the recv-window FAIL threshold, and still only a warning.
    let huge = DEFAULT_CLOCK_FAIL_MS * 20;
    let r = check_clock_skew("deribit", &canary, &skewed(huge), None);
    assert_eq!(r.status, CheckStatus::Warn, "{}", r.message);
    assert!(r.message.contains("cannot reject an order"), "the row says why: {}", r.message);
    let report = run_preflight(
        &PreflightConfig { clock_venues: vec!["deribit".to_string()], ..canary },
        &skewed(huge),
        None,
    );
    assert!(report.degraded_venues().is_empty(), "a canary venue is never degraded by a clock");
    assert_eq!(report.venue_disposition("deribit"), VenueDisposition::Live);

    // …and the same reading at a venue that DOES reject orders over drift is a FAIL.
    let signed = cfg_with_policy(
        "bybit",
        ClockPolicy {
            warn_ms: DEFAULT_CLOCK_WARN_MS,
            fail_ms: Some(DEFAULT_CLOCK_FAIL_MS),
            remedy: "signed",
        },
    );
    assert_eq!(check_clock_skew("bybit", &signed, &skewed(huge), None).status, CheckStatus::Fail);
}

/// A passing clock check carries no remedy, declared or otherwise.
#[test]
fn a_passing_clock_check_carries_no_venue_remedy() {
    let cfg = cfg_with_policy(
        "deribit",
        ClockPolicy {
            warn_ms: DEFAULT_CLOCK_WARN_MS,
            fail_ms: Some(DEFAULT_CLOCK_FAIL_MS),
            remedy: "never shown",
        },
    );
    assert!(check_clock_skew("deribit", &cfg, &skewed(1), None).remediation.is_empty());
}

/// An unwired clock leg reports NO_PROBE rather than quietly passing.
#[test]
fn an_unwired_clock_probe_warns_rather_than_passes() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("binance", &cfg, &FnProbes::new(), None);
    assert_eq!(r.status, CheckStatus::Warn);
    assert!(r.message.contains(NO_PROBE), "{}", r.message);
}

/// The clock probe is called with the venue slug being checked, once per configured venue.
#[test]
fn clock_probe_receives_the_venue_slug() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let probes = healthy().with_venue_server_time_ms(move |v: &str| {
        sink.lock().unwrap().push(v.to_string());
        Ok(NOW)
    });
    let _ = run_preflight(&cfg_for(&["binance", "bybit"]), &probes, None);
    let want = vec!["binance".to_string(), "bybit".to_string()];
    assert_eq!(*seen.lock().unwrap(), want);
}

// ---- (b) credential validity ----------------------------------------------------------------

#[test]
fn accepted_authed_read_passes() {
    let r = check_credentials("okx", &healthy(), None);
    assert_eq!(r.status, CheckStatus::Pass);
    assert_eq!(r.venue.as_deref(), Some("okx"));
    assert_eq!(r.name, CHECK_CREDENTIALS);
    assert!(r.remediation.is_empty());
}

#[test]
fn rejected_authed_read_fails_that_venue() {
    let bad = healthy().with_venue_authed_read(auth_rejected);
    let r = check_credentials("okx", &bad, None);
    assert_eq!(r.status, CheckStatus::Fail);
    assert!(r.message.contains("401 invalid api key"));
    assert!(!r.remediation.is_empty());
}

/// An unwired credential probe FAILS (unlike the clock leg): "we could not prove these keys
/// work" must not mount a venue live.
#[test]
fn an_unwired_credential_probe_fails() {
    let r = check_credentials("okx", &FnProbes::new(), None);
    assert_eq!(r.status, CheckStatus::Fail);
    assert!(r.message.contains(NO_PROBE));
}

/// THE distinction the enforcement rests on: a probe that did not ANSWER is a WARN, never a
/// FAIL — so it cannot degrade a venue and cannot flip the go bit. Being unable to measure is
/// not evidence, which is the same rule the clock leg's ②/③/④ already obey.
#[test]
fn an_unanswered_credential_probe_warns_and_never_degrades() {
    let silent = healthy().with_venue_authed_read(|_: &str| {
        Err(CredentialGap::Unanswered {
            waited_ms: 5_000,
            detail: "no answer from the venue".to_string(),
        })
    });
    let r = check_credentials("alpaca", &silent, None);
    assert_eq!(r.status, CheckStatus::Warn, "a silence must never demote a venue");
    assert_ne!(r.status, CheckStatus::Fail);
    assert!(r.message.contains("5000 ms"), "the row states its own bound: {}", r.message);
    assert!(!r.remediation.is_empty());

    // …and the report agrees: nothing degrades, and the go bit is untouched.
    let cfg = PreflightConfig {
        credential_venues: vec!["alpaca".to_string()],
        ..PreflightConfig::default()
    };
    let report = run_preflight(&cfg, &silent, None);
    assert!(report.degraded_venues().is_empty(), "{:?}", report.lines());
    assert_eq!(
        report.venue_disposition("alpaca"),
        VenueDisposition::Live,
        "a venue we merely could not reach must be mounted exactly as configured"
    );
    assert!(report.go());
}

/// …and its opposite, which is the row that now has teeth: a venue that ANSWERED and refused is
/// a FAIL, degrades, and reads `Paper`.
#[test]
fn a_rejected_credential_probe_degrades_that_venue_to_paper() {
    let refused =
        healthy().with_venue_authed_read(|_: &str| Err(CredentialGap::Rejected("401".to_string())));
    let cfg = PreflightConfig {
        credential_venues: vec!["okx".to_string()],
        ..PreflightConfig::default()
    };
    let report = run_preflight(&cfg, &refused, None);
    assert_eq!(report.degraded_venues(), vec!["okx".to_string()]);
    assert_eq!(report.venue_disposition("okx"), VenueDisposition::Paper);
    // A per-venue FAIL never grounds the process — only a GLOBAL one flips the go bit.
    assert!(report.go(), "a venue-scoped FAIL must not be a process no-go");
}

/// THE credential-leg BOUND, in the pure core: a venue the leg's budget never reached reports
/// its own row — a WARN naming the budget — rather than vanishing, and rather than being
/// reported as a credential failure it never measured.
#[test]
fn a_credential_venue_past_the_budget_is_warned_not_failed() {
    // A clock that has already passed the deadline the caller hands in.
    let probes = healthy().with_now_ms(|| NOW).with_venue_authed_read(auth_rejected);
    let r = check_credentials("bybit", &probes, Some(NOW - 1));
    assert_eq!(r.status, CheckStatus::Warn, "an UNRUN check is not a finding");
    assert_ne!(r.status, CheckStatus::Fail, "…and must never degrade the venue");
    assert!(r.message.contains("not checked"), "{}", r.message);
}

/// The leg's budget is DERIVED from how many venues it must cover, clamped at both ends — the
/// same shape (and the same rot argument) as [`clock_budget_for`].
#[test]
fn the_credential_budget_is_derived_and_clamped() {
    assert_eq!(
        credential_budget_for(0),
        DEFAULT_CREDENTIAL_BUDGET_MS,
        "a floor for a small roster"
    );
    assert_eq!(credential_budget_for(1), DEFAULT_CREDENTIAL_BUDGET_MS);
    assert_eq!(credential_budget_for(4), 4 * PER_VENUE_CREDENTIAL_ALLOWANCE_MS);
    assert!(credential_budget_for(4) > DEFAULT_CREDENTIAL_BUDGET_MS, "…and it GROWS");
    assert_eq!(credential_budget_for(10_000), MAX_CREDENTIAL_BUDGET_MS, "…up to a chosen wall");
    // The requirement it is sized against: a roster of credentialed venues must still fit its
    // per-venue allowance, so a venue joining buys time instead of squeezing its neighbours.
    for n in 1..=(MAX_CREDENTIAL_BUDGET_MS / PER_VENUE_CREDENTIAL_ALLOWANCE_MS) as usize {
        assert!(
            credential_budget_for(n) >= n as i64 * PER_VENUE_CREDENTIAL_ALLOWANCE_MS,
            "{n} credentialed venues do not fit their own allowance"
        );
    }
}

/// ⚠ The two legs must not spend EACH OTHER's budget. A slow CLOCK read is not credential-leg
/// work, so the credential deadline is pushed out by it — without that, one unreachable clock
/// endpoint would silently drop every credential row behind it and blame a leg that was fine.
/// (The mirror direction — credential cost not charged to the clock budget — is
/// `a_slow_credential_probe_does_not_spend_the_clock_budget`.)
#[test]
fn a_slow_clock_read_does_not_spend_the_credential_budget() {
    // The clock is a shared cursor. Reading it is FREE (+1 ms, so bookkeeping reads are not
    // themselves charged as work — that would make the accounting untestable); the thing that
    // takes time is the blocking venue READ, which advances the cursor by 4 s. That models the
    // real shape: one slow endpoint, and every credential venue behind it.
    let cursor = Arc::new(AtomicI64::new(NOW));
    let for_now = Arc::clone(&cursor);
    let for_server = Arc::clone(&cursor);
    let probes = healthy()
        .with_now_ms(move || for_now.fetch_add(1, Ordering::Relaxed))
        .with_venue_server_time_ms(move |_: &str| {
            // The venue answers 4 s later, with a stamp matching the (advanced) local clock —
            // so this is a SLOW read, not a skewed one, and no clock row can fail on it.
            Ok(for_server.fetch_add(4_000, Ordering::Relaxed) + 4_000)
        })
        .with_venue_authed_read(|_: &str| Ok::<(), CredentialGap>(()));
    let cfg = PreflightConfig {
        clock_venues: vec!["binance".to_string(), "bybit".to_string()],
        credential_venues: vec!["binance".to_string(), "bybit".to_string()],
        clock_budget_ms: 0, // the clock leg is not what is under test here
        credential_budget_ms: 6_000,
        ..PreflightConfig::default()
    };
    let report = run_preflight(&cfg, &probes, None);
    let starved: Vec<&CheckReport> = report
        .checks
        .iter()
        .filter(|c| c.name == CHECK_CREDENTIALS && c.message.contains("not checked"))
        .collect();
    assert!(starved.is_empty(), "a clock read spent the CREDENTIAL budget: {:?}", report.lines());
}

/// The seam is PER VENUE: one venue's dead key must not condemn the others.
#[test]
fn credential_failure_is_scoped_to_its_own_venue() {
    let probes = healthy().with_venue_authed_read(bybit_auth_fails);
    let report = run_preflight(&cfg_for(&["binance", "bybit", "okx"]), &probes, None);
    assert_eq!(report.degraded_venues(), vec!["bybit".to_string()]);
    assert_eq!(report.venue_disposition("bybit"), VenueDisposition::Paper);
    assert_eq!(report.venue_disposition("binance"), VenueDisposition::Live);
    assert_eq!(report.venue_disposition("okx"), VenueDisposition::Live);
}

// ---- (c) disk headroom ----------------------------------------------------------------------

#[test]
fn ample_free_space_passes() {
    let cfg = PreflightConfig::default();
    let r = check_disk_headroom("journal", Path::new("/j"), &cfg, &healthy());
    assert_eq!(r.status, CheckStatus::Pass, "at the warn floor exactly, still ample");
    assert_eq!(r.venue, None, "disk is a GLOBAL check, not a per-venue one");
    assert_eq!(r.name, CHECK_DISK);
    assert!(r.message.contains("journal"), "{}", r.message);
}

#[test]
fn free_space_below_the_warn_floor_warns() {
    let cfg = PreflightConfig::default();
    assert_eq!(disk_status(DEFAULT_DISK_WARN_BYTES - 1, &cfg), CheckStatus::Warn);
}

#[test]
fn free_space_below_the_fail_floor_fails() {
    let cfg = PreflightConfig::default();
    assert_eq!(disk_status(DEFAULT_DISK_FAIL_BYTES - 1, &cfg), CheckStatus::Fail);
    assert_eq!(disk_status(0, &cfg), CheckStatus::Fail);
}

#[test]
fn disk_floors_are_configurable() {
    let cfg = PreflightConfig {
        disk_warn_bytes: 2_000,
        disk_fail_bytes: 1_000,
        ..PreflightConfig::default()
    };
    assert_eq!(disk_status(5_000, &cfg), CheckStatus::Pass);
    assert_eq!(disk_status(1_500, &cfg), CheckStatus::Warn);
    assert_eq!(disk_status(500, &cfg), CheckStatus::Fail);
}

/// An unqueryable directory warns — a preflight must not ground the app on its own inability
/// to measure.
#[test]
fn unqueryable_free_space_warns() {
    let cfg = PreflightConfig::default();
    let probes = FnProbes::new().with_free_space_bytes(disk_unqueryable);
    let r = check_disk_headroom("journal", Path::new("/nope"), &cfg, &probes);
    assert_eq!(r.status, CheckStatus::Warn);
    assert!(r.message.contains("no such directory"));
}

/// Every configured dir is checked, and the configured path reaches the probe.
#[test]
fn every_configured_dir_is_checked() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let probes = healthy().with_free_space_bytes(move |p: &Path| {
        sink.lock().unwrap().push(p.display().to_string());
        Ok(DEFAULT_DISK_WARN_BYTES)
    });
    let dirs = vec![
        ("journal".to_string(), PathBuf::from("/data/journal")),
        ("hist".to_string(), PathBuf::from("/market_data/hist")),
    ];
    let cfg = PreflightConfig { dirs, ..PreflightConfig::default() };
    let report = run_preflight(&cfg, &probes, None);
    assert_eq!(seen.lock().unwrap().len(), 2);
    let disk_checks = report.checks.iter().filter(|c| c.name == CHECK_DISK).count();
    assert_eq!(disk_checks, 2, "one disk check per configured dir");
}

// ---- (d) network: the EXISTING NetProbe, read through its handle ----------------------------

#[test]
fn no_net_probe_wired_warns() {
    let r = check_network(None, true);
    assert_eq!(r.status, CheckStatus::Warn);
    assert_eq!(r.venue, None);
    assert_eq!(r.name, CHECK_NETWORK);
}

/// A run with NO venue to check is a paper run: nothing here will place an order, so the order
/// path's connectivity is not a fault to report — it is NOT APPLICABLE. It used to WARN on
/// every credential-free start, with text addressed to a DEVELOPER ("spawn a
/// vike_bridge_core::NetProbe to make it observable"), which is exactly the fires-forever-on-a
/// -healthy-box shape this crate's clock table was built to remove.
#[test]
fn a_run_with_no_venue_to_check_does_not_warn_about_a_missing_net_probe() {
    let report = run_preflight(&PreflightConfig::default(), &healthy(), None);
    let net: Vec<&CheckReport> = report.checks.iter().filter(|c| c.name == CHECK_NETWORK).collect();
    assert_eq!(net.len(), 1, "the row is still PRESENT, never vanished");
    assert_eq!(net[0].status, CheckStatus::NotApplicable, "{}", net[0].message);
    assert!(report.go());
}

/// …and the WARN survives where it means something: a venue IS being checked, so a live mount
/// is in play and its network liveness genuinely is unknown.
#[test]
fn a_missing_net_probe_still_warns_when_a_venue_is_being_checked() {
    let cfg = PreflightConfig { clock_venues: vec!["binance".to_string()], ..Default::default() };
    let report = run_preflight(&cfg, &healthy(), None);
    let net = report.checks.iter().find(|c| c.name == CHECK_NETWORK).expect("a network row");
    assert_eq!(net.status, CheckStatus::Warn, "{}", net.message);
}

/// An unprobed handle reads `internet_up() == true` optimistically — that is NOT a measurement,
/// so it must warn, not pass.
#[test]
fn an_unprobed_net_handle_warns_rather_than_passes() {
    let p = NetProbe::with_defaults();
    let h = p.handle();
    assert!(h.internet_up(), "precondition: optimistic");
    assert!(!h.has_probed(), "precondition: unmeasured");
    assert_eq!(check_network(Some(&h), true).status, CheckStatus::Warn);
}

#[test]
fn a_measured_up_net_probe_passes() {
    let p = net_probe(true);
    let r = check_network(Some(&p.handle()), true);
    assert_eq!(r.status, CheckStatus::Pass);
    assert!(r.remediation.is_empty());
}

#[test]
fn a_measured_down_net_probe_fails_globally() {
    let p = net_probe(false);
    let r = check_network(Some(&p.handle()), true);
    assert_eq!(r.status, CheckStatus::Fail);
    assert_eq!(r.venue, None, "network is global, so it flips the go bit");
}

/// The handle is Arc-shared state, so preflight never depends on the probe's lifetime.
#[test]
fn a_net_handle_outlives_its_probe() {
    let p = net_probe(false);
    let h = p.handle();
    drop(p);
    assert_eq!(check_network(Some(&h), true).status, CheckStatus::Fail);
}

// ---- the aggregate decision -----------------------------------------------------------------

/// All-green: go, nothing degraded, worst == Pass.
#[test]
fn an_all_green_run_is_a_go() {
    let p = net_probe(true);
    let probes = healthy().with_venue_server_time_ms(|_: &str| Ok(NOW + 10));
    let report = run_preflight(&cfg_full(), &probes, Some(&p.handle()));
    assert!(!report.skipped);
    assert_eq!(report.worst(), CheckStatus::Pass);
    assert!(report.go());
    assert!(report.degraded_venues().is_empty());
    assert!(report.failures().is_empty());
    assert_eq!(report.checks.len(), 4, "network + 1 dir + clock + credentials");
}

/// Deterministic ordering: network first, then dirs, then per-venue clock+credentials.
#[test]
fn checks_are_emitted_in_a_deterministic_order() {
    let dirs = vec![("journal".to_string(), PathBuf::from("/j"))];
    let cfg = PreflightConfig { dirs, ..cfg_for(&["binance", "bybit"]) };
    let report = run_preflight(&cfg, &healthy(), None);
    let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
    let want_names = vec![
        CHECK_NETWORK,
        CHECK_DISK,
        CHECK_CLOCK_SKEW,
        CHECK_CREDENTIALS,
        CHECK_CLOCK_SKEW,
        CHECK_CREDENTIALS,
    ];
    assert_eq!(names, want_names);
    let venues: Vec<_> = report.checks.iter().map(|c| c.venue.as_deref()).collect();
    let b = Some("binance");
    let y = Some("bybit");
    assert_eq!(venues, vec![None, None, b, b, y, y]);
}

/// THE degrade-to-paper policy: a per-venue hard FAIL never blocks the go — that venue goes
/// paper and the process still starts (the "absent credentials => stay paper" idiom).
#[test]
fn a_venue_failure_degrades_that_venue_but_is_still_a_go() {
    let probes = healthy().with_venue_server_time_ms(binance_skew_only);
    let report = run_preflight(&cfg_for(&["binance", "okx"]), &probes, None);
    assert!(report.go(), "a venue-scoped failure must never be a process no-go");
    assert_eq!(report.worst(), CheckStatus::Fail);
    assert_eq!(report.degraded_venues(), vec!["binance".to_string()]);
    assert_eq!(report.venue_disposition("binance"), VenueDisposition::Paper);
    assert_eq!(report.venue_disposition("okx"), VenueDisposition::Live);
}

/// THE decoupling: the clock list and the credential list are independent, because a clock
/// endpoint needs no credential and the credential leg FAILs any venue it cannot authed-read.
/// While they were one list, the clock leg only ever ran for venues that had a reconcile client
/// (the crypto-CEX trio) — so every other venue's endpoint could be wired and still never
/// measured.
#[test]
fn the_clock_and_credential_venue_lists_are_independent() {
    let cfg = PreflightConfig {
        clock_venues: vec!["deribit".to_string(), "binance".to_string()],
        credential_venues: vec!["binance".to_string()],
        ..PreflightConfig::default()
    };
    let report = run_preflight(&cfg, &healthy(), None);
    let rows: Vec<(&str, Option<&str>)> =
        report.checks.iter().map(|c| (c.name.as_str(), c.venue.as_deref())).collect();
    let want = vec![
        (CHECK_NETWORK, None),
        // deribit is clock-checked without an authed read…
        (CHECK_CLOCK_SKEW, Some("deribit")),
        // …and binance gets both legs, grouped together.
        (CHECK_CLOCK_SKEW, Some("binance")),
        (CHECK_CREDENTIALS, Some("binance")),
    ];
    assert_eq!(rows, want);
    assert!(report.go());
    assert!(
        report.degraded_venues().is_empty(),
        "a clock-only venue must never be failed by the credential leg it was not listed for"
    );
}

/// A venue that was never checked is never demoted — preflight only DEMOTES.
#[test]
fn an_unchecked_venue_stays_live() {
    let report = run_preflight(&cfg_for(&[]), &FnProbes::new(), None);
    assert_eq!(report.venue_disposition("deribit"), VenueDisposition::Live);
}

/// A venue with two failing legs is listed ONCE.
#[test]
fn degraded_venues_are_deduplicated() {
    let bad = skewed(DEFAULT_CLOCK_FAIL_MS);
    let probes = bad.with_venue_authed_read(auth_rejected);
    let report = run_preflight(&cfg_for(&["aster"]), &probes, None);
    assert_eq!(report.failures().len(), 2, "both venue legs failed");
    assert_eq!(report.degraded_venues(), vec!["aster".to_string()], "but listed once");
}

/// A GLOBAL hard failure (full disk) IS a no-go, and demotes no single venue.
#[test]
fn a_global_failure_is_a_no_go() {
    let probes = healthy().with_free_space_bytes(|_: &Path| Ok(0));
    let report = run_preflight(&cfg_full(), &probes, None);
    assert!(!report.go(), "no disk headroom is a process-wide no-go");
    assert!(report.degraded_venues().is_empty(), "a global fail demotes no single venue");
    assert_eq!(report.venue_disposition("binance"), VenueDisposition::Live);
}

/// Warnings alone never block anything.
#[test]
fn warnings_alone_are_still_a_go() {
    let probes = skewed(DEFAULT_CLOCK_WARN_MS);
    let report = run_preflight(&cfg_for(&["binance"]), &probes, None);
    assert_eq!(report.worst(), CheckStatus::Warn);
    assert!(report.go());
    assert!(report.degraded_venues().is_empty());
    assert_eq!(report.venue_disposition("binance"), VenueDisposition::Live);
}

/// Pass < Warn < Fail, which is what makes `worst()` a plain max().
#[test]
fn status_severity_is_ordered() {
    assert!(CheckStatus::Pass < CheckStatus::Warn);
    assert!(CheckStatus::Warn < CheckStatus::Fail);
    assert_eq!(CheckStatus::Fail.as_str(), "FAIL");
    assert!(CheckStatus::Fail.is_fail());
    assert!(!CheckStatus::Warn.is_fail());
}

/// Every check renders as one operator line, with remediation only where there is one.
#[test]
fn lines_render_status_name_scope_and_remediation() {
    let probes = healthy().with_venue_authed_read(auth_rejected);
    let report = run_preflight(&cfg_for(&["okx"]), &probes, None);
    let lines = report.lines();
    assert_eq!(lines.len(), 3, "network + clock + credentials");
    let head = "[FAIL] credentials (okx): ";
    assert!(lines.iter().any(|l| l.starts_with(head)), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains(" — ")), "a failing line carries remediation");
}

// ---- the skip override (the OFF path) -------------------------------------------------------

#[test]
fn preflight_skipped_true_only_for_exact_one() {
    assert!(preflight_skipped(&map(&[("VIKE_PREFLIGHT_SKIP", "1")])));
    assert!(!preflight_skipped(&map(&[("VIKE_PREFLIGHT_SKIP", "true")])));
    assert!(!preflight_skipped(&map(&[("VIKE_PREFLIGHT_SKIP", "yes")])));
    assert!(!preflight_skipped(&map(&[("VIKE_PREFLIGHT_SKIP", "0")])));
    assert!(!preflight_skipped(&map(&[("VIKE_PREFLIGHT_SKIP", "")])));
    assert!(!preflight_skipped(&map(&[])));
}

/// THE off-path test: with the skip set, NOT ONE probe is called, the report is empty, it is a
/// go, and every venue stays Live — indistinguishable from having no preflight at all.
#[test]
fn the_skip_override_calls_no_probe_and_leaves_every_venue_live() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c1 = Arc::clone(&calls);
    let c2 = Arc::clone(&calls);
    let c3 = Arc::clone(&calls);
    let c4 = Arc::clone(&calls);
    let probes = FnProbes::new()
        .with_now_ms(move || {
            c1.fetch_add(1, Ordering::Relaxed);
            NOW
        })
        .with_venue_server_time_ms(move |_: &str| {
            c2.fetch_add(1, Ordering::Relaxed);
            Ok(NOW + DEFAULT_CLOCK_FAIL_MS)
        })
        .with_venue_authed_read(move |_: &str| {
            c3.fetch_add(1, Ordering::Relaxed);
            Err("401".to_string())
        })
        .with_free_space_bytes(move |_: &Path| {
            c4.fetch_add(1, Ordering::Relaxed);
            Ok(0)
        });
    let vars = map(&[("VIKE_PREFLIGHT_SKIP", "1")]);

    let report = run_preflight_gated(&vars, &cfg_full(), &probes, None);

    assert_eq!(calls.load(Ordering::Relaxed), 0, "the skip path must not probe anything");
    assert!(report.skipped);
    assert!(report.checks.is_empty());
    assert_eq!(report.worst(), CheckStatus::Pass);
    assert!(report.go());
    assert!(report.degraded_venues().is_empty());
    assert_eq!(report.venue_disposition("binance"), VenueDisposition::Live);
    assert!(report.lines().is_empty());
    assert!(report.failures().is_empty());
}

/// Unset (the DEFAULT) runs the checks — the skip is opt-in, not opt-out.
#[test]
fn unset_skip_runs_the_checks() {
    let vars = map(&[]);
    let cfg = cfg_for(&["binance"]);
    let report = run_preflight_gated(&vars, &cfg, &healthy(), None);
    assert!(!report.skipped);
    assert_eq!(report.checks.len(), 3, "network + clock + credentials");
}

/// A non-exact value (`"true"`) does NOT skip — same unfuzzy idiom as VIKE_RECONCILE.
#[test]
fn a_fuzzy_skip_value_does_not_skip() {
    let vars = map(&[("VIKE_PREFLIGHT_SKIP", "true")]);
    let cfg = cfg_for(&["binance"]);
    let report = run_preflight_gated(&vars, &cfg, &healthy(), None);
    assert!(!report.skipped);
    assert!(!report.checks.is_empty());
}

// ---- misc -----------------------------------------------------------------------------------

/// An empty config still produces the one global network check (and is a go). With no venue to
/// check, nothing would mount live, so an unwired probe is DECLARED not-applicable rather than
/// an unknown — and must still never read as a measurement that was taken.
#[test]
fn an_empty_config_still_reports_the_network_check() {
    let cfg = PreflightConfig::default();
    let report = run_preflight(&cfg, &FnProbes::new(), None);
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].name, CHECK_NETWORK);
    assert!(report.go());
    assert_eq!(report.checks[0].status, CheckStatus::NotApplicable);
    assert_ne!(report.checks[0].status, CheckStatus::Pass, "never a fake PASS");
}

#[test]
fn fmt_bytes_renders_binary_units() {
    assert_eq!(fmt_bytes(512), "512 B");
    assert_eq!(fmt_bytes(1024 * 1024), "1.0 MiB");
    assert_eq!(fmt_bytes(1024 * 1024 * 1024), "1.0 GiB");
    assert_eq!(fmt_bytes(DEFAULT_DISK_WARN_BYTES), "5.0 GiB");
}

/// The default thresholds are the documented fractions of the signers' recvWindow=5000.
#[test]
fn default_clock_thresholds_are_derived_from_the_recv_window() {
    const RECV_WINDOW_MS: i64 = 5_000;
    assert_eq!(DEFAULT_CLOCK_FAIL_MS, RECV_WINDOW_MS / 2, "fail at half the budget");
    assert_eq!(DEFAULT_CLOCK_WARN_MS, RECV_WINDOW_MS / 10, "warn at 10% of the budget");
    let d = PreflightConfig::default();
    assert_eq!(d.clock_warn_ms, DEFAULT_CLOCK_WARN_MS);
    assert_eq!(d.clock_fail_ms, DEFAULT_CLOCK_FAIL_MS);
    assert_eq!(d.clock_samples, DEFAULT_CLOCK_SAMPLES);
    assert_eq!(d.clock_budget_ms, DEFAULT_CLOCK_BUDGET_MS);
    assert!(d.clock_venues.is_empty(), "nothing is checked unless configured");
    assert!(d.credential_venues.is_empty(), "nothing is checked unless configured");
    assert!(d.clock_policies.is_empty(), "no venue policy unless the caller supplies one");
    assert!(d.dirs.is_empty(), "nothing is checked unless configured");
}

/// The ±rtt/2 floor, at the row that needs it: binance's demo host reads +247 ms over a 757 ms
/// round trip on a healthy box, which PROVES nothing at all — a link that slow cannot resolve
/// half a second. Judging the point estimate instead would have put a 247 ms reading half way
/// to a warning for no reason.
#[test]
fn a_slow_round_trip_cannot_manufacture_a_warning() {
    let cfg = PreflightConfig::default();
    // Every sample repeats the same reading — a venue whose behaviour does not change between
    // looks, which is what makes "no amount of resampling turns this into a warning" the claim.
    let slow = repeated_sample(DEFAULT_CLOCK_SAMPLES, 757, 247);
    let r = check_clock_skew("binance", &cfg, &slow, None);
    assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
    assert!(r.message.contains("proven |skew| >= 0 ms"), "the floor is disclosed: {}", r.message);
    // …and a comparable skew over a TIGHT link does warn: the rule costs resolution, not
    // detection.
    let tight = check_clock_skew("binance", &cfg, &repeated_sample(1, 40, 520), None);
    assert_eq!(tight.status, CheckStatus::Warn, "{}", tight.message);
}

// ---- the budget is SIZED AGAINST THE ROSTER, not written down ------------------------------

/// The budget GROWS with the roster, which is the half that stops it rotting: the fixed constant
/// was sized against six reads and silently covered a growing roster for weeks. The requirement it
/// grows toward — absorb one dead venue and still read every other one, over the real registry's
/// wired clocks — is `crates/vike-tradehub/tests/mount_roster/preflight.rs`'s
/// `the_budget_absorbs_one_dead_venue_and_still_reads_the_rest`, which moved there with the
/// venue mount contract's finish (docs/decisions/0096) because only that crate holds the registry.
#[test]
fn the_budget_grows_when_a_venue_joins_the_wired_roster() {
    assert!(
        clock_budget_for(8) > clock_budget_for(7),
        "a venue joining the wired roster must buy the leg more time"
    );
}

/// …but stays BOUNDED at both ends: a small roster never drops below the floor the leg was
/// always given, and a large one cannot make a trading daemon's startup unbounded.
#[test]
fn the_budget_is_clamped_at_both_ends() {
    assert_eq!(clock_budget_for(0), DEFAULT_CLOCK_BUDGET_MS, "floor");
    assert_eq!(clock_budget_for(1), DEFAULT_CLOCK_BUDGET_MS, "floor");
    assert_eq!(clock_budget_for(10_000), MAX_CLOCK_BUDGET_MS, "ceiling");
    // A const block: the relationship is a compile-time fact, not a runtime one.
    const { assert!(MAX_CLOCK_BUDGET_MS > DEFAULT_CLOCK_BUDGET_MS) };
}

// ---- the leg's TOTAL budget ------------------------------------------------------------------

/// THE BOUND: the clock leg is a series of blocking REST reads, and the budget caps the WHOLE
/// leg rather than each read. With a budget worth less than two reads, the first venue is
/// measured and every later one says — in its own row — that it was never read, so the leg's
/// cost is bounded no matter how many venues are wired.
#[test]
fn the_leg_budget_stops_reading_and_says_so() {
    let read_ms = 3_000;
    let cfg = PreflightConfig {
        clock_venues: vec!["binance".to_string(), "bybit".to_string(), "okx".to_string()],
        clock_budget_ms: 5_000,
        ..PreflightConfig::default()
    };
    let report = run_preflight(&cfg, &ticking_clock(read_ms), None);
    let clock: Vec<&CheckReport> =
        report.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW).collect();
    assert_eq!(clock.len(), 3, "every venue still gets a ROW");
    assert_eq!(clock[0].status, CheckStatus::Pass, "{}", clock[0].message);
    for row in &clock[1..] {
        assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
        assert!(row.message.contains("5000 ms budget"), "the row names it: {}", row.message);
        assert!(row.message.contains("not read"), "{}", row.message);
    }
    assert!(report.go(), "an unread venue never grounds a mount");
    assert!(report.degraded_venues().is_empty(), "…and never degrades one either");
}

/// The budget counts EVERY read, RESAMPLES INCLUDED — the arithmetic it exists to cap is
/// `venues × samples × per-read timeout`, not `venues × timeout`. A venue whose reading is
/// inconclusive would otherwise take three reads on its own.
#[test]
fn the_budget_counts_resamples_of_a_single_venue() {
    // Each read costs 2000 ms of wall clock and lands |skew| 700 over a 2000 ms round trip:
    // the band is [0, 1700], which straddles the 500 ms warn threshold, so every sample is
    // INCONCLUSIVE and asks to be retaken.
    let probes = || {
        let now = Arc::new(AtomicI64::new(NOW));
        let for_now = Arc::clone(&now);
        let for_server = Arc::clone(&now);
        healthy()
            .with_now_ms(move || for_now.fetch_add(2_000, Ordering::Relaxed))
            .with_venue_server_time_ms(move |_: &str| {
                Ok(for_server.load(Ordering::Relaxed) - 1_000 + 700)
            })
    };
    let cfg = PreflightConfig {
        clock_venues: vec!["binance".to_string(), "bybit".to_string()],
        clock_budget_ms: 5_000,
        clock_samples: 3,
        ..PreflightConfig::default()
    };
    let report = run_preflight(&cfg, &probes(), None);
    let clock: Vec<&CheckReport> =
        report.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW).collect();
    assert!(
        clock[0].message.contains("best of 1 sample(s)"),
        "the budget stopped an inconclusive reading from being retaken: {}",
        clock[0].message
    );
    assert!(
        clock[1].message.contains("budget"),
        "…and the second venue was never read at all: {}",
        clock[1].message
    );

    // THE MUTATION: with the budget off, the same probes spend all three samples on the first
    // venue and then read the second — which is the 630 s arithmetic the budget exists to cap.
    let unbounded = PreflightConfig { clock_budget_ms: 0, ..cfg };
    let report = run_preflight(&unbounded, &probes(), None);
    let clock: Vec<&CheckReport> =
        report.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW).collect();
    for row in clock {
        assert!(
            row.message.contains("best of 3 sample(s)"),
            "unbounded, every venue is read until its sample budget runs out: {}",
            row.message
        );
    }
}

/// THE ACCOUNTING: the budget bounds the CLOCK leg, and ONLY the clock leg. `run_preflight`
/// interleaves each venue's credential probe between the clock reads, and a credential probe is
/// a blocking authed read of its own — a geo-blocked venue sits on a TCP connect for tens of
/// seconds. Charging that wall clock to the clock budget silently drops the clock check of
/// every venue BEHIND it, and the row it emits blames "an earlier venue's clock read", sending
/// an operator to inspect rows that are all fast and healthy. Measured on the Windows dev box
/// 2026-08-22: the whole clock leg cost 3.2 s of its 5 s budget, the preflight took 27 s, and
/// alpaca/aster/hyperliquid each reported their clock "not read".
#[test]
fn a_slow_credential_probe_does_not_spend_the_clock_budget() {
    let now = Arc::new(AtomicI64::new(NOW));
    let (for_now, for_server, for_auth) = (Arc::clone(&now), Arc::clone(&now), Arc::clone(&now));
    // Every clock read costs 100 ms; the ONE credential probe costs 10 s — twice the budget.
    let probes = healthy()
        .with_now_ms(move || for_now.fetch_add(100, Ordering::Relaxed))
        .with_venue_server_time_ms(move |_: &str| Ok(for_server.load(Ordering::Relaxed) - 50))
        .with_venue_authed_read(move |_: &str| {
            for_auth.fetch_add(10_000, Ordering::Relaxed);
            Ok::<(), CredentialGap>(())
        });
    let cfg = PreflightConfig {
        clock_venues: vec!["binance".to_string(), "bybit".to_string()],
        credential_venues: vec!["binance".to_string()],
        clock_budget_ms: 5_000,
        ..PreflightConfig::default()
    };
    let report = run_preflight(&cfg, &probes, None);
    let clock: Vec<&CheckReport> =
        report.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW).collect();
    assert_eq!(clock.len(), 2, "every clock venue still gets a ROW");
    assert!(
        !clock[1].message.contains("budget"),
        "the clock leg spent 200 ms of its 5000 ms budget — the 10 s credential probe is not \
             its cost: {}",
        clock[1].message
    );
    assert_eq!(clock[1].status, CheckStatus::Pass, "{}", clock[1].message);
}

/// A zero/negative budget is UNBOUNDED — the shape a single-venue caller or a test wants — and
/// a run with no clock venue never reads the clock for a deadline it will not use.
#[test]
fn a_zero_budget_is_unbounded_and_an_empty_leg_reads_no_clock() {
    let cfg = PreflightConfig {
        clock_venues: vec!["binance".to_string(), "bybit".to_string(), "okx".to_string()],
        clock_budget_ms: 0,
        ..PreflightConfig::default()
    };
    let report = run_preflight(&cfg, &ticking_clock(3_000), None);
    let budgeted = report.checks.iter().filter(|c| c.message.contains("budget")).count();
    assert_eq!(budgeted, 0, "an unbounded leg reads every venue");

    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&calls);
    let probes = healthy().with_now_ms(move || {
        c.fetch_add(1, Ordering::Relaxed);
        NOW
    });
    let no_clock = PreflightConfig {
        credential_venues: vec!["binance".to_string()],
        // ⚠ BOTH budgets off. The CREDENTIAL leg gained a deadline of its own, and a deadline
        // is measured on the injected clock — so this property is now "no BOUNDED leg ⇒ no
        // clock read", not "no clock leg ⇒ no clock read". Leaving the credential budget at its
        // default here measured two reads and looked like a regression in the clock leg; it was
        // the new bound doing exactly its job.
        credential_budget_ms: 0,
        ..PreflightConfig::default()
    };
    let _ = run_preflight(&no_clock, &probes, None);
    assert_eq!(calls.load(Ordering::Relaxed), 0, "no bounded leg ⇒ no deadline ⇒ no clock read");

    // …and the twin the new bound needs: with the credential budget ON, the leg DOES consult
    // the clock, because that is what a deadline is. A bound nobody measures is not a bound.
    let counted = Arc::new(AtomicUsize::new(0));
    let c2 = Arc::clone(&counted);
    let bounded_probes = healthy().with_now_ms(move || {
        c2.fetch_add(1, Ordering::Relaxed);
        NOW
    });
    let bounded = PreflightConfig {
        credential_venues: vec!["binance".to_string()],
        credential_budget_ms: DEFAULT_CREDENTIAL_BUDGET_MS,
        ..PreflightConfig::default()
    };
    let _ = run_preflight(&bounded, &bounded_probes, None);
    assert!(counted.load(Ordering::Relaxed) > 0, "a bounded credential leg must measure time");
}

/// FnProbes' Debug must never leak a closure's captured environment.
#[test]
fn fn_probes_debug_is_opaque() {
    assert_eq!(format!("{:?}", FnProbes::new()), "FnProbes(<injected closures>)");
}

/// Compile-time proof that the probe bundle crosses threads: a mount site builds it on the
/// main thread and the real (blocking-REST) legs run wherever the preflight is driven from.
#[test]
fn fn_probes_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FnProbes>();
}
