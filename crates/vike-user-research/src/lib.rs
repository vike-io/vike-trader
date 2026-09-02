//! `vike-user-research` — the compiled user-STUDY HOST.
//!
//! `build.rs` scans `<workspace>/user_data/research/studies/rust/<name>/<name>.rs` (build-time
//! override: `VIKE_USER_DATA_DIR`) and generates the registry this crate re-exports:
//! [`USER_STUDIES`], [`user_study_entry`] and [`run_user_study`]. It is the exact mechanism
//! `crates/vike-user-strategies` uses for the strategy tier, copied rather than reinvented —
//! `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md` asks for *"a mechanism
//! copied from `vike-user-strategies`, not as a trait extracted from today's tangle"*.
//!
//! ## What this closes
//!
//! `crates/vike-studio-core/src/listing.rs`'s `list_studies` said it in its own module doc:
//! *"⚠ A study is LISTED, never compiled or run… A study has no execution contract in this
//! workspace at all, and inventing one here would bury a whole design decision inside a directory
//! scan."* This crate is that decision, made where it can be argued: [`crate::contract`]. That
//! doc's second sentence has since been withdrawn BY this crate existing, and the withdrawal is
//! recorded there rather than left for a reader to notice — the boundary it argued for is
//! unchanged.
//!
//! ## What RUNS one
//!
//! Not this crate. A study returns a [`StudyOutcome`] and somebody above mints the run —
//! `crates/vike-studio-core/src/study_run.rs`'s `run_study`, which resolves a name through the
//! registry below and writes the result through `crates/vike-backtest/src/runs.rs`. Why it lives
//! there and cannot live here is [`crate::contract`]'s "Why the study RETURNS its result instead of
//! writing it", finished in
//! `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md`.
//!
//! ## Why a separate crate (the load-bearing property)
//!
//! User entry files compile as modules of THIS crate — a separate compilation unit from
//! `vike-ml` or `vike-analytics` — so they resolve only the platform's PUBLIC API
//! (through this crate's dependencies). An internals refactor of any framework crate can never
//! break a user study, and a user study can never grow a load-bearing dependency on a `pub(crate)`
//! detail (the Hyrum freeze the strategy spec's v2 rejected:
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
//! It is a FUNCTION, not a trait, and it is NON-GENERIC. Both are decisions with arguments, and
//! both arguments live in [`crate::contract`] beside the types rather than here.
//!
//! ⚠ The contract types live in THIS crate rather than in a lower one, and that is deliberate
//! while there is one study. The spec's verdict on the seam is *"The SECOND study is what would
//! justify a trait, and it should be written first, so a seam is extracted from two real users
//! rather than imagined from one"* — so the types sit in the one crate whose entire purpose is
//! this contract, where nothing below them names them and changing them costs one crate. When a
//! second study exists and a lower consumer needs the vocabulary without the host, that is the
//! moment to move them down, not before. `extern crate self as vike_user_research;` below is what
//! lets a user file spell them as `vike_user_research::…` regardless of which crate they end up
//! in, so that move will not touch a single user file.
//!
//! ## The dependency set — and what is deliberately ABSENT
//!
//! `Cargo.toml`'s `[dependencies]` IS the user-study API surface, and each entry is argued there.
//! The absences are argued here, because nothing else in the tree would record them:
//!
//! * **No HTTP client — no `ureq`, no base URL, no API key.**
//!   `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md`, accepted. A study reads
//!   the hist store; the exploratory fetch is an ingest command. The decisive measurement in that
//!   record is that a throttled runtime fetch does not FAIL a study, it becomes a number: on
//!   QuantConnect Cloud a spent `Download()` quota returns an empty string, and that value flows
//!   into a feature, a z-score and a Sharpe.
//! * **No `vike-backfill`.** That is where the vendor clients live, so depending on it would hand
//!   back through the side door exactly what the line above closes.
//! * **No `vike-backtest`.** Illegal by layer, and wrong by kind: a research run's Sharpe comes
//!   from `vike-analytics`' vectorized signal backtest and a strategy backtest's from the
//!   event-driven simulator. The design spends a whole ⚠ on the two not being comparable; a study
//!   able to call the simulator directly is how they would quietly become one number.
//! * **No `vike-report`.** Layer 55, above this crate — the same reason
//!   `crates/vike-research/src/report/html.rs` COPIED its escaping and SVG helpers instead of
//!   calling them. ⚠ That file is GONE (it went with the research crate, and the report had no
//!   successor), so the citation is the evidence for the precedent rather than a file to go and
//!   read; it is filed in `crates/vike-ops/tests/citation_gate.rs`'s `DEAD_PATH_EXCEPTIONS`.
//!   A study that wants an HTML report emits it as an artifact; sharing the real
//!   renderer needs that surface pushed DOWN to a layer-20 crate first, which is a separate piece
//!   of work with its own argument.
//! * **No `serde`/`serde_json`.** A study returns DATA and the caller serializes it — that is the
//!   whole point of [`StudyOutcome`]. A study hand-writing JSON would be a second spelling of a
//!   manifest schema that already has exactly one owner.
//! * **No `sha2`/`hex`.** The fit cache's content-addressed keys are `vike_ml::Learner`'s
//!   `fit_identity`, which is a method a study CALLS rather than a hash it computes.
//!
//! ⚠ **One absence ENDED, and it ended on the condition it named.** This list used to carry a
//! *"No `rayon`"* bullet whose closing sentence was *"It is the likeliest first addition to this
//! manifest, and it should arrive when a real study measures that it needs it."* The cohort study
//! is that consumer, and `rayon`, `indexmap` and `rand_chacha` arrived together for it; `Cargo.toml`
//! argues each on its own terms, and the split that matters is that two of the three are
//! CORRECTNESS (an insertion-ordered map and a portable RNG stream are both bit-reproducibility
//! machinery in this workspace's own stated convention) while only `rayon` is speed. The bullet is
//! not kept as a stale absence: an absence that has ended is a claim, and the record of what it
//! cost to end is in the manifest beside the entries.
//!
//! ⚠ **The tier asymmetry that bullet warned about is REAL and is not cured by admitting it.** A
//! Rhai study cannot express `rayon`, so a study that needs the grid parallel is a Rust-tier study.
//! What makes that acceptable rather than a new problem is that it is not new: the interpreted tier
//! can express no [`StudyLearner`] implementation, no generic and no `#[cfg(test)]` module either,
//! and the design records the tiers as differing *"only in REACH, and that is a property of
//! compilation rather than a rule anyone chose"*. R7 says a study may be written in Rhai **or** in
//! Rust; it does not say every study can be written in both, and no addition to this manifest could
//! make that true.
//!
//! ## CI / empty state
//!
//! No `user_data/` (every CI checkout, every fresh clone) ⇒ the generated registry is EMPTY and
//! this crate is inert — [`user_study_entry`] answers `None` for every name, byte-identical
//! consumer behavior. The full pipeline is still CI-proven on every run through the committed
//! fixture tree (`tests/fixture_user_data/` → a second generated registry the integration test
//! drives end to end, including a real fit through `vike_ml::test_support::ScriptedLearner` and a
//! real store read through `vike_data::test_support::MemHistStore`, so the ML half is proven on a
//! box with no LightGBM binary — which is every CI runner and every Windows box). User code
//! compiles only in a source checkout and into the operator's OWN binary; it never enters the
//! repo — `user_data/` is gitignored.

/// So a `#[path]`-included user file — and the generated registry — can spell this crate's own
/// types as `vike_user_research::…`, exactly as it would spell any dependency. Without it the only
/// spelling would be `crate::…`, which resolves to the HOST inside the library and to the TEST
/// CRATE inside an integration test, so one rendered registry could not serve both.
extern crate self as vike_user_research;

/// The study entry contract: [`StudyContext`], [`StudyOutcome`], [`StudyError`], [`StudyFn`] and
/// the erased learner seam. Every design argument lives in that module's own docs.
pub mod contract;

/// The pure scan/render half, shared verbatim with `build.rs` (which `include!`s the same file).
/// Public so the generator is unit-testable as a plain function of paths and strings.
pub mod gen;

pub use contract::{
    valid_artifact_name, CapturedStudyFit, StudyContext, StudyError, StudyFn, StudyLearner,
    StudyOutcome, MAX_ARTIFACT_NAME,
};

include!(concat!(env!("OUT_DIR"), "/user_registry.rs"));
