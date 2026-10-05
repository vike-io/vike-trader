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

impl StudioState {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();
        // QA autorun (see the field doc): consult on the FIRST frame only — disarm
        // unconditionally so a launch against an empty store can never leave the flag armed to
        // fire a surprise backtest minutes later, the moment a slice first becomes selectable
        // (review finding: `picker.refresh` auto-selects row 0 once data appears).
        self.take_qa_autorun();
        // Keyboard shortcuts (checked once per frame): Ctrl/Cmd+Enter runs the backtest,
        // Ctrl/Cmd+S saves the current editor buffer, Ctrl/Cmd+/ cycles the right-hand tool tab.
        // `modifiers.command` is Cmd on macOS / Ctrl elsewhere — the cross-platform egui idiom.
        let cmd = ui.input(|i| i.modifiers.command);
        let tk = Tokens::of(ui.ctx());
        if cmd {
            self.apply_shortcuts(ui);
        }
        // Panels are added in the order egui carves the screen up — toolbar (top), editor (left),
        // the tool rail then the tools panel (right), the central panel LAST — and that order
        // decides both the layout and the auto-ids, so these calls are not reorderable.
        self.toolbar_panel(ui, &tk);
        self.editor_panel(ui, &tk);
        // Rail FIRST so it hugs the window edge; the tools panel lands to its left (`tool_rail`).
        self.tool_rail(ui, &tk);
        if !self.tools_collapsed {
            self.tools_panel(ui);
        }
        self.maybe_persist_workspace();
        self.central_panel(ui);
    }

    /// The Ctrl/Cmd chords (the caller has already seen the command modifier held): Enter runs the
    /// backtest, S saves the editor buffer, Slash cycles the right-hand tool tab.
    fn apply_shortcuts(&mut self, ui: &egui::Ui) {
        let (run_pressed, save_pressed, cycle_pressed) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::Slash),
            )
        });
        if run_pressed
            && !self.running
            && self.picker.selected().is_some()
            && self.run_blocked_reason().is_none()
        {
            self.start_run();
        }
        if save_pressed {
            // Ctrl+S with an empty name box re-saves under the LAST saved name (normal
            // editor Save semantics), falling back to "untitled" only when this session has
            // never saved. The old always-"untitled" fallback silently misfiled follow-up
            // saves: save "momo-1" (name box clears), keep editing, Ctrl+S again → the edits
            // landed under "untitled" while "momo-1" kept the stale code.
            if self.saved.save_name.trim().is_empty() {
                self.saved.save_name =
                    self.last_saved_name.clone().unwrap_or_else(|| "untitled".to_string());
            }
            self.handle_saved_action(SavedAction::SaveCurrent);
        }
        if cycle_pressed {
            self.right_tab = next_tab(self.right_tab);
            // Cycling while collapsed used to mutate an invisible tab with zero on-screen
            // feedback (the rail highlight requires an expanded panel) — expand like a rail
            // click does, so the shortcut always shows its effect.
            self.tools_collapsed = false;
        }
    }

    /// The top toolbar panel: the primary row (brand, slice picker, strategy chip, Run, Refresh,
    /// running status), the backend selector line, and — for the named backend — its roster row.
    fn toolbar_panel(&mut self, ui: &mut egui::Ui, tk: &Tokens) {
        egui::Panel::top("studio-toolbar").show(ui, |ui| {
            ui.add_space(3.0);
            self.toolbar_primary_row(ui, tk);
            self.toolbar_backend_row(ui, tk);
            self.toolbar_named_roster_row(ui, tk);
            ui.add_space(3.0);
        });
    }

    /// The toolbar's first line: brand block and data-slice picker on the left, the strategy chip,
    /// Run and Refresh after them, the transient running status right-aligned.
    fn toolbar_primary_row(&mut self, ui: &mut egui::Ui, tk: &Tokens) {
        ui.horizontal(|ui| {
            // Brand block: the Studio glyph + name, then the data-slice
            // picker and the ONE primary action (Run). Everything transient (spinner, Cancel)
            // is right-aligned so the left half of the bar never jumps around mid-run.
            let title_px = tk.text.px(TextRole::Title);
            ui.label(icons::STUDIO.rich().size(title_px).color(tk.theme.text2));
            ui.label(egui::RichText::new("Studio").strong().size(title_px));
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            self.picker.ui(ui);
            ui.add_space(4.0);
            self.toolbar_strategy_chip(ui);
            ui.add_space(4.0);
            self.toolbar_run_button(ui);
            self.toolbar_refresh_button(ui);
            self.toolbar_running_status(ui);
        });
    }

    /// The strategy chip: which source Run will execute; clicking it jumps to the Strategy tab.
    fn toolbar_strategy_chip(&mut self, ui: &mut egui::Ui) {
        // WHICH strategy Run will execute. Without this the toolbar looks identical in
        // both modes while running completely different code — clicking it jumps to the
        // Strategy tab. The STRATEGY icon, then the source's name: a kit secondary
        // button, whose words (and the icon among them) are the Strong role.
        let (chip_text, chip_tip) = match self.strategy_source {
            StrategySource::Rhai => {
                ("rhai".to_string(), "Running the editor buffer — click to change")
            }
            StrategySource::Native => (
                self.native_name().to_string(),
                "Running a native registry strategy — click to change",
            ),
            StrategySource::Plugin => (
                if self.plugin_name.is_empty() {
                    "plugin".to_string()
                } else {
                    self.plugin_name.clone()
                },
                "Running a runtime-loaded Rust plugin — click to change",
            ),
        };
        if ui
            .add(ActionButton::secondary((icons::STRATEGY, chip_text)))
            .on_hover_text(chip_tip)
            .clicked()
        {
            self.right_tab = RightTab::Strategy;
            self.tools_collapsed = false;
        }
    }

    /// The ONE primary action, Run, disabled with its reason while nothing can run.
    fn toolbar_run_button(&mut self, ui: &mut egui::Ui) {
        let mut run = ActionButton::primary((icons::RUN, "Run"));
        if let Some(why) = self.run_disabled_reason() {
            run = run.disabled_because(why);
        }
        if ui.add(run).on_hover_text("Run backtest  (Ctrl+Enter)").clicked() {
            self.start_run();
        }
    }

    /// The Refresh button, which spawns the catalog re-scan on a worker and disables itself while
    /// that is in flight.
    fn toolbar_refresh_button(&mut self, ui: &mut egui::Ui) {
        // Refresh spawns the catalog walk on a worker thread (`spawn_catalog_refresh`)
        // rather than running it in this frame — see `crate::catalog`'s module doc. While
        // it is in flight the button is DISABLED and says so: the walk it is waiting on
        // may be a remote connect, and a button that looks clickable but silently no-ops
        // reads as a broken button rather than as a busy one.
        let scanning = self.catalog_scanning();
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
            self.spawn_catalog_refresh(ui.ctx());
        }
    }

    /// The right-aligned transient status: Cancel, a spinner and what is running, only while
    /// something is.
    fn toolbar_running_status(&mut self, ui: &mut egui::Ui) {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if self.any_running() {
                if ui
                    .button((icons::CANCEL, "Cancel"))
                    .on_hover_text("Abandon in-flight runs")
                    .clicked()
                {
                    self.cancel();
                }
                ui.spinner();
                ui.label(
                    egui::RichText::new(if self.running {
                        "running backtest…"
                    } else if self.sweep_rx.is_some() {
                        "running sweep…"
                    } else if self.wf_rx.is_some() {
                        "running walk-forward…"
                    } else if self.study_rx.is_some() {
                        "running study…"
                    } else if self.build_rx.is_some() {
                        "building plugin…"
                    } else {
                        "comparing saved…"
                    })
                    .weak(),
                );
            }
        });
    }

    /// The backend selector (second toolbar line): where a Run/Sweep/Walk-Forward executes, and
    /// the compute daemon's address for whichever backend is picked.
    fn toolbar_backend_row(&mut self, ui: &mut egui::Ui, tk: &Tokens) {
        // The backend selector (second toolbar line): WHERE a Run/Sweep/Walk-Forward
        // executes. TWO choices, both of them the COMPUTE daemon - `Remote` (a script this
        // shell sends) or `Named` (a strategy that daemon already holds). Kept off the busy
        // primary row above.
        //
        // ⚠ There was a THIRD button here, "Local", and deleting `Backend::Local` without it
        // left a live defect for one commit: the button tested `!is_remote`, so once `Local`
        // was gone it lit up whenever the backend was NAMED - labelling the named-run
        // backend "Local" - and clicking it silently switched the user to Remote. A
        // boolean that USED to mean "not remote, therefore local" does not survive the
        // removal of the third state it was quietly assuming.
        // `docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Backend").weak());
            // ⚠ **THE NAMED BACKEND — the one that works with the OBSERVE key alone**
            // (`docs/decisions/0064-a-named-run-carries-no-source.md`). Remote above needs the
            // CONTROL key: every `Run*` verb is `Scope::Write` because it compiles
            // client-supplied Rhai or runs a built artifact, while `ListStrategies` plus the
            // named run are the Observe half. ⚠ Until 2026-09-26 this said this process could
            // never hold that key; the owner then ruled that it may, for Studio's COMPUTE dial
            // only (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1), as
            // `Self::compute_key` — so Remote works against a keyed daemon exactly when the
            // launcher handed that key over, and Named remains the backend that needs none.
            // (The key pair is ONE pair for both daemons, one domain separator; the daemon both
            // backends dial is the COMPUTE one.)
            let mut kind = self.backend_kind();
            let changed = segmented::segmented(
                ui,
                &mut kind,
                &[
                    Segment { value: BackendKind::Remote, label: "Remote", why: REMOTE_WHY },
                    Segment { value: BackendKind::Named, label: "Named", why: NAMED_WHY },
                ],
            );
            if changed {
                self.switch_backend(kind);
            }
            if let Backend::Remote { addr } = &mut self.backend {
                ui.add_sized(
                    [160.0, tk.metrics.control_h],
                    egui::TextEdit::singleline(addr)
                        .font(tk.mono(TextRole::Body))
                        .hint_text("host:port"),
                )
                // ⚠ NOT the datahub, and this tooltip said it was. `Backend::Remote`'s own
                // doc records the same correction on 2026-09-20: the three verbs this
                // backend sends are `Plane::Compute`, which the data daemon refuses BY
                // PLANE - it only ever DEFAULTED to the datahub's port. The doc was fixed
                // and the tooltip an operator actually reads was not.
                .on_hover_text(
                    "COMPUTE daemon address (host:port) - `vike-backend backtest --addr`, \n                         a different process and port from the datahub",
                );
            }
            if let Backend::Named { addr } = &mut self.backend {
                let edited = ui
                    .add_sized(
                        [160.0, tk.metrics.control_h],
                        egui::TextEdit::singleline(addr)
                            .font(tk.mono(TextRole::Body))
                            .hint_text("host:port"),
                    )
                    .on_hover_text(
                        "COMPUTE daemon address (`vike-backend backtest --addr`) — a DIFFERENT \
                             credential and ask from the Remote backend, which dials the SAME \n                             daemon and port",
                    )
                    .changed();
                if edited {
                    self.named_roster = None;
                    self.named_roster_rx = None;
                }
            }
        });
    }

    /// The named backend's own row (only while that backend is picked): the Roster button and the
    /// strategies the daemon reported. Folds a finished fetch in BEFORE rendering.
    fn toolbar_named_roster_row(&mut self, ui: &mut egui::Ui, tk: &Tokens) {
        // The named backend's own row: the roster this daemon would run, and — when its
        // operator has not armed the lane — the SWITCH rather than an empty list.
        if let Backend::Named { addr } = &self.backend {
            let addr = addr.clone();
            // Fold in a finished fetch before rendering, so the answer appears on the frame it
            // lands rather than the one after.
            if let Some(rx) = &self.named_roster_rx
                && let Ok(answer) = rx.try_recv()
            {
                self.named_roster = Some(answer);
                self.named_roster_rx = None;
            }
            let fetching = self.named_roster_rx.is_some();
            ui.horizontal_wrapped(|ui| self.named_roster_contents(ui, tk, addr, fetching));
        }
    }

    /// The roster row's contents: the Roster button, then `asking…`, the fetch error, the unarmed
    /// note, or one selectable per strategy. `addr` is the daemon address the button dials;
    /// `fetching` is whether a dial is already in flight.
    fn named_roster_contents(
        &mut self,
        ui: &mut egui::Ui,
        tk: &Tokens,
        addr: String,
        fetching: bool,
    ) {
        // ⚠ The dial goes to a WORKER, never this thread — see `named_roster_rx`. The
        // button is disabled while one is in flight so a click cannot start a second.
        if ui
            .add_enabled(!fetching, egui::Button::new("Roster").small())
            .on_hover_text("Ask this daemon which strategies it can run")
            .on_disabled_hover_text("Asking this daemon…")
            .clicked()
        {
            self.named_roster_rx = Some(crate::remote::spawn_named_roster_remote(
                addr.clone(),
                self.named_run_keys.clone(),
            ));
        }
        if fetching {
            ui.label(egui::RichText::new("asking…").weak());
            return;
        }
        match &self.named_roster {
            None => {
                ui.label(egui::RichText::new("roster not fetched").weak());
            }
            Some(Err(e)) => {
                ui.label(egui::RichText::new(e.clone()).color(Status::Error.color()));
            }
            // ⚠ An UNARMED daemon WITHHOLDS the roster (it answers `armed: false`
            // with an EMPTY list — arming gates the NAMES as well as the run, 0064's
            // decision 8 leg 3). Render the SWITCH, never that empty list — the
            // teaching-refusal rule, and the reason this outcome is a success on the
            // wire rather than an error.
            Some(Ok(roster)) if !roster.armed => {
                ui.label(
                    egui::RichText::new(
                        vike_datahub_client::named_run::NamedRoster::unarmed_note(),
                    )
                    .color(Status::Warning.color()),
                );
            }
            Some(Ok(roster)) => {
                for name in &roster.strategies {
                    if ui
                        .selectable_label(
                            self.named_strategy.as_deref() == Some(name.as_str()),
                            // A selectable's words default to the Button style (the
                            // Strong role), so the Body role is named, not implied.
                            egui::RichText::new(name).size(tk.text.px(TextRole::Body)),
                        )
                        .clicked()
                    {
                        self.named_strategy = Some(name.clone());
                    }
                }
            }
        }
    }

    /// The left editor panel: a 28px expand strip while collapsed, otherwise the resizable panel
    /// holding the header row and the editor buffer.
    fn editor_panel(&mut self, ui: &mut egui::Ui, tk: &Tokens) {
        if self.editor_collapsed {
            // NOTE: a DIFFERENT panel id than the expanded editor — egui persists panel width
            // by id, so sharing one id would store this 28px width and reopen the expanded
            // editor as a min-width sliver instead of at the user's dragged width.
            egui::Panel::left("studio-editor-min").exact_size(28.0).resizable(false).show(
                ui,
                |ui| {
                    ui.add_space(4.0);
                    ui.vertical_centered(|ui| {
                        if icons::named(ui.small_button(icons::EXPAND_PANEL), "Expand editor")
                            .clicked()
                        {
                            self.editor_collapsed = false;
                        }
                    });
                },
            );
        } else {
            egui::Panel::left("studio-editor").resizable(true).default_size(460.0).show(ui, |ui| {
                ui.add_space(2.0);
                self.editor_header_row(ui, tk);
                self.editor_body(ui);
            });
        }
    }

    /// The editor panel's header line: the collapse button, the title, the compile-status chip and
    /// the unsaved-changes chip.
    fn editor_header_row(&mut self, ui: &mut egui::Ui, tk: &Tokens) {
        ui.horizontal(|ui| {
            if icons::named(ui.small_button(icons::COLLAPSE_PANEL), "Collapse editor").clicked() {
                self.editor_collapsed = true;
            }
            ui.label(egui::RichText::new("Editor").strong().size(tk.text.px(TextRole::Title)));
            ui.add_space(4.0);
            // Compile-status chip: green "compiles" when the last-compiled source was
            // clean, red "line N" (full message on hover) otherwise — same facts as the
            // old glance-dots, now readable without hovering.
            match self.rhai_verdict() {
                Some(Ok(())) => {
                    chip::badge(ui, "● compiles", Status::Ok).on_hover_text("Compiles OK");
                }
                Some(Err(msg)) => {
                    let label = match crate::editor::error_line(msg) {
                        Some(line) => format!("● error · line {line}"),
                        None => "● error".to_string(),
                    };
                    chip::badge(ui, &label, Status::Error).on_hover_text(msg.as_str());
                }
                // No Rhai verdict is held: the buffer is a Plugin's RUST source
                // (`buffer_is_rhai`). Say what IS known — its language, and where its
                // verdict comes from — in the muted status, because nothing here has
                // judged it. The Build result in the Strategy pane is the authority.
                None => {
                    chip::badge(ui, PLUGIN_EDITOR_CHIP, Status::Muted).on_hover_text(
                        "Plugin mode: this buffer is Rust, which the Rhai check cannot \
                         judge. Build (Strategy tab) compiles it — its result there is \
                         the verdict.",
                    );
                }
            }
            // Unsaved-changes chip: amber while the current mode's own baseline has
            // drifted. Native compares its OWN (name, params) baseline
            // (`native_is_dirty`) rather than the parked editor buffer every other mode
            // uses — see `native_saved`'s doc for why the two cannot share one check. The
            // hover says what Ctrl+S would really do about it (`unsaved_chip_tip`).
            let dirty = match self.strategy_source {
                StrategySource::Native => self.native_is_dirty(),
                StrategySource::Rhai | StrategySource::Plugin => {
                    self.editor.source != self.saved_source
                }
            };
            if dirty {
                chip::badge(ui, "● unsaved", Status::Warning)
                    .on_hover_text(self.unsaved_chip_tip());
            }
        });
    }

    /// Below the editor header: the inline compile-error banner (when the verdict is an error),
    /// then the editor buffer filling the height that is left.
    fn editor_body(&mut self, ui: &mut egui::Ui) {
        // Inline compile-error banner ABOVE the editor (under the header), not below it:
        // the chip is a glance affordance, but a failing compile deserves a message
        // visible without hovering — and above the editor it can never be pushed off the
        // panel bottom if the editor's row estimate runs long.
        if let Some(Err(msg)) = self.rhai_verdict() {
            let words = egui::RichText::new(crate::editor::format_compile_error(msg))
                .color(Status::Error.color());
            ui.label(icons::FAILED.before(ui.style(), words));
        }
        ui.add_space(2.0);
        // The editor fills whatever height is left.
        self.editor.ui_sized(ui, ui.available_height().max(80.0));
    }

    /// The activity-bar rail: one icon button per tool tab, shown even while the tools panel is
    /// collapsed. Must be added BEFORE `tools_panel` so it is the one hugging the window edge.
    fn tool_rail(&mut self, ui: &mut egui::Ui, tk: &Tokens) {
        // The tool rail + the tools panel are two SIBLING right panels (VS Code activity-bar
        // shape), replacing the old single panel that hand-partitioned its width. Two reasons:
        // the rail can no longer be pushed off-screen by over-wide pane content (the pre-redesign
        // clipping bug — `ui.set_width` is only a minimum, so a 380px row in a 300px column shoved
        // the rail past the panel edge), and the rail stays visible while the tools panel is
        // collapsed, so the tools are always one click away. Panel::right order matters: the rail
        // is added FIRST so it hugs the window edge; the tools panel lands to its left.
        const RAIL_WIDTH: f32 = 40.0;
        egui::Panel::right("studio-rail").exact_size(RAIL_WIDTH).resizable(false).show(ui, |ui| {
            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                for tab in RightTab::ALL {
                    let active = self.right_tab == tab && !self.tools_collapsed;
                    let ink = if active { tk.theme.accent } else { tk.theme.text3 };
                    let icon = tab.icon().rich().size(tk.text.px(TextRole::Title)).color(ink);
                    let button = egui::Button::new(icon)
                        .min_size(egui::vec2(
                            RAIL_WIDTH - 8.0,
                            tk.metrics.control_h + tk.metrics.gap,
                        ))
                        .fill(if active { tk.theme.surface } else { egui::Color32::TRANSPARENT })
                        .frame(active);
                    let resp = icons::named(ui.add(button), tab.label());
                    if active {
                        let mut bar = resp.rect;
                        bar.set_right(bar.left() + 2.0);
                        ui.painter().rect_filled(bar, 0.0, tk.theme.accent);
                    }
                    if resp.clicked() {
                        // VS Code behavior: clicking the active tool's icon toggles the panel;
                        // clicking any other icon selects it (expanding if collapsed).
                        if active {
                            self.tools_collapsed = true;
                        } else {
                            self.right_tab = tab;
                            self.tools_collapsed = false;
                        }
                    }
                    ui.add_space(2.0);
                }
            });
        });
    }

    /// The shared right-hand tools panel (the caller skips it while collapsed): the Data pane
    /// manages its own scrolling, every other tool's pane sits in one salted vertical scroll.
    fn tools_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::right("studio-tools").exact_size(340.0).resizable(false).show(ui, |ui| {
            if self.right_tab == RightTab::Data {
                // The Data pane manages its own scrolling (the shared catalog grid brings an
                // internal vertical ScrollArea, and the pane adds a horizontal wrap for the
                // grid's ~690px natural width) — nesting that inside the shared vertical
                // ScrollArea below would hand the internal scroll unbounded height.
                ui.add_space(4.0);
                if let Some((venue, symbol, interval)) = self.data_browser.ui(ui) {
                    self.picker.select_by_key(&venue, &symbol, &interval);
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
                        ui.add_space(4.0);
                        self.tool_pane_ui(ui);
                        ui.add_space(6.0);
                    });
            }
        });
    }

    /// The pane the current tool tab names — ONE arm per `RightTab`, so a swapped arm shows the
    /// operator the wrong tool. `RightTab::Data` never reaches it (`tools_panel` draws that pane
    /// outside the shared scroll).
    fn tool_pane_ui(&mut self, ui: &mut egui::Ui) {
        match self.right_tab {
            RightTab::Sweep => self.sweep_pane_ui(ui),
            RightTab::Strategy => self.strategy_pane_ui(ui),
            RightTab::Data => unreachable!("handled above"),
            RightTab::Indicators => {
                // The pane returns true the frame "Compute" is clicked; loading
                // bars + compute happens here (the pane never touches the store)
                // so its logic stays testable.
                if self.indicators.ui(ui) {
                    self.compute_indicator_preview();
                }
            }
            RightTab::Saved => {
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
            RightTab::Research => {
                // Scanned LAZILY, on the first frame this tool is opened: a
                // session that never opens it reads no directory at all, and
                // a session that does gets a list without having to press
                // Rescan first.
                self.research.ensure_scanned();
                let window = self.study_window_label();
                let running = self.study_rx.is_some();
                // Bound BEFORE the match: `ui` borrows the pane mutably, and
                // both arms below then touch `self` again.
                let action = self.research.ui(ui, &window, running);
                match action {
                    Some(ResearchAction::Refresh) => self.research.refresh(),
                    Some(ResearchAction::RunStudy) => self.start_study(),
                    None => {}
                }
            }
            RightTab::Chat => self.chat_pane_ui(ui),
        }
    }

    /// The central results panel: the study surface or the backtest surface, each naming its own
    /// producer (see [`CenterView`]).
    fn central_panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| match self.center {
            // TWO surfaces, each naming its own producer — see [`CenterView`] for why a study's
            // numbers and a backtest's may not share one.
            CenterView::Study => self.study_center(ui),
            CenterView::Backtest => self.backtest_center(ui),
        });
    }

    /// The backtest surface: the single run's results (else the sweep's best entry) through
    /// `results_ui`, else the first failure with an honest title, else the empty state.
    fn backtest_center(&mut self, ui: &mut egui::Ui) {
        // Bound FIRST, and bound to an `f64`: `display_periods_per_year` reads the whole
        // of `self`, while `results_ui` below is handed `&mut self.tab` alongside three
        // shared field borrows. Taking the factor as a value ends that read before any of
        // them start.
        let periods_per_year = self.display_periods_per_year();
        let sweep = self.sweep_last.as_ref().and_then(|r| r.as_ref().ok());
        let wf = self.wf_last.as_ref().and_then(|r| r.as_ref().ok());
        let single = self.last.as_ref().and_then(|r| r.as_ref().ok());
        // show the single-run result if present, else the sweep's best entry (so Equity/Trades
        // have data to render even when the user only ran a sweep).
        let r = single.or_else(|| sweep.map(|s| &s.entries[s.best_index].result));
        match r {
            Some(res) => results_ui(ui, &mut self.tab, res, sweep, wf, periods_per_year),
            None => {
                // Which failure to surface, with an honest per-source title (the old fixed
                // "Run failed" header misattributed sweep failures). Walk-forward errors are
                // included too — previously a failed Walk-Forward was completely invisible
                // (read only via `.ok()`).
                let err = match (&self.last, &self.sweep_last, &self.wf_last) {
                    (Some(Err(e)), _, _) => Some(("Run failed", format!("{e}"))),
                    (_, Some(Err(e)), _) => Some(("Sweep failed", format!("{e}"))),
                    (_, _, Some(Err(e))) => Some(("Walk-forward failed", format!("{e}"))),
                    _ => None,
                };
                match err {
                    Some((title, msg)) => Self::error_state(ui, title, &msg),
                    None => self.empty_state(ui),
                }
            }
        }
    }
}
