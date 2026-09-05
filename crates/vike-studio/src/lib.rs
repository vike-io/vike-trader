//! The egui Studio shell (Studio SP2): a Rhai editor + store-backed slice picker + worker-thread
//! Run over the `HistStore` + tabbed backtest results. egui without eframe/wgpu (CI-testable like
//! vike-chart); vike-app mounts it. Builds on the merged vike-script (RhaiStrategy).

mod catalog;
mod chat;
// The "Connect to Claude" `claude mcp add` generator (spawns `vike-cli mcp`) — only compiled under
// the non-default `mcp` feature, mirroring the retired vike-mcp dep gate it replaced (Phase B).
#[cfg(feature = "mcp")]
mod connect;
mod data_browser;
mod editor;
mod indicators;
mod picker;
mod remote;
mod research;
mod results;
mod saved;
mod studio;
mod syntax;
mod templates_gallery;
pub mod theme;
mod workspace;
// The off-thread catalog walk behind ⟳ Refresh (the walk, its one message, and the spawn).
pub use catalog::{CATALOG_WALK_LOST, CatalogLoad, load_catalog, spawn_catalog_load};
pub use chat::{ChatApiKeys, ChatOutcome, ChatPane, apply_result, summary_of};
pub use data_browser::DataBrowserPane;
pub use editor::EditorPane;
pub use indicators::{IndicatorsPane, compute_indicator, default_params, grouped_search};
pub use picker::{
    DEPTH_NOT_REPLAYABLE, SERIES_SCAN_ADVICE, SERIES_SCAN_FAILED, SeriesRow, SlicePicker,
};
// PR-5 — the Local/Remote run backend + its remote-dispatch spawns (mirror `spawn_run`/`spawn_sweep`/
// `spawn_walkforward`, same `Receiver` types, so `StudioState::poll` folds a remote outcome unchanged).
pub use remote::{
    Backend, remote_store_tick_refusal, spawn_run_remote, spawn_sweep_remote,
    spawn_walkforward_remote,
};
// The Research pane + its central-panel result surface: the user's studies, the ONE verb that runs
// one, and the run it leaves behind.
pub use research::{
    BACKTEST_RUN_KIND, NO_HOST, NO_LEARNER, NO_METRICS, NO_RUNS, NO_STUDIES, RUNS_BLURB,
    ResearchAction, ResearchPane, RunRow, STUDY_RESULT_TITLE, StudyHost, fact_rows, kind_color,
    metric_rows, run_rows, study_result_ui, tier_label,
};
pub use results::{ResultsTab, perf_rows, results_ui, returns_hist};
pub use saved::{
    CompareRow, SavedAction, SavedPane, SavedStrategy, StrategySource, comparison_rows,
    load_strategies, save_strategies,
};
pub use studio::{CenterView, RightTab, StudioState};
pub use templates_gallery::{gallery_ui, template_preview};
pub use workspace::{StudioWorkspace, load_workspace, save_workspace};
// The Run pipeline + templates moved to vike-studio-core (SP3 Part 0). Re-export so existing
// `vike_studio::{DataSlice, StudioSweep, run_slice, …}` and `vike_studio::templates` paths resolve.
pub use vike_studio_core::{
    DataSlice, RunError, RunOutcome, SliceKind, StoreHandle, StrategySpec, StudioSweep, SweepEntry,
    native_strategies, params_from_rows, run_slice, run_sweep_slice, run_walkforward_slice,
    spawn_run, spawn_sweep, spawn_walkforward, templates,
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
