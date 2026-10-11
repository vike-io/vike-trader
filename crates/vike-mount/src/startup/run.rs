//! The preflight run: the disk and network legs, the credential withhold, `run_startup_preflight`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_bridge_core::{NetProbe, NetProbeThread};

use crate::preflight::{
    FnProbes, PreflightConfig, PreflightReport, credential_budget_for, preflight_skipped,
    run_preflight_gated,
};

use super::clock::{bounded_server_time_ms, clock_policies, clock_venues};
use super::credentials::{authed_read_probe, authed_read_probes};
use super::{DEFAULT_NET_PROBE_WAIT, NET_PROBE_POLL};

/// Free bytes on the filesystem holding `dir` — the disk leg.
///
/// ⚠ On unix this is `rustix::fs::statvfs`, a SAFE wrapper in a crate `Cargo.lock` already held:
/// no `unsafe`, no `UNSAFE_EXEMPT` carve-out in an order-signing binary, no new package.
/// `f_bavail × f_frsize` is the NON-PRIVILEGED free space — not `f_bfree`, whose root-reserved
/// blocks a normal-user daemon can never write into.
///
/// On Windows (`GetDiskFreeSpaceExW` has no dependency-free safe route) it returns a DECLARED
/// reason and the leg WARNs with it; both shipped daemons run on Linux.
#[cfg(unix)]
pub fn free_space_bytes(dir: &Path) -> Result<u64, String> {
    let stat = rustix::fs::statvfs(dir).map_err(|e| format!("statvfs failed: {e}"))?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// The Windows half of [`free_space_bytes`]: declares rather than measures (see the unix half).
#[cfg(not(unix))]
pub fn free_space_bytes(_dir: &Path) -> Result<u64, String> {
    Err("free space is not queried on this platform (rustix's statvfs is POSIX-only, and the \
         Windows call has no dependency-free safe route); the leg is armed on unix, where both \
         shipped daemons run"
        .to_string())
}

/// Deduplicate the watched directories, preserving order: two labels can resolve to ONE path (a
/// journal inside the store root), and two rows would double every finding.
pub(super) fn disk_dirs(dirs: &[(String, PathBuf)]) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::with_capacity(dirs.len());
    for (label, path) in dirs {
        if !out.iter().any(|(_, p)| p == path) {
            out.push((label.clone(), path.clone()));
        }
    }
    out
}

/// Spawn the [`NetProbe`] and wait at most `wait` for its first round, so the leg reads a
/// MEASUREMENT, not the handle's optimistic initial value. Keep the [`NetProbeThread`] alive while
/// its handle is read; dropping it stops WITHOUT joining (a wedged DNS resolve never parks us).
pub fn spawn_net_probe(wait: Duration) -> NetProbeThread {
    let thread = NetProbe::with_defaults().spawn();
    let handle = thread.handle();
    let deadline = Instant::now() + wait;
    while !handle.has_probed() && Instant::now() < deadline {
        std::thread::sleep(NET_PROBE_POLL);
    }
    thread
}

/// ENFORCE a preflight demotion: remove every `{VENUE}_`-prefixed key from the credential map, so
/// `make_engine` sees absent credentials and mounts the paper fallback. Returns the count withheld.
///
/// ⚠ NOT a "force paper" flag: **absent credentials ARE the live gate** (root `CLAUDE.md`), so this
/// reuses the credential-less path instead of a parallel switch that could disagree. The
/// `ReconClient` factory sits behind the same keys, so a demoted venue reconciles nothing (a live
/// account reconciled against a paper engine is how `PositionDrift` imports live positions).
///
/// The PREFIX, not a key list: every venue key family is `{VENUE}_`-spelled, so a future spelling
/// is withheld the day it exists and over-withholding only makes the mount MORE paper.
/// `vike_model::credential_keys`' grid is NOT enough: alpaca (`ALPACA_SANDBOX_CLIENT_ID`), ctrader
/// (`CTRADER_DEMO_ACCESS_TOKEN`) and ig/oanda — the venues this leg probes — are bespoke.
///
/// ⚠ **The `data_only` declaration is the second caller, of this SAME body**:
/// `crates/vike-tradehub/src/tradehub_cli/live_mount.rs`'s `live_mount_with`, AFTER every feed
/// plan resolved (each plan CARRIES its config), so the feed keeps the keys exec loses. No
/// `data_only`-eligible venue has a `{VENUE}_MAINNET` flag or attribution code to over-strip
/// (`crates/vike-tradehub/src/venue_arming.rs`;
/// `vike_model::venues::attribution::attribution_for`).
pub fn withhold_venue_credentials(vars: &mut HashMap<String, String>, venue: &str) -> usize {
    let prefix = format!("{}_", venue.to_uppercase());
    let withheld: Vec<String> = vars.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
    for key in &withheld {
        vars.remove(key);
    }
    withheld.len()
}

/// Whether ANY roster venue would mount live from `vars` **at its DEFAULT account's tier** — the
/// pure, network-free live-INTENT probe `make_engine`'s pre-connect refusal uses.
/// Gates whether a [`NetProbe`] is spawned at all.
///
/// ⚠ Tier-aware although the [`NetProbe`] touches NO venue (it resolves
/// `vike_bridge_core::net_probe::DEFAULT_PROBE_HOSTS`), for two reasons: the leg's verdict reads
/// *"no venue would mount live, so no order path depends on connectivity"*
/// (`crate::preflight::check_network`), TRUE on an all-paper box — a tier-blind gate would have the
/// row assert a live order path the mount does not have; and a second "is anything armed"
/// predicate is exactly the drift to avoid. Public for the roster tests in
/// `crates/vike-tradehub/tests/mount_roster.rs` (docs/decisions/0096).
pub fn any_venue_would_mount_live(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> bool {
    vike_model::VENUES
        .iter()
        .any(|venue| crate::would_mount_live_under_policy(registry, venue, vars, policy))
}

/// Run the startup preflight with the REAL probes. `vars` is the credentials map `make_engine`
/// gates on, with the root's resolved flags folded in — the SKIP flag included, which is read from
/// `vars` alone ([`crate::preflight::preflight_skipped`]): decision 0111 retired
/// `VIKE_PREFLIGHT_SKIP`, so the process environment is no source. Never panics, never blocks a
/// mount.
///
/// ⚠ `dirs` is a PARAMETER so the disk leg measures what the mount WRITES: the journal dir is
/// decided above (`[sinks.journal]` / `config.journal_dir` into `CoreConfig::journal`), and resolving
/// it here would be a second authority. Empty = no disk leg (paper/CI, offline).
///
/// ⚠ `policy` carries each account's TIER (its `account` table). **`None` reads all-`paper`** (the
/// same `crate::arming`'s `account_tier` [`crate::make_engine`] uses), so a caller that threads no
/// policy contacts NOTHING. `vike_mount::build_node` passes `Some(&cfg.policy)`, the one
/// production site.
pub fn run_startup_preflight(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    dirs: &[(String, PathBuf)],
    policy: Option<&crate::MountPolicy>,
) -> PreflightReport {
    // The skip is the `flags.preflight_skip` row the composition root FOLDED into `vars`
    // (overwriting any credential-store line of the same name). Short-circuit BEFORE any probe is
    // built or thread spawned.
    if preflight_skipped(vars) {
        return run_preflight_gated(vars, &PreflightConfig::default(), &FnProbes::new(), None);
    }

    let probes_by_venue = authed_read_probes(registry, vars, policy);
    // The CREDENTIAL leg's list IS the authed-readable set (the module doc's INVARIANT)…
    let mut credential_venues: Vec<String> = probes_by_venue.keys().cloned().collect();
    credential_venues.sort();
    // …and the CLOCK leg's is every venue about to mount live, a different question.
    let clock_venues = clock_venues(registry, vars, policy);
    let clock_policies = clock_policies(registry, &clock_venues);
    // The budget is DERIVED from how many venues are actually READ (only wired ones buy time), not
    // `PreflightConfig::default()`'s floor: the fixed 5000 ms, sized against a six-read
    // measurement, let one unreachable venue leave eight of ten unread.
    let wired = clock_venues
        .iter()
        .filter(|v| {
            matches!(
                crate::server_time::clock_decl(registry, v),
                Some(vike_bridge_core::venue_mount::ClockDecl::Wired { .. })
            )
        })
        .count();
    let probes = FnProbes::new()
        .with_venue_server_time_ms({
            // ⚠ `Arc`: `bounded_server_time_ms` builds a probe closure per read that must OWN its
            // captures (the mount may abandon it), so `vars` and `policy` are shared by refcount,
            // not copied per venue per sample.
            let vars = Arc::new(vars.clone());
            let policy = Arc::new(policy.cloned());
            move |venue: &str| {
                let live =
                    crate::tier_permits_live(crate::venue_tier(policy.as_ref().as_ref(), venue));
                bounded_server_time_ms(registry, venue, &vars, live, &policy)
            }
        })
        .with_venue_authed_read(authed_read_probe(probes_by_venue))
        .with_free_space_bytes(free_space_bytes);
    let cfg = PreflightConfig {
        // DERIVED like the clock budget: a fixed one silently drops tomorrow's last venues.
        credential_budget_ms: credential_budget_for(credential_venues.len()),
        clock_venues,
        credential_venues,
        clock_policies,
        clock_budget_ms: crate::preflight::clock_budget_for(wired),
        dirs: disk_dirs(dirs),
        ..PreflightConfig::default()
    };

    let net = any_venue_would_mount_live(registry, vars, policy)
        .then(|| spawn_net_probe(DEFAULT_NET_PROBE_WAIT));
    let handle = net.as_ref().map(NetProbeThread::handle);
    let report = run_preflight_gated(vars, &cfg, &probes, handle.as_ref());
    // Signals stop without joining — see `NetProbeThread`'s `Drop` doc.
    drop(net);
    report
}
