//! **The Research pane**: the backtest server's studies, the ONE button that runs one THERE, and the
//! runs the server has kept — the Studio's sibling of the Sweep pane, and a thin client of the backtest
//! server the Studio already uses (the owner's ruling of 2026-11-19: studies run ONLY on the server).
//!
//! It lists, runs and reads through `crates/vike-studio/src/backend/backtesthub/studies.rs`; the shell
//! holds the receivers and hands each answer to this pane (`StudioState::poll`). Nothing here reads,
//! moves or deletes this PC's own `user_data/research/studies` or `user_data/runs`: a study a user
//! wants to run is copied into the server's study folder, which the pane's top line names.
//!
//! # What it is a sibling OF
//!
//! `crates/vike-studio/src/studio/tool_panes.rs`'s Sweep pane is the shape: a `pane_header`, a
//! picker, one primary button (`vike_ui_theme::components::button::ThemeButtonAction::primary`), and a
//! worker-thread dispatch whose answer the shell's `poll()` folds in. The result lands in the same
//! central panel the backtest results land in.
//!
//! # ONE picker for BOTH tiers
//!
//! The server lists the `rhai` tier and the `rust` tier as rows of one list
//! (`vike_backtesthub_client::study::WireStudyListing`), differing only in a [`StudyTierWire`] tag. So
//! this pane has ONE list and ONE Run button; the tier is a BADGE, sent back with the run, and what it
//! means (a Rhai study runs inside the server, a Rust study file in its sandboxed child) is the
//! server's business.
//!
//! # ⚠ R6: one results surface, and the numbers do NOT come to the list
//!
//! `crates/vike-studio-core/src/study_run.rs` spends a section on this: a study's Sharpe is
//! vectorized and a backtest's is event-driven with real fills, so the two must never end up in one
//! shared column. It defends that structurally, by NESTING a study's metrics under
//! `detail.metrics`, and its doc names the failure it is defending against — *"a top-level `sharpe`
//! would have been renderable in a shared column by a listing that never asked which scorer
//! produced it"*.
//!
//! This pane honours that in the only way a renderer can:
//!
//! * **A runs-list row is a `vike_backtesthub_client::study::WireRunRow`, which carries no metric at
//!   all**: the COMMON manifest fields and nothing else, so the shared runs list has no column a
//!   number could be put in — and [`tests::a_study_runs_metrics_never_reach_the_shared_runs_list`] is
//!   the gate, over a server page that smuggles one in.
//! * **The `kind` is a BADGE on every row**, so a study row and a backtest row are told apart at a
//!   glance rather than by reading the producer name and inferring.
//! * **The numbers appear only in [`StudioResearchPane::study_result_ui`]**, a surface that is reached
//!   per-RUN, is titled as a study's, and renders `vike_studio_core::STUDY_METRICS_NOTE` verbatim
//!   beside them — the sentence the library wrote precisely so a manifest read ALONE still carries
//!   the distinction.

use vike_backtesthub_client::proto::StudyRunRequestWire;
use vike_backtesthub_client::study::{
    StudyTierWire, WireListedStudy, WireRunArtifact, WireRunPage, WireRunRow, WireStudyListing,
    WireStudyReport,
};
use vike_data::TimestampRange;
use vike_runs::{BACKTEST_RUN_KIND, RunManifest};
use vike_studio_core::study_run::StudyDetail;
use vike_studio_core::{STUDY_METRICS_NOTE, STUDY_RUN_KIND};
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, chip};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::studio;

/// What [`StudioResearchPane::ui`] asks the shell to do this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StudioResearchAction {
    /// Ask the server for both lists again.
    Refresh,
    /// Run the selected study on the server.
    RunStudy,
    /// Ask for the next page of older runs.
    OlderRuns,
}

/// The server's runs as the pane holds them: every page asked for so far, newest first.
#[derive(Debug, Default)]
struct StudioResearchRuns {
    /// The runs folder on the SERVER's box, shown verbatim (a path there, never on this PC).
    root: String,
    /// How many runs the folder holds in all.
    total: usize,
    /// How many pages are held, which is also the next page to ask for.
    pages: usize,
    /// The rows, newest first.
    rows: Vec<WireRunRow>,
    /// The last page's scan diagnostics, and how many the scan had in all.
    diagnostics: Vec<String>,
    diagnostic_total: usize,
    /// Why the last "Older runs" failed, shown under the rows it left in place.
    older_error: Option<String>,
}

impl StudioResearchRuns {
    /// Add `page` behind the rows held.
    ///
    /// shortcut: a run the server mints between two pages shifts the next page by one, so its oldest
    /// row may repeat; a finished study re-reads page 0, and a row keyed by `run_id` is the upgrade.
    fn append(&mut self, page: &WireRunPage) {
        self.root = page.root.clone();
        self.total = page.total;
        self.pages = page.page + 1;
        self.rows.extend(run_rows(page));
        self.diagnostics = page.diagnostics.clone();
        self.diagnostic_total = page.diagnostic_total;
        self.older_error = None;
    }
}

/// One page's rows, newest first — a pure map, so the "no numbers in the shared list" property is
/// testable without a frame. The server sends a page in scan order (oldest first, by directory
/// name, which for a minted id is chronological), so a reverse walk is the most-recent-first order a
/// person reads a results list in.
pub(crate) fn run_rows(page: &WireRunPage) -> Vec<WireRunRow> {
    page.runs.iter().rev().cloned().collect()
}

/// The badge text for a study's tier.
///
/// Exhaustive on purpose: a third tier is a compile error here rather than a row that silently
/// renders as one of the two that exist.
pub(crate) fn tier_label(t: StudyTierWire) -> &'static str {
    match t {
        StudyTierWire::Rhai => "rhai",
        StudyTierWire::Rust => "rust",
    }
}

/// The status a run's `kind` badge is drawn in.
///
/// Three answers rather than two, and the third is the point: a kind this build does not recognise
/// is a WARNING, under its own name, rather than being folded into one of the two it does —
/// `vike_runs::RunManifest::kind`'s own doc says the set of kinds grows in crates that cannot
/// see each other, so "I do not know this one" is an answer a reader needs. The two known kinds are
/// categories, not states, so neither is a success or a failure colour: a study is the info blue,
/// a backtest the muted grey (design system spec §3.2).
pub(crate) fn kind_status(kind: &str) -> ThemeStatus {
    match kind {
        STUDY_RUN_KIND => ThemeStatus::Info,
        BACKTEST_RUN_KIND => ThemeStatus::Muted,
        _ => ThemeStatus::Warning,
    }
}

/// `window` (epoch ms) as the server's bounds: unix SECONDS, or empty for unbounded on that side
/// (the whole store). The start rounds down and the end up, so the seconds cover every millisecond
/// of the picked slice.
pub(crate) fn window_seconds(window: TimestampRange) -> (String, String) {
    let seconds = |ms: Option<i64>, up: i64| {
        ms.map_or_else(String::new, |ms| ms.saturating_add(up).div_euclid(1000).to_string())
    };
    (seconds(window.start, 0), seconds(window.end, 999))
}

/// The backtest server's studies, the runs it has kept, and the one verb between them.
///
/// `Default` is DERIVED and is the honest starting state: nothing asked, nothing listed, nothing
/// selected. The shell asks the first time the tool is opened ([`Self::needs_listing`]).
#[derive(Default)]
pub struct StudioResearchPane {
    /// The backtest server the lists are of, as the shell last asked it; `None` until the tool is
    /// first opened.
    address: Option<String>,
    /// Whether that backend dials at the Write scope (the Source backend), which the runs list, a
    /// run and an artifact need. The Named backend dials Read only.
    compute: bool,
    /// The server's studies: `None` while the list is on its way.
    studies: Option<Result<WireStudyListing, String>>,
    /// Which study row is selected. Clamped on read, so a shrinking list is never an index panic.
    pub(crate) study_index: usize,
    /// `0` = run with no recipe; `n` = the selected study's recipe `n - 1`.
    pub(crate) recipe_index: usize,
    /// The server's runs: `None` while page 0 is on its way.
    runs: Option<Result<StudioResearchRuns, String>>,
    /// The page of runs in flight, if any.
    runs_fetching: Option<usize>,
    /// The artifact the result screen shows: its name, and its first page once read (`None` while
    /// reading), as text or the refusal.
    artifact: Option<(String, Option<Result<String, String>>)>,
}

impl StudioResearchPane {
    /// Whether the shell must ask for the lists: the tool has never asked, or the backend changed
    /// between Source and Named (which changes what may be read). An edited ADDRESS is not asked
    /// on its own — a keystroke is not a dial — and Rescan asks it.
    pub(crate) fn needs_listing(&self, compute: bool) -> bool {
        self.address.is_none() || self.compute != compute
    }

    /// The shell has asked `address` for the lists (`compute`: and for the runs): both show as
    /// loading until their answers land.
    pub(crate) fn begin_listing(&mut self, address: &str, compute: bool) {
        self.address = Some(address.to_string());
        self.compute = compute;
        self.studies = None;
        self.runs = None;
        self.runs_fetching = None;
    }

    /// The study list has answered. The recipe pick resets: recipe 2 of one listing has nothing to
    /// do with recipe 2 of the next, and carrying the index across is how a run ends up driven by a
    /// file the operator did not choose.
    pub(crate) fn apply_studies(&mut self, answer: Result<WireStudyListing, String>) {
        if let Ok(listing) = &answer {
            self.study_index = self.study_index.min(listing.studies.len().saturating_sub(1));
        }
        self.recipe_index = 0;
        self.studies = Some(answer);
    }

    /// The shell has asked for page `page` of the runs.
    pub(crate) fn fetching_runs(&mut self, page: usize) {
        self.runs_fetching = Some(page);
    }

    /// The page to ask for when the operator presses "Older runs".
    pub(crate) fn next_runs_page(&self) -> usize {
        match &self.runs {
            Some(Ok(runs)) => runs.pages,
            _ => 0,
        }
    }

    /// A page of runs has answered: page 0 replaces the list, a later page joins it, and a later
    /// page's refusal is shown under the rows it leaves in place.
    pub(crate) fn apply_runs(&mut self, answer: Result<WireRunPage, String>) {
        let older = self.runs_fetching.take().is_some_and(|page| page > 0);
        match (&mut self.runs, answer) {
            (Some(Ok(held)), Ok(page)) if older => held.append(&page),
            (Some(Ok(held)), Err(why)) if older => held.older_error = Some(why),
            (slot, answer) => {
                *slot = Some(answer.map(|page| {
                    let mut runs = StudioResearchRuns::default();
                    runs.append(&page);
                    runs
                }));
            }
        }
    }

    /// The runs held, newest first; empty while none are.
    pub fn runs(&self) -> &[WireRunRow] {
        match &self.runs {
            Some(Ok(runs)) => &runs.rows,
            _ => &[],
        }
    }

    /// The selected study, or `None` when the list is empty or not here.
    pub(crate) fn selected(&self) -> Option<&WireListedStudy> {
        let studies = &self.studies.as_ref()?.as_ref().ok()?.studies;
        studies.get(self.study_index.min(studies.len().saturating_sub(1)))
    }

    /// Why Run study is disabled, or `None` when it may go: the Named backend has no Write dial, a
    /// study is running, or none is selected.
    pub(crate) fn run_disabled_reason(&self, running: bool) -> Option<&'static str> {
        if !self.compute {
            return Some(STUDY_NEEDS_COMPUTE);
        }
        if running || self.selected().is_none() {
            return Some(RUN_DISABLED_TIP);
        }
        None
    }

    /// The `RunStudy` request for the selected study and its picked recipe over `window` (the picked
    /// slice's, in ms; `TimestampRange::all` for the whole store), or `None` with no study selected.
    /// The recipe goes by its FILE NAME and the server reads it there; the request asks for an
    /// unsaved result's numbers rather than an error.
    pub(crate) fn request(&self, window: TimestampRange) -> Option<StudyRunRequestWire> {
        let study = self.selected()?;
        let (from, to) = window_seconds(window);
        Some(StudyRunRequestWire {
            study: study.name.clone(),
            tier: Some(study.tier),
            recipe: self.recipe_index.checked_sub(1).and_then(|i| study.recipes.get(i)).cloned(),
            from,
            to,
            report_unsaved_result: true,
            ..StudyRunRequestWire::default()
        })
    }

    /// The result screen is reading `file`'s first page.
    pub(crate) fn reading_artifact(&mut self, file: &str) {
        self.artifact = Some((file.to_string(), None));
    }

    /// The artifact page the result screen asked for has answered.
    pub(crate) fn apply_artifact(&mut self, answer: Result<WireRunArtifact, String>) {
        if let Some((_, shown)) = &mut self.artifact {
            *shown = Some(answer.and_then(|artifact| artifact_text(&artifact)));
        }
    }

    /// A new study result replaces the old one, and with it the artifact it showed.
    pub(crate) fn clear_artifact(&mut self) {
        self.artifact = None;
    }

    /// Draw the pane. `window` is the human sentence describing which range a dispatch would ask
    /// about — the shell owns the slice picker, so it is the only thing that can say.
    ///
    /// Returns at most one action per frame, the `StudioSavedPane::ui` shape.
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        window: &str,
        running: bool,
    ) -> Option<StudioResearchAction> {
        crate::studio::pane_header(ui, icons::RESEARCH, "Research");
        let address = self.address.clone().unwrap_or_default();
        let mut action = None;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Studies").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button((icons::REFRESH, "Rescan")).on_hover_text(RESCAN_TIP).clicked() {
                    action = Some(StudioResearchAction::Refresh);
                }
            });
        });

        let mut picked: Option<usize> = None;
        match &self.studies {
            None => {
                ui.label(egui::RichText::new(loading_line(&address)).weak());
            }
            Some(Err(why)) => {
                ui.add(diagnostic_label(why));
            }
            Some(Ok(listing)) => {
                let line = format!(
                    "Studies run on the backtest server {address}. Its study folder: {}.",
                    listing.root
                );
                ui.add(egui::Label::new(egui::RichText::new(line).weak()).wrap());
                if listing.studies.is_empty() {
                    ui.add(egui::Label::new(egui::RichText::new(NO_STUDIES).weak()).wrap());
                }
                egui::ScrollArea::vertical()
                    .id_salt("studio-research-studies")
                    .max_height(studio::STUDY_LIST_MAX_HEIGHT)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (i, s) in listing.studies.iter().enumerate() {
                            let selected = i == self.study_index;
                            ui.horizontal(|ui| {
                                if ui.selectable_label(selected, s.name.as_str()).clicked() {
                                    picked = Some(i);
                                }
                                chip::badge(ui, tier_label(s.tier), ThemeStatus::Muted)
                                    .on_hover_text(tier_tip(s.tier));
                            });
                        }
                    });
                for d in &listing.diagnostics {
                    ui.add(diagnostic_label(d));
                }
            }
        }
        if let Some(i) = picked {
            self.study_index = i;
            self.recipe_index = 0;
        }

        ui.add_space(space::MD);
        let name = ui.label(egui::RichText::new("Recipe").weak()).id;
        // The selected study's recipe FILE NAMES, as the server listed them; index 0 is none.
        let recipes: Vec<String> = self.selected().map(|s| s.recipes.clone()).unwrap_or_default();
        egui::ComboBox::from_id_salt("studio-research-recipe")
            .width((ui.available_width() - space::LG).max(80.0))
            .show_index(ui, &mut self.recipe_index, recipes.len() + 1, |i| match i.checked_sub(1) {
                None => NO_RECIPE.to_string(),
                Some(i) => recipes[i].clone(),
            })
            .labelled_by(name);

        ui.add_space(space::MD);
        ui.label(egui::RichText::new(format!("Window · {window}")).weak());
        // The host's ceiling, stated where a study would hit it: the SERVER supplies (or lacks) the
        // learner and the backtest, and a study that needs one it lacks refuses by name.
        ui.add(egui::Label::new(egui::RichText::new(NO_LEARNER).weak()).wrap());

        ui.add_space(space::MD);
        ui.horizontal(|ui| {
            let mut run = ThemeButtonAction::primary((icons::RUN, "Run study"));
            if let Some(why) = self.run_disabled_reason(running) {
                run = run.disabled_because(why);
            }
            if ui.add(run).on_hover_text(RUN_TIP).clicked() {
                action = Some(StudioResearchAction::RunStudy);
            }
            if running {
                ui.spinner();
            }
        });

        ui.add_space(space::LG);
        ui.separator();
        ui.add_space(space::SM);
        ui.label(egui::RichText::new("Runs").strong());
        ui.add(egui::Label::new(egui::RichText::new(RUNS_BLURB).weak()).wrap());
        if !self.compute {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(STUDY_NEEDS_COMPUTE).color(ThemeStatus::Warning.color()),
                )
                .wrap(),
            );
            return action;
        }
        match &self.runs {
            None => {
                ui.label(egui::RichText::new(loading_line(&address)).weak());
            }
            Some(Err(why)) => {
                ui.add(diagnostic_label(why));
            }
            Some(Ok(runs)) => {
                if runs.rows.is_empty() {
                    ui.label(egui::RichText::new(NO_RUNS).weak());
                }
                egui::ScrollArea::vertical()
                    .id_salt("studio-research-runs")
                    .max_height(studio::RUN_LIST_MAX_HEIGHT)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for row in &runs.rows {
                            ui.horizontal(|ui| {
                                chip::badge(ui, &row.kind, kind_status(&row.kind));
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(row.run_id.as_str()).monospace(),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(format!(
                                    "{} · produced by {} · started {}{}",
                                    row.kind,
                                    row.produced_by,
                                    row.started_at,
                                    if row.has_report { "" } else { " · no report on disk" }
                                ));
                            });
                        }
                    });
                for d in &runs.diagnostics {
                    ui.add(diagnostic_label(d));
                }
                if runs.diagnostic_total > runs.diagnostics.len() {
                    ui.add(diagnostic_label(&format!(
                        "… and {} more the scan could not use",
                        runs.diagnostic_total - runs.diagnostics.len()
                    )));
                }
                if let Some(why) = &runs.older_error {
                    ui.add(diagnostic_label(why));
                }
                if runs.rows.len() < runs.total {
                    let fetching = self.runs_fetching.is_some();
                    if ui
                        .add_enabled(!fetching, egui::Button::new("Older runs").small())
                        .on_hover_text(format!("{} of {} runs shown", runs.rows.len(), runs.total))
                        .on_disabled_hover_text("Reading the runs…")
                        .clicked()
                    {
                        action = Some(StudioResearchAction::OlderRuns);
                    }
                }
                ui.add_space(space::SM);
                // Which TREE these rows are about: the server's runs folder, a path on ITS box,
                // shown as the server spelled it.
                ui.label(egui::RichText::new(runs.root.as_str()).weak().small());
            }
        }
        action
    }

    /// The central-panel surface for ONE study run on the server; returns the artifact a click asks
    /// to read (a saved run's, on a backend that may read it).
    ///
    /// ⚠ **Deliberately NOT `crates/vike-studio/src/panes/results/mod.rs`'s `results_ui`.** That
    /// surface renders a run summary — an equity curve, a trade list, `perf_rows`' metric catalog —
    /// and a study produces none of those things. Rendering a study's numbers through it would need a
    /// translation, and the translation is exactly the shared column
    /// `crates/vike-studio-core/src/study_run.rs` nests its metrics to prevent. Two surfaces, each
    /// naming its own producer, is the honest shape.
    ///
    /// A run the server could NOT save (`WireStudyReport::not_saved`) is the same screen: its reason
    /// in red, its numbers, and "Saved to" saying it was not.
    pub(crate) fn study_result_ui(
        &self,
        ui: &mut egui::Ui,
        report: &WireStudyReport,
    ) -> Option<String> {
        let tk = ThemeTokens::of(ui.ctx());
        let manifest =
            report.manifest.clone().and_then(|m| serde_json::from_value::<RunManifest>(m).ok());
        let kind = manifest.as_ref().map_or(STUDY_RUN_KIND, |m| m.kind.as_str());
        let readable = self.compute && report.run_id.is_some();
        let mut read = None;
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(STUDY_RESULT_TITLE)
                    .strong()
                    .size(tk.text.px(ThemeTextRole::Title)),
            );
            chip::badge(ui, kind, kind_status(kind)).on_hover_text(KIND_TIP);
            if let Some(run_id) = &report.run_id {
                ui.label(egui::RichText::new(run_id.as_str()).monospace());
            }
        });
        if let Some(why) = &report.not_saved {
            ui.add(diagnostic_label(&format!("Not saved: {why}")));
        }
        ui.separator();
        ui.add_space(space::SM);
        egui::ScrollArea::vertical()
            .id_salt("studio-study-result")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("studio-study-facts")
                    .num_columns(2)
                    .spacing([space::XL2, space::SM])
                    .show(ui, |ui| {
                        for (k, v) in fact_rows(report, manifest.as_ref()) {
                            ui.label(egui::RichText::new(k).weak());
                            ui.label(v);
                            ui.end_row();
                        }
                    });
                ui.add_space(space::LG);
                ui.label(egui::RichText::new("Metrics").strong());
                if report.metrics.is_empty() {
                    ui.label(egui::RichText::new(NO_METRICS).weak());
                }
                // ⚠ Each value prints through the SERVER's `text`, the ONE printed form every
                // surface shares (`vike_studio_core::study_run::StudyMetric`'s `text`): a non-finite
                // value is IEEE-754's own name, never a number it was rounded into.
                egui::Grid::new("studio-study-metrics")
                    .num_columns(2)
                    .spacing([space::XL2, space::SM])
                    .show(ui, |ui| {
                        for metric in &report.metrics {
                            ui.label(metric.name.as_str());
                            ui.label(egui::RichText::new(metric.text.as_str()).monospace());
                            ui.end_row();
                        }
                    });
                if !report.artifacts.is_empty() {
                    ui.add_space(space::LG);
                    ui.label(egui::RichText::new("Artifacts").strong());
                    for artifact in &report.artifacts {
                        let label = format!("{}  ({} bytes)", artifact.name, artifact.bytes);
                        let text = egui::RichText::new(label).monospace();
                        if !readable {
                            ui.label(text);
                            continue;
                        }
                        let open = matches!(&self.artifact, Some((n, _)) if *n == artifact.name);
                        if ui
                            .selectable_label(open, text)
                            .on_hover_text("Read its first page from the server")
                            .clicked()
                        {
                            read = Some(artifact.name.clone());
                        }
                    }
                }
                if let Some((name, shown)) = &self.artifact {
                    ui.add_space(space::MD);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(egui::RichText::new(name.as_str()).strong());
                        match shown {
                            None => {
                                ui.label(egui::RichText::new("Reading…").weak());
                            }
                            Some(Err(why)) => {
                                ui.add(diagnostic_label(why));
                            }
                            Some(Ok(text)) => {
                                egui::ScrollArea::vertical()
                                    .id_salt("studio-study-artifact")
                                    .max_height(studio::RUN_LIST_MAX_HEIGHT)
                                    .show(ui, |ui| {
                                        ui.label(egui::RichText::new(text.as_str()).monospace());
                                    });
                            }
                        }
                    });
                }
                ui.add_space(space::XL);
                // Verbatim, from the library constant — the sentence `study_run.rs` writes into every
                // study run's own `detail` so that a manifest read ALONE still carries the
                // distinction. Rendered here for the reader who never opens the file.
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(STUDY_METRICS_NOTE)
                                .color(ThemeStatus::Warning.color()),
                        )
                        .wrap(),
                    );
                });
            });
        read
    }
}

/// What the result screen shows for the first page of an artifact: its text when the bytes are
/// UTF-8 (a page that ends inside a character is still text, up to it), `binary` otherwise, and how
/// much of the file the page is when it is not all of it.
pub(crate) fn artifact_text(artifact: &WireRunArtifact) -> Result<String, String> {
    let bytes = artifact.data()?;
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(e) if e.error_len().is_none() => {
            std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => return Ok(format!("binary ({} bytes)", artifact.total)),
    };
    let shown = bytes.len() as u64;
    if artifact.offset == 0 && shown >= artifact.total {
        return Ok(text.to_string());
    }
    Ok(format!("{text}\n… the first {shown} of {} bytes", artifact.total))
}

/// The run's own facts, off the COMMON manifest plus the study subtree the kind entitles this surface
/// to read. A run that was not saved has no manifest, so it states only its study and why "Saved to"
/// is empty. Pure, so the rendering is a loop.
pub(crate) fn fact_rows(
    report: &WireStudyReport,
    manifest: Option<&RunManifest>,
) -> Vec<(&'static str, String)> {
    let saved_to = match (&report.dir, &report.not_saved) {
        (Some(dir), _) => dir.clone(),
        (None, Some(why)) => format!("NOT saved — {why}"),
        (None, None) => "?".to_string(),
    };
    let Some(manifest) = manifest else {
        return vec![("Study", report.study.clone()), ("Saved to", saved_to)];
    };
    // The writer's own type (`vike_studio_core::study_run::StudyDetail`), so a key the writer renames
    // is a compile error here rather than a blank row.
    let detail = StudyDetail::of(manifest);
    let bound = |v: Option<i64>| v.map_or_else(|| "unbounded".to_string(), |n| n.to_string());
    let window = detail.as_ref().map(|d| d.window);
    vec![
        ("Study", detail.as_ref().map_or_else(|| report.study.clone(), |d| d.study.clone())),
        ("Produced by", manifest.produced_by.clone()),
        ("Started", manifest.started_at.clone()),
        ("Finished", manifest.finished_at.clone()),
        (
            "Window",
            format!(
                "{} … {}",
                bound(window.and_then(|w| w.start)),
                bound(window.and_then(|w| w.end))
            ),
        ),
        (
            "Learner",
            if detail.is_some_and(|d| d.learner) { "supplied" } else { "none" }.to_string(),
        ),
        ("Saved to", saved_to),
    ]
}

/// The line a list shows while the server's answer is on its way.
fn loading_line(address: &str) -> String {
    format!("Loading from {address}…")
}

/// The copy for a server whose study folder holds no study.
pub(crate) const NO_STUDIES: &str = "No studies in that folder yet — a study is a FOLDER under its \
                              rhai/ (interpreted, runs inside the server) or rust/ (a study file \
                              the server builds and runs sandboxed). Copy a study folder there to \
                              run it.";

/// The runs list's own sentence: what it shows and, by omission, what it deliberately does not.
pub(crate) const RUNS_BLURB: &str = "Every run of every kind, told apart by the manifest's kind. Numbers \
                              live in the run, never in this list — a study's Sharpe and a \
                              backtest's are not the same measurement.";

/// The copy for a server whose runs folder is empty.
pub(crate) const NO_RUNS: &str = "No runs yet.";

/// The picker's index-0 entry: run the study with no recipe at all.
pub(crate) const NO_RECIPE: &str = "(no recipe)";

/// The server's ceiling, stated in the pane rather than discovered by a study that wanted to fit.
pub(crate) const NO_LEARNER: &str = "The server supplies the learner and the backtest when it has \
                              them; a study that needs one it lacks refuses by name.";

/// Run study's and the runs list's refusal on the Named backend, which dials the server at the Read
/// scope only (decision 0064): a run and a run's results need the Write dial.
pub const STUDY_NEEDS_COMPUTE: &str = "Running a study and reading its runs needs the compute \
                              connection (Write), which the Named backend does not dial — switch \
                              the backend to Source.";

const RESCAN_TIP: &str = "Ask the backtest server for its studies and runs again";
const RUN_TIP: &str = "Run this study on the backtest server; a saved run joins the list below";
const RUN_DISABLED_TIP: &str = "Pick a study first (and wait for the running one to finish)";

/// One diagnostic line, wrapped and in the error colour — a scan failure is a row, never an
/// absence.
fn diagnostic_label(text: &str) -> egui::Label {
    egui::Label::new(egui::RichText::new(text).color(ThemeStatus::Error.color())).wrap()
}

/// What a tier means to somebody choosing between two rows.
fn tier_tip(t: StudyTierWire) -> &'static str {
    match t {
        StudyTierWire::Rhai => {
            "Interpreted — compiled at run time from its folder, inside the server"
        }
        StudyTierWire::Rust => {
            "Compiled — a study file the server builds and runs in its sandboxed child"
        }
    }
}

/// The kind badge's hover text on the result screen.
const KIND_TIP: &str = "The manifest's kind — the one top-level field a reader may branch on";

/// The heading the study result screen opens with — a constant so a test can assert the study
/// surface is on screen without re-spelling it.
pub const STUDY_RESULT_TITLE: &str = "Study result";

/// The copy for a study that recorded nothing.
///
/// A study that legitimately found nothing is supposed to say so with a metric, so an EMPTY table is
/// worth naming rather than rendering as a blank gap.
pub(crate) const NO_METRICS: &str = "This study recorded no metric.";

#[cfg(test)]
mod tests;
