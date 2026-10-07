//! The aggregate go/no-go decision, the skip override, and the misc checks.

use super::*;

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

/// THE decoupling: a clock endpoint needs no credential, and the credential leg FAILs any venue
/// it cannot authed-read. As one list, the clock leg ran only for the reconcile-client trio, so
/// every other venue's wired endpoint was never measured.
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

/// THE off-path test: with the skip set NOT ONE probe is called — indistinguishable from no
/// preflight at all.
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

/// An empty config still produces the one global network check (a go). Nothing would mount
/// live, so an unwired probe is DECLARED not-applicable — never a measurement taken.
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

/// The ±rtt/2 floor: binance's demo host reads +247 ms over a 757 ms round trip on a healthy
/// box, which PROVES nothing (that link cannot resolve half a second); the point estimate
/// would have put it half way to a warning for no reason.
#[test]
fn a_slow_round_trip_cannot_manufacture_a_warning() {
    let cfg = PreflightConfig::default();
    // Identical samples: "no amount of resampling turns this into a warning" is the claim.
    let slow = repeated_sample(DEFAULT_CLOCK_SAMPLES, 757, 247);
    let r = check_clock_skew("binance", &cfg, &slow, None);
    assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
    assert!(r.message.contains("proven |skew| >= 0 ms"), "the floor is disclosed: {}", r.message);
    // …and a comparable skew over a TIGHT link warns: the rule costs resolution, not detection.
    let tight = check_clock_skew("binance", &cfg, &repeated_sample(1, 40, 520), None);
    assert_eq!(tight.status, CheckStatus::Warn, "{}", tight.message);
}
