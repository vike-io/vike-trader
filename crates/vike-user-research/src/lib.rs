//! `vike-user-research` — the user-STUDY contract, and the two halves a study FILE runs across.
//!
//! A study is a FILE (owner's rule, 2026-11-15): `<project>/user_data/research/studies/rust/<name>/<name>.rs`,
//! built by the runtime builder, loaded by the one loader and run in the jailed process a strategy
//! file runs in, exactly like a strategy file, with no rebuild of vike. [`file`] holds how; nothing
//! compiles a study into a vike binary.
//!
//! ## What this closes
//!
//! `crates/vike-study-runner/src/listing.rs`'s `list_studies` LISTS a study and never runs one;
//! inventing an execution contract inside a directory scan would have buried a design decision.
//! This crate is that decision, made where it can be argued: [`crate::contract`].
//!
//! ## What RUNS one
//!
//! Not this crate. A study returns a [`StudyOutcome`] and the caller mints the run —
//! `crates/vike-study-runner/src/study_run.rs`'s `run_study_with`, handed the study file's sandboxed
//! run by its host (`crates/vike-backtesthub/src/study_sandbox.rs`'s `run_in_child`), and it writes
//! the result through `crates/vike-runs/src/lib.rs`. Why it cannot live here: [`crate::contract`]'s
//! "Why the study RETURNS its result instead of writing it" and
//! `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md`.
//!
//! ## Why a separate crate (the load-bearing property)
//!
//! A study file compiles against this crate's PUBLIC API only, in its own artifact: an internals
//! refactor can never break a user study, and a study can never freeze a `pub(crate)` detail (the
//! Hyrum freeze the strategy spec's v2 rejected:
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
//! A FUNCTION, not a trait, and NON-GENERIC; both arguments live in [`crate::contract`]. A study may
//! split itself into sibling `.rs` files beside `<name>.rs` (`mod matrix;` resolves to its folder's
//! `matrix.rs`); the folder's `.rs` files are the study, built together.
//!
//! ## The dependency set — and what is deliberately ABSENT
//!
//! `Cargo.toml`'s `[dependencies]` IS the user-study API surface (the study file template,
//! `crates/vike-rust-strategy/template/study_Cargo.toml.in`, names the same crates, plus `vike-simulator`
//! for the RL environment (decision 0291), which this crate itself never names), and each entry is
//! argued there. The absences are argued here, because nothing else in the tree would record them:
//!
//! * **No HTTP client — no `ureq`, no base URL, no API key.**
//!   `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md`: a study reads the hist
//!   store; fetching is an ingest command. A throttled runtime fetch does not FAIL a study, it
//!   becomes a number (an empty string that flows into a feature, a z-score and a Sharpe). Since a
//!   study is a file the jail refuses every socket too.
//! * **No `vike-backfill`.** It holds the vendor clients: the side door to the line above.
//! * **No `vike-backtest`.** Illegal by layer, and wrong by kind: a research run's Sharpe comes
//!   from `vike-analytics`' vectorized signal backtest and a strategy backtest's from the
//!   event-driven simulator. The design spends a whole ⚠ on the two not being comparable; a study
//!   able to call the simulator directly is how they would quietly become one number. ⚠ ONE cost
//!   model, two engines (the owner, 2026-11-19): a study charges the backtest's costs, the one cost
//!   type its recipe states ([`costs_of_recipe`]), so the two numbers differ by engine, never by a
//!   cost formula of the study's own.
//! * **No `vike-report`.** Layer 30, above this crate — why the GONE
//!   `crates/vike-research/src/report/html.rs` COPIED its escaping and SVG helpers (filed in
//!   `crates/vike-ops/tests/docs/citation_gate/dead_paths.rs`'s `DEAD_PATH_EXCEPTIONS`). A study
//!   emits an HTML report as an artifact.
//! * **`serde` and `rmp-serde` are here for [`file`]'s messages only.** A study still returns DATA
//!   ([`StudyOutcome`]) and the caller serializes the run; hand-written JSON in a study would be a
//!   second spelling of a manifest schema.
//! * **No `sha2`/`hex`.** Fit-cache keys are `vike_ml_training::Learner`'s `fit_identity`, a method a study
//!   CALLS, not a hash it computes.
//!
//! ⚠ **One absence ENDED on the condition it named** ("No `rayon`" until "a real study measures
//! that it needs it"): the cohort study did, and `rayon`, `indexmap` and `rand_chacha` arrived
//! together, each argued in `Cargo.toml`. ⚠ **The tier asymmetry is REAL and accepted**: a Rhai
//! study cannot express `rayon` — nor a [`StudyLearner`] implementation, a generic or a
//! `#[cfg(test)]` module — because the tiers differ in REACH, a property of compilation.
//!
//! ## CI
//!
//! The committed `tests/fixture_user_data/` tree holds two fixtures and the real studies, `poly_mm`,
//! `microstructure` (VPIN, BVC-VPIN, the tick rule and the information-driven bars over a trade
//! tape), `book_export`, `event_calibration` and the other book studies beside them, among them
//! `rl_episode`, a training and a held-out evaluation episode of the RL environment, split by time
//! (`vike_simulator::RlEnvironment`, decision 0291).
//! `tests/pipeline.rs` compiles them into its TEST binary (never a shipped one) and runs each both
//! directly and across [`file`]'s two halves, which must agree bit for bit; the lane test
//! `crates/vike-strategy-sandbox/tests/study_child.rs` builds them with the builder and runs them in
//! the jailed process.

// The lib side of the lint.
#![warn(unreachable_pub)]

/// So a study file spells this crate's types `vike_user_research::…`, like any dependency, and the
/// tests that compile a fixture study as a module of their own binary do too.
extern crate self as vike_user_research;

// `Cargo.toml`'s `[dependencies]` IS the user-study API surface: a study FILE names these five, this crate's
// own code never does, and cargo 1.100's `unused_dependencies` would read them as dead (a per-crate
// `[lints.cargo]` cannot sit beside `[lints] workspace = true`). Naming each once keeps the table what it is.
use {rayon as _, vike_arbitrage as _, vike_orderflow as _, vike_paths as _, vike_rag as _};

/// The study entry contract: [`StudyContext`], [`StudyOutcome`], [`StudyError`], [`StudyFn`] and
/// the erased learner seam. Every design argument lives in that module's own docs.
pub mod contract;

/// A study FILE: its side ([`file::guest`]) and its host's ([`file::host`]).
pub mod file;

pub use contract::{
    CapturedStudyFit, ContractValuation, KeptInstruments, MAX_ARTIFACT_NAME, SimOutcome,
    StudyContext, StudyError, StudyFn, StudyForwardRun, StudyLearner, StudyOutcome, StudyRuns,
    StudySim, costs_of_recipe, valid_artifact_name,
};
