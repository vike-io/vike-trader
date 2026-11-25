//! The Account window's History tab: the account's EXECUTION HISTORY and the performance summary over
//! it, read from the DATAHUB's store — Fills · Order history · Summary (the owner's ruling of
//! 2026-10-11). It was a window of its own, the Execution-history ("Tearsheet") window, until it became
//! this tab (top layers, wave 3); its body is unchanged.
//!
//! ⚠ It read a LOCAL command journal until then (`config.journal_dir` of this PC's database), which
//! a thin client never has: the journal is written on the trade hub's box, so the window showed
//! "Journaling is off" on every desktop. That path is DELETED; the rows come from
//! the datahub's store ([`page`]'s types, read by vike-app-core) and the Summary folds ONE account's loaded fills through
//! `vike_analytics::trading_results_table_from_fills` (the rows converted by `vike_data::ExecFillRow::fill_events`).
//!
//! One source per fact: what is working NOW, and its ✕, is the Account window's (the trade hub's
//! read model); what WAS executed is this tab's. Nothing here reads the snapshot's orders except
//! to COUNT the terminal ones (the one nudge: a newly terminal order re-reads the newest page a few
//! seconds later, because the trade hub streams its executions to the datahub off the fold, and the
//! datahub stores them in windows), and nothing here writes —
//! there is no write control in this tab. Status words are the Account window's own
//! (`OrderStatus::as_str`, judged terminal by `!OrderStatus::is_live`, as that window judges them).
//!
//! The body is [`history_body`], which takes its inputs as a value and RETURNS the load it wants,
//! so the CI layout suite drives it without a socket; the Account window's body
//! ([`crate::account_window_body`]) draws it on its History tab and returns that request, and
//! vike-app-core's `account_tool_content` hands it to `crates/vike-app-core/src/data/execution_history.rs`'s
//! `spawn_load` over the shell's dial, which leaves the page in [`AccountWindowHistoryView::slot`]. This crate
//! opens no connection. The window was vike-app-core's `ui/windows/tearsheet.rs`, its load `data/exec_history.rs`,
//! until both moved into this crate (top layers, wave 2); the load went back to vike-app-core on 2026-11-22.

use std::time::{Duration, Instant};

use page::{
    ExecutionHistoryFailure, ExecutionHistoryLoad, ExecutionHistoryPage, ExecutionHistoryQuery,
    ExecutionHistorySlot, ExecutionHistoryWindow, HISTORY_MAX_ROWS,
};
use vike_ui_theme::components::segmented::{self, ThemeSegment};
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;

mod fills;
mod orders;
pub mod page;
mod summary;

/// Empty state 1: no datahub to read from.
pub const NO_DATAHUB: &str = "No datahub configured. Execution history is read from the datahub's \
     store (config.datahub_addr, or the address the backend advertises).";
/// Empty state 2's lead-in; the reason follows verbatim.
pub const READ_FAILED: &str = "Could not read execution history: ";
/// State 3: the datahub predates the execution-history reads.
pub const DATAHUB_TOO_OLD: &str = "This datahub predates the execution-history read; showing fills \
     only, whole series. Upgrade vike-datahub for orders and time windows.";
/// State 4, with no rows: the datahub is known to record no live executions (it does not advertise
/// `vike_datahub_client::proto::FEATURE_EXECUTIONS_RECORDED`).
pub const NOT_RECORDING_EMPTY: &str = "This datahub records no live executions (an older build, or \
     its store refuses writes), so no fill or order of the trade hub reaches its store.";
/// State 4, with rows: the banner above them.
pub const NOT_RECORDING_BANNER: &str = "This datahub records no live executions (an older build, or \
     its store refuses writes): the rows shown are older or imported.";
/// The standing caption under every table.
pub const LAG_CAPTION: &str = "History is recorded from the executions the trade hub streams to the \
     datahub and can lag the live Account window by a few seconds.";
/// The note over a venue with several accounts when some rows carry none.
pub const ACCOUNT_NOTE: &str = "Some rows were written before the store recorded accounts, and this \
     venue has several: those rows show no account and cannot be told apart.";
/// While the first load is in flight.
pub const LOADING: &str = "Reading execution history…";

/// The step a load is on before its catalog said how many series there are to read. Said from the
/// first frame: on the owner's link the catalog was 3.0 of a 3.8 s load (the CI box, 2026-11-17), and a
/// line that appeared only after it read as a stalled window.
pub const CATALOG_STEP: &str = "reading the catalog";

/// How far the load in flight has read: the catalog, then `read of total series`.
fn progress_words(progress: Option<(usize, usize)>) -> String {
    match progress {
        Some((read, total)) => format!("{read} of {total} series"),
        None => CATALOG_STEP.to_string(),
    }
}

/// [`LOADING`] with [`progress_words`].
fn loading_words(progress: Option<(usize, usize)>) -> String {
    format!("{LOADING} {}", progress_words(progress))
}

/// How long after a live order turns terminal the newest page is read again — the datahub stores
/// the trade hub's streamed executions every few seconds, off the fold.
const NUDGE_DELAY: Duration = Duration::from_secs(6);

/// Empty state 5: the window holds nothing.
pub(crate) fn nothing_in_range(from: &str, to: &str) -> String {
    format!("No executions between {from} and {to}.")
}

/// The sentence said when the window reaches [`HISTORY_MAX_ROWS`].
pub(crate) fn capped_note() -> String {
    format!("Showing the newest {HISTORY_MAX_ROWS} rows; narrow the range.")
}

/// Which tab the window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AccountWindowHistoryTab {
    #[default]
    Fills,
    Orders,
    Summary,
}

/// The History tab's own state: its tab, the filters, what is on screen and the load in
/// flight. Lives in [`crate::AccountWindowView::history`] (one per window).
#[derive(Clone)]
pub struct AccountWindowHistoryView {
    pub tab: AccountWindowHistoryTab,
    /// `None` = every venue.
    pub venue: Option<String>,
    /// `None` = every symbol of the chosen venue.
    pub symbol: Option<String>,
    pub window: ExecutionHistoryWindow,
    /// The Summary's starting capital.
    pub seed: f64,
    /// The Summary's account, `(venue, account label)`; `None` (or an account the loaded fills no longer
    /// name) is the first account they name.
    pub summary_account: Option<(String, String)>,
    /// The order whose lifecycle is expanded under the Orders table.
    pub expanded: Option<String>,
    shown: Option<ExecutionHistoryLoad>,
    slot: ExecutionHistorySlot,
    loading: bool,
    asked_once: bool,
    generation: u64,
    terminal_seen: Option<usize>,
    nudge_at: Option<Instant>,
    summary: Option<summary::AccountWindowHistorySummaryCache>,
    /// The Fills tab's rows ([`fills::shown_fills`]), kept with the `(generation, stored rows)` they
    /// were folded from.
    fills_shown: Option<((u64, usize), Vec<vike_data::ExecFillRow>)>,
}

impl Default for AccountWindowHistoryView {
    fn default() -> Self {
        Self {
            tab: AccountWindowHistoryTab::default(),
            venue: None,
            symbol: None,
            window: ExecutionHistoryWindow::default(),
            seed: vike_analytics::results::DEFAULT_REPORT_SEED,
            summary_account: None,
            expanded: None,
            shown: None,
            slot: ExecutionHistorySlot::default(),
            loading: false,
            asked_once: false,
            generation: 0,
            terminal_seen: None,
            nudge_at: None,
            summary: None,
            fills_shown: None,
        }
    }
}

impl AccountWindowHistoryView {
    /// A view already SHOWING `load`, asking for nothing on its first frame — the layout suite's
    /// and the QA capture's way in, since neither runs a datahub.
    pub fn showing(load: ExecutionHistoryLoad) -> Self {
        Self {
            generation: load.query.generation,
            shown: Some(load),
            asked_once: true,
            ..Self::default()
        }
    }

    /// Where this window's background loads land.
    pub fn slot(&self) -> ExecutionHistorySlot {
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
            && matches!(self.shown, Some(ExecutionHistoryLoad { outcome: Ok(_), .. }));
        match load.outcome {
            Ok(next) if onto_shown => {
                if let Some(ExecutionHistoryLoad { outcome: Ok(page), .. }) = self.shown.as_mut() {
                    page.extend_older(next);
                }
            }
            // a fresh page, or an "Older" read that FAILED: what it says replaces what was shown
            outcome => self.shown = Some(ExecutionHistoryLoad { query: load.query, outcome }),
        }
    }

    fn page(&self) -> Option<&ExecutionHistoryPage> {
        self.shown.as_ref().and_then(|l| l.outcome.as_ref().ok())
    }
}

/// What [`history_body`] reads besides its own state.
pub struct AccountWindowHistoryInputs<'a> {
    /// The RESOLVED datahub address; `None` → empty state 1 and no load is ever asked for.
    pub datahub: Option<&'a str>,
    pub display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
    /// The node's account directory — only to say when a venue has several accounts.
    pub directory: Option<&'a vike_tradehub_client::wire::AccountDirectoryWire>,
    /// How many of the live snapshot's orders are terminal — the nudge's only input.
    pub terminal_orders: usize,
    /// `(venue, currency)` of every engine of the live snapshot whose ledger is not the dollar's
    /// ([`vike_tradehub_client::fold_guards::off_dollar_ledgers`]); EMPTY on a node of one currency. The Summary leaves those venues'
    /// fills out, because a stored fill carries no currency and the fold reads every fill as dollars.
    pub off_dollar_ledgers: &'a [(String, String)],
    /// `(venue, symbol, value)` of every contract the live snapshot values off the default
    /// ([`vike_tradehub_client::fold_guards::off_default_contracts`]); EMPTY on a node of linear unit contracts. The Summary leaves those
    /// fills out, because a stored fill carries no contract value and the fold reads every fill as linear.
    pub off_default_contracts: &'a [(String, String, vike_model::ContractValue)],
    /// Now, epoch-ms: where a requested window ends.
    pub now_ms: i64,
}

/// The window's whole body. Returns the load it wants started, or `None`.
pub fn history_body(
    ui: &mut egui::Ui,
    view: &mut AccountWindowHistoryView,
    inputs: &AccountWindowHistoryInputs<'_>,
) -> Option<ExecutionHistoryQuery> {
    use egui::RichText;
    let t = ThemeTokens::of(ui.ctx());
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
                .size(role_px(ui.ctx(), ThemeTextRole::Strong))
                .color(t.theme.text)
                .strong(),
        );
        segmented::segmented(
            ui,
            &mut view.tab,
            &[
                ThemeSegment {
                    value: AccountWindowHistoryTab::Fills,
                    label: "Fills",
                    why: "Every fill, newest first",
                },
                ThemeSegment {
                    value: AccountWindowHistoryTab::Orders,
                    // Not "Orders": the Account window's own Orders tab is what is working NOW.
                    label: "Order history",
                    why: "One row per order: its last state; click a row for its lifecycle",
                },
                ThemeSegment {
                    value: AccountWindowHistoryTab::Summary,
                    label: "Summary",
                    why: "One account's performance summary over its fills loaded here",
                },
            ],
        );
    });
    ui.add_space(space::SM);

    if inputs.datahub.is_none() {
        state::view(ui, ThemeEmptyPane::Unreachable(NO_DATAHUB));
        return None;
    }

    // Filters: venue and symbol from the CATALOG (never a list in code), the time window, Refresh
    // and Older.
    let venues = view.page().map(|p| p.venues.clone()).unwrap_or_default();
    let can_older = view
        .page()
        .is_some_and(|p| p.more && p.rows() < HISTORY_MAX_ROWS && p.oldest_timestamp.is_some());
    ui.horizontal_wrapped(|ui| {
        let before = (view.venue.clone(), view.symbol.clone());
        // No visible label sits beside either picker, so each is named in the tree alone.
        egui::ComboBox::from_id_salt("history_venue")
            .selected_text(view.venue.as_deref().unwrap_or("All venues"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut view.venue, None, "All venues");
                for v in venues.keys() {
                    ui.selectable_value(&mut view.venue, Some(v.clone()), v.as_str());
                }
            })
            .response
            .accessible_name("Venue");
        if view.venue != before.0 {
            view.symbol = None;
        }
        let symbols: Vec<String> = match &view.venue {
            Some(v) => venues.get(v).map(|s| s.iter().cloned().collect()).unwrap_or_default(),
            None => venues.values().flatten().cloned().collect(),
        };
        egui::ComboBox::from_id_salt("history_symbol")
            .selected_text(view.symbol.as_deref().unwrap_or("All symbols"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut view.symbol, None, "All symbols");
                for s in &symbols {
                    ui.selectable_value(&mut view.symbol, Some(s.clone()), s.as_str());
                }
            })
            .response
            .accessible_name("Symbol");
        let windows: Vec<ThemeSegment<'_, ExecutionHistoryWindow>> = ExecutionHistoryWindow::ALL
            .iter()
            .map(|w| ThemeSegment {
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
            ui.label(progress_words(view.slot.progress(view.generation)));
        }
    });
    ui.add_space(space::SM);

    match view.shown.as_ref().map(|l| &l.outcome) {
        None => {
            state::view(
                ui,
                ThemeEmptyPane::Loading(&loading_words(view.slot.progress(view.generation))),
            );
        }
        Some(Err(ExecutionHistoryFailure::NoDatahub)) => {
            state::view(ui, ThemeEmptyPane::Unreachable(NO_DATAHUB));
        }
        Some(Err(ExecutionHistoryFailure::Unreadable(why))) => {
            state::view(ui, ThemeEmptyPane::Unreachable(&format!("{READ_FAILED}{why}")));
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
        RichText::new(LAG_CAPTION)
            .size(role_px(ui.ctx(), ThemeTextRole::Caption))
            .color(t.theme.text_tertiary),
    );

    let query = |view: &mut AccountWindowHistoryView, older_than: Option<i64>| {
        view.generation += 1;
        view.loading = true;
        ExecutionHistoryQuery {
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
        let oldest = view.page().and_then(|p| p.oldest_timestamp);
        return Some(query(view, oldest));
    }
    None
}

/// The body under the filters once a page is on screen: the notes, then the tab.
fn shown_body(
    ui: &mut egui::Ui,
    view: &mut AccountWindowHistoryView,
    load: &ExecutionHistoryLoad,
    inputs: &AccountWindowHistoryInputs<'_>,
    t: &ThemeTokens,
) {
    use egui::RichText;
    let Ok(page) = &load.outcome else { return };
    let warn = |ui: &mut egui::Ui, text: &str| {
        ui.label(
            icons::WARNING.before(
                ui.style(),
                RichText::new(text)
                    .size(role_px(ui.ctx(), ThemeTextRole::Body))
                    .color(ThemeStatus::Warning.color()),
            ),
        );
    };
    if page.served.predates_history() {
        warn(ui, DATAHUB_TOO_OLD);
    }
    let has_rows = !page.fills.is_empty() || !page.orders.is_empty();
    if page.served.not_recording() && has_rows {
        warn(ui, NOT_RECORDING_BANNER);
    }
    if account_note_due(page, inputs.directory) {
        ui.label(
            RichText::new(ACCOUNT_NOTE)
                .size(role_px(ui.ctx(), ThemeTextRole::Caption))
                .color(t.theme.text_secondary),
        );
    }
    if page.rows() >= HISTORY_MAX_ROWS {
        ui.label(
            RichText::new(capped_note())
                .size(role_px(ui.ctx(), ThemeTextRole::Caption))
                .color(t.theme.text_secondary),
        );
    }
    let (lower, upper) = load.query.bounds();
    let empty = || {
        if page.served.not_recording() {
            NOT_RECORDING_EMPTY.to_string()
        } else {
            let fmt = |timestamp: Option<i64>| match timestamp {
                Some(timestamp) => fmt_timestamp(timestamp, inputs.display_time_zone),
                None => "the first execution".to_string(),
            };
            nothing_in_range(&fmt(lower), &fmt(Some(upper)))
        }
    };
    match view.tab {
        AccountWindowHistoryTab::Fills if page.fills.is_empty() => {
            state::view(ui, ThemeEmptyPane::Empty(&empty()));
        }
        AccountWindowHistoryTab::Fills => {
            let key = (load.query.generation, page.fills.len());
            if view.fills_shown.as_ref().is_none_or(|(k, _)| *k != key) {
                view.fills_shown = Some((key, fills::shown_fills(&page.fills)));
            }
            if let Some((_, rows)) = &view.fills_shown {
                fills::fills_table(ui, rows, inputs.display_time_zone);
            }
        }
        AccountWindowHistoryTab::Orders if page.served.predates_history() => {
            state::view(ui, ThemeEmptyPane::Empty(DATAHUB_TOO_OLD));
        }
        AccountWindowHistoryTab::Orders if page.orders.is_empty() => {
            state::view(ui, ThemeEmptyPane::Empty(&empty()));
        }
        AccountWindowHistoryTab::Orders => {
            orders::orders_table(ui, page, &mut view.expanded, inputs.display_time_zone);
        }
        AccountWindowHistoryTab::Summary if page.fills.is_empty() => {
            state::view(ui, ThemeEmptyPane::Empty(&empty()));
        }
        AccountWindowHistoryTab::Summary => summary::summary(ui, view, load, page, inputs, t),
    }
}

/// Whether a venue on the page has several accounts in the directory AND some of its rows carry no
/// account — the only case where rows cannot be told apart.
fn account_note_due(
    page: &ExecutionHistoryPage,
    directory: Option<&vike_tradehub_client::wire::AccountDirectoryWire>,
) -> bool {
    let Some(dir) = directory else { return false };
    let several = |venue: &str| dir.accounts.iter().filter(|a| a.venue == venue).count() > 1;
    page.fills.iter().any(|f| f.account.is_none() && several(&f.venue))
        || page.orders.iter().any(|o| o.account.is_none() && several(&o.venue))
}

/// An epoch-ms instant in the display timezone, to the second.
fn fmt_timestamp(
    timestamp: i64,
    time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
) -> String {
    vike_ui_theme::time_zone::to_naive(timestamp, time_zone)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}
