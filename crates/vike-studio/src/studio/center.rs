//! `StudioState`'s central-panel helpers: the annualization factor every displayed metric is
//! scaled by (`display_periods_per_year`) and the panel's non-result surfaces — the study view
//! (`study_center`), the getting-started block (`empty_state`) and the failure header
//! (`error_state`).
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;

impl StudioState {
    /// The annualization factor every DISPLAYED metric is scaled by: the number of RETURN
    /// OBSERVATIONS a year produces at the picked slice's bar step, which is one per bar.
    ///
    /// ONE derivation for the whole window. `crate::results`' Performance tab, its Validation
    /// Sharpe row and its sweep table all take this value, and so does the Saved pane's compare
    /// table — so two panes describing one run cannot print two different Sharpes. The arithmetic
    /// is `vike_analytics::report::periods_per_year_for_interval` (vike-analytics', re-exported),
    /// the SAME function the harness plane uses, which is what makes the Studio door and the
    /// CLI/MCP door agree about the same strategy over the same series. Before it existed this
    /// plane passed a bare `252.0` on every interval and disagreed with that door by
    /// `sqrt(24)` on 1h bars and `sqrt(1440)` on 1m.
    ///
    /// TWO fallbacks, both landing on [`DEFAULT_PERIODS_PER_YEAR`] and neither of them a panic:
    ///
    /// * **No slice picked.** The app starts there, and `SlicePicker::apply` leaves it there for
    ///   an empty store. There is no interval to derive from, so the metrics keep the daily anchor
    ///   — exactly the scale every build before this one used — rather than blanking the pane over
    ///   a display knob.
    /// * **A tick slice** (`SeriesRow::interval` is `None`). A tick stream has no fixed period, so
    ///   there is no honest observation count to derive. This MIRRORS
    ///   `vike_backtest::harness::report::periods_per_year`'s tick branch, whose doc owns the
    ///   decision, rather than inventing a second answer for the same question.
    ///
    /// ⚠ **It reads the PICKER, not the run, and the pane can therefore be re-scaled without
    /// re-running.** Nothing clears `last`/`sweep_last`/`wf_last` when the combo changes, so a
    /// result stays on screen across a re-pick and its annualized rows follow the NEW interval.
    /// That is a smaller version of the staleness the whole pane already has in that state (the
    /// equity curve and trade list are the previous run's too), but it is the one place the number
    /// itself becomes something that never happened. Closing it means the interval travelling WITH
    /// the outcome — a `vike_studio_core` run-plumbing change, not a change to this display — so
    /// it is named here rather than papered over.
    pub(super) fn display_periods_per_year(&self) -> f64 {
        match self.picker.selected_row().and_then(|row| row.interval.as_deref()) {
            Some(interval) => periods_per_year_for_interval(interval),
            None => DEFAULT_PERIODS_PER_YEAR,
        }
    }

    /// The central panel while the STUDY surface is in front: the last study run, the last refusal,
    /// or the spinner in between.
    ///
    /// `Self::error_state` is REUSED rather than re-spelled — a study that refused and a backtest
    /// that refused are the same event to a reader, and the title parameter is what names which
    /// one it was. The result side is `crate::research::study_result_ui`, which is deliberately not
    /// `results_ui`: that argument lives on that function.
    pub(super) fn study_center(&self, ui: &mut egui::Ui) {
        match &self.study_last {
            Some(Ok(run)) => crate::research::study_result_ui(ui, run),
            Some(Err(msg)) => Self::error_state(ui, "Study failed", msg),
            None if self.study_rx.is_some() => {
                ui.add_space((ui.available_height() * 0.26).max(16.0));
                state::view(ui, Load::Loading("Running study…"));
            }
            None => {
                ui.add_space((ui.available_height() * 0.26).max(16.0));
                state::view(ui, Load::Empty(NO_STUDY_RESULT));
            }
        }
    }

    /// The central panel before any result exists: a centered getting-started block instead of
    /// one weak sentence lost in a gray void. Offers the two next actions directly (Run when a
    /// slice is pickable, template load otherwise) and adapts its copy to an empty store.
    ///
    /// ⚠ It asks [`crate::SlicePicker::error`] BEFORE `available().is_empty()`, and the order is
    /// the whole point: an unreadable store leaves the picker's list empty too, so the emptiness
    /// test alone told an operator whose datahub had gone away to go and backfill history. This is
    /// the second surface that rendered that lie (the picker's own combo was the first).
    pub(super) fn empty_state(&mut self, ui: &mut egui::Ui) {
        ui.add_space((ui.available_height() * 0.26).max(16.0));
        if let Some(err) = self.picker.error() {
            let why = format!("{}\n\n{err}", crate::picker::SERIES_SCAN_ADVICE);
            state::view(ui, Load::Unreachable(&why));
            return;
        }
        if !self.picker.depth_only().is_empty() && self.picker.available().is_empty() {
            // The store is NOT empty — it holds series no slice can replay. Sending this operator
            // to a backfill would have them re-fetch data they already have.
            state::view(ui, Load::Empty(crate::picker::DEPTH_NOT_REPLAYABLE));
            return;
        }
        if self.picker.available().is_empty() {
            state::view(ui, Load::Empty(EMPTY_STORE));
            return;
        }
        state::view(ui, Load::Empty("No results yet"));
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(
                    "1  Pick a data slice    2  Write or load a strategy    3  Run",
                )
                .weak(),
            );
            ui.add_space(12.0);
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
            // `crates/vike-chart/tests/frame_record_gate.rs` beside the opt-in check they
            // exercise (`vike_ui_theme::frame_sanity`'s `clipped_text_shapes`). Residual: a
            // panel narrower than ONE button still clips that button's right edge — the floor
            // of any layout that keeps buttons at natural size.
            let row_w = ui.available_width().min(280.0);
            ui.allocate_ui_with_layout(
                egui::vec2(row_w, 28.0),
                egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
                |ui| {
                    let mut run = ActionButton::primary((icons::RUN, "Run backtest"));
                    if let Some(why) = self.run_disabled_reason() {
                        run = run.disabled_because(why);
                    }
                    if ui.add(run).on_hover_text("Ctrl+Enter").clicked() {
                        self.start_run();
                    }
                    // A template is RHAI: refused over a Plugin's Rust buffer, with the reason
                    // on hover (`rhai_writer_blocked_reason`).
                    let blocked = self.rhai_writer_blocked_reason();
                    if ui
                        .add_enabled(blocked.is_none(), egui::Button::new("Load a template"))
                        .on_disabled_hover_text(blocked.unwrap_or_default())
                        .clicked()
                    {
                        self.load_template(TEMPLATES[self.template_idx].1);
                    }
                },
            );
        });
    }

    /// The central panel when the last run/sweep/walk-forward failed: a visible failure header
    /// (`title` names WHICH action failed) + the message in a framed monospace block (was: one
    /// bare red line in the void).
    pub(super) fn error_state(ui: &mut egui::Ui, title: &str, msg: &str) {
        let tk = Tokens::of(ui.ctx());
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(
                icons::FAILED
                    .rich()
                    .size(tk.text.px(TextRole::Heading))
                    .color(Status::Error.color()),
            );
            ui.label(egui::RichText::new(title).size(tk.text.px(TextRole::Title)).strong());
        });
        ui.add_space(8.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(msg).monospace().color(Status::Error.color()));
        });
    }
}
