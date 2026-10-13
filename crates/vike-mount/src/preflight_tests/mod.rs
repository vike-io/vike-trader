//! `preflight`'s unit tests: fixed clock, measured reading and probe builders; a child per check.

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
/// and bybit's stamp, measured from the CI box on 2026-08-08 (rtt 193 ms, +11 ms skew after the
/// midpoint correction). Drifted-HOST tests shift these local samples, not the box's clock.
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

/// `(rtt_ms, skew_ms)` readings consumed in order: the local clock advances `rtt_ms` across
/// each venue read and the venue stamps `midpoint + skew_ms`, so the check MEASURES exactly the
/// scripted skew over the scripted round trip.
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

/// `n` identical `(rtt_ms, skew_ms)` readings: makes "resampling cannot rescue a genuinely bad
/// clock" testable.
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

/// BOTH venue legs over exactly `venues`, defaults elsewhere (a fully-credentialed CEX mount);
/// the lists are independent in general (`the_clock_and_credential_venue_lists_are_independent`).
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

/// Every local-clock read advances `step_ms` (a slow blocking REST read) and the venue stamp
/// lands on the midpoint, so a reading measures ZERO skew and only the leg's BUDGET is tested.
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

#[cfg(test)]
mod aggregate_and_skip;
#[cfg(test)]
mod budgets;
#[cfg(test)]
mod clock_skew;
#[cfg(test)]
mod credentials;
#[cfg(test)]
mod disk_and_network;
