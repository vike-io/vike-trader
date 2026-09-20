//! The scheduling DECISION logic, as pure integer functions: given a source's last-run stamp, its
//! cadence, its consecutive-failure count and "now", decide whether to run it, when it next runs,
//! and whether it has failed enough to be parked.
//!
//! Deliberately free of clocks, sleeping, threads and the store — the loop in [`super::run`] is the
//! only thing that reads a real clock, so every branch here is unit-testable with literals.
//!
//! BOUNDED RETRY/BACKOFF. There is no in-pass retry (that would need a sleep inside a pass, holding
//! the loop hostage). Instead a failed source's EFFECTIVE interval grows exponentially from its
//! cadence — `cadence * 2^failures` — capped at `max_backoff_ms`, so a venue that is down gets
//! probed with geometrically decreasing frequency until it recovers, and a recovered source snaps
//! straight back to its cadence (failures reset to 0). [`is_parked`] is the optional hard stop on
//! top of that.

/// The interval actually enforced between two passes over a source: its `cadence_ms` when healthy,
/// or `cadence_ms * 2^consecutive_failures` capped at `max_backoff_ms` after failures.
///
/// - `consecutive_failures == 0` → exactly `cadence_ms` (a healthy source is never slowed down).
/// - The doubling exponent is clamped to 20, so the shift can never overflow no matter how long a
///   venue stays down; the `max_backoff_ms` cap normally binds long before that anyway.
/// - A `max_backoff_ms` below the cadence is raised to the cadence — the backoff may never make a
///   source run MORE often than configured.
/// - A non-positive `cadence_ms` is floored at 1ms rather than treated as "always due" (config
///   validation rejects a zero cadence; this is the defensive twin).
pub fn effective_interval_ms(
    cadence_ms: i64,
    consecutive_failures: u32,
    max_backoff_ms: i64,
) -> i64 {
    let base = cadence_ms.max(1);
    if consecutive_failures == 0 {
        return base;
    }
    let factor = 1_i64 << consecutive_failures.min(20);
    let grown = base.saturating_mul(factor);
    let cap = max_backoff_ms.max(base);
    grown.min(cap)
}

/// Is a source due to run? `None` (never run) is always due — a freshly started supervisor makes a
/// first pass over every source immediately. Otherwise the elapsed time since `last_run_ms` must
/// have reached `interval_ms` (inclusive at the boundary, matching the crate's other
/// at-the-boundary-it-is-done conventions).
///
/// A `last_run_ms` in the FUTURE (the wall clock stepped backwards) reports due rather than wedging
/// the source until the clock catches up.
pub fn is_due(last_run_ms: Option<i64>, interval_ms: i64, now_ms: i64) -> bool {
    match last_run_ms {
        None => true,
        Some(last) if last > now_ms => true,
        Some(last) => now_ms.saturating_sub(last) >= interval_ms.max(1),
    }
}

/// When a source is next expected to run — `now_ms` if it has never run (due immediately), else
/// `last_run_ms + interval_ms`. Purely informational: it is what the status file publishes, and it
/// may be in the past when a pass is overdue (the loop ticks on its own cadence).
pub fn next_run_ms(last_run_ms: Option<i64>, interval_ms: i64, now_ms: i64) -> i64 {
    match last_run_ms {
        None => now_ms,
        Some(last) => last.saturating_add(interval_ms.max(1)),
    }
}

/// Has a source failed enough consecutive passes to be parked (skipped entirely until the
/// supervisor is restarted)? `None` — the default — means never park: retries continue forever at
/// the capped backoff, which is the right behaviour for an always-on collector against a venue that
/// may come back. `Some(0)` is read as "unbounded" too, so a `max_consecutive_failures = 0` typo
/// cannot silently park every source on its first hiccup.
pub fn is_parked(consecutive_failures: u32, max_consecutive_failures: Option<u32>) -> bool {
    match max_consecutive_failures {
        None | Some(0) => false,
        Some(max) => consecutive_failures >= max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_never_run_source_is_due_immediately() {
        assert!(is_due(None, 60_000, 1_700_000_000_000));
        assert!(is_due(None, i64::MAX, 0));
    }

    #[test]
    fn a_source_inside_its_cadence_is_not_due() {
        let last = 1_700_000_000_000_i64;
        assert!(!is_due(Some(last), 60_000, last), "zero elapsed");
        assert!(!is_due(Some(last), 60_000, last + 59_999));
    }

    #[test]
    fn the_cadence_boundary_is_inclusive() {
        let last = 1_700_000_000_000_i64;
        assert!(is_due(Some(last), 60_000, last + 60_000), "exactly at the cadence = due");
        assert!(is_due(Some(last), 60_000, last + 60_001));
    }

    #[test]
    fn a_backwards_clock_step_does_not_wedge_a_source() {
        let last = 1_700_000_000_000_i64;
        assert!(is_due(Some(last), 60_000, last - 10_000_000));
    }

    #[test]
    fn next_run_is_now_for_a_never_run_source_and_last_plus_interval_otherwise() {
        assert_eq!(next_run_ms(None, 60_000, 1_700_000_000_000), 1_700_000_000_000);
        assert_eq!(next_run_ms(Some(1_000), 60_000, 5_000), 61_000);
        // overdue is expressible (a next_run in the past) — the status file reports it as-is.
        assert_eq!(next_run_ms(Some(1_000), 1_000, 900_000), 2_000);
        // saturating, not panicking, on an absurd stamp
        assert_eq!(next_run_ms(Some(i64::MAX), 60_000, 0), i64::MAX);
    }

    #[test]
    fn a_healthy_source_runs_at_exactly_its_cadence() {
        assert_eq!(effective_interval_ms(60_000, 0, 3_600_000), 60_000);
    }

    #[test]
    fn failures_double_the_interval_up_to_the_cap() {
        assert_eq!(effective_interval_ms(60_000, 1, 3_600_000), 120_000);
        assert_eq!(effective_interval_ms(60_000, 2, 3_600_000), 240_000);
        assert_eq!(effective_interval_ms(60_000, 3, 3_600_000), 480_000);
        // 60_000 * 2^6 = 3_840_000 > the 1h cap
        assert_eq!(effective_interval_ms(60_000, 6, 3_600_000), 3_600_000);
        assert_eq!(effective_interval_ms(60_000, 1_000, 3_600_000), 3_600_000, "clamped exponent");
    }

    #[test]
    fn the_backoff_cap_never_makes_a_source_run_more_often_than_its_cadence() {
        // a cap below the cadence is raised to the cadence
        assert_eq!(effective_interval_ms(60_000, 5, 1_000), 60_000);
        assert_eq!(effective_interval_ms(60_000, 5, 0), 60_000);
    }

    #[test]
    fn a_non_positive_cadence_is_floored_not_treated_as_always_due() {
        assert_eq!(effective_interval_ms(0, 0, 3_600_000), 1);
        assert_eq!(effective_interval_ms(-5, 0, 3_600_000), 1);
        assert!(!is_due(Some(100), effective_interval_ms(0, 0, 0), 100));
    }

    #[test]
    fn recovery_snaps_back_to_the_cadence() {
        // the loop resets consecutive_failures to 0 on a clean pass — that must restore the cadence
        // immediately, not decay back to it.
        assert_eq!(effective_interval_ms(60_000, 8, 3_600_000), 3_600_000);
        assert_eq!(effective_interval_ms(60_000, 0, 3_600_000), 60_000);
    }

    #[test]
    fn parking_is_off_by_default_and_off_for_a_zero_limit() {
        assert!(!is_parked(0, None));
        assert!(!is_parked(9_999, None), "unbounded retry is the default");
        assert!(!is_parked(9_999, Some(0)), "0 is read as unbounded, not park-immediately");
    }

    #[test]
    fn parking_engages_at_the_configured_limit() {
        assert!(!is_parked(2, Some(3)));
        assert!(is_parked(3, Some(3)));
        assert!(is_parked(4, Some(3)));
    }
}
