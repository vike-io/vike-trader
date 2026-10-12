//! The clock budget sized against the roster, each leg's TOTAL budget, and FnProbes' traits.

use super::*;

// ---- the budget is SIZED AGAINST THE ROSTER, not written down ------------------------------

/// The budget GROWS with the roster (the fixed constant, sized for six reads, silently covered a
/// growing roster for weeks). The requirement it grows toward — absorb one dead venue and still
/// read the rest, over the real registry — is
/// `crates/vike-tradehub/tests/mount_roster/preflight.rs`'s
/// `the_budget_absorbs_one_dead_venue_and_still_reads_the_rest` (only that crate holds the
/// registry, docs/decisions/0096).
#[test]
fn the_budget_grows_when_a_venue_joins_the_wired_roster() {
    assert!(
        clock_budget_for(8) > clock_budget_for(7),
        "a venue joining the wired roster must buy the leg more time"
    );
}

/// …but BOUNDED at both ends: never below the floor, and a large roster cannot make a trading
/// daemon's startup unbounded.
#[test]
fn the_budget_is_clamped_at_both_ends() {
    assert_eq!(clock_budget_for(0), DEFAULT_CLOCK_BUDGET_MS, "floor");
    assert_eq!(clock_budget_for(1), DEFAULT_CLOCK_BUDGET_MS, "floor");
    assert_eq!(clock_budget_for(10_000), MAX_CLOCK_BUDGET_MS, "ceiling");
    // A const block: the relationship is a compile-time fact, not a runtime one.
    const { assert!(MAX_CLOCK_BUDGET_MS > DEFAULT_CLOCK_BUDGET_MS) };
}

// ---- the leg's TOTAL budget ------------------------------------------------------------------

/// THE BOUND: the budget caps the WHOLE leg of blocking REST reads, not each read. Worth less
/// than two reads: the first venue is measured and every later one says in its own row that it
/// was never read.
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

/// The budget counts EVERY read, RESAMPLES INCLUDED: it caps `venues × samples × per-read
/// timeout`, not `venues × timeout`.
#[test]
fn the_budget_counts_resamples_of_a_single_venue() {
    // Each read: 2000 ms wall clock, |skew| 700 over a 2000 ms rtt -> band [0, 1700] straddles
    // the 500 ms warn threshold, so every sample is INCONCLUSIVE and asks to be retaken.
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

    // THE MUTATION: budget off, the same probes spend all three samples on the first venue and
    // then read the second — the 630 s arithmetic the budget exists to cap.
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

/// THE ACCOUNTING: the budget bounds ONLY the clock leg. `run_preflight` interleaves each
/// venue's credential probe (a blocking authed read; a geo-blocked venue sits on a TCP connect
/// for tens of seconds) between clock reads; charging it to the clock budget drops every venue
/// BEHIND it with a row blaming "an earlier venue's clock read". Measured on the Windows dev box
/// 2026-08-22: clock leg 3.2 s of its 5 s budget, preflight 27 s, and alpaca/aster/hyperliquid
/// each reported their clock "not read".
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

/// A zero/negative budget is UNBOUNDED (single-venue callers, tests), and a run with no clock
/// venue never reads the clock for a deadline it will not use.
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
        // ⚠ BOTH budgets off: the CREDENTIAL leg's own deadline is measured on the injected
        // clock, so the property is "no BOUNDED leg ⇒ no clock read". At its default here it
        // measured two reads and looked like a clock-leg regression; it was that bound working.
        credential_budget_ms: 0,
        ..PreflightConfig::default()
    };
    let _ = run_preflight(&no_clock, &probes, None);
    assert_eq!(calls.load(Ordering::Relaxed), 0, "no bounded leg ⇒ no deadline ⇒ no clock read");

    // …and the twin: with the credential budget ON the leg DOES consult the clock (a bound
    // nobody measures is not a bound).
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

/// Compile-time proof the probe bundle crosses threads (built on the main thread, the blocking
/// legs run wherever the preflight is driven from).
#[test]
fn fn_probes_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<FnProbes>();
}
