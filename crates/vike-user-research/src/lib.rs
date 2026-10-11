//! `vike-user-research` — the compiled user-STUDY HOST.
//!
//! `build.rs` scans `<workspace>/user_data/research/studies/rust/<name>/<name>.rs` (build-time
//! override: `VIKE_USER_DATA_DIR`) and generates the registry this crate re-exports:
//! [`USER_STUDIES`], [`user_study_entry`] and [`run_user_study`]. The build mechanism is the one
//! `crates/vike-user-strategies` calls too, `vike_model::host_build` — plain functions, not a
//! trait (`docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md`); this crate
//! keeps only its tier paths, its both-tiers warning and its generated resolver (`src/codegen.rs`).
//!
//! ## What this closes
//!
//! `crates/vike-studio-core/src/listing.rs`'s `list_studies` LISTS a study and never runs one;
//! inventing an execution contract inside a directory scan would have buried a design decision.
//! This crate is that decision, made where it can be argued: [`crate::contract`].
//!
//! ## What RUNS one
//!
//! Not this crate. A study returns a [`StudyOutcome`] and the caller mints the run —
//! `crates/vike-studio-core/src/study_run.rs`'s `run_study`, which resolves a name through the
//! registry below and writes the result through `crates/vike-model/src/runs.rs`. Why it cannot live
//! here: [`crate::contract`]'s "Why the study RETURNS its result instead of writing it" and
//! `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md`.
//!
//! ## Why a separate crate (the load-bearing property)
//!
//! User entry files compile as modules of THIS crate, a separate compilation unit, so they resolve
//! only the platform's PUBLIC API: an internals refactor can never break a user study, and a study
//! can never freeze a `pub(crate)` detail (the Hyrum freeze the strategy spec's v2 rejected:
//! `docs/superpowers/specs/2026-08-11-user-native-strategies-design.md`).
//!
//! ## The entry-file contract
//!
//! ```ignore
//! use vike_user_research::{StudyContext, StudyError, StudyOutcome};
//!
//! pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError>
//! ```
//!
//! A FUNCTION, not a trait, and NON-GENERIC; both arguments live in [`crate::contract`].
//!
//! ⚠ The contract types live in THIS crate deliberately while there is one study: the spec says
//! the SECOND study is what would justify a trait. When a second study exists and a lower
//! consumer needs the vocabulary without the host, that is the moment to move them down, not
//! before. `extern crate self as vike_user_research;` below lets a user file spell them
//! `vike_user_research::…` wherever they end up, so that move touches no user file.
//!
//! ## The dependency set — and what is deliberately ABSENT
//!
//! `Cargo.toml`'s `[dependencies]` IS the user-study API surface, and each entry is argued there.
//! The absences are argued here, because nothing else in the tree would record them:
//!
//! * **No HTTP client — no `ureq`, no base URL, no API key.**
//!   `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md`: a study reads the hist
//!   store; fetching is an ingest command. A throttled runtime fetch does not FAIL a study, it
//!   becomes a number (an empty string that flows into a feature, a z-score and a Sharpe).
//! * **No `vike-backfill`.** It holds the vendor clients: the side door to the line above.
//! * **No `vike-backtest`.** Illegal by layer, and wrong by kind: a research run's Sharpe comes
//!   from `vike-analytics`' vectorized signal backtest and a strategy backtest's from the
//!   event-driven simulator. The design spends a whole ⚠ on the two not being comparable; a study
//!   able to call the simulator directly is how they would quietly become one number.
//! * **No `vike-report`.** Layer 30, above this crate — why the GONE
//!   `crates/vike-research/src/report/html.rs` COPIED its escaping and SVG helpers (filed in
//!   `crates/vike-ops/tests/docs/citation_gate/dead_paths.rs`'s `DEAD_PATH_EXCEPTIONS`). A study
//!   emits an HTML report as an artifact.
//! * **No `serde`/`serde_json`.** A study returns DATA ([`StudyOutcome`]) and the caller
//!   serializes it; hand-written JSON would be a second spelling of a manifest schema.
//! * **No `sha2`/`hex`.** Fit-cache keys are `vike_ml::Learner`'s `fit_identity`, a method a study
//!   CALLS, not a hash it computes.
//!
//! ⚠ **One absence ENDED on the condition it named** ("No `rayon`" until "a real study measures
//! that it needs it"): the cohort study did, and `rayon`, `indexmap` and `rand_chacha` arrived
//! together, each argued in `Cargo.toml`. ⚠ **The tier asymmetry is REAL and accepted**: a Rhai
//! study cannot express `rayon` — nor a [`StudyLearner`] implementation, a generic or a
//! `#[cfg(test)]` module — because the tiers differ in REACH, a property of compilation.
//!
//! ## CI / empty state
//!
//! No `user_data/` (every CI checkout, every fresh clone) ⇒ the registry is EMPTY and this crate
//! inert: [`user_study_entry`] answers `None` for every name. The full pipeline is still CI-proven
//! through the committed `tests/fixture_user_data/` tree and a second generated registry, with a
//! real fit through `vike_ml::seam::test_support::ScriptedLearner` and a real store read through
//! `vike_data::MemHistStore` — no LightGBM binary needed. `user_data/` is gitignored: user code
//! compiles only into the operator's OWN binary.

// The lib side of the lint: `build.rs` is a separate crate and never sees it.
#![warn(unreachable_pub)]

/// So a `#[path]`-included user file and the generated registry spell this crate's types
/// `vike_user_research::…`, like any dependency. `crate::…` would mean the HOST in the library but
/// the TEST CRATE in an integration test, so one rendered registry could not serve both.
extern crate self as vike_user_research;

/// The study entry contract: [`StudyContext`], [`StudyOutcome`], [`StudyError`], [`StudyFn`] and
/// the erased learner seam. Every design argument lives in that module's own docs.
pub mod contract;

/// This host's scan policy and registry render over `vike_model::host_build`, `include!`d
/// verbatim by `build.rs`. Public so the generator is unit-testable as a plain function of paths
/// and strings.
pub mod codegen;

pub use contract::{
    CapturedStudyFit, MAX_ARTIFACT_NAME, SimOutcome, StudyContext, StudyError, StudyFn,
    StudyLearner, StudyOutcome, StudySim, valid_artifact_name,
};

include!(concat!(env!("OUT_DIR"), "/user_registry.rs"));
