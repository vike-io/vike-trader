//! The startup preflight's scripted probes, ONE copy for this crate's preflight unit tests
//! (`src/preflight_tests/`) and the gatewayhub's roster preflight suite
//! (`crates/vike-gatewayhub/tests/trade_roster/mount_roster/preflight.rs`), which each carried their own
//! until 2026-10-31.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::preflight::{
    ClockPolicy, CredentialGap, DEFAULT_DISK_WARN_BYTES, FnProbes, PreflightConfig,
};

/// A fixed local clock, so skew arithmetic is exact.
pub const NOW: i64 = 1_700_000_000_000;

/// All four legs wired healthy: zero skew, accepted credentials, ample free space.
pub fn healthy() -> FnProbes {
    FnProbes::new()
        .with_now_ms(|| NOW)
        .with_venue_server_time_ms(|_: &str| Ok(NOW))
        .with_venue_authed_read(|_: &str| Ok::<(), CredentialGap>(()))
        .with_free_space_bytes(|_: &Path| Ok(DEFAULT_DISK_WARN_BYTES))
}

/// A probe scripted with `(rtt_ms, skew_ms)` readings, consumed in order: the local clock advances
/// `rtt_ms` across each venue read, and the venue stamps `midpoint + skew_ms`, so the check MEASURES
/// exactly the scripted skew over exactly the scripted round trip.
pub fn scripted_samples(script: &[(i64, i64)]) -> FnProbes {
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

/// `n` identical readings of `(rtt_ms, skew_ms)`: a venue whose behaviour does not change between
/// samples, which is what makes "resampling cannot rescue a genuinely bad clock" testable.
pub fn repeated_sample(n: usize, rtt: i64, skew: i64) -> FnProbes {
    let script: Vec<(i64, i64)> = std::iter::repeat_n((rtt, skew), n).collect();
    scripted_samples(&script)
}

/// Defaults, plus ONE venue's declared clock policy.
pub fn cfg_with_policy(venue: &str, policy: ClockPolicy) -> PreflightConfig {
    let mut clock_policies = std::collections::HashMap::new();
    clock_policies.insert(venue.to_string(), policy);
    PreflightConfig { clock_policies, ..PreflightConfig::default() }
}
