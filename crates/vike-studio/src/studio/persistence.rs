//! `StudioState`'s persistence: the saved-strategy list's and the workspace snapshot's paths and
//! writes (`persist_saved`, `workspace_snapshot`, `maybe_persist_workspace`, `flush_workspace`), and
//! the fold of a Saved pane action (`handle_saved_action`) that mutates and persists that list.
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;
use crate::backend::backtesthub::named_run;

impl StudioState {
    /// Path the saved-strategy list is persisted to: in the Studio's state directory (the desktop
    /// passes the project's `settings/state`, beside the workspace snapshot).
    fn saved_strategies_path(&self) -> std::path::PathBuf {
        self.state_dir.join(SAVED_STRATEGIES_FILE)
    }

    /// A loaded row's costs back in the test conditions (each of `STUDIO_COST_ROWS`, empty where the row set
    /// none, as its Run had it), answering its other params, the values.
    fn restore_costs(&mut self, params: &[(String, String)]) -> Vec<(String, String)> {
        let is_cost = |key: &str| STUDIO_COST_ROWS.iter().any(|(cost, _)| *cost == key);
        let (costs, values): (Vec<_>, Vec<_>) =
            params.iter().cloned().partition(|(key, _)| is_cost(key));
        for (cost, _) in STUDIO_COST_ROWS {
            // The last row of a key is the one a run reads (`params_from_rows`).
            let value = costs.iter().rev().find(|(key, _)| key == cost);
            let value = value.map(|(_, value)| value.clone()).unwrap_or_default();
            match self.conditions.costs.iter_mut().find(|row| row.name == cost) {
                Some(row) => row.value = value,
                None => self.conditions.costs.push(StudioParamRow {
                    name: cost.to_string(),
                    value,
                    ..StudioParamRow::default()
                }),
            }
        }
        values
    }

    /// A loaded Rhai or Rust row's values back on the Param scan pane's rows of its kind ([`Self::grid`]): each
    /// row its saved value (empty where the row saved none), a value of a name with no row a row of its own.
    fn restore_values(&mut self, values: Vec<(String, String)>) {
        self.sync_grid_key();
        for row in &mut self.grid {
            let saved = values.iter().rev().find(|(name, _)| *name == row.name);
            row.value = saved.map(|(_, value)| value.clone()).unwrap_or_default();
        }
        for (name, value) in values {
            if !self.grid.iter().any(|row| row.name == name) {
                self.grid.push(StudioParamRow { name, value, ..StudioParamRow::default() });
            }
        }
    }

    /// Persist `self.saved.strategies` to disk, creating its directory, and answer whether it was
    /// written. A failure is kept in `StudioSavedPane::write_error`, which the Saved pane and the editor
    /// header show, and a success clears it. ⚠ It used to be SWALLOWED, into a directory that did not
    /// exist on a thin client: Save cleared "unsaved", the row showed, and a restart lost it.
    ///
    /// ⚠ A file the pane REFUSED to read (`crate::panes::saved::load_strategies`) is never written:
    /// the list in memory is empty, so a write would erase every row the file still holds.
    pub(super) fn persist_saved(&mut self) -> bool {
        if self.saved.refused.is_some() {
            return false;
        }
        let path = self.saved_strategies_path();
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| save_strategies(&path, &self.saved.strategies));
        self.saved.write_error =
            written.as_ref().err().map(|e| format!("Not saved to {}: {e}", path.display()));
        written.is_ok()
    }

    /// What a workspace write would record right now — the ONE spelling of the snapshot, used both
    /// by the per-frame persist below and by the constructor (which seeds `workspace_saver`
    /// with it), so the two cannot describe the state differently and trigger a spurious write.
    pub(super) fn workspace_snapshot(&self) -> StudioWorkspace {
        StudioWorkspace {
            right_tab: self.right_tab,
            editor_collapsed: self.editor_collapsed,
            tools_collapsed: self.tools_collapsed,
            template_index: self.template_index,
            editor_source: self.editor.source.clone(),
            strategy_source: self.strategy_source,
            picked_file: self.picked_file.clone(),
            file_params: self.file_params.clone(),
            rust_file: self.rust_file.clone(),
            editor_width: Some(self.editor_width),
            param_rows: {
                let mut rows = self.param_grids.clone();
                rows.insert(self.grid_key.clone(), self.grid.clone());
                rows
            },
            test_conditions: self.conditions.clone(),
            buffer_is_rust: self.buffer_is_rust,
            parked_source: self.other_buffer.as_ref().map(|(source, _)| source.clone()),
        }
    }

    /// Hand this frame's workspace snapshot to the saver — called once per frame at the end of
    /// `ui()`. The comparison (a handful of scalars, the buffer and the per-source selection) runs
    /// every frame; a drift is written once it has held still for
    /// `crate::studio::workspace::WORKSPACE_SAVE_DEBOUNCE`, on a thread of its own, so typing in the
    /// editor writes once per pause and never on this thread. A frame is asked for when the saver
    /// next needs one: egui draws none unprompted, and the quiet period ends with no input. The file
    /// is the project state root's (`<project>/settings/state/studio_workspace.json`), NOT the
    /// store's, unlike `saved_strategies_path` (`crate::studio::workspace`'s module doc). A failed
    /// write stays pending and is shown in the toolbar (`toolbar_running_status`).
    pub(super) fn maybe_persist_workspace(&mut self, ctx: &egui::Context) {
        // A `VIKE_STUDIO_TAB` capture session never writes: any drift (even one editor
        // keystroke, or a strategy-source switch) would persist the FORCED state over the user's
        // real file.
        if self.qa_workspace_readonly {
            return;
        }
        let current = self.workspace_snapshot();
        if let Some(wait) = self.workspace_saver.tick(current, std::time::Instant::now()) {
            ctx.request_repaint_after(wait);
        }
    }

    /// **Write the workspace edits still pending, now, and wait for the write** — for the Studio
    /// window's close and the app's exit, after which no frame would write them. `Err` is the
    /// failed write's sentence for the caller to log (this crate links no logger); the edit stays
    /// pending. A capture session writes nothing, as in `maybe_persist_workspace`.
    pub fn flush_workspace(&mut self) -> Result<(), String> {
        if self.qa_workspace_readonly {
            return Ok(());
        }
        let current = self.workspace_snapshot();
        self.workspace_saver.flush(current)
    }

    /// Fold in a `StudioSavedAction` from `StudioSavedPane::ui` this frame. `Load`/`Delete`/`SaveCurrent`
    /// mutate `self.saved.strategies` and persist; `CompareAll` sends every saved strategy's
    /// backtest over the selected slice to the backtesthub on a worker thread
    /// (`spawn_backtesthub_compare` — the `panes/saved/mod.rs`-side twin of `start_run`'s dispatch), so a large
    /// saved list or slice never blocks the UI thread. No-op if a compare is already in flight.
    pub(super) fn handle_saved_action(&mut self, action: StudioSavedAction) {
        match action {
            StudioSavedAction::Load(i) => {
                if let Some(s) = self.saved.strategies.get(i).cloned() {
                    // A RHAI row's Load overwrites `editor.source` below — the fifth Rhai writer,
                    // same hazard `rhai_writer_blocked_reason` refuses for the template/copilot
                    // writers: while the buffer is Rust with no other copy (Strategy file mode), that
                    // write would destroy it. The UI already disables this row's Load button for
                    // that case (`panes/saved/mod.rs`'s `ui`); this is the second layer, so a caller that
                    // reaches this method any other way is covered too. A Native or Strategy file row
                    // never touches `editor.source`, so neither is blocked here.
                    if s.source == StudioStrategySource::Rhai
                        && self.rhai_writer_blocked_reason().is_some()
                    {
                        return;
                    }
                    // A NATIVE entry has no script: loading it must restore the strategy SOURCE
                    // (file path + params) instead of blanking the editor with its empty `code`. A
                    // Rust row brings the Rust buffer back (`switch_source`). Its costs and values come
                    // back too, so a Run after the Load runs what the row saved, as Compare all does.
                    self.switch_source(s.source);
                    let values = self.restore_costs(&s.params);
                    match s.source {
                        StudioStrategySource::Rhai => {
                            self.editor.source = s.code.clone();
                            self.saved_source = s.code.clone();
                            self.restore_values(values);
                        }
                        StudioStrategySource::File => {
                            self.picked_file = s.file.clone();
                            self.file_params = s.params.clone();
                            // A Load is a baseline, exactly like Rhai's `saved_source = s.code`
                            // above — the row just loaded IS what a save would re-capture.
                            self.file_saved =
                                Some((self.picked_file.clone(), self.file_params.clone()));
                            self.right_tab = StudioTab::Strategy;
                            self.tools_collapsed = false;
                        }
                        // The saved sha loads back too — a row saved AFTER a successful Build
                        // still names a real artifact.
                        //
                        // ⚠ `rust_built_source` is cleared rather than guessed at, and the
                        // clearing is what stops this path from lying in EITHER direction. A saved
                        // row carries a name and a sha, never the source (the design: what is
                        // persisted is a SHA, not a file), so this shell genuinely does not know
                        // what that artifact was built from. Leaving the previous build's source
                        // in place would make `run_blocked_reason` compare the reloaded sha
                        // against an unrelated buffer and block a perfectly good row; inventing
                        // one would claim knowledge nothing here has. `None` means "not built by
                        // this session", which is exactly true.
                        //
                        // The residual is stated rather than hidden: a row whose artifact has
                        // since been pruned, or whose source the author edited elsewhere, still
                        // loads — and the refusal for it is the LOADER's, on the server, naming
                        // the missing artifact. That is a refusal, not a silent wrong answer.
                        StudioStrategySource::Rust => {
                            self.rust_file = s.rust_file.clone();
                            self.rust_sha = s.rust_sha.clone();
                            self.rust_built_source = None;
                            self.right_tab = StudioTab::Strategy;
                            self.tools_collapsed = false;
                            self.restore_values(values);
                        }
                    }
                }
            }
            StudioSavedAction::Delete(i) => {
                if i < self.saved.strategies.len() {
                    // The list shown is the list on disk: a delete that was not written is undone.
                    let removed = self.saved.strategies.remove(i);
                    if !self.persist_saved() {
                        self.saved.strategies.insert(i, removed);
                    }
                }
            }
            StudioSavedAction::SaveCurrent => {
                let name = self.saved.save_name.trim().to_string();
                // A refused file keeps its rows on disk only while nothing is saved over it.
                if name.is_empty() || self.saved.refused.is_some() {
                    return;
                }
                // Snapshot whatever the CURRENT strategy source is (`current_entry`, the row a Run
                // would execute) — a native entry persists its registry name + param rows, a Rhai
                // entry its script, a Strategy file entry its name + sha — and, once it is WRITTEN,
                // re-baseline the unsaved-changes dot for what was saved. A failed write leaves the
                // list as it was and the dot on, and says why (`persist_saved`).
                let entry = self.current_entry(name.clone());
                let before = self.saved.strategies.clone();
                match self.saved.strategies.iter_mut().find(|s| s.name == name) {
                    Some(existing) => *existing = entry,
                    None => self.saved.strategies.push(entry),
                }
                if !self.persist_saved() {
                    self.saved.strategies = before;
                    return;
                }
                match self.strategy_source {
                    StudioStrategySource::Rhai => self.saved_source = self.editor.source.clone(),
                    StudioStrategySource::File => {
                        self.file_saved =
                            Some((self.picked_file.clone(), self.file_params.clone()));
                    }
                    // A Strategy file row stores a name and a sha, never the buffer — so the baseline moves
                    // to the source that sha was BUILT from, the only buffer this row can be said to
                    // have saved (`saved_source`'s doc). With no sha this session built, it does not
                    // move at all: nothing here has saved the buffer, and the chip saying so is true.
                    StudioStrategySource::Rust => {
                        if let Some(built) = &self.rust_built_source {
                            self.saved_source = built.clone();
                        }
                    }
                }
                // Remember the name so an empty-name Ctrl+S re-saves HERE instead of "untitled".
                self.last_saved_name = Some(name);
                self.saved.save_name.clear();
            }
            StudioSavedAction::CompareAll => {
                if self.compare_rx.is_some() {
                    return; // a compare is already in flight
                }
                self.saved.compare_error = None;
                self.saved.compare_rows = None;
                let Some(slice) = self.picker.selected() else {
                    self.saved.compare_error = Some("pick a data slice first".to_string());
                    return;
                };
                // Each saved row contributes its OWN spec, so a Rhai script and a native registry
                // strategy rank side by side in one comparison.
                let strategies: Vec<(String, StrategySpec)> =
                    self.saved.strategies.iter().map(|s| (s.name.clone(), s.spec())).collect();
                // Every row goes to the backtesthub exactly as a Run of it would — the Run's two
                // backends, the Run's two keys — and nothing is simulated in this process
                // (decision 0078): the compare table and a Run of a row price one run one way.
                let rx = match &self.backend {
                    StudioRunBackend::Source { address } => {
                        crate::backend::backtesthub::spawn_backtesthub_compare(
                            address.clone(),
                            self.hub_keys.clone(),
                            strategies,
                            slice,
                            self.run_engine(),
                        )
                    }
                    // A named run's request has no capital field (`NAMED_BACKEND_CAPITAL` says so).
                    StudioRunBackend::Named { address } => {
                        named_run::spawn_backtesthub_named_compare(
                            address.clone(),
                            self.hub_keys.clone(),
                            strategies,
                            slice,
                        )
                    }
                };
                self.compare_rx = Some(rx);
            }
        }
    }
}
