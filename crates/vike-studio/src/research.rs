//! **The Research pane**: the studies a user has written, the ONE button that runs one, and the
//! runs that came out — the Studio's sibling of the Sweep pane, over
//! `crates/vike-studio-core/src/study_dispatch.rs` instead of over the backtest engine.
//!
//! # What it is a sibling OF, and why that matters more than what it adds
//!
//! `crates/vike-studio/src/studio.rs`'s Sweep pane is the shape: a `section_header`, a picker, one
//! `theme::primary` verb, and a worker-thread dispatch whose result the shell's `poll()` folds in.
//! Nothing here invents a second way to do any of that — the dispatch goes through
//! `vike_studio_core::spawn_study`, which is built on the same `spawn_outcome` every other Studio
//! worker uses, and the result lands in the same central panel the backtest results land in.
//!
//! # ⚠ ONE picker for BOTH tiers — the whole point of the shared contract
//!
//! `crates/vike-studio-core/src/listing.rs`'s `list_studies` returns the `rhai` tier and the `rust`
//! tier as rows of one list, differing only in a [`StudyTier`] tag, and
//! `vike_studio_core::run_study_plan` holds the only `match` on that tag in the workspace. So this
//! pane has ONE list, ONE Run button and no notion of which tier it is about — the tier shows up as
//! a BADGE, because it is a fact a user acts on (an interpreted study runs on a shipped binary; a
//! compiled one needs the checkout it was built from), never as a second surface.
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
//! * **[`RunRow`] carries no metric at all.** It is built from the COMMON manifest fields and
//!   nothing else, so the shared runs list has no column a number could be put in — and
//!   [`tests::a_study_runs_metrics_never_reach_the_shared_runs_list`] is the gate, over a manifest
//!   whose `detail.metrics` is populated.
//! * **The `kind` is a BADGE on every row**, so a study row and a backtest row are told apart at a
//!   glance rather than by reading the producer name and inferring.
//! * **The numbers appear only in [`study_result_ui`]**, a surface that is reached per-RUN, is
//!   titled as a study's, and renders `vike_studio_core::STUDY_METRICS_NOTE` verbatim beside them —
//!   the sentence the library wrote precisely so a manifest read ALONE still carries the
//!   distinction.

use std::path::PathBuf;

use vike_backtest::runs::RunConfig;
use vike_data::TsRange;
use vike_studio_core::listing::{list_runs, list_studies, ListedStudy, RunListing, StudyTier};
use vike_studio_core::study_run::nonfinite_tag;
use vike_studio_core::{
    empty_params, read_recipe, recipes, StoreHandle, StudyRun, StudyRunPlan, STUDY_METRICS_NOTE,
};

/// Everything about the HOST a study run has to record, resolved by the BINARY and handed down.
///
/// Set on [`ResearchPane::host`] after construction, the same way `crates/vike-studio/src/studio.rs`
/// takes `store_is_remote` and `backend` — a library resolves no project root
/// (`crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` is the ratchet), and
/// `crates/vike-boot/tests/one_owner.rs` additionally forbids a booted root from WALKING a second
/// time, so `vike-app` derives these from the settings directory it already resolved
/// (`vike_model::state_path::user_data_dir_beside`) rather than calling `user_runs_dir`.
///
/// `None` on the pane is the ordinary state of a binary running from outside any project, and it
/// is DISCLOSED rather than papered over: with no host there is nowhere to list studies from and
/// nowhere to put a run, so the pane says so and arms nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StudyHost {
    /// `<project>/user_data/research/studies` — BOTH tiers live under it.
    pub studies_root: PathBuf,
    /// `<project>/user_data/runs` — the ONE runs namespace every producer mints into.
    pub runs_root: PathBuf,
    /// A directory a study may write scratch into. Created by the runner; never deleted by it.
    pub scratch: PathBuf,
    /// The BINARY doing this, spelled literally — `vike_backtest::runs::RunManifest::produced_by`
    /// carries why that is not `CARGO_PKG_NAME`.
    pub produced_by: String,
    /// The commit that binary was built from, or `None` when it cannot name one. The Studio's own
    /// crates do not depend on `vike-buildinfo`, so a host that can answer has to say so here.
    pub git_sha: Option<String>,
}

/// What [`ResearchPane::ui`] asks the shell to do this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResearchAction {
    /// Re-scan both listings.
    Refresh,
    /// Dispatch the selected study.
    RunStudy,
}

/// One run as the SHARED list renders it: the common manifest fields, and deliberately not one
/// number from any producer's kind-specific subtree. See this module's doc for the argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    /// The DIRECTORY name, which is the address the run is found at —
    /// `crates/vike-studio-core/src/listing.rs`'s `ListedRun::run_id` explains why it is the
    /// directory rather than the manifest's own field.
    pub run_id: String,
    /// The manifest's `kind`, verbatim. The one top-level field a reader may branch on, and the
    /// badge that tells a study row from a backtest row.
    pub kind: String,
    /// Which binary produced it.
    pub produced_by: String,
    /// When it started, RFC-3339 UTC as the manifest carries it.
    pub started_at: String,
    /// Whether the run's report file is there — the difference between "the result survived" and
    /// "nothing was kept", offered because a results surface can only open what exists.
    pub has_report: bool,
}

/// Every run in `listing`, as rows — a pure map, so the "no numbers in the shared list" property is
/// testable without a frame.
pub fn run_rows(listing: &RunListing) -> Vec<RunRow> {
    listing
        .runs
        .iter()
        .map(|r| RunRow {
            run_id: r.run_id.clone(),
            kind: r.manifest.kind.clone(),
            produced_by: r.manifest.produced_by.clone(),
            started_at: r.manifest.started_at.clone(),
            has_report: r.report.is_some(),
        })
        .collect()
}

/// The badge text for a study's tier.
///
/// Exhaustive on purpose: a third tier is a compile error here rather than a row that silently
/// renders as one of the two that exist.
pub fn tier_label(t: StudyTier) -> &'static str {
    match t {
        StudyTier::Rhai => "rhai",
        StudyTier::Rust => "rust",
    }
}

/// The colour a run's `kind` badge takes.
///
/// Three answers rather than two, and the third is the point: a kind this build does not recognise
/// is rendered in [`crate::theme::WARN`] under its own name rather than being folded into one of
/// the two it does — `vike_backtest::runs::RunManifest::kind`'s own doc says the set of kinds grows
/// in crates that cannot see each other, so "I do not know this one" is an answer a reader needs.
pub fn kind_color(kind: &str) -> egui::Color32 {
    match kind {
        vike_studio_core::STUDY_RUN_KIND => crate::theme::ACCENT,
        BACKTEST_RUN_KIND => crate::theme::OK,
        _ => crate::theme::WARN,
    }
}

/// The `kind` `crates/vike-backtest/src/backtest_cli.rs` writes.
///
/// Spelled here rather than imported because `vike-backtest` does not export it as a constant — a
/// finding worth one line of prose and not worth a library change: if it ever does, this becomes a
/// `use` and [`tests::the_two_known_kinds_are_coloured_apart`] keeps holding.
pub const BACKTEST_RUN_KIND: &str = "backtest";

/// One study's metrics, formatted for a table.
///
/// ⚠ A non-finite value renders as IEEE-754's own name through
/// `vike_studio_core::study_run::nonfinite_tag` — the same three tokens the manifest writes, called
/// rather than re-derived. `study_run.rs` spends a section on why a `NaN` Sharpe over a fold that
/// never traded is an OBSERVATION and must not be rounded into a number; a surface that printed it
/// as `0.00` would undo that at the last step.
pub fn metric_rows(run: &StudyRun) -> Vec<(String, String)> {
    run.outcome
        .metrics()
        .iter()
        .map(|(name, value)| {
            let text = match nonfinite_tag(*value) {
                Some(tag) => tag.to_string(),
                None => format!("{value:.6}"),
            };
            (name.clone(), text)
        })
        .collect()
}

/// The studies a user has written, the runs they produced, and the one verb between them.
///
/// `Default` is DERIVED and is the honest starting state: no host (the binary has not seeded one
/// yet), nothing scanned, nothing selected. A hand-written impl would have said the same thing at
/// more length — and clippy would have said so too.
#[derive(Default)]
pub struct ResearchPane {
    /// Where `user_data` is and who is running — set by the BINARY. See [`StudyHost`].
    pub host: Option<StudyHost>,
    /// The studies found by the last scan, both tiers, in the order `list_studies` returned them.
    studies: Vec<ListedStudy>,
    /// Everything that scan could not use, already rendered to one line each.
    study_diagnostics: Vec<String>,
    /// Which study row is selected. Clamped on read, so a shrinking list is never an index panic.
    pub study_idx: usize,
    /// The selected study's recipes. Index 0 of the picker is "no recipe" and is NOT in here.
    study_recipes: Vec<PathBuf>,
    /// `0` = run with no configuration; `n` = [`Self::study_recipes`]`[n - 1]`.
    pub recipe_idx: usize,
    /// The runs found by the last scan.
    runs: Vec<RunRow>,
    /// Everything THAT scan could not use, one line each.
    run_diagnostics: Vec<String>,
    /// True once [`Self::refresh`] has run at least once — so "no studies" is distinguishable from
    /// "nobody has looked yet", which are different sentences.
    scanned: bool,
}

impl ResearchPane {
    /// Re-scan both listings. A no-op with no host — there is nothing to scan and nothing to say
    /// that the pane's own copy does not already say.
    pub fn refresh(&mut self) {
        let Some(host) = self.host.clone() else { return };
        let studies = list_studies(&host.studies_root);
        self.study_diagnostics = studies.diagnostics.iter().map(|d| d.to_string()).collect();
        self.studies = studies.studies;
        self.study_idx = self.study_idx.min(self.studies.len().saturating_sub(1));
        self.rescan_recipes();
        self.refresh_runs();
        self.scanned = true;
    }

    /// Scan ONCE, the first time somebody looks.
    ///
    /// The shell calls this on every frame the Research tool is open, so a session that never
    /// opens it reads no directory at all and one that does gets a populated list without having
    /// to press Rescan first. Idempotent by [`Self::scanned`], which [`Self::refresh`] sets —
    /// so this is a no-op from the second frame on, and a no-op forever with no host.
    pub fn ensure_scanned(&mut self) {
        if !self.scanned {
            self.refresh();
        }
    }

    /// Whether either listing has been scanned yet — the difference between "no studies" and
    /// "nobody has looked", which are different sentences.
    pub fn scanned(&self) -> bool {
        self.scanned
    }

    /// Re-scan the RUNS only — what a finished study run needs, so its own row appears without
    /// re-reading every study folder.
    pub fn refresh_runs(&mut self) {
        let Some(host) = self.host.clone() else { return };
        let listing = list_runs(&host.runs_root);
        self.run_diagnostics = listing.diagnostics.iter().map(|d| d.to_string()).collect();
        self.runs = run_rows(&listing);
        self.scanned = true;
    }

    /// The selected study, or `None` when the list is empty.
    pub fn selected(&self) -> Option<&ListedStudy> {
        self.studies.get(self.study_idx.min(self.studies.len().saturating_sub(1)))
    }

    /// The studies found by the last scan.
    pub fn studies(&self) -> &[ListedStudy] {
        &self.studies
    }

    /// The runs found by the last scan.
    pub fn runs(&self) -> &[RunRow] {
        &self.runs
    }

    /// Re-read the selected study's recipes and reset the picker to "no recipe".
    ///
    /// Reset rather than preserved: recipe 2 of one study has nothing to do with recipe 2 of
    /// another, and carrying the index across a selection change is how a run ends up driven by a
    /// file the operator did not choose.
    fn rescan_recipes(&mut self) {
        self.study_recipes = match self.selected() {
            Some(s) => recipes(&s.dir),
            None => Vec::new(),
        };
        self.recipe_idx = 0;
    }

    /// Everything a dispatch needs, or `None` when this frame cannot dispatch: no host, or no
    /// study selected.
    ///
    /// The recipe is read HERE rather than on the worker thread so a broken TOML file is a refusal
    /// the pane can show beside the picker that chose it — [`Err`] carries that sentence.
    pub fn plan(
        &self,
        store: StoreHandle,
        window: TsRange,
    ) -> Option<Result<StudyRunPlan, String>> {
        let host = self.host.as_ref()?;
        let study = self.selected()?;
        let recipe = match self.recipe_idx.checked_sub(1).and_then(|i| self.study_recipes.get(i)) {
            Some(path) => match read_recipe(path) {
                Ok(params) => Some((params, path.clone())),
                Err(why) => return Some(Err(why)),
            },
            None => None,
        };
        let (params, config) = match recipe {
            Some((params, path)) => {
                let name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
                (params, RunConfig { path: Some(path.display().to_string()), name })
            }
            // A run with no recipe RECORDS that it had none, rather than recording a path it did
            // not use: `RunConfig`'s two `Option`s are both "the operator chose nothing here".
            None => (empty_params(), RunConfig { path: None, name: None }),
        };
        Some(Ok(StudyRunPlan {
            name: study.name.clone(),
            dir: study.dir.clone(),
            tier: study.tier,
            params,
            config,
            store,
            window,
            scratch: host.scratch.clone(),
            runs_root: host.runs_root.clone(),
            produced_by: host.produced_by.clone(),
            // The Studio cannot fit: its ML surface is the INFERENCE half only (`ml.rs`), and
            // `scripts/fetch_release_tools.sh` ships no trainer binary off Linux. Stated rather
            // than assumed, now that a host which CAN fit exists.
            learner: None,
            git_sha: host.git_sha.clone(),
        }))
    }

    /// Draw the pane. `window` is the human sentence describing which range a dispatch would ask
    /// about — the shell owns the slice picker, so it is the only thing that can say.
    ///
    /// Returns at most one action per frame, the `SavedPane::ui` shape.
    pub fn ui(&mut self, ui: &mut egui::Ui, window: &str, running: bool) -> Option<ResearchAction> {
        crate::theme::section_header(ui, "\u{1F52C}", "Research");
        let Some(host) = self.host.clone() else {
            ui.add(
                egui::Label::new(egui::RichText::new(NO_HOST).weak().color(crate::theme::WARN))
                    .wrap(),
            );
            return None;
        };
        let mut action = None;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Studies").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("\u{27F3} Rescan").on_hover_text(RESCAN_TIP).clicked() {
                    action = Some(ResearchAction::Refresh);
                }
            });
        });

        if self.studies.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(NO_STUDIES).weak()).wrap());
        }
        let mut picked: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_salt("studio-research-studies")
            .max_height(150.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (i, s) in self.studies.iter().enumerate() {
                    let selected = i == self.study_idx;
                    ui.horizontal(|ui| {
                        if ui.selectable_label(selected, s.name.as_str()).clicked() {
                            picked = Some(i);
                        }
                        crate::theme::chip(ui, crate::theme::ACCENT, tier_label(s.tier))
                            .on_hover_text(tier_tip(s.tier));
                    });
                }
            });
        if let Some(i) = picked {
            self.study_idx = i;
            self.rescan_recipes();
        }
        for d in &self.study_diagnostics {
            ui.add(diagnostic_label(d));
        }

        ui.add_space(6.0);
        ui.label(egui::RichText::new("Recipe").weak().size(11.0));
        let n = self.study_recipes.len() + 1;
        egui::ComboBox::from_id_salt("studio-research-recipe")
            .width((ui.available_width() - 8.0).max(80.0))
            .show_index(ui, &mut self.recipe_idx, n, |i| match i.checked_sub(1) {
                None => NO_RECIPE.to_string(),
                Some(i) => self.study_recipes[i]
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            });

        ui.add_space(6.0);
        ui.label(egui::RichText::new(format!("Window · {window}")).weak().size(11.0));
        // The host's ceiling, stated where a study would hit it rather than discovered by a study
        // that wanted to fit. `vike_studio_core::ml`'s doc: this crate's ML surface is inference.
        ui.add(egui::Label::new(egui::RichText::new(NO_LEARNER).weak().size(11.0)).wrap());

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let can = !running && self.selected().is_some();
            if ui
                .add_enabled(can, crate::theme::primary("\u{25B6} Run study"))
                .on_hover_text(if can { RUN_TIP } else { RUN_DISABLED_TIP })
                .clicked()
            {
                action = Some(ResearchAction::RunStudy);
            }
            if running {
                ui.spinner();
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Runs").strong());
        ui.add(egui::Label::new(egui::RichText::new(RUNS_BLURB).weak().size(11.0)).wrap());
        if self.runs.is_empty() && self.scanned {
            ui.label(egui::RichText::new(NO_RUNS).weak());
        }
        egui::ScrollArea::vertical()
            .id_salt("studio-research-runs")
            .max_height(220.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                // Newest first: `list_runs` sorts by directory NAME, which for a minted id is
                // chronological, so a reverse walk is the most-recent-first order a person reads a
                // results list in.
                for row in self.runs.iter().rev() {
                    ui.horizontal(|ui| {
                        crate::theme::chip(ui, kind_color(&row.kind), &row.kind);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(row.run_id.as_str()).monospace().size(11.0),
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
        for d in &self.run_diagnostics {
            ui.add(diagnostic_label(d));
        }
        ui.add_space(4.0);
        // Which TREE these rows are about — the fact that matters the moment `VIKE_USER_DATA_DIR`
        // is in play, and the one `RunListing::root` is carried for.
        ui.label(egui::RichText::new(path_line(&host.runs_root)).weak().size(10.0));
        action
    }
}

/// The copy for a Studio with no project above it — the ordinary state of a binary launched from
/// somewhere else, and one a user can act on.
pub const NO_HOST: &str = "No project folder above this working directory, so there is nowhere to \
                           read studies from and nowhere to put a run. Launch the app from inside \
                           a project, or set VIKE_SETTINGS_DIR to name one.";

/// The copy for a project that has no studies yet.
pub const NO_STUDIES: &str = "No studies yet — a study is a FOLDER under \
                              user_data/research/studies/rhai (interpreted, runs on this binary) \
                              or .../rust (compiled, needs a checkout).";

/// The runs list's own sentence: what it shows and, by omission, what it deliberately does not.
pub const RUNS_BLURB: &str = "Every run of every kind, told apart by the manifest's kind. Numbers \
                              live in the run, never in this list — a study's Sharpe and a \
                              backtest's are not the same measurement.";

/// The copy for a project whose runs directory is empty.
pub const NO_RUNS: &str = "No runs yet.";

/// The picker's index-0 entry: run the study with no configuration at all.
pub const NO_RECIPE: &str = "(no recipe)";

/// The host ceiling, stated in the pane rather than discovered by a study that wanted to fit.
pub const NO_LEARNER: &str = "This host supplies no learner: a study that must FIT refuses with \
                              NoLearner. Inference and importance still work.";

const RESCAN_TIP: &str = "Re-read user_data/research/studies and user_data/runs";
const RUN_TIP: &str = "Run this study and leave a run behind in user_data/runs";
const RUN_DISABLED_TIP: &str = "Pick a study first (and wait for the running one to finish)";

/// A filesystem path, rendered with ONE separator throughout.
///
/// ⚠ `Path::display()` prints components exactly as they were stored and `Path::join` appends the
/// PLATFORM separator, so a root that arrived as a forward-slash string — which is every
/// `VIKE_SETTINGS_DIR` / `VIKE_USER_DATA_DIR` value and every path this workspace's shell tooling
/// hands in — renders on Windows as `C:/Users/…/qa-root\user_data\runs`: both separators in one
/// line, changing halfway through.
///
/// Cosmetic, and still worth fixing, because this label exists to be READ. It is the answer to
/// "which tree are these rows about", and a path a human has to squint at fails the one job it has.
/// Normalised to `std::path::MAIN_SEPARATOR`, so Windows reads native and every other platform is
/// unchanged — there `/` is already the separator and the replace is a no-op.
///
/// Found by a real GPU capture of this pane, which is the only rung that can see it: the CPU-side
/// suites assert on `Path` values, never on the rendered string.
fn path_line(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    if std::path::MAIN_SEPARATOR == '/' {
        s
    } else {
        s.replace('/', std::path::MAIN_SEPARATOR_STR)
    }
}

/// One diagnostic line, wrapped and in the error colour — a scan failure is a row, never an
/// absence (`crates/vike-studio-core/src/listing.rs`'s module doc carries the rule).
fn diagnostic_label(text: &str) -> egui::Label {
    egui::Label::new(egui::RichText::new(text).color(crate::theme::ERR).size(11.0)).wrap()
}

/// What a tier means to somebody choosing between two rows.
fn tier_tip(t: StudyTier) -> &'static str {
    match t {
        StudyTier::Rhai => {
            "Interpreted — compiled at run time from the folder, so a shipped \
                            binary can run it"
        }
        StudyTier::Rust => {
            "Compiled — resolved through a registry generated at BUILD time, so it \
                            runs only on a binary built from a checkout that held it"
        }
    }
}

/// The central-panel surface for ONE study run.
///
/// ⚠ **Deliberately NOT `crates/vike-studio/src/results.rs`'s `results_ui`.** That surface renders
/// a `vike_backtest::BacktestResult` — an equity curve, a trade list, `perf_cells`' eleven
/// event-driven metrics — and a study produces none of those things. Rendering a study's numbers
/// through it would need a translation, and the translation is exactly the shared column
/// `crates/vike-studio-core/src/study_run.rs` nests its metrics to prevent. Two surfaces, each
/// naming its own producer, is the honest shape.
pub fn study_result_ui(ui: &mut egui::Ui, run: &StudyRun) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(STUDY_RESULT_TITLE).strong().size(15.0));
        crate::theme::chip(ui, kind_color(&run.manifest.kind), &run.manifest.kind)
            .on_hover_text("The manifest's kind — the one top-level field a reader may branch on");
        ui.label(egui::RichText::new(run.run_id.as_str()).monospace().size(11.0));
    });
    ui.separator();
    ui.add_space(4.0);
    egui::ScrollArea::vertical().id_salt("studio-study-result").auto_shrink([false, false]).show(
        ui,
        |ui| {
            egui::Grid::new("studio-study-facts").num_columns(2).spacing([16.0, 4.0]).show(
                ui,
                |ui| {
                    for (k, v) in fact_rows(run) {
                        ui.label(egui::RichText::new(k).weak());
                        ui.label(v);
                        ui.end_row();
                    }
                },
            );
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Metrics").strong());
            let rows = metric_rows(run);
            if rows.is_empty() {
                ui.label(egui::RichText::new(NO_METRICS).weak());
            }
            egui::Grid::new("studio-study-metrics").num_columns(2).spacing([16.0, 4.0]).show(
                ui,
                |ui| {
                    for (name, value) in rows {
                        ui.label(name);
                        ui.label(egui::RichText::new(value).monospace());
                        ui.end_row();
                    }
                },
            );
            let artifacts = run.outcome.artifacts();
            if !artifacts.is_empty() {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Artifacts").strong());
                for (name, body) in artifacts {
                    ui.label(
                        egui::RichText::new(format!("{name}  ({} bytes)", body.len()))
                            .monospace()
                            .size(11.0),
                    );
                }
            }
            ui.add_space(10.0);
            // Verbatim, from the library constant — the sentence `study_run.rs` writes into every
            // study run's own `detail` so that a manifest read ALONE still carries the
            // distinction. Rendered here for the reader who never opens the file.
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(STUDY_METRICS_NOTE)
                            .size(11.0)
                            .color(crate::theme::WARN),
                    )
                    .wrap(),
                );
            });
        },
    );
}

/// The heading [`study_result_ui`] opens with — a constant so a test can assert the study surface
/// is on screen without re-spelling it.
pub const STUDY_RESULT_TITLE: &str = "Study result";

/// The copy for a study that recorded nothing.
///
/// A study that legitimately found nothing is supposed to say so with a metric
/// (`crates/vike-studio-core/src/rhai_study/mod.rs`'s refusal message spells that rule), so an
/// EMPTY table is worth naming rather than rendering as a blank gap.
pub const NO_METRICS: &str = "This study recorded no metric.";

/// The run's own facts, off the COMMON manifest plus the study subtree the kind entitles this
/// surface to read. Pure, so the rendering is a loop.
pub fn fact_rows(run: &StudyRun) -> Vec<(&'static str, String)> {
    let detail = &run.manifest.detail;
    let window = &detail["window"];
    let bound = |v: &serde_json::Value| match v.as_i64() {
        Some(n) => n.to_string(),
        None => "unbounded".to_string(),
    };
    vec![
        ("Study", detail["study"].as_str().unwrap_or("?").to_string()),
        ("Produced by", run.manifest.produced_by.clone()),
        ("Started", run.manifest.started_at.clone()),
        ("Finished", run.manifest.finished_at.clone()),
        ("Window", format!("{} … {}", bound(&window["start"]), bound(&window["end"]))),
        (
            "Learner",
            match detail["learner"].as_bool() {
                Some(true) => "supplied".to_string(),
                _ => "none".to_string(),
            },
        ),
        ("Saved to", path_line(&run.dir)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_backtest::runs::{RunManifest, MANIFEST_FILE, REPORT_FILE};

    /// THE gate for [`path_line`], and it asserts the rendered LINE rather than that the function
    /// was called — so removing the normalisation reddens it.
    ///
    /// The input is the exact shape the defect appears in: a forward-slash root, which is how every
    /// `VIKE_SETTINGS_DIR` value arrives, with platform-joined children on top — precisely what
    /// `crates/vike-app/src/main.rs`'s `runs_root: user_data.join(RUNS_SUBDIR)` produces.
    #[test]
    fn a_rendered_path_never_mixes_separators() {
        let p =
            std::path::Path::new("C:/Users/the operator/scratch/qa-root").join("user_data").join("runs");
        let line = path_line(&p);
        assert!(
            !(line.contains('/') && line.contains('\\')),
            "a path a human is meant to READ must not change separator halfway: {line}"
        );
        assert!(line.ends_with("runs"), "the path itself is unchanged: {line}");
        // The control, and it is why this test is not vacuous: the RAW rendering must actually
        // exhibit the defect. It can only assert that where the two spellings differ — on a
        // platform whose separator is already `/` this would be a test of the host OS, not of the
        // code, so it says nothing there rather than asserting something it cannot mean.
        if std::path::MAIN_SEPARATOR != '/' {
            let raw = p.display().to_string();
            assert!(
                raw.contains('/') && raw.contains('\\'),
                "the defect must be reproducible, or this gate proves nothing: {raw}"
            );
        }
    }
    use vike_studio_core::STUDY_RUN_KIND;

    /// Write a run directory by hand, exactly as `crates/vike-backtest/src/runs.rs`'s `write_run`
    /// lays one out, so [`run_rows`] is exercised over the real reader.
    fn write_run(runs_root: &std::path::Path, manifest: &RunManifest) {
        let dir = runs_root.join(&manifest.run_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(REPORT_FILE), "{}\n").unwrap();
        std::fs::write(dir.join(MANIFEST_FILE), serde_json::to_string_pretty(manifest).unwrap())
            .unwrap();
    }

    fn manifest(run_id: &str, kind: &str, detail: serde_json::Value) -> RunManifest {
        RunManifest {
            run_id: run_id.to_string(),
            kind: kind.to_string(),
            produced_by: "vike-app".to_string(),
            started_at: "2026-08-24T09:15:04Z".to_string(),
            finished_at: "2026-08-24T09:15:16Z".to_string(),
            git_sha: None,
            config: RunConfig { path: None, name: None },
            detail,
        }
    }

    /// **The R6 gate this pane owes.** A study run's numbers are in its manifest — and none of them
    /// reaches the row the SHARED list renders.
    ///
    /// Asserted over the whole row rather than over a named field, so a future column that
    /// reaches into `detail` fails here rather than shipping: the defect
    /// `crates/vike-studio-core/src/study_run.rs` nests its metrics to prevent is a number appearing
    /// in a column beside a backtest's, and the only structural defence a renderer has is having
    /// no such column at all.
    #[test]
    fn a_study_runs_metrics_never_reach_the_shared_runs_list() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        write_run(
            &runs_root,
            &manifest(
                "1756000000-1-0",
                STUDY_RUN_KIND,
                serde_json::json!({
                    "study": "vol",
                    // A value chosen to be UNMISTAKABLE in a rendered row and to be no
                    // mathematical constant: clippy's `approx_constant` rejects the obvious
                    // memorable ones, and a plausible Sharpe would be too easy to match by
                    // accident.
                    "metrics": [{ "name": "sharpe", "value": 1234.5, "nonfinite": null }],
                    "metrics_note": vike_studio_core::STUDY_METRICS_NOTE,
                }),
            ),
        );
        let rows = run_rows(&list_runs(&runs_root));
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        let rendered = format!("{row:?}");
        for leak in ["1234", "sharpe", "metrics"] {
            assert!(!rendered.contains(leak), "{leak:?} reached the shared runs list: {rendered}");
        }
        assert_eq!(row.kind, STUDY_RUN_KIND, "...and the kind IS on the row, which is the point");
    }

    /// A study run and a backtest run sit in ONE list and are told apart by `kind` — the property
    /// `crates/vike-studio-core/src/study_run.rs`'s own listing test asserts on disk, asserted here
    /// on what the surface actually renders.
    #[test]
    fn one_list_carries_both_kinds_and_names_each() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        write_run(
            &runs_root,
            &manifest("1755000000-1-0", BACKTEST_RUN_KIND, serde_json::json!({})),
        );
        write_run(
            &runs_root,
            &manifest("1756000000-1-0", STUDY_RUN_KIND, serde_json::json!({ "study": "vol" })),
        );
        let kinds: Vec<String> =
            run_rows(&list_runs(&runs_root)).into_iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [BACKTEST_RUN_KIND, STUDY_RUN_KIND], "one list, two kinds, in id order");
    }

    /// The two known kinds are coloured apart, and an unrecognised one is neither of them.
    #[test]
    fn the_two_known_kinds_are_coloured_apart() {
        let study = kind_color(STUDY_RUN_KIND);
        let backtest = kind_color(BACKTEST_RUN_KIND);
        let unknown = kind_color("sweep-of-the-future");
        assert_ne!(study, backtest, "a reader must not have to read the text to tell them apart");
        assert_ne!(study, unknown);
        assert_ne!(backtest, unknown);
    }

    /// A run that is missing its report still lists, and says so — the row carries the difference
    /// `crates/vike-studio-core/src/listing.rs` calls "the result survived" vs "nothing was kept".
    #[test]
    fn a_run_without_a_report_still_lists_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let m = manifest("1756000000-1-0", STUDY_RUN_KIND, serde_json::json!({}));
        write_run(&runs_root, &m);
        std::fs::remove_file(runs_root.join(&m.run_id).join(REPORT_FILE)).unwrap();
        let rows = run_rows(&list_runs(&runs_root));
        assert!(!rows[0].has_report);
    }

    /// Both tiers get a badge and the two badges differ — the ONE thing this pane renders about a
    /// tier, because everything else about the difference is the runner's business.
    #[test]
    fn every_tier_has_its_own_badge_and_its_own_tooltip() {
        assert_ne!(tier_label(StudyTier::Rhai), tier_label(StudyTier::Rust));
        assert_ne!(tier_tip(StudyTier::Rhai), tier_tip(StudyTier::Rust));
    }

    /// With no host there is nothing to plan: the pane cannot invent a runs directory to mint into.
    #[test]
    fn a_pane_with_no_host_plans_nothing() {
        let pane = ResearchPane::default();
        let dir = tempfile::tempdir().unwrap();
        let store: StoreHandle =
            std::sync::Arc::new(vike_data::DataFusionHist::open(dir.path()).unwrap());
        assert!(pane.plan(store, TsRange::all()).is_none());
    }
}
