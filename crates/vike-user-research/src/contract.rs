//! The STUDY ENTRY CONTRACT: what a user's `<name>.rs` is handed, and what it hands back.
//!
//! ```ignore
//! pub fn run(
//!     ctx: &vike_user_research::StudyContext,
//!     params: &toml::Value,
//! ) -> Result<vike_user_research::StudyOutcome, vike_user_research::StudyError>
//! ```
//!
//! [`StudyFn`] is that signature as a type, and the generated registry coerces every entry to it
//! (`user_<name>::run as StudyFn`) — so a user file whose signature drifts fails the BUILD naming
//! the mismatch, not the call site of a study somebody is waiting on.
//!
//! # Why a FUNCTION and not a `Study` trait
//!
//! `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md` refuses a `Study`
//! trait — *"Wrong tool for one implementation; extract it from two"*. A study is called ONCE and
//! returns a value: no trait to implement, no object to construct, no lifecycle to learn. A
//! strategy needs a trait because it is DRIVEN (`on_bar` thousands of times, its state living
//! between calls); a study has no between-calls.
//!
//! # Why the study RETURNS its result instead of writing it
//!
//! `crates/vike-model/src/runs.rs` owns run persistence for EVERY kind of run (`create_run_dir`'s
//! id mint, `RunManifest`, `write_run`'s manifest-last completion marker) at layer 50, ABOVE this
//! crate. A study that minted its own run would be a second copy of a schema whose point (R6: one
//! results surface) is that there is only one.
//!
//! So: the study produces [`StudyOutcome`]; the caller mints the run, writes the artifacts and
//! folds the metrics into the manifest's `detail`. The argument is
//! `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md`; the precedent was
//! `crates/vike-research/src/cli.rs`'s `run_cohort` (deleted, filed in
//! `crates/vike-ops/tests/docs/citation_gate/dead_paths.rs`'s `DEAD_PATH_EXCEPTIONS`).
//!
//! # Why the context is HANDED IN
//!
//! `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md`: a study reads the hist
//! store and nothing else. This is where that becomes structural. The study never resolves a path,
//! never reads an environment variable, never opens a socket and never constructs a store — it is
//! handed [`StudyContext`], which forwards the store's READ verbs and nothing else. `vike-data` is
//! taken feature-free, so `DataFusionHist` (the only implementation with an append half) is not
//! compiled and a study holds no value it could append through.
//!
//! ⚠ **That is a property of the SANCTIONED surface, not a guarantee.** ADR 0029 says so itself:
//! *"The lever is NOT enforcement."* A user file is ordinary Rust compiled into the operator's own
//! binary; what this contract buys is that the cheapest path is the correct one and that an
//! unsanctioned one has to be added to a manifest, in a diff.

mod context;
mod error;
mod learner;
mod outcome;
mod sim;

pub use context::StudyContext;
pub use error::StudyError;
pub use learner::{CapturedStudyFit, StudyLearner};
pub use outcome::{MAX_ARTIFACT_NAME, StudyOutcome, valid_artifact_name};
pub use sim::{SimOutcome, StudySim};

/// The entry function every user study exposes, as a type.
///
/// NON-GENERIC on purpose, unlike the strategy tier's `build<B: HftBroker>`: R7 says a study may
/// be written in Rhai, a Rhai script cannot carry a type parameter, and a study places no orders,
/// so nothing about it is generic over a broker.
pub type StudyFn = fn(&StudyContext, &toml::Value) -> Result<StudyOutcome, StudyError>;
