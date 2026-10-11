//! Saved strategies: persist named strategies to disk (a `Vec<SavedStrategy>` as pretty JSON
//! colocated with the store, at `store.root().join("studio_strategies.json")`) and compare
//! several saved strategies' backtests over the currently-picked data slice.
//!
//! A saved entry is either a **Rhai** script (the original, and still the default) or a **Native**
//! Rust strategy from the `vike-backtest` harness registry plus its free-form param rows — see
//! [`StrategySource`] and the file-format note on it.
//!
//! ⚠ **This blob is not the user's strategy library.** `vike_studio_core::user_strategies` loads
//! user strategies from `<project>/user_data/strategies/`, one folder per strategy; that module's
//! doc argues why — the short version is that [`load_strategies`] answers a parse failure with an
//! EMPTY list, which is the right call for disposable state and the wrong one for the only copy of
//! somebody's work. Nothing converts this list into that tree (decision 0117).
//!
//! Load/save (`load_strategies`/`save_strategies`) and the ranking builder
//! (`comparison_rows`) are pure/testable; `SavedPane::ui` (SP2 discipline, mirrors
//! `indicators.rs`/`data_browser.rs`) is thin — it only calls into the tested helpers below plus
//! `StudioState`'s store/picker, which is why running the comparison itself lives in
//! `studio.rs` rather than here.
//!
//! # The compare table's Sharpe, and why this file needs NO migration
//!
//! [`comparison_rows`] used to annualize with a bare `252.0` — the same defect
//! `crate::panes::results` carried, and the same fix: the factor is a parameter now, derived once by
//! `StudioState::display_periods_per_year` from the PICKED SLICE and handed to both surfaces, so
//! the Performance tab and the compare table cannot print two different Sharpes for one run.
//!
//! ⚠ The obvious worry — "saved rows on disk were scored at 252, so a saved row and a re-run will
//! disagree with nothing on screen saying why" — **does not apply here, and the reason is worth
//! writing down so nobody re-opens it.** Look at what is actually persisted: [`SavedStrategy`]
//! carries `name`, `code`, `source`, `native`, `params` — a strategy DEFINITION and not one
//! metric. No Sharpe, no equity curve, no run at all. [`CompareRow`] (which does carry a Sharpe)
//! is built fresh from a live backtest every time "Compare all" is pressed, lives in
//! [`SavedPane::compare_rows`] for as long as the pane is open, and is written to no file —
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
use vike_analytics::{
    BacktestResult,
    metrics::{max_drawdown, sharpe},
};
use vike_studio_core::{StrategySpec, params_from_rows};
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::input::{self, Field};
use vike_ui_theme::components::{Status, Tokens, chip};
use vike_ui_theme::icons;
use vike_ui_theme::maps;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::Pair;
use vike_ui_theme::value::studio;

/// Where a saved strategy's behavior comes from.
///
/// **Backward compatibility is load-bearing.** `studio_strategies.json` files written before native
/// strategies existed have NO `source` key at all; `#[serde(default)]` on the field plus
/// `#[derive(Default)]` here (defaulting to `Rhai`) makes every one of those entries load as the
/// Rhai script it has always been, with its `code` untouched. Never reorder this so that `Native`
/// becomes the default — that would silently reinterpret every legacy entry's `code` as a registry
/// name. Pinned by `legacy_file_without_a_source_field_loads_as_rhai`.
///
/// ⚠ **A THIRD variant, `Plugin`, joined 2026-09-21** — a runtime-loaded Rust strategy, named by an
/// already-built artifact's sha256 rather than a registry lookup. It is appended AFTER `Native` for
/// the same back-compat reason `Native` itself was: `#[serde(rename_all = "snake_case")]` encodes a
/// unit variant by NAME, not by ordinal, so appending it changes no existing encoding — a legacy
/// file with no `source` key still decodes to `Rhai` (the `#[default]`) and an existing `"native"`
/// row is unaffected. Never reorder these so the default changes meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategySource {
    #[default]
    Rhai,
    Native,
    Plugin,
}

impl StrategySource {
    /// The short badge the Saved list / compare table shows.
    pub fn label(self) -> &'static str {
        match self {
            StrategySource::Rhai => "rhai",
            StrategySource::Native => "native",
            StrategySource::Plugin => "plugin",
        }
    }
}

/// One persisted strategy.
///
/// Every field added after the original `{name, code}` shape carries `#[serde(default)]` so a
/// legacy file keeps loading — the migration is purely additive, there is no version stamp and no
/// rewrite pass (see [`StrategySource`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedStrategy {
    pub name: String,
    /// Rhai source. Unused (and conventionally empty) for a `Native` entry.
    pub code: String,
    /// Which of the two bodies below is authoritative. Absent in a legacy file -> `Rhai`.
    #[serde(default)]
    pub source: StrategySource,
    /// `Native` only: the `vike_backtest::harness::registry` strategy name.
    #[serde(default)]
    pub native: String,
    /// `Native` only: the free-form `(key, value-text)` param rows the Studio's param editor holds,
    /// serialized to a `toml::Value` table by `params_from_rows` at run time. Stored as TEXT rows
    /// rather than a typed table on purpose: the registry has no param spec to validate against
    /// (see `vike_studio_core::spec`'s module doc), so round-tripping exactly what the user typed
    /// is the only lossless option.
    #[serde(default)]
    pub params: Vec<(String, String)>,
    /// `Plugin` only: the plugin's declared name (distinct from `name` above, which is this SAVED
    /// ROW's own label — the two coincide in the common case but need not).
    #[serde(default)]
    pub plugin_name: String,
    /// `Plugin` only: the sha256 of the last successful Build, if one had returned by the time this
    /// row was saved. `None` means "saved before a Build finished" — reloading such a row starts
    /// back at `StudioState::run_blocked_reason`'s refusal rather than pretending a stale build
    /// still exists on disk.
    #[serde(default)]
    pub plugin_sha: Option<String>,
}

impl SavedStrategy {
    /// A Rhai entry (the pre-native constructor).
    pub fn rhai(name: impl Into<String>, code: impl Into<String>) -> Self {
        SavedStrategy {
            name: name.into(),
            code: code.into(),
            source: StrategySource::Rhai,
            native: String::new(),
            params: Vec::new(),
            plugin_name: String::new(),
            plugin_sha: None,
        }
    }

    /// A native entry: a registry name + its param rows.
    pub fn native(
        name: impl Into<String>,
        native: impl Into<String>,
        params: Vec<(String, String)>,
    ) -> Self {
        SavedStrategy {
            name: name.into(),
            code: String::new(),
            source: StrategySource::Native,
            native: native.into(),
            params,
            plugin_name: String::new(),
            plugin_sha: None,
        }
    }

    /// A plugin entry: a declared plugin name plus the sha the last Build returned (`None` if saved
    /// before a Build finished).
    pub fn plugin(
        name: impl Into<String>,
        plugin_name: impl Into<String>,
        plugin_sha: Option<String>,
    ) -> Self {
        SavedStrategy {
            name: name.into(),
            code: String::new(),
            source: StrategySource::Plugin,
            native: String::new(),
            params: Vec::new(),
            plugin_name: plugin_name.into(),
            plugin_sha,
        }
    }

    /// The runnable [`StrategySpec`] this entry denotes — the ONE place a persisted row becomes
    /// something `vike_studio_core::run_slice` can execute.
    ///
    /// ⚠ A `Plugin` row with no `plugin_sha` still produces a [`StrategySpec::Plugin`] — an EMPTY
    /// sha, never a panic or a fallback to another source — because the refusal that matters
    /// (`StudioState::run_blocked_reason`) belongs at the UI boundary, before a run is even
    /// dispatched, not buried inside the conversion that every backend (local build, remote wire)
    /// shares.
    pub fn spec(&self) -> StrategySpec {
        match self.source {
            StrategySource::Rhai => StrategySpec::rhai(self.code.clone()),
            StrategySource::Native => {
                StrategySpec::native(self.native.clone(), params_from_rows(&self.params))
            }
            StrategySource::Plugin => StrategySpec::plugin(
                self.plugin_name.clone(),
                self.plugin_sha.clone().unwrap_or_default(),
                params_from_rows(&self.params),
            ),
        }
    }
}

/// Load the saved-strategy list from `path`. A missing file, or one that fails to parse, yields
/// an empty list rather than panicking — the file is best-effort state, not load-bearing data.
pub fn load_strategies(path: &Path) -> Vec<SavedStrategy> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Write the saved-strategy list to `path` as pretty JSON, overwriting any existing file.
pub fn save_strategies(path: &Path, list: &[SavedStrategy]) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".to_string());
    std::fs::write(path, text)
}

/// One row of the ranked comparison table. A strategy that failed to compile/run over the
/// selected slice is represented as `error: Some(message)` with the metric fields left at their
/// defaults, rather than being silently dropped from the table (the previous behavior).
#[derive(Debug, Clone, PartialEq)]
pub struct CompareRow {
    pub name: String,
    /// Which kind of strategy produced this row (`rhai`/`native`) — the compare pane's Kind
    /// column, so a mixed table says WHAT it ranked, not just how well.
    pub source: StrategySource,
    /// ANNUALIZED Sharpe, on the `periods_per_year` [`comparison_rows`] was handed — i.e. the
    /// scale of the slice this comparison was run over, NOT a fixed daily 252 (see the module
    /// doc). Recomputed on every "Compare all" and persisted nowhere, so it cannot go stale
    /// against the Performance tab.
    pub sharpe: f64,
    pub final_equity: f64,
    pub n_trades: usize,
    /// `metrics::max_drawdown` — the max fractional peak-to-trough decline (0.0..=1.0+), NOT a
    /// negative number; render it as e.g. `-{max_dd*100:.1}%`. `0.0` when `error.is_some()`.
    pub max_dd: f64,
    /// The equity curve (for the per-row sparkline). Empty when `error.is_some()`.
    pub equity: Vec<f64>,
    /// `Some(message)` when the strategy failed to compile/run over the selected slice; `None`
    /// for a successful row.
    pub error: Option<String>,
}

/// Build the ranked comparison table from `(name, outcome)` pairs, where `outcome` is the
/// `run_slice` result (already stringified so this module doesn't need to know `RunError`).
/// Successful rows are sorted by annualized Sharpe descending, with NaN Sharpes (e.g. a flat
/// equity curve) sorted last among successes; every failed row sorts after every successful row
/// (regardless of Sharpe, since failed rows have none) — surfaced, not silently dropped.
///
/// `periods_per_year` is the annualization factor, derived by the caller from the slice every row
/// was replayed over (`StudioState::display_periods_per_year`, which uses
/// `vike_analytics::report::periods_per_year_for_interval` and owns the no-slice / tick fallbacks).
/// This doc used to say "252 periods/year, matching the rest of the Studio" — which was true and
/// was the bug: the rest of the Studio matched it by ALSO hard-coding 252, so an intraday
/// comparison was understated by `sqrt(bars-per-day)` and disagreed with the CLI/MCP door for the
/// same strategy over the same series.
///
/// ⚠ The RANKING is unchanged by the factor. `sqrt(periods_per_year)` is a positive constant
/// applied to every row, so the order is identical whatever is passed — pinned by
/// `the_ranking_is_identical_on_any_factor_while_the_values_move`. It is the printed numbers, and
/// their agreement with the Performance tab, that this parameter buys.
///
/// `source_of` maps a row's NAME back to its [`StrategySource`] for the Kind column. It is a
/// lookup rather than a third tuple element because `vike_studio_core::CompareOutcome` (what the
/// worker thread delivers) carries names only — the caller already holds the saved list the names
/// came from. Rhai-only callers pass `|_| StrategySource::Rhai`.
pub fn comparison_rows(
    results: &[(String, Result<BacktestResult, String>)],
    periods_per_year: f64,
    source_of: impl Fn(&str) -> StrategySource,
) -> Vec<CompareRow> {
    let mut rows: Vec<CompareRow> = results
        .iter()
        .map(|(name, outcome)| match outcome {
            Ok(r) => CompareRow {
                name: name.clone(),
                source: source_of(name),
                sharpe: sharpe(&r.equity_curve, periods_per_year),
                final_equity: r.final_equity,
                n_trades: r.n_trades,
                max_dd: max_drawdown(&r.equity_curve),
                equity: r.equity_curve.clone(),
                error: None,
            },
            Err(msg) => CompareRow {
                name: name.clone(),
                source: source_of(name),
                sharpe: f64::NAN,
                final_equity: 0.0,
                n_trades: 0,
                max_dd: 0.0,
                equity: Vec::new(),
                error: Some(msg.clone()),
            },
        })
        .collect();
    rows.sort_by(|a, b| match (a.error.is_some(), b.error.is_some()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => match (a.sharpe.is_nan(), b.sharpe.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => b.sharpe.partial_cmp(&a.sharpe).unwrap_or(std::cmp::Ordering::Equal),
        },
    });
    rows
}

/// Save's refusal while the name box is empty.
const NAME_IT_FIRST: &str = "Name the strategy first.";
/// Compare all's refusal while nothing is saved.
const NOTHING_TO_COMPARE: &str = "Save a strategy first — Compare all ranks the saved ones.";

/// The Saved-strategies pane: the loaded list, the "save current" name box, and the last
/// comparison run's ranked rows (or an error). Owns no store/picker access directly — matches
/// `IndicatorsPane`/`DataBrowserPane`: `StudioState` loads/saves/compares and threads the result
/// in, so this stays pure/testable at the struct level and thin in `ui()`.
#[derive(Default)]
pub struct SavedPane {
    pub strategies: Vec<SavedStrategy>,
    pub save_name: String,
    pub compare_rows: Option<Vec<CompareRow>>,
    pub compare_error: Option<String>,
}

/// What the pane's `ui()` asks the caller (`StudioState`) to do this frame — the pane never
/// touches the store/editor/picker itself, mirroring how `IndicatorsPane::ui` returns a bool and
/// `DataBrowserPane::ui` returns an `Option<(venue, symbol, interval)>`.
pub enum SavedAction {
    /// Load this saved strategy's source into the editor.
    Load(usize),
    /// Delete this saved strategy (persist the updated list).
    Delete(usize),
    /// Snapshot the editor's current source under `save_name` (persist the updated list).
    SaveCurrent,
    /// Run every saved strategy over the selected slice and rank the results.
    CompareAll,
}

impl SavedPane {
    /// Load the list from disk (call once at construction — mirrors `IndicatorsPane`/
    /// `DataBrowserPane`'s one-shot `refresh`, not a per-frame read).
    pub fn load(path: &Path) -> Self {
        Self { strategies: load_strategies(path), ..Self::default() }
    }

    /// `rhai_writer_blocked` is [`crate::studio::StudioState::rhai_writer_blocked_reason`] — `Some`
    /// while the editor holds a Rust buffer with no other copy (Plugin mode). Loading a RHAI row
    /// would overwrite that buffer with the saved script exactly the way the template/copilot
    /// writers would, so a Rhai row's `Load` is disabled under the same reason; a Native or Plugin
    /// row's `Load` never touches the editor and stays enabled regardless.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        rhai_writer_blocked: Option<&'static str>,
    ) -> Option<SavedAction> {
        let mut action = None;
        let tk = Tokens::of(ui.ctx());
        crate::studio::pane_header(ui, icons::SAVED, "Saved strategies");
        // Save row: the name field takes whatever width the Save button leaves — a fixed-width
        // field here is what used to overflow the 340px tools panel and shove the icon rail
        // off-screen (the pre-redesign clipping bug).
        ui.horizontal(|ui| {
            let btn_w = studio::SAVE_BUTTON_W;
            let field_w = (ui.available_width() - btn_w - space::XL).max(60.0);
            ui.scope(|ui| {
                ui.spacing_mut().text_edit_width = field_w;
                input::text(
                    ui,
                    &mut self.save_name,
                    Field { hint: "strategy name", ..Field::default() },
                );
            });
            let can_save = !self.save_name.trim().is_empty();
            if ui
                .add_enabled(can_save, egui::Button::new((icons::SAVE, "Save")))
                .on_hover_text("Save the editor buffer under this name  (Ctrl+S)")
                .on_disabled_hover_text(NAME_IT_FIRST)
                .clicked()
            {
                action = Some(SavedAction::SaveCurrent);
            }
        });
        ui.add_space(space::SM);
        egui::ScrollArea::vertical()
            .id_salt("saved-list")
            .max_height(studio::SAVED_LIST_MAX_H)
            .show(ui, |ui| {
                for (i, s) in self.strategies.iter().enumerate() {
                    ui.horizontal(|ui| {
                        // Load/Delete hug the right edge; the name truncates into what's left, so a
                        // long strategy name can never push the buttons out of the panel.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icons::named(ui.small_button(icons::DELETE), "Delete").clicked() {
                                action = Some(SavedAction::Delete(i));
                            }
                            let load_blocked = (s.source == StrategySource::Rhai)
                                .then_some(rhai_writer_blocked)
                                .flatten();
                            // The refusal is the DISABLED hover: egui shows `on_hover_text` only on an
                            // enabled widget, so a reason put there on a disabled Load was never seen.
                            let load_resp = ui
                                .add_enabled(
                                    load_blocked.is_none(),
                                    egui::Button::new("Load").small(),
                                )
                                .on_hover_text("Load into the editor")
                                .on_disabled_hover_text(load_blocked.unwrap_or_default());
                            if load_resp.clicked() {
                                action = Some(SavedAction::Load(i));
                            }
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    // Kind badge FIRST so a native row is identifiable at a glance —
                                    // its `code` is empty, so the name is the only other signal. A kind
                                    // is a CATEGORY, not a state, so all three wear the one muted badge
                                    // (design system spec §3.2); the words tell them apart.
                                    let tip = match s.source {
                                        StrategySource::Rhai => "Rhai script",
                                        StrategySource::Native => "Native Rust strategy",
                                        StrategySource::Plugin => "Runtime-loaded Rust plugin",
                                    };
                                    chip::badge(ui, s.source.label(), Status::Muted)
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
        let mut compare = ActionButton::primary("Compare all");
        if self.strategies.is_empty() {
            compare = compare.disabled_because(NOTHING_TO_COMPARE);
        }
        if ui
            .add(compare)
            .on_hover_text("Backtest every saved strategy over the selected slice and rank them")
            .clicked()
        {
            action = Some(SavedAction::CompareAll);
        }
        if let Some(err) = &self.compare_error {
            ui.colored_label(Status::Error.color(), err);
        } else if let Some(rows) = &self.compare_rows {
            ui.add_space(space::SM);
            // The 7-column table is wider than the tools panel — give it its own 2-axis scroll
            // instead of letting it clip (the columns stay readable; the panel stays fixed).
            egui::ScrollArea::both()
                .id_salt("saved-compare-scroll")
                .max_height(studio::COMPARE_SCROLL_MAX_H)
                .show(ui, |ui| {
                    egui::Grid::new("saved-compare-grid").striped(true).show(ui, |ui| {
                        ui.strong("Name");
                        ui.strong("Kind");
                        ui.strong("Sharpe");
                        ui.strong("Final equity");
                        ui.strong("Max DD");
                        ui.strong("Trades");
                        ui.strong("Equity");
                        ui.end_row();
                        for (rank, row) in rows.iter().enumerate() {
                            match &row.error {
                                Some(msg) => {
                                    ui.add(egui::Label::new(&row.name).truncate());
                                    ui.monospace(row.source.label());
                                    // The failure REASON stays readable inline (the two-axis
                                    // scroll contains any width); hover keeps the full text for
                                    // very long messages.
                                    ui.colored_label(
                                        Status::Error.color(),
                                        format!("failed: {msg}"),
                                    )
                                    .on_hover_text(msg.as_str());
                                    ui.label("—");
                                    ui.label("—");
                                    ui.label("—");
                                    ui.label("");
                                }
                                None => {
                                    // Rank 0 is the best row (rows are sorted by Sharpe desc). The
                                    // trophy is the mark; the accent is a shape, never the colour
                                    // of a name (design system spec §2, §4.3).
                                    if rank == 0 {
                                        ui.label(
                                            icons::BEST
                                                .before(ui.style(), egui::RichText::new(&row.name)),
                                        );
                                    } else {
                                        ui.add(egui::Label::new(&row.name).truncate());
                                    }
                                    ui.monospace(row.source.label());
                                    // Sign convention: positive up, negative down (the market
                                    // set's colours — money, not a status), zero/NaN neutral — a
                                    // flat no-trade strategy must not read as a loss "0.000"/"NaN".
                                    let sharpe_text =
                                        egui::RichText::new(format!("{:.3}", row.sharpe))
                                            .monospace();
                                    if row.sharpe > 0.0 {
                                        ui.label(
                                            sharpe_text
                                                .color(Pair::PositiveNegative.text(true, &tk)),
                                        );
                                    } else if row.sharpe < 0.0 {
                                        ui.label(
                                            sharpe_text
                                                .color(Pair::PositiveNegative.text(false, &tk)),
                                        );
                                    } else {
                                        ui.label(sharpe_text);
                                    }
                                    ui.monospace(format!("{:.2}", row.final_equity));
                                    // Drawdown is only a loss colour when there IS one.
                                    let dd_text = format!("-{:.1}%", row.max_dd * 100.0);
                                    if row.max_dd > 0.0 {
                                        ui.label(
                                            egui::RichText::new(dd_text)
                                                .monospace()
                                                .color(maps::side::LOSS.text.resolve(&tk)),
                                        );
                                    } else {
                                        ui.monospace(dd_text);
                                    }
                                    ui.monospace(row.n_trades.to_string());
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

/// Paint a small polyline of `equity` into a fixed-size rect (`studio::SPARKLINE_SIZE`) — the per-row sparkline in
/// the compare table. A curve with fewer than 2 points (nothing to draw a line between) or an
/// all-flat curve (zero range, would divide by zero) renders nothing rather than panicking.
fn equity_sparkline(ui: &mut egui::Ui, equity: &[f64]) {
    let size = studio::SPARKLINE_SIZE;
    let (rect, _resp) = ui.allocate_exact_size(size, egui::Sense::hover());
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
    let color = Pair::GainLoss.colour(equity[n - 1] >= equity[0], &Tokens::of(ui.ctx()));
    let stroke = egui::Stroke::new(stroke::LINE, color);
    ui.painter().add(egui::Shape::line(points, stroke));
}

#[path = "saved_tests.rs"]
#[cfg(test)]
mod saved_tests;
