//! Saved strategies: persist named strategies to disk (a `Vec<SavedStrategy>` as pretty JSON
//! colocated with the store, at `store.root().join("studio_strategies.json")`) and compare
//! several saved strategies' backtests over the currently-picked data slice.
//!
//! A saved entry is either a **Rhai** script (the original, and still the default) or a **Native**
//! Rust strategy from the `vike-backtest` harness registry plus its free-form param rows — see
//! [`StrategySource`] and the file-format migration note on it.
//!
//! ⚠ **This blob is being retired in favour of FILES.** `vike_studio_core::user_strategies` loads
//! user strategies from `<project>/user_data/strategies/`, one folder per strategy, and migrates
//! this list into that tree once via [`SavedStrategy::legacy_entry`]. The blob's defects are
//! argued in that module's doc — the short version is that [`load_strategies`] answers a parse
//! failure with an EMPTY list, which is the right call for disposable state and the wrong one for
//! the only copy of somebody's work. After the migration this file is a READ-ONLY fallback:
//! nothing deletes or rewrites it.
//!
//! Load/save (`load_strategies`/`save_strategies`) and the ranking builder
//! (`comparison_rows`) are pure/testable; `SavedPane::ui` (SP2 discipline, mirrors
//! `indicators.rs`/`data_browser.rs`) is thin — it only calls into the tested helpers below plus
//! `StudioState`'s store/picker, which is why running the comparison itself lives in
//! `studio.rs` rather than here.
use std::path::Path;

use serde::{Deserialize, Serialize};
use vike_backtest::{
    BacktestResult,
    metrics::{max_drawdown, sharpe},
};
use vike_studio_core::user_strategies::{LegacyBody, LegacyEntry};
use vike_studio_core::{StrategySpec, params_from_rows};

/// Where a saved strategy's behavior comes from.
///
/// **Backward compatibility is load-bearing.** `studio_strategies.json` files written before native
/// strategies existed have NO `source` key at all; `#[serde(default)]` on the field plus
/// `#[derive(Default)]` here (defaulting to `Rhai`) makes every one of those entries load as the
/// Rhai script it has always been, with its `code` untouched. Never reorder this so that `Native`
/// becomes the default — that would silently reinterpret every legacy entry's `code` as a registry
/// name. Pinned by `legacy_file_without_a_source_field_loads_as_rhai`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategySource {
    #[default]
    Rhai,
    Native,
}

impl StrategySource {
    /// The short badge the Saved list / compare table shows.
    pub fn label(self) -> &'static str {
        match self {
            StrategySource::Rhai => "rhai",
            StrategySource::Native => "native",
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
        }
    }

    /// The runnable [`StrategySpec`] this entry denotes — the ONE place a persisted row becomes
    /// something `vike_studio_core::run_slice` can execute.
    pub fn spec(&self) -> StrategySpec {
        match self.source {
            StrategySource::Rhai => StrategySpec::rhai(self.code.clone()),
            StrategySource::Native => {
                StrategySpec::native(self.native.clone(), params_from_rows(&self.params))
            }
        }
    }

    /// This row as the neutral input `vike_studio_core::user_strategies`'s `plan_migration` reads
    /// — the ONE place the legacy JSON schema becomes the file-tree migration's vocabulary.
    ///
    /// The conversion lives HERE, beside the schema, rather than in the crate that performs the
    /// migration: `vike-studio-core` sits below this crate and cannot name [`SavedStrategy`], and
    /// a second serde mirror of this format down there would be a second authority for the
    /// back-compat contract [`StrategySource`] documents. So the schema stays in one file and the
    /// caller hands over already-parsed rows — the same split the rest of the workspace draws
    /// between a library and its composition root.
    ///
    /// ⚠ **`load_strategies` stays; `save_strategies` is what the migration retires.** After the
    /// user's rows exist as files the JSON is a READ-ONLY fallback: it is never written, moved or
    /// deleted again, so a build the user rolls back to still finds their strategies.
    pub fn legacy_entry(&self) -> LegacyEntry {
        LegacyEntry {
            name: self.name.clone(),
            body: match self.source {
                StrategySource::Rhai => LegacyBody::Rhai { code: self.code.clone() },
                StrategySource::Native => {
                    LegacyBody::Native { native: self.native.clone(), params: self.params.clone() }
                }
            },
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
/// Successful rows are sorted by annualized Sharpe (252 periods/year, matching the rest of the
/// Studio — see `run.rs`) descending, with NaN Sharpes (e.g. a flat equity curve) sorted last
/// among successes; every failed row sorts after every successful row (regardless of Sharpe,
/// since failed rows have none) — surfaced, not silently dropped.
///
/// `source_of` maps a row's NAME back to its [`StrategySource`] for the Kind column. It is a
/// lookup rather than a third tuple element because `vike_studio_core::CompareOutcome` (what the
/// worker thread delivers) carries names only — the caller already holds the saved list the names
/// came from. Rhai-only callers pass `|_| StrategySource::Rhai`.
pub fn comparison_rows(
    results: &[(String, Result<BacktestResult, String>)],
    source_of: impl Fn(&str) -> StrategySource,
) -> Vec<CompareRow> {
    let mut rows: Vec<CompareRow> = results
        .iter()
        .map(|(name, outcome)| match outcome {
            Ok(r) => CompareRow {
                name: name.clone(),
                source: source_of(name),
                sharpe: sharpe(&r.equity_curve, 252.0),
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

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<SavedAction> {
        let mut action = None;
        crate::theme::section_header(ui, "★", "Saved strategies");
        // Save row: the name field takes whatever width the Save button leaves — a fixed-width
        // field here is what used to overflow the 340px tools panel and shove the icon rail
        // off-screen (the pre-redesign clipping bug).
        ui.horizontal(|ui| {
            let btn_w = 64.0;
            let field_w = (ui.available_width() - btn_w - 12.0).max(60.0);
            ui.add_sized(
                [field_w, 20.0],
                egui::TextEdit::singleline(&mut self.save_name).hint_text("strategy name"),
            );
            let can_save = !self.save_name.trim().is_empty();
            if ui
                .add_enabled(can_save, egui::Button::new("💾 Save"))
                .on_hover_text("Save the editor buffer under this name  (Ctrl+S)")
                .clicked()
            {
                action = Some(SavedAction::SaveCurrent);
            }
        });
        ui.add_space(4.0);
        egui::ScrollArea::vertical().id_salt("saved-list").max_height(200.0).show(ui, |ui| {
            for (i, s) in self.strategies.iter().enumerate() {
                ui.horizontal(|ui| {
                    // Load/Delete hug the right edge; the name truncates into what's left, so a
                    // long strategy name can never push the buttons out of the panel.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").on_hover_text("Delete").clicked() {
                            action = Some(SavedAction::Delete(i));
                        }
                        if ui.small_button("Load").on_hover_text("Load into the editor").clicked() {
                            action = Some(SavedAction::Load(i));
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            // Kind badge FIRST so a native row is identifiable at a glance —
                            // its `code` is empty, so the name is the only other signal.
                            let (color, tip) = match s.source {
                                StrategySource::Rhai => (crate::theme::ACCENT, "Rhai script"),
                                StrategySource::Native => {
                                    (crate::theme::OK, "Native Rust strategy")
                                }
                            };
                            ui.label(
                                egui::RichText::new(s.source.label())
                                    .color(color)
                                    .size(10.0)
                                    .monospace(),
                            )
                            .on_hover_text(tip);
                            ui.add(egui::Label::new(&s.name).truncate());
                        });
                    });
                });
            }
            if self.strategies.is_empty() {
                ui.weak("No saved strategies yet — name the editor buffer above and Save.");
            }
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        let can_compare = !self.strategies.is_empty();
        if ui
            .add_enabled(can_compare, crate::theme::primary("Compare all"))
            .on_hover_text("Backtest every saved strategy over the selected slice and rank them")
            .clicked()
        {
            action = Some(SavedAction::CompareAll);
        }
        if let Some(err) = &self.compare_error {
            ui.colored_label(crate::theme::ERR, err);
        } else if let Some(rows) = &self.compare_rows {
            ui.add_space(4.0);
            // The 7-column table is wider than the tools panel — give it its own 2-axis scroll
            // instead of letting it clip (the columns stay readable; the panel stays fixed).
            egui::ScrollArea::both().id_salt("saved-compare-scroll").max_height(280.0).show(
                ui,
                |ui| {
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
                                    ui.colored_label(crate::theme::ERR, format!("failed: {msg}"))
                                        .on_hover_text(msg.as_str());
                                    ui.label("—");
                                    ui.label("—");
                                    ui.label("—");
                                    ui.label("");
                                }
                                None => {
                                    // Rank 0 is the best row (rows are sorted by Sharpe desc).
                                    if rank == 0 {
                                        ui.label(
                                            egui::RichText::new(format!("★ {}", row.name))
                                                .color(crate::theme::ACCENT),
                                        );
                                    } else {
                                        ui.add(egui::Label::new(&row.name).truncate());
                                    }
                                    ui.monospace(row.source.label());
                                    // Sign convention: positive OK, negative ERR, zero/NaN
                                    // neutral — a flat no-trade strategy must not read as an
                                    // error-red "0.000"/"NaN".
                                    let sharpe_text = format!("{:.3}", row.sharpe);
                                    if row.sharpe > 0.0 {
                                        ui.colored_label(crate::theme::OK, sharpe_text);
                                    } else if row.sharpe < 0.0 {
                                        ui.colored_label(crate::theme::ERR, sharpe_text);
                                    } else {
                                        ui.monospace(sharpe_text);
                                    }
                                    ui.monospace(format!("{:.2}", row.final_equity));
                                    // Drawdown is only "bad red" when there IS one.
                                    let dd_text = format!("-{:.1}%", row.max_dd * 100.0);
                                    if row.max_dd > 0.0 {
                                        ui.colored_label(crate::theme::ERR, dd_text);
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
                },
            );
        }
        action
    }
}

/// Paint a small green polyline of `equity` into a fixed 64x18 rect — the per-row sparkline in
/// the compare table. A curve with fewer than 2 points (nothing to draw a line between) or an
/// all-flat curve (zero range, would divide by zero) renders nothing rather than panicking.
fn equity_sparkline(ui: &mut egui::Ui, equity: &[f64]) {
    let size = egui::vec2(64.0, 18.0);
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
    // Color by outcome (end vs start), not a flat green — a losing curve drawn in success-green
    // undercuts the one-palette contract the theme exists for.
    let color = if equity[n - 1] >= equity[0] { crate::theme::OK } else { crate::theme::ERR };
    let stroke = egui::Stroke::new(1.5, color);
    ui.painter().add(egui::Shape::line(points, stroke));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strat(name: &str, code: &str) -> SavedStrategy {
        SavedStrategy::rhai(name, code)
    }

    /// MIGRATION GATE: an existing `studio_strategies.json` written before native strategies
    /// existed has NO `source`/`native`/`params` keys. It must still load, keep its code, and be
    /// treated as Rhai — the entire back-compat contract of this file format in one test.
    #[test]
    fn legacy_file_without_a_source_field_loads_as_rhai() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio_strategies.json");
        // Byte-for-byte the shape `save_strategies` used to emit.
        std::fs::write(
            &path,
            r#"[
  {
    "name": "sma-cross",
    "code": "fn on_bar() {}"
  }
]"#,
        )
        .unwrap();

        let loaded = load_strategies(&path);

        assert_eq!(loaded.len(), 1, "the legacy entry must survive the migration");
        assert_eq!(loaded[0].name, "sma-cross");
        assert_eq!(loaded[0].code, "fn on_bar() {}", "its script must be untouched");
        assert_eq!(loaded[0].source, StrategySource::Rhai, "a missing source key means Rhai");
        assert!(loaded[0].native.is_empty());
        assert!(loaded[0].params.is_empty());
        assert_eq!(
            loaded[0].spec(),
            StrategySpec::rhai("fn on_bar() {}"),
            "and it still RUNS as the Rhai script it always was"
        );
    }

    /// A native entry round-trips through the file, and its `spec()` carries the typed params —
    /// the forward half of the migration.
    #[test]
    fn native_entry_round_trips_and_specs_with_typed_params() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio_strategies.json");
        let entry = SavedStrategy::native(
            "hold-2",
            "buy_hold",
            vec![("size".into(), "2".into()), ("symbol".into(), "BTCUSDT".into())],
        );
        save_strategies(&path, std::slice::from_ref(&entry)).unwrap();
        let loaded = load_strategies(&path);
        assert_eq!(loaded, vec![entry]);

        match loaded[0].spec() {
            StrategySpec::Native { name, params } => {
                assert_eq!(name, "buy_hold");
                assert_eq!(params.get("size").and_then(toml::Value::as_integer), Some(2));
                assert_eq!(params.get("symbol").and_then(toml::Value::as_str), Some("BTCUSDT"));
            }
            other => panic!("expected a native spec, got {other:?}"),
        }
    }

    /// A mixed file (one legacy-shaped Rhai row + one native row) loads both, each with the right
    /// source — the realistic post-upgrade file.
    #[test]
    fn mixed_legacy_and_native_file_loads_both() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio_strategies.json");
        std::fs::write(
            &path,
            r#"[
  { "name": "old", "code": "fn on_bar() {}" },
  { "name": "new", "code": "", "source": "native", "native": "buy_hold",
    "params": [["size", "1"]] }
]"#,
        )
        .unwrap();
        let loaded = load_strategies(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].source, StrategySource::Rhai);
        assert_eq!(loaded[1].source, StrategySource::Native);
        assert_eq!(loaded[1].native, "buy_hold");
        assert_eq!(loaded[1].params, vec![("size".to_string(), "1".to_string())]);
    }

    /// The bridge to the file-tree migration: BOTH populations map to their own legacy body, and
    /// a native row keeps its param ROWS as text (the migration is what turns them into TOML —
    /// see `vike_studio_core::user_strategies`'s `plan_migration`). A native row mapped as Rhai
    /// would migrate an empty script under the user's name and lose the params entirely.
    #[test]
    fn legacy_entry_maps_both_sources_to_their_own_body() {
        let script = SavedStrategy::rhai("sma-cross", "fn on_bar() {}");
        assert_eq!(
            script.legacy_entry(),
            LegacyEntry {
                name: "sma-cross".to_string(),
                body: LegacyBody::Rhai { code: "fn on_bar() {}".to_string() },
            }
        );

        let rows = vec![("size".to_string(), "2".to_string())];
        let preset = SavedStrategy::native("hold-2", "buy_hold", rows.clone());
        assert_eq!(
            preset.legacy_entry(),
            LegacyEntry {
                name: "hold-2".to_string(),
                body: LegacyBody::Native { native: "buy_hold".to_string(), params: rows },
            }
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio_strategies.json");
        let list =
            vec![strat("sma-cross", "fn on_bar() {}"), strat("rsi-mean-revert", "let x = 1;")];

        save_strategies(&path, &list).unwrap();
        let loaded = load_strategies(&path);

        assert_eq!(loaded, list);
    }

    #[test]
    fn load_of_a_missing_path_is_an_empty_vec_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.json");
        assert_eq!(load_strategies(&path), Vec::new());
    }

    #[test]
    fn load_of_a_corrupt_file_is_an_empty_vec_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.json");
        std::fs::write(&path, "{ not: valid json ]]]").unwrap();
        assert_eq!(load_strategies(&path), Vec::new());
    }

    #[test]
    fn save_overwrites_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio_strategies.json");
        save_strategies(&path, &[strat("a", "1")]).unwrap();
        save_strategies(&path, &[strat("b", "2")]).unwrap();
        assert_eq!(load_strategies(&path), vec![strat("b", "2")]);
    }

    fn result(equity_curve: Vec<f64>, n_trades: usize) -> BacktestResult {
        let final_equity = *equity_curve.last().unwrap();
        BacktestResult { equity_curve, final_equity, n_trades, ..Default::default() }
    }

    #[test]
    fn comparison_rows_sorts_by_sharpe_descending_and_carries_fields() {
        // a strongly uptrending curve (high sharpe), a flat/noisy one (low/negative sharpe).
        let good = result(vec![100.0, 105.0, 110.0, 116.0, 123.0, 131.0], 4);
        let bad = result(vec![100.0, 98.0, 101.0, 97.0, 100.0, 96.0], 9);
        let results =
            vec![("bad".to_string(), Ok(bad.clone())), ("good".to_string(), Ok(good.clone()))];

        let rows = comparison_rows(&results, |_| StrategySource::Rhai);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "good", "higher sharpe should rank first");
        assert_eq!(rows[1].name, "bad");
        assert!(rows[0].sharpe > rows[1].sharpe);
        assert_eq!(rows[0].final_equity, *good.equity_curve.last().unwrap());
        assert_eq!(rows[0].n_trades, 4);
        assert_eq!(rows[1].n_trades, 9);
        assert!(rows[0].error.is_none());
        assert!(rows[1].error.is_none());
    }

    #[test]
    fn comparison_rows_puts_nan_sharpe_last() {
        // an equity curve with a NaN point (e.g. a div-by-zero upstream) makes every return NaN,
        // so `sharpe` (metrics.rs) itself comes back NaN — a flat curve is NOT this case; `sharpe`
        // explicitly guards zero variance to 0.0, not NaN (see metrics.rs's own doc).
        let broken = result(vec![100.0, f64::NAN, 100.0], 0);
        let trending = result(vec![100.0, 102.0, 104.5, 107.0], 2);
        let results =
            vec![("broken".to_string(), Ok(broken)), ("trending".to_string(), Ok(trending))];

        let rows = comparison_rows(&results, |_| StrategySource::Rhai);

        assert_eq!(rows[0].name, "trending");
        assert_eq!(rows[1].name, "broken");
        assert!(rows[1].sharpe.is_nan());
    }

    #[test]
    fn comparison_rows_on_empty_input_is_empty() {
        assert!(comparison_rows(&[], |_| StrategySource::Rhai).is_empty());
    }

    /// Hand-verified against `metrics::max_drawdown`'s own contract (running-peak fractional
    /// decline, NOT a negative number): peak hits 120 after the second point, then the deepest
    /// dip to 90 is `(120-90)/120 == 0.25`; the later partial recovery to 110 is a smaller
    /// drawdown (`(120-110)/120 ≈ 0.083`) so 0.25 stays the worst.
    #[test]
    fn comparison_rows_computes_max_dd_correctly() {
        let r = result(vec![100.0, 120.0, 90.0, 110.0], 3);
        let results = vec![("dd-check".to_string(), Ok(r))];

        let rows = comparison_rows(&results, |_| StrategySource::Rhai);

        assert_eq!(rows.len(), 1);
        assert!((rows[0].max_dd - 0.25).abs() < 1e-12, "max_dd = {}", rows[0].max_dd);
    }

    /// A successful row carries the full equity curve (for the sparkline); a failed row carries
    /// none.
    #[test]
    fn comparison_rows_carries_the_equity_curve_for_successful_rows() {
        let curve = vec![100.0, 101.0, 99.0, 103.0];
        let r = result(curve.clone(), 1);
        let results =
            vec![("ok".to_string(), Ok(r)), ("broken".to_string(), Err("boom".to_string()))];

        let rows = comparison_rows(&results, |_| StrategySource::Rhai);

        let ok_row = rows.iter().find(|r| r.name == "ok").unwrap();
        assert_eq!(ok_row.equity, curve);
        let failed_row = rows.iter().find(|r| r.name == "broken").unwrap();
        assert!(failed_row.equity.is_empty());
    }

    /// A strategy that failed to compile/run is surfaced as a row with `error: Some(..)` rather
    /// than silently dropped, and sorts after every successful row regardless of the successful
    /// rows' Sharpe ranking.
    #[test]
    fn comparison_rows_surfaces_a_failed_strategy_and_sorts_it_last() {
        let good = result(vec![100.0, 105.0, 110.0, 116.0], 2);
        let results = vec![
            ("broken".to_string(), Err("compile error: unexpected token".to_string())),
            ("good".to_string(), Ok(good)),
        ];

        let rows = comparison_rows(&results, |_| StrategySource::Rhai);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "good", "the successful row ranks first");
        assert!(rows[0].error.is_none());
        assert_eq!(rows[1].name, "broken");
        assert_eq!(rows[1].error.as_deref(), Some("compile error: unexpected token"));
    }
}
