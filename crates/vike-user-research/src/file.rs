//! **A study is a FILE** (owner's rule, 2026-11-15): `<project>/user_data/research/studies/rust/<name>/<name>.rs`,
//! copied into its folder and run with `vike-cli research study`, with NO rebuild of vike. It is built,
//! loaded and sandboxed exactly like a strategy file:
//!
//! - **built** by the runtime builder (`vike-strategy-builder`'s `build_study_file`) into
//!   `<project>/user_data/strategy-builds/<folder>/<sha256>.so`, the study's folder written verbatim into the
//!   cdylib template `crates/vike-rust-strategy/template/study_lib.rs.in`;
//! - **loaded** by the one loader (`vike_rust_strategy::loader::load_study`: the same handshake, the same
//!   log slot);
//! - **run** in the same jailed process a strategy file runs in (`vike-backend strategy-sandbox study`:
//!   no file, no network, no other process), which a host spawns per study run.
//!
//! There is no other way a study runs: nothing compiles one into a vike binary.
//!
//! # The two halves, and what crosses
//!
//! The study file links THIS crate, so [`guest`] is the study's side: it builds the [`crate::StudyContext`]
//! a study is handed out of a host connection, calls the study's `run`, and answers its finished
//! outcome. [`host`] is the other side, the **parent**'s (the process that holds the store and the
//! trainer, and mints the run): it answers the context's read verbs, the learner and the study's ONE
//! backtest (`host::Parent`), and the run is written exactly as it always was
//! (`crates/vike-study-runner/src/study_run.rs`'s `run_study_with`). The backtest runs in the parent so
//! a strategy FILE its profile names takes the same builder, loader and sandbox as every backtest; the
//! store the study hands it is read back through the study file (`guest::serve`, `host::Serve`). The
//! **sandboxed child** only relays, unread.
//!
//! What crosses is MSGPACK (`rmp-serde`), never JSON: a JSON number cannot carry a `NaN` metric or a
//! non-finite price, and a study's numbers must reach the run as the study computed them.
//!
//! # What a study file can NOT do, and why it is the same study
//!
//! Inside the jail a study reads the store only through the context (decision 0029, now held by the
//! kernel rather than by the absence of a dependency): no file, no socket, no process. Its
//! `ctx.scratch()` is a path it cannot write, so a study that keeps a cache there (the cohort study's
//! fit cache) runs uncached, its numbers unchanged. A trained model whose text the learner hands back
//! (`vike_ml_training::GbdtLearner`) is parsed beside the study (`vike_ml_prediction::parse_model_text`, the parse the
//! learner itself made), so scoring a row crosses nothing; any other model stays with the parent and is
//! asked per row.

pub mod guest;
pub mod host;
mod wire;
