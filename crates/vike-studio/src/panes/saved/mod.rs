//! Saved strategies: persist named strategies to disk (a `Vec<StudioSavedStrategy>` as pretty JSON,
//! `studio_strategies.json` in the Studio's state directory: the desktop passes `settings/state`) and compare
//! several saved strategies' backtests over the currently-picked data slice.
//!
//! A saved entry is a **Rhai** script (the original, and still the default), a strategy **file** of
//! the backtest server's folder plus its free-form param rows, or a **Rust** buffer's build — see
//! [`StudioStrategySource`] and the file-format note on it.
//!
//! ⚠ **This blob is not a strategy library.** Strategies are files of the server's `strategies/`
//! folder. Nothing converts this list into that folder (decision 0117), and [`load_strategies`]
//! refuses a file it cannot parse rather than letting a Save write over it.
//!
//! Load/save (`load_strategies`/`save_strategies`) and the ranking builder
//! (`saved_compare_rows`) are pure/testable; `StudioSavedPane::ui` (SP2 discipline, mirrors
//! `indicators.rs`/`data_browser.rs`) is thin — it only calls into the tested helpers below plus
//! `StudioState`'s store/picker, which is why running the comparison itself lives in
//! `studio/` rather than here.
//!
//! # The compare table SHOWS the catalog and runs nothing
//!
//! "Compare all" is one backtesthub `RunSlice` per saved row (`StudioState`'s `CompareAll` arm),
//! the very request a Run of that row sends — it used to run the simulator in this process at ZERO
//! fee, so the table ranked rows the Performance tab then priced differently. Each row is that
//! run's SUMMARY (decision 0078's amendment of 2026-11-18; until then its whole answer), and the rows
//! rank by the summary's Sharpe, computed on the server over the WHOLE curve, through
//! `vike_analytics::metrics::cmp_scores_desc`, the one best-first comparator, rather than a sort of this
//! file's own: the ranking the whole answers gave (`tests.rs`'s
//! `the_summary_ranks_the_saved_runs_as_their_whole_answers_did`). Each row's cells are the ONE
//! comparison, `vike_analytics::results_comparison::TradingResultsComparison` of the top-ranked row and
//! that row (`vike-cli report --compare` renders it too), over the results table each summary carries
//! (`RunSummaryWire::results`): its value, the Performance tab's text, and under it its difference from the
//! top row, each in its metric's unit. Each side is named by its summary's `source_label` (source and
//! period): the top row in the line over the table, every row on its name's hover. A backtesthub that
//! sends no table (an older build) gets its values printed alone, and the line over the table says so.
//!
//! # The compare table's Sharpe, and why this file needs NO migration
//!
//! [`saved_compare_rows`] used to annualize with a bare `252.0` — the same defect
//! `crate::panes::results` carried, and the same fix: the factor is the PICKED SLICE's, so the
//! Performance tab and the compare table cannot print two different Sharpes for one run. Since the rows
//! are summaries the server annualizes each one at its slice's interval
//! (`vike_analytics::results::periods_per_year_for`), so the table takes no factor of its own.
//!
//! ⚠ The obvious worry — "saved rows on disk were scored at 252, so a saved row and a re-run will
//! disagree with nothing on screen saying why" — **does not apply here, and the reason is worth
//! writing down so nobody re-opens it.** Look at what is actually persisted: [`StudioSavedStrategy`]
//! carries `name`, `code`, `source`, `file`, `params` — a strategy DEFINITION and not one
//! metric. No Sharpe, no equity curve, no run at all. [`StudioSavedCompareRow`] (which does carry a Sharpe)
//! is built fresh from a live backtest every time "Compare all" is pressed, lives in
//! [`StudioSavedPane::compare_rows`] for as long as the pane is open, and is written to no file —
//! `studio_strategies.json` holds strategies and `studio_workspace.json` holds tab/editor state,
//! neither holds a number this change moves.
//!
//! So there is nothing on the old scale to migrate, nothing to stamp a factor onto, and no
//! optional field to add — adding one would create the versioning problem it was meant to solve
//! (a required field breaks every existing file; an optional one means "scored at an unknown
//! scale", which is worse than a value that is simply recomputed). What a user WILL see is their
//! compare table reporting bigger Sharpes than it did in the previous build for the same intraday
//! strategy. That is the fix, not a regression, and it is the same number the CLI reports.
use std::path::Path;

use serde::{Deserialize, Serialize};
use vike_analytics::metrics::{cmp_scores_desc, max_drawdown};
use vike_analytics::results::TradingResults;
use vike_analytics::results_comparison::TradingResultsComparison;
use vike_analytics::{INVALID_LABEL, NOT_RECORDED};
use vike_backtesthub_client::summary::RunSummaryWire;
use vike_studio_core::{StrategySpec, params_from_rows};
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::input::{self, ThemeInputField};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, chip};
use vike_ui_theme::icons;
use vike_ui_theme::maps;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::value::studio;

/// Where a saved strategy's behavior comes from: a Rhai script, a strategy FILE of the backtest
/// server's `strategies/` folder (by its path), or the editor buffer as RUST, built by the builder
/// service.
///
/// The variant names, in snake case, are the words `studio_strategies.json` and
/// `studio_workspace.json` store (`rhai`, `file`, `rust`). A file holding an older word — the
/// pre-plan-2b Rust tag `RETIRED_RUST_TAG`, or `native` for a file row — is REFUSED by
/// `load_strategies` with a one-line hint, never converted (the owner: no migration, and no
/// serde rename that keeps an old word). `Rhai` is the `Default` of a fresh Studio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StudioStrategySource {
    #[default]
    Rhai,
    File,
    Rust,
}

impl StudioStrategySource {
    /// The short badge the Saved list / compare table shows.
    pub fn label(self) -> &'static str {
        match self {
            StudioStrategySource::Rhai => "rhai",
            StudioStrategySource::File => "file",
            StudioStrategySource::Rust => "rust",
        }
    }
}

/// One persisted strategy. `save_strategies` writes every field, so every field is required and
/// an unknown key refuses the file: a row an older build wrote (it says `native`) is never read
/// with an empty field (`load_strategies`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StudioSavedStrategy {
    pub name: String,
    /// Rhai source. Unused (and conventionally empty) for a `File` entry.
    pub(crate) code: String,
    /// Which of the bodies below is authoritative.
    pub source: StudioStrategySource,
    /// `File` only: the strategy file's path under the backtest server's `strategies/` folder.
    pub(crate) file: String,
    /// The free-form `(key, value-text)` param rows of a `File`, `Rust` or `Rhai` row (a Rhai script's `param()`
    /// values and the run's costs; empty in a Rhai row saved before 2026-11-20),
    /// typed into a `vike_model::params::Params` table by `params_from_rows` at run time. Stored as
    /// TEXT rows rather than a typed table on purpose: the registry has no param spec to validate
    /// against
    /// (see `vike_studio_core::spec`'s module doc), so round-tripping exactly what the user typed
    /// is the only lossless option.
    pub(crate) params: Vec<(String, String)>,
    /// `Rust` only: the path the buffer was built as (`trend/momentum.rs`; distinct from `name`
    /// above, which is this SAVED ROW's own label).
    pub(crate) rust_file: String,
    /// `Rust` only: the sha256 of the last successful Build, if one had returned by the time this
    /// row was saved. `None` means "saved before a Build finished" — reloading such a row starts
    /// back at `StudioState::run_blocked_reason`'s refusal rather than pretending a stale build
    /// still exists on disk.
    pub(crate) rust_sha: Option<String>,
}

impl StudioSavedStrategy {
    /// A Rhai entry (the pre-native constructor).
    pub fn rhai(name: impl Into<String>, code: impl Into<String>) -> Self {
        StudioSavedStrategy {
            name: name.into(),
            code: code.into(),
            source: StudioStrategySource::Rhai,
            file: String::new(),
            params: Vec::new(),
            rust_file: String::new(),
            rust_sha: None,
        }
    }

    /// A strategy-file entry: the file's path under the server's folder + its param rows.
    pub fn file(
        name: impl Into<String>,
        file: impl Into<String>,
        params: Vec<(String, String)>,
    ) -> Self {
        StudioSavedStrategy {
            name: name.into(),
            code: String::new(),
            source: StudioStrategySource::File,
            file: file.into(),
            params,
            rust_file: String::new(),
            rust_sha: None,
        }
    }

    /// A Rust-buffer entry: the path the buffer was built as plus the sha the last Build returned
    /// (`None` if saved before a Build finished).
    pub(crate) fn rust(
        name: impl Into<String>,
        rust_file: impl Into<String>,
        rust_sha: Option<String>,
    ) -> Self {
        StudioSavedStrategy {
            name: name.into(),
            code: String::new(),
            source: StudioStrategySource::Rust,
            file: String::new(),
            params: Vec::new(),
            rust_file: rust_file.into(),
            rust_sha,
        }
    }

    /// The runnable [`StrategySpec`] this entry denotes — the ONE place a persisted row becomes
    /// something the backtest server's `vike_backtesthub::studio_run::run_slice` can execute.
    ///
    /// ⚠ A `Rust` row runs its FILE ([`StrategySpec::File`]), built or not: the server reads the file
    /// and refuses one with no build in words, and the Studio's own refusal
    /// (`StudioState::run_blocked_reason`) belongs at the UI boundary, not in this conversion.
    pub fn spec(&self) -> StrategySpec {
        match self.source {
            // With values, the script runs at them (`param()` reads each); with none, the bare buffer.
            StudioStrategySource::Rhai => match params_from_rows(&self.params) {
                parameters if parameters.is_empty() => StrategySpec::rhai(self.code.clone()),
                parameters => {
                    StrategySpec::RhaiWithParameters { source: self.code.clone(), parameters }
                }
            },
            StudioStrategySource::File => {
                StrategySpec::file(self.file.clone(), params_from_rows(&self.params))
            }
            // The FILE itself: a hub runs a `.rs` file in a sandboxed child and loads no artifact by
            // its sha (plan 2b), so the sha is a record of the Build, not what runs.
            StudioStrategySource::Rust => {
                StrategySpec::file(self.rust_file.clone(), params_from_rows(&self.params))
            }
        }
    }
}

/// The Rust kind's saved tag before plan 2b, the one a file is refused for. Built from parts, so a
/// scripted rename of the retired word cannot rewrite the one place that must still recognise it.
pub(crate) const RETIRED_RUST_TAG: &str = concat!("plu", "gin");

/// The strategy-file kind's saved word (and its path key) before 2026-11-18, built from parts for
/// the same reason as [`RETIRED_RUST_TAG`].
pub(crate) const OLD_FILE_WORD: &str = concat!("nat", "ive");

/// The words only a file an older build wrote holds, as a value (`unknown variant`) or as a key
/// (`unknown field`) — what [`load_strategies`] and the Studio workspace load name in their hint.
pub(crate) const OLD_SAVED_WORDS: [&str; 2] = [RETIRED_RUST_TAG, OLD_FILE_WORD];

/// The older word serde's error `e` names, when it names one of `old` as an unknown variant or
/// field.
pub(crate) fn old_word_in<'a>(e: &serde_json::Error, old: &[&'a str]) -> Option<&'a str> {
    let text = e.to_string();
    old.iter().copied().find(|word| {
        ["unknown variant", "unknown field"]
            .iter()
            .any(|what| text.contains(&format!("{what} `{word}`")))
    })
}

/// Load the saved-strategy list from `path`. A missing file is an empty list.
///
/// ⚠ **A file that does not parse is REFUSED with a one-line hint, not read as empty.** An empty
/// list here used to be the answer, and the next Save then wrote the new list OVER the file,
/// losing every row it held. A file an older build wrote (a row saying [`RETIRED_RUST_TAG`] or
/// `native`) is refused the same way and never converted (the owner: no migration, no serde
/// rename that keeps an old word): the user moves it away, and until then the file is not written.
pub(crate) fn load_strategies(path: &Path) -> Result<Vec<StudioSavedStrategy>, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(Vec::new()) };
    serde_json::from_str(&text).map_err(|e| {
        if let Some(word) = old_word_in(&e, &OLD_SAVED_WORDS) {
            format!(
                "{} was written by an older Vike Trader (it says `{word}`) and is not read: move \
                 it away and save your strategies again in the Saved tab. Nothing is saved over it \
                 until then.",
                path.display()
            )
        } else {
            format!(
                "{} could not be read ({e}): nothing is saved over it until it is fixed or moved \
                 away.",
                path.display()
            )
        }
    })
}

/// Write the saved-strategy list to `path` as pretty JSON, replacing any existing file whole through
/// `vike_utility::atomic_file`'s `write_atomic`: a crash mid-save leaves the previous list, never a
/// truncated file [`load_strategies`] would then refuse.
pub(crate) fn save_strategies(path: &Path, list: &[StudioSavedStrategy]) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".to_string());
    vike_utility::atomic_file::write_atomic(path, text.as_bytes())
}

/// The catalog ids the compare table shows as columns, in this order (after Name and Kind).
pub(crate) const COMPARE_IDS: [&str; 4] = ["sharpe", "final_equity", "max_drawdown", "n_trades"];

/// One row of the ranked comparison table. A strategy that failed to compile/run over the
/// selected slice is `outcome: Err(message)`, rather than being silently dropped from the table.
#[derive(Debug, Clone)]
pub struct StudioSavedCompareRow {
    pub(crate) name: String,
    /// Which kind of strategy produced this row (`rhai`/`file`/`rust`) — the compare pane's Kind
    /// column, so a mixed table says WHAT it ranked, not just how well.
    pub(crate) source: StudioStrategySource,
    /// The run's SUMMARY as the backtesthub answered it (`vike_backtesthub_client::summary::summarize`, at the
    /// scale of the slice this comparison ran over, NOT a fixed daily 252: see the module doc), or the sentence
    /// it failed with. Recomputed on every "Compare all" and persisted nowhere.
    pub(crate) outcome: Result<RunSummaryWire, String>,
    /// [`compare_cells`] of this row against the top-ranked one (or [`summary_cells`] where a summary carries
    /// no results table): what the row prints. Empty for a failed row.
    pub(crate) cells: [[String; 2]; 4],
    /// The equity curve thinned as the summary carries it, for the per-row sparkline: its maximum drawdown is
    /// the whole curve's (`vike_backtesthub_client::summary::downsample`). Empty for a failed row.
    pub(crate) equity: Vec<f64>,
}

/// Each of [`COMPARE_IDS`] of `report` held against `reference` (the top-ranked row), in that order, as
/// `[value, difference]`: the `TradingResultsComparison::rendered_rows` text of the ONE comparison
/// `vike-cli report --compare` prints, so a value is what the Performance tab prints and the difference
/// is `report − reference` in the row's unit (empty where either side has no value). A run whose equity
/// reached zero (`TradingResults::invalid`) has no Sharpe or drawdown: those cells say
/// `INVALID: equity reached 0 on <day>…`, never `not recorded`.
fn compare_cells(reference: &TradingResults, report: &TradingResults) -> [[String; 2]; 4] {
    let rows = TradingResultsComparison::of(reference.clone(), report.clone()).rendered_rows();
    COMPARE_IDS.map(|id| {
        let [_, value, difference] =
            rows.iter().find(|(k, _)| *k == id).map(|(_, cells)| cells.clone()).unwrap_or_default();
        // The comparison names an INVALID side by its label alone; the row keeps its words.
        let value = match &report.invalid {
            Some(why) if value.is_empty() || value == INVALID_LABEL => {
                format!("{INVALID_LABEL}: {why}")
            }
            _ if value.is_empty() => NOT_RECORDED.to_string(),
            _ => value,
        };
        [value, difference]
    })
}

/// The cells of a summary that carries no results table (a backtesthub older than
/// `RunSummaryWire::results`): each of [`COMPARE_IDS`]' rendered text, the Performance tab's, and NO
/// difference, since there is no table to hold against another. An INVALID run's Sharpe and drawdown say
/// `INVALID: equity reached 0 on <day>…`, as [`compare_cells`]' do.
fn summary_cells(summary: &RunSummaryWire) -> [[String; 2]; 4] {
    let missing = || match &summary.invalid {
        Some(why) => format!("{INVALID_LABEL}: {why}"),
        None => NOT_RECORDED.to_string(),
    };
    COMPARE_IDS.map(|id| {
        let value =
            summary.rows.iter().find(|(k, _)| k == id).map_or_else(missing, |(_, v)| v.clone());
        [value, String::new()]
    })
}

/// The line over the compare table when a summary carries no results table, said once.
pub(crate) const NO_RESULTS_TABLE: &str = "This backtesthub sends no results table with its summaries \
     (an older build), so each value shows without its difference from the top row.";

/// The line over the compare table naming what every difference under a value is taken from: the
/// top-ranked row, by its summary's `source_label` (the `a` side of each row's comparison), or
/// [`NO_RESULTS_TABLE`] when a ran row's summary carries no table. `None` while fewer than two rows ran,
/// so no row shows a difference.
pub(crate) fn difference_header(rows: &[StudioSavedCompareRow]) -> Option<String> {
    let top = rows.first()?.outcome.as_ref().ok()?;
    let ran: Vec<&RunSummaryWire> = rows.iter().filter_map(|r| r.outcome.as_ref().ok()).collect();
    if ran.len() < 2 {
        return None;
    }
    if ran.iter().any(|summary| summary.results.is_none()) {
        return Some(NO_RESULTS_TABLE.to_string());
    }
    let label = top.source_label.as_deref()?;
    Some(format!("Under each value: its difference from {label}"))
}

/// Build the ranked comparison table from `(name, outcome)` pairs, where `outcome` is the hub
/// run's summary (already a sentence on failure, so this module doesn't need to know `RunError`).
/// Successful rows rank by the summary's annualized Sharpe through `cmp_scores_desc` — best first, a
/// Sharpe that is not a number last among successes; every failed row sorts after every successful row
/// (it has no Sharpe) — surfaced, not silently dropped. The sort is stable, so rows that tie keep the saved
/// list's order.
///
/// The Sharpe is the server's, over the WHOLE curve at the slice's annualization, the very number the
/// whole answer's report carried (a valid run's finite curve has a finite Sharpe, so the summary's `None`
/// is the report's NaN), so this ranks as the whole answers ranked until decision 0078's amendment of
/// 2026-11-18 (`tests.rs`'s `the_summary_ranks_the_saved_runs_as_their_whole_answers_did`).
///
/// `source_of` maps a row's NAME back to its [`StudioStrategySource`] for the Kind column. It is a
/// lookup rather than a third tuple element because the worker's
/// `crate::backend::backtesthub::StudioSavedCompareOutcome` carries names only — the caller already holds the saved
/// list the names came from. Rhai-only callers pass `|_| StudioStrategySource::Rhai`.
pub fn saved_compare_rows(
    results: &[(String, Result<RunSummaryWire, String>)],
    source_of: impl Fn(&str) -> StudioStrategySource,
) -> Vec<StudioSavedCompareRow> {
    let mut rows: Vec<StudioSavedCompareRow> = results
        .iter()
        .map(|(name, outcome)| StudioSavedCompareRow {
            name: name.clone(),
            source: source_of(name),
            outcome: outcome.clone(),
            cells: Default::default(),
            equity: outcome
                .as_ref()
                .map(|summary| summary.curve.iter().map(|&(_, v)| v).collect())
                .unwrap_or_default(),
        })
        .collect();
    let sharpe = |summary: &RunSummaryWire| summary.sharpe.unwrap_or(f64::NAN);
    // An INVALID run (its equity reached zero) ranks after every valid one, never by its Sharpe.
    rows.sort_by(|a, b| match (&a.outcome, &b.outcome) {
        (Ok(a), Ok(b)) => match (a.invalid.is_some(), b.invalid.is_some()) {
            (false, false) => cmp_scores_desc(sharpe(a), sharpe(b)),
            (a_invalid, b_invalid) => a_invalid.cmp(&b_invalid),
        },
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => std::cmp::Ordering::Equal,
    });
    // Every ran row is held against the top-ranked one (a failed top means no row ran), through the
    // results tables the summaries carry; a summary without one prints its values alone.
    let reference =
        rows.first().and_then(|r| r.outcome.as_ref().ok()).and_then(|s| s.results.clone());
    for row in &mut rows {
        if let Ok(summary) = &row.outcome {
            row.cells = match (&reference, &summary.results) {
                (Some(reference), Some(report)) => compare_cells(reference, report),
                _ => summary_cells(summary),
            };
        }
    }
    rows
}

/// Save's refusal while the name box is empty.
const NAME_IT_FIRST: &str = "Name the strategy first.";
/// Save's refusal while the saved file was not read (the reason is printed under the Save row).
pub(crate) const FILE_NOT_READ: &str =
    "The saved-strategies file was not read: nothing is saved over it.";
/// Compare all's refusal while nothing is saved.
const NOTHING_TO_COMPARE: &str = "Save a strategy first — Compare all ranks the saved ones.";

/// The Saved-strategies pane: the loaded list, the "save current" name box, and the last
/// comparison run's ranked rows (or an error). Owns no store/picker access directly — matches
/// `StudioIndicatorsPane`/`StudioDataBrowserPane`: `StudioState` loads/saves/compares and threads the result
/// in, so this stays pure/testable at the struct level and thin in `ui()`.
#[derive(Default)]
pub struct StudioSavedPane {
    pub strategies: Vec<StudioSavedStrategy>,
    pub save_name: String,
    pub compare_rows: Option<Vec<StudioSavedCompareRow>>,
    pub compare_error: Option<String>,
    /// Why the file was not read (`load_strategies`'s hint), `None` when it was. While `Some`,
    /// nothing is written over the file (`StudioState::persist_saved`) and Save is disabled.
    pub refused: Option<String>,
    /// Why the last write of the list failed (`StudioState::persist_saved`), `None` after one succeeded:
    /// shown here and under the editor's header, since a Save that did not reach the disk is lost on
    /// restart.
    pub write_error: Option<String>,
}

/// What the pane's `ui()` asks the caller (`StudioState`) to do this frame — the pane never
/// touches the store/editor/picker itself, mirroring how `StudioIndicatorsPane::ui` returns a bool and
/// `StudioDataBrowserPane::ui` returns an `Option<(venue, symbol, interval)>`.
pub enum StudioSavedAction {
    /// Load this saved strategy's source into the editor.
    Load(usize),
    /// Delete this saved strategy (persist the updated list).
    Delete(usize),
    /// Snapshot the editor's current source under `save_name` (persist the updated list).
    SaveCurrent,
    /// Run every saved strategy over the selected slice and rank the results.
    CompareAll,
}

impl StudioSavedPane {
    /// Load the list from disk (call once at construction — mirrors `StudioIndicatorsPane`/
    /// `StudioDataBrowserPane`'s one-shot `refresh`, not a per-frame read).
    pub(crate) fn load(path: &Path) -> Self {
        match load_strategies(path) {
            Ok(strategies) => Self { strategies, ..Self::default() },
            Err(hint) => Self { refused: Some(hint), ..Self::default() },
        }
    }

    /// `rhai_writer_blocked` is `crate::studio::StudioState::rhai_writer_blocked_reason` — `Some`
    /// while the editor holds a Rust buffer with no other copy (Strategy file mode). Loading a RHAI row
    /// would overwrite that buffer with the saved script exactly the way the template/copilot
    /// writers would, so a Rhai row's `Load` is disabled under the same reason; a Native or Strategy file
    /// row's `Load` never touches the editor and stays enabled regardless.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        rhai_writer_blocked: Option<&'static str>,
    ) -> Option<StudioSavedAction> {
        let mut action = None;
        let tk = ThemeTokens::of(ui.ctx());
        crate::studio::pane_header(ui, icons::SAVED, "Saved strategies");
        // Save row: the name field takes whatever width the Save button leaves — a fixed-width
        // field here is what used to overflow the 340px tools panel and shove the icon rail
        // off-screen (the pre-redesign clipping bug).
        ui.horizontal(|ui| {
            let button_w = studio::SAVE_BUTTON_WIDTH;
            let field_w = (ui.available_width() - button_w - space::XL).max(60.0);
            ui.scope(|ui| {
                ui.spacing_mut().text_edit_width = field_w;
                input::text(
                    ui,
                    &mut self.save_name,
                    ThemeInputField { hint: "strategy name", ..ThemeInputField::default() },
                );
            });
            let save_why = if self.refused.is_some() {
                Some(FILE_NOT_READ)
            } else if self.save_name.trim().is_empty() {
                Some(NAME_IT_FIRST)
            } else {
                None
            };
            if ui
                .add_enabled(save_why.is_none(), egui::Button::new((icons::SAVE, "Save")))
                .on_hover_text("Save the editor buffer under this name  (Ctrl+S)")
                .on_disabled_hover_text(save_why.unwrap_or_default())
                .clicked()
            {
                action = Some(StudioSavedAction::SaveCurrent);
            }
        });
        for hint in self.refused.iter().chain(&self.write_error) {
            ui.add(
                egui::Label::new(egui::RichText::new(hint).color(ThemeStatus::Error.color()))
                    .wrap(),
            );
        }
        ui.add_space(space::SM);
        egui::ScrollArea::vertical()
            .id_salt("saved-list")
            .max_height(studio::SAVED_LIST_MAX_HEIGHT)
            .show(ui, |ui| {
                for (i, s) in self.strategies.iter().enumerate() {
                    ui.horizontal(|ui| {
                        // Load/Delete hug the right edge; the name truncates into what's left, so a
                        // long strategy name can never push the buttons out of the panel.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icons::named(ui.small_button(icons::DELETE), "Delete").clicked() {
                                action = Some(StudioSavedAction::Delete(i));
                            }
                            let load_blocked = (s.source == StudioStrategySource::Rhai)
                                .then_some(rhai_writer_blocked)
                                .flatten();
                            // The refusal is the DISABLED hover: egui shows `on_hover_text` only on an
                            // enabled widget, so a reason put there on a disabled Load was never seen.
                            let load_response = ui
                                .add_enabled(
                                    load_blocked.is_none(),
                                    egui::Button::new("Load").small(),
                                )
                                .on_hover_text("Load into the editor")
                                .on_disabled_hover_text(load_blocked.unwrap_or_default());
                            if load_response.clicked() {
                                action = Some(StudioSavedAction::Load(i));
                            }
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    // Kind badge FIRST so a native row is identifiable at a glance —
                                    // its `code` is empty, so the name is the only other signal. A kind
                                    // is a CATEGORY, not a state, so all three wear the one muted badge
                                    // (design system spec §3.2); the words tell them apart.
                                    let tip = match s.source {
                                        StudioStrategySource::Rhai => "My strategy (Rhai)",
                                        StudioStrategySource::File => "Strategy file",
                                        StudioStrategySource::Rust => "My strategy (Rust)",
                                    };
                                    chip::badge(ui, s.source.label(), ThemeStatus::Muted)
                                        .on_hover_text(tip);
                                    ui.add(egui::Label::new(&s.name).truncate());
                                },
                            );
                        });
                    });
                }
                if self.strategies.is_empty() {
                    ui.weak("No saved strategies yet — name the editor buffer above and Save.");
                }
            });
        ui.add_space(space::SM);
        ui.separator();
        ui.add_space(space::SM);
        let mut compare = ThemeButtonAction::primary("Compare all");
        if self.strategies.is_empty() {
            compare = compare.disabled_because(NOTHING_TO_COMPARE);
        }
        if ui
            .add(compare)
            .on_hover_text("Backtest every saved strategy over the selected slice and rank them")
            .clicked()
        {
            action = Some(StudioSavedAction::CompareAll);
        }
        if let Some(err) = &self.compare_error {
            ui.colored_label(ThemeStatus::Error.color(), err);
        } else if let Some(rows) = &self.compare_rows {
            ui.add_space(space::SM);
            if let Some(header) = difference_header(rows) {
                ui.add(egui::Label::new(egui::RichText::new(header).weak().small()).wrap());
            }
            // The 7-column table is wider than the tools panel — give it its own 2-axis scroll
            // instead of letting it clip (the columns stay readable; the panel stays fixed).
            egui::ScrollArea::both()
                .id_salt("saved-compare-scroll")
                .max_height(studio::COMPARE_SCROLL_MAX_HEIGHT)
                .show(ui, |ui| {
                    egui::Grid::new("saved-compare-grid").striped(true).show(ui, |ui| {
                        ui.strong("Name");
                        ui.strong("Kind");
                        for id in COMPARE_IDS {
                            ui.strong(id);
                        }
                        ui.strong("Equity");
                        ui.end_row();
                        for (rank, row) in rows.iter().enumerate() {
                            match &row.outcome {
                                Err(message) => {
                                    ui.add(egui::Label::new(&row.name).truncate());
                                    ui.monospace(row.source.label());
                                    // The failure REASON stays readable inline (the two-axis
                                    // scroll contains any width); hover keeps the full text for
                                    // very long messages.
                                    ui.colored_label(
                                        ThemeStatus::Error.color(),
                                        format!("failed: {message}"),
                                    )
                                    .on_hover_text(message.as_str());
                                    ui.label("—");
                                    ui.label("—");
                                    ui.label("—");
                                    ui.label("");
                                }
                                Ok(summary) => {
                                    // Rank 0 is the best row (rows are sorted by Sharpe desc). The
                                    // trophy is the mark; the accent is a shape, never the colour
                                    // of a name (design system spec §2, §4.3).
                                    let name = if rank == 0 {
                                        ui.label(
                                            icons::BEST
                                                .before(ui.style(), egui::RichText::new(&row.name)),
                                        )
                                    } else {
                                        ui.add(egui::Label::new(&row.name).truncate())
                                    };
                                    // The row's side of its comparison, by its source and period.
                                    if let Some(label) = &summary.source_label {
                                        name.on_hover_text(label);
                                    }
                                    ui.monospace(row.source.label());
                                    let [sharpe, equity, drawdown, trades] = &row.cells;
                                    // Sign convention: positive up, negative down (the market
                                    // set's colours — money, not a status), zero/none neutral — a
                                    // flat no-trade strategy must not read as a loss. Drawdown is
                                    // only a loss colour when there IS one: the thinned curve's,
                                    // which is the whole curve's.
                                    let ratio = summary.sharpe.unwrap_or(0.0);
                                    let positive = ratio > 0.0;
                                    let sharpe_colour = (positive || ratio < 0.0).then(|| {
                                        ThemeSidePair::PositiveNegative.text(positive, &tk)
                                    });
                                    let drawdown_colour = (max_drawdown(&row.equity) > 0.0)
                                        .then(|| maps::side::LOSS.text.resolve(&tk));
                                    for (cell, colour) in [
                                        (sharpe, sharpe_colour),
                                        (equity, None),
                                        (drawdown, drawdown_colour),
                                        (trades, None),
                                    ] {
                                        // The top row is the reference: no difference of its own.
                                        compare_cell(ui, cell, colour, rank > 0);
                                    }
                                    equity_sparkline(ui, &row.equity);
                                }
                            }
                            ui.end_row();
                        }
                    });
                });
        }
        action
    }
}

/// One compare cell: the row's value in `colour` (the label's own when `None`), and under it, muted, its
/// difference from the top-ranked row, when `with_difference` and the comparison has one.
fn compare_cell(
    ui: &mut egui::Ui,
    [value, difference]: &[String; 2],
    colour: Option<egui::Color32>,
    with_difference: bool,
) {
    let mut text = egui::RichText::new(value).monospace();
    if let Some(colour) = colour {
        text = text.color(colour);
    }
    ui.vertical(|ui| {
        ui.label(text);
        if with_difference && !difference.is_empty() {
            ui.label(egui::RichText::new(difference).monospace().weak().small());
        }
    });
}

/// Paint a small polyline of `equity` into a fixed-size rect (`studio::SPARKLINE_SIZE`) — the per-row sparkline in
/// the compare table. A curve with fewer than 2 points (nothing to draw a line between) or an
/// all-flat curve (zero range, would divide by zero) renders nothing rather than panicking.
fn equity_sparkline(ui: &mut egui::Ui, equity: &[f64]) {
    let size = studio::SPARKLINE_SIZE;
    let (rect, _response) = ui.allocate_exact_size(size, egui::Sense::hover());
    if equity.len() < 2 {
        return;
    }
    let min = equity.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = equity.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;
    if range <= 0.0 {
        return;
    }
    let n = equity.len();
    let points: Vec<egui::Pos2> = equity
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = rect.left() + (i as f32 / (n - 1) as f32) * rect.width();
            // y is flipped: higher equity draws nearer the top of the rect.
            let y = rect.bottom() - ((v - min) / range) as f32 * rect.height();
            egui::pos2(x, y)
        })
        .collect();
    // Color by outcome (end vs start), not a flat green — a losing curve drawn in the up colour
    // undercuts the one-palette contract. An equity curve is money, so the market set paints it.
    let color =
        ThemeSidePair::GainLoss.colour(equity[n - 1] >= equity[0], &ThemeTokens::of(ui.ctx()));
    let stroke = egui::Stroke::new(stroke::LINE, color);
    ui.painter().add(egui::Shape::line(points, stroke));
}

#[cfg(test)]
mod tests;
