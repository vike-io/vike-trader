//! `StudioState::ui` — the Studio shell's one frame: keyboard shortcuts, the toolbar and backend
//! selector, the editor panel, the tool rail and the shared right-hand panel, and the central
//! results panel.
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! method moved verbatim, as ONE unit). `use super::*` brings in the parent module's imports and
//! items, so nothing about resolution changes.
//!
//! `ui` itself is then a short orchestrator over named phase methods in this file — one per
//! visual region (`toolbar_panel`, `editor_panel`, `tool_rail`, `tools_panel`, `central_panel`,
//! and the rows under them). The egui calls inside each phase keep their original order, ids and
//! id salts: the order the orchestrator calls the panels in is the layout, so it is not a place
//! to tidy.

use super::*;
use crate::panes::sdk_source::{sdk_pick_label, sdk_pick_text};
use egui::{Key, KeyboardShortcut, Modifiers};
use vike_model::host_build::error_lines::{BuildErrorLine, build_error_lines};
use vike_model::host_build::source_edits::{
    SourceEdits, apply_edits, builtin_renames, builtin_rhai_renames, rhai_edits, source_edits,
};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::value::studio;

/// The Studio's keyboard shortcuts, each with what it does. `apply_shortcuts` reads its keys from
/// these and the desktop's Help → Keyboard shortcuts lists them, so the list is the handler's own.
/// Every one is a Ctrl/Cmd chord: `ui` checks the command modifier before it looks at a key.
pub const SHORTCUTS: [(KeyboardShortcut, &str); 5] = [
    (RUN, "Studio: run the backtest"),
    (SAVE, "Studio: save the editor's strategy (under the last saved name when the name is empty)"),
    (CYCLE_TOOL_TAB, "Studio: show the next tool tab on the right"),
    (TOGGLE_EDITOR, "Studio: hide or show the script editor on the left"),
    (TOGGLE_TOOLS, "Studio: hide or show the settings panel on the right"),
];
const RUN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter);
const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
const CYCLE_TOOL_TAB: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Slash);
const TOGGLE_EDITOR: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::OpenBracket);
const TOGGLE_TOOLS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::CloseBracket);

/// `shortcut` in the context's words with its key's symbol: `Ctrl+[`, where egui's own names spell it
/// `Ctrl+OpenBracket`.
fn shortcut_text(ctx: &egui::Context, shortcut: &KeyboardShortcut) -> String {
    let key = shortcut.logical_key;
    ctx.format_shortcut(shortcut).replace(key.name(), key.symbol_or_name())
}

/// The short word under each tool's icon on the rail; its accessible name stays the tab's label.
fn rail_word(tab: StudioTab) -> &'static str {
    match tab {
        StudioTab::Sweep => "Scan",
        StudioTab::Strategy => "Strategy",
        StudioTab::Data => "Data",
        StudioTab::Indicators => "Indicators",
        StudioTab::Saved => "Saved",
        StudioTab::Research => "Research",
        StudioTab::Chat => "AI chat",
    }
}

/// What each tool's panel is for, in a sentence: the rail button's hover.
fn rail_tip(tab: StudioTab) -> &'static str {
    match tab {
        StudioTab::Sweep => {
            "Parameter scan and validation: run the strategy over many parameter values, then \
             check the best out of sample (walk-forward)."
        }
        StudioTab::Strategy => {
            "Strategy: which strategy Run runs (the script in the editor, your Rust strategy, or \
             a file on the server)."
        }
        StudioTab::Data => "Data: the history the data server holds, and importing a CSV file.",
        StudioTab::Indicators => "Indicators: browse them and preview one on the picked data.",
        StudioTab::Saved => {
            "Saved strategies: save the editor's strategy, load one back, compare them all."
        }
        StudioTab::Research => "Research: the server's studies and the runs it kept.",
        StudioTab::Chat => "AI chat: describe a strategy, and the copilot writes and tests it.",
    }
}

/// **The side panels' widths in a Studio `width` wide, so the results keep [`studio::RESULTS_MIN_WIDTH`]**
/// (the owner's pass of 2026-11-21: at about 1420 px the editor and the settings panel kept theirs and
/// left the results 80). `editor` is the splitter's width and `tools` the settings panel's, each `None`
/// while the user hid it (its strip takes [`studio::EDITOR_MIN_WIDTH`]). While both fit, both stay; else
/// both shrink by one factor, each no narrower than [`studio::SIDE_PANEL_MIN_WIDTH`]; an editor that
/// would be narrower than that beside the settings panel folds to its strip (`None`) and the settings
/// panel takes what is left. Alone, neither folds: the strip's arrow hides the settings panel to show
/// the editor, so that ask is kept. Narrower still, the results get what remains.
pub(crate) fn fit_side_panels(
    width: f32,
    editor: Option<f32>,
    tools: Option<f32>,
) -> (Option<f32>, Option<f32>) {
    let strips = [editor, tools].iter().filter(|w| w.is_none()).count() as f32;
    let room =
        width - studio::RAIL_WIDTH - studio::RESULTS_MIN_WIDTH - strips * studio::EDITOR_MIN_WIDTH;
    let want = editor.unwrap_or_default() + tools.unwrap_or_default();
    if want <= room {
        return (editor, tools);
    }
    let floor = studio::SIDE_PANEL_MIN_WIDTH;
    let alone = |w: f32| room.max(floor).min(w);
    match (editor, tools) {
        (Some(_), Some(t)) => {
            let tools_shrunk = (t * room / want).max(floor).min(t);
            match room - tools_shrunk {
                e if e < floor => (None, Some((room - studio::EDITOR_MIN_WIDTH).max(floor).min(t))),
                e => (Some(e), Some(tools_shrunk)),
            }
        }
        (editor, tools) => (editor.map(alone), tools.map(alone)),
    }
}

/// `word` painted top to bottom down a collapsed panel's strip, under what the strip drew above it:
/// the label that says which panel the strip brings back.
fn strip_word(ui: &mut egui::Ui, tk: &ThemeTokens, word: &str) {
    let ink = tk.theme.text_secondary;
    let galley =
        ui.painter().layout_no_wrap(word.to_string(), tk.font(ThemeTextRole::Caption), ink);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(galley.size().y, galley.size().x), egui::Sense::hover());
    // A quarter turn clockwise about the text's top-left corner: it reads downward, its glyphs
    // standing to the left of that corner, so the corner is the strip's top-right.
    let text = egui::epaint::TextShape::new(rect.right_top(), galley, ink)
        .with_angle(std::f32::consts::FRAC_PI_2);
    ui.painter().add(text);
}

impl StudioState {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();
        // A trades page, a scan point, a Research list or artifact, or an SDK read on its way keeps
        // frames coming until it lands, as a run's spinner does: egui draws nothing without an event,
        // so the answer would wait in its channel for a mouse move. Not an immediate repaint, so a
        // frame settles meanwhile.
        if self.trades_rx.is_some()
            || self.point_rx.is_some()
            || self.research_studies_rx.is_some()
            || self.research_runs_rx.is_some()
            || self.study_artifact_rx.is_some()
            || self.sdk_listing_rx.is_some()
            || self.sdk_folder_rx.is_some()
            || self.sdk_file_rx.is_some()
            || self.server_check_rx.is_some()
        {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
        self.poll_csv_upload(ui.ctx());
        // QA autorun (see the field doc): consult on the FIRST frame only — disarm
        // unconditionally so a launch against an empty store can never leave the flag armed to
        // fire a surprise backtest minutes later, the moment a slice first becomes selectable
        // (review finding: `picker.refresh` auto-selects row 0 once data appears).
        self.take_qa_autorun();
        // Keyboard shortcuts (checked once per frame): Ctrl/Cmd+Enter runs the backtest,
        // Ctrl/Cmd+S saves the current editor buffer, Ctrl/Cmd+/ cycles the right-hand tool tab.
        // `modifiers.command` is Cmd on macOS / Ctrl elsewhere — the cross-platform egui idiom.
        let command = ui.input(|i| i.modifiers.command);
        let tk = ThemeTokens::of(ui.ctx());
        if command {
            self.apply_shortcuts(ui);
        }
        // The side panels' widths this frame, so the results keep theirs (`fit_side_panels`), and the
        // most the splitter may give the editor beside the settings panel.
        let width = ui.available_width();
        let (editor, tools) = fit_side_panels(
            width,
            (!self.editor_collapsed).then_some(self.editor_width),
            (!self.tools_collapsed).then_some(studio::TOOLS_PANEL_WIDTH),
        );
        let editor_room = width
            - studio::RAIL_WIDTH
            - studio::RESULTS_MIN_WIDTH
            - tools.unwrap_or(studio::EDITOR_MIN_WIDTH);
        // Panels are added in the order egui carves the screen up — toolbar (top), editor (left),
        // the tool rail then the tools panel (right), the central panel LAST — and that order
        // decides both the layout and the auto-ids, so these calls are not reorderable.
        self.toolbar_panel(ui, &tk);
        self.editor_panel(ui, &tk, editor, editor_room);
        // Rail FIRST so it hugs the window edge; the tools panel lands to its left (`tool_rail`).
        self.tool_rail(ui, &tk);
        match tools {
            None => self.tools_strip(ui, &tk),
            Some(width) => self.tools_panel(ui, width),
        }
        self.maybe_persist_workspace(ui.ctx());
        self.central_panel(ui);
    }

    /// The Ctrl/Cmd chords (the caller has already seen the command modifier held): Enter runs the
    /// backtest, S saves the editor buffer, Slash cycles the right-hand tool tab, `[` hides or shows
    /// the editor and `]` the settings panel.
    fn apply_shortcuts(&mut self, ui: &egui::Ui) {
        let (run_pressed, save_pressed, cycle_pressed, editor_pressed, tools_pressed) =
            ui.input(|i| {
                (
                    i.key_pressed(RUN.logical_key),
                    i.key_pressed(SAVE.logical_key),
                    i.key_pressed(CYCLE_TOOL_TAB.logical_key),
                    i.key_pressed(TOGGLE_EDITOR.logical_key),
                    i.key_pressed(TOGGLE_TOOLS.logical_key),
                )
            });
        if editor_pressed {
            self.editor_collapsed = !self.editor_collapsed;
        }
        if tools_pressed {
            self.tools_collapsed = !self.tools_collapsed;
        }
        if run_pressed
            && !self.running
            && self.picker.selected().is_some()
            && self.run_blocked_reason().is_none()
        {
            self.start_run();
        }
        if save_pressed {
            self.save_editor();
        }
        if cycle_pressed {
            self.right_tab = next_tab(self.right_tab);
            // Cycling while collapsed used to mutate an invisible tab with zero on-screen
            // feedback (the rail highlight requires an expanded panel) — expand like a rail
            // click does, so the shortcut always shows its effect.
            self.tools_collapsed = false;
        }
    }

    /// The editor's Save (Ctrl+S and the header's Save button): the Saved list's Save, under the name
    /// box's name, else the LAST saved name (normal editor Save semantics), falling back to "untitled"
    /// only when this session has never saved. The old always-"untitled" fallback silently misfiled
    /// follow-up saves: save "momo-1" (name box clears), keep editing, Ctrl+S again → the edits
    /// landed under "untitled" while "momo-1" kept the stale code.
    pub(crate) fn save_editor(&mut self) {
        if self.saved.save_name.trim().is_empty() {
            self.saved.save_name =
                self.last_saved_name.clone().unwrap_or_else(|| "untitled".to_string());
        }
        self.handle_saved_action(StudioSavedAction::SaveCurrent);
    }

    /// The top toolbar panel: the primary row (brand, slice picker, strategy chip, Run, Refresh,
    /// running status), the run window and server line, the Advanced line while it is open, and —
    /// for the named backend — its roster row.
    fn toolbar_panel(&mut self, ui: &mut egui::Ui, tk: &ThemeTokens) {
        egui::Panel::top("studio-toolbar").show(ui, |ui| {
            ui.add_space(space::SM);
            self.toolbar_primary_row(ui, tk);
            self.toolbar_window_row(ui);
            if self.server_advanced {
                self.toolbar_backend_row(ui, tk);
            }
            self.toolbar_named_roster_row(ui, tk);
            ui.add_space(space::SM);
        });
    }

    /// The toolbar's second line: the run window (From, To, the presets, where the data starts) on
    /// the left, the backtest server's status and the Advanced switch on the right.
    fn toolbar_window_row(&mut self, ui: &mut egui::Ui) {
        let coverage = self.picker.selected_row().and_then(|row| {
            self.data_browser.coverage_of(&row.venue, &row.symbol, row.interval.as_deref())
        });
        self.wrapped_row(
            ui,
            "studio-toolbar-window-row",
            |st, ui| st.picker.range_ui(ui, coverage),
            |st, ui| {
                ui.toggle_value(&mut st.server_advanced, "Advanced").on_hover_text(
                    "Where runs go: the backtest server's address, and whether the Studio sends \
                     its strategy or runs one the server holds",
                );
                st.server_status_ui(ui);
            },
        );
    }

    /// `left`'s widgets wrapped across the toolbar's width, then `right`'s right-aligned on the last
    /// line when they fit beside it, else on a line of their own: in a narrow Studio nothing draws over
    /// anything (a right-to-left group in a full row drew the server over the run window's presets).
    /// `right` is measured as it draws and its width kept under `salt`, which the next pass fits by.
    fn wrapped_row(
        &mut self,
        ui: &mut egui::Ui,
        salt: &str,
        left: impl FnOnce(&mut Self, &mut egui::Ui),
        right: impl FnOnce(&mut Self, &mut egui::Ui),
    ) {
        let id = ui.make_persistent_id(salt);
        let need: f32 = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            left(self, ui);
            if ui.available_size_before_wrap().x < need + ui.spacing().item_spacing.x {
                ui.end_row();
            }
            let width = ui
                .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    right(self, ui);
                    ui.min_rect().width()
                })
                .inner;
            if (width - need).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(id, width));
                ui.ctx().request_discard("the toolbar's right-hand group changed width");
            }
        });
    }

    /// The backtest server's status, drawn right to left: Check, the dot and its word, then
    /// `Server: <name>`. The hover names the address and, when the check failed, why.
    fn server_status_ui(&mut self, ui: &mut egui::Ui) {
        let checking = self.server_check_rx.is_some();
        if ui
            .add_enabled(!checking, egui::Button::new("Check").small())
            .on_hover_text("Ask the backtest server again whether it answers")
            .on_disabled_hover_text("Asking the backtest server…")
            .clicked()
        {
            self.check_server();
        }
        let (status, word, why) = match (&self.server_check, checking) {
            (_, true) => (ThemeStatus::Muted, "checking…", None),
            (None, false) => (ThemeStatus::Muted, "not checked", None),
            (Some(Ok(_)), false) => (ThemeStatus::Ok, "connected", None),
            (Some(Err(why)), false) => (ThemeStatus::Error, "not connected", Some(why.as_str())),
        };
        let address = self.backend.address();
        let tip = match why {
            Some(why) => format!("The backtest server at {address}: {why}"),
            None => format!("The backtest server at {address}, where every run goes"),
        };
        chip::status_dot(ui, status, word).on_hover_text(tip.as_str());
        ui.label(format!("Server: {}", self.server_name())).on_hover_text(tip);
    }

    /// The toolbar's first line: brand block and data-slice picker on the left, the strategy chip,
    /// Run and Refresh after them, the transient running status right-aligned.
    fn toolbar_primary_row(&mut self, ui: &mut egui::Ui, tk: &ThemeTokens) {
        // Brand block: the Studio glyph + name, then the data-slice
        // picker and the ONE primary action (Run). Everything transient (spinner, Cancel)
        // is right-aligned so the left half of the bar never jumps around mid-run.
        let title_px = tk.text.px(ThemeTextRole::Title);
        let brand = tk.theme.text_secondary;
        self.wrapped_row(
            ui,
            "studio-toolbar-primary-row",
            |st, ui| {
                ui.label(icons::STUDIO.rich().size(title_px).color(brand));
                ui.label(egui::RichText::new("Studio").strong().size(title_px));
                ui.add_space(space::LG);
                ui.separator();
                ui.add_space(space::SM);
                st.picker.ui(ui);
                ui.add_space(space::SM);
                st.toolbar_strategy_chip(ui);
                st.toolbar_sdk_pick(ui);
                ui.add_space(space::SM);
                st.toolbar_run_button(ui);
                st.toolbar_refresh_button(ui);
            },
            Self::toolbar_running_status,
        );
    }

    /// The strategy chip: which source Run will execute; clicking it jumps to the Strategy tab.
    fn toolbar_strategy_chip(&mut self, ui: &mut egui::Ui) {
        // WHICH strategy Run will execute. Without this the toolbar looks identical in
        // both modes while running completely different code — clicking it jumps to the
        // Strategy tab. The STRATEGY icon, then the source's name: a kit secondary
        // button, whose words (and the icon among them) are the Strong role.
        let (chip_text, chip_tip) = match self.strategy_source {
            StudioStrategySource::Rhai => {
                ("rhai".to_string(), "Running the editor buffer — click to change")
            }
            StudioStrategySource::File => (
                if self.picked_file.is_empty() {
                    "no file".to_string()
                } else {
                    self.picked_file.clone()
                },
                "Running a strategy file of the server's folder — click to change",
            ),
            StudioStrategySource::Rust => (
                if self.rust_file.is_empty() { "rust".to_string() } else { self.rust_file.clone() },
                "Running your Rust strategy — click to change",
            ),
        };
        if ui
            .add(ThemeButtonAction::secondary((icons::STRATEGY, chip_text)))
            .on_hover_text(chip_tip)
            .clicked()
        {
            self.right_tab = StudioTab::Strategy;
            self.tools_collapsed = false;
        }
    }

    /// The SDK pick beside the strategy chip (decision 0296): a dropdown of the backtest server's SDKs
    /// (the list the SDK source view reads, asked the first time either opens), "As the file says" by
    /// default. Each SDK shows its number and, when a file does not build against it there, why; such
    /// a row cannot be picked. Shown only for a source that runs a file: a Rhai buffer runs on this
    /// vike's own code.
    fn toolbar_sdk_pick(&mut self, ui: &mut egui::Ui) {
        if self.strategy_source == StudioStrategySource::Rhai {
            return;
        }
        ui.add_space(space::SM);
        // The file's own pin is known while the editor holds its Rust; a server file run by path
        // keeps its header there.
        let pin =
            (self.strategy_source == StudioStrategySource::Rust).then(|| self.strategy_sdk_pin());
        let listing = self.sdk_source.listing().and_then(|l| l.as_ref().ok());
        let selected = sdk_pick_text(self.sdk_pick, pin, listing);
        let tip = match self.sdk_pick {
            None => {
                "Build and run against the SDK the file pins, else the newest usable on the server"
            }
            Some(_) => "Build and run the file against this SDK, over its own pin",
        };
        let mut pick = None;
        let dropdown = egui::ComboBox::from_id_salt("studio-sdk-pick")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                if ui.selectable_label(self.sdk_pick.is_none(), "As the file says").clicked() {
                    pick = Some(None);
                }
                match self.sdk_source.listing() {
                    None => {
                        ui.label(egui::RichText::new("Listing the server's SDKs…").weak());
                    }
                    Some(Err(why)) => {
                        ui.label(egui::RichText::new(why).color(ThemeStatus::Error.color()));
                    }
                    Some(Ok(listed)) => {
                        for sdk in &listed.sdks {
                            let label = sdk_pick_label(sdk);
                            let row = egui::Button::selectable(
                                sdk.number.is_some() && self.sdk_pick == sdk.number,
                                label.as_str(),
                            );
                            let pickable = sdk.usable && sdk.number.is_some();
                            // The row's own words say why; the hover says it for a pointer too.
                            let why = if sdk.number.is_none() {
                                "a dev checkout builds every file against its live tree: no SDK is picked there"
                            } else {
                                sdk.why_unusable.as_str()
                            };
                            if ui.add_enabled(pickable, row).on_disabled_hover_text(why).clicked() {
                                pick = Some(sdk.number);
                            }
                        }
                    }
                }
            });
        dropdown.response.on_hover_text(tip);
        if dropdown.inner.is_some() && !self.sdk_source.listed() {
            self.start_sdk_listing();
        }
        if let Some(pick) = pick {
            self.sdk_pick = pick;
        }
    }

    /// The ONE primary action, Run, disabled with its reason while nothing can run.
    fn toolbar_run_button(&mut self, ui: &mut egui::Ui) {
        let mut run = ThemeButtonAction::primary((icons::RUN, "Run"));
        if let Some(why) = self.run_disabled_reason() {
            run = run.disabled_because(why);
        }
        if ui.add(run).on_hover_text("Run backtest  (Ctrl+Enter)").clicked() {
            self.start_run();
        }
        ui.checkbox(&mut self.recompute, "Recompute").on_hover_text(
            "Run it even when the server saved an identical run (same strategy text, slice, params, build \
             and data) whose answer it would give back at once. For a measurement.",
        );
    }

    /// The Refresh button, which spawns the catalog re-scan on a worker and disables itself while
    /// that is in flight.
    fn toolbar_refresh_button(&mut self, ui: &mut egui::Ui) {
        // Refresh spawns the catalog walk on a worker thread (`start_catalog_load`)
        // rather than running it in this frame — see `crate::backend::catalog`'s module doc. While
        // it is in flight the button is DISABLED and says so: the walk it is waiting on
        // may be a remote connect, and a button that looks clickable but silently no-ops
        // reads as a broken button rather than as a busy one.
        let scanning = self.catalog_loading();
        let refresh = ui
            .add_enabled(
                !scanning,
                egui::Button::new((icons::REFRESH, if scanning { "scanning…" } else { "Refresh" })),
            )
            .on_hover_text("Re-scan the data store")
            .on_disabled_hover_text(
                "Re-scanning the data store… (a remote store answers over the network)",
            );
        if refresh.clicked() {
            self.start_catalog_load(ui.ctx());
        }
    }

    /// The right-aligned transient status: Cancel, a spinner and what is running, only while
    /// something is; and, while the workspace file's last write failed, that it is not saved
    /// (the reason on hover).
    fn toolbar_running_status(&mut self, ui: &mut egui::Ui) {
        if let Some(why) = &self.workspace_saver.error {
            let words =
                egui::RichText::new("workspace not saved").color(ThemeStatus::Warning.color());
            ui.label(icons::WARNING.before(ui.style(), words)).on_hover_text(why.as_str());
        }
        if self.any_running() {
            let cancel = ui.button((icons::CANCEL, "Cancel"));
            if cancel.on_hover_text("Abandon in-flight runs").clicked() {
                self.cancel();
            }
            ui.spinner();
            ui.label(
                egui::RichText::new(if self.running {
                    "running backtest…"
                } else if self.sweep_rx.is_some() {
                    "running param scan…"
                } else if self.walkforward_rx.is_some() {
                    "running walk-forward…"
                } else if self.study_rx.is_some() {
                    "running study…"
                } else if self.build_rx.is_some() {
                    "building strategy…"
                } else {
                    "comparing saved…"
                })
                .weak(),
            );
        }
    }

    /// The backend selector (second toolbar line): where a Run/Sweep/Walk-Forward executes, and
    /// the backtesthub's address for whichever backend is picked.
    fn toolbar_backend_row(&mut self, ui: &mut egui::Ui, tk: &ThemeTokens) {
        // The backend selector (second toolbar line): WHERE a Run/Sweep/Walk-Forward
        // executes. TWO choices, both of them the backtesthub - `Source` (a script this
        // shell sends) or `Named` (a strategy that daemon already holds). Kept off the busy
        // primary row above.
        //
        // ⚠ There was a THIRD button here, "Local", and deleting `StudioRunBackend::Local` without it
        // left a live defect for one commit: the button tested `!is_remote`, so once `Local`
        // was gone it lit up whenever the backend was NAMED - labelling the named-run
        // backend "Local" - and clicking it silently switched the user to Remote. A
        // boolean that USED to mean "not remote, therefore local" does not survive the
        // removal of the third state it was quietly assuming.
        // `docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`.
        // Shown under the Advanced switch: the two choices read "Strategy from This Studio | The
        // server" (Source: the Studio sends what it runs; Named: the server runs one it holds).
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Strategy from").weak());
            // ⚠ **THE NAMED BACKEND — the one an `observe` role reaches**
            // (`docs/decisions/0064-a-named-run-carries-no-source.md`). Source above needs the
            // Write scope: every `Run*` verb is `Scope::Write` because it compiles client-supplied
            // Rhai or runs a built artifact, while `ListStrategies` plus the named run are the
            // Observe half. Both sign with this PC's device key (`Self::hub_keys`, decision 0288),
            // and the server's access list decides: Source needs the device at `control`, Named
            // runs at `observe`. The daemon both backends dial is the COMPUTE one.
            let mut kind = self.backend_kind();
            let changed = segmented::segmented(
                ui,
                &mut kind,
                &[
                    ThemeSegment {
                        value: StudioRunBackendKind::Source,
                        label: "This Studio",
                        why: SOURCE_WHY,
                    },
                    ThemeSegment {
                        value: StudioRunBackendKind::Named,
                        label: "The server",
                        why: NAMED_WHY,
                    },
                ],
            );
            if changed {
                self.switch_backend(kind);
            }
            let name = ui.label(egui::RichText::new("Address").weak()).id;
            // Both backends dial the SAME daemon and port; the address is one field of either.
            let (StudioRunBackend::Source { address } | StudioRunBackend::Named { address }) =
                &mut self.backend;
            let field = ui
                .add_sized(
                    [studio::BACKEND_ADDRESS_WIDTH, tk.metrics.control_height],
                    egui::TextEdit::singleline(address)
                        .font(tk.mono(ThemeTextRole::Body))
                        .hint_text("host:port"),
                )
                .labelled_by(name)
                // ⚠ NOT the datahub, and this tooltip said it was. `StudioRunBackend::Source`'s own
                // doc records the same correction on 2026-09-20: the three verbs this
                // backend sends are `Plane::Backtest`, which the data daemon refuses BY
                // PLANE - it only ever DEFAULTED to the datahub's port. The doc was fixed
                // and the tooltip an operator actually reads was not.
                .on_hover_text(
                    "The backtest server's address (host:port): `vike-backend backtesthub --addr`, \
                     a different process and port from the data server",
                );
            if field.changed() {
                self.server_check = None;
                self.named_roster = None;
                self.named_roster_rx = None;
            }
            // An edited address is asked once the box is left, not per keystroke.
            if field.lost_focus() && self.server_check.is_none() {
                self.check_server();
            }
        });
    }

    /// The named backend's own row (only while that backend is picked): the Roster button and the
    /// strategies the daemon reported. Folds a finished fetch in BEFORE rendering.
    fn toolbar_named_roster_row(&mut self, ui: &mut egui::Ui, tk: &ThemeTokens) {
        // The named backend's own row: the roster this daemon would run, and — when its
        // operator has not armed the lane — the SWITCH rather than an empty list.
        if let StudioRunBackend::Named { address } = &self.backend {
            let address = address.clone();
            // Fold in a finished fetch before rendering, so the answer appears on the frame it
            // lands rather than the one after.
            if let Some(rx) = &self.named_roster_rx
                && let Ok(answer) = rx.try_recv()
            {
                self.named_roster = Some(answer);
                self.named_roster_rx = None;
            }
            let fetching = self.named_roster_rx.is_some();
            ui.horizontal_wrapped(|ui| self.named_roster_contents(ui, tk, address, fetching));
        }
    }

    /// The roster row's contents: the Roster button, then `asking…`, the fetch error, the unarmed
    /// note, or one selectable per strategy. `address` is the daemon address the button dials;
    /// `fetching` is whether a dial is already in flight.
    fn named_roster_contents(
        &mut self,
        ui: &mut egui::Ui,
        tk: &ThemeTokens,
        address: String,
        fetching: bool,
    ) {
        // ⚠ The dial goes to a WORKER, never this thread — see `named_roster_rx`. The
        // button is disabled while one is in flight so a click cannot start a second.
        if ui
            .add_enabled(!fetching, egui::Button::new("List the server's strategies").small())
            .on_hover_text("Ask the backtest server which strategies it can run by name")
            .on_disabled_hover_text("Asking the backtest server…")
            .clicked()
        {
            self.named_roster_rx =
                Some(crate::backend::backtesthub::named_run::spawn_backtesthub_named_roster(
                    address.clone(),
                    self.hub_keys.clone(),
                ));
        }
        if fetching {
            ui.label(egui::RichText::new("asking…").weak());
            return;
        }
        match &self.named_roster {
            None => {
                ui.label(egui::RichText::new("not listed yet").weak());
            }
            Some(Err(e)) => {
                ui.label(egui::RichText::new(e.clone()).color(ThemeStatus::Error.color()));
            }
            // ⚠ An UNARMED daemon WITHHOLDS the roster (it answers `armed: false`
            // with an EMPTY list — arming gates the NAMES as well as the run, 0064's
            // decision 8 leg 3). Render the SWITCH, never that empty list — the
            // teaching-refusal rule, and the reason this outcome is a success on the
            // wire rather than an error.
            Some(Ok(roster)) if !roster.armed => {
                ui.label(
                    egui::RichText::new(
                        vike_backtesthub_client::named_run::NamedRunRoster::unarmed_note(),
                    )
                    .color(ThemeStatus::Warning.color()),
                );
            }
            Some(Ok(roster)) => {
                for name in &roster.strategies {
                    if ui
                        .selectable_label(
                            self.named_strategy.as_deref() == Some(name.as_str()),
                            // A selectable's words default to the Button style (the
                            // Strong role), so the Body role is named, not implied.
                            egui::RichText::new(name).size(tk.text.px(ThemeTextRole::Body)),
                        )
                        .clicked()
                    {
                        self.named_strategy = Some(name.clone());
                    }
                }
            }
        }
    }

    /// The left editor panel: a slim strip while collapsed (the expand arrow over the word "Editor"),
    /// otherwise the resizable panel holding the header row and the editor buffer. Its right edge is
    /// the splitter between the editor and the results: dragged, it sets the width the workspace
    /// keeps (`editor_width`), no further than `room` (what leaves the results theirs); a grip marks
    /// it. `width` is this frame's ([`fit_side_panels`]): `None` folds it to its strip, and one under
    /// `editor_width` draws it that narrow, unsized by a drag, keeping the user's width for the frame
    /// the Studio is wide enough again.
    fn editor_panel(&mut self, ui: &mut egui::Ui, tk: &ThemeTokens, width: Option<f32>, room: f32) {
        let keys = shortcut_text(ui.ctx(), &TOGGLE_EDITOR);
        let fitted = width.is_none_or(|w| w < self.editor_width);
        if let Some(width) = width.filter(|_| !self.editor_collapsed) {
            // Shrunk, or the frame after: the width is set, not read back, so egui's own memory of
            // the panel (the last width it drew) goes back to the user's.
            let fixed = fitted || self.editor_fitted;
            let panel = egui::Panel::left("studio-editor");
            let panel = if fixed {
                panel.exact_size(width).resizable(false)
            } else {
                // A drag wider than the room would only snap back, shrunk and fixed.
                panel.resizable(true).default_size(self.editor_width).max_size(room.max(width))
            };
            let shown = panel.show(ui, |ui| {
                ui.add_space(space::XS);
                let title = self.editor_header_row(ui, tk, &keys);
                self.template_confirm_row(ui);
                self.open_confirm_row(ui);
                self.editor_body(ui, title);
            });
            if !fixed {
                let edge = shown.response.rect;
                self.editor_width = edge.width();
                // The splitter's grip, just inside the edge: the results panel drawn next covers
                // anything past it.
                let grip = egui::Rect::from_center_size(
                    egui::pos2(edge.right() - space::SM, edge.center().y),
                    egui::vec2(space::SM, tk.metrics.control_height),
                );
                ui.painter().rect_filled(grip, space::XS, tk.theme.text_tertiary);
            }
        } else {
            // Folded for room, not by the user: its arrow makes room by hiding the settings panel.
            let folded = !self.editor_collapsed;
            let tip = if folded {
                format!("Show the script editor: the settings panel hides to make room ({keys})")
            } else {
                format!("Show the script editor ({keys})")
            };
            // NOTE: a DIFFERENT panel id than the expanded editor — egui persists panel width
            // by id, so sharing one id would store this 28px width and reopen the expanded
            // editor as a min-width sliver instead of at the user's dragged width.
            egui::Panel::left("studio-editor-min")
                .exact_size(studio::EDITOR_MIN_WIDTH)
                .resizable(false)
                .show(ui, |ui| {
                    ui.add_space(space::SM);
                    ui.vertical_centered(|ui| {
                        if icons::named(ui.small_button(icons::EXPAND_PANEL), &tip).clicked() {
                            if folded {
                                self.tools_collapsed = true;
                            }
                            self.editor_collapsed = false;
                        }
                        ui.add_space(space::SM);
                        strip_word(ui, tk, "Editor");
                    });
                });
        }
        self.editor_fitted = fitted;
    }

    /// The editor panel's header line: the title, the compile-status chip, the unsaved-changes chip
    /// and Save, then Hide on the panel's inner (right) edge. Returns the title's id, the name the
    /// code field is read by. `keys` is the hide shortcut as this platform spells it.
    fn editor_header_row(&mut self, ui: &mut egui::Ui, tk: &ThemeTokens, keys: &str) -> egui::Id {
        ui.horizontal(|ui| {
            let title = ui.label(
                egui::RichText::new("Editor").strong().size(tk.text.px(ThemeTextRole::Title)),
            );
            ui.add_space(space::SM);
            // Compile-status chip: green "compiles" when the last-compiled source was
            // clean, red "line N" (full message on hover) otherwise — same facts as the
            // old glance-dots, now readable without hovering.
            let built = self.editor_built_sha().map(|sha| sha.get(..8).unwrap_or(sha).to_string());
            match self.rhai_verdict() {
                Some(Ok(())) => {
                    chip::badge(ui, "● compiles", ThemeStatus::Ok).on_hover_text("Compiles OK");
                }
                Some(Err(message)) => {
                    let label = match message.line() {
                        Some(line) => format!("● error · line {line}"),
                        None => "● error".to_string(),
                    };
                    chip::badge(ui, &label, ThemeStatus::Error).on_hover_text(message.to_string());
                }
                // No Rhai verdict is held: the buffer is a Strategy file's RUST source
                // (`buffer_is_rhai`). Say what IS known — its language, and where its
                // verdict comes from — in the muted status, because nothing here has
                // judged it. The Build result in the Strategy pane is the authority, and once it
                // holds this very buffer the chip says what that pane says.
                None => match built {
                    Some(sha) => {
                        chip::badge(ui, &format!("● built {sha}"), ThemeStatus::Ok)
                            .on_hover_text("The last Build compiled exactly this buffer.");
                    }
                    None => {
                        chip::badge(ui, RUST_EDITOR_CHIP, ThemeStatus::Muted).on_hover_text(
                            "My strategy (Rust): this buffer is Rust, which the Rhai check cannot \
                             judge. Build (Strategy tab) compiles it — its result there is \
                             the verdict.",
                        );
                    }
                },
            }
            // Unsaved-changes chip: amber while the current mode's own baseline has
            // drifted. Native compares its OWN (name, params) baseline
            // (`file_is_dirty`) rather than the parked editor buffer every other mode
            // uses — see `file_saved`'s doc for why the two cannot share one check. The
            // hover says what Ctrl+S would really do about it (`unsaved_chip_tip`).
            let dirty = match self.strategy_source {
                StudioStrategySource::File => self.file_is_dirty(),
                StudioStrategySource::Rhai | StudioStrategySource::Rust => {
                    self.editor.source != self.saved_source
                }
            };
            if dirty {
                chip::badge(ui, "● unsaved", ThemeStatus::Warning)
                    .on_hover_text(self.unsaved_chip_tip());
            }
            // Save beside the chip: Ctrl+S's own action, the Saved list's Save. Over unsaved
            // changes the hover is the chip's, which says what a save of this mode keeps.
            let save_tip = if dirty {
                self.unsaved_chip_tip().to_string()
            } else {
                format!("Save the strategy to the Saved list ({})", ui.ctx().format_shortcut(&SAVE))
            };
            if ui
                .add_enabled(
                    self.saved.refused.is_none(),
                    egui::Button::new((icons::SAVE, "Save")).small(),
                )
                .on_hover_text(save_tip)
                .on_disabled_hover_text(crate::panes::saved::FILE_NOT_READ)
                .clicked()
            {
                self.save_editor();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let hide = egui::Button::new((icons::COLLAPSE_PANEL, "Hide")).small();
                if ui.add(hide).on_hover_text(format!("Hide the script editor ({keys})")).clicked()
                {
                    self.editor_collapsed = true;
                }
            });
            title.id
        })
        .inner
    }

    /// The line a template load waits on while the editor holds unsaved changes
    /// (`Self::load_template`): load it over them, or keep them. Nothing while none waits.
    fn template_confirm_row(&mut self, ui: &mut egui::Ui) {
        let Some(code) = self.pending_template.clone() else { return };
        // A switch to Rust since the ask: no Rhai is written over Rust source.
        if self.rhai_writer_blocked_reason().is_some() {
            self.pending_template = None;
            return;
        }
        ui.horizontal_wrapped(|ui| {
            let words = egui::RichText::new("The editor has unsaved changes. Load the template?")
                .color(ThemeStatus::Warning.color());
            ui.label(icons::WARNING.before(ui.style(), words));
            if ui.button("Load it over my changes").clicked() {
                self.write_template(&code);
            }
            if ui.button("Keep my changes").clicked() {
                self.pending_template = None;
            }
        });
    }

    /// The line an Open waits on while the buffer it would replace has unsaved changes
    /// (`Self::open_file_text`): open it over them, or keep them. Nothing while none waits.
    fn open_confirm_row(&mut self, ui: &mut egui::Ui) {
        let Some((file, text)) = self.pending_open.clone() else { return };
        ui.horizontal_wrapped(|ui| {
            let words = format!("The editor has unsaved changes. Open {file}?");
            let words = egui::RichText::new(words).color(ThemeStatus::Warning.color());
            ui.label(icons::WARNING.before(ui.style(), words));
            if ui.button("Open it over my changes").clicked() {
                self.write_open(&file, text);
            }
            if ui.button("Keep my changes").clicked() {
                self.pending_open = None;
            }
        });
    }

    /// Below the editor header: the inline compile-error banner (when the verdict is an error),
    /// then the editor buffer filling the height that is left, named by the header's `title`.
    fn editor_body(&mut self, ui: &mut egui::Ui, title: egui::Id) {
        // Inline compile-error banner ABOVE the editor (under the header), not below it:
        // the chip is a glance affordance, but a failing compile deserves a message
        // visible without hovering — and above the editor it can never be pushed off the
        // panel bottom if the editor's row estimate runs long.
        if let Some(Err(message)) = self.rhai_verdict() {
            let words = egui::RichText::new(crate::panes::editor::format_compile_error(message))
                .color(ThemeStatus::Error.color());
            ui.label(icons::FAILED.before(ui.style(), words));
        }
        // A Save (Ctrl+S, the header's Save) that did not reach the disk says so where it was pressed.
        if let Some(why) = &self.saved.write_error {
            let words = egui::RichText::new(why).color(ThemeStatus::Error.color());
            ui.label(icons::FAILED.before(ui.style(), words));
        }
        ui.add_space(space::XS);
        // A failed Rust Build's errors and the renames the buffer still reaches, in a panel RESERVED
        // at the bottom before the editor takes the rest: below an editor whose row estimate ran
        // long, a list would be pushed off the panel.
        let errors = self.rust_build_errors();
        let fixes = self.rename_fixes(!errors.is_empty());
        if !errors.is_empty() || !fixes.edits.is_empty() || !fixes.hints.is_empty() {
            let most = ui.available_height() / 3.0;
            egui::Panel::bottom("studio-build-errors").resizable(false).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(most).show(ui, |ui| {
                    self.build_error_list(ui, &errors, &fixes);
                });
            });
        }
        // The editor fills whatever height is left.
        self.editor.ui_sized(ui, ui.available_height().max(80.0)).labelled_by(title);
    }

    /// The renames the buffer still reaches, for the list's Apply: over a failed Rust Build, the
    /// names that moved before the first SDK (`source_edits`); over a Rhai buffer, the pre-SDK Rhai
    /// rows (`rhai_edits`: a function only while this vike binds nothing by it and the script
    /// defines nothing by it, so an applied rename is never offered again; a property or a type is
    /// a hint on its line, never an edit).
    fn rename_fixes(&self, build_failed: bool) -> SourceEdits {
        match &self.strategy_source {
            StudioStrategySource::Rust if build_failed => {
                source_edits(&self.editor.source, &builtin_renames())
            }
            StudioStrategySource::Rhai => {
                rhai_edits(&self.editor.source, &builtin_rhai_renames(), &vike_rhai::binds_name)
            }
            _ => SourceEdits::default(),
        }
    }

    /// The failed Build's errors on their lines of the buffer: empty unless the buffer is Rust and
    /// its last Build failed (`vike_model::host_build::error_lines`, which says how rustc's text maps
    /// onto the file's lines).
    pub(crate) fn rust_build_errors(&self) -> Vec<BuildErrorLine> {
        match (&self.strategy_source, &self.build_last) {
            (StudioStrategySource::Rust, Some(Err(diagnostics))) => {
                build_error_lines(diagnostics, &self.editor.source)
            }
            _ => Vec::new(),
        }
    }

    /// The list under the editor: each error a click that jumps to its line, then the renames the
    /// buffer still reaches ([`Self::rename_fixes`]) with one Apply for every edit they carry and a
    /// line for each that no edit can carry. Apply rewrites the BUFFER only; the next Build (a Rhai
    /// buffer's compile dot) is the verdict.
    fn build_error_list(
        &mut self,
        ui: &mut egui::Ui,
        errors: &[BuildErrorLine],
        fixes: &SourceEdits,
    ) {
        ui.horizontal(|ui| {
            let words = if errors.is_empty() {
                "Names this vike renamed".to_string()
            } else {
                format!("{} build error(s): click one to go to its line", errors.len())
            };
            ui.label(egui::RichText::new(words).weak());
            let edits = fixes.edits.len();
            if edits > 0
                && ui
                    .button(format!("Apply {edits} rename(s)"))
                    .on_hover_text("Rewrite the old names in the editor; check it again after")
                    .clicked()
            {
                self.editor.source = apply_edits(&self.editor.source, &fixes.edits);
            }
        });
        for error in errors {
            let words = format!("line {}:{} · {}", error.line, error.column, error.message);
            let row = egui::RichText::new(words).monospace().color(ThemeStatus::Error.color());
            if ui.add(egui::Button::new(row).frame(false)).clicked() {
                self.editor.jump_to_line(error.line);
            }
        }
        let mut hints: Vec<&str> = Vec::new();
        for hint in fixes
            .edits
            .iter()
            .map(|e| e.hint.as_str())
            .chain(fixes.hints.iter().map(String::as_str))
        {
            if !hints.contains(&hint) {
                hints.push(hint);
            }
        }
        for hint in hints {
            ui.label(egui::RichText::new(hint).weak());
        }
    }

    /// The activity-bar rail: one icon button per tool tab, shown even while the tools panel is
    /// collapsed. Must be added BEFORE `tools_panel` so it is the one hugging the window edge.
    fn tool_rail(&mut self, ui: &mut egui::Ui, tk: &ThemeTokens) {
        // The tool rail + the tools panel are two SIBLING right panels (VS Code activity-bar
        // shape), replacing the old single panel that hand-partitioned its width. Two reasons:
        // the rail can no longer be pushed off-screen by over-wide pane content (the pre-redesign
        // clipping bug — `ui.set_width` is only a minimum, so a 380px row in a 300px column shoved
        // the rail past the panel edge), and the rail stays visible while the tools panel is
        // collapsed, so the tools are always one click away. Panel::right order matters: the rail
        // is added FIRST so it hugs the window edge; the tools panel lands to its left.
        // Each tool is one tile: its icon, and under it the tool's short word (painted, so the
        // tile's one accessible name stays the tab's label); the hover says what the panel is for
        // and how to hide it.
        let keys = shortcut_text(ui.ctx(), &TOGGLE_TOOLS);
        egui::Panel::right("studio-rail").exact_size(studio::RAIL_WIDTH).resizable(false).show(
            ui,
            |ui| {
                ui.add_space(space::MD);
                ui.vertical_centered(|ui| {
                    for tab in StudioTab::ALL {
                        let active = self.right_tab == tab && !self.tools_collapsed;
                        let ink = if active { tk.theme.accent } else { tk.theme.text_tertiary };
                        let word = ui.painter().layout_no_wrap(
                            rail_word(tab).to_string(),
                            tk.font(ThemeTextRole::Caption),
                            ink,
                        );
                        let size = egui::vec2(
                            studio::RAIL_WIDTH - space::LG,
                            tk.metrics.control_height + tk.metrics.gap + word.size().y,
                        );
                        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Button, true, tab.label())
                        });
                        let fill = if active {
                            tk.theme.surface
                        } else if response.hovered() {
                            tk.theme.hover
                        } else {
                            egui::Color32::TRANSPARENT
                        };
                        let painter = ui.painter();
                        painter.rect_filled(rect, egui::CornerRadius::same(RADIUS), fill);
                        let icon_at = egui::pos2(
                            rect.center().x,
                            rect.top() + (tk.metrics.control_height + tk.metrics.gap) / 2.0,
                        );
                        let icon_px = tk.text.px(ThemeTextRole::Title);
                        let font = egui::FontId::new(icon_px, icons::family());
                        tab.icon().paint(painter, icon_at, egui::Align2::CENTER_CENTER, font, ink);
                        let word_at = egui::pos2(
                            rect.center().x - word.size().x / 2.0,
                            rect.bottom() - word.size().y,
                        );
                        painter.galley(word_at, word, ink);
                        let action = if active {
                            format!("Click to hide this panel ({keys}).")
                        } else {
                            "Click to show this panel.".to_string()
                        };
                        let response =
                            response.on_hover_text(format!("{}\n{action}", rail_tip(tab)));
                        if active {
                            let mut bar = response.rect;
                            bar.set_right(bar.left() + stroke::EDGE);
                            ui.painter().rect_filled(bar, 0.0, tk.theme.accent);
                        }
                        if response.clicked() {
                            // VS Code behavior: clicking the active tool's icon toggles the panel;
                            // clicking any other icon selects it (expanding if collapsed).
                            if active {
                                self.tools_collapsed = true;
                            } else {
                                self.right_tab = tab;
                                self.tools_collapsed = false;
                            }
                        }
                        ui.add_space(space::XS);
                    }
                });
            },
        );
    }

    /// The settings panel collapsed: a slim strip left of the rail, the expand arrow over the word
    /// "Settings", so how to bring it back is on screen (the rail's icons do it too).
    fn tools_strip(&mut self, ui: &mut egui::Ui, tk: &ThemeTokens) {
        let keys = shortcut_text(ui.ctx(), &TOGGLE_TOOLS);
        egui::Panel::right("studio-tools-min")
            .exact_size(studio::EDITOR_MIN_WIDTH)
            .resizable(false)
            .show(ui, |ui| {
                ui.add_space(space::SM);
                ui.vertical_centered(|ui| {
                    // The arrow points the way the panel opens: leftward, out of the strip.
                    let tip = format!("Show the settings panel ({keys})");
                    if icons::named(ui.small_button(icons::COLLAPSE_PANEL), &tip).clicked() {
                        self.tools_collapsed = false;
                    }
                    ui.add_space(space::SM);
                    strip_word(ui, tk, "Settings");
                });
            });
    }

    /// The shared right-hand tools panel (the caller draws the strip instead while collapsed), `width`
    /// wide ([`fit_side_panels`]): Hide on its inner (left) edge, then the open tool's pane — the Data
    /// pane manages its own scrolling, every other tool's pane sits in one salted vertical scroll.
    fn tools_panel(&mut self, ui: &mut egui::Ui, width: f32) {
        let keys = shortcut_text(ui.ctx(), &TOGGLE_TOOLS);
        egui::Panel::right("studio-tools").exact_size(width).resizable(false).show(ui, |ui| {
            ui.add_space(space::XS);
            // The arrow points the way the panel goes: rightward, into the rail.
            let hide = egui::Button::new((icons::EXPAND_PANEL, "Hide")).small();
            if ui.add(hide).on_hover_text(format!("Hide the settings panel ({keys})")).clicked() {
                self.tools_collapsed = true;
            }
            if self.right_tab == StudioTab::Data {
                // The Data pane manages its own scrolling (the shared catalog grid brings an
                // internal vertical ScrollArea, and the pane adds a horizontal wrap for the
                // grid's ~690px natural width) — nesting that inside the shared vertical
                // ScrollArea below would hand the internal scroll unbounded height.
                ui.add_space(space::SM);
                let upload_to = self.datahub_upload.as_ref().map(|d| d.address.as_str());
                if let Some((venue, symbol, interval)) = self.data_browser.ui(ui, upload_to) {
                    self.picker.select_by_key(&venue, &symbol, &interval);
                }
                if let Some(ask) = self.data_browser.import.take_ask() {
                    self.start_csv_upload(ask, ui.ctx());
                }
            } else {
                // One vertical scroll for the whole pane body: content taller than the panel
                // scrolls instead of clipping, and nothing here can affect the rail's
                // geometry. Salted per tab so each tool keeps its OWN scroll offset —
                // unsalted, switching tabs would open the new pane at the old pane's offset.
                egui::ScrollArea::vertical()
                    .id_salt(("studio-tools-scroll", self.right_tab))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(space::SM);
                        self.tool_pane_ui(ui);
                        ui.add_space(space::MD);
                    });
            }
        });
    }

    /// The pane the current tool tab names — ONE arm per `StudioTab`, so a swapped arm shows the
    /// operator the wrong tool. `StudioTab::Data` never reaches it (`tools_panel` draws that pane
    /// outside the shared scroll).
    fn tool_pane_ui(&mut self, ui: &mut egui::Ui) {
        match self.right_tab {
            StudioTab::Sweep => self.sweep_pane_ui(ui),
            StudioTab::Strategy => self.strategy_pane_ui(ui),
            StudioTab::Data => unreachable!("handled above"),
            StudioTab::Indicators => {
                // The pane returns true the frame "Compute" is clicked; loading
                // bars + compute happens here (the pane never touches the store)
                // so its logic stays testable.
                if self.indicators.ui(ui) {
                    self.compute_indicator_preview();
                }
            }
            StudioTab::Saved => {
                let blocked = self.rhai_writer_blocked_reason();
                if let Some(action) = self.saved.ui(ui, blocked) {
                    self.handle_saved_action(action);
                }
                if self.compare_rx.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("comparing…").weak());
                    });
                }
            }
            StudioTab::Research => {
                // Listed LAZILY, on the first frame this tool is opened: a
                // session that never opens it dials nothing, and a session
                // that does gets the server's lists without pressing Rescan.
                self.ensure_research_listed();
                let window = self.study_window_label();
                let running = self.study_rx.is_some();
                // Bound BEFORE the match: `ui` borrows the pane mutably, and
                // the arms below then touch `self` again.
                let action = self.research.ui(ui, &window, running);
                match action {
                    Some(StudioResearchAction::Refresh) => self.start_research_listing(),
                    Some(StudioResearchAction::RunStudy) => self.start_study(),
                    Some(StudioResearchAction::OlderRuns) => {
                        self.start_research_runs_page(self.research.next_runs_page());
                    }
                    None => {}
                }
            }
            StudioTab::Chat => self.chat_pane_ui(ui),
        }
    }

    /// The central results panel: the study surface or the backtest surface, each naming its own
    /// producer (see [`StudioCenterView`]).
    fn central_panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| match self.center {
            // TWO surfaces, each naming its own producer — see [`StudioCenterView`] for why a study's
            // numbers and a backtest's may not share one.
            StudioCenterView::Study => self.study_center(ui),
            StudioCenterView::Backtest => self.backtest_center(ui),
            // A strategy SDK's file, read-only; "Back to results" returns to the backtest surface.
            StudioCenterView::SdkFile => {
                if self.sdk_source.file_ui(ui, self.sdk_file_rx.is_some()) {
                    self.center = StudioCenterView::Backtest;
                }
            }
        });
    }

    /// The backtest surface: the run line (what is on its way, with Cancel, or how long each kind's last
    /// answer took); then the opened scan point's results, else the single run's, else the scan's best
    /// point's, under the code line that names what produced them, through `results_ui` (whose asks, a
    /// trades page or another point, are answered here); else the last walk-forward alone; else the first
    /// failure with an honest title; else the empty state.
    fn backtest_center(&mut self, ui: &mut egui::Ui) {
        let kinds = [
            ("Backtest", self.running, &self.run_origin),
            ("Param scan", self.sweep_rx.is_some(), &self.scan_origin),
            ("Walk-forward", self.walkforward_rx.is_some(), &self.walkforward_origin),
        ];
        // Each kind's last answer read back from a run the server saved (Recompute runs it again).
        let saved = [
            (
                "Backtest",
                self.last
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .map_or("", |v| v.summary.answered_from_saved_run.as_str()),
            ),
            (
                "Param scan",
                self.sweep_last
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .map_or("", |s| s.answered_from_saved_run.as_str()),
            ),
            (
                "Walk-forward",
                self.walkforward_last
                    .as_ref()
                    .and_then(|r| r.as_ref().ok())
                    .map_or("", |w| w.answered_from_saved_run.as_str()),
            ),
        ];
        if run_line_ui(ui, &kinds, &saved) {
            self.cancel();
        }
        let sweep = self.sweep_last.as_ref().and_then(|r| r.as_ref().ok());
        let walkforward = self.walkforward_last.as_ref().and_then(|r| r.as_ref().ok());
        // The values a fixed walk ran with: its windows name them, having chosen none.
        let walked = walkforward.map(|w| (w, self.walkforward_origin.values()));
        let single = self.last.as_ref().and_then(|r| r.as_ref().ok());
        let opened = self.sweep_open.as_ref().and_then(|r| r.as_ref().ok());
        // The scan point the user opened, else the single run, else the scan's best point (so
        // Equity/Trades have something to render when the user only ran a scan).
        let shown = opened
            .or(single)
            .map(|view| (&view.summary, view.trades.as_deref()))
            .or_else(|| sweep.and_then(|s| s.best.as_ref()).map(|best| (best, None)));
        // An opened point and the scan's best are the scan's results; the single run is the Run's.
        let origin =
            if opened.is_some() || single.is_none() { &self.scan_origin } else { &self.run_origin };
        let asked = match shown {
            Some((summary, trades)) => {
                if let Some((_, code)) = origin.answered() {
                    code_line_ui(ui, code, &self.editor.source);
                }
                // A point that would not open leaves the one on screen, and says why.
                if let Some(Err(e)) = &self.sweep_open {
                    ui.label(
                        egui::RichText::new(format!("{e}")).color(ThemeStatus::Warning.color()),
                    );
                }
                // A failure beside a result on screen says so here: the failure state below shows
                // only when there is nothing to show.
                let failures = [
                    ("Run failed", self.last.as_ref().and_then(|r| r.as_ref().err())),
                    ("Param scan failed", self.sweep_last.as_ref().and_then(|r| r.as_ref().err())),
                    (
                        "Walk-forward failed",
                        self.walkforward_last.as_ref().and_then(|r| r.as_ref().err()),
                    ),
                ];
                for (title, failed) in failures {
                    if let Some(e) = failed {
                        let words = egui::RichText::new(format!("{title}: {e}"))
                            .color(ThemeStatus::Warning.color());
                        ui.label(words);
                    }
                }
                let of = (summary.run.clone(), summary.point);
                let in_hand = trades.is_some();
                let pager = &self.trades;
                // The file the scan was sent with names its points (their records name none).
                let scan_file = self.scan_origin.answered().map_or("", |(_, code)| code.file());
                let scan = sweep.map(|s| (s, scan_file));
                results_ui(ui, &mut self.tab, summary, trades, pager, scan, walked)
                    .map(|action| (action, of, in_hand))
            }
            // A walk-forward is the only result: its own view, under the code it ran.
            None if walkforward.is_some() => {
                if let Some((_, code)) = self.walkforward_origin.answered() {
                    code_line_ui(ui, code, &self.editor.source);
                }
                if let Some((w, set)) = walked {
                    walkforward_view(ui, w, set);
                }
                None
            }
            None => {
                // Which failure to surface, with an honest per-source title (the old fixed
                // "Run failed" header misattributed sweep failures). Walk-forward errors are
                // included too — previously a failed Walk-Forward was completely invisible
                // (read only via `.ok()`).
                let err = match (&self.last, &self.sweep_last, &self.walkforward_last) {
                    (Some(Err(e)), _, _) => Some(("Run failed", format!("{e}"))),
                    (_, Some(Err(e)), _) => Some(("Param scan failed", format!("{e}"))),
                    (_, _, Some(Err(e))) => Some(("Walk-forward failed", format!("{e}"))),
                    _ => None,
                };
                match err {
                    Some((title, message)) => Self::error_state(ui, title, &message),
                    None => self.empty_state(ui),
                }
                None
            }
        };
        if let Some((action, of, in_hand)) = asked {
            self.on_results_action(action, of, in_hand);
        }
    }
}
