//! The preflight run: the disk leg, the credential withhold, and `run_startup_preflight` — the
//! orchestrator that asks a [`VenueBackend`] for the venue legs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::preflight::{
    DiskThresholds, FnProbes, PreflightConfig, PreflightReport, credential_budget_for,
    preflight_skipped, run_preflight_gated,
};
use crate::{ArmingTable, PreflightPlan, PreflightQuery, VenueBackend, venue_would_go_live};

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

/// ENFORCE a preflight stop (and a `data_only` declaration): the directory with every credential of
/// `venue` removed, so no exec client of that venue can be built from it. Returns the directory and
/// the count of credential fields withheld.
///
/// ⚠ **Absent credentials ARE the live gate** (root `CLAUDE.md`), so this keeps credentials away from
/// every arm whatever else decides; the REASON a preflight-failed venue's accounts do not run is
/// carried separately (`crate::AdapterPolicy::preflight_failed`, so they read `PreflightFailed`
/// rather than `NoCredentials`). The `ReconClient` factory sits behind the same credentials, so a
/// stopped venue reconciles nothing.
///
/// The credentials are the ACCOUNTS' (decision 0156 item 6): every account row of the venue, and the
/// venue's own application fields, so a field filed against either goes. The rows stay — the tier is
/// the operator's word — and over-withholding only stops MORE.
///
/// ⚠ **The `data_only` declaration is the second caller, of this SAME body**:
/// `crates/vike-tradehub/src/tradehub_cli/live_runtime.rs`'s `live_runtime_with`, AFTER every feed
/// plan resolved (each plan CARRIES its config), so the feed keeps the keys exec loses.
#[must_use]
pub fn withhold_venue_credentials(
    accounts: &vike_secrets::account_directory::AccountDirectory,
    venue: &str,
) -> (vike_secrets::account_directory::AccountDirectory, usize) {
    accounts.without_venue_credentials(venue)
}

/// Whether ANY roster venue would go live **at its DEFAULT account's tier** — the pure,
/// network-free live-INTENT probe `make_engine`'s pre-connect refusal uses, read off the arming
/// table. Gates whether the network probe is read at all.
///
/// ⚠ Tier-aware although the network probe touches NO venue (it resolves the probe's default
/// hosts), for two reasons: the leg's verdict reads *"no venue would go live, so no order path
/// depends on connectivity"* (`crate::preflight::check_network`), TRUE on an all-paper box — a
/// tier-blind gate would have the row assert a live order path the start does not have; and a
/// second "is anything armed" predicate is exactly the drift to avoid. Public for the roster tests
/// in `crates/vike-gatewayhub/tests/trade_roster/mount_roster.rs` (docs/decisions/0096).
pub fn any_venue_would_go_live(table: &ArmingTable, policy: Option<&crate::AdapterPolicy>) -> bool {
    vike_model::VENUES.iter().any(|venue| venue_would_go_live(table, venue, policy))
}

/// The disk leg's floors for this start: the `config.preflight_disk_*_mib` rows the root put on the
/// policy ([`crate::AdapterPolicy::preflight_disk`]), else the compiled pair — the same floors a
/// caller that threads no policy has always got.
pub(super) fn disk_floors(policy: Option<&crate::AdapterPolicy>) -> DiskThresholds {
    policy.map_or_else(DiskThresholds::default, |p| p.preflight_disk)
}

/// Run the startup preflight with the REAL probes. `vars` is the credentials map with the root's
/// resolved flags folded in — the SKIP flag included, which is read from `vars` alone
/// ([`crate::preflight::preflight_skipped`]): decision 0111 retired `VIKE_PREFLIGHT_SKIP`, so the
/// process environment is no source. Never panics, never blocks a start.
///
/// ⚠ `dirs` is a PARAMETER so the disk leg measures what the start WRITES: the journal dir is
/// decided above (the `config.journal_dir` row into `CoreConfig::journal`), and resolving it here
/// would be a second authority. Empty = no disk leg (paper/CI, offline).
///
/// ⚠ `policy` carries each account's TIER (its `account` table) and `table` is the arming snapshot
/// of the same assembly. **`None` reads all-`paper`** (the same `vike_config::account_arming`'s
/// `account_tier` [`crate::make_engine`] uses), so a caller that threads no policy contacts NOTHING.
/// `vike_tradehub_core::assemble_runtime` passes `Some(&cfg.policy)`, the one production site.
///
/// The venue legs are the `backend`'s: its plan names the credential venues and the clock
/// policies, and its reads answer each probe. The closures own an `Arc` of it because the probe
/// seam is `'static` (a read may be abandoned and outlive this call).
pub fn run_startup_preflight(
    backend: &Arc<dyn VenueBackend>,
    table: &ArmingTable,
    vars: &HashMap<String, String>,
    dirs: &[(String, PathBuf)],
    policy: Option<&crate::AdapterPolicy>,
) -> PreflightReport {
    // The skip is the `flags.preflight_skip` row the composition root FOLDED into `vars`
    // (overwriting any credential-store line of the same name). Short-circuit BEFORE any probe is
    // built or thread spawned.
    if preflight_skipped(vars) {
        return run_preflight_gated(vars, &PreflightConfig::default(), &FnProbes::new(), None);
    }

    // The CLOCK leg's list is every venue about to start live (a different question from the
    // credential leg's, which is client-buildability — the backend's plan).
    let clock_venues: Vec<String> = vike_model::VENUES
        .iter()
        .filter(|venue| venue_would_go_live(table, venue, policy))
        .map(|venue| (*venue).to_string())
        .collect();
    let plan = backend
        .preflight_plan(&PreflightQuery { clock_venues: &clock_venues })
        .unwrap_or_else(|why| {
            tracing::error!(
                "the startup preflight could not ask the venue backend for its legs ({why}): \
                 no credential leg and no clock policy this start"
            );
            PreflightPlan::default()
        });
    // The budget is DERIVED from how many venues are actually READ (only wired ones buy time), not
    // `PreflightConfig::default()`'s floor: the fixed 5000 ms, sized against a six-read
    // measurement, let one unreachable venue leave eight of ten unread.
    let wired = plan.wired_clock_reads;
    let probes = FnProbes::new()
        .with_venue_server_time_ms({
            // ⚠ `Arc`: the probe closure must be `'static` (the start may abandon a read), so the
            // backend and the policy snapshot are shared by refcount, not borrowed.
            let backend = Arc::clone(backend);
            let policy = Arc::new(policy.cloned());
            move |venue: &str| {
                // Only a venue whose default account would start a venue client is clock-probed
                // (`clock_venues`), so its row states a tier; one that states none is not asked.
                let Some(tier) = vike_config::account_arming::venue_tier(
                    policy.as_ref().as_ref().map(|p| &p.accounts),
                    venue,
                ) else {
                    return Err(crate::preflight::ServerTimeGap::NotChecked(
                        "the account does not run: no account row states its tier",
                    ));
                };
                backend.server_time_ms(venue, tier)
            }
        })
        .with_venue_authed_read({
            let backend = Arc::clone(backend);
            move |venue: &str| backend.authed_read(venue)
        })
        .with_free_space_bytes(free_space_bytes);
    let disk = disk_floors(policy);
    let cfg = PreflightConfig {
        // DERIVED like the clock budget: a fixed one silently drops tomorrow's last venues.
        credential_budget_ms: credential_budget_for(plan.credential_venues.len()),
        clock_venues,
        credential_venues: plan.credential_venues,
        clock_policies: plan.clock_policies,
        clock_budget_ms: crate::preflight::clock_budget_for(wired),
        dirs: disk_dirs(dirs),
        // The two `config.preflight_disk_*_mib` rows, else the compiled floors (`disk_floors`).
        disk_warn_bytes: disk.warn_bytes,
        disk_fail_bytes: disk.fail_bytes,
        ..PreflightConfig::default()
    };

    // The network probe is read only when a venue would go live; a pure-paper start stays offline.
    let net = any_venue_would_go_live(table, policy).then(|| backend.net_reading());
    run_preflight_gated(vars, &cfg, &probes, net.as_ref())
}
