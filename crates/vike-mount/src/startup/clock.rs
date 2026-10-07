//! The clock leg: the venue clock read, its bounded twin, and the clock venue list and thresholds.

use std::collections::HashMap;
use std::sync::Arc;

use crate::preflight::{ClockPolicy, ServerTimeGap};

use super::credentials::{BoundedProbe, bounded_probe};
use super::{CLOCK_PROBE_THREAD, CLOCK_PROBE_TIMEOUT};

/// The venue clock read, delegated to `crate::server_time`'s roster-gated table (the one home of
/// the per-venue knowledge, with a roster completeness test). Returns an ABSOLUTE epoch-ms stamp
/// (what [`crate::preflight::PreflightProbes::venue_server_time_ms`] wants, not
/// `BinanceSpotRest::server_time_offset`'s OFFSET), or one of the two DISTINCT gaps.
///
/// ⚠ **UNBOUNDED, deliberately**: it runs on the caller's thread, bounded only by
/// `crate::server_time::CLOCK_READ_TIMEOUT`, which cannot preempt a wedged name resolution. The
/// preflight calls [`bounded_server_time_ms`] (abandoned at [`CLOCK_PROBE_TIMEOUT`]); so should a
/// new caller, unless it is itself the thread that may be abandoned.
pub fn venue_server_time_ms(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    live_permitted: bool,
) -> Result<i64, ServerTimeGap> {
    crate::server_time::venue_server_time_ms(registry, venue, vars, live_permitted)
}

/// One venue's clock read, ABANDONED at [`CLOCK_PROBE_TIMEOUT`]: [`venue_server_time_ms`] through
/// the same `bounded_probe` the credential leg uses.
///
/// ⚠ **It closes a wedged RESOLVER, not a slow venue.** `CLOCK_READ_TIMEOUT` times an in-flight
/// request, and `std` name resolution has no timeout (`vike_bridge_core::net_probe`'s caveat), so a
/// wedged DNS would park the FIRST clock read at the top of a mount; the leg budget
/// (`crate::preflight::check_clock_skew`) is checked only BETWEEN reads. An abandoned read is
/// outcome ② ([`ServerTimeGap::Unreachable`], a WARN): a broken resolver costs a line, never a
/// demotion.
///
/// ⚠ A venue with no WIRED endpoint is NOT routed through the probe: its answer is the registry
/// row's declared clock (`crate::server_time`'s `clock_decl`), no I/O, so a mount whose armed
/// venues are all DECLARED spawns zero threads.
///
/// ⚠ NOT a fan-out: one venue at a time, roster order, one shared budget —
/// `docs/decisions/0027-clock-budget-derived-from-the-roster.md` rejected per-venue slicing.
///
/// `policy` is shared by refcount like `vars`
/// (`crate::server_time::venue_server_time_ms_under_policy`). Public for the roster tests in
/// `crates/vike-tradehub/tests/mount_roster.rs` (docs/decisions/0096).
pub fn bounded_server_time_ms(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &Arc<HashMap<String, String>>,
    live_permitted: bool,
    policy: &Arc<Option<crate::MountPolicy>>,
) -> Result<i64, ServerTimeGap> {
    if !matches!(
        crate::server_time::clock_decl(registry, venue),
        Some(vike_bridge_core::venue_mount::ClockDecl::Wired { .. })
    ) {
        return crate::server_time::venue_server_time_ms_under_policy(
            registry,
            venue,
            vars,
            live_permitted,
            policy.as_ref().as_ref(),
        );
    }
    let probe: BoundedProbe<Result<i64, ServerTimeGap>> = {
        let venue = venue.to_string();
        let vars = Arc::clone(vars);
        let policy = Arc::clone(policy);
        Arc::new(move || {
            crate::server_time::venue_server_time_ms_under_policy(
                registry,
                &venue,
                &vars,
                live_permitted,
                policy.as_ref().as_ref(),
            )
        })
    };
    bounded_probe(&probe, CLOCK_PROBE_TIMEOUT, CLOCK_PROBE_THREAD)
        .unwrap_or_else(|waited_ms| Err(abandoned_clock_gap(waited_ms)))
}

/// What an ABANDONED clock read reports: outcome ② and nothing else.
///
/// ⚠ Named so the choice can be asserted: both neighbours would lie in the operator's favour.
/// [`ServerTimeGap::NotChecked`] renders NOT-APPLICABLE (and takes a `&'static str` so a runtime
/// failure cannot manufacture one); [`ServerTimeGap::UnmeasuredRisk`] asserts a DECLARED venue
/// property. `Unreachable` is true — the venue publishes a clock, this attempt got none — and a
/// WARN, so our own wedged resolver demotes no venue.
///
/// ⚠ **Residual: two non-wedged cases render as this row too.** [`bounded_probe`] maps a worker
/// that panicked (`recv_timeout` reads `Disconnected`) onto the same `Err(bound)`, after ~0 ms, and
/// an unspawnable worker onto `Err(0)` (a fault on THIS box: "inside the mount's own 0 ms bound").
/// The text stays TRUE (no answer inside the bound; the mount stopped waiting), the verdict is
/// right (② WARN, nothing demoted), the credential leg's `Unanswered` has the same shape, and a
/// panicking fetcher becomes visible rather than hidden.
pub(super) fn abandoned_clock_gap(waited_ms: u64) -> ServerTimeGap {
    ServerTimeGap::Unreachable(format!(
        "no answer inside the mount's own {waited_ms} ms bound, so the mount stopped waiting (the \
         read's own transport timeout cannot preempt a wedged name resolution, so the read was \
         abandoned, not cancelled — a worker that died before answering reads the same here)"
    ))
}

/// The venues the CLOCK leg runs for: every canonical-roster venue this `vars` map would mount
/// LIVE **under this deployment's arming ceiling**, in roster order.
///
/// ⚠ Derived from live INTENT, not the authed-read client map (the module doc's two-lists note):
/// [`crate::would_mount_live_under_policy`] is pure and network-free, so a paper mount gets an
/// EMPTY list and "no credentials ⇒ not one network call" holds.
///
/// ⚠ **The CEILING-AWARE predicate `make_engine` gates on** — the module doc's "what the ceiling
/// is doing in a CLOCK leg" says why a check whose measured QUANTITY is box-scoped is gated per
/// venue.
///
/// A venue with no wired endpoint is still LISTED: its DECLARED not-applicable row is the
/// disclosure. Public for the roster tests in `crates/vike-tradehub/tests/mount_roster.rs`
/// (docs/decisions/0096).
pub fn clock_venues(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> Vec<String> {
    vike_model::VENUES
        .iter()
        .filter(|venue| crate::would_mount_live_under_policy(registry, venue, vars, policy))
        .map(|venue| (*venue).to_string())
        .collect()
}

/// Per-venue thresholds + remediation text for a MEASURED skew, from each venue's declared
/// `ClockRisk`: only wired venues have one, and only venues that REJECT orders over drift carry a
/// FAIL threshold (`crate::server_time::clock_policy_of`). Public for the roster tests in
/// `crates/vike-tradehub/tests/mount_roster.rs` (docs/decisions/0096).
pub fn clock_policies(
    registry: &'static [crate::VenueRow],
    venues: &[String],
) -> HashMap<String, ClockPolicy> {
    venues
        .iter()
        .filter_map(|venue| {
            crate::server_time::clock_policy(registry, venue).map(|p| (venue.clone(), p))
        })
        .collect()
}
