//! The repository's build tooling, as a library — so the answer to "what does CI run" has ONE
//! implementation and three consumers (CI, the justfile, a developer at a prompt).
//!
//! # Why this crate exists
//!
//! The derivation used to live in three languages plus a hand copy: a 1,271-line Python script
//! derived the crate roster and the affected set, `scripts/ci_feature_suite.sh` (bash) was the
//! authority for the feature-lane set, and the `justfile`'s `ci_crates :=` was a hand-written copy of
//! the first, machine-checked against it by `crates/vike-ops/tests/local_gate_mirrors_ci.rs`. Every
//! one of those files was correct on its own; what they could not be is ONE answer. And the Python
//! half was load-bearing in a tree whose standing rule is that there is no Python in it — which it
//! now again is not: the script was deleted in the change that pointed the workflows here.
//!
//! # What is here, and what is still elsewhere
//!
//! [`ci::compute`] is the derivation, and its OUTPUT is a contract with
//! `.github/workflows/{ci,release}.yml`: seven `key=value` lines in a fixed order, a `suites` JSON
//! array whose order becomes matrix-leg order, and one diagnostic line on stderr. Nothing else in
//! the repository re-derives any of it.
//!
//! The feature lanes' KEYS and TRIGGER SETS live here ([`ci::tables::FEATURE_SUITES`]); what a lane
//! RUNS is still `scripts/ci_feature_suite.sh`'s `case` arms, and the justfile's `clippy`/`features`
//! recipes are still gated as a 1:1 mirror of that script. Moving the lane COMMANDS in here is a
//! separate change with its own gate to rewrite; this crate owns the SELECTION only.
//!
//! # The one rule for editing this crate
//!
//! [`ci::tables`] is now the sole authority for the declared sets, so a new exclusion, feature lane
//! or trigger crate is ONE edit — and the argument for it belongs on the table, in that file, because
//! there is no longer a second copy to keep it in step with. `crates/vike-ops/tests/ci_plan_gate.rs`
//! holds the behavioural half: the selection rules and the diff-base ladder, over planted git
//! topologies.

pub mod ci;
pub mod docs_bins;
