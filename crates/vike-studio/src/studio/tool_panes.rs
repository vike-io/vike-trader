//! `StudioState`'s right-hand tool panes: the Strategy pane (`strategy_pane_ui`), the Sweep &
//! Validate pane (`sweep_pane_ui`) and the AI Copilot pane (`chat_pane_ui`). The other tools'
//! panes (Data, Indicators, Saved, Research) are rendered by their own modules and only embedded
//! by the shell.
//!
//! Split out of `studio.rs`'s one `impl StudioState` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;
use vike_ui_theme::metrics::space;
use vike_ui_theme::side::Pair;
use vike_ui_theme::value::studio;

/// One served file's state under its row: why it is broken (an error), else its build state in
/// the scan's words, coloured by what it says, and `edited` when it differs from its shipped
/// copy. One truncated line; the hover holds the whole text (a compiler message is long).
fn strategy_file_state_line(ui: &mut egui::Ui, f: &WireStrategyFile) {
    let (text, colour) = if !f.broken.is_empty() {
        (format!("broken: {}", f.broken), Some(Status::Error.color()))
    } else {
        let colour = if f.build.starts_with("built ") {
            Some(Status::Ok.color())
        } else if f.build.starts_with("build failed") {
            Some(Status::Error.color())
        } else if f.build.starts_with("cannot run here") {
            Some(Status::Warning.color())
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
        let tk = Tokens::of(ui.ctx());
        pane_header(ui, icons::STRATEGY, "Strategy");
        if let Some(why) = &self.workspace_refused {
            ui.add(
                egui::Label::new(egui::RichText::new(why).color(Status::Warning.color())).wrap(),
            );
            ui.add_space(space::SM);
        }
        segmented::segmented(
            ui,
            &mut self.strategy_source,
            // Display order only — the files first, where a new user starts; the persisted
            // `StrategySource` value and its default are untouched.
            &[
                Segment {
                    value: StrategySource::Native,
                    label: "Strategy files",
                    why: "Pick a strategy file of the backtest server's folder and run it with \
                          your params",
                },
                Segment {
                    value: StrategySource::Rhai,
                    label: "My strategy (Rhai)",
                    why: "Run the editor buffer (the original Studio path)",
                },
                Segment {
                    value: StrategySource::Rust,
                    label: "My strategy (Rust)",
                    why: "Build the Rust code in the editor and run what it compiles to — Run is \
                          disabled until a Build returns a sha",
                },
            ],
        );
        ui.add_space(space::MD);
        match self.strategy_source {
            StrategySource::Rhai => {
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
                        Field { hint: "my/strategy.rhai", ..Field::default() },
                    )
                    .on_hover_text("The server file Save writes this buffer to")
                    .labelled_by(name);
                });
                self.save_to_server_ui(ui);
            }
            StrategySource::Native => {
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
                            self.native_params.push((String::new(), String::new()));
                        }
                    });
                });
                if self.native_params.is_empty() {
                    ui.label(
                        egui::RichText::new(
                            "No params — the strategy's own defaults apply. \
                             Add a row for e.g. size = 2 or symbol = BTCUSDT.",
                        )
                        .weak(),
                    );
                }
                let mut remove: Option<usize> = None;
                for (i, (key, value)) in self.native_params.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        if icons::named(ui.small_button(icons::REMOVE), "Remove").clicked() {
                            remove = Some(i);
                        }
                        ui.add_sized(
                            [studio::PARAM_KEY_W, studio::PARAM_FIELD_H],
                            egui::TextEdit::singleline(key).hint_text("key"),
                        );
                        let w = (ui.available_width() - space::LG).max(40.0);
                        ui.add_sized(
                            [w, studio::PARAM_FIELD_H],
                            egui::TextEdit::singleline(value).hint_text("value"),
                        );
                    });
                }
                if let Some(i) = remove {
                    self.native_params.remove(i);
                }
                ui.add_space(space::MD);
                // Echo the TYPED table the strategy will actually receive — the only feedback
                // available without a param spec, and it makes the string-vs-number fallback
                // visible instead of surprising.
                let typed = params_from_rows(&self.native_params);
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
            // happens to that buffer: it is built into a `.so` by the builder service — which
            // `crate::backend::strategy_file_build` DOES call from this crate now, through the Build button below
            // — rather than compiled in-process, which is why a name and a build status live here
            // instead of a params table.
            StrategySource::Rust => {
                // ⚠ **This label said the Run path was not connected, and that is no longer
                // true.** It read "Plumbing only so far: nothing here builds the editor buffer,
                // and no Run path loads a strategy file yet", which was an honest description of
                // `docs/decisions/0082`'s state and is now the opposite of what happens: Build
                // dials the builder service below, and `vike_studio_core`'s `build_strategy`
                // `dlopen`s the artifact the returned sha names. Leaving the old sentence would
                // be the mirror of the promise IT replaced — a pane telling a user that the
                // button in front of them does nothing.
                ui.label(
                    egui::RichText::new(
                        "Build sends the editor buffer to the builder service as this file, which \
                         compiles it and answers with its sha. Run then runs the FILE on the \
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
                        Field { hint: "my/strategy.rs", ..Field::default() },
                    )
                    .on_hover_text("The path this buffer is built as, under the strategies folder")
                    .labelled_by(name);
                });
                ui.horizontal(|ui| {
                    let name = ui.label(egui::RichText::new("Builder").weak()).id;
                    ui.add(
                        egui::TextEdit::singleline(&mut self.builder_address)
                            .font(tk.mono(TextRole::Body))
                            .min_size(egui::vec2(0.0, tk.metrics.control_h))
                            .desired_width(studio::BUILDER_ADDR_W),
                    )
                    .on_hover_text(
                        "The BUILDER service (vike-strategy-builder), not the backtesthub \
                         — a third service with its own key. Loopback only; reach a remote \
                         box through a tunnel.",
                    )
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
                        self.start_strategy_file_build();
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
                        chip::badge(ui, &built, Status::Ok).on_hover_text(sha.as_str());
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
                            egui::RichText::new(e).monospace().color(Status::Error.color()),
                        )
                        .wrap(),
                    );
                }
            }
        }
        ui.add_space(space::LG);
        ui.separator();
        self.costs_ui(ui);
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
                    egui::Label::new(egui::RichText::new(e).color(Status::Error.color())).wrap(),
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
                    .max_height(studio::SAVED_LIST_MAX_H)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for f in files {
                            let selected = self.native_file == f.file;
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
            self.native_file = file;
        }
        ui.add_space(space::SM);
        let open_why = if self.native_file.is_empty() {
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
                self.start_open_strategy_file(self.native_file.clone());
            }
            if self.open_rx.is_some() {
                ui.spinner();
            }
        });
        if let Some(e) = &self.open_last {
            ui.add(egui::Label::new(egui::RichText::new(e).color(Status::Error.color())).wrap());
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
                     folder (Studio's backtest key). A running live mount keeps the file it opened \
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
                    egui::Label::new(egui::RichText::new(e).color(Status::Error.color())).wrap(),
                );
            }
            None => {}
        }
    }

    /// The run's costs, under the strategy: the commission the server charges, and the slippage
    /// switch with its value field. The switch only edits what a Run, a Sweep and a Walk-Forward
    /// SEND (`SlippageSwitch::wire`); the server writes it into the profile's one
    /// `[engine.slippage]` table, so it is the same logic as a profile's, and nothing here computes.
    fn costs_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(space::SM);
        ui.label(egui::RichText::new("Costs").strong());
        ui.label(
            egui::RichText::new("Commission: the venue's own fee table, charged by the server.")
                .weak(),
        );
        ui.add_space(space::SM);
        let s = &mut self.slippage;
        ui.horizontal(|ui| {
            let name = ui.label(egui::RichText::new("Slippage").weak()).id;
            egui::ComboBox::from_id_salt("studio-slippage-model")
                .selected_text(s.model.label())
                .show_ui(ui, |ui| {
                    for m in SlippageModel::ALL {
                        ui.selectable_value(&mut s.model, m, m.label());
                    }
                })
                .response
                .labelled_by(name);
        });
        let value_label = match s.model {
            SlippageModel::Off => None,
            SlippageModel::Percent => Some("Share of the price (0.0005 is 0.05%)"),
            SlippageModel::Ticks => Some("Ticks against you on each fill"),
            SlippageModel::MarketImpact => Some("Constant share of the price (0 for none)"),
        };
        if let Some(label) = value_label {
            ui.horizontal(|ui| {
                let name = ui.label(egui::RichText::new(label).weak()).id;
                ui.add(egui::DragValue::new(&mut s.value).range(0.0..=f64::MAX).speed(0.0001))
                    .labelled_by(name);
            });
        }
        if s.model == SlippageModel::Ticks {
            ui.horizontal(|ui| {
                let name =
                    ui.label(egui::RichText::new("Tick size (the price of one tick)").weak()).id;
                ui.add(egui::DragValue::new(&mut s.tick).range(0.0..=f64::MAX).speed(0.0001))
                    .labelled_by(name);
            });
        }
        if s.model == SlippageModel::MarketImpact {
            ui.label(
                egui::RichText::new(
                    "Market impact grows with the order's size against the bars' volume, at the \
                     published coefficients.",
                )
                .weak(),
            );
        }
    }

    /// The Sweep & Validate tool pane (extracted from the old inline match arm; behavior
    /// unchanged, layout restyled: header, full-width template row, grouped grid section, one
    /// primary action).
    pub(super) fn sweep_pane_ui(&mut self, ui: &mut egui::Ui) {
        let tk = Tokens::of(ui.ctx());
        pane_header(ui, icons::SWEEP, "Param scan & Validate");
        let name = ui.label(egui::RichText::new("Template").weak()).id;
        ui.horizontal(|ui| {
            let combo_w = (ui.available_width() - studio::COMBO_BESIDE_BUTTON_RESERVE).max(80.0);
            egui::ComboBox::from_id_salt("studio-template")
                .width(combo_w)
                .show_index(ui, &mut self.template_idx, TEMPLATES.len(), |i| {
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
                self.load_template(TEMPLATES[self.template_idx].1);
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
        ui.add_space(space::LG);
        ui.separator();
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Parameter grid").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Seed from params")
                    .on_hover_text("Fill the grid from the script's param() declarations")
                    .clicked()
                {
                    self.seed_sweep_grid();
                }
            });
        });
        if self.grid.is_empty() {
            ui.label(
                egui::RichText::new(match self.strategy_source {
                    StrategySource::Rhai => {
                        "No grid yet — declare param(\"name\", default) in the script, then Seed."
                    }
                    StrategySource::Native => {
                        "No grid yet — add numeric param rows in the Strategy tab, then Seed."
                    }
                    StrategySource::Rust => {
                        "No grid yet — My strategy (Rust) has no param editor to seed from yet."
                    }
                })
                .weak(),
            );
        }
        for (name, csv) in self.grid.iter_mut() {
            ui.horizontal(|ui| {
                // Fixed, truncating name column: a long script param name must squeeze itself,
                // not push the value field past the panel edge.
                ui.add_sized(
                    [studio::GRID_NAME_W, tk.metrics.control_h],
                    egui::Label::new(name.as_str()).truncate(),
                )
                .on_hover_text(name.as_str());
                let w = (ui.available_width() - space::LG).max(40.0);
                ui.scope(|ui| {
                    ui.spacing_mut().text_edit_width = w;
                    input::text(ui, csv, Field { hint: "1, 2, 3", ..Field::default() });
                });
            });
        }
        ui.add_space(space::MD);
        // Enabled only when the grid actually parses to at least one candidate value — a styled
        // primary button that silently no-ops on click (start_sweep's empty-grid early return)
        // reads as broken. The very reading start_sweep makes (`sweep_axes`, over the one
        // `parse_axis`); the base table only types values, it never decides whether one exists.
        let grid_ok = !super::dispatch::sweep_axes(&self.grid, None).is_empty();
        ui.horizontal(|ui| {
            let mut sweep = ActionButton::primary((icons::RUN, "Run param scan"));
            if let Some(why) = self.sweep_disabled_reason(grid_ok) {
                sweep = sweep.disabled_because(why);
            }
            if ui
                .add(sweep)
                .on_hover_text("Backtest every grid combination and rank by Sharpe")
                .clicked()
            {
                self.start_sweep();
            }
            let walkforward_why = self.walk_forward_disabled_reason();
            if ui
                .add_enabled(walkforward_why.is_none(), egui::Button::new("Walk-Forward"))
                .on_hover_text(format!(
                    "{}-split out-of-sample validation",
                    self.walkforward_splits
                ))
                .on_disabled_hover_text(walkforward_why.unwrap_or_default())
                .clicked()
            {
                self.start_walkforward();
            }
            if self.sweep_rx.is_some() || self.walkforward_rx.is_some() {
                ui.spinner();
            }
        });
        // The walk-forward's own two knobs: how many out-of-sample windows, and whether each
        // window searches the grid above on its training half (the wire's searching walk) or walks
        // the strategy's fixed parameters.
        // Two lines, not one: the tools panel is a fixed width, and a row wider than it shoves the
        // whole pane sideways under the results column.
        ui.horizontal(|ui| {
            let label = ui.label(egui::RichText::new("Walk-Forward windows").weak()).id;
            ui.add(
                egui::DragValue::new(&mut self.walkforward_splits).range(WALKFORWARD_SPLITS_RANGE),
            )
            .labelled_by(label);
        });
        ui.add_enabled(
            grid_ok,
            egui::Checkbox::new(&mut self.walkforward_search, "Search the grid in each window"),
        )
        .on_hover_text("Each window picks its parameters from the grid on its training half")
        .on_disabled_hover_text(SEED_A_GRID);
        // Surface the last sweep/walk-forward failure here too: when the central panel is busy
        // showing an older successful result, a failed validation would otherwise end as a
        // spinner that just stops (review finding: invisible Walk-Forward errors).
        if let Some(Err(e)) = &self.sweep_last {
            ui.add_space(space::SM);
            ui.colored_label(Status::Error.color(), format!("param scan failed: {e}"));
        }
        if let Some(Err(e)) = &self.walkforward_last {
            ui.add_space(space::SM);
            ui.colored_label(Status::Error.color(), format!("walk-forward failed: {e}"));
        }
    }

    /// The AI Copilot tool pane (extracted from the old inline match arm; behavior unchanged,
    /// layout restyled: header, full-width provider/input, wrapped transcript, one primary Send).
    pub(super) fn chat_pane_ui(&mut self, ui: &mut egui::Ui) {
        let tk = Tokens::of(ui.ctx());
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
            .max_height(studio::CHAT_HISTORY_MAX_H)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for (role, text) in &self.chat.history {
                    let color = if role == "assistant" {
                        tk.theme.text2
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
            let mut send = ActionButton::primary("Send");
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
                Pair::PositiveNegative.text(true, &tk)
            } else if last.oos_sharpe < 0.0 {
                Pair::PositiveNegative.text(false, &tk)
            } else {
                ui.visuals().text_color()
            };
            // A MEASUREMENT, not a status, so not a badge: a label row with the Sharpe in the
            // market colour, in JetBrains Mono like every number beside a word.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("OOS Sharpe").color(tk.theme.text2));
                ui.label(
                    egui::RichText::new(format!("{:.2}", last.oos_sharpe))
                        .monospace()
                        .color(out_of_sample_color),
                );
                ui.label(
                    egui::RichText::new(format!("· {} trades", last.n_trades))
                        .color(tk.theme.text2),
                );
            });
            // Review changes: a line-level diff of the current buffer -> the generated script,
            // so Apply is a reviewed action rather than a blind clobber.
            egui::CollapsingHeader::new("Review changes").default_open(true).show(ui, |ui| {
                let rows = crate::panes::chat::diff_rows(&self.editor.source, &last.code);
                egui::ScrollArea::both()
                    .id_salt("studio-chat-diff")
                    .max_height(studio::CHAT_DIFF_MAX_H)
                    .show(ui, |ui| {
                        for row in &rows {
                            let (prefix, color) = match row.kind {
                                crate::panes::chat::DiffKind::Insert => ("+", Status::Ok.color()),
                                crate::panes::chat::DiffKind::Delete => {
                                    ("-", Status::Error.color())
                                }
                                crate::panes::chat::DiffKind::Equal => {
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
                let mut apply = ActionButton::primary("Apply to editor");
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
            // verbs BY PLANE. Since ruling 7 that is equally true of `Remote`: both variants dial
            // `vike-backend backtesthub --addr`. So the `Remote` arm was committing the exact defect
            // the `Named` arm beside it was written to avoid, and the comment above them asserted
            // the premise that made it look correct.
            //
            // `None` lets `vike-mcp` fall back to its own datahub default, which is the only
            // address here that is actually a datahub's.
            self.chat.connect_to_claude(None);
        }
        if let Some(cmd) = self.chat.connect_command.clone() {
            let mut command_display = cmd;
            // Named by the button that produced it: the only visible words about this field.
            ui.add(
                egui::TextEdit::singleline(&mut command_display)
                    .font(tk.mono(TextRole::Body))
                    .min_size(egui::vec2(0.0, tk.metrics.control_h))
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(connect.id);
        }
    }
}
