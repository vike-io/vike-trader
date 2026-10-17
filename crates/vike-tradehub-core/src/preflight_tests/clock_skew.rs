//! (a) The clock-skew leg: thresholds, the outcomes, sampling, the real reading, the remedy.

use super::*;

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

/// The RTT correction: a PERFECTLY-synced clock over a slow round trip must measure ~zero skew;
/// a pre-call-only sample would book the whole 800 ms flight as skew and warn.
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

/// ...and it does not hide a REAL skew on the same slow link: the cost is RESOLUTION, not
/// detection. 2500 ms measured over an 800 ms round trip proves only 2100 ms (warn); the SAME
/// drift over a tight link proves it all (fail).
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
    // The tight-link twin: the host clock is SHARED, so the sharpest round trip resolves it —
    // why this rule is not a hole.
    let tight = check_clock_skew("bybit", &cfg, &repeated_sample(1, 60, 2_600), None);
    assert_eq!(tight.status, CheckStatus::Fail, "{}", tight.message);
}

/// The local clock is sampled on BOTH sides of the venue read (what makes a midpoint exist).
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

/// Failing to MEASURE the skew is not evidence of a bad clock: warn, never fail or degrade.
#[test]
fn unmeasurable_clock_skew_warns_and_does_not_degrade() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("binance", &cfg, &unreachable("t/o"), None);
    assert_eq!(r.status, CheckStatus::Warn);
    assert!(r.message.contains("t/o"));
}

// ---- the FOUR outcomes: ① measured (above) / ② unreachable / ③ declared / ④ unmeasured risk ----

/// ③ A venue that publishes no clock is NOT-APPLICABLE with its declared reason — never a
/// warning that looks like a fault on a healthy mount (the defect this split fixes).
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

/// ② A venue that DOES publish a clock and did not answer says so (once indistinguishable
/// from ③).
#[test]
fn an_unreachable_venue_warns_and_says_the_venue_publishes_one() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("bybit", &cfg, &unreachable("connection timed out"), None);
    assert_eq!(r.status, CheckStatus::Warn);
    assert!(r.message.contains("publishes it"), "the row says the venue HAS one: {}", r.message);
    assert!(r.message.contains("connection timed out"), "{}", r.message);
    assert_eq!(r.remediation, REMEDY_CLOCK_UNREACHABLE);
}

/// ④ A venue with NO leg whose auth signs the clock into the order path WARNs with what is at
/// stake, NOT the quiet N/A row (polymarket: the roster's one order-affecting clock gap had
/// printed as "nothing to check here").
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

/// The THREE gaps never render alike (status, message, remedy); ③ and ④, once one, differ in
/// the field an operator reads FIRST.
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

/// An INCONCLUSIVE reading (±rtt/2 band straddles a threshold) is resampled; the TIGHTEST
/// round trip wins (a slow, asymmetric hop cannot fake it).
#[test]
fn an_inconclusive_reading_is_resampled_and_the_tightest_round_trip_wins() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("bybit", &cfg, &scripted_samples(&[(400, 400), (100, 100)]), None);
    assert_eq!(r.status, CheckStatus::Pass, "the tight sample decides: {}", r.message);
    assert!(r.message.contains("clock skew 100 ms"), "{}", r.message);
    assert!(r.message.contains("rtt 100 ms"), "{}", r.message);
    assert!(r.message.contains("best of 2 sample(s)"), "{}", r.message);
}

/// …the TIGHTEST, not the last: with the middle of three readings fastest, keeping the latest
/// would report the 900 ms hop's 800 ms and WARN — the false alarm the rule exists to prevent.
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
/// 549 ms round trip** (the CI box, 2026-08-08) while every other rep read 13-23 ms: path asymmetry
/// surviving the midpoint, not clock error. 36% of the warn threshold from one sample must not
/// warn (crying wolf).
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

/// A read failing AFTER a good sample keeps the measurement already paid for (no "could not
/// measure" warning).
#[test]
fn a_late_read_failure_keeps_the_sample_already_taken() {
    let cfg = PreflightConfig::default();
    let reads = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&reads);
    // 400 ms per read: the first sample is |skew| 600 over a 400 ms rtt, band [400, 800]
    // straddles the warn threshold -> INCONCLUSIVE, resampled; the second read fails.
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

/// The measured the CI box bybit reading (a real `/v5/market/time` round trip, NTP-disciplined box)
/// passes the real check.
#[test]
fn the_measured_bybit_reading_passes() {
    let cfg = PreflightConfig::default();
    let r = check_clock_skew("bybit", &cfg, &replay_measured(0), None);
    assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
    let want = format!("clock skew {MEASURED_SKEW_MS} ms");
    assert!(r.message.contains(&want), "{}", r.message);
    assert!(r.message.contains("rtt 193 ms"), "{}", r.message);
}

/// …and the SAME reading with the host clock shifted warns, then fails, arithmetic disclosed:
/// proof the check fires on a real drift, not only a hand-built fixture (only the local
/// samples move, as a mis-set host clock moves them).
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

/// A measured skew's remedy comes from the VENUE's row: never a recv-window rejection claimed
/// at a venue whose auth stamps no timestamp.
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

/// THE per-venue FAIL rule: a policy with no `fail_ms` tops out at WARN however far the clock
/// reads (never degraded to paper); the SAME reading at a recv-window venue fails. Production
/// fills it via `crate::server_time::clock_policy_of`; spelled out here so the RULE is tested.
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
