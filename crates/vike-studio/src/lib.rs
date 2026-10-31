//! The egui Studio shell (Studio SP2): a Rhai editor + store-backed slice picker + worker-thread
//! Run over the `HistStore` + tabbed backtest results. egui without eframe/wgpu (CI-testable like
//! vike-chart); vike-desktop mounts it. Builds on the merged vike-script (RhaiStrategy).

#![warn(unreachable_pub)]

mod backend;
mod panes;
mod studio;
// The off-thread catalog walk behind ⟳ Refresh (the walk, its one message, and the spawn).
pub use backend::catalog::{CATALOG_WALK_LOST, CatalogLoad, load_catalog, spawn_catalog_load};
pub use panes::chat::{ChatApiKeys, ChatOutcome, ChatPane, ChatSettings, apply_result, summary_of};
pub use panes::data_browser::DataBrowserPane;
pub use panes::editor::EditorPane;
pub use panes::indicators::{IndicatorsPane, compute_indicator, default_params, grouped_search};
pub use panes::picker::{
    DEPTH_NOT_REPLAYABLE, SERIES_SCAN_ADVICE, SERIES_SCAN_FAILED, SeriesRow, SlicePicker,
};
// PR-5 — the run backend (`Remote` and `Named`, both the backtesthub since `Local` was
// deleted) + its remote-dispatch spawns (each through `vike_studio_core::spawn_outcome`, so
// `StudioState::poll` folds a remote outcome on the `Receiver` types the deleted local spawns had).
// The backtest key those three spawns sign with rides beside them: its type, its one row name and
// its one resolver over the credential map (`docs/decisions/0083-the-runtime-plugin-join-lands.md`,
// question 1). The desktop is the one caller of the resolver;
// `crates/vike-ops/tests/gui/studio_backtest_key_reach_gate.rs` holds that.
pub use backend::remote::{
    BACKTEST_KEY_NAME, Backend, BacktestKey, backtest_key_from_credentials, spawn_run_remote,
    spawn_sweep_remote, spawn_walkforward_remote,
};
// The BUILDER dispatch — the same `Receiver` shape as the run spawns above, folded by
// `StudioState::poll` the same way. `Ok(sha)` is what unblocks Run.
pub use backend::plugin_build::spawn_build;
// The Research pane + its central-panel result surface: the user's studies, the ONE verb that runs
// one, and the run it leaves behind.
// ⚠ `BACKTEST_RUN_KIND` is NOT in this list any more, and taking it out was the point rather
// than a casualty: `crates/vike-runs/src/lib.rs` exports it now, `research.rs` takes a private
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
pub use studio::workspace::{
    StudioWorkspace, declare_workspace_state_dir, load_workspace, save_workspace,
};
pub use studio::{CenterView, PLUGIN_EDITOR_CHIP, QaAutorun, RightTab, StudioState};
// The Run pipeline moved to vike-studio-core (SP3 Part 0); its names are spelled
// `vike_studio_core::…` everywhere, this crate's own tests and examples included. The store
// handle `StudioState` takes is `vike_data::StoreHandle`, named there by every caller.

#[cfg(test)]
mod smoke {
    // Regression guard: egui_code_editor's `CodeEditor` widget lives behind its `editor`
    // feature (and `.show()` behind `egui`). If the workspace dependency ever regains
    // `default-features = false` without an override, this fails to COMPILE (not just at
    // runtime) because the type won't resolve — catching the defect one task earlier relied on.
    #[test]
    fn code_editor_widget_is_available() {
        let _ = egui_code_editor::CodeEditor::default();
    }
}
