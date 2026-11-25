//! vike-study-runner — the study HOST the backtest server runs. Studies run ONLY on the server (the
//! owner's ruling of 2026-11-19): the backtesthub's `RunStudy`, `ListStudies` and `ListRuns` and the
//! `vike-study` runner call this crate, and the desktop links none of it
//! (`crates/vike-ops/tests/architecture/named_run_closure_gate/fences.rs`'s `THIN_CLIENT_FENCE`). It left
//! `vike-studio-core` that day, because that crate is what the desktop links.
//!
//! The module map (each module's own `//!` carries its argument):
//!
//! - [`listing`] — the RUNS under a runs root and the STUDIES of a study folder, enumerated with every
//!   failure NAMED. Reading half only.
//! - [`study_run`] — running a study and leaving a run behind: the ONE persistence path of every study,
//!   writing `vike_studio_core::study_run`'s vocabulary.
//!   `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md` carries why the runner lives
//!   beside the listing.
//! - [`study_dispatch`] — the one `match` on a study's tier, and the recipe and window helpers both
//!   hosts share. The HOST hands the learner, the backtest seam and the Rust-file runner
//!   (`study_dispatch::StudyRunPlan`).
//! - [`rhai_study`] — the INTERPRETED study tier, run in the host's process under the study limits.
//!
//! Nothing is re-exported at the root: every caller names the module path.

pub mod listing;
pub mod rhai_study;
pub mod study_dispatch;
pub mod study_run;
