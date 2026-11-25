//! The run: the skip override, the venue order, and the ungated and gated entry points.

use vike_model::NetReading;

use super::checks::{check_credentials, check_disk_headroom, check_network};
use super::clock::check_clock_skew;
use super::config::PreflightConfig;
use super::probes::PreflightProbes;
use super::report::PreflightReport;
#[cfg(doc)]
use super::report::PreflightVenueDisposition;

/// Every venue this run touches, first-seen order: the clock list, then credential-only venues.
/// The report groups per VENUE because the degrade-to-paper decision is read per venue.
fn checked_venues(config: &PreflightConfig) -> Vec<&str> {
    let mut out: Vec<&str> =
        Vec::with_capacity(config.clock_venues.len() + config.credential_venues.len());
    for venue in config.clock_venues.iter().chain(config.credential_venues.iter()) {
        if !out.contains(&venue.as_str()) {
            out.push(venue.as_str());
        }
    }
    out
}

/// Run every configured check and aggregate, in order: network, each `config.dirs` entry, then each
/// venue in [`checked_venues`] order (clock before credentials). No I/O of its own, never panics.
///
/// ⚠ The two venue lists are INDEPENDENT ([`PreflightConfig::clock_venues`]): most clock endpoints
/// are keyless, while the credential leg can only name venues an authed read was built for.
#[must_use]
pub fn run_preflight(
    config: &PreflightConfig,
    probes: &dyn PreflightProbes,
    net: Option<&NetReading>,
) -> PreflightReport {
    let venues = checked_venues(config);
    let mut checks = Vec::with_capacity(1 + config.dirs.len() + venues.len() * 2);
    checks.push(check_network(net, !venues.is_empty()));
    for (label, dir) in &config.dirs {
        checks.push(check_disk_headroom(label, dir, config, probes));
    }
    // ONE deadline per leg on the injected clock: the leg's TOTAL is what is bounded (module doc).
    // Computed only when the leg has work, so a run with no clock venue reads no clock at all.
    let mut clock_deadline = (config.clock_budget_ms > 0 && !config.clock_venues.is_empty())
        .then(|| probes.local_now_ms().saturating_add(config.clock_budget_ms));
    let mut credential_deadline = (config.credential_budget_ms > 0
        && !config.credential_venues.is_empty())
    .then(|| probes.local_now_ms().saturating_add(config.credential_budget_ms));
    for venue in venues {
        if config.clock_venues.iter().any(|v| v == venue) {
            // Each leg's cost is pushed OUT of the other leg's deadline: otherwise one unreachable
            // endpoint eats the other budget and every venue behind it reports "not checked",
            // blaming a healthy leg. Same for the credential arm below.
            let started = credential_deadline.map(|_| probes.local_now_ms());
            checks.push(check_clock_skew(venue, config, probes, clock_deadline));
            if let (Some(t0), Some(deadline)) = (started, credential_deadline) {
                let spent = probes.local_now_ms().saturating_sub(t0).max(0);
                credential_deadline = Some(deadline.saturating_add(spent));
            }
        }
        if config.credential_venues.iter().any(|v| v == venue) {
            // Authed reads, bounded only by `crate::startup`'s per-attempt ceiling. The clock is
            // read only when a deadline exists (a run with no clock leg reads it ZERO times).
            let started = clock_deadline.map(|_| probes.local_now_ms());
            checks.push(check_credentials(venue, probes, credential_deadline));
            if let (Some(t0), Some(deadline)) = (started, clock_deadline) {
                let spent = probes.local_now_ms().saturating_sub(t0).max(0);
                clock_deadline = Some(deadline.saturating_add(spent));
            }
        }
    }
    PreflightReport { checks, skipped: false }
}

/// [`run_preflight`] behind the skip: `skip` is the `flags.preflight_skip` row
/// (`crate::AdapterPolicy::preflight_skip`). Skipped = the EMPTY report (`skipped: true`) WITHOUT
/// calling a probe: every venue stays [`PreflightVenueDisposition::Live`].
#[must_use]
pub fn run_preflight_gated(
    skip: bool,
    config: &PreflightConfig,
    probes: &dyn PreflightProbes,
    net: Option<&NetReading>,
) -> PreflightReport {
    if skip {
        return PreflightReport { checks: Vec::new(), skipped: true };
    }
    run_preflight(config, probes, net)
}
