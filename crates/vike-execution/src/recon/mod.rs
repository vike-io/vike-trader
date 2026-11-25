//! The normalized reconciliation engine (pure core). `diff` turns venue reports + a `ReconcileLocalView`
//! into `ReconcileDivergence`s; `resolve` turns those + a `ReconcilePolicy` into a `ReconcilePassOutcome`; both are pure fns
//! with no I/O. The live pass puts `persist` between them (two passes in a row: decision 0043 as amended
//! 2026-11-21). `client` adds the `ReconcileClient` venue-facing report seam, `run_pass`
//! (fetch → diff → resolve) and the offline `ReconcileClientFake`; `restore` plans which orders a
//! previous session left behind to adopt, declare gone or leave alone (also pure). Design:
//! docs/superpowers/specs/2026-07-15-reconciliation-engine-design.md.

mod admitted;
mod client;
mod diff;
mod journal_view;
mod local;
mod persist;
mod resolve;
mod restore;
mod terms;
pub mod types;

pub use admitted::admitted_throughout;
pub use client::{ReconcileClient, ReconcileFetchError, ReconcileMassStatus, run_pass};
#[cfg(any(test, feature = "test-support"))]
pub use client::{ReconcileClientFake, ReconcileClientStalling};
pub use diff::{ReconcileBalanceCheck, diff, diff_balance, diff_scoped};
pub use journal_view::ReconcileJournalView;
pub use local::ReconcileLocalState;
pub use persist::{RECONCILE_CONFIRM_PASSES, ReconcileStreak, persist};
// `mode_applies` is exported so a caller can STATE what a policy will auto-fold without
// re-deriving it from `ReconcilePolicy::mode_for` (only half the rule — see its doc);
// `mode_applies_divergence` is its per-DIVERGENCE refinement.
pub use resolve::{
    ORPHAN_LOCAL_ORDER_KEY, PitFn, ReconcileAdoptContext, external_order_client_id, mode_applies,
    mode_applies_divergence, orphan_local_position_alert, resolve, resolve_scoped,
};
pub use restore::{
    RESTORE_GONE_REASON, RESTORE_MIN_QUIET_SPAN_MS, RESTORE_QUIET_PASSES, RestorePlan,
    RestoreScope, name_by_venue_order_id, plan_restore,
};
pub use terms::diff_order_terms;
pub use types::{
    ReconAlert, ReconcileBalanceTolerance, ReconcileDivergence, ReconcileLocalCash,
    ReconcileLocalView, ReconcileMode, ReconcileOrphanPosition, ReconcilePassOutcome,
    ReconcilePolicy, RestoredOrderRef,
};
