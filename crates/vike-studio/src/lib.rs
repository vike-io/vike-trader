//! The egui Studio shell (Studio SP2): a Rhai and Rust editor + store-backed slice picker + a
//! worker-thread Run dispatched to the backtest server + tabbed backtest results. egui without
//! eframe/wgpu (CI-testable like vike-chart); vike-desktop mounts it.

#![warn(unreachable_pub)]

mod backend;
mod panes;
mod studio;
// The Data pane's Import CSV (decision 0272): where an upload goes (the desktop seeds it).
pub use backend::csv_upload::StudioCsvUploadDial;
pub use panes::chat::{StudioChatApiKeys, StudioChatConfig};
pub use panes::editor::StudioEditorPane;
pub use panes::param_scan::StudioParamRow;
pub use panes::slice_picker::{DEPTH_NOT_REPLAYABLE, SERIES_SCAN_ADVICE, StudioSlicePicker};
// PR-5 — the run backend (`Source` and `Named`, both the backtesthub since `Local` was
// deleted) + its remote-dispatch spawns (each through `vike_studio_core::spawn_outcome`, so
// `StudioState::poll` folds a remote outcome on the `Receiver` types the deleted local spawns had).
// They sign with this PC's device key (`StudioState::hub_keys`, decision 0288); no shared key is
// resolved anywhere in this crate (`crates/vike-ops/tests/gui/studio_backtest_key_reach_gate.rs`).
// The BUILD dispatch (through the backtesthub, `spawn_backtesthub_build`) has the same `Receiver` shape,
// folded by `StudioState::poll` the same way; its `Ok(sha)` is what unblocks Run.
pub use backend::backtesthub::strategy_files::spawn_backtesthub_build;
pub use backend::backtesthub::{
    StudioRunBackend, StudioRunEngine, spawn_backtesthub_run, spawn_backtesthub_sweep,
    spawn_backtesthub_walkforward,
};
// The words the Research pane's tests read. The pane is a thin client of the backtest server (studies
// run ONLY there, the owner's ruling of 2026-11-19), so the desktop hands it nothing but the backend.
// ⚠ `BACKTEST_RUN_KIND` is NOT in this list any more, and taking it out was the point rather
// than a casualty: `crates/vike-runs/src/lib.rs` exports it now, `panes/research/mod.rs` takes a private
// `use` of that one, and re-exporting it from here would mint a second name for somebody else's
// constant — what this workspace's conventions call a name that rots. Nothing outside
// `crates/vike-studio/` ever imported it.
pub use panes::research::{STUDY_NEEDS_COMPUTE, STUDY_RESULT_TITLE};
pub use panes::results::StudioResultsTab;
pub use panes::saved::{
    StudioSavedAction, StudioSavedPane, StudioSavedStrategy, StudioStrategySource,
    saved_compare_rows,
};
pub use studio::shell::SHORTCUTS;
pub use studio::workspace::declare_workspace_state_dir;
pub use studio::{RUST_EDITOR_CHIP, StudioAutorun, StudioState, StudioTab};
// The Run VOCABULARY (`DataSlice`, `StrategySpec`, `RunOutcome`, `StudioParamscan`, a study's display
// vocabulary) is vike-studio-core's; its names are spelled `vike_studio_core::…` everywhere, this crate's own tests and
// examples included. The Run PIPELINE that computes them is the backtest server's
// (`vike_backtesthub::studio_run`, 2026-11-10): this crate links no engine. The store handle
// `StudioState` takes is `Arc<dyn HistoryStore + Send + Sync>`, named there by every caller.

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
