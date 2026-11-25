//! `StudioState`'s right-hand tool panes: the Strategy pane (`strategy_pane_ui`), the Sweep &
//! Validate pane (`sweep_pane_ui`) and the AI Copilot pane (`chat_pane_ui`). The other tools'
//! panes (Data, Indicators, Saved, Research) are rendered by their own modules and only embedded
//! by the shell.
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;
use crate::panes::param_scan::{
    axis_values, fixed_constants, point_label, scan_results_ui, scan_size_line, scan_total,
    took_text,
};
use vike_backtesthub_client::proto::{FEATURE_ATTACH_FUNDING, FEATURE_RHAI_PARAMETERS};
use vike_backtesthub_client::summary::{
    PARAMSCAN_POINTS_MAX, PARAMSCAN_ROWS_PAGE_SIZE, PARAMSCAN_ROWS_PAGE_SIZE_MAX,
};
use vike_ui_theme::metrics::space;
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::value::studio;

/// One served file's state under its row: why it is broken (an error), else its build state in
/// the scan's words, coloured by what it says, and `edited` when it differs from its shipped
/// copy. One truncated line; the hover holds the whole text (a compiler message is long).
fn strategy_file_state_line(ui: &mut egui::Ui, f: &WireStrategyFile) {
    let (text, colour) = if !f.broken.is_empty() {
        (format!("broken: {}", f.broken), Some(ThemeStatus::Error.color()))
    } else {
        let colour = if f.build.starts_with("built ") {
            Some(ThemeStatus::Ok.color())
        } else if f.build.starts_with("build failed") {
            Some(ThemeStatus::Error.color())
        } else if f.build.starts_with("cannot run here") {
            Some(ThemeStatus::Warning.color())
        } else {
            None
        };
        let build = if f.build.is_empty() { "Rhai" } else { f.build.as_str() };
        (if f.edited { format!("{build} · edited") } else { build.to_string() }, colour)
    };
    let first = text.lines().next().unwrap_or_default();
    let words = match colour {
        Some(c) => egui::RichText::new(first).color(c),
        None => egui::RichText::new(first).weak(),
    };
    ui.indent(f.file.as_str(), |ui| {
        ui.add(egui::Label::new(words).truncate()).on_hover_text(text.as_str());
    });
}

/// What the pane says while "Include funding" is OFF on a server known to predate the switch: a run there is
/// refused rather than charged (`StudioRunEngine::wire`).
const FUNDING_NOT_SWITCHABLE: &str = "Needs a server update: this backtest server cannot switch \
     funding off, so a run is refused until it is updated, or charged a perpetual's funding with \
     the box ticked.";

/// What "Include funding" does, on its hover.
const FUNDING_SWITCH: &str = "Charge a perpetual's stored funding to every Run, scan, Walk-Forward and \
     Compare all. Off by default: a backtest runs without funding unless you tick it.";

/// A row's range field's hint, around `around` (its value, else its default): the ladder a first tick
/// fills the field with (`qa_sweep_ladder`, `10, 20, 30` around `20`), else a neutral one where no ladder
/// reads (no default, a zero, a word).
pub(super) fn range_hint(around: &str) -> String {
    let ladder = qa_sweep_ladder(around);
    if ladder.contains(',') { ladder } else { "a range or a list".to_string() }
}

/// The space, rule and space between two of the Param scan pane's sections.
fn section_break(ui: &mut egui::Ui) {
    ui.add_space(space::LG);
    ui.separator();
    ui.add_space(space::SM);
}

/// A dim, wrapping line of explanation.
fn weak_line(ui: &mut egui::Ui, words: &str) {
    ui.add(egui::Label::new(egui::RichText::new(words).weak()).wrap());
}

/// A wrapping line in the warning colour.
fn warning_line(ui: &mut egui::Ui, words: &str) {
    ui.add(egui::Label::new(egui::RichText::new(words).color(ThemeStatus::Warning.color())).wrap());
}

/// `word` with its first letter capitalized: a cost param's key as a field's label.
fn capitalized(word: &str) -> String {
    let mut letters = word.chars();
    letters.next().map(|c| c.to_uppercase().chain(letters).collect()).unwrap_or_default()
}

/// One test condition: its label and a field with `hint` shown while it is empty.
fn condition_field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.horizontal(|ui| {
        let name = ui
            .add_sized(
                [studio::GRID_NAME_WIDTH, studio::PARAM_FIELD_HEIGHT],
                egui::Label::new(label).truncate(),
            )
            .id;
        let w = (ui.available_width() - space::LG).max(40.0);
        ui.add_sized(
            [w, studio::PARAM_FIELD_HEIGHT],
            egui::TextEdit::singleline(value).hint_text(hint),
        )
        .labelled_by(name);
    });
}

/// One parameter row: its name, its value field when `value` is given (a cost's is its test condition,
/// above), a tick, and the range the tick enables. A first tick on an empty range fills it with a ladder
/// around the value (`qa_sweep_ladder`). Returns whether the value changed.
fn param_row_ui(ui: &mut egui::Ui, row: &mut StudioParamRow, value: Option<&mut String>) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        // Fixed, truncating name column: a long parameter name must squeeze itself, not push the
        // fields past the panel edge.
        ui.add_sized(
            [studio::GRID_NAME_WIDTH, studio::PARAM_FIELD_HEIGHT],
            egui::Label::new(row.name.as_str()).truncate(),
        )
        .on_hover_text(row.name.as_str());
        let set = |text: &str| !text.trim().is_empty();
        let mut around = if set(&row.value) { row.value.clone() } else { row.default.clone() };
        if let Some(value) = value {
            let w = ui.available_width() / 3.0;
            let field = egui::TextEdit::singleline(value).hint_text(row.default.as_str());
            let edit = ui.add_sized([w, studio::PARAM_FIELD_HEIGHT], field);
            changed = icons::named(edit, &format!("{} value", row.name)).changed();
            if set(value) {
                around = value.clone();
            }
        }
        let was = row.scan;
        icons::named(ui.checkbox(&mut row.scan, ""), &format!("Scan {} over its range", row.name));
        if row.scan && !was && row.range.trim().is_empty() {
            row.range = qa_sweep_ladder(&around);
        }
        let w = (ui.available_width() - space::LG).max(40.0);
        let field = egui::TextEdit::singleline(&mut row.range).hint_text(range_hint(&around));
        let range = ui
            .add_enabled(row.scan, |ui: &mut egui::Ui| {
                ui.add_sized([w, studio::PARAM_FIELD_HEIGHT], field)
            })
            .on_disabled_hover_text("Tick the box beside it to scan this range");
        icons::named(range, &format!("{} range", row.name));
    });
    changed
}

impl StudioState {
    /// The Strategy tool pane: pick WHAT the Run toolbar executes — a strategy FILE of the backtest
    /// server's `strategies/` folder plus its params (the ONE list, served with each file's state),
    /// the Rhai editor buffer, or the Rust editor buffer the builder builds.
    ///
    /// The param editor is deliberately a free-form key/value table rather than a generated form:
    /// a file's header declares its keys and the SERVER judges them (an undeclared key, a wrong
    /// type or range is refused in its words), so any file's knobs are expressible here with no
    /// change to this pane. `params_from_rows` types each cell
    /// (`3` -> integer, `2.5` -> float, `true` -> bool, bare text -> string).
    pub(super) fn strategy_pane_ui(&mut self, ui: &mut egui::Ui) {
        pane_header(ui, icons::STRATEGY, "Strategy");
        if let Some(why) = &self.workspace_refused {
            ui.add(
                egui::Label::new(egui::RichText::new(why).color(ThemeStatus::Warning.color()))
                    .wrap(),
            );
            ui.add_space(space::SM);
        }
        // Through `switch_source`, so the editor shows the picked language's own buffer.
        let mut source = self.strategy_source;
        segmented::segmented(
            ui,
            &mut source,
            // Display order only — the files first, where a new user starts; the persisted
            // `StudioStrategySource` value and its default are untouched.
            &[
                ThemeSegment {
                    value: StudioStrategySource::File,
                    label: "Strategy files",
                    why: "Pick a strategy file of the backtest server's folder and run it with \
                          your params",
                },
                ThemeSegment {
                    value: StudioStrategySource::Rhai,
                    label: "My strategy (Rhai)",
                    why: "Run the editor buffer (the original Studio path)",
                },
                ThemeSegment {
                    value: StudioStrategySource::Rust,
                    label: "My strategy (Rust)",
                    why: "Build the Rust code in the editor and run what it compiles to — Run is \
                          disabled until a Build returns a sha",
                },
            ],
        );
        if source != self.strategy_source {
            self.switch_source(source);
        }
        ui.add_space(space::MD);
        match self.strategy_source {
            StudioStrategySource::Rhai => {
                ui.label(
                    egui::RichText::new(
                        "Run/Param scan/Walk-Forward execute the editor buffer on the left.",
                    )
                    .weak(),
                );
                ui.add_space(space::MD);
                ui.horizontal(|ui| {
                    let name = ui.label(egui::RichText::new("File").weak()).id;
                    input::text(
                        ui,
                        &mut self.rhai_file,
                        ThemeInputField { hint: "my/strategy.rhai", ..ThemeInputField::default() },
                    )
                    .on_hover_text("The server file Save writes this buffer to")
                    .labelled_by(name);
                });
                self.save_to_server_ui(ui);
            }
            StudioStrategySource::File => {
                self.strategy_files_ui(ui);
                ui.add_space(space::LG);
                ui.separator();
                ui.add_space(space::SM);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Params").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("+ row")
                            .on_hover_text("Add a key = value param row")
                            .clicked()
                        {
                            self.file_params.push((String::new(), String::new()));
                        }
                    });
                });
                // Commission and slippage are test conditions, set once in the Param scan pane for every
                // run (`StudioState::current_entry` writes them over these rows).
                ui.label(
                    egui::RichText::new(
                        "Commission and slippage: Param scan & Validate, Test conditions.",
                    )
                    .weak(),
                );
                if self.file_params.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No params — the strategy's own defaults apply.                              Add a row for e.g. size = 2 or symbol = BTCUSDT.",
                        )
                        .weak(),
                    );
                }
                let mut remove: Option<usize> = None;
                for (i, (key, value)) in self.file_params.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if icons::named(ui.small_button(icons::REMOVE), "Remove").clicked() {
                            remove = Some(i);
                        }
                        ui.add_sized(
                            [studio::PARAM_KEY_WIDTH, studio::PARAM_FIELD_HEIGHT],
                            egui::TextEdit::singleline(key).hint_text("key"),
                        );
                        let w = (ui.available_width() - space::LG).max(40.0);
                        ui.add_sized(
                            [w, studio::PARAM_FIELD_HEIGHT],
                            egui::TextEdit::singleline(value).hint_text("value"),
                        );
                    });
                }
                if let Some(i) = remove {
                    self.file_params.remove(i);
                }
                ui.add_space(space::MD);
                // Echo the TYPED table the strategy will actually receive — the only feedback
                // available without a param spec, and it makes the string-vs-number fallback
                // visible instead of surprising.
                let typed = params_from_rows(&self.file_params);
                let rendered =
                    typed.iter().map(|(k, v)| format!("{k} = {v}")).collect::<Vec<_>>().join("\n");
                if !rendered.is_empty() {
                    ui.label(egui::RichText::new("Resolved params").weak());
                    ui.add(egui::Label::new(egui::RichText::new(rendered).monospace()).wrap());
                }
            }
            // Unlike Native, this mode does NOT replace the pane's content with a dropdown — a
            // strategy file is CODE the user writes, exactly like Rhai, so the editor buffer on the left
            // stays the thing Run/Sweep/Walk-Forward act on. What differs from Rhai is what
            // happens to that buffer: it is built into a `.so` by the builder service — which the
            // Build button below reaches THROUGH the backtesthub (`crate::backend::backtesthub::build`)
            // — rather than compiled in-process, which is why a name and a build status live here
            // instead of a params table.
            StudioStrategySource::Rust => {
                // ⚠ **This label said the Run path was not connected, and that is no longer
                // true.** It read "Plumbing only so far: nothing here builds the editor buffer,
                // and no Run path loads a strategy file yet", which was an honest description of
                // `docs/decisions/0082`'s state and is now the opposite of what happens: Build
                // reaches the builder service below (through the backtesthub), and `vike_studio_core`'s `build_strategy`
                // `dlopen`s the artifact the returned sha names. Leaving the old sentence would
                // be the mirror of the promise IT replaced — a pane telling a user that the
                // button in front of them does nothing.
                ui.label(
                    egui::RichText::new(
                        "Build sends the editor buffer as this file to the backtest server, whose \
                         builder compiles it and answers with its sha. Run then runs the FILE on the \
                         backtest server, in a sandbox. Build first — Run stays disabled until a \
                         sha comes back, after any edit, and while the editor differs from the \
                         file as it was opened.",
                    )
                    .weak(),
                );
                // ⚠ **The one thing a strategy file author must know that is not visible anywhere else in
                // this pane.** There is no params editor for this tier yet (`current_spec` resolves
                // an EMPTY table), so a Sweep override is the ONLY way to set a knob at all — and
                // every override is an `f64`, i.e. a TOML FLOAT. A `build` that reads its knob with
                // `as_integer()` alone therefore ignores every swept value and reports one flat row
                // of identical results: no error, no warning, and a perfectly plausible answer.
                // The user is looking at this pane while writing that `build`, which is why the
                // sentence is here and not only in the tier README.
                ui.label(icons::WARNING.before(
                    ui.style(),
                    egui::RichText::new(
                        "No params editor yet — a Sweep override is the only way to set a knob, \
                         and every override arrives as a TOML float. Read params as \
                         `as_integer().or_else(|| as_float()…)` or a swept knob silently falls \
                         back to your default.",
                    )
                    .weak(),
                ));
                ui.add_space(space::MD);
                ui.horizontal(|ui| {
                    let name = ui.label(egui::RichText::new("File").weak()).id;
                    input::text(
                        ui,
                        &mut self.rust_file,
                        ThemeInputField { hint: "my/strategy.rs", ..ThemeInputField::default() },
                    )
                    .on_hover_text("The path this buffer is built as, under the strategies folder")
                    .labelled_by(name);
                });
                ui.add_space(space::SM);
                let building = self.build_rx.is_some();
                let build_why = if building {
                    Some("A build is running.")
                } else if self.rust_file.trim().is_empty() {
                    Some("Name the file first (for example my/strategy.rs).")
                } else {
                    None
                };
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(build_why.is_none(), egui::Button::new("Build"))
                        .on_hover_text(
                            "Compile the editor buffer on the builder box (a cargo \
                                        build — seconds when cached, longer when not)",
                        )
                        .on_disabled_hover_text(build_why.unwrap_or_default())
                        .clicked()
                    {
                        self.start_rust_build();
                    }
                    if building {
                        ui.label(egui::RichText::new("building…").weak());
                    }
                });
                self.save_to_server_ui(ui);
                ui.add_space(space::SM);
                match (&self.rust_sha, self.run_blocked_reason()) {
                    (Some(sha), None) => {
                        let built = format!("\u{25cf} built {}", &sha[..sha.len().min(8)]);
                        chip::badge(ui, &built, ThemeStatus::Ok).on_hover_text(sha.as_str());
                    }
                    // A sha exists but Run is still blocked — today that means the buffer drifted
                    // since the Build. Render the REASON rather than the reassuring chip: a green
                    // "built abc123" over stale source is the exact misreading the staleness guard
                    // exists to prevent.
                    (Some(_), Some(reason)) | (None, Some(reason)) => {
                        ui.label(egui::RichText::new(reason).weak());
                    }
                    (None, None) => {}
                }
                if let Some(Err(e)) = &self.build_last {
                    ui.add_space(space::SM);
                    // rustc's own diagnostics, verbatim and monospaced — the design's
                    // `Err(<rustc diagnostics as text>)` reaching the author unedited is the whole
                    // point of carrying them back across the wire.
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(e).monospace().color(ThemeStatus::Error.color()),
                        )
                        .wrap(),
                    );
                }
                ui.add_space(space::LG);
                self.sdk_source_ui(ui);
            }
        }
    }

    /// The Rust pane's "SDK source" section (decision 0296, verdict 3): the strategy SDK the editor's
    /// strategy builds against on the backtest server, as a read-only tree (`crate::panes::sdk_source`).
    /// Collapsed until opened, and the server's SDKs are listed the first time it is: a session that
    /// never opens it dials nothing.
    fn sdk_source_ui(&mut self, ui: &mut egui::Ui) {
        let pin = self.strategy_sdk_pin();
        self.sdk_source.follow(pin);
        let listing = self.sdk_listing_rx.is_some();
        let reading = self.sdk_folder_rx.is_some();
        let opened = egui::CollapsingHeader::new("SDK source")
            .id_salt("studio-sdk-source")
            .show(ui, |ui| self.sdk_source.tree_ui(ui, pin, listing, reading));
        if opened.body_returned.is_some() && !self.sdk_source.listed() {
            self.start_sdk_listing();
        }
        if let Some(action) = opened.body_returned.flatten() {
            self.on_sdk_source_action(action);
        }
    }

    /// The ONE strategy-file list: every file of the backtest server's `strategies/` folder, each
    /// with its state in the scan's words (`not built`, `built <sha8>`, `build failed: …`,
    /// `cannot run here: …`, `edited`, or why it is broken), served by the hub
    /// (`Request::ListStrategyFiles`, folded by `poll`). A click selects the file Run executes;
    /// Open puts its text in the editor.
    fn strategy_files_ui(&mut self, ui: &mut egui::Ui) {
        let fetching = self.strategy_files_rx.is_some();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Files on the server").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!fetching, egui::Button::new("Refresh list").small())
                    .on_hover_text("List the strategy files of the backtest server's folder")
                    .on_disabled_hover_text("Listing…")
                    .clicked()
                {
                    self.start_strategy_files_fetch();
                }
                if fetching {
                    ui.spinner();
                }
            });
        });
        let mut picked: Option<String> = None;
        match &self.strategy_files {
            None => {
                ui.label(
                    egui::RichText::new("Refresh list to see the strategy files on the server.")
                        .weak(),
                );
            }
            Some(Err(e)) => {
                ui.add(
                    egui::Label::new(egui::RichText::new(e).color(ThemeStatus::Error.color()))
                        .wrap(),
                );
            }
            Some(Ok(files)) if files.is_empty() => {
                ui.label(
                    egui::RichText::new("The server's strategies folder holds no file.").weak(),
                );
            }
            Some(Ok(files)) => {
                egui::ScrollArea::vertical()
                    .id_salt("studio-strategy-files")
                    .max_height(studio::SAVED_LIST_MAX_HEIGHT)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for f in files {
                            let selected = self.picked_file == f.file;
                            if ui
                                .add(egui::Button::selectable(selected, f.file.as_str()).truncate())
                                .on_hover_text(f.file.as_str())
                                .clicked()
                            {
                                picked = Some(f.file.clone());
                            }
                            strategy_file_state_line(ui, f);
                        }
                    });
            }
        }
        if let Some(file) = picked {
            self.picked_file = file;
        }
        ui.add_space(space::SM);
        let open_why = if self.picked_file.is_empty() {
            Some("Pick a file in the list first.")
        } else {
            self.open_blocked_reason()
        };
        ui.horizontal(|ui| {
            if ui
                .add_enabled(open_why.is_none(), egui::Button::new("Open in editor"))
                .on_hover_text(
                    "Put this file's text in the editor: a .rhai file runs as your Rhai script, a \
                     .rs file builds as your Rust strategy",
                )
                .on_disabled_hover_text(open_why.unwrap_or_default())
                .clicked()
            {
                self.start_open_strategy_file(self.picked_file.clone());
            }
            if self.open_rx.is_some() {
                ui.spinner();
            }
        });
        if let Some(e) = &self.open_last {
            ui.add(
                egui::Label::new(egui::RichText::new(e).color(ThemeStatus::Error.color())).wrap(),
            );
        }
    }

    /// The Rhai and Rust panes' Save: the editor buffer written to its server file
    /// ([`StudioState::save_target`]) in the backtest server's `strategies/` folder, then the
    /// answer in one line (the file, its sha256 and its state, or the server's refusal).
    fn save_to_server_ui(&mut self, ui: &mut egui::Ui) {
        let why = self.save_blocked_reason();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(why.is_none(), egui::Button::new((icons::SAVE, "Save to server")))
                .on_hover_text(
                    "Write the editor buffer to this file in the backtest server's strategies \
                     folder (this PC's device key, at control). A running live mount keeps the file it opened \
                     until it restarts.",
                )
                .on_disabled_hover_text(why.unwrap_or_default())
                .clicked()
            {
                self.start_save_strategy_file();
            }
            if self.save_file_rx.is_some() {
                ui.spinner();
            }
        });
        match &self.save_file_last {
            Some(Ok(saved)) => {
                ui.add(egui::Label::new(egui::RichText::new(saved).weak()).truncate())
                    .on_hover_text(saved.as_str());
            }
            Some(Err(e)) => {
                ui.add(
                    egui::Label::new(egui::RichText::new(e).color(ThemeStatus::Error.color()))
                        .wrap(),
                );
            }
            None => {}
        }
    }

    /// **The Param scan & Validate tool pane**, top to bottom: the template row, then four sections —
    /// the strategy's own parameters (a value a Run uses, a tick and a range a scan varies), the test
    /// conditions every run shares (starting capital, commission, slippage), the scan and the
    /// walk-forward with their plain explanations, and the last scan's results (a heatmap and a
    /// sortable table, a click opening a point), which a finished scan scrolls into view.
    pub(super) fn sweep_pane_ui(&mut self, ui: &mut egui::Ui) {
        pane_header(ui, icons::SWEEP, "Param scan & Validate");
        let name = ui.label(egui::RichText::new("Template").weak()).id;
        ui.horizontal(|ui| {
            let combo_w = (ui.available_width() - studio::COMBO_BESIDE_BUTTON_RESERVE).max(80.0);
            egui::ComboBox::from_id_salt("studio-template")
                .width(combo_w)
                .show_index(ui, &mut self.template_index, TEMPLATES.len(), |i| {
                    TEMPLATES[i].0.to_string()
                })
                .labelled_by(name);
            // Templates are RHAI, so over a Strategy file's Rust buffer both loaders below are refused —
            // disabled, with the reason on hover (`rhai_writer_blocked_reason` argues why refused
            // rather than switching the source as they load).
            let blocked = self.rhai_writer_blocked_reason();
            if ui
                .add_enabled(blocked.is_none(), egui::Button::new("Load"))
                .on_hover_text("Load this template into the editor")
                .on_disabled_hover_text(blocked.unwrap_or_default())
                .clicked()
            {
                self.load_template(TEMPLATES[self.template_index].1);
            }
        });
        // Gallery: preview each starter script before loading it (the dropdown loads blind). The
        // gallery only reports WHICH template was picked; the write is `load_template`'s, so it
        // passes the same guard as every other Rhai writer.
        let blocked = self.rhai_writer_blocked_reason();
        let mut picked: Option<&'static str> = None;
        egui::CollapsingHeader::new("Browse templates").default_open(false).show(ui, |ui| {
            picked = crate::panes::templates_gallery::gallery_ui(ui, blocked);
        });
        if let Some(code) = picked {
            self.load_template(code);
        }
        // The rows follow the strategy on screen: re-cut when its text changed.
        self.seed_sweep_grid();
        section_break(ui);
        self.strategy_parameters_ui(ui);
        section_break(ui);
        self.test_conditions_ui(ui);
        section_break(ui);
        self.scan_and_validate_ui(ui);
        section_break(ui);
        self.last_scan_ui(ui);
    }

    /// "Strategy parameters": one row per parameter the strategy declares — its value, which a Run uses
    /// (a strategy file's value IS its params row, the Strategy tab's), a tick, and the range a scan
    /// varies it over.
    fn strategy_parameters_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Strategy parameters").strong());
        let words = format!(
            "{} Tick to scan a range (5..30 step 5) or a list (5, 10, 20).",
            self.value_words()
        );
        weak_line(ui, &words);
        let constants = match self.strategy_source {
            StudioStrategySource::Rhai => fixed_constants(&self.editor.source),
            _ => Vec::new(),
        };
        if self.grid.is_empty() && constants.is_empty() {
            weak_line(
                ui,
                match self.strategy_source {
                    StudioStrategySource::Rhai => {
                        "No parameters: declare one in the script, let fast = param(\"fast\", 5);"
                    }
                    StudioStrategySource::File if self.picked_file.trim().is_empty() => {
                        "Pick a strategy file in the Strategy tab."
                    }
                    StudioStrategySource::File => "The file's header declares no number parameter.",
                    StudioStrategySource::Rust => {
                        "No parameters: declare them in the file's header, [params.fast]."
                    }
                },
            );
        }
        if !constants.is_empty() {
            let words = format!(
                "{} {} fixed constants in the script, so a scan cannot vary them.",
                constants.join(", "),
                if constants.len() == 1 { "is a" } else { "are" }
            );
            weak_line(ui, &words);
            let blocked = self.rhai_writer_blocked_reason();
            if ui
                .add_enabled(blocked.is_none(), egui::Button::new("Make them parameters"))
                .on_hover_text(
                    "Rewrite each in the editor as const NAME = param(\"NAME\", value) with the \
                     same number, so a scan can vary it",
                )
                .on_disabled_hover_text(blocked.unwrap_or_default())
                .clicked()
            {
                self.make_constants_params();
            }
        }
        if self.seed_file_rx.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new("reading the file's parameters…").weak());
            });
        }
        if let Some(why) = &self.seed_failed {
            let words = format!("The file's parameters were not read: {why}");
            ui.add(
                egui::Label::new(egui::RichText::new(words).color(ThemeStatus::Warning.color()))
                    .wrap(),
            );
        }
        let file_mode = self.strategy_source == StudioStrategySource::File;
        let file_params = &mut self.file_params;
        for row in &mut self.grid {
            let mut value = match file_mode {
                true => cost_row_value(file_params, &row.name),
                false => row.value.clone(),
            };
            if param_row_ui(ui, row, Some(&mut value)) {
                match file_mode {
                    true => set_cost_row(file_params, &row.name, &value),
                    false => row.value = value,
                }
            }
        }
    }

    /// Whether the backtest server is known to predate a Rhai buffer's parameters (`FEATURE_RHAI_PARAMETERS`) while
    /// the source is Rhai: a plain Run with values or costs set is then refused (`served_spec`), never sent without
    /// them; a scan and the Walk-Forward still carry them as axes.
    pub(super) fn rhai_values_unsent(&self) -> bool {
        self.strategy_source == StudioStrategySource::Rhai
            && self.server_serves(FEATURE_RHAI_PARAMETERS) == Some(false)
    }

    /// What a row's Value field is used by, in words: a Run, except a Rhai script's plain Run on a server that
    /// predates its parameters ([`Self::rhai_values_unsent`]).
    pub(super) fn value_words(&self) -> &'static str {
        match self.rhai_values_unsent() {
            true => {
                "Value: used by a scan and the Walk-Forward; a plain Run of a Rhai script with \
                 values set is refused on this server (it needs an update)."
            }
            false => "Value: what a Run uses (empty: the strategy's own).",
        }
    }

    /// "Test conditions": the market every run is tested in, set once — starting capital, commission,
    /// slippage, funding — and, under "Advanced", a cost a scan may vary. "Include funding" is OFF unless
    /// ticked; on a server that predates the switch the pane says an unticked run is refused there.
    fn test_conditions_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Test conditions").strong());
        weak_line(ui, "Set once: every Run, scan and Walk-Forward uses them.");
        for (cost, _) in STUDIO_COST_ROWS {
            if !self.conditions.costs.iter().any(|r| r.name == cost) {
                let row = StudioParamRow { name: cost.to_string(), ..StudioParamRow::default() };
                self.conditions.costs.push(row);
            }
        }
        condition_field(
            ui,
            "Starting capital",
            &mut self.conditions.capital,
            "the engine's default",
        );
        if !self.conditions.capital.trim().is_empty() && self.conditions.capital().is_none() {
            ui.colored_label(
                ThemeStatus::Warning.color(),
                "Starting capital is a number above zero; the engine's default is used.",
            );
        }
        if let Some(why) = self.capital_not_sent() {
            warning_line(ui, why);
        }
        for (cost, hint) in STUDIO_COST_ROWS {
            if let Some(row) = self.conditions.costs.iter_mut().find(|r| r.name == cost) {
                condition_field(ui, &capitalized(cost), &mut row.value, hint);
            }
        }
        ui.checkbox(&mut self.conditions.funding, "Include funding").on_hover_text(FUNDING_SWITCH);
        if self.server_serves(FEATURE_ATTACH_FUNDING) == Some(false) && !self.conditions.funding {
            warning_line(ui, FUNDING_NOT_SWITCHABLE);
        }
        egui::CollapsingHeader::new("Advanced: also vary costs").default_open(false).show(
            ui,
            |ui| {
                weak_line(ui, "Tick a cost to scan it over a range, like a parameter.");
                for row in &mut self.conditions.costs {
                    param_row_ui(ui, row, None);
                }
            },
        );
        if self.rhai_values_unsent() {
            weak_line(
                ui,
                "This backtest server predates a Rhai script's parameters: a plain Run with values or \
                 costs set here is refused until it is updated. The scan and the Walk-Forward use \
                 them.",
            );
        }
    }

    /// "Scan and validate": the scan button with its size, then the walk-forward's window count and
    /// search switch, each with a line saying in plain words what it does.
    fn scan_and_validate_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(egui::RichText::new("Scan and validate").strong());
        let ticked = self.ticked_rows();
        let counts: Vec<usize> = ticked
            .iter()
            .map(|(_, text)| axis_values(text, None).len())
            .filter(|n| *n > 0)
            .collect();
        let unread: Vec<&str> = ticked
            .iter()
            .filter(|(_, text)| axis_values(text, None).is_empty())
            .map(|(name, _)| name.as_str())
            .collect();
        // Enabled only when a ticked row reads to a value: a styled primary button that silently
        // no-ops on click reads as broken. The very reading `start_sweep` makes (`axis_values`).
        let grid_ok = !counts.is_empty();
        let total = scan_total(&counts);
        let label = match grid_ok {
            true => format!("Run param scan · {}", scan_size_line(&counts)),
            false => "Run param scan".to_string(),
        };
        ui.horizontal(|ui| {
            let mut sweep = ThemeButtonAction::primary((icons::RUN, label.as_str()));
            if let Some(why) = self.sweep_disabled_reason(&counts) {
                sweep = sweep.disabled_because(why);
            }
            if ui
                .add(sweep)
                .on_hover_text(
                    "Backtest every combination of the ticked ranges and rank them by Sharpe",
                )
                .clicked()
            {
                self.start_sweep();
            }
            if self.sweep_rx.is_some() {
                ui.spinner();
            }
        });
        if !unread.is_empty() {
            let words = format!(
                "{}: write a range (5..30 step 5) or a list (5, 10, 20); a range names at most {} \
                 values.",
                unread.join(", "),
                PARAMSCAN_ROWS_PAGE_SIZE_MAX
            );
            warning_line(ui, &words);
        }
        // Past the cap the button refuses (the server would too); past a page of a summarized search's
        // rows a scan is called long.
        if total > PARAMSCAN_POINTS_MAX {
            let words = format!(
                "{total} backtests: one scan runs at most {PARAMSCAN_POINTS_MAX}. Narrow a range or \
                 widen its step."
            );
            warning_line(ui, &words);
        } else if total > PARAMSCAN_ROWS_PAGE_SIZE {
            let words = format!(
                "{total} backtests: a scan this size takes long. Narrow a range or widen its step."
            );
            warning_line(ui, &words);
        }
        if let Some(name) = self.multi_value_held() {
            warning_line(ui, &format!("{name}: {HELD_VALUE_IS_ONE}"));
        }
        ui.add_space(space::MD);
        // The walk-forward's own two knobs: how many out-of-sample windows, and whether each
        // window searches the ticked ranges on the data before it (the wire's searching walk) or
        // walks the parameters as set.
        // Two lines, not one: the tools panel is a fixed width, and a row wider than it shoves the
        // whole pane sideways under the results column.
        ui.horizontal(|ui| {
            let label = ui.label(egui::RichText::new("Walk-Forward windows").weak()).id;
            ui.add(
                egui::DragValue::new(&mut self.walkforward_splits).range(WALKFORWARD_SPLITS_RANGE),
            )
            .labelled_by(label);
        });
        let n = self.walkforward_splits;
        let searched = self.walkforward_search && grid_ok;
        weak_line(
            ui,
            &match searched {
                true => format!(
                    "History is cut into {} equal parts. Each of the last {n} is tested with the \
                     parameters that did best on all the data before it.",
                    n + 1
                ),
                false => format!(
                    "History is cut into {} equal parts. Each of the last {n} is tested in turn \
                     with the parameters as set above.",
                    n + 1
                ),
            },
        );
        ui.add_enabled(
            grid_ok,
            egui::Checkbox::new(&mut self.walkforward_search, "Search the grid in each window"),
        )
        .on_hover_text("Each window picks its parameters from the ticked ranges")
        .on_disabled_hover_text(SEED_A_GRID);
        weak_line(
            ui,
            &match grid_ok {
                true => format!(
                    "Ticked: before each part is tested, the {total} combinations run on the data \
                     before it and the best one is used. Unticked: the parameters stay as set."
                ),
                false => "Tick a parameter to scan first; until then every part runs the \
                          parameters as set."
                    .to_string(),
            },
        );
        ui.horizontal(|ui| {
            let walkforward_why = self.walkforward_disabled_reason();
            if ui
                .add_enabled(walkforward_why.is_none(), egui::Button::new("Walk-Forward"))
                .on_hover_text(format!("{n} windows, each tested on data it was not chosen on"))
                .on_disabled_hover_text(walkforward_why.unwrap_or_default())
                .clicked()
            {
                self.start_walkforward();
            }
            if self.walkforward_rx.is_some() {
                ui.spinner();
            }
        });
        // A walk-forward's failure is said here: its answer is shown in the results pane, which may be
        // showing an older one.
        if let Some(Err(e)) = &self.walkforward_last {
            ui.colored_label(ThemeStatus::Error.color(), format!("walk-forward failed: {e}"));
        }
    }

    /// "Last scan results": a status line (running, failed with its reason, or the points and the time
    /// taken), then the heatmap and the table. A scan's answer, or its refusal, scrolls this section
    /// into view once.
    fn last_scan_ui(&mut self, ui: &mut egui::Ui) {
        let heading = ui.label(egui::RichText::new("Last scan results").strong());
        if std::mem::take(&mut self.sweep_reveal) {
            heading.scroll_to_me(Some(egui::Align::TOP));
        }
        if self.sweep_rx.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new("Scanning…").weak());
            });
        }
        // The point on screen in the results pane: an opened one, else a single run's (no point), else
        // the best.
        let opened = self.sweep_open.as_ref().and_then(|r| r.as_ref().ok());
        let single = self.last.as_ref().and_then(|r| r.as_ref().ok());
        let mut open = None;
        match &self.sweep_last {
            None if self.sweep_rx.is_none() => weak_line(ui, "No scan yet."),
            None => {}
            Some(Err(e)) => {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("The param scan failed: {e}"))
                            .color(ThemeStatus::Error.color()),
                    )
                    .wrap(),
                );
            }
            Some(Ok(sweep)) => {
                let took = self.sweep_took.map(|t| format!(" in {}", took_text(t)));
                let best = sweep.points.get(sweep.best_index).map(point_label);
                let words = format!(
                    "{} points{}. Best: {}.",
                    sweep.points.len(),
                    took.unwrap_or_default(),
                    best.unwrap_or_else(|| "none".to_string())
                );
                ui.label(words);
                let shown = match opened.or(single) {
                    Some(view) => view.summary.point,
                    None => Some(sweep.best_index),
                };
                open = scan_results_ui(ui, sweep, shown);
            }
        }
        if let Some(point) = open {
            // The point opens in the results pane, as one opened there does.
            self.center = StudioCenterView::Backtest;
            self.on_results_action(
                StudioResultsAction::OpenScanPoint(point),
                (String::new(), None),
                false,
            );
        }
    }

    /// The AI Copilot tool pane (extracted from the old inline match arm; behavior unchanged,
    /// layout restyled: header, full-width provider/input, wrapped transcript, one primary Send).
    pub(super) fn chat_pane_ui(&mut self, ui: &mut egui::Ui) {
        let tk = ThemeTokens::of(ui.ctx());
        pane_header(ui, icons::CHAT, "AI Copilot");
        if self.chat.available_providers().is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(NO_PROVIDER_KEY).weak()).wrap());
        } else {
            // No visible label sits beside the picker, so it is named in the tree alone.
            egui::ComboBox::from_id_salt("studio-chat-provider")
                .width(ui.available_width() - space::LG)
                .selected_text(format!("{:?}", self.chat.provider))
                .show_ui(ui, |ui| {
                    for p in self.chat.available_providers().to_vec() {
                        ui.selectable_value(&mut self.chat.provider, p, format!("{p:?}"));
                    }
                })
                .response
                .accessible_name("Provider");
        }
        ui.add_space(space::SM);
        egui::ScrollArea::vertical()
            .id_salt("studio-chat-history")
            .max_height(studio::CHAT_HISTORY_MAX_HEIGHT)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for (role, text) in &self.chat.history {
                    let color = if role == "assistant" {
                        tk.theme.text_secondary
                    } else {
                        ui.visuals().strong_text_color()
                    };
                    ui.label(egui::RichText::new(role).color(color).strong());
                    ui.add(egui::Label::new(egui::RichText::new(text)).wrap());
                    ui.add_space(space::SM);
                }
                if self.chat.history.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "Describe a strategy — the copilot writes it, backtests it \
                             out-of-sample, and shows you the diff before it touches the editor.",
                        )
                        .weak(),
                    );
                }
            });
        ui.add_space(space::SM);
        ui.add(
            egui::TextEdit::multiline(&mut self.chat.input)
                .desired_width(f32::INFINITY)
                .desired_rows(3)
                .hint_text("e.g. RSI mean-reversion with a 2% stop"),
        );
        ui.horizontal(|ui| {
            let mut send = ThemeButtonAction::primary("Send");
            if let Some(why) = self.send_disabled_reason() {
                send = send.disabled_because(why);
            }
            if ui.add(send).clicked() {
                self.start_chat_send();
            }
            if self.chat.running {
                ui.spinner();
                ui.label(egui::RichText::new("developing…").weak());
            }
        });
        if let Some(last) = self.chat.last.clone() {
            ui.add_space(space::MD);
            ui.separator();
            // Same sign convention as every table in the Studio: positive in the market set's up
            // colour, negative in its down colour, zero/NaN neutral (a Sharpe is money, not a
            // status — design system spec §3.2).
            let out_of_sample_color = if last.oos_sharpe > 0.0 {
                ThemeSidePair::PositiveNegative.text(true, &tk)
            } else if last.oos_sharpe < 0.0 {
                ThemeSidePair::PositiveNegative.text(false, &tk)
            } else {
                ui.visuals().text_color()
            };
            // A MEASUREMENT, not a status, so not a badge: a label row with the Sharpe in the
            // market colour, in JetBrains Mono like every number beside a word.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("OOS Sharpe").color(tk.theme.text_secondary));
                ui.label(
                    egui::RichText::new(format!("{:.2}", last.oos_sharpe))
                        .monospace()
                        .color(out_of_sample_color),
                );
                ui.label(
                    egui::RichText::new(format!("· {} trades", last.n_trades))
                        .color(tk.theme.text_secondary),
                );
            });
            // Review changes: a line-level diff of the current buffer -> the generated script,
            // so Apply is a reviewed action rather than a blind clobber.
            egui::CollapsingHeader::new("Review changes").default_open(true).show(ui, |ui| {
                let rows = crate::panes::chat::diff_rows(&self.editor.source, &last.code);
                egui::ScrollArea::both()
                    .id_salt("studio-chat-diff")
                    .max_height(studio::CHAT_DIFF_MAX_HEIGHT)
                    .show(ui, |ui| {
                        for row in &rows {
                            let (prefix, color) = match row.kind {
                                crate::panes::chat::StudioChatDiffKind::Insert => {
                                    ("+", ThemeStatus::Ok.color())
                                }
                                crate::panes::chat::StudioChatDiffKind::Delete => {
                                    ("-", ThemeStatus::Error.color())
                                }
                                crate::panes::chat::StudioChatDiffKind::Equal => {
                                    (" ", ui.visuals().weak_text_color())
                                }
                            };
                            ui.label(
                                egui::RichText::new(format!("{prefix} {}", row.text))
                                    .monospace()
                                    .color(color),
                            );
                        }
                    });
            });
            ui.horizontal(|ui| {
                // The copilot writes RHAI: refused over a Strategy file's Rust buffer, with the reason on
                // hover (`rhai_writer_blocked_reason`). Discard stays available — it touches no
                // buffer.
                let mut apply = ThemeButtonAction::primary("Apply to editor");
                if let Some(why) = self.rhai_writer_blocked_reason() {
                    apply = apply.disabled_because(why);
                }
                if ui.add(apply).clicked() {
                    self.apply_copilot_result(&last);
                }
                if ui.button("Discard").clicked() {
                    self.chat.last = None;
                }
            });
        }
        ui.add_space(space::MD);
        ui.separator();
        let connect =
            ui.button("Connect to Claude").on_hover_text("Generate the MCP connect command");
        if connect.clicked() {
            // `vike-mcp`'s run/list tools need a RUNNING vike-datahub server (the retired
            // vike-mcp read a local store instead), so this used to hand over "the datahub the
            // Studio's Remote backend dials".
            //
            // ⚠ **NEITHER backend's address is the datahub's, and the match that stood here said
            // so in one arm while contradicting it in the other.** The `Named` arm passed `None`
            // with the reason written out - its address is the backtesthub's, MCP's tools dial
            // the DATA daemon, and handing one over points them at a socket that refuses their
            // verbs BY PLANE. Since ruling 7 that is equally true of `Source`: both variants dial
            // `vike-backend backtesthub --addr`. So the `Source` arm was committing the exact defect
            // the `Named` arm beside it was written to avoid, and the comment above them asserted
            // the premise that made it look correct.
            //
            // `None` lets `vike-mcp` fall back to its own datahub default, which is the only
            // address here that is actually a datahub's.
            self.chat.connect_to_claude(None);
        }
        if let Some(command) = self.chat.connect_command.clone() {
            let mut command_display = command;
            // Named by the button that produced it: the only visible words about this field.
            ui.add(
                egui::TextEdit::singleline(&mut command_display)
                    .font(tk.mono(ThemeTextRole::Body))
                    .min_size(egui::vec2(0.0, tk.metrics.control_height))
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(connect.id);
        }
    }
}
