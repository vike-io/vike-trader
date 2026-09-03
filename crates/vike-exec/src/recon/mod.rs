//! The normalized reconciliation engine (pure core). `diff` (Task 3) turns venue reports + a
//! `LocalView` into `Divergence`s; `resolve` (Tasks 4–5) turns those + a `ReconPolicy` into a
//! `Recon`. Both are pure fns with no I/O. `client` (Task 6) adds the `ReconClient` venue-facing
//! report seam, `run_pass` (fetch → diff → resolve), and the offline `FakeReconClient` — see the
//! module spec at docs/superpowers/specs/2026-07-15-reconciliation-engine-design.md.

mod client;
mod diff;
mod journal_view;
mod local;
mod resolve;
pub mod types;

pub use client::{run_pass, FakeReconClient, MassStatus, ReconClient};
pub use diff::{diff, diff_balance};
pub use journal_view::JournalView;
pub use local::OwnedLocalState;
// `mode_applies` is exported so a caller can STATE what a policy will auto-fold without
// re-deriving it from `ReconPolicy::mode_for` (which is only half the rule — see its doc);
// `mode_applies_divergence` is its per-DIVERGENCE refinement (inert unless the policy opts in).
pub use resolve::{
    external_coid, mode_applies, mode_applies_divergence, resolve, AdoptContext, PitFn,
    ORPHAN_LOCAL_ORDER_KEY,
};
pub use types::{
    BalanceTol, Divergence, DivergenceKind, DivergenceOrigin, LocalCash, LocalView, Recon,
    ReconAlert, ReconMode, ReconPolicy,
};
