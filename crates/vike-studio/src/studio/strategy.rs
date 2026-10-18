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
    /// The native strategy name currently selected in the Strategy pane. Clamped against the
    /// registry roster so a stale index is never an out-of-bounds panic.
    pub fn native_name(&self) -> &'static str {
        let roster = native_strategies();
        roster[self.native_idx.min(roster.len().saturating_sub(1))]
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
            // A Plugin joins Rhai here for the same reason: it is not on the SERVER's compiled-in
            // roster either (`crate::backend::remote::to_named_run_spec` refuses it by name, the same way it
            // refuses a script), so falling through to `current_spec` is the honest answer rather
            // than inventing a substitution the daemon cannot resolve.
            (StrategySource::Rhai, _)
            | (StrategySource::Plugin, _)
            | (StrategySource::Native, None) => self.current_spec(),
            (StrategySource::Native, Some(name)) => {
                StrategySpec::native(name.clone(), params_from_rows(&self.native_params))
            }
        }
    }

    /// What every Run/Sweep/Walk-Forward/Compare-from-the-toolbar executes right now — the ONE
    /// place `strategy_source` is turned into a runnable [`StrategySpec`].
    pub fn current_spec(&self) -> StrategySpec {
        match self.strategy_source {
            StrategySource::Rhai => StrategySpec::rhai(self.editor.source.clone()),
            StrategySource::Native => {
                StrategySpec::native(self.native_name(), params_from_rows(&self.native_params))
            }
            // No params editor for Plugin mode yet, so this always resolves an EMPTY params table;
            // a dedicated params pane is later work, the same way Native's grew one after its
            // spec did. An empty table is a valid empty TOML document on the plugin's side of the
            // C-ABI, so a plugin runs on its own `build` fallbacks rather than on nothing — which
            // is exactly what the template's params test pins.
            StrategySource::Plugin => StrategySpec::plugin(
                self.plugin_name.clone(),
                self.plugin_sha.clone().unwrap_or_default(),
                vike_studio_core::empty_params(),
            ),
        }
    }

    /// Why Run/Sweep/Walk-Forward are disabled right now, if they are — `None` means "go ahead".
    ///
    /// The one reason this shell's plumbing adds: [`StrategySource::Plugin`] names an artifact by
    /// sha, and running with `plugin_sha` still `None` would ask the server for a build that never
    /// happened. [`crate::backend::remote::to_wire_spec`] would happily serialize an EMPTY sha onto the wire
    /// — nothing downstream refuses it structurally — so this refusal is what actually stops that
    /// request from being sent, not a side effect of some other check.
    /// ⚠ **The second reason is STALENESS, and it is the other half of what makes a sha honest.**
    /// A Build binds the run to the artifact that was built; an edit afterwards leaves the sha
    /// naming the previous one, so a Run would report on code the author had already replaced —
    /// with no error and entirely plausible numbers. Only a sha THIS session built is checked
    /// ([`Self::plugin_built_source`] says why a reloaded one is not).
    pub fn run_blocked_reason(&self) -> Option<&'static str> {
        if self.strategy_source != StrategySource::Plugin {
            return None;
        }
        if self.plugin_sha.is_none() {
            return Some(
                "Build the plugin first — Run needs a sha, and no Build has returned one yet.",
            );
        }
        if self.plugin_built_source.as_deref().is_some_and(|src| src != self.editor.source) {
            return Some(
                "The editor changed since the last Build — Build again, or the run would report \
                 on the previous artifact.",
            );
        }
        None
    }

    /// Whether the editor buffer is RHAI source right now — the one question the Rhai compile
    /// check, the editor header's `● compiles` / `● error · line N` chip and the inline error
    /// banner under it all answer. `false` means none of the three may speak about the buffer.
    ///
    /// - [`StrategySource::Rhai`]: yes — it is the script every Run executes.
    /// - [`StrategySource::Plugin`]: **no.** The buffer is RUST that Build hands to the builder
    ///   service, so a Rhai parse of it is meaningless — measured on the real GUI, a valid plugin
    ///   read `● error · line 4` beside the `● built <sha>` of its own successful Build. The Build
    ///   result in the Strategy pane is the verdict on this buffer.
    /// - [`StrategySource::Native`]: yes, and deliberately. Native mode never WRITES the editor —
    ///   the registry dropdown, the param rows and a native saved row's Load all leave it alone —
    ///   so what it holds is the parked Rhai script that switching back to Rhai will run, and the
    ///   verdict is a true statement about Rhai source. The one way Rust gets there is typing it in
    ///   Plugin mode and then picking Native; the red chip that follows is then the verdict Rhai
    ///   mode WOULD return on that buffer, not a claim about what Native runs.
    ///
    /// An exhaustive `match` rather than `!= Plugin`, so a fourth source is a compile error here
    /// until somebody decides what language its buffer is.
    pub fn buffer_is_rhai(&self) -> bool {
        match self.strategy_source {
            StrategySource::Rhai | StrategySource::Native => true,
            StrategySource::Plugin => false,
        }
    }

    /// The Rhai verdict the editor header renders: `None` when no verdict is held — which is
    /// always the case while [`Self::buffer_is_rhai`] is `false` — else `compile_status`'s answer
    /// for the buffer as of the last [`Self::poll`].
    pub fn rhai_verdict(&self) -> Option<&Result<(), String>> {
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
    /// the author's code.** While the buffer is not Rhai ([`Self::buffer_is_rhai`] — Plugin mode) it
    /// is RUST, and it has no other copy: a Plugin save stores the plugin's name and its Build's
    /// sha, never the source (`SavedStrategy::plugin`). A writer that flipped the mode and loaded
    /// would destroy the only copy of a plugin in one click, and one that loaded WITHOUT flipping
    /// would leave Plugin selected over a Rhai buffer that Build then hands to cargo. Refused, it
    /// costs one deliberate click — pick `Rhai script`, where the same buffer wears the Rhai verdict
    /// and a load is the ordinary Rhai-mode overwrite — and destroys nothing by itself.
    ///
    /// Tied to `buffer_is_rhai` rather than spelled as its own match, because it IS that question:
    /// a Rhai writer may write exactly when the buffer is Rhai. Native passes for the reason that
    /// function gives — its buffer is the parked Rhai script.
    pub fn rhai_writer_blocked_reason(&self) -> Option<&'static str> {
        (!self.buffer_is_rhai()).then_some(RHAI_WRITER_BLOCKED_IN_PLUGIN)
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
    /// ⚠ **In Plugin mode it said "Ctrl+S saves to the Saved list", and that was false twice.** A
    /// Plugin save stores the name and a sha, never the buffer, and it did not re-baseline either —
    /// so the chip never cleared, and the tooltip promised a save that could not capture the edits
    /// it was pointing at. Now a Plugin save re-baselines to what its sha was built from (see
    /// [`Self::saved_source`]), and this names which of the two situations the author is in: the
    /// buffer IS the held sha's source (a save names this code), or it is not (no Build, an edit
    /// since, or a sha reloaded from a saved row) and only a Build can make a save capture it.
    ///
    /// ⚠ **Native used to keep the Rhai sentence, and that was the same defect in the third mode.**
    /// A Native save stores the registry name and params, never the parked buffer, so "Ctrl+S saves
    /// to the Saved list" named the wrong payload — fixed the same way as Plugin, by naming what a
    /// save actually captures.
    pub fn unsaved_chip_tip(&self) -> &'static str {
        match self.strategy_source {
            StrategySource::Rhai => UNSAVED_TIP,
            StrategySource::Native => UNSAVED_TIP_NATIVE,
            StrategySource::Plugin => {
                let buffer_is_built = self.plugin_sha.is_some()
                    && self.plugin_built_source.as_deref() == Some(self.editor.source.as_str());
                if buffer_is_built { UNSAVED_TIP_PLUGIN_BUILT } else { UNSAVED_TIP_PLUGIN_UNBUILT }
            }
        }
    }

    /// [`StrategySource::Native`]'s own dirty check — see [`Self::native_saved`]'s doc for why it
    /// cannot be `editor.source != saved_source` the way the other two modes' can.
    pub(super) fn native_is_dirty(&self) -> bool {
        self.native_saved.as_ref().map(|(name, params)| (*name, params.as_slice()))
            != Some((self.native_name(), self.native_params.as_slice()))
    }
}
