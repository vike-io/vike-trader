//! The clock-leg tests that replay MEASURED readings under each venue's REAL declared clock policy,
//! read through the real registry. They lived in `crates/vike-mount/src/preflight_tests/mod.rs` until
//! the venue mount contract finished (docs/decisions/0096): each venue's clock row is its bridge's
//! declaration now. The probe helpers below are copies of that file's, which its staying tests
//! still use.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use vike_mount::preflight::{
    CANARY_CLOCK_WARN_MS, CheckStatus, ClockPolicy, ClockSample, CredentialGap,
    DEFAULT_CLOCK_SAMPLES, DEFAULT_CLOCK_WARN_MS, DEFAULT_DISK_WARN_BYTES, FnProbes,
    PreflightConfig, check_clock_skew, clock_budget_for,
};
use vike_tradehub::registry::REGISTRY;

/// A fixed local clock so skew arithmetic is exact.
const NOW: i64 = 1_700_000_000_000;

/// All four legs wired healthy: zero skew, accepted credentials, ample free space.
fn healthy() -> FnProbes {
    FnProbes::new()
        .with_now_ms(|| NOW)
        .with_venue_server_time_ms(|_: &str| Ok(NOW))
        .with_venue_authed_read(|_: &str| Ok::<(), CredentialGap>(()))
        .with_free_space_bytes(|_: &Path| Ok(DEFAULT_DISK_WARN_BYTES))
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

/// `n` identical readings of `(rtt_ms, skew_ms)` — a venue whose behaviour does not change
/// between samples, which is what makes "resampling cannot rescue a genuinely bad clock"
/// testable.
fn repeated_sample(n: usize, rtt: i64, skew: i64) -> FnProbes {
    let script: Vec<(i64, i64)> = std::iter::repeat_n((rtt, skew), n).collect();
    scripted_samples(&script)
}

/// Defaults, plus ONE venue's declared clock policy.
fn cfg_with_policy(venue: &str, policy: ClockPolicy) -> PreflightConfig {
    let mut clock_policies = HashMap::new();
    clock_policies.insert(venue.to_string(), policy);
    PreflightConfig { clock_policies, ..PreflightConfig::default() }
}

/// EVERY reading taken on a healthy, NTP-disciplined box, as the `(venue, skew_ms, rtt_ms)`
/// PAIRS they were measured as — never a bare magnitude, because the round trip is half of what
/// a reading means (module doc, "a reading is only as sharp as its round trip").
///
/// the CI box (`timedatectl`: "System clock synchronized: yes"), 2026-08-09, reproducible with
/// `crates/vike-tradehub/tests/server_time_smoke.rs`. Each row is a real curl-and-`date` sample,
/// midpoint-corrected exactly as [`check_clock_skew`] corrects. The hyperliquid rows are the
/// EXTREMES of a 40-sample soak of its testnet node inside three minutes, plus its mainnet
/// node's own range; the two ig rows are the CI box (2026-08-08) and the Windows dev box.
const MEASURED_HEALTHY_READINGS: &[(&str, i64, i64)] = &[
    ("binance", 236, 723),
    ("binance", 247, 757),
    ("bybit", 9, 200),
    ("bybit", 23, 221),
    ("okx", 10, 307),
    ("okx", 37, 335),
    ("aster", 15, 269),
    ("aster", 33, 320),
    ("deribit", 15, 55),
    ("deribit", 25, 75),
    ("hyperliquid", -220, 296),
    ("hyperliquid", -424, 282),
    ("hyperliquid", -59, 493),
    ("hyperliquid", -221, 283),
    ("ig", 24, 94),
    ("ig", 160, 444),
    // The worst RTT-asymmetry ARTIFACT ever seen here: bybit demo read 182 ms of apparent skew
    // over a 549 ms round trip while every other rep on the same host read 13-23 ms
    // (2026-08-08). It is path asymmetry surviving the midpoint correction, not clock error.
    ("bybit", 182, 549),
];

/// THE derivation, and it is a MEASUREMENT: every reading above, replayed through the real
/// check under the venue's REAL declared policy, must PASS. A threshold that warns on a healthy
/// box is the false-alarm twin of the silent degrade this lane exists to fix.
///
/// ⚠ This replaced a pin of the same intent that was ALREADY FALSE when it shipped — see
/// `the_replaced_global_derivation_is_falsified_by_its_own_measurement` below.
#[test]
fn every_measured_healthy_reading_passes_under_its_venues_real_policy() {
    for &(venue, skew, rtt) in MEASURED_HEALTHY_READINGS {
        let policy = vike_mount::server_time::clock_policy(REGISTRY, venue)
            .unwrap_or_else(|| panic!("{venue} is measured here, so its clock must be wired"));
        let cfg = cfg_with_policy(venue, policy);
        // Every sample repeats the reading, so a resample cannot rescue (or degrade) it: the
        // verdict is the one this measurement produces however many times it is looked at.
        let probes = repeated_sample(DEFAULT_CLOCK_SAMPLES, rtt, skew);
        let r = check_clock_skew(venue, &cfg, &probes, None);
        assert_eq!(
            r.status,
            CheckStatus::Pass,
            "{venue} MEASURED {skew} ms over a {rtt} ms round trip on a disciplined host, and \
                 this threshold fires on it: {}",
            r.message
        );
        // …and with margin: what the reading PROVES is at most half the venue's warn floor, so
        // an ordinary bad minute cannot cross it either. (hyperliquid's -424/282 is the
        // tightest row: it proves 283 ms against a 1000 ms floor.)
        let proven = ClockSample { skew_ms: skew, rtt_ms: rtt }.proven_magnitude();
        assert!(
            proven * 2 <= policy.warn_ms,
            "{venue}'s {skew}/{rtt} reading proves {proven} ms against a {} ms floor — under \
                 half the margin this check needs to stay quiet on a healthy box",
            policy.warn_ms
        );
    }
}

/// The falsification, kept as a test because it is the ARGUMENT for the per-venue split.
///
/// The shipped derivation pinned `WORST_HEALTHY_SKEW_MS = 288` and const-asserted that the
/// global 500 ms warn threshold clears it by half again. Re-running that same measurement
/// produced -424 ms from an NTP-disciplined box: the assert is false against it, and the point
/// estimate sits 76 ms from warning. Hence hyperliquid is judged against
/// [`CANARY_CLOCK_WARN_MS`] with no FAIL at all, rather than the recv-window pair.
#[test]
fn the_replaced_global_derivation_is_falsified_by_its_own_measurement() {
    const PREVIOUSLY_PINNED_WORST_MS: i64 = 288;
    let worst = MEASURED_HEALTHY_READINGS
        .iter()
        .map(|&(_, skew, _)| skew.abs())
        .max()
        .expect("the table is not empty");
    assert!(
        worst > PREVIOUSLY_PINNED_WORST_MS,
        "the pinned worst-healthy reading was stale the next time it was measured"
    );
    assert!(
        DEFAULT_CLOCK_WARN_MS <= worst + worst / 2,
        "…and the half-again margin the old pin const-asserted does not hold against {worst} ms"
    );
    assert_eq!(
        DEFAULT_CLOCK_WARN_MS - worst,
        76,
        "the global threshold sits this many ms from warning on a HEALTHY box"
    );
    let hl = vike_mount::server_time::clock_policy(REGISTRY, "hyperliquid").expect("wired");
    assert_eq!(hl.warn_ms, CANARY_CLOCK_WARN_MS, "so it is not judged by that threshold");
    assert_eq!(hl.fail_ms, None, "and it can never be degraded to paper by this leg");
}

/// THE REQUIREMENT, stated as arithmetic over this module's own pinned measurements: the clock
/// leg's budget must absorb **one timing-out venue and still read every other one**. A venue
/// that answers nothing costs a full `CLOCK_READ_TIMEOUT`, and one venue being unreachable is
/// an ordinary Tuesday — not the pathological case the budget exists to bound.
///
/// It failed that at the fixed 5000 ms. Measured on the Windows dev box 2026-08-22: bybit's
/// endpoint stopped answering, its read spent 3 s of the 5 s, and EIGHT venues behind it
/// reported "not read" — including aster, which runs against mainnet in practice and whose auth
/// binds the clock into the order path. Nothing was broken; the budget was simply smaller than
/// the roster it had to cover, having been sized (module doc) against a SIX-read 1781 ms
/// measurement and never revisited as venues were added.
#[test]
fn the_budget_absorbs_one_dead_venue_and_still_reads_the_rest() {
    // Each wired venue's WORST pinned healthy round trip — what a good pass actually costs.
    let mut healthy_leg_ms = 0i64;
    let mut wired = 0usize;
    for venue in vike_model::VENUES {
        let Some(decl) = vike_mount::server_time::clock_decl(REGISTRY, venue) else { continue };
        if !matches!(decl, vike_bridge_core::venue_mount::ClockDecl::Wired { .. }) {
            continue;
        }
        wired += 1;
        let worst = MEASURED_HEALTHY_READINGS
            .iter()
            .filter(|(v, _, _)| v == venue)
            .map(|(_, _, rtt)| *rtt)
            .max();
        // A wired venue with no pinned reading contributes its ceiling rather than nothing —
        // an unmeasured venue is not a free one.
        healthy_leg_ms +=
            worst.unwrap_or(vike_mount::server_time::CLOCK_READ_TIMEOUT.as_millis() as i64);
    }
    let dead_venue_ms = vike_mount::server_time::CLOCK_READ_TIMEOUT.as_millis() as i64;
    let required = healthy_leg_ms + dead_venue_ms;
    let budget = clock_budget_for(wired);
    assert!(
        budget >= required,
        "{wired} wired venues cost {healthy_leg_ms} ms on a healthy pass; one dead venue adds \
             {dead_venue_ms} ms, so the leg needs >= {required} ms and the budget is {budget} ms. \
             Raise PER_VENUE_CLOCK_ALLOWANCE_MS — do not special-case a venue."
    );
}
