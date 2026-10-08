//! The normalized reconciliation engine (pure core). `diff` turns venue reports + a `LocalView`
//! into `Divergence`s; `resolve` turns those + a `ReconPolicy` into a `Recon`; both are pure fns
//! with no I/O. `client` adds the `ReconClient` venue-facing report seam, `run_pass`
//! (fetch → diff → resolve) and the offline `FakeReconClient`. Design:
//! docs/superpowers/specs/2026-07-15-reconciliation-engine-design.md.

mod client;
mod diff;
mod journal_view;
mod local;
mod resolve;
pub mod types;

#[cfg(any(test, feature = "test-support"))]
pub use client::FakeReconClient;
pub use client::{MassStatus, ReconClient, run_pass};
pub use diff::{BalanceCheck, diff, diff_balance};
pub use journal_view::JournalView;
pub use local::OwnedLocalState;
// `mode_applies` is exported so a caller can STATE what a policy will auto-fold without
// re-deriving it from `ReconPolicy::mode_for` (only half the rule — see its doc);
// `mode_applies_divergence` is its per-DIVERGENCE refinement.
pub use resolve::{
    AdoptContext, ORPHAN_LOCAL_ORDER_KEY, PitFn, external_coid, mode_applies,
    mode_applies_divergence, resolve,
};
pub use types::{
    BalanceTol, Divergence, DivergenceKind, DivergenceOrigin, LocalCash, LocalView, POLICY_NAMES,
    Recon, ReconAlert, ReconMode, ReconPolicy,
};
