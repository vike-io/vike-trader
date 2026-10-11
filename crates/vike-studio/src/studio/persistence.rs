//! `StudioState`'s persistence: the saved-strategy list's and the workspace snapshot's paths and
//! writes (`persist_saved`, `workspace_snapshot`, `maybe_persist_workspace`), and the fold of a
//! Saved pane action (`handle_saved_action`) that mutates and persists that list.
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;

impl StudioState {
    /// Path the saved-strategy list is persisted to — colocated with the per-store state
    /// directory (the caller-supplied stand-in for the store's root).
    fn saved_strategies_path(&self) -> std::path::PathBuf {
        self.state_dir.join(SAVED_STRATEGIES_FILE)
    }

    /// Persist `self.saved.strategies` to disk. A write failure is swallowed (best-effort state,
    /// like `SlicePicker::refresh`'s own `unwrap_or_default` posture) — the in-memory list stays
    /// authoritative for the rest of the session even if the disk write fails.
    pub(super) fn persist_saved(&self) {
        let _ = save_strategies(&self.saved_strategies_path(), &self.saved.strategies);
    }

    /// What a workspace write would record right now — the ONE spelling of the snapshot, used both
    /// by the per-frame persist below and by the constructor (which seeds `persisted_workspace`
    /// with it), so the two cannot describe the state differently and trigger a spurious write.
    pub(super) fn workspace_snapshot(&self) -> StudioWorkspace {
        StudioWorkspace {
            right_tab: self.right_tab,
            editor_collapsed: self.editor_collapsed,
            tools_collapsed: self.tools_collapsed,
            template_idx: self.template_idx,
            editor_source: self.editor.source.clone(),
            strategy_source: self.strategy_source,
            native_strategy: self.native_name().to_string(),
            native_params: self.native_params.clone(),
            plugin_name: self.plugin_name.clone(),
        }
    }

    /// Persist the current workspace snapshot iff it drifted from `persisted_workspace` since
    /// the last check — called once per frame at the end of `ui()`. The equality comparison (a
    /// handful of scalars, the buffer and the per-source selection) runs every frame, but the disk
    /// write only happens on an actual change, so typing in the editor writes once per drift rather
    /// than once per frame. The file is the project state root's
    /// (`<project>/settings/state/studio_workspace.json`), NOT the store's, unlike
    /// `saved_strategies_path` (`crate::studio::workspace`'s module doc). A write failure, or no
    /// state directory to write to, is swallowed (best-effort UI state, like `persist_saved`).
    pub(super) fn maybe_persist_workspace(&mut self) {
        // A `VIKE_STUDIO_TAB` capture session never writes: any drift (even one editor
        // keystroke, or a strategy-source switch) would persist the FORCED state over the user's
        // real file.
        if self.qa_workspace_readonly {
            return;
        }
        let current = self.workspace_snapshot();
        if current != self.persisted_workspace {
            if let Ok(path) = workspace_write_path() {
                let _ = save_workspace(&path, &current);
            }
            self.persisted_workspace = current;
        }
    }

    /// Fold in a `SavedAction` from `SavedPane::ui` this frame. `Load`/`Delete`/`SaveCurrent`
    /// mutate `self.saved.strategies` and persist; `CompareAll` kicks off every saved strategy's
    /// backtest over the selected slice on a worker thread (`spawn_compare_all` — the
    /// `saved.rs`-side twin of `start_run`/`start_sweep`/`start_walkforward`), so a large saved
    /// list or slice never blocks the UI thread. No-op if a compare is already in flight.
    pub(super) fn handle_saved_action(&mut self, action: SavedAction) {
        match action {
            SavedAction::Load(i) => {
                if let Some(s) = self.saved.strategies.get(i) {
                    // A RHAI row's Load overwrites `editor.source` below — the fifth Rhai writer,
                    // same hazard `rhai_writer_blocked_reason` refuses for the template/copilot
                    // writers: while the buffer is Rust with no other copy (Plugin mode), that
                    // write would destroy it. The UI already disables this row's Load button for
                    // that case (`saved.rs`'s `ui`); this is the second layer, so a caller that
                    // reaches this method any other way is covered too. A Native or Plugin row
                    // never touches `editor.source`, so neither is blocked here.
                    if s.source == StrategySource::Rhai
                        && self.rhai_writer_blocked_reason().is_some()
                    {
                        return;
                    }
                    // A NATIVE entry has no script: loading it must restore the strategy SOURCE
                    // (registry name + params) instead of blanking the editor with its empty `code`.
                    self.strategy_source = s.source;
                    match s.source {
                        StrategySource::Rhai => {
                            self.editor.source = s.code.clone();
                            self.saved_source = s.code.clone();
                        }
                        StrategySource::Native => {
                            self.native_idx = native_strategies()
                                .iter()
                                .position(|n| *n == s.native)
                                .unwrap_or(self.native_idx);
                            self.native_params = s.params.clone();
                            // A Load is a baseline, exactly like Rhai's `saved_source = s.code`
                            // above — the row just loaded IS what a save would re-capture.
                            self.native_saved =
                                Some((self.native_name(), self.native_params.clone()));
                            self.right_tab = RightTab::Strategy;
                            self.tools_collapsed = false;
                        }
                        // The saved sha loads back too — a row saved AFTER a successful Build
                        // still names a real artifact.
                        //
                        // ⚠ `plugin_built_source` is cleared rather than guessed at, and the
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
                        StrategySource::Plugin => {
                            self.plugin_name = s.plugin_name.clone();
                            self.plugin_sha = s.plugin_sha.clone();
                            self.plugin_built_source = None;
                            self.right_tab = RightTab::Strategy;
                            self.tools_collapsed = false;
                        }
                    }
                }
            }
            SavedAction::Delete(i) => {
                if i < self.saved.strategies.len() {
                    self.saved.strategies.remove(i);
                    self.persist_saved();
                }
            }
            SavedAction::SaveCurrent => {
                let name = self.saved.save_name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                // Snapshot whatever the CURRENT strategy source is — a native entry persists its
                // registry name + param rows, a Rhai entry its script (and re-baselines the
                // unsaved-changes dot), a Plugin entry its name + sha (re-baselining to what that
                // sha was built from).
                let entry = match self.strategy_source {
                    StrategySource::Rhai => {
                        let code = self.editor.source.clone();
                        self.saved_source = code.clone();
                        SavedStrategy::rhai(name.clone(), code)
                    }
                    StrategySource::Native => {
                        self.native_saved = Some((self.native_name(), self.native_params.clone()));
                        SavedStrategy::native(
                            name.clone(),
                            self.native_name(),
                            self.native_params.clone(),
                        )
                    }
                    // A Plugin row stores a name and a sha, never the buffer — so the baseline moves
                    // to the source that sha was BUILT from, the only buffer this row can be said to
                    // have saved (`saved_source`'s doc). With no sha this session built, it does not
                    // move at all: nothing here has saved the buffer, and the chip saying so is true.
                    StrategySource::Plugin => {
                        if let Some(built) = &self.plugin_built_source {
                            self.saved_source = built.clone();
                        }
                        SavedStrategy::plugin(
                            name.clone(),
                            self.plugin_name.clone(),
                            self.plugin_sha.clone(),
                        )
                    }
                };
                match self.saved.strategies.iter_mut().find(|s| s.name == name) {
                    Some(existing) => *existing = entry,
                    None => self.saved.strategies.push(entry),
                }
                // Remember the name so an empty-name Ctrl+S re-saves HERE instead of "untitled".
                self.last_saved_name = Some(name);
                self.saved.save_name.clear();
                self.persist_saved();
            }
            SavedAction::CompareAll => {
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
                let store = self.store.clone();
                self.compare_rx = Some(spawn_compare_all(strategies, slice, store));
            }
        }
    }
}
