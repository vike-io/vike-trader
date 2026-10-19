//! The Tearsheet window: the account's EXECUTION HISTORY and the performance summary over it, read
//! from the DATAHUB's store — tabs Fills · Orders · Summary (the owner's ruling of 2026-10-11).
//!
//! ⚠ It read a LOCAL command journal until then (`config.journal_dir` of this PC's database), which
//! a thin client never has: the journal is written on the trade hub's box, so the window showed
//! "Journaling is off" on every desktop. That path is DELETED; the rows come from
//! `crate::data::exec_history`'s load and the Summary folds the loaded fills through
//! `vike_analytics::tearsheet_from_fills` (each row converted by `vike_data::ExecFillRow::to_fill_event`).
//!
//! One source per fact: what is working NOW, and its ✕, is the Account window's (the trade hub's
//! read model); what WAS executed is this window's. Nothing here reads the snapshot's orders except
//! to COUNT the terminal ones (the one nudge: a newly terminal order re-reads the newest page a few
//! seconds later, because the materializer runs off the fold and lags), and nothing here writes —
//! there is no write control in this window. Status words are the Account window's own
//! (`OrderStatus::as_str`, judged terminal by `!OrderStatus::is_live`, as that window judges them).
//!
//! The body is [`history_body`], which takes its inputs as a value and RETURNS the load it wants,
//! so the CI layout suite drives it without a socket; [`tearsheet_tool_content`] is the window's
//! entry, which hands that request to `crate::data::exec_history::spawn_load` over the shell's dial.

use std::time::{Duration, Instant};

use super::ToolCtx;
use crate::data::exec_history::{
    self, ExecHistoryLoad, ExecHistorySlot, HISTORY_MAX_ROWS, HistoryFailure, HistoryPage,
    HistoryQuery, HistoryWindow,
};
use crate::tools;
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::{Status, Tokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::side::Pair;
use vike_ui_theme::type_scale::TextRole;

mod fills;
mod orders;

/// Empty state 1: no datahub to read from.
pub const NO_DATAHUB: &str = "No datahub configured. Execution history is read from the datahub's \
     store (config.datahub_addr, or the address the backend advertises).";
/// Empty state 2's lead-in; the reason follows verbatim.
pub const READ_FAILED: &str = "Could not read execution history: ";
/// State 3: the datahub predates the execution-history reads.
pub const DATAHUB_TOO_OLD: &str = "This datahub predates the execution-history read; showing fills \
     only, whole series. Upgrade vike-datahub for orders and time windows.";
/// State 4, with no rows: the materializer is known to be off.
pub const MATERIALIZER_OFF_EMPTY: &str = "Trade-history recording is OFF on this datahub, so live \
     executions are not written to the store. Turn it on with the flags.journal_materializer row \
     on the datahub box.";
/// State 4, with rows: the banner above them.
pub const MATERIALIZER_OFF_BANNER: &str = "Trade-history recording is OFF — new fills are not \
     being recorded; rows shown are imported or older.";
/// The standing caption under every table.
pub const LAG_CAPTION: &str = "History is recorded from the trade daemon's journal and can lag the \
     live Account window by a few seconds.";
/// The note over a venue with several accounts when some rows carry none.
pub const ACCOUNT_NOTE: &str = "Some rows were written before the store recorded accounts, and this \
     venue has several: those rows show no account and cannot be told apart.";
/// While the first load is in flight.
pub const LOADING: &str = "Reading execution history…";

/// How long after a live order turns terminal the newest page is read again — the materializer
/// drains the journal every few seconds, off the fold.
const NUDGE_DELAY: Duration = Duration::from_secs(6);

/// Empty state 5: the window holds nothing.
pub fn nothing_in_range(from: &str, to: &str) -> String {
    format!("No executions between {from} and {to}.")
}

/// The sentence said when the window reaches [`HISTORY_MAX_ROWS`].
pub fn capped_note() -> String {
    format!("Showing the newest {HISTORY_MAX_ROWS} rows; narrow the range.")
}

/// Which tab the window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HistoryTab {
    #[default]
    Fills,
    Orders,
    Summary,
}

/// The Tearsheet window's own state: the tab, the filters, what is on screen and the load in
/// flight. Lives on `tools::ToolView` (one per window).
#[derive(Clone)]
pub struct HistoryView {
    pub tab: HistoryTab,
    /// `None` = every venue.
    pub venue: Option<String>,
    /// `None` = every symbol of the chosen venue.
    pub symbol: Option<String>,
    pub window: HistoryWindow,
    /// The Summary's starting capital.
    pub seed: f64,
    /// The order whose lifecycle is expanded under the Orders table.
    pub expanded: Option<String>,
    shown: Option<ExecHistoryLoad>,
    slot: ExecHistorySlot,
    loading: bool,
    asked_once: bool,
    generation: u64,
    terminal_seen: Option<usize>,
    nudge_at: Option<Instant>,
    summary: Option<SummaryCache>,
}

/// The Summary rows, kept until the fills or the seed under them change.
#[derive(Clone)]
struct SummaryCache {
    generation: u64,
    fills: usize,
    seed: f64,
    rows: Vec<(String, String)>,
}

impl Default for HistoryView {
    fn default() -> Self {
        Self {
            tab: HistoryTab::default(),
            venue: None,
            symbol: None,
            window: HistoryWindow::default(),
            seed: 10_000.0,
            expanded: None,
            shown: None,
            slot: ExecHistorySlot::default(),
            loading: false,
            asked_once: false,
            generation: 0,
            terminal_seen: None,
            nudge_at: None,
            summary: None,
        }
    }
}

impl HistoryView {
    /// A view already SHOWING `load`, asking for nothing on its first frame — the layout suite's
    /// and the QA capture's way in, since neither runs a datahub.
    pub fn showing(load: ExecHistoryLoad) -> Self {
        Self {
            generation: load.query.generation,
            shown: Some(load),
            asked_once: true,
            ..Self::default()
        }
    }

    /// Where this window's background loads land.
    pub fn slot(&self) -> ExecHistorySlot {
        self.slot.clone()
    }

    /// Take a landed load if it answers the LATEST request (an older one is dropped: a filter
    /// changed while it was in flight). An "Older" page is appended below what is shown.
    fn adopt(&mut self) {
        let Some(load) = self.slot.take() else { return };
        if load.query.generation != self.generation {
            return;
        }
        self.loading = false;
        let onto_shown = load.query.older_than.is_some()
            && matches!(self.shown, Some(ExecHistoryLoad { outcome: Ok(_), .. }));
        match load.outcome {
            Ok(next) if onto_shown => {
                if let Some(ExecHistoryLoad { outcome: Ok(page), .. }) = self.shown.as_mut() {
                    page.extend_older(next);
                }
            }
            // a fresh page, or an "Older" read that FAILED: what it says replaces what was shown
            outcome => self.shown = Some(ExecHistoryLoad { query: load.query, outcome }),
        }
    }

    fn page(&self) -> Option<&HistoryPage> {
        self.shown.as_ref().and_then(|l| l.outcome.as_ref().ok())
    }
}

/// What [`history_body`] reads besides its own state.
pub struct HistoryInputs<'a> {
    /// The RESOLVED datahub address; `None` → empty state 1 and no load is ever asked for.
    pub datahub: Option<&'a str>,
    pub display_tz: vike_chart::DisplayTz,
    /// The node's account directory — only to say when a venue has several accounts.
    pub directory: Option<&'a vike_tradehub_client::wire::WireDirectory>,
    /// How many of the live snapshot's orders are terminal — the nudge's only input.
    pub terminal_orders: usize,
    /// Now, epoch-ms: where a requested window ends.
    pub now_ms: i64,
}

/// The read-only inputs the window takes from `ToolCtx`: where the datahub is, and the SHELL's dial
/// (`fn(addr, key_name)`), because resolving the observe key needs the binary's settings directory
/// and environment, which this crate does not read.
#[derive(Clone, Copy)]
pub struct HistoryCtx<'a> {
    pub datahub_addr: Option<&'a str>,
    pub key_name: &'a str,
    pub dial: fn(String, &str) -> vike_datahub_client::RemoteHistStore,
}

impl<'a> HistoryCtx<'a> {
    pub fn new(
        datahub_addr: Option<&'a str>,
        key_name: &'a str,
        dial: fn(String, &str) -> vike_datahub_client::RemoteHistStore,
    ) -> Self {
        Self { datahub_addr, key_name, dial }
    }

    /// No datahub: the window says so and dials nothing.
    pub fn none() -> HistoryCtx<'static> {
        HistoryCtx { datahub_addr: None, key_name: "", dial: dial_without_keys }
    }
}

fn dial_without_keys(addr: String, _key_name: &str) -> vike_datahub_client::RemoteHistStore {
    vike_datahub_client::RemoteHistStore::new(addr)
}

/// The Tearsheet window: [`history_body`] over this window's [`HistoryView`], and the load it asks
/// for handed to a background thread over the shell's dial.
pub fn tearsheet_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    let inputs = HistoryInputs {
        datahub: ctx.history.datahub_addr,
        display_tz: ctx.display_tz,
        directory: ctx.directory,
        terminal_orders: ctx.snap.orders.iter().filter(|o| !o.status.is_live()).count(),
        now_ms: vike_model::now_ms(),
    };
    if let Some(query) = history_body(ui, &mut tv.hist, &inputs) {
        let (addr, key, dial) = (
            ctx.history.datahub_addr.map(str::to_string),
            ctx.history.key_name.to_string(),
            ctx.history.dial,
        );
        let wake = ui.ctx().clone();
        exec_history::spawn_load(
            query,
            tv.hist.slot(),
            move || addr.map(|a| dial(a, &key)),
            move || wake.request_repaint(),
        );
    }
}

/// The window's whole body. Returns the load it wants started, or `None`.
pub fn history_body(
    ui: &mut egui::Ui,
    view: &mut HistoryView,
    inputs: &HistoryInputs<'_>,
) -> Option<HistoryQuery> {
    use egui::RichText;
    let t = Tokens::of(ui.ctx());
    view.adopt();
    let mut fresh = !view.asked_once;
    let mut older = false;
    view.asked_once = true;

    // The one nudge: a live order turned terminal → the newest page again, a little later.
    if let Some(seen) = view.terminal_seen
        && inputs.terminal_orders > seen
    {
        view.nudge_at = Some(Instant::now() + NUDGE_DELAY);
    }
    view.terminal_seen = Some(inputs.terminal_orders);
    if let Some(at) = view.nudge_at {
        if Instant::now() >= at && !view.loading {
            view.nudge_at = None;
            fresh = true;
        } else {
            ui.ctx().request_repaint_after(at.saturating_duration_since(Instant::now()));
        }
    }

    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Execution history")
                .size(role_px(ui.ctx(), TextRole::Strong))
                .color(t.theme.text)
                .strong(),
        );
        segmented::segmented(
            ui,
            &mut view.tab,
            &[
                Segment {
                    value: HistoryTab::Fills,
                    label: "Fills",
                    why: "Every fill, newest first",
                },
                Segment {
                    value: HistoryTab::Orders,
                    label: "Orders",
                    why: "One row per order: its last state; click a row for its lifecycle",
                },
                Segment {
                    value: HistoryTab::Summary,
                    label: "Summary",
                    why: "The performance summary over the fills loaded here",
                },
            ],
        );
    });
    ui.add_space(space::SM);

    if inputs.datahub.is_none() {
        state::view(ui, Load::Unreachable(NO_DATAHUB));
        return None;
    }

    // Filters: venue and symbol from the CATALOG (never a list in code), the time window, Refresh
    // and Older.
    let venues = view.page().map(|p| p.venues.clone()).unwrap_or_default();
    let can_older =
        view.page().is_some_and(|p| p.more && p.rows() < HISTORY_MAX_ROWS && p.oldest_ts.is_some());
    ui.horizontal_wrapped(|ui| {
        let before = (view.venue.clone(), view.symbol.clone());
        egui::ComboBox::from_id_salt("hist_venue")
            .selected_text(view.venue.as_deref().unwrap_or("All venues"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut view.venue, None, "All venues");
                for v in venues.keys() {
                    ui.selectable_value(&mut view.venue, Some(v.clone()), v.as_str());
                }
            });
        if view.venue != before.0 {
            view.symbol = None;
        }
        let symbols: Vec<String> = match &view.venue {
            Some(v) => venues.get(v).map(|s| s.iter().cloned().collect()).unwrap_or_default(),
            None => venues.values().flatten().cloned().collect(),
        };
        egui::ComboBox::from_id_salt("hist_symbol")
            .selected_text(view.symbol.as_deref().unwrap_or("All symbols"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut view.symbol, None, "All symbols");
                for s in &symbols {
                    ui.selectable_value(&mut view.symbol, Some(s.clone()), s.as_str());
                }
            });
        let windows: Vec<Segment<'_, HistoryWindow>> = HistoryWindow::ALL
            .iter()
            .map(|w| Segment {
                value: *w,
                label: w.label(),
                why: "How far back the history reaches",
            })
            .collect();
        if segmented::segmented(ui, &mut view.window, &windows) {
            fresh = true;
        }
        if (view.venue.clone(), view.symbol.clone()) != before {
            fresh = true;
        }
        if ui.add_enabled(!view.loading, egui::Button::new((icons::REFRESH, "Refresh"))).clicked() {
            fresh = true;
        }
        if can_older && ui.add_enabled(!view.loading, egui::Button::new("Older")).clicked() {
            older = true;
        }
        if view.loading {
            ui.add(egui::Spinner::new());
        }
    });
    ui.add_space(space::SM);

    match view.shown.as_ref().map(|l| &l.outcome) {
        None => {
            state::view(ui, Load::Loading(LOADING));
        }
        Some(Err(HistoryFailure::NoDatahub)) => {
            state::view(ui, Load::Unreachable(NO_DATAHUB));
        }
        Some(Err(HistoryFailure::Unreadable(why))) => {
            state::view(ui, Load::Unreachable(&format!("{READ_FAILED}{why}")));
        }
        Some(Ok(_)) => {
            // Taken out for the draw, so the body can mutate the view's other fields beside it.
            if let Some(load) = view.shown.take() {
                shown_body(ui, view, &load, inputs, &t);
                view.shown = Some(load);
            }
        }
    }
    ui.add_space(space::SM);
    ui.label(
        RichText::new(LAG_CAPTION).size(role_px(ui.ctx(), TextRole::Caption)).color(t.theme.text3),
    );

    let query = |view: &mut HistoryView, older_than: Option<i64>| {
        view.generation += 1;
        view.loading = true;
        HistoryQuery {
            venue: view.venue.clone(),
            symbol: view.symbol.clone(),
            window: view.window,
            now_ms: inputs.now_ms,
            older_than,
            generation: view.generation,
        }
    };
    if fresh {
        view.expanded = None;
        return Some(query(view, None));
    }
    if older {
        let oldest = view.page().and_then(|p| p.oldest_ts);
        return Some(query(view, oldest));
    }
    None
}

/// The body under the filters once a page is on screen: the notes, then the tab.
fn shown_body(
    ui: &mut egui::Ui,
    view: &mut HistoryView,
    load: &ExecHistoryLoad,
    inputs: &HistoryInputs<'_>,
    t: &Tokens,
) {
    use egui::RichText;
    let Ok(page) = &load.outcome else { return };
    let warn = |ui: &mut egui::Ui, text: &str| {
        ui.label(
            icons::WARNING.before(
                ui.style(),
                RichText::new(text)
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .color(Status::Warning.color()),
            ),
        );
    };
    if page.served.predates_history() {
        warn(ui, DATAHUB_TOO_OLD);
    }
    let has_rows = !page.fills.is_empty() || !page.orders.is_empty();
    if page.served.materializer_off() && has_rows {
        warn(ui, MATERIALIZER_OFF_BANNER);
    }
    if account_note_due(page, inputs.directory) {
        ui.label(
            RichText::new(ACCOUNT_NOTE)
                .size(role_px(ui.ctx(), TextRole::Caption))
                .color(t.theme.text2),
        );
    }
    if page.rows() >= HISTORY_MAX_ROWS {
        ui.label(
            RichText::new(capped_note())
                .size(role_px(ui.ctx(), TextRole::Caption))
                .color(t.theme.text2),
        );
    }
    let (lower, upper) = load.query.bounds();
    let empty = || {
        if page.served.materializer_off() {
            MATERIALIZER_OFF_EMPTY.to_string()
        } else {
            let fmt = |ts: Option<i64>| match ts {
                Some(ts) => fmt_ts(ts, inputs.display_tz),
                None => "the first execution".to_string(),
            };
            nothing_in_range(&fmt(lower), &fmt(Some(upper)))
        }
    };
    match view.tab {
        HistoryTab::Fills if page.fills.is_empty() => {
            state::view(ui, Load::Empty(&empty()));
        }
        HistoryTab::Fills => fills::fills_table(ui, &page.fills, inputs.display_tz),
        HistoryTab::Orders if page.served.predates_history() => {
            state::view(ui, Load::Empty(DATAHUB_TOO_OLD));
        }
        HistoryTab::Orders if page.orders.is_empty() => {
            state::view(ui, Load::Empty(&empty()));
        }
        HistoryTab::Orders => {
            orders::orders_table(ui, page, &mut view.expanded, inputs.display_tz);
        }
        HistoryTab::Summary if page.fills.is_empty() => {
            state::view(ui, Load::Empty(&empty()));
        }
        HistoryTab::Summary => summary(ui, view, load, page, t),
    }
}

/// Whether a venue on the page has several accounts in the directory AND some of its rows carry no
/// account — the only case where rows cannot be told apart.
fn account_note_due(
    page: &HistoryPage,
    directory: Option<&vike_tradehub_client::wire::WireDirectory>,
) -> bool {
    let Some(dir) = directory else { return false };
    let several = |venue: &str| dir.accounts.iter().filter(|a| a.venue == venue).count() > 1;
    page.fills.iter().any(|f| f.account.is_none() && several(&f.venue))
        || page.orders.iter().any(|o| o.account.is_none() && several(&o.venue))
}

/// The performance summary over the loaded fills: the seed, then every catalog row.
fn summary(
    ui: &mut egui::Ui,
    view: &mut HistoryView,
    load: &ExecHistoryLoad,
    page: &HistoryPage,
    t: &Tokens,
) {
    use egui::RichText;
    let (pos, neg) = Pair::PositiveNegative.texts(t);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Seed").size(role_px(ui.ctx(), TextRole::Caption)).color(t.theme.text3),
        );
        ui.add(egui::DragValue::new(&mut view.seed).speed(100.0).prefix("$"));
        ui.label(
            RichText::new(format!("over the {} fills loaded above", page.fills.len()))
                .size(role_px(ui.ctx(), TextRole::Caption))
                .color(t.theme.text3),
        );
    });
    ui.add_space(space::SM);
    let fresh = view.summary.as_ref().is_some_and(|c| {
        c.generation == load.query.generation && c.fills == page.fills.len() && c.seed == view.seed
    });
    if !fresh {
        let fills = page.fills.iter().filter_map(vike_data::ExecFillRow::to_fill_event).collect();
        let sheet = vike_analytics::tearsheet_from_fills(
            fills,
            vike_analytics::FillOrder::ByTime,
            view.seed,
            252.0,
        )
        .0;
        view.summary = Some(SummaryCache {
            generation: load.query.generation,
            fills: page.fills.len(),
            seed: view.seed,
            rows: tearsheet_rows(&sheet),
        });
    }
    let Some(cache) = &view.summary else { return };
    egui::Grid::new("tearsheet_rows")
        .num_columns(2)
        .spacing([space::XL3, space::SM])
        .striped(true)
        .show(ui, |ui| {
            for (label, value) in &cache.rows {
                ui.label(RichText::new(label).color(t.theme.text2));
                // Sign-colour the value cell on the RENDERED string's first byte: '-' is a loss,
                // '+' a gain, else neutral (see [`tearsheet_rows`] for why only losses colour).
                let col = match value.as_bytes().first() {
                    Some(b'+') => pos,
                    Some(b'-') => neg,
                    _ => t.theme.text,
                };
                ui.label(RichText::new(value).monospace().color(col));
                ui.end_row();
            }
        });
}

/// An epoch-ms instant in the display timezone, to the second.
fn fmt_ts(ts: i64, tz: vike_chart::DisplayTz) -> String {
    vike_chart::to_naive(ts, tz)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ts.to_string())
}

/// Flatten a `LiveTearsheet` into ordered `(label, value)` rows for the grid — **every
/// `vike_analytics::metric_catalog::METRICS` entry, in that table's own declaration order,
/// rendered by that entry's own `MetricUnit`.**
///
/// # It is now an adapter, and that is the whole change
///
/// This function used to be a twenty-row `vec![]` of `(label, format!(…))` pairs against a catalog
/// of thirty-eight, with its own precision per row and its own `* 100.0` on the percent ones; then
/// it became a catalog walk over a local `live_metric_value` accessor, which its own doc called the
/// residual and asked to have DELETED once the accessor moved onto the document type. That move
/// landed — `vike_analytics::LiveTearsheet::rendered_rows` is the one rendering of that document's
/// numbers, shared with the `Display` table and the HTML tearsheet — so the accessor and its pin
/// test are gone and what remains here is the `&'static str` → `String` conversion `ToolView`'s
/// row type needs. Nothing about the roster, the order or the formatting is decided in this file.
///
/// # A row this document does not carry now says so, where it used to VANISH
///
/// The old accessor answered `None` for the thirteen catalog ids the flat struct had no field for,
/// and those rows were OMITTED — defensible then, because `None` was a fact about the TYPE rather
/// than about the session, identical on every run. It is not a fact about the type any more:
/// `LiveTearsheet` carries the whole catalog, so `None` means THIS reconstruction recorded no
/// number, which is exactly the case `vike_analytics::report::BacktestReport::render_metrics`
/// prints `vike_analytics::NOT_RECORDED` for. So the panel prints it too. The rows that were
/// missing because the struct dropped them (`long_ratio`, the two ulcer figures, the four
/// return-distribution figures and the rest) now carry real numbers over the same fills, which is
/// the "honest fix" the deleted accessor's doc named; `funding_paid` keeps saying *not recorded* on
/// the store door too, deliberately, because a fill stream folds no funding cashflow and a `0.00`
/// there would tell an operator of a live PERP account that funding was free.
///
/// `NOT_RECORDED` begins with `n`, so the grid's sign-coloring reads it as neutral rather than as a
/// loss — worth stating because that colouring keys on the rendered string's first byte.
fn tearsheet_rows(s: &vike_analytics::LiveTearsheet) -> Vec<(String, String)> {
    s.rendered_rows().into_iter().map(|(id, text)| (id.to_string(), text)).collect()
}

#[cfg(test)]
mod tests {
    use super::tearsheet_rows;
    use vike_analytics::metric_catalog::METRICS;
    use vike_analytics::{LiveTearsheet, MetricValues, NOT_RECORDED, TEARSHEET_SCHEMA};

    /// A sheet with distinctive values on the rows the formatting assertions read, and NOTHING
    /// recorded elsewhere — so both halves of `rendered_rows` are exercised: a rendered number and
    /// [`NOT_RECORDED`].
    ///
    /// Built through `MetricValues::from_fn` rather than as a struct literal because the document
    /// has no per-metric fields left to name; the ids below are the same strings an operator types
    /// at `--metrics`.
    fn sheet() -> LiveTearsheet {
        const VALUES: &[(&str, f64)] = &[
            ("n_trades", 3.0),
            ("final_equity", 10_500.5),
            ("total_return", 0.1234),
            ("net_profit", 500.5),
            ("gross_profit", 700.0),
            ("gross_loss", -199.5),
            ("total_fees", 1.2345),
            ("win_rate", 0.6667),
            ("profit_factor", f64::INFINITY),
            ("sharpe", 1.5),
            ("sortino", 2.25),
            ("calmar", 0.75),
            ("max_drawdown", -0.25),
            ("cagr", -0.05),
            ("sqn", 1.1),
            ("avg_win", 350.0),
            ("avg_loss", -199.5),
            ("payoff_ratio", 1.75),
            ("expected_payoff", 166.83),
            ("largest_win", 400.0),
            ("largest_loss", -199.5),
            ("consecutive_wins", 2.0),
            ("consecutive_losses", 1.0),
            ("value_at_risk_95", -0.02),
            ("expected_shortfall_95", -0.03),
        ];
        LiveTearsheet {
            schema: TEARSHEET_SCHEMA,
            name: None,
            metrics: MetricValues::from_fn(|id| {
                VALUES.iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
            }),
        }
    }

    /// The grid shows the WHOLE catalog, in the catalog's own declaration order — pinned against
    /// `METRICS` rather than against a literal, because a literal here would be the hand copy this
    /// file has already deleted twice.
    ///
    /// ⚠ This replaces `the_rows_a_live_tearsheet_cannot_answer_are_pinned`, whose whole subject —
    /// a pinned SET of catalog rows the panel silently skipped — no longer exists: nothing is
    /// filtered, so there is no omission to keep honest. That makes this the stronger claim of the
    /// two, and it is also why the old `UNANSWERABLE` list is not carried forward as data.
    #[test]
    fn the_panel_renders_every_catalog_row_in_the_catalogs_own_order() {
        let rows = tearsheet_rows(&sheet());
        let rendered: Vec<&str> = rows.iter().map(|(l, _)| l.as_str()).collect();
        let expected: Vec<&str> = METRICS.iter().map(|m| m.id).collect();
        assert_eq!(rendered, expected);
        // …and it is far WIDER than the twenty-row hand copy that started this, which is the
        // defect that motivated every step of it.
        assert!(rendered.len() > 20, "{} rows", rendered.len());
    }

    /// The formatting is `MetricUnit::render`'s, per the row's OWN unit — so the scaling, the
    /// precision and the non-finite spelling are all decided in one place for this panel, the
    /// CLI, the HTML tearsheet and the stored report alike.
    #[test]
    fn values_are_rendered_by_the_rows_own_unit() {
        let rows = tearsheet_rows(&sheet());
        let get = |id: &str| rows.iter().find(|(l, _)| l == id).map(|(_, v)| v.as_str());
        // Count: no decimals.
        assert_eq!(get("n_trades"), Some("3"));
        assert_eq!(get("consecutive_wins"), Some("2"));
        // Money: two decimals, no scaling.
        assert_eq!(get("final_equity"), Some("10500.50"));
        assert_eq!(get("avg_loss"), Some("-199.50"));
        // Percent: the `* 100.0` a hand-rolled renderer forgets, four decimals.
        assert_eq!(get("total_return"), Some("12.3400%"));
        assert_eq!(get("cagr"), Some("-5.0000%"));
        assert_eq!(get("max_drawdown"), Some("-25.0000%"));
        assert_eq!(get("win_rate"), Some("66.6700%"));
        // Ratio: four decimals, and the house inf sentinel reaches the operator as Rust's own
        // `inf` — the same bytes every other door prints, rather than this panel's old `∞` glyph.
        assert_eq!(get("payoff_ratio"), Some("1.7500"));
        assert_eq!(get("profit_factor"), Some("inf"));
        // A row this reconstruction recorded no number for says so — it is PRESENT and reads
        // `not recorded`, where the deleted accessor made it vanish from the panel entirely.
        assert_eq!(get("omega"), Some(NOT_RECORDED));
    }
}
