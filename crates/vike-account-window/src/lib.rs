//! `vike-account-window` — the desktop's Account window, egui with no eframe/wgpu, so it builds and
//! tests with the derived CI roster like `vike-chart`.
//!
//! The contract: every input is a value from a crate below this one (layer 75) — the read model's
//! `CoreSnapshot` (vike-execution), the node's words and directory (vike-tradehub-client), the history's
//! loaded page (`vike-datahub-client`'s wire rows, landed in the tab's slot) — and every
//! output is a value the caller acts on: the window writes a working order's cancel into a slot, and
//! RETURNS the History read it wants. Nothing here dials a node or a datahub, reads a setting or the
//! environment, or sends an order. vike-app-core keeps the window wiring (`account_tool_content` over its
//! `WindowToolContext` and `WindowView`), and runs the load the History tab asks for
//! (`crates/vike-app-core/src/data/execution_history.rs`'s `spawn_load`).
//!
//! The window (`docs/superpowers/specs/2026-09-30-trade-window-design.md` §3.10) is the old Trade
//! window without its ticket. One column, top to bottom: the equity header, then the tabs
//! Accounts · Positions · Orders · Strategies · History (the owner's order, top layers wave 3), one
//! module each:
//! - [`equity`] — the view-model over the snapshot (`AccountWindowEquityModel`), one currency and one
//!   tier per sum, and [`equity::hero`], the header drawn from it: one group per tier, and the one
//!   cross-venue hero for a node that reports no tier.
//! - [`accounts`] — the Accounts tab: one row per account with its link, the rows of the accounts the
//!   node runs no engine for, and the conversion rates below them.
//! - [`positions`] — the Positions tab's rows over every account's block, VIEW ONLY: it is handed no
//!   OUT slot.
//! - [`orders`] — the Orders tab: the working orders, whose ✕ leaves through the `cancel` slot
//!   [`account_window_body`] writes ([`AccountWindowView::cancel`]), and the emulated ones below them.
//! - `strategies` — the Strategies tab: one row per mount.
//! - [`history`] — the History tab: Fills · Order history · Summary over the datahub's store
//!   ([`history::history_body`]), and the page types it is handed ([`history::page`]); the load the body
//!   RETURNS is vike-app-core's.
//!
//! This root holds what every tab shares: the window's state ([`AccountWindowView`], `&mut`, so the
//! body keeps writing its OUT intent), its tabs ([`AccountWindowTab`]), the read-only inputs as one
//! value ([`AccountWindowInputs`]), the body ([`account_window_body`]) and the aligned-table cells
//! (`text_cell`, `number_cell`, `text_w`). Order ENTRY lives in the Trade window (vike-app-core's
//! `ui/windows/trade_window/mod.rs`, the widget in `vike_order_entry::trade_window`).
//!
//! History: these sections moved verbatim from `vike-app`'s CI-excluded `main.rs` (tool-view
//! extraction batch 3, audit F1) into vike-app-core, and from there into this crate (top layers,
//! wave 2), and the decision helpers the render code used to inline (`equity::hero`'s `hero_totals`,
//! `accounts`'s `unpriced_tooltip`) became named, pure functions with unit tests, the POINT of that
//! move. The Execution-history ("Tearsheet") window moved here with it and became the History tab in
//! wave 3; its window kind is gone.
//!
//! ⚠ Up and down are the chosen market-colour set since the design system's PR 4 — text in the
//! set's text colours, fills in its graphic colours.

#![warn(unreachable_pub)]

pub mod accounts;
pub mod equity;
pub mod history;
pub mod orders;
pub mod positions;
mod strategies;
#[cfg(test)]
mod test_support;

use crate::accounts::{accounts_section, engineless, rates_section};
use crate::equity::hero::equity_header;
use crate::orders::{emulated_section, orders_section};
use crate::strategies::strategies_section;
use vike_tradehub_client::node_words::BackendNodeWords;
use vike_ui_theme::components::role_px;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::account;

/// Right-aligned fixed-width monospace numeric cell — the aligned-table idiom (mirrors the
/// Calendar table's `allocate_ui_with_layout` columns). Used by the Account window's venue +
/// orders tables so numbers line up on the decimal.
pub(crate) fn number_cell(ui: &mut egui::Ui, w: f32, txt: String, col: egui::Color32) {
    use egui::{Align, Label, Layout, RichText, vec2};
    let cell = vec2(w, account::CELL_HEIGHT);
    ui.allocate_ui_with_layout(cell, Layout::right_to_left(Align::Center), |ui| {
        ui.set_min_width(w);
        ui.add(
            Label::new(
                RichText::new(txt)
                    .monospace()
                    .size(role_px(ui.ctx(), ThemeTextRole::Strong))
                    .color(col),
            )
            .truncate(),
        );
    });
}

/// Left-aligned fixed-width text cell (venue-name / side columns of the Account tables).
pub(crate) fn text_cell(ui: &mut egui::Ui, w: f32, rt: egui::RichText) {
    use egui::{Align, Label, Layout, vec2};
    let cell = vec2(w, account::CELL_HEIGHT);
    ui.allocate_ui_with_layout(cell, Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(w);
        ui.add(Label::new(rt).truncate());
    });
}

/// How wide `rt` lays out unwrapped, the way [`text_cell`] and [`number_cell`] draw it, plus a point so a
/// cell sized from it never elides it (the layout rounds glyph edges up). The Account tables' columns
/// are sized from this, never from a guess about the longest word: a fixed cell cut `1006910.45` to
/// `1006910…` and `hyperliquid` to `hyperliq…`.
pub(crate) fn text_w(ui: &egui::Ui, rt: egui::RichText) -> f32 {
    egui::WidgetText::from(rt)
        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body)
        .size()
        .x
        .ceil()
        + 1.0
}

/// What the Account window keeps between frames, one per window (vike-app-core's
/// `WindowView::account`): the tab it shows, the History tab's view, and the one write, the
/// cancel the ✕ on a working order asks for, which the shell drains into the dispatcher.
///
/// All of it survives a tab switch: the History tab's filters, seed, expanded order, page and the
/// load in flight wait in `history` while another tab shows (a load that lands meanwhile is taken
/// on the next History frame).
#[derive(Clone, Default)]
pub struct AccountWindowView {
    /// The tab the window shows.
    pub tab: AccountWindowTab,
    /// The History tab's view: its own tab, filters, the rows on screen and the load in flight.
    pub history: crate::history::AccountWindowHistoryView,
    /// OUT: cancel this `order_client_id`.
    pub cancel: Option<String>,
}

/// The Account window's tabs, under the equity header, left to right in declaration order; one
/// shows at a time, per window ([`AccountWindowView::tab`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccountWindowTab {
    /// The Accounts table, its conversion rates below it.
    #[default]
    Accounts,
    /// Every account's open positions with their P/L, VIEW ONLY ([`crate::positions`]).
    Positions,
    /// The working orders, the node's emulated ones below them.
    Orders,
    Strategies,
    /// What was executed, read from the datahub's store: Fills · Order history · Summary
    /// ([`crate::history::history_body`]).
    History,
}

/// What the Account window reads besides its own state: the snapshot and the node's words for every
/// tab, and what the History tab reads beside them.
pub struct AccountWindowInputs<'a> {
    pub snapshot: &'a vike_execution::CoreSnapshot,
    pub words: &'a BackendNodeWords,
    /// The RESOLVED datahub address the History tab reads; `None` → its "No datahub" state.
    pub datahub: Option<&'a str>,
    pub display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
    /// The node's account directory (the History tab's account note).
    pub directory: Option<&'a vike_tradehub_client::wire::AccountDirectoryWire>,
    /// Now, epoch-ms: where a History read's window ends.
    pub now_ms: i64,
}

/// The Account window: the equity header, then the tabs (`Accounts (n)`, `Positions (n)`, `Orders (n)`,
/// `Strategies (n)`, each counting the rows its tab draws, live off the snapshot, then `History`) and
/// the one in `view.tab`. Writes a working order's cancel into `view.cancel`, which the shell hands
/// to the dispatcher, and RETURNS the History read the History tab wants started (only ever on a
/// frame that draws History, so a window that never opens it reads nothing).
pub fn account_window_body(
    ui: &mut egui::Ui,
    inputs: &AccountWindowInputs<'_>,
    view: &mut AccountWindowView,
) -> Option<crate::history::page::ExecutionHistoryQuery> {
    use vike_ui_theme::components::tabs::{ThemeTab, underline};
    let (snapshot, words) = (inputs.snapshot, inputs.words);

    // Cap the composed content width so a maximized window doesn't stretch the tables across the
    // whole screen; a normal-sized Account window (< cap) is unaffected. The History tab's tables
    // keep the window's whole width, as the Execution-history window's did.
    let capped = ui.available_width().min(account::MAX_WIDTH);
    let model = equity::AccountWindowEquityModel::from_snapshot(snapshot);
    let open = crate::positions::positions_rows(snapshot);
    ui.scope(|ui| {
        ui.set_max_width(capped);
        equity_header(ui, &model, snapshot.portfolio.margin_used_total);
        ui.add_space(space::XL);
        let accounts = format!("Accounts ({})", model.rows.len() + engineless(snapshot).count());
        let positions = format!("Positions ({})", open.len());
        let orders = format!("Orders ({})", snapshot.orders.len());
        let strategies = format!("Strategies ({})", words.mounts.len());
        let tabs = [
            ThemeTab { value: AccountWindowTab::Accounts, label: &accounts, count: None },
            ThemeTab { value: AccountWindowTab::Positions, label: &positions, count: None },
            ThemeTab { value: AccountWindowTab::Orders, label: &orders, count: None },
            ThemeTab { value: AccountWindowTab::Strategies, label: &strategies, count: None },
            ThemeTab { value: AccountWindowTab::History, label: "History", count: None },
        ];
        underline(ui, &mut view.tab, &tabs);
    });
    ui.add_space(space::SM);
    if view.tab != AccountWindowTab::History {
        ui.set_max_width(capped);
    }
    match view.tab {
        AccountWindowTab::Accounts => {
            accounts_section(ui, snapshot, &model, words);
            rates_section(ui, &model);
        }
        AccountWindowTab::Positions => crate::positions::positions_section(ui, &open),
        AccountWindowTab::Orders => {
            orders_section(ui, snapshot, &words.mounts, &mut view.cancel);
            emulated_section(ui, snapshot);
        }
        AccountWindowTab::Strategies => strategies_section(ui, words),
        AccountWindowTab::History => {
            let ledgers = vike_tradehub_client::fold_guards::off_dollar_ledgers(snapshot);
            let contracts = vike_tradehub_client::fold_guards::off_default_contracts(snapshot);
            let history = crate::history::AccountWindowHistoryInputs {
                datahub: inputs.datahub,
                display_time_zone: inputs.display_time_zone,
                directory: inputs.directory,
                terminal_orders: snapshot.orders.iter().filter(|o| !o.status.is_live()).count(),
                off_dollar_ledgers: &ledgers,
                off_default_contracts: &contracts,
                now_ms: inputs.now_ms,
            };
            return crate::history::history_body(ui, &mut view.history, &history);
        }
    }
    None
}
