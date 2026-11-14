//! `StudioState`'s strategy-source methods: which strategy a Run executes (`native_name`,
//! `named_spec`, `current_spec`), why a run is refused (`run_blocked_reason`), what language the
//! editor buffer holds and who may write Rhai into it (`buffer_is_rhai`, `rhai_verdict`,
//! `rhai_writer_blocked_reason`, `load_template`, `apply_copilot_result`), and the unsaved-chip
//! logic (`unsaved_chip_tip`, `native_is_dirty`).
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;

impl StudioState {
    /// The strategy file currently selected in the Strategy pane's list, by its path under the
    /// server's folder (empty while none is picked; the server refuses an empty path in words).
    pub fn native_name(&self) -> &str {
        &self.native_file
    }

    /// What [`Backend::Named`] runs: the name picked off the SERVER's roster, with this pane's
    /// params rows.
    ///
    /// ⚠ **A SCRIPT is passed through unchanged rather than substituted**, deliberately. Quietly
    /// swapping in a native name would run something the operator did not choose and report it as
    /// the answer; instead the spec reaches `crate::backend::remote::to_named_run_spec`, which refuses it
    /// with the sentence that explains why a named run carries no source and what to do instead.
    ///
    /// With no name picked this falls back to [`Self::current_spec`], so the pane's own native
    /// selection still runs — which is right on a daemon whose roster overlaps this binary's.
    pub fn named_spec(&self) -> StrategySpec {
        match (&self.strategy_source, &self.named_strategy) {
            // A Strategy file joins Rhai here for the same reason: it is not on the SERVER's compiled-in
            // roster either (`crate::backend::remote::to_named_run_spec` refuses it by name, the same way it
            // refuses a script), so falling through to `current_spec` is the honest answer rather
            // than inventing a substitution the daemon cannot resolve.
            (StrategySource::Rhai, _)
            | (StrategySource::Rust, _)
            | (StrategySource::Native, None) => self.current_spec(),
            (StrategySource::Native, Some(name)) => {
                SavedStrategy::native(String::new(), name.clone(), self.native_params.clone())
                    .spec()
            }
        }
    }

    /// The strategy the panes hold right now as a saved row named `name` — the ONE place
    /// `strategy_source` becomes a strategy definition. [`Self::current_spec`] is this row's
    /// `SavedStrategy::spec` and a Save stores this row, so what a Run executes and what a Save
    /// keeps (and a later Compare-all runs) are one reading of the panes, not two builders that
    /// could disagree — they did, on a Strategy file's params.
    ///
    /// A Strategy file row carries no param rows: there is no params editor for Strategy file mode yet, so it
    /// resolves an EMPTY params table, a valid empty TOML document on the strategy file's side of the
    /// C-ABI — a strategy file runs on its own `build` fallbacks, which the template's params test pins.
    pub(super) fn current_entry(&self, name: String) -> SavedStrategy {
        match self.strategy_source {
            StrategySource::Rhai => SavedStrategy::rhai(name, self.editor.source.clone()),
            StrategySource::Native => {
                SavedStrategy::native(name, self.native_name(), self.native_params.clone())
            }
            StrategySource::Rust => {
                SavedStrategy::strategy_file(name, self.rust_file.clone(), self.rust_sha.clone())
            }
        }
    }

    /// What every Run/Sweep/Walk-Forward executes right now: [`Self::current_entry`]'s spec.
    pub fn current_spec(&self) -> StrategySpec {
        self.current_entry(String::new()).spec()
    }

    /// Why Run/Sweep/Walk-Forward are disabled right now, if they are — `None` means "go ahead".
    ///
    /// The first reason this shell's plumbing adds: a [`StrategySource::Rust`] run needs the file
    /// built, and running with `rust_sha` still `None` would ask the server for a build that never
    /// happened — this refusal stops that request before it is sent.
    /// ⚠ **The second reason is STALENESS, and it is the other half of what makes a sha honest.**
    /// A Build binds the run to the artifact that was built; an edit afterwards leaves the sha
    /// naming the previous one, so a Run would report on code the author had already replaced —
    /// with no error and entirely plausible numbers. Only a sha THIS session built is checked
    /// ([`Self::strategy_file_built_source`] says why a reloaded one is not).
    pub fn run_blocked_reason(&self) -> Option<&'static str> {
        // The Named backend runs the roster name it picked instead (`named_spec`).
        let named_pick =
            matches!(self.backend, Backend::Named { .. }) && self.named_strategy.is_some();
        if self.strategy_source == StrategySource::Native
            && self.native_file.is_empty()
            && !named_pick
        {
            return Some("Pick a strategy file in the Strategy tab first.");
        }
        if self.strategy_source != StrategySource::Rust {
            return None;
        }
        if self.rust_sha.is_none() {
            return Some(
                "Build the strategy first — Run needs a sha, and no Build has returned one yet.",
            );
        }
        if self.strategy_file_built_source.as_deref().is_some_and(|src| src != self.editor.source) {
            return Some(
                "The editor changed since the last Build — Build again, or the run would report \
                 on the previous artifact.",
            );
        }
        // ⚠ The third reason: Run runs the FILE on the server (a hub loads no artifact by sha), so
        // a buffer that is not that file as it was opened would report on the server's text, not
        // on the code in front of the author.
        let holds_the_file = self
            .opened_rust_file
            .as_ref()
            .is_some_and(|(file, text)| *file == self.rust_file && *text == self.editor.source);
        if !holds_the_file {
            return Some(RUST_RUN_NEEDS_THE_FILE);
        }
        None
    }

    /// Whether the editor buffer is RHAI source right now — the one question the Rhai compile
    /// check, the editor header's `● compiles` / `● error · line N` chip and the inline error
    /// banner under it all answer. `false` means none of the three may speak about the buffer.
    ///
    /// - [`StrategySource::Rhai`]: yes — it is the script every Run executes.
    /// - [`StrategySource::Rust`]: **no.** The buffer is RUST that Build hands to the builder
    ///   service, so a Rhai parse of it is meaningless — measured on the real GUI, a valid strategy file
    ///   read `● error · line 4` beside the `● built <sha>` of its own successful Build. The Build
    ///   result in the Strategy pane is the verdict on this buffer.
    /// - [`StrategySource::Native`]: yes, and deliberately. Native mode never WRITES the editor —
    ///   the registry dropdown, the param rows and a native saved row's Load all leave it alone —
    ///   so what it holds is the parked Rhai script that switching back to Rhai will run, and the
    ///   verdict is a true statement about Rhai source. The one way Rust gets there is typing it in
    ///   Strategy file mode and then picking Native; the red chip that follows is then the verdict Rhai
    ///   mode WOULD return on that buffer, not a claim about what Native runs.
    ///
    /// An exhaustive `match` rather than `!= StrategyFile`, so a fourth source is a compile error here
    /// until somebody decides what language its buffer is.
    pub fn buffer_is_rhai(&self) -> bool {
        match self.strategy_source {
            StrategySource::Rhai | StrategySource::Native => true,
            StrategySource::Rust => false,
        }
    }

    /// The Rhai verdict the editor header renders: `None` when no verdict is held — which is
    /// always the case while [`Self::buffer_is_rhai`] is `false` — else `compile_status`'s answer
    /// for the buffer as of the last [`Self::poll`].
    pub fn rhai_verdict(&self) -> Option<&Result<(), vike_rhai::ScriptError>> {
        self.rhai_check.as_ref().map(|(_, verdict)| verdict)
    }

    /// Why a control that writes a RHAI script into the editor buffer is refused right now, if it
    /// is — `None` means "go ahead". There are four such writers: the Sweep pane's template
    /// `Load`, `Browse templates`' per-card `Load`, the empty results panel's `Load a template`,
    /// and the AI Copilot's `Apply to editor`. Each renders disabled on this answer with it as the
    /// hover text, and [`Self::load_template`] / [`Self::apply_copilot_result`] ask it AGAIN inside
    /// the write — the shape `start_run` gives `run_blocked_reason`: the button's state protects the
    /// button, the check inside the write protects every caller.
    ///
    /// ⚠ **Refused, rather than made to switch the source to Rhai as it loads, and the difference is
    /// the author's code.** While the buffer is not Rhai ([`Self::buffer_is_rhai`] — Strategy file mode) it
    /// is RUST, and it has no other copy: a Strategy file save stores the strategy file's name and its Build's
    /// sha, never the source (`SavedStrategy::strategy_file`). A writer that flipped the mode and loaded
    /// would destroy the only copy of a strategy file in one click, and one that loaded WITHOUT flipping
    /// would leave Strategy file selected over a Rhai buffer that Build then hands to cargo. Refused, it
    /// costs one deliberate click — pick `Rhai script`, where the same buffer wears the Rhai verdict
    /// and a load is the ordinary Rhai-mode overwrite — and destroys nothing by itself.
    ///
    /// Tied to `buffer_is_rhai` rather than spelled as its own match, because it IS that question:
    /// a Rhai writer may write exactly when the buffer is Rhai. Native passes for the reason that
    /// function gives — its buffer is the parked Rhai script.
    pub fn rhai_writer_blocked_reason(&self) -> Option<&'static str> {
        (!self.buffer_is_rhai()).then_some(RHAI_WRITER_BLOCKED_IN_STRATEGY_FILE)
    }

    /// Put a template's `code` into the editor and re-baseline the unsaved chip (a template load is
    /// a LOAD, not an edit) — or refuse and return `false` while
    /// [`Self::rhai_writer_blocked_reason`] says so, leaving the buffer untouched.
    pub fn load_template(&mut self, code: &str) -> bool {
        if self.rhai_writer_blocked_reason().is_some() {
            return false;
        }
        self.editor.source = code.to_string();
        self.saved_source = self.editor.source.clone();
        true
    }

    /// The AI Copilot's `Apply to editor`: write the script it generated into the buffer — an EDIT,
    /// so the unsaved chip is left to show it — or refuse and return `false` while
    /// [`Self::rhai_writer_blocked_reason`] says so. The copilot writes Rhai
    /// (`vike_ai::develop_strategy_with_ledger` backtests what it wrote as a script).
    pub fn apply_copilot_result(&mut self, result: &vike_ai::AgentResult) -> bool {
        if self.rhai_writer_blocked_reason().is_some() {
            return false;
        }
        crate::panes::chat::apply_result(&mut self.editor.source, result);
        true
    }

    /// The editor header's `● unsaved` chip hover text: what Ctrl+S would actually do about the
    /// drift the chip is reporting.
    ///
    /// ⚠ **In Strategy file mode it said "Ctrl+S saves to the Saved list", and that was false twice.** A
    /// Strategy file save stores the name and a sha, never the buffer, and it did not re-baseline either —
    /// so the chip never cleared, and the tooltip promised a save that could not capture the edits
    /// it was pointing at. Now a Strategy file save re-baselines to what its sha was built from (see
    /// [`Self::saved_source`]), and this names which of the two situations the author is in: the
    /// buffer IS the held sha's source (a save names this code), or it is not (no Build, an edit
    /// since, or a sha reloaded from a saved row) and only a Build can make a save capture it.
    ///
    /// ⚠ **Native used to keep the Rhai sentence, and that was the same defect in the third mode.**
    /// A Native save stores the registry name and params, never the parked buffer, so "Ctrl+S saves
    /// to the Saved list" named the wrong payload — fixed the same way as Strategy file, by naming what a
    /// save actually captures.
    pub fn unsaved_chip_tip(&self) -> &'static str {
        match self.strategy_source {
            StrategySource::Rhai => UNSAVED_TIP,
            StrategySource::Native => UNSAVED_TIP_NATIVE,
            StrategySource::Rust => {
                let buffer_is_built = self.rust_sha.is_some()
                    && self.strategy_file_built_source.as_deref()
                        == Some(self.editor.source.as_str());
                if buffer_is_built {
                    UNSAVED_TIP_STRATEGY_FILE_BUILT
                } else {
                    UNSAVED_TIP_STRATEGY_FILE_UNBUILT
                }
            }
        }
    }

    /// [`StrategySource::Native`]'s own dirty check — see [`Self::native_saved`]'s doc for why it
    /// cannot be `editor.source != saved_source` the way the other two modes' can.
    pub(super) fn native_is_dirty(&self) -> bool {
        self.native_saved.as_ref().map(|(file, params)| (file.as_str(), params.as_slice()))
            != Some((self.native_name(), self.native_params.as_slice()))
    }

    /// Why Open (a file of the served list into the editor) is refused right now, if it is.
    ///
    /// ⚠ **Open replaces the buffer, and a Rust buffer may have no other copy**: a Rust save stores
    /// a path and a sha, never the source ([`Self::rhai_writer_blocked_reason`] argues the same for
    /// the Rhai writers). So while the buffer is Rust and neither its last load nor its last Build
    /// holds it, Open waits; a Rhai buffer is overwritten the way a template load overwrites it.
    pub fn open_blocked_reason(&self) -> Option<&'static str> {
        if self.open_rx.is_some() {
            return Some("A file is opening.");
        }
        let unkept = self.strategy_source == StrategySource::Rust
            && self.editor.source != self.saved_source
            && self.strategy_file_built_source.as_deref() != Some(self.editor.source.as_str());
        unkept.then_some(
            "The editor holds Rust edits that no file and no Build holds: Build them, or undo \
             them, before opening another file.",
        )
    }

    /// The server file a Save writes the editor buffer to: [`Self::rust_file`] in Rust mode, the
    /// Rhai buffer's own [`Self::rhai_file`] otherwise.
    pub fn save_target(&self) -> &str {
        match self.strategy_source {
            StrategySource::Rust => &self.rust_file,
            StrategySource::Rhai | StrategySource::Native => &self.rhai_file,
        }
    }

    /// Why Save (the editor buffer into the server's `strategies/` folder) is refused right now, if
    /// it is. The server judges the path itself; these are the refusals the shell can see first.
    pub fn save_blocked_reason(&self) -> Option<&'static str> {
        let file = self.save_target().trim();
        let extension = std::path::Path::new(file).extension().and_then(|e| e.to_str());
        if self.save_file_rx.is_some() {
            Some("A save is running.")
        } else if self.strategy_source == StrategySource::Native {
            Some("Save writes the editor buffer: pick My strategy (Rhai) or (Rust) first.")
        } else if file.is_empty() {
            Some("Name the file first.")
        } else if self.strategy_source == StrategySource::Rust
            && !extension.is_some_and(|e| e.eq_ignore_ascii_case("rs"))
        {
            Some("A Rust buffer saves as a .rs file.")
        } else if self.strategy_source == StrategySource::Rhai
            && !extension.is_some_and(|e| e.eq_ignore_ascii_case("rhai"))
        {
            Some("A Rhai buffer saves as a .rhai file.")
        } else {
            None
        }
    }

    /// A Save has answered: the server's file now holds the text SENT. A Rust save makes that text
    /// the file a Run runs ([`Self::opened_rust_file`]); the list is fetched again to show it.
    pub(super) fn saved_file(&mut self, file: String, sent: String, sha256: &str, state: &str) {
        let shown = if state.is_empty() { "Rhai" } else { state };
        self.save_file_last = Some(Ok(format!(
            "saved {file} · sha256 {} · {shown}",
            sha256.get(..8).unwrap_or(sha256)
        )));
        if self.strategy_source == StrategySource::Rust && file == self.rust_file {
            self.opened_rust_file = Some((file, sent));
        }
        self.start_strategy_files_fetch();
    }

    /// A file's text has arrived: put it in the editor as a LOAD (the unsaved chip re-baselines)
    /// and pick the mode its extension says — a `.rhai` file is a Rhai buffer, a `.rs` file a Rust
    /// buffer built as that path, not built yet.
    pub(super) fn open_file_text(&mut self, file: &str, text: String) {
        self.editor.source = text;
        self.saved_source = self.editor.source.clone();
        let rust =
            std::path::Path::new(file).extension().is_some_and(|e| e.eq_ignore_ascii_case("rs"));
        if rust {
            self.strategy_source = StrategySource::Rust;
            self.rust_file = file.to_string();
            self.rust_sha = None;
            self.strategy_file_built_source = None;
            self.opened_rust_file = Some((file.to_string(), self.editor.source.clone()));
        } else {
            self.strategy_source = StrategySource::Rhai;
            self.rhai_file = file.to_string();
        }
    }
}
