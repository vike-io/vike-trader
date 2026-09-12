//! Gated: these drive the venue-direct collectors, which a default build does not compile
//! (feature `venue-backfill`). Without it this file is an empty test binary — the same shape
//! `vike-tradehub`'s `telegram_control.rs` uses for its own off-by-default feature.
#![cfg(feature = "venue-backfill")]

//! Offline gate for the collector supervisor's documented config + its planning brain. NO network,
//! NO store, NO sleeping — the shipped example roster is parsed by the REAL parser (so the file the
//! bin's doc comment points operators at can never drift from what the code accepts), and the pure
//! scheduling/heal decisions are re-asserted end to end over it.

use std::path::Path;

use vike_backfill::supervisor::{
    JobReason, SupervisorConfig, collector_by_name, effective_interval_ms, is_due, is_parked,
    load_supervisor_config, next_run_ms, plan_pass,
};

fn example() -> SupervisorConfig {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/supervisor_example.toml");
    load_supervisor_config(&path).expect("the shipped example roster parses and validates")
}

#[test]
fn the_shipped_example_roster_parses_and_names_only_registered_collectors() {
    let cfg = example();
    assert_eq!(cfg.tick_secs, 30);
    assert_eq!(cfg.sources.len(), 3);
    for src in &cfg.sources {
        let c = collector_by_name(&src.collector).unwrap_or_else(|| {
            panic!("example names an unregistered collector {:?}", src.collector)
        });
        assert_eq!(src.kind, c.kind);
        assert!(!src.symbols.is_empty());
        assert!(src.cadence_secs > 0);
    }
    let names: Vec<&str> = cfg.sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["binance-majors-1m", "bybit-btc-1h", "okx-btc-5m"]);
}

#[test]
fn the_example_covers_the_heal_on_and_heal_off_shapes() {
    let cfg = example();
    assert!(cfg.sources[0].heal, "the first source heals (the default)");
    assert_eq!(cfg.sources[0].max_heal_jobs, 8);
    assert_eq!(cfg.sources[1].max_consecutive_failures, Some(12));
    assert!(!cfg.sources[2].heal, "the third source is freshness-only");
}

#[test]
fn a_healing_source_plans_freshness_first_then_bounded_gap_work() {
    let cfg = example();
    let src = &cfg.sources[0];
    let now = 1_700_000_000_000_i64;
    // 10 holes, but max_heal_jobs = 8 -> 1 fresh + 8 heal. (A cold series — watermark 0 — so the
    // freshness window is the full configured lookback.)
    let gaps: Vec<(i64, i64)> = (0..10).map(|i| (i * 1_000, i * 1_000 + 500)).collect();
    let jobs = plan_pass(src, "BTCUSDT", &gaps, now, 0, 0);
    assert_eq!(jobs.len(), 9);
    assert_eq!(jobs[0].reason, JobReason::Fresh);
    assert_eq!((jobs[0].start_ms, jobs[0].end_ms), (now - 86_400_000, now));
    assert!(jobs[1..].iter().all(|j| j.reason == JobReason::Heal));
    assert_eq!((jobs[1].start_ms, jobs[1].end_ms), (0, 500), "oldest hole first at cursor 0");
    assert_eq!((jobs[8].start_ms, jobs[8].end_ms), (7_000, 7_500));

    // The next pass rotates on by `max_heal_jobs`, so holes 9 and 10 are actually reached even
    // though the first eight can never be closed (a 0-row fetch records no commit key).
    let jobs = plan_pass(src, "BTCUSDT", &gaps, now, 0, src.max_heal_jobs);
    assert_eq!((jobs[1].start_ms, jobs[1].end_ms), (8_000, 8_500));
    assert_eq!((jobs[2].start_ms, jobs[2].end_ms), (9_000, 9_500));
    assert_eq!((jobs[3].start_ms, jobs[3].end_ms), (0, 500), "then wraps");
}

#[test]
fn a_current_series_plans_no_freshness_window() {
    // THE ANTI-DUPLICATE PIN over the shipped roster: the store dedups by batch commit key, so a
    // window sliding with the clock would re-append the same bars every 5 minutes. Anchored on the
    // watermark, an up-to-date series plans nothing at all.
    let cfg = example();
    let src = &cfg.sources[0];
    let now = 1_700_000_000_000_i64;
    assert!(plan_pass(src, "BTCUSDT", &[], now, now, 0).is_empty());
    // one minute of new bars = a one-minute window, NOT the whole 24h lookback again
    let jobs = plan_pass(src, "BTCUSDT", &[], now, now - 60_000, 0);
    assert_eq!(jobs.len(), 1);
    assert_eq!((jobs[0].start_ms, jobs[0].end_ms), (now, now));
}

#[test]
fn a_freshness_only_source_ignores_its_gaps_entirely() {
    let cfg = example();
    let src = &cfg.sources[2];
    let now = 1_700_000_000_000_i64;
    let jobs = plan_pass(src, "BTC-USDT", &[(1, 2), (3, 4)], now, 0, 0);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].reason, JobReason::Fresh);
    assert_eq!((jobs[0].start_ms, jobs[0].end_ms), (now - 43_200_000, now));
}

#[test]
fn the_example_cadences_drive_the_expected_run_decisions() {
    let cfg = example();
    let now = 1_700_000_000_000_i64;

    // binance: 300s cadence, healthy.
    let src = &cfg.sources[0];
    let healthy = effective_interval_ms(src.cadence_ms(), 0, src.max_backoff_ms());
    assert_eq!(healthy, 300_000);
    assert!(is_due(None, healthy, now), "never run = due");
    assert!(!is_due(Some(now - 299_999), healthy, now));
    assert!(is_due(Some(now - 300_000), healthy, now));
    assert_eq!(next_run_ms(Some(now - 100), healthy, now), now - 100 + 300_000);

    // bybit: 3600s cadence; after 3 failures the interval quadruples-and-doubles but the default
    // 1h backoff cap already binds at the cadence itself.
    let src = &cfg.sources[1];
    assert_eq!(effective_interval_ms(src.cadence_ms(), 0, src.max_backoff_ms()), 3_600_000);
    assert_eq!(
        effective_interval_ms(src.cadence_ms(), 3, src.max_backoff_ms()),
        3_600_000,
        "the default max_backoff_secs cap equals this source's cadence, so backoff cannot grow it"
    );
    assert!(!is_parked(11, src.max_consecutive_failures));
    assert!(is_parked(12, src.max_consecutive_failures));
    // the other two never park (no limit configured)
    assert!(!is_parked(9_999, cfg.sources[0].max_consecutive_failures));
    assert!(!is_parked(9_999, cfg.sources[2].max_consecutive_failures));
}
