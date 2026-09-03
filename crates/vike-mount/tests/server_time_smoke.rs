//! LIVE smoke for the clock leg: run `vike_mount::server_time`'s REAL table against the REAL
//! venues and print what each one answers. `#[ignore]`d — it makes network calls — and read-only:
//! every request is a public GET (plus IG's one API-key-only GET), no order, no signed private
//! call, no state.
//!
//! It exists because the unit tests prove the DECISION (four outcomes, thresholds, sampling,
//! budget) against injected probes and the PARSE against captured fixtures, and neither can prove
//! that the endpoint still answers at all: an endpoint that moved, or a venue whose clock has
//! wandered out of the band the thresholds were derived against, would sail through every one of
//! them and be discovered at the next live mount — which is precisely how this defect was found in
//! the first place.
//!
//! ```sh
//! cargo test -p vike-mount --test server_time_smoke -- --ignored --nocapture
//! ```
//!
//! Each venue self-skips when its tier's credentials are absent (only IG needs any), so an
//! uncredentialed box runs the keyless rows and reports the rest as skipped rather than failing.

use std::collections::HashMap;
use std::time::Instant;

use vike_mount::preflight::ServerTimeGap;
use vike_mount::server_time::{
    clock_policy, clock_source, ClockAuth, ClockSource, CLOCK_READ_TIMEOUT, CLOCK_SOURCES,
};

/// The venue's stamp minus the midpoint of the local clock either side of the read — the same
/// correction `vike_mount::preflight::check_clock_skew` applies, restated here so the smoke reports
/// the number an operator would see rather than a raw difference.
fn measure(venue: &str, vars: &HashMap<String, String>) -> Result<(i64, i64), ServerTimeGap> {
    let t0 = vike_model::now_ms();
    let server = vike_mount::server_time::venue_server_time_ms(venue, vars)?;
    let t1 = vike_model::now_ms();
    Ok((server - (t0 + t1) / 2, t1 - t0))
}

/// Read every WIRED venue's clock and print `skew ms` / `rtt ms` / what the reading PROVES against
/// that venue's own thresholds; report every DECLARED venue's reason. Fails only if a wired venue
/// answers something unusable — a body we cannot parse, or a stamp that is not a plausible epoch-ms
/// instant (the unit-error trap: a seconds or nanosecond field read as milliseconds).
///
/// It also prints the leg's TOTAL wall cost, which is the number
/// `vike_mount::preflight::DEFAULT_CLOCK_BUDGET_MS` is sized against.
#[test]
#[ignore = "live: makes public network calls to every wired venue"]
fn every_wired_venue_answers_a_plausible_clock() {
    let vars = vike_bridge_core::credentials::load_workspace_secrets_from_env(
        &std::env::vars().collect::<HashMap<String, String>>(),
    );
    let now = vike_model::now_ms();
    // One hour either side: wide enough that a genuinely drifted box still passes (the smoke is
    // proving the PARSE, not the host), narrow enough that a seconds- or nanosecond-valued field
    // read as ms is off by decades and cannot hide.
    let plausible = (now - 3_600_000)..(now + 3_600_000);

    let mut failures: Vec<String> = Vec::new();
    let leg_started = Instant::now();
    for (venue, source) in CLOCK_SOURCES {
        match source {
            ClockSource::Wired { endpoint, auth, risk, .. } => match measure(venue, &vars) {
                Ok((skew, rtt)) => {
                    let stamp = now + skew;
                    // What the reading PROVES: |skew| less the ±rtt/2 measurement floor — the
                    // quantity the check actually judges (see the preflight's module doc).
                    let proven = (skew.abs() - rtt / 2).max(0);
                    let warn = clock_policy(venue).map_or(0, |p| p.warn_ms);
                    println!(
                        "{venue:12} {endpoint:52} skew {skew:>6} ms  rtt {rtt:>4} ms  proven \
                         {proven:>5} ms  warn {warn:>5} ms  {risk:?}"
                    );
                    if !plausible.contains(&stamp) {
                        failures.push(format!(
                            "{venue} answered {stamp} epoch-ms, which is not a plausible instant — \
                             suspect the wrong FIELD or the wrong UNIT"
                        ));
                    }
                }
                Err(ServerTimeGap::Unreachable(e)) => {
                    // A CREDENTIALED row may legitimately skip on a box with no key for it. A
                    // KEYLESS one has no such excuse: from a box with egress, a public endpoint
                    // that does not answer is exactly the ② this whole lane exists to surface, and
                    // a smoke that shrugged at it would pass with the parse broken. The row's own
                    // DECLARED `auth` says which kind it is — reading it off the endpoint LABEL
                    // instead classified deribit ("(public, testnet host)") as credentialed and
                    // swallowed a real failure.
                    if *auth == ClockAuth::Public {
                        failures.push(format!("{venue} publishes a KEYLESS clock and failed: {e}"));
                    } else {
                        println!("{venue:12} SKIPPED (needs a credential this box lacks): {e}");
                    }
                }
                Err(other) => {
                    failures
                        .push(format!("{venue} is Wired but answered a DECLARED gap: {other:?}"));
                }
            },
            ClockSource::NotWired { reason, unmeasured_risk } => {
                match unmeasured_risk {
                    None => println!("{venue:12} declared, no clock leg: {reason}"),
                    Some(at_stake) => {
                        println!("{venue:12} declared, ORDERS AT STAKE: {at_stake} ({reason})")
                    }
                }
                match (measure(venue, &vars), unmeasured_risk) {
                    (Err(ServerTimeGap::NotChecked(_)), None)
                    | (Err(ServerTimeGap::UnmeasuredRisk { .. }), Some(_)) => {}
                    (other, _) => {
                        failures.push(format!("{venue} must answer a DECLARED gap: {other:?}"))
                    }
                }
            }
        }
    }
    println!(
        "\nthe whole clock leg cost {} ms (budget {} ms, per-read ceiling {} ms)",
        leg_started.elapsed().as_millis(),
        vike_mount::preflight::DEFAULT_CLOCK_BUDGET_MS,
        CLOCK_READ_TIMEOUT.as_millis(),
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The tier discipline, live: deribit's own body says WHICH host answered, so this proves the read
/// went to the testnet the exec spawn sites bind rather than to mainnet. No other venue in the
/// table offers a self-identifying field — where one exists it should be asserted.
#[test]
#[ignore = "live: one public GET to test.deribit.com"]
fn deribit_answers_from_the_testnet_host_the_mount_binds() {
    let vars: HashMap<String, String> = HashMap::new();
    let (skew, rtt) = measure("deribit", &vars).expect("deribit's public get_time answers");
    println!("deribit testnet: skew {skew} ms, rtt {rtt} ms (body asserted testnet=true)");
    assert!(matches!(clock_source("deribit"), Some(ClockSource::Wired { .. })));
}

/// THE BOUND, measured rather than asserted: a clock read against a BLACKHOLE returns inside
/// [`CLOCK_READ_TIMEOUT`], where the shared 30 s agent every other venue call uses does not.
///
/// `203.0.113.1` is RFC 5737 TEST-NET-3 — reserved for documentation, routed nowhere, so the
/// connect hangs rather than being refused (verified from the CI box: `curl -m 12` times out at 12.01 s
/// against it). That is the shape a wedged venue has, and it is what made the unbounded leg worth
/// `venues × samples × 30 s`. Costs ~33 s, hence `#[ignore]`.
#[test]
#[ignore = "live: dials a blackhole twice and waits out both timeouts (~33 s)"]
fn a_clock_read_against_a_blackhole_returns_inside_the_bound() {
    use vike_bridge_core::transport::{RestTransport, UreqTransport};
    const BLACKHOLE: &str = "https://203.0.113.1";

    let bounded = UreqTransport::with_agent(
        "blackhole",
        vike_bridge_core::http::blocking_agent_with_timeout(CLOCK_READ_TIMEOUT),
    );
    let t = Instant::now();
    let r = bounded.public(BLACKHOLE, "/api/v3/time", &[]);
    let bounded_ms = t.elapsed();
    println!("bounded  ({CLOCK_READ_TIMEOUT:?}): {} ms, {r:?}", bounded_ms.as_millis());
    assert!(r.is_err(), "a blackhole cannot answer");
    assert!(
        bounded_ms < CLOCK_READ_TIMEOUT * 2,
        "a bounded clock read must return near its ceiling, took {bounded_ms:?}"
    );

    let shared = UreqTransport::new("blackhole");
    let t = Instant::now();
    let _ = shared.public(BLACKHOLE, "/api/v3/time", &[]);
    let shared_ms = t.elapsed();
    println!("shared agent (the 30 s default): {} ms", shared_ms.as_millis());
    assert!(
        shared_ms > bounded_ms * 3,
        "…and the shared agent is the thing being bounded: {shared_ms:?} vs {bounded_ms:?}"
    );
}
