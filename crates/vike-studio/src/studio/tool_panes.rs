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

impl StudioState {
    /// The Strategy tool pane: pick WHAT the Run toolbar executes — the Rhai editor buffer, or a
    /// native `vike-backtest` registry strategy plus its params.
    ///
    /// The param editor is deliberately a free-form key/value table rather than a generated form:
    /// the registry has no param spec to generate from (each strategy reads a `&toml::Value` ad
    /// hoc — `vike_studio_core::spec`'s module doc), so any strategy's knobs are expressible here
    /// the day it lands, with no change to this pane. `params_from_rows` types each cell
    /// (`3` -> integer, `2.5` -> float, `true` -> bool, bare text -> string).
    pub(super) fn strategy_pane_ui(&mut self, ui: &mut egui::Ui) {
        let tk = Tokens::of(ui.ctx());
        pane_header(ui, icons::STRATEGY, "Strategy");
        segmented::segmented(
            ui,
            &mut self.strategy_source,
            &[
                Segment {
                    value: StrategySource::Rhai,
                    label: "Rhai script",
                    why: "Run the editor buffer (the original Studio path)",
                },
                Segment {
                    value: StrategySource::Native,
                    label: "Native (Rust)",
                    why: "Run a compiled strategy from the vike-backtest registry",
                },
                Segment {
                    value: StrategySource::Plugin,
                    label: "Plugin (Rust)",
                    why: "Build the editor buffer as a Rust plugin and run the compiled artifact — \
                          Run is disabled until a Build returns a sha",
                },
            ],
        );
        ui.add_space(space::MD);
        match self.strategy_source {
            StrategySource::Rhai => {
                ui.label(
                    egui::RichText::new(
                        "Run/Sweep/Walk-Forward execute the editor buffer on the left.",
                    )
                    .weak(),
                );
            }
            StrategySource::Native => {
                let roster = native_strategies();
                self.native_idx = self.native_idx.min(roster.len().saturating_sub(1));
                ui.label(egui::RichText::new("Registry strategy").weak());
                egui::ComboBox::from_id_salt("studio-native-strategy")
                    .width((ui.available_width() - space::LG).max(80.0))
                    .show_index(ui, &mut self.native_idx, roster.len(), |i| roster[i].to_string());
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
                let rendered = typed
                    .as_table()
                    .map(|t| {
                        t.iter().map(|(k, v)| format!("{k} = {v}")).collect::<Vec<_>>().join("\n")
                    })
                    .unwrap_or_default();
                if !rendered.is_empty() {
                    ui.label(egui::RichText::new("Resolved params").weak());
                    ui.add(egui::Label::new(egui::RichText::new(rendered).monospace()).wrap());
                }
            }
            // Unlike Native, this mode does NOT replace the pane's content with a dropdown — a
            // plugin is CODE the user writes, exactly like Rhai, so the editor buffer on the left
            // stays the thing Run/Sweep/Walk-Forward act on. What differs from Rhai is what
            // happens to that buffer: it is built into a `.so` by the builder service — which
            // `crate::backend::plugin_build` DOES call from this crate now, through the Build button below
            // — rather than compiled in-process, which is why a name and a build status live here
            // instead of a params table.
            StrategySource::Plugin => {
                // ⚠ **This label said the Run path was not connected, and that is no longer
                // true.** It read "Plumbing only so far: nothing here builds the editor buffer,
                // and no Run path loads a plugin yet", which was an honest description of
                // `docs/decisions/0082`'s state and is now the opposite of what happens: Build
                // dials the builder service below, and `vike_studio_core`'s `build_strategy`
                // `dlopen`s the artifact the returned sha names. Leaving the old sentence would
                // be the mirror of the promise IT replaced — a pane telling a user that the
                // button in front of them does nothing.
                ui.label(
                    egui::RichText::new(
                        "Build sends the editor buffer to the builder service, which compiles it \
                         to a .so and answers with its sha. Run then sends that sha, and the \
                         backtest server loads exactly that artifact. Build first — Run stays \
                         disabled until a sha comes back, and again after any edit.",
                    )
                    .weak(),
                );
                // ⚠ **The one thing a plugin author must know that is not visible anywhere else in
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
                    ui.label(egui::RichText::new("Name").weak());
                    input::text(ui, &mut self.plugin_name, Field::default());
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Builder").weak());
                    ui.add(
                        egui::TextEdit::singleline(&mut self.builder_addr)
                            .font(tk.mono(TextRole::Body))
                            .min_size(egui::vec2(0.0, tk.metrics.control_h))
                            .desired_width(studio::BUILDER_ADDR_W),
                    )
                    .on_hover_text(
                        "The BUILDER service (vike-strategy-builder), not the compute daemon \
                         — a third service with its own key. Loopback only; reach a remote \
                         box through a tunnel.",
                    );
                });
                ui.add_space(space::SM);
                let building = self.build_rx.is_some();
                let build_why = if building {
                    Some("A build is running.")
                } else if self.plugin_name.trim().is_empty() {
                    Some("Name the plugin first.")
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
                        self.start_plugin_build();
                    }
                    if building {
                        ui.label(egui::RichText::new("building…").weak());
                    }
                });
                ui.add_space(space::SM);
                match (&self.plugin_sha, self.run_blocked_reason()) {
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
    }

    /// The Sweep & Validate tool pane (extracted from the old inline match arm; behavior
    /// unchanged, layout restyled: header, full-width template row, grouped grid section, one
    /// primary action).
    pub(super) fn sweep_pane_ui(&mut self, ui: &mut egui::Ui) {
        let tk = Tokens::of(ui.ctx());
        pane_header(ui, icons::SWEEP, "Sweep & Validate");
        ui.label(egui::RichText::new("Template").weak());
        ui.horizontal(|ui| {
            let combo_w = (ui.available_width() - studio::COMBO_BESIDE_BUTTON_RESERVE).max(80.0);
            egui::ComboBox::from_id_salt("studio-template").width(combo_w).show_index(
                ui,
                &mut self.template_idx,
                TEMPLATES.len(),
                |i| TEMPLATES[i].0.to_string(),
            );
            // Templates are RHAI, so over a Plugin's Rust buffer both loaders below are refused —
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
                    StrategySource::Plugin => {
                        "No grid yet — Plugin mode has no param editor to seed from yet."
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
        // reads as broken. Mirrors start_sweep's own CSV parse.
        let grid_ok = self
            .grid
            .iter()
            .any(|(_, csv)| csv.split(',').any(|s| s.trim().parse::<f64>().is_ok()));
        ui.horizontal(|ui| {
            let mut sweep = ActionButton::primary((icons::RUN, "Run Sweep"));
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
            let wf_why = self.walk_forward_disabled_reason();
            if ui
                .add_enabled(wf_why.is_none(), egui::Button::new("Walk-Forward"))
                .on_hover_text("4-split out-of-sample validation")
                .on_disabled_hover_text(wf_why.unwrap_or_default())
                .clicked()
            {
                self.start_walkforward();
            }
            if self.sweep_rx.is_some() || self.wf_rx.is_some() {
                ui.spinner();
            }
        });
        // Surface the last sweep/walk-forward failure here too: when the central panel is busy
        // showing an older successful result, a failed validation would otherwise end as a
        // spinner that just stops (review finding: invisible Walk-Forward errors).
        if let Some(Err(e)) = &self.sweep_last {
            ui.add_space(space::SM);
            ui.colored_label(Status::Error.color(), format!("sweep failed: {e}"));
        }
        if let Some(Err(e)) = &self.wf_last {
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
            egui::ComboBox::from_id_salt("studio-chat-provider")
                .width(ui.available_width() - space::LG)
                .selected_text(format!("{:?}", self.chat.provider))
                .show_ui(ui, |ui| {
                    for p in self.chat.available_providers().to_vec() {
                        ui.selectable_value(&mut self.chat.provider, p, format!("{p:?}"));
                    }
                });
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
            let oos_color = if last.oos_sharpe > 0.0 {
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
                        .color(oos_color),
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
                // The copilot writes RHAI: refused over a Plugin's Rust buffer, with the reason on
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
        if ui
            .button("Connect to Claude")
            .on_hover_text("Generate the MCP connect command")
            .clicked()
        {
            // `vike-cli mcp`'s run/list tools need a RUNNING vike-datahub server (the retired
            // vike-mcp read a local store instead), so this used to hand over "the datahub the
            // Studio's Remote backend dials".
            //
            // ⚠ **NEITHER backend's address is the datahub's, and the match that stood here said
            // so in one arm while contradicting it in the other.** The `Named` arm passed `None`
            // with the reason written out - its address is the COMPUTE daemon's, MCP's tools dial
            // the DATA daemon, and handing one over points them at a socket that refuses their
            // verbs BY PLANE. Since ruling 7 that is equally true of `Remote`: both variants dial
            // `vike-backend backtest --addr`. So the `Remote` arm was committing the exact defect
            // the `Named` arm beside it was written to avoid, and the comment above them asserted
            // the premise that made it look correct.
            //
            // `None` lets `vike-cli mcp` fall back to its own datahub default, which is the only
            // address here that is actually a datahub's.
            self.chat.connect_to_claude(None);
        }
        if let Some(cmd) = self.chat.connect_command.clone() {
            let mut cmd_display = cmd;
            ui.add(
                egui::TextEdit::singleline(&mut cmd_display)
                    .font(tk.mono(TextRole::Body))
                    .min_size(egui::vec2(0.0, tk.metrics.control_h))
                    .desired_width(f32::INFINITY),
            );
        }
    }
}
