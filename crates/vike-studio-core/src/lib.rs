//! Headless Studio backend (Studio SP3): the Run pipeline (resolve→backtest→score over a
//! `HistStore`) + starter Rhai templates, extracted from `vike-studio` so headless consumers — the
//! in-app AI loop (`vike-ai`); originally also the retired `vike-mcp` server, whose tool surface
//! now lives in `vike-cli mcp` over datahub verbs — can reuse it without pulling in egui. No GUI
//! here.
//!
//! Two vocabularies the Run pipeline is parameterized by:
//! - [`StrategySpec`] (`spec`) — Rhai source OR a native `vike-backtest` registry strategy + params.
//! - [`DataSlice`] (`run`) — venue + symbol LIST + [`SliceKind`] (`kind=bar` series, or the
//!   recorded quote/trade/book tick series replayed through `vike_backtest::hist_replay`).
//!
//! Where the strategies themselves COME FROM is [`user_strategies`]: the user-content directory
//! `<project>/user_data/strategies/` (`vike_model::state_path`), which replaces the Studio's
//! one-JSON-blob store. It lives here rather than in `vike-studio` for this crate's own reason —
//! it needs `spec`, `build_strategy` and `vike-script`, and nothing about reading a user's
//! strategy folder is a GUI concern, so a headless caller must be able to do it too.
//!
//! [`ml`] is the crate's ML surface, and it is deliberately the INFERENCE half only: load a model
//! somebody already trained from its TEXT, score rows into a probability series, and report which
//! input mattered. It fits nothing — `scripts/fetch_release_tools.sh`'s `platform_default_tools`
//! ships no trainer binary off Linux (it is a Linux x86-64 ELF, in that script's own words), while
//! `crates/vike-ml/src/infer.rs` is pure Rust over the model TEXT and works everywhere.
//!
//! What a run LEAVES BEHIND is [`listing`], the other end of the same argument: the RUNS under
//! `<project>/user_data/runs/` (`vike_model::runs` writes them) and the STUDIES under
//! `<project>/user_data/research/studies/` that produce them, enumerated with every failure NAMED.
//! It is the reading half only — it compiles nothing, runs nothing, and resolves no path.
//!
//! …and [`study_run`] is what PUTS a study run there. `crates/vike-user-research` can hold a study
//! and cannot run one: it returns a `StudyOutcome` and somebody higher up mints the run. This crate
//! is that somebody, and ⚠ **the reason is no longer a ranking one.** It used to be that
//! `vike-backtest` owned the run directory and only a crate above it could mint; run persistence
//! moved DOWN to `crates/vike-model/src/runs.rs` on 2026-09-13, so every crate in that argument now
//! clears the floor and the arithmetic decides nothing. What survives is R6: this crate already owns
//! the LISTING those runs must appear in, and putting the writer beside the reader is what makes
//! "a study run and a backtest run are rows of one list" a single in-crate test.
//! `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md`'s 2026-09-13 outcome
//! section carries the spent floor and what still holds.
//!
//! …and [`study_dispatch`] is what a SURFACE calls: it holds the one `match` on a study's TIER, so
//! a caller holding the row `list_studies` gave it reaches [`study_run`]'s single persistence path
//! without knowing (or caring) whether the study it clicked is interpreted or compiled. That module
//! also carries the one finding this consumer turned up — `run_study_with` promises the interpreted
//! tier a way in that its own signature cannot give.

//! [`rhai_study`] is the third user-content tier this crate hosts, and the one that RUNS rather
//! than lists: the INTERPRETED half of R7's "some user would want to write it on pure rust some on
//! rhai". It lives here because it is the only layer that can see both halves — `vike-script`
//! (layer 30) for the Rhai precedent and `vike-user-research` (layer 35) for the study contract,
//! and 30 may not see 35. That module's doc carries the whole argument, including the one property
//! this tier has and the compiled one cannot: in a script, the sanctioned surface IS enforcement.

pub mod listing;
pub mod ml;
pub mod rhai_study;
mod run;
pub mod spec;
pub mod study_dispatch;
pub mod study_run;
// The `vike-study` CLI, as a library function so both the bin and the `vike` multicall
// dispatcher reach one copy. Gated exactly as the bin is (`required-features = ["study-cli"]`):
// `scripts/ci_feature_suite.sh`'s `studio-standalone` lane greps this crate's DEFAULT normal-dep
// tree for a datafusion crate and fails if it finds one, so this must stay invisible by default.
#[cfg(feature = "study-cli")]
pub mod study_cli;
pub mod templates;
// The Studio compute-to-data BOUNDARY: the `Wire*` DTO ↔ studio-type conversions and the three
// `*_local` runners the COMPUTE daemon serves `RunSlice`/`RunSweep`/`RunWalkforward` with. MOVED
// here from `vike-datahub` by ruling 7 — that module's own doc carries why the layer rule left it
// no other home.
pub mod user_strategies;
pub mod wire_run;

pub use ml::{FeatureWeight, ModelError, ScoringModel};
pub use rhai_study::{RhaiStudy, StudyFit};
pub use run::{
    CompareOutcome, DataSlice, ParamscanEntry, REPLAYABLE_TICK_KINDS, RunError, RunOutcome,
    SeriesLists, SliceKind, StoreHandle, StudioParamscan, UNREPLAYABLE_TICK_KIND, bar_series,
    build_strategy, build_strategy_with, compare_all_slice, depth_only_series, load_slice_bars,
    run_paramscan_slice, run_paramscan_slice_with_params, run_slice, run_walkforward_slice,
    run_walkforward_slice_with_params, series_lists, spawn_compare_all, spawn_outcome,
    spawn_paramscan, spawn_run, spawn_walkforward, tick_series,
};
pub use spec::{
    StrategySpec, empty_params, native_strategies, params_from_rows, params_with_overrides,
};
pub use study_dispatch::{
    RECIPE_EXT, StudyRunPlan, read_recipe, recipes, run_study_plan, spawn_study,
};
pub use study_run::{
    STUDY_METRICS_NOTE, STUDY_RUN_KIND, StudyPersistError, StudyRun, StudyRunError,
    StudyRunRequest, run_study, run_study_with,
};
pub use wire_run::{
    run_error_to_wire, run_slice_local, run_sweep_local, run_walkforward_local, studio_run_table,
    study_run_fn, to_data_slice, to_engine_params, to_strategy_spec, to_wire_paramscan_result,
    to_wire_result, to_wire_walkforward_result,
};
