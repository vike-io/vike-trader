//! `StudioState`'s strategy-source methods: which strategy a Run executes (`named_spec`,
//! `current_spec`), why a run is refused (`run_blocked_reason`), what language the
//! editor buffer holds and who may write Rhai into it (`buffer_is_rhai`, `rhai_verdict`,
//! `rhai_writer_blocked_reason`, `load_template`, `apply_copilot_result`), and the unsaved-chip
//! logic (`unsaved_chip_tip`, `file_is_dirty`).
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;

impl StudioState {
    /// What [`StudioRunBackend::Named`] runs: the name picked off the SERVER's roster, with this pane's
    /// params rows.
    ///
    /// ⚠ **A SCRIPT is passed through unchanged rather than substituted**, deliberately. Quietly
    /// swapping in a native name would run something the operator did not choose and report it as
    /// the answer; instead the spec reaches `crate::backend::backtesthub::named_run::to_named_run_spec`, which refuses it
    /// with the sentence that explains why a named run carries no source and what to do instead.
    ///
    /// With no name picked this falls back to [`Self::current_spec`], so the pane's own native
    /// selection still runs — which is right on a daemon whose roster overlaps this binary's.
    pub(crate) fn named_spec(&self) -> StrategySpec {
        match (&self.strategy_source, &self.named_strategy) {
            // A Strategy file joins Rhai here for the same reason: it is not on the SERVER's compiled-in
            // roster either (`crate::backend::backtesthub::named_run::to_named_run_spec` refuses it by name, the same way it
            // refuses a script), so falling through to `current_spec` is the honest answer rather
            // than inventing a substitution the daemon cannot resolve.
            (StudioStrategySource::Rhai, _)
            | (StudioStrategySource::Rust, _)
            | (StudioStrategySource::File, None) => self.current_spec(),
            // The SDK pick rides along, so the named run refuses it by name rather than dropping it. The
            // params, the test conditions' costs among them, are `current_entry`'s.
            (StudioStrategySource::File, Some(name)) => {
                StudioSavedStrategy { file: name.clone(), ..self.current_entry(String::new()) }
                    .spec()
                    .with_sdk(self.picked_sdk())
            }
        }
    }

    /// The strategy the panes hold right now as a saved row named `name` — the ONE place
    /// `strategy_source` becomes a strategy definition. [`Self::current_spec`] is this row's
    /// `StudioSavedStrategy::spec` and a Save stores this row, so what a Run executes and what a Save
    /// keeps (and a later Compare-all runs) are one reading of the panes, not two builders that
    /// could disagree — they did, on a Strategy file's params.
    ///
    /// A row's params (a strategy file's are [`Self::file_params`]; a Rust file's and a Rhai script's the Param
    /// scan pane's values) end with the test conditions' costs, so a cost set there wins over a params row of the
    /// same key (`params_from_rows` keeps the last) and a saved row, a Compare-all row and a Named run carry them
    /// as a Run does. A Rhai row's run them through its `param()`s (`StrategySpec::RhaiWithParameters`); a scan and
    /// a walk-forward also hold them as one-value axes (`Self::held_rows`), which a server predating a Rhai
    /// buffer's parameters still reads.
    pub(super) fn current_entry(&self, name: String) -> StudioSavedStrategy {
        let costs = self.conditions.costs.iter().filter(|r| !r.value.trim().is_empty());
        let costs = costs.map(|r| (r.name.clone(), r.value.trim().to_string()));
        // The Param scan pane's values: a Rust file's params (its header names them) and a Rhai script's
        // (its `param()`s read them; a Run sends them as `RhaiWithParameters`).
        let values = self.grid.iter().filter(|r| !r.value.trim().is_empty());
        let values = values.map(|r| (r.name.clone(), r.value.trim().to_string()));
        match self.strategy_source {
            StudioStrategySource::Rhai => StudioSavedStrategy {
                params: values.chain(costs).collect(),
                ..StudioSavedStrategy::rhai(name, self.editor.source.clone())
            },
            StudioStrategySource::File => StudioSavedStrategy::file(
                name,
                self.picked_file.as_str(),
                self.file_params.iter().cloned().chain(costs).collect(),
            ),
            StudioStrategySource::Rust => StudioSavedStrategy {
                params: values.chain(costs).collect(),
                ..StudioSavedStrategy::rust(name, self.rust_file.clone(), self.rust_sha.clone())
            },
        }
    }

    /// What every Run/Sweep/Walk-Forward executes right now: `Self::current_entry`'s spec, its file built
    /// against the toolbar's SDK pick ([`Self::picked_sdk`]).
    pub fn current_spec(&self) -> StrategySpec {
        self.current_entry(String::new()).spec().with_sdk(self.picked_sdk())
    }

    /// The toolbar's SDK pick ([`Self::sdk_pick`]): the SDK number, or `None` for "as the file says".
    /// Read only for a source that runs a file: a Rhai editor buffer runs on this vike's own code and
    /// has no SDK.
    pub(crate) fn picked_sdk(&self) -> Option<u32> {
        self.sdk_pick.filter(|_| self.strategy_source != StudioStrategySource::Rhai)
    }

    /// Why Run/Sweep/Walk-Forward are disabled right now, if they are — `None` means "go ahead".
    ///
    /// The first reason this shell's plumbing adds: a [`StudioStrategySource::Rust`] run needs the file
    /// built, and running with `rust_sha` still `None` would ask the server for a build that never
    /// happened — this refusal stops that request before it is sent.
    /// ⚠ **The second reason is STALENESS, and it is the other half of what makes a sha honest.**
    /// A Build binds the run to the artifact that was built; an edit afterwards leaves the sha
    /// naming the previous one, so a Run would report on code the author had already replaced —
    /// with no error and entirely plausible numbers. Only a sha THIS session built is checked
    /// ([`Self::rust_built_source`] says why a reloaded one is not).
    pub(crate) fn run_blocked_reason(&self) -> Option<&'static str> {
        // The Named backend runs the roster name it picked instead (`named_spec`).
        let named_pick =
            matches!(self.backend, StudioRunBackend::Named { .. }) && self.named_strategy.is_some();
        if self.strategy_source == StudioStrategySource::File
            && self.picked_file.is_empty()
            && !named_pick
        {
            return Some("Pick a strategy file in the Strategy tab first.");
        }
        if self.strategy_source != StudioStrategySource::Rust {
            return None;
        }
        if self.rust_sha.is_none() {
            return Some(
                "Build the strategy first — Run needs a sha, and no Build has returned one yet.",
            );
        }
        if self.rust_built_source.as_deref().is_some_and(|src| src != self.editor.source) {
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
    /// - [`StudioStrategySource::Rhai`]: yes — it is the script every Run executes.
    /// - [`StudioStrategySource::Rust`]: **no.** The buffer is RUST that Build hands to the builder
    ///   service, so a Rhai parse of it is meaningless — measured on the real GUI, a valid strategy file
    ///   read `● error · line 4` beside the `● built <sha>` of its own successful Build. The Build
    ///   result in the Strategy pane is the verdict on this buffer.
    /// - [`StudioStrategySource::File`]: the language of the buffer it PARKS. Strategy files never
    ///   writes the editor — the file list, the param rows and a file row's Load all leave it alone —
    ///   so the buffer is what the last editing mode left: Rhai after Rhai, Rust after my Rust
    ///   strategy ([`Self::buffer_is_rust`], kept by [`Self::poll`]). ⚠ It answered "Rhai" always,
    ///   and the live desktop showed the cost: open a `.rs` file, pick Strategy files, and the
    ///   editor wore `● error · line 21 · 'use' is a reserved keyword` over valid Rust.
    ///
    /// An exhaustive `match` rather than `!= StrategyFile`, so a fourth source is a compile error here
    /// until somebody decides what language its buffer is.
    pub(crate) fn buffer_is_rhai(&self) -> bool {
        match self.strategy_source {
            StudioStrategySource::Rhai => true,
            StudioStrategySource::File => !self.buffer_is_rust,
            StudioStrategySource::Rust => false,
        }
    }

    /// Record the buffer's language from the mode that edits it: Rhai or my Rust strategy say it,
    /// Strategy files keeps what was there. Called first in every [`Self::poll`].
    pub(super) fn note_buffer_language(&mut self) {
        match self.strategy_source {
            StudioStrategySource::Rhai => self.buffer_is_rust = false,
            StudioStrategySource::Rust => self.buffer_is_rust = true,
            StudioStrategySource::File => {}
        }
    }

    /// The sha the editor header names as built: the Strategy pane's own `● built` rule in my Rust
    /// strategy (a sha, and nothing blocks Run), and in Strategy files the sha whose Build compiled
    /// exactly the parked buffer. `None` otherwise: the header then says "Build to check".
    pub(crate) fn editor_built_sha(&self) -> Option<&str> {
        let sha = self.rust_sha.as_deref()?;
        let built = match self.strategy_source {
            StudioStrategySource::Rust => self.run_blocked_reason().is_none(),
            StudioStrategySource::File => {
                self.rust_built_source.as_deref() == Some(self.editor.source.as_str())
            }
            StudioStrategySource::Rhai => false,
        };
        built.then_some(sha)
    }

    /// The `[strategy] sdk` pin of the Rust strategy in the editor, which the SDK source view resolves
    /// (decision 0296, verdict 2); `None` for a Rhai buffer, which runs on the server's own code.
    pub(crate) fn strategy_sdk_pin(&self) -> Option<u32> {
        if self.buffer_is_rhai() {
            return None;
        }
        crate::panes::sdk_source::strategy_pin(&self.editor.source)
    }

    /// The Rhai verdict the editor header renders: `None` when no verdict is held — which is
    /// always the case while [`Self::buffer_is_rhai`] is `false` — else `compile_status`'s answer
    /// for the buffer as of the last [`Self::poll`].
    pub(crate) fn rhai_verdict(&self) -> Option<&Result<(), vike_rhai::ScriptError>> {
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
    /// sha, never the source (`StudioSavedStrategy::rust`). A writer that flipped the mode and loaded
    /// would destroy the only copy of a strategy file in one click, and one that loaded WITHOUT flipping
    /// would leave Strategy file selected over a Rhai buffer that Build then hands to cargo. Refused, it
    /// costs one deliberate click — pick `Rhai script`, where the same buffer wears the Rhai verdict
    /// and a load is the ordinary Rhai-mode overwrite — and destroys nothing by itself.
    ///
    /// Tied to `buffer_is_rhai` rather than spelled as its own match, because it IS that question:
    /// a Rhai writer may write exactly when the buffer is Rhai. Native passes for the reason that
    /// function gives — its buffer is the parked Rhai script.
    pub(crate) fn rhai_writer_blocked_reason(&self) -> Option<&'static str> {
        (!self.buffer_is_rhai()).then_some(RHAI_WRITER_BLOCKED_IN_RUST)
    }

    /// Put a template's `code` into the editor and re-baseline the unsaved chip (a template load is
    /// a LOAD, not an edit) — or refuse and return `false` while
    /// [`Self::rhai_writer_blocked_reason`] says so, leaving the buffer untouched.
    ///
    /// Over UNSAVED changes it writes nothing yet: the template waits in [`Self::pending_template`]
    /// and the editor asks (its "load it over my changes" is [`Self::write_template`]), so one click
    /// on a loader no longer throws an edit away. Every loader comes through here, so every one asks.
    pub(crate) fn load_template(&mut self, code: &str) -> bool {
        if self.rhai_writer_blocked_reason().is_some() {
            return false;
        }
        if self.editor.source != self.saved_source && self.editor.source != code {
            self.pending_template = Some(code.to_string());
            // The question is in the editor's header: show it.
            self.editor_collapsed = false;
            return true;
        }
        self.write_template(code);
        true
    }

    /// The template write itself: the buffer becomes `code` and the unsaved baseline with it, and
    /// nothing waits any more.
    pub(crate) fn write_template(&mut self, code: &str) {
        self.editor.source = code.to_string();
        self.saved_source = self.editor.source.clone();
        self.pending_template = None;
    }

    /// The AI Copilot's `Apply to editor`: write the script it generated into the buffer — an EDIT,
    /// so the unsaved chip is left to show it — or refuse and return `false` while
    /// [`Self::rhai_writer_blocked_reason`] says so. The copilot writes Rhai
    /// (`vike_ai::develop_strategy_with_ledger` backtests what it wrote as a script).
    pub(crate) fn apply_copilot_result(&mut self, result: &vike_ai::AgentResult) -> bool {
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
    pub(crate) fn unsaved_chip_tip(&self) -> &'static str {
        match self.strategy_source {
            StudioStrategySource::Rhai => UNSAVED_TIP,
            StudioStrategySource::File => UNSAVED_TIP_FILE,
            StudioStrategySource::Rust => {
                let buffer_is_built = self.rust_sha.is_some()
                    && self.rust_built_source.as_deref() == Some(self.editor.source.as_str());
                if buffer_is_built { UNSAVED_TIP_RUST_BUILT } else { UNSAVED_TIP_RUST_UNBUILT }
            }
        }
    }

    /// [`StudioStrategySource::File`]'s own dirty check — see [`Self::file_saved`]'s doc for why it
    /// cannot be `editor.source != saved_source` the way the other two modes' can.
    pub(super) fn file_is_dirty(&self) -> bool {
        self.file_saved.as_ref().map(|(file, params)| (file.as_str(), params.as_slice()))
            != Some((self.picked_file.as_str(), self.file_params.as_slice()))
    }

    /// Why Open (a file of the served list into the editor) is refused right now, if it is.
    ///
    /// ⚠ **Open replaces the buffer, and a Rust buffer may have no other copy**: a Rust save stores
    /// a path and a sha, never the source ([`Self::rhai_writer_blocked_reason`] argues the same for
    /// the Rhai writers). So while the buffer is Rust and neither its last load nor its last Build
    /// holds it, Open waits; a Rhai buffer is overwritten the way a template load overwrites it.
    pub(crate) fn open_blocked_reason(&self) -> Option<&'static str> {
        if self.open_rx.is_some() {
            return Some("A file is opening.");
        }
        let unkept = self.strategy_source == StudioStrategySource::Rust
            && self.editor.source != self.saved_source
            && self.rust_built_source.as_deref() != Some(self.editor.source.as_str());
        unkept.then_some(
            "The editor holds Rust edits that no file and no Build holds: Build them, or undo \
             them, before opening another file.",
        )
    }

    /// The server file a Save writes the editor buffer to: [`Self::rust_file`] in Rust mode, the
    /// Rhai buffer's own [`Self::rhai_file`] otherwise.
    pub(crate) fn save_target(&self) -> &str {
        match self.strategy_source {
            StudioStrategySource::Rust => &self.rust_file,
            StudioStrategySource::Rhai | StudioStrategySource::File => &self.rhai_file,
        }
    }

    /// Why Save (the editor buffer into the server's `strategies/` folder) is refused right now, if
    /// it is. The server judges the path itself; these are the refusals the shell can see first.
    pub(crate) fn save_blocked_reason(&self) -> Option<&'static str> {
        let file = self.save_target().trim();
        let extension = std::path::Path::new(file).extension().and_then(|e| e.to_str());
        if self.save_file_rx.is_some() {
            Some("A save is running.")
        } else if self.strategy_source == StudioStrategySource::File {
            Some("Save writes the editor buffer: pick My strategy (Rhai) or (Rust) first.")
        } else if file.is_empty() {
            Some("Name the file first.")
        } else if self.strategy_source == StudioStrategySource::Rust
            && !extension.is_some_and(|e| e.eq_ignore_ascii_case("rs"))
        {
            Some("A Rust buffer saves as a .rs file.")
        } else if self.strategy_source == StudioStrategySource::Rhai
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
        if self.strategy_source == StudioStrategySource::Rust && file == self.rust_file {
            self.opened_rust_file = Some((file, sent));
        }
        self.start_strategy_files_fetch();
    }

    /// Switch the strategy source, the editor showing that language's OWN buffer: Rhai and Rust keep a
    /// text each with its unsaved baseline, and a switch between them parks the one on screen
    /// ([`Self::other_buffer`]) and brings the other back (the shipped Rhai script, or an empty Rust
    /// buffer, the first time). Strategy files shows the buffer it found. ⚠ One shared buffer showed a
    /// `.rs` file in Rhai mode and in Strategy files under `● error · 'use' is a reserved keyword`.
    pub(crate) fn switch_source(&mut self, to: StudioStrategySource) {
        let rust = match to {
            StudioStrategySource::Rhai => false,
            StudioStrategySource::Rust => true,
            StudioStrategySource::File => self.buffer_is_rust,
        };
        if rust != self.buffer_is_rust {
            let shown =
                (std::mem::take(&mut self.editor.source), std::mem::take(&mut self.saved_source));
            let (source, saved) = self.other_buffer.replace(shown).unwrap_or_else(|| {
                let text = match rust {
                    true => String::new(),
                    false => crate::panes::editor::StudioEditorPane::default().source,
                };
                (text.clone(), text)
            });
            self.editor.source = source;
            self.saved_source = saved;
            self.buffer_is_rust = rust;
        }
        self.strategy_source = to;
    }

    /// A server file's text has arrived from Open: open it ([`Self::write_open`]), or, when the buffer it
    /// would replace has unsaved changes, hold it in [`Self::pending_open`] and let the editor ask, as a
    /// template load over unsaved changes does.
    pub(super) fn open_file_text(&mut self, file: &str, text: String) {
        let rust = is_rust_file(file);
        let (source, saved) = match rust == self.buffer_is_rust {
            true => (self.editor.source.as_str(), self.saved_source.as_str()),
            false => match &self.other_buffer {
                Some((source, saved)) => (source.as_str(), saved.as_str()),
                None => ("", ""),
            },
        };
        if source != saved && source != text {
            self.pending_open = Some((file.to_string(), text));
            self.editor_collapsed = false;
            return;
        }
        self.write_open(file, text);
    }

    /// Open's write itself: the file's text becomes its language's buffer as a LOAD (the unsaved chip
    /// re-baselines), the buffer of the other language parked, and the mode is the one its extension
    /// says — a `.rhai` file is a Rhai buffer, a `.rs` file a Rust buffer built as that path, not built
    /// yet.
    pub(crate) fn write_open(&mut self, file: &str, text: String) {
        self.pending_open = None;
        let rust = is_rust_file(file);
        if rust != self.buffer_is_rust {
            let shown =
                (std::mem::take(&mut self.editor.source), std::mem::take(&mut self.saved_source));
            self.other_buffer = Some(shown);
            self.buffer_is_rust = rust;
        }
        self.editor.source = text;
        self.saved_source = self.editor.source.clone();
        if rust {
            self.strategy_source = StudioStrategySource::Rust;
            self.rust_file = file.to_string();
            self.rust_sha = None;
            self.rust_built_source = None;
            self.opened_rust_file = Some((file.to_string(), self.editor.source.clone()));
        } else {
            self.strategy_source = StudioStrategySource::Rhai;
            self.rhai_file = file.to_string();
        }
    }
}

/// Whether `file` is a Rust strategy file, by its extension.
fn is_rust_file(file: &str) -> bool {
    std::path::Path::new(file).extension().is_some_and(|e| e.eq_ignore_ascii_case("rs"))
}
