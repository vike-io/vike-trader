//! CoreSnapshot — the immutable state view the core publishes to the GUI via arc-swap.
//!
//! The GUI is a LOSSY DOWNSTREAM OBSERVER (plan §1): it reads the latest snapshot on
//! repaint and can never back-pressure the core. Snapshots are built on a dirty flag: coalesced
//! to a ≥16 ms interval while the core is busy, AND on every idle transition (see
//! `crates/vike-core/src/runtime/publish.rs`'s `publish_guarded`). On a sporadic feed that is
//! once per event, so the build cost IS on the per-event hot path: it lands inside the gated core
//! hop, and the `snapshot-build-*` labels of the latency harness measure it alone.

mod build;
mod views;

pub use views::{
    CoreSnapshot, HeldOrderView, MountRowKind, MountView, OrderView, PositionView, ReconAlertView,
    ReconBlock, VenueBlock,
};

#[cfg(test)]
mod query_tests;

#[cfg(test)]
mod snapshot_tests;

/// **STAGE 0 of the account-routing seam** — the additive `VenueBlock::{account, route_key}` and
/// `CoreSnapshot::accounts_epoch`
/// (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §7.1, §6.3).
///
/// The claim these tests exist for is **"a single-account node is byte-identical"**, which is the
/// whole of what makes Stage 0 safe to stop at. It is asserted as a FROZEN BASELINE rather than as
/// a behavioural property, for the reason `runtime::mount_account_tests`'
/// `an_account_less_mount_is_the_single_account_core_unchanged` states: *"unchanged" is the claim,
/// and a behavioural test can only ever check the properties somebody thought to name.*
#[cfg(test)]
mod account_fields_tests;

#[cfg(test)]
use vike_exec::{BalanceMode, ExecutionEngine, PriceCfg};
