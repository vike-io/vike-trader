//! Per-venue arming decisions: what `build_node` actually mounted (`CexArming`), which network a
//! venue's accounts mount on, and the `data_only` credential-withholding rule — and, at the
//! bottom, the two dead-man switches the LIVE mount arms (the silence switch and the per-venue
//! link switch), which are arming decisions of the same kind: pure folds from `policy`, the venue
//! table and the mounted plan onto what the core is told to arm. Those moved here from
//! `crate::tradehub_cli` with the `run` phase split, bodies unchanged.
//!
//! Layout: `venue_arming/tier.rs` holds the network rule (`exec_tier`, `selected_tier`,
//! `cex_mainnet_enabled`, `feed_tier_from_rows`, `feed_tier`); `venue_arming/arming.rs` the exec
//! badge and the per-venue `*_arming` disclosures; `venue_arming/deadman.rs` the dead-man folds
//! (`deadman_config_from_policy`, `mount_link_disclosure`, `link_deadman_config_from_policy`,
//! `link_deadman_arming_report`, `deadman_absent_warning`). `warn_deadman_absent` stays in this
//! file: its tracing target is this module's path, `vike_tradehub::venue_arming`, which
//! `crates/vike-tradehub/tests/deadman_absent_warning.rs` filters on exactly.

// In-crate callers name the canonical child path (`crate::venue_arming::arming::cex_arming`); the
// root re-exports ONLY what `tests/` (or another crate) names as `vike_tradehub::venue_arming::X`.
pub(crate) mod arming;
pub(crate) mod deadman;
pub(crate) mod tier;

pub use deadman::deadman_absent_warning;
pub use tier::{FeedChoice, cex_mainnet_enabled, feed_tier_from_rows, selected_tier};

/// The `Once` latch over [`deadman_absent_warning`]: `tracing::warn!` the message, at most once
/// per process, and only when there is one (an absent key). Same idiom, and the same reason, as
/// `crates/vike-mount/src/paper_fallback.rs`'s `venue_arming_migration` — the fact is
/// process-wide, and a paste-ready block repeated is a paste-ready block buried. Called from
/// `live_mount_with` beside the `deadman:` construction, and from nowhere else: the composition
/// root is where the policy is known to be a LIVE mount's, and the gate-off `paper_mount` arm
/// neither arms the switch nor warns about it.
///
/// ⚠ The latch sits INSIDE the `None` test, not around it, so a call that had nothing to say does
/// not consume the one chance to say it — the test that drives this counts events across a
/// `Some(n)`, a `Some(0)` and two `None`s in that order and expects exactly one.
pub fn warn_deadman_absent(policy: &vike_config::Policy) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    if let Some(message) = deadman_absent_warning(policy) {
        ONCE.call_once(|| tracing::warn!("{message}"));
    }
}
