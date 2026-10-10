//! The run: the skip override, the venue order, and the ungated and gated entry points.

use std::collections::HashMap;

use vike_bridge_core::NetProbeHandle;

use super::checks::{check_credentials, check_disk_headroom, check_network};
use super::clock::check_clock_skew;
use super::config::{PREFLIGHT_SKIP_ENV, PreflightConfig};
use super::probes::PreflightProbes;
use super::report::PreflightReport;
#[cfg(doc)]
use super::report::VenueDisposition;

/// [`PREFLIGHT_SKIP_ENV`] -> skip the whole preflight: `true` iff the EXACT string `"1"` (`"true"`,
/// `"yes"`, `"0"`, … run it), the unfuzzy idiom of
/// `vike_tradehub::reconcile_config::reconcile_enabled`. `vars` must be the REAL process env
/// (`std::env::vars()`), not the credentials `.env` map.
#[must_use]
pub fn preflight_skipped(vars: &HashMap<String, String>) -> bool {
    vars.get(PREFLIGHT_SKIP_ENV).map(String::as_str) == Some("1")
}

/// Every venue this run touches, first-seen order: the clock list, then credential-only venues.
/// The report groups per VENUE because the degrade-to-paper decision is read per venue.
fn checked_venues(cfg: &PreflightConfig) -> Vec<&str> {
    let mut out: Vec<&str> =
        Vec::with_capacity(cfg.clock_venues.len() + cfg.credential_venues.len());
    for venue in cfg.clock_venues.iter().chain(cfg.credential_venues.iter()) {
        if !out.contains(&venue.as_str()) {
            out.push(venue.as_str());
        }
    }
    out
}

/// Run every configured check and aggregate, in order: network, each `cfg.dirs` entry, then each
/// venue in [`checked_venues`] order (clock before credentials). No I/O of its own, never panics.
///
/// ⚠ The two venue lists are INDEPENDENT ([`PreflightConfig::clock_venues`]): most clock endpoints
/// are keyless, while the credential leg can only name venues an authed read was built for.
#[must_use]
pub fn run_preflight(
    cfg: &PreflightConfig,
    probes: &dyn PreflightProbes,
    net: Option<&NetProbeHandle>,
) -> PreflightReport {
    let venues = checked_venues(cfg);
    let mut checks = Vec::with_capacity(1 + cfg.dirs.len() + venues.len() * 2);
    checks.push(check_network(net, !venues.is_empty()));
    for (label, dir) in &cfg.dirs {
        checks.push(check_disk_headroom(label, dir, cfg, probes));
    }
    // ONE deadline per leg on the injected clock: the leg's TOTAL is what is bounded (module doc).
    // Computed only when the leg has work, so a run with no clock venue reads no clock at all.
    let mut clock_deadline = (cfg.clock_budget_ms > 0 && !cfg.clock_venues.is_empty())
        .then(|| probes.local_now_ms().saturating_add(cfg.clock_budget_ms));
    let mut credential_deadline = (cfg.credential_budget_ms > 0
        && !cfg.credential_venues.is_empty())
    .then(|| probes.local_now_ms().saturating_add(cfg.credential_budget_ms));
    for venue in venues {
        if cfg.clock_venues.iter().any(|v| v == venue) {
            // Each leg's cost is pushed OUT of the other leg's deadline: otherwise one unreachable
            // endpoint eats the other budget and every venue behind it reports "not checked",
            // blaming a healthy leg. Same for the credential arm below.
            let started = credential_deadline.map(|_| probes.local_now_ms());
            checks.push(check_clock_skew(venue, cfg, probes, clock_deadline));
            if let (Some(t0), Some(deadline)) = (started, credential_deadline) {
                let spent = probes.local_now_ms().saturating_sub(t0).max(0);
                credential_deadline = Some(deadline.saturating_add(spent));
            }
        }
        if cfg.credential_venues.iter().any(|v| v == venue) {
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

/// [`run_preflight`] behind the [`PREFLIGHT_SKIP_ENV`] gate. Skipped = the EMPTY report
/// (`skipped: true`) WITHOUT calling a probe: every venue stays [`VenueDisposition::Live`].
#[must_use]
pub fn run_preflight_gated(
    vars: &HashMap<String, String>,
    cfg: &PreflightConfig,
    probes: &dyn PreflightProbes,
    net: Option<&NetProbeHandle>,
) -> PreflightReport {
    if preflight_skipped(vars) {
        return PreflightReport { checks: Vec::new(), skipped: true };
    }
    run_preflight(cfg, probes, net)
}
