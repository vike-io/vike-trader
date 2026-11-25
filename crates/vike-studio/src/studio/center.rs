//! `StudioState`'s central-panel helpers: the panel's non-result surfaces — the study view
//! (`study_center`), the getting-started block (`empty_state`) and the failure header
//! (`error_state`). The annualization factor that stood here (`display_periods_per_year`) went on
//! 2026-11-18 with its last caller: every displayed run is a summary the server annualized at its
//! own slice's interval (decision 0078's amendments).
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::studio;

impl StudioState {
    /// The parameter values a dispatch sends right now: [`Self::current_spec`]'s params (a strategy file's,
    /// a Rust file's), none for a Rhai script, whose request carries none. A fixed walk-forward's windows
    /// name these, having chosen none.
    pub(super) fn sent_values(&self) -> Vec<(String, vike_model::params::ParamValue)> {
        let spec = self.current_spec();
        let params = spec.base_params().into_iter().flat_map(|p| p.iter());
        params.map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// The code a dispatch sends right now, as the results pane names it: the same reading of the panes
    /// [`Self::current_entry`] and [`Self::named_spec`] make (a Named run of a picked roster name runs that
    /// server strategy; a Rhai source sends the editor's text; a Strategy file source runs the server's file; a
    /// Rust source the server's build of the file the editor holds).
    pub(super) fn results_code(&self) -> StudioResultsCode {
        let named = matches!(self.backend, StudioRunBackend::Named { .. });
        match (self.strategy_source, &self.named_strategy) {
            (StudioStrategySource::File, Some(name)) if named => {
                StudioResultsCode::ServerFile(name.clone())
            }
            (StudioStrategySource::Rhai, _) => StudioResultsCode::Buffer {
                text: self.editor.source.clone(),
                file: self.rhai_file.clone(),
                saved: self.editor.source == self.saved_source,
            },
            (StudioStrategySource::File, _) => {
                StudioResultsCode::ServerFile(self.picked_file.clone())
            }
            (StudioStrategySource::Rust, _) => StudioResultsCode::RustFile {
                file: self.rust_file.clone(),
                text: self.editor.source.clone(),
            },
        }
    }

    /// The central panel while the STUDY surface is in front: the last study run, the last refusal,
    /// or the spinner in between; an artifact the result screen asks for is read from here.
    ///
    /// `Self::error_state` is REUSED rather than re-spelled — a study that refused and a backtest
    /// that refused are the same event to a reader, and the title parameter is what names which
    /// one it was. The result side is `crate::panes::research::StudioResearchPane::study_result_ui`,
    /// which is deliberately not `results_ui`: that argument lives on that function.
    pub(super) fn study_center(&mut self, ui: &mut egui::Ui) {
        let read = match &self.study_last {
            Some(Ok(report)) => self.research.study_result_ui(ui, report),
            Some(Err(message)) => {
                Self::error_state(ui, "Study failed", message);
                None
            }
            None if self.study_rx.is_some() => {
                ui.add_space((ui.available_height() * 0.26).max(16.0));
                state::view(ui, ThemeEmptyPane::Loading("Running study…"));
                None
            }
            None => {
                ui.add_space((ui.available_height() * 0.26).max(16.0));
                state::view(ui, ThemeEmptyPane::Empty(NO_STUDY_RESULT));
                None
            }
        };
        if let Some(file) = read {
            self.start_study_artifact_read(file);
        }
    }

    /// The central panel before any result exists: a centered getting-started block instead of
    /// one weak sentence lost in a gray void. Offers the two next actions directly (Run when a
    /// slice is pickable, template load otherwise) and adapts its copy to an empty store.
    ///
    /// ⚠ It asks [`crate::StudioSlicePicker::error`] BEFORE `available().is_empty()`, and the order is
    /// the whole point: an unreadable store leaves the picker's list empty too, so the emptiness
    /// test alone told an operator whose datahub had gone away to go and backfill history. This is
    /// the second surface that rendered that lie (the picker's own combo was the first).
    pub(super) fn empty_state(&mut self, ui: &mut egui::Ui) {
        ui.add_space((ui.available_height() * 0.26).max(16.0));
        if let Some(err) = self.picker.error() {
            let why = format!("{}\n\n{err}", crate::panes::slice_picker::SERIES_SCAN_ADVICE);
            state::view(ui, ThemeEmptyPane::Unreachable(&why));
            return;
        }
        if !self.picker.depth_only().is_empty() && self.picker.available().is_empty() {
            // The store is NOT empty — it holds series no slice can replay. Sending this operator
            // to a backfill would have them re-fetch data they already have.
            state::view(
                ui,
                ThemeEmptyPane::Empty(crate::panes::slice_picker::DEPTH_NOT_REPLAYABLE),
            );
            return;
        }
        if self.picker.available().is_empty() {
            state::view(ui, ThemeEmptyPane::Empty(EMPTY_STORE));
            return;
        }
        state::view(ui, ThemeEmptyPane::Empty("No results yet"));
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(
                    "1  Pick a data slice    2  Write or load a strategy    3  Run",
                )
                .weak(),
            );
            ui.add_space(space::XL);
            // Clamped to the panel and wrap-enabled, NOT a fixed 280px block. The centre
            // panel can be narrower than 280 (the resizable editor + the rail + the 340px
            // tools panel squeeze it), and `vertical_centered` centres a fixed-width
            // allocation into whatever is left — below 280px the block starts LEFT of the
            // panel's clip rect and the first button rendered as "?un backtest" (the
            // 2026-08-21 GPU contact sheet; no CPU rung saw it — coordinates finite, button
            // present and enabled, click still firing). Width-aware sizing off
            // `available_width` is this file's own idiom (the combo widths below);
            // `with_main_wrap` makes the row wrap instead of overflow when even the clamped
            // width cannot hold both buttons (it also flips the Ui's default text wrap mode
            // from Extend to Wrap, so egui may wrap a label inside its button rather than
            // drop the button to a second row — both outcomes stay inside the clip). Panels
            // ≥280px wide render identically to the old fixed block.
            // The kill-proof twins — broken and fixed — live in
            // `crates/vike-chart/tests/headless/frame_record_gate.rs` beside the opt-in check they
            // exercise (`vike_ui_theme::frame_sanity`'s `clipped_text_shapes`). Residual: a
            // panel narrower than ONE button still clips that button's right edge — the floor
            // of any layout that keeps buttons at natural size.
            let row_w = ui.available_width().min(studio::RUN_ROW_MAX_WIDTH);
            ui.allocate_ui_with_layout(
                egui::vec2(row_w, studio::RUN_ROW_HEIGHT),
                egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                |ui| {
                    let mut run = ThemeButtonAction::primary((icons::RUN, "Run backtest"));
                    if let Some(why) = self.run_disabled_reason() {
                        run = run.disabled_because(why);
                    }
                    if ui.add(run).on_hover_text("Ctrl+Enter").clicked() {
                        self.start_run();
                    }
                    // A template is RHAI: refused over a Strategy file's Rust buffer, with the reason
                    // on hover (`rhai_writer_blocked_reason`).
                    let blocked = self.rhai_writer_blocked_reason();
                    if ui
                        .add_enabled(blocked.is_none(), egui::Button::new("Load a template"))
                        .on_disabled_hover_text(blocked.unwrap_or_default())
                        .clicked()
                    {
                        self.load_template(TEMPLATES[self.template_index].1);
                    }
                },
            );
        });
    }

    /// The central panel when the last run/sweep/walk-forward failed: a visible failure header
    /// (`title` names WHICH action failed) + the message in a framed monospace block (was: one
    /// bare red line in the void).
    pub(super) fn error_state(ui: &mut egui::Ui, title: &str, message: &str) {
        let tk = ThemeTokens::of(ui.ctx());
        ui.add_space(space::XL3);
        ui.vertical_centered(|ui| {
            ui.label(
                icons::FAILED
                    .rich()
                    .size(tk.text.px(ThemeTextRole::Heading))
                    .color(ThemeStatus::Error.color()),
            );
            ui.label(egui::RichText::new(title).size(tk.text.px(ThemeTextRole::Title)).strong());
        });
        ui.add_space(space::LG);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(message).monospace().color(ThemeStatus::Error.color()));
        });
    }
}
