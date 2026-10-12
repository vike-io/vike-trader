//! (b) The credential-validity leg: accepted, rejected, unanswered, and its own budget.

use super::*;

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
    assert!(
        r.message.contains("this proves nothing about the credentials, so it does not degrade"),
        "an operator-facing sentence, spaced like one (it once carried a run of 18 spaces): {}",
        r.message
    );
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
