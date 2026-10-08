//! The egui Studio shell (Studio SP2): a Rhai editor + store-backed slice picker + worker-thread
//! Run over the `HistStore` + tabbed backtest results. egui without eframe/wgpu (CI-testable like
//! vike-chart); vike-desktop mounts it. Builds on the merged vike-script (RhaiStrategy).

mod backend;
mod panes;
mod studio;
// The off-thread catalog walk behind ⟳ Refresh (the walk, its one message, and the spawn).
pub use backend::catalog::{CATALOG_WALK_LOST, CatalogLoad, load_catalog, spawn_catalog_load};
pub use panes::chat::{ChatApiKeys, ChatOutcome, ChatPane, apply_result, summary_of};
pub use panes::data_browser::DataBrowserPane;
pub use panes::editor::EditorPane;
pub use panes::indicators::{IndicatorsPane, compute_indicator, default_params, grouped_search};
pub use panes::picker::{
    DEPTH_NOT_REPLAYABLE, SERIES_SCAN_ADVICE, SERIES_SCAN_FAILED, SeriesRow, SlicePicker,
};
// PR-5 — the run backend (`Remote` and `Named`, both the COMPUTE daemon since `Local` was
// deleted) + its remote-dispatch spawns (mirror `spawn_run`/`spawn_paramscan`/`spawn_walkforward`,
// same `Receiver` types, so `StudioState::poll` folds a remote outcome unchanged).
// The compute key those three spawns sign with rides beside them: its type, its one variable and
// its one resolver (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1). The
// desktop is the one caller of the resolver; `crates/vike-ops/tests/gui/studio_compute_key_reach_gate.rs`
// holds that.
pub use backend::remote::{
    Backend, COMPUTE_KEY_ENV, ComputeKey, compute_key_from_vars, spawn_run_remote,
    spawn_sweep_remote, spawn_walkforward_remote,
};
// The BUILDER dispatch — the same `Receiver` shape as the run spawns above, folded by
// `StudioState::poll` the same way. `Ok(sha)` is what unblocks Run.
pub use backend::plugin_build::spawn_build;
// The Research pane + its central-panel result surface: the user's studies, the ONE verb that runs
// one, and the run it leaves behind.
// ⚠ `BACKTEST_RUN_KIND` is NOT in this list any more, and taking it out was the point rather
// than a casualty: `crates/vike-model/src/runs.rs` exports it now, `research.rs` takes a private
// `use` of that one, and re-exporting it from here would mint a second name for somebody else's
// constant — what this workspace's conventions call a name that rots. Nothing outside
// `crates/vike-studio/` ever imported it.
pub use panes::research::{
    NO_HOST, NO_LEARNER, NO_METRICS, NO_RUNS, NO_STUDIES, RUNS_BLURB, ResearchAction, ResearchPane,
    RunRow, STUDY_RESULT_TITLE, StudyHost, fact_rows, kind_status, metric_rows, run_rows,
    study_result_ui, tier_label,
};
pub use panes::results::{ResultsTab, perf_rows, results_ui, returns_hist};
pub use panes::saved::{
    CompareRow, SavedAction, SavedPane, SavedStrategy, StrategySource, comparison_rows,
    load_strategies, save_strategies,
};
pub use panes::templates_gallery::{gallery_ui, template_preview};
pub use studio::workspace::{StudioWorkspace, load_workspace, save_workspace};
pub use studio::{CenterView, PLUGIN_EDITOR_CHIP, QaAutorun, RightTab, StudioState};
// The Run pipeline + templates moved to vike-studio-core (SP3 Part 0). Re-export so existing
// `vike_studio::{DataSlice, StudioParamscan, run_slice, …}` paths resolve. (This also promised
// `vike_studio::templates` until 2026-09-28; the templates moved on to `vike-script` in #1942,
// 2026-09-19 — `vike_script::TEMPLATES` — and nothing re-exports them here.)
pub use vike_studio_core::{
    DataSlice, ParamscanEntry, RunError, RunOutcome, SliceKind, StoreHandle, StrategySpec,
    StudioParamscan, native_strategies, params_from_rows, run_paramscan_slice, run_slice,
    run_walkforward_slice, spawn_paramscan, spawn_run, spawn_walkforward,
};
// The STUDY half of that crate, re-exported for the same reason: a consumer of the Studio shell
// that wants to pose or assert a study run should not have to name a second crate to do it.
pub use vike_studio_core::{STUDY_METRICS_NOTE, STUDY_RUN_KIND, StudyRun, StudyRunError};

#[cfg(test)]
mod smoke {
    #[test]
    fn builds() {
        assert_eq!(2 + 2, 4);
    }

    // Regression guard: egui_code_editor's `CodeEditor` widget lives behind its `editor`
    // feature (and `.show()` behind `egui`). If the workspace dependency ever regains
    // `default-features = false` without an override, this fails to COMPILE (not just at
    // runtime) because the type won't resolve — catching the defect one task earlier relied on.
    #[test]
    fn code_editor_widget_is_available() {
        let _ = egui_code_editor::CodeEditor::default();
    }
}
