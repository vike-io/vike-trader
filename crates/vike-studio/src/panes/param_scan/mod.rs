//! The Param scan & Validate pane's own parts: the parameter rows it keeps per strategy
//! ([`StudioParamRow`]), the reading of a row's range (`5..30 step 5`) and of the scan's size, the
//! discovery of a strategy's own parameters (a strategy file header's `[params]`, a Rhai script's
//! `param()`s, and the numeric `const`s a script would have to turn into `param()`s to be scanned), and
//! the last scan's results drawn in the pane: a heatmap over two scanned parameters and a sortable table.
//!
//! A row's VALUE is what a Run uses and a TICKED row is scanned over its RANGE. The results are the
//! scan's own summary numbers (`vike_backtesthub_client::summary::ParamscanPointWire`, no drawdown among
//! them); a clicked point opens as the results pane opens one (`StudioResultsAction::OpenScanPoint`).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use vike_analytics::INVALID_LABEL;
use vike_backtesthub_client::summary::{
    PARAMSCAN_ROWS_PAGE_SIZE_MAX, ParamscanPointWire, ParamscanSummaryWire,
};
use vike_config::params_text::{parse_axis, typed_by_base};
use vike_model::params::ParamValue;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::icons;
use vike_ui_theme::value::studio;

/// One parameter of the strategy as the pane keeps it, persisted per strategy in the workspace.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct StudioParamRow {
    /// The parameter's name, as the strategy declares it.
    pub name: String,
    /// The value a Run uses; empty: the strategy's own.
    pub value: String,
    /// Whether the scan varies it over [`Self::range`].
    pub scan: bool,
    /// `5..30 step 5` or a comma list (`5, 10, 20`).
    pub range: String,
    /// The strategy's own default as text: the value field's hint. Rediscovered, never persisted.
    #[serde(skip)]
    pub default: String,
}

impl StudioParamRow {
    /// A row ticked to scan `range`.
    pub fn scanned(name: &str, range: &str) -> Self {
        Self { name: name.to_string(), scan: true, range: range.to_string(), ..Self::default() }
    }
}

/// **The test conditions**: the market a strategy is tested in, set once and used by a Run, a scan and a
/// walk-forward alike, persisted in the workspace (not per strategy).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub(crate) struct StudioTestConditions {
    /// The starting capital as typed; empty: the engine's default.
    pub(crate) capital: String,
    /// One row per cost param (`crate::studio`'s `STUDIO_COST_ROWS`: `commission`, `slippage`): its value
    /// every run uses (empty: the strategy's own) and, under "Advanced: also vary costs", a range a scan
    /// may vary it over.
    pub(crate) costs: Vec<StudioParamRow>,
    /// "Include funding": charge a perpetual's stored funding to every run. OFF unless ticked (the owner's ruling
    /// of 2026-11-20: a backtest runs without funding unless he asks). Absent from an older workspace: off.
    #[serde(default)]
    pub(crate) funding: bool,
}

impl StudioTestConditions {
    /// The starting capital, when the text is a number above zero.
    pub(crate) fn capital(&self) -> Option<f64> {
        self.capital.trim().parse::<f64>().ok().filter(|c| c.is_finite() && *c > 0.0)
    }
}

/// A row's text as a RANGE, `low..high step s` (`low..high` steps by 1): `low`, `low + s`, … up to
/// `high`, included when a step lands on it. Whole numbers when all three are written whole, else
/// numbers rounded to the decimals `low` and `s` were written with, so `0.1..0.3 step 0.1` ends at
/// `0.3`. `None` when the text is not a range (it has no `..`); `Some` of nothing when it is one that
/// names no value: a bound that is not a number, a step at or below zero, `high` below `low`, or more
/// than [`PARAMSCAN_ROWS_PAGE_SIZE_MAX`] values (a typo such as `0..1e9` must not expand on the paint
/// thread).
pub(crate) fn range_values(text: &str) -> Option<Vec<ParamValue>> {
    let text = text.trim().to_ascii_lowercase();
    let (low, rest) = text.split_once("..")?;
    let (high, step) = rest.split_once("step").unwrap_or((rest, "1"));
    let number = |s: &str| s.trim().parse::<f64>().ok().filter(|v| v.is_finite());
    let (Some(lo), Some(hi), Some(st)) = (number(low), number(high), number(step)) else {
        return Some(Vec::new());
    };
    let steps = ((hi - lo) / st + 1e-9).floor();
    if st <= 0.0 || hi < lo || steps >= PARAMSCAN_ROWS_PAGE_SIZE_MAX as f64 {
        return Some(Vec::new());
    }
    let whole = [low, high, step].iter().all(|s| s.trim().parse::<i64>().is_ok());
    // An exponent (`1e-3`) has no decimals to count, so it is left unrounded, and so is a number written
    // with more decimals than a power of ten holds exactly (past 22, `power_of_ten` has no answer).
    let decimals =
        [low, step].iter().map(|s| s.trim()).try_fold(0, |most, s| match s.contains('e') {
            true => None,
            false => Some(s.split_once('.').map_or(0, |(_, f)| f.len()).max(most)),
        });
    let scale =
        decimals.and_then(|d| i32::try_from(d).ok()).and_then(vike_math::powers::power_of_ten);
    Some(
        (0..=steps as usize)
            .map(|i| {
                let v = lo + i as f64 * st;
                match (whole, scale) {
                    (true, _) => ParamValue::Int(v.round() as i64),
                    (false, Some(scale)) => ParamValue::Float((v * scale).round() / scale),
                    (false, None) => ParamValue::Float(v),
                }
            })
            .collect(),
    )
}

/// One row's text → its values: a range ([`range_values`]) typed by `base` as a list's values are, else
/// the comma list or TOML array `vike_config::params_text::parse_axis` reads (the CLI's
/// `--set paramscan.<k>=…` reads no range).
pub(crate) fn axis_values(text: &str, base: Option<&ParamValue>) -> Vec<ParamValue> {
    match range_values(text) {
        Some(values) => values.into_iter().map(|v| typed_by_base(v, base)).collect(),
        None => parse_axis(text, base),
    }
}

/// How many runs a scan of axes of these sizes is: their product, saturating rather than wrapping.
pub(crate) fn scan_total(counts: &[usize]) -> usize {
    counts.iter().fold(1usize, |total, &n| total.saturating_mul(n))
}

/// The scan's size in words: `6 × 5 = 30 runs`, `6 runs` for one scanned row, empty for none.
pub(crate) fn scan_size_line(counts: &[usize]) -> String {
    let total = scan_total(counts);
    let runs = if total == 1 { "1 run".to_string() } else { format!("{total} runs") };
    match counts {
        [] => String::new(),
        [_] => runs,
        _ => {
            let factors: Vec<String> = counts.iter().map(ToString::to_string).collect();
            format!("{} = {runs}", factors.join(" × "))
        }
    }
}

/// `line` as a top-level `const NAME = <number>;` declaration: its name, its number as written and what
/// follows the `;`. Only a line starting in the first column counts (a hook body's lines are indented).
fn numeric_constant(line: &str) -> Option<(&str, &str, &str)> {
    let rest = line.strip_prefix("const ")?;
    let (name, rest) = rest.split_once('=')?;
    let name = name.trim();
    let first = name.chars().next()?;
    let word = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !(first.is_ascii_alphabetic() || first == '_') || !word {
        return None;
    }
    let (value, tail) = rest.split_once(';')?;
    let value = value.trim();
    value.parse::<f64>().ok().filter(|v| v.is_finite())?;
    Some((name, value, tail))
}

/// Each line of `src` (its `\n` kept), with whether it is code: a line that STARTS inside a string literal
/// is not. A backtick string spans lines, and a `"` one does past a `\` at its line's end; a `'x'`
/// character and a `//` comment open none.
// shortcut: a `/* */` comment is not read (a quote inside one opens a string); read it if one bites.
fn code_lines(src: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut open: Option<char> = None;
    src.split_inclusive('\n').map(move |line| {
        let code = open.is_none();
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match (open, c) {
                // Only a `"` string escapes; a backtick string is literal.
                (Some('"'), '\\') => {
                    chars.next();
                }
                (Some(quote), c) if c == quote => open = None,
                (Some(_), _) => {}
                (None, '"' | '`') => open = Some(c),
                (None, '/') if chars.as_str().starts_with('/') => break,
                (None, '\'') => {
                    let mut escaped = false;
                    for c in chars.by_ref() {
                        match (escaped, c) {
                            (false, '\\') => escaped = true,
                            (false, '\'') => break,
                            _ => escaped = false,
                        }
                    }
                }
                (None, _) => {}
            }
        }
        (line, code)
    })
}

/// The names of `src`'s top-level numeric `const`s: numbers a scan cannot vary, since a scan varies only
/// what `param()` reads. A line inside a string literal is text, not a `const` ([`code_lines`]).
pub(crate) fn fixed_constants(src: &str) -> Vec<String> {
    let code = code_lines(src).filter(|(_, code)| *code).map(|(line, _)| line);
    code.filter_map(numeric_constant).map(|(name, _, _)| name.to_string()).collect()
}

/// `src` with each of [`fixed_constants`] reading a `param()` of its own name and number
/// (`const FAST = param("FAST", 5).to_int();`, the `.to_int()` keeping a whole number whole): the idiom
/// `crates/vike-tradehub/src/config/mount.rs`'s `resolve_script` documents. Every other line is untouched,
/// a line inside a string literal among them.
pub(crate) fn constants_as_params(src: &str) -> String {
    code_lines(src)
        .map(|(line, code)| match numeric_constant(line).filter(|_| code) {
            Some((name, value, tail)) => {
                let to_int = if value.parse::<i64>().is_ok() { ".to_int()" } else { "" };
                format!("const {name} = param(\"{name}\", {value}){to_int};{tail}")
            }
            None => line.to_string(),
        })
        .collect()
}

/// A parameter value as the pane prints it: a `Float` as an `f64` prints (`5`, `2.5`), anything else
/// its TOML text.
pub(crate) fn value_text(v: &ParamValue) -> String {
    match v {
        ParamValue::Float(x) => format!("{x}"),
        other => format!("{other}"),
    }
}

/// A strategy's own parameters as `(name, default as text)`, in declaration order: a strategy file
/// header's `number` and `integer` keys (`vike_strategy::file::StrategyFileHeader`; the default, else
/// the range's low end, else empty) when the text carries a header, else a Rhai script's `param()`s
/// (`vike_rhai::discover_params`). `None` when neither reads, as for a script that does not compile:
/// the caller keeps the rows it has.
pub(crate) fn declared_params(text: &str) -> Option<Vec<(String, String)>> {
    use vike_strategy::file::{ParamType, StrategyFileHeader};
    if let Ok(header) = StrategyFileHeader::parse(text) {
        let numeric = |p: &&(String, vike_strategy::file::StrategyFileHeaderParam)| {
            matches!(p.1.r#type, ParamType::Number | ParamType::Integer)
        };
        let default = |p: &vike_strategy::file::StrategyFileHeaderParam| match (&p.default, p.range)
        {
            (Some(v), _) => value_text(v),
            (None, Some([low, _])) => format!("{low}"),
            (None, None) => String::new(),
        };
        return Some(
            header.params.iter().filter(numeric).map(|(n, p)| (n.clone(), default(p))).collect(),
        );
    }
    let found = vike_rhai::discover_params(text).ok()?;
    Some(found.into_iter().map(|(name, default)| (name, format!("{default}"))).collect())
}

/// `rows` re-cut to `declared`: one row per declared name in its order, keeping what the user set on a
/// row of that name (in `rows`, else in `dropped`), each carrying its default. A row the strategy no
/// longer declares moves to `dropped`, so a name edited mid-word (`fast`, `fas`, `fast`: each keystroke
/// compiles) gets its value, tick and range back; `dropped` empties at the first re-cut that declares
/// exactly the names `rows` had, once the edit has settled.
pub(crate) fn rows_for(
    declared: Vec<(String, String)>,
    rows: &[StudioParamRow],
    dropped: &mut Vec<StudioParamRow>,
) -> Vec<StudioParamRow> {
    if declared.iter().map(|(name, _)| name).eq(rows.iter().map(|r| &r.name)) {
        dropped.clear();
    }
    let cut: Vec<StudioParamRow> = declared
        .into_iter()
        .map(|(name, default)| {
            let kept = rows.iter().chain(dropped.iter()).find(|r| r.name == name).cloned();
            let row = kept.unwrap_or_else(|| StudioParamRow { name, ..StudioParamRow::default() });
            StudioParamRow { default, ..row }
        })
        .collect();
    for row in rows.iter().filter(|r| !cut.iter().any(|c| c.name == r.name)) {
        dropped.retain(|d| d.name != row.name);
        dropped.push(row.clone());
    }
    cut
}

/// How long a scan took, in words: `12.4 seconds`, `2 minutes 5 seconds`.
pub(crate) fn took_text(took: Duration) -> String {
    let seconds = took.as_secs_f64();
    if seconds < 60.0 {
        format!("{seconds:.1} seconds")
    } else {
        format!("{} minutes {} seconds", took.as_secs() / 60, took.as_secs() % 60)
    }
}

fn override_of<'a>(point: &'a ParamscanPointWire, name: &str) -> Option<&'a ParamValue> {
    point.overrides.iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// `name=value, …` of one point's overrides.
pub(crate) fn point_label(point: &ParamscanPointWire) -> String {
    let pairs: Vec<String> =
        point.overrides.iter().map(|(k, v)| format!("{k}={}", value_text(v))).collect();
    pairs.join(", ")
}

/// The scan's parameters that vary from point to point, in the first point's order: the heatmap's
/// axes and the table's parameter columns. A parameter held at one value is not one of them.
pub(crate) fn scanned_names(sweep: &ParamscanSummaryWire) -> Vec<String> {
    let Some(first) = sweep.points.first() else { return Vec::new() };
    first
        .overrides
        .iter()
        .filter(|(name, value)| {
            sweep.points.iter().any(|p| override_of(p, name).is_some_and(|v| v != value))
        })
        .map(|(name, _)| name.clone())
        .collect()
}

fn compare_values(a: &ParamValue, b: &ParamValue) -> std::cmp::Ordering {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        _ => value_text(a).cmp(&value_text(b)),
    }
}

/// `name`'s distinct values over the scan's points, in ascending order.
fn values_of(sweep: &ParamscanSummaryWire, name: &str) -> Vec<ParamValue> {
    let mut values: Vec<ParamValue> = Vec::new();
    for v in sweep.points.iter().filter_map(|p| override_of(p, name)) {
        if !values.contains(v) {
            values.push(v.clone());
        }
    }
    values.sort_by(compare_values);
    values
}

/// The heatmap over `x` (across) and `y` (down): their values in order, and for each row of `y` and
/// column of `x` the index of the best point there, `None` where the scan has none. The points are
/// ranked best first, so the first one met is the best; with more than two scanned parameters a cell
/// is the best over the others.
pub(crate) fn heatmap_cells(
    sweep: &ParamscanSummaryWire,
    x: &str,
    y: &str,
) -> (Vec<ParamValue>, Vec<ParamValue>, Vec<Vec<Option<usize>>>) {
    let (xs, ys) = (values_of(sweep, x), values_of(sweep, y));
    let cells = ys
        .iter()
        .map(|yv| {
            xs.iter()
                .map(|xv| {
                    sweep.points.iter().position(|p| {
                        override_of(p, x) == Some(xv) && override_of(p, y) == Some(yv)
                    })
                })
                .collect()
        })
        .collect();
    (xs, ys, cells)
}

/// A point's Sharpe when it ran and is valid, else `None`: what colours a cell and sorts a column.
fn ranked_sharpe(point: &ParamscanPointWire) -> Option<f64> {
    point.sharpe.filter(|s| s.is_finite() && point.invalid.is_none())
}

/// The table's columns after the parameters: the ranking metric first. The maximum drawdown (third) is the one
/// whose best is its LOWEST.
const METRIC_COLUMNS: [&str; 5] =
    ["Sharpe", "Total return", "Maximum drawdown", "Final equity", "Trades"];

/// One point's value in column `column` of a table whose first columns are `names`, for sorting.
fn sort_key(point: &ParamscanPointWire, names: &[String], column: usize) -> Option<f64> {
    match column.checked_sub(names.len()) {
        None => override_of(point, &names[column]).and_then(ParamValue::as_f64),
        Some(0) => ranked_sharpe(point),
        Some(1) => point.total_return.filter(|_| point.invalid.is_none()),
        Some(2) => point.max_drawdown.filter(|_| point.invalid.is_none()),
        Some(3) => Some(point.final_equity),
        Some(_) => Some(point.n_trades as f64),
    }
}

/// Whether a click on `column` (of a table whose first columns are `names`) sorts it falling first: a metric's
/// best on top, which for the maximum drawdown is its lowest; a parameter rises first.
fn falls_first(names: &[String], column: usize) -> bool {
    !matches!(column.checked_sub(names.len()), None | Some(2))
}

/// A point's maximum drawdown cell: a percent of the peak, INVALID for a point whose equity reached zero, `—`
/// from an older server, which sends none.
pub(crate) fn drawdown_cell(point: &ParamscanPointWire) -> String {
    match (point.invalid.is_some(), point.max_drawdown) {
        (true, _) => INVALID_LABEL.to_string(),
        (false, Some(dd)) => crate::panes::results::percent(dd),
        (false, None) => "—".to_string(),
    }
}

/// The table's rows in order: ranked (the scan's own order) when `sort` is `None`, else by that
/// column, falling when its flag says so; a point with no number there sorts last either way.
pub(crate) fn sorted_rows(
    sweep: &ParamscanSummaryWire,
    names: &[String],
    sort: Option<(usize, bool)>,
) -> Vec<usize> {
    let mut rows: Vec<usize> = (0..sweep.points.len()).collect();
    if let Some((column, falling)) = sort {
        rows.sort_by(|&a, &b| {
            let key = |i: usize| sort_key(&sweep.points[i], names, column);
            match (key(a), key(b)) {
                (Some(x), Some(y)) if falling => y.total_cmp(&x),
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });
    }
    rows
}

/// What the pane keeps between frames about the results on screen: the two parameters the heatmap
/// spans (indices into [`scanned_names`]) and the table's sort. Egui's memory: a view choice, not
/// state the Studio persists.
#[derive(Clone, Copy, Default)]
struct ScanView {
    x: usize,
    y: usize,
    sort: Option<(usize, bool)>,
}

/// **The last scan's results, drawn in the pane**: a heatmap of the Sharpe over two scanned parameters
/// (chosen when the scan varies more than two), the best point marked with the trophy, then the table of
/// every point, sortable by any column. `shown` is the point on screen in the results pane. Returns the
/// point clicked, for the caller to open.
pub(crate) fn scan_results_ui(
    ui: &mut egui::Ui,
    sweep: &ParamscanSummaryWire,
    shown: Option<usize>,
) -> Option<usize> {
    let id = egui::Id::unique("studio-param-scan-view");
    let mut view: ScanView = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    // Solid bars on the heatmap's and the table's scrolls: egui's floating ones show only under a hovering
    // pointer, so the table's last columns read as cut off at the panel's edge.
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    let names = scanned_names(sweep);
    let mut open = None;
    if names.len() >= 2 {
        view.x = view.x.min(names.len() - 1);
        view.y = view.y.min(names.len() - 1);
        if view.x == view.y {
            (view.x, view.y) = (0, 1);
        }
        if names.len() > 2 {
            ui.horizontal(|ui| {
                axis_picker(ui, "Across", "studio-param-scan-x", &names, &mut view.x);
                axis_picker(ui, "Down", "studio-param-scan-y", &names, &mut view.y);
            });
        }
        open = heatmap_ui(ui, sweep, &names[view.x], &names[view.y], shown);
        ui.add_space(vike_ui_theme::metrics::space::MD);
    }
    open = table_ui(ui, sweep, &names, shown, &mut view.sort).or(open);
    ui.data_mut(|d| d.insert_temp(id, view));
    open
}

fn axis_picker(ui: &mut egui::Ui, words: &str, salt: &str, names: &[String], at: &mut usize) {
    let label = ui.label(egui::RichText::new(words).weak()).id;
    egui::ComboBox::from_id_salt(salt)
        .selected_text(names[*at].as_str())
        .show_index(ui, at, names.len(), |i| names[i].clone())
        .labelled_by(label);
}

/// The heatmap: one cell per pair of values, its colour the cell's Sharpe from the lowest to the highest
/// (`vike_ui_theme::heat::ramp`), its text that Sharpe, the trophy on the best point.
fn heatmap_ui(
    ui: &mut egui::Ui,
    sweep: &ParamscanSummaryWire,
    x: &str,
    y: &str,
    shown: Option<usize>,
) -> Option<usize> {
    let tk = ThemeTokens::of(ui.ctx());
    let (xs, ys, cells) = heatmap_cells(sweep, x, y);
    // The Sharpe range the colours span, seeded by a real Sharpe: none when no point ranks, and then
    // no cell is filled.
    let sharpes: Vec<f64> = sweep.points.iter().filter_map(ranked_sharpe).collect();
    let low = sharpes.iter().copied().reduce(f64::min);
    let high = sharpes.iter().copied().reduce(f64::max);
    let cell = egui::vec2(tk.metrics.control_height * 2.0, tk.metrics.control_height);
    let mut open = None;
    ui.label(egui::RichText::new(format!("Sharpe by {x} (across) and {y} (down)")).weak());
    egui::ScrollArea::horizontal().id_salt("studio-param-scan-heatmap").show(ui, |ui| {
        egui::Grid::new("studio-param-scan-heatmap-grid").show(ui, |ui| {
            ui.label("");
            for xv in &xs {
                ui.label(egui::RichText::new(value_text(xv)).weak());
            }
            ui.end_row();
            for (yv, row) in ys.iter().zip(&cells) {
                ui.label(egui::RichText::new(value_text(yv)).weak());
                for &at in row {
                    let Some(i) = at else {
                        ui.label("");
                        continue;
                    };
                    let point = &sweep.points[i];
                    let words = match (ranked_sharpe(point), &point.invalid) {
                        (_, Some(_)) => INVALID_LABEL.to_string(),
                        (Some(s), None) => format!("{s:.2}"),
                        (None, None) => "—".to_string(),
                    };
                    let text: egui::WidgetText = if i == sweep.best_index {
                        icons::BEST.before(ui.style(), egui::RichText::new(words)).into()
                    } else {
                        words.into()
                    };
                    let mut button =
                        egui::Button::selectable(shown == Some(i), text).min_size(cell);
                    if let (Some(s), Some(low), Some(high)) = (ranked_sharpe(point), low, high) {
                        let t = if high > low { (s - low) / (high - low) } else { 0.5 };
                        button = button.fill(vike_ui_theme::heat::ramp(t as f32));
                    }
                    // The heat fill covers the selected look, so the open point is outlined.
                    if shown == Some(i) {
                        let edge = vike_ui_theme::metrics::stroke::EDGE;
                        button = button.stroke(egui::Stroke::new(edge, tk.theme.text));
                    }
                    let why = point.invalid.as_deref().unwrap_or_default();
                    let hover = format!("{}\n{}", point_label(point), why);
                    if ui.add(button).on_hover_text(hover.trim_end()).clicked() {
                        open = Some(i);
                    }
                }
                ui.end_row();
            }
        });
    });
    open
}

/// The table of every point: its scanned parameters, then Sharpe, total return (two decimals), final
/// equity and trades. A header sorts by its column (again to reverse); any cell opens its point.
fn table_ui(
    ui: &mut egui::Ui,
    sweep: &ParamscanSummaryWire,
    names: &[String],
    shown: Option<usize>,
    sort: &mut Option<(usize, bool)>,
) -> Option<usize> {
    let mut open = None;
    let headers: Vec<&str> =
        names.iter().map(String::as_str).chain(METRIC_COLUMNS.iter().copied()).collect();
    let rows = sorted_rows(sweep, names, *sort);
    // Its own height even at the foot of the panel's scroll: sized by the room left on screen, it showed
    // a row and a half there (egui's 64 px floor).
    egui::ScrollArea::both()
        .id_salt("studio-param-scan-table")
        .max_height(studio::COMPARE_SCROLL_MAX_HEIGHT)
        .min_scrolled_height(studio::COMPARE_SCROLL_MAX_HEIGHT)
        .show(ui, |ui| {
            egui::Grid::new("studio-param-scan-table-grid").striped(true).show(ui, |ui| {
                for (column, header) in headers.iter().enumerate() {
                    let (on, falling) = match *sort {
                        Some((c, falling)) if c == column => (true, falling),
                        _ => (false, false),
                    };
                    let text: egui::WidgetText = match (on, falling) {
                        (true, true) => icons::SORT_DESCENDING.before(ui.style(), *header).into(),
                        (true, false) => icons::SORT_ASCENDING.before(ui.style(), *header).into(),
                        (false, _) => egui::RichText::new(*header).strong().into(),
                    };
                    let clicked = ui
                        .selectable_label(on, text)
                        .on_hover_text("Sort by this column; again to reverse")
                        .clicked();
                    if clicked {
                        let first = falls_first(names, column);
                        *sort = Some((column, if on { !falling } else { first }));
                    }
                }
                ui.end_row();
                for i in rows {
                    let point = &sweep.points[i];
                    let [sharpe, _, equity] = crate::panes::results::scan_cells(point);
                    let total_return = match (point.invalid.is_some(), point.total_return) {
                        (true, _) => INVALID_LABEL.to_string(),
                        (false, Some(r)) => crate::panes::results::percent(r),
                        (false, None) => "—".to_string(),
                    };
                    let mut cells: Vec<String> = names
                        .iter()
                        .map(|n| override_of(point, n).map(value_text).unwrap_or_default())
                        .collect();
                    let drawdown = drawdown_cell(point);
                    let trades = point.n_trades.to_string();
                    cells.extend([sharpe, total_return, drawdown, equity, trades]);
                    for (column, words) in cells.into_iter().enumerate() {
                        let text: egui::WidgetText = if column == 0 && i == sweep.best_index {
                            icons::BEST.before(ui.style(), egui::RichText::new(words)).into()
                        } else {
                            words.into()
                        };
                        let cell = ui.selectable_label(shown == Some(i), text);
                        let cell = match &point.invalid {
                            Some(why) => cell.on_hover_text(why.as_str()),
                            None => cell,
                        };
                        if cell.clicked() {
                            open = Some(i);
                        }
                    }
                    ui.end_row();
                }
            });
        });
    open
}

#[cfg(test)]
mod tests;
