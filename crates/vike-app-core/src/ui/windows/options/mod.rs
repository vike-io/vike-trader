//! The Options window body: two tabs, the Chain (one underlying's option chain,
//! `vike_order_entry::option_chain`) and Portfolio Greeks (the trader's open Deribit option positions
//! with live Δ/Γ/ν/Θ, `greeks`). The Greeks window was a window kind of its
//! own until 2026-11-17; its launcher now opens this window on the second tab
//! (`crate::ui::window::spawn::WindowSpawnTarget::PortfolioGreeks`). The chain arm moved here from
//! `vike-desktop`'s `tool_content`, which now only dispatches to [`options_tool_content`].

use super::{WindowToolContext, venue_label};

mod greeks;
#[path = "order_ticket_tests.rs"]
#[cfg(test)]
mod order_ticket_tests;
use crate::ui::windows::WindowView;
use vike_order_entry::option_chain::ticket::{
    OptionChainTicket, OptionChainTicketChoice, order_ticket,
};
use vike_ui_theme::components::tabs::{ThemeTab, underline};
use vike_ui_theme::metrics::space;

/// The Options window's tabs; one shows at a time, per window (`WindowView::options_tab`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptionsTab {
    /// One underlying's option chain at one expiry; a bid or ask click prefills the confirm ticket.
    #[default]
    Chain,
    /// The open Deribit option positions with per-position and net greeks. Read-only.
    PortfolioGreeks,
}

/// The Options window: the tabs, then the one in `tv.options_tab`, then the confirm ticket when one is
/// open. The ticket is its own modal and stays up whichever tab shows; the order reaches the core
/// (`tv.options_submit`) ONLY on Confirm.
pub fn options_tool_content(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
) {
    let tabs = [
        ThemeTab { value: OptionsTab::Chain, label: "Chain", count: None },
        ThemeTab { value: OptionsTab::PortfolioGreeks, label: "Portfolio Greeks", count: None },
    ];
    underline(ui, &mut tv.options_tab, &tabs);
    ui.add_space(space::SM);
    match tv.options_tab {
        OptionsTab::Chain => chain_tab(ui, context, tv),
        OptionsTab::PortfolioGreeks => greeks::greeks_tool_content(ui, context),
    }
    if let Some(mut ticket) = tv.options_order_ticket.take() {
        match order_ticket(ui.ctx(), &mut ticket) {
            OptionChainTicketChoice::Confirmed => tv.options_submit = Some(ticket),
            OptionChainTicketChoice::Open => tv.options_order_ticket = Some(ticket),
            OptionChainTicketChoice::Cancelled => {}
        }
    }
}

/// The Chain tab: the selected underlying's bundle (default = the first fetched), an expiry the
/// server does not list reset, the pick recorded for the options poll, and the chain's clicks turned
/// into the window's OUT slots.
fn chain_tab(ui: &mut egui::Ui, context: &WindowToolContext<'_>, tv: &mut WindowView) {
    let td = context.tool_feeds;
    let bundle = vike_datahub_client::tool_feeds::poll::options::options_selection(
        td,
        &tv.options_underlying_selection,
        &mut tv.options_expiry_selection,
    );
    let empty_chains = std::collections::BTreeMap::new();
    let (chains, default_expiry, expiries) = match bundle {
        Some(b) => (&b.chains, b.default_expiry.as_str(), b.expiries.as_slice()),
        None => (&empty_chains, "", &[][..]),
    };
    // The user's OWN deribit working orders + positions, keyed by instrument name — the chain paints
    // per-strike markers from this (empty ⇒ grid unchanged). Built fresh each frame from the lossy
    // snapshot (cheap: O(orders+positions), both small).
    let (working, positions) = crate::orders::option_chain_orders::option_chain_orders(
        &context.snapshot.orders,
        &context.snapshot.positions,
    );
    let acts = vike_order_entry::option_chain::draw(
        ui,
        vike_order_entry::option_chain::OptionChainInputs {
            chains,
            default_expiry,
            expiries,
            expiry_selection: &mut tv.options_expiry_selection,
            underlyings: &td.options_underlyings,
            underlying_selection: &mut tv.options_underlying_selection,
            strike_window: &mut tv.options_strike_window,
            working: &working,
            positions: &positions,
            // "deribit" is the chain's data-source KEY (an address, not a spelling).
            venue_label: venue_label(context.directory, "deribit"),
        },
    );
    // A working-order marker click → the order client id in the OUT slot; the update loop drains it
    // into an `OrderIntent::Cancel` (mirrors the Trade window's `TradeWindowAction::Cancel`).
    if let Some(order_client_id) = acts.cancel {
        tv.options_cancel = Some(order_client_id);
    }
    // Refresh pill → wake the options poll thread for an IMMEDIATE ask instead of waiting out its
    // cadence (a closed channel at shutdown is ignored, never a panic).
    if acts.refresh_clicked {
        td.options_link.refresh();
    }
    // A chain bid/ask click prefills a CONFIRM ticket — it NEVER submits an order. Side, instrument
    // and price come from the click; qty defaults to a deribit option size the user can edit.
    if let Some(click) = acts.order {
        tv.options_order_ticket = Some(OptionChainTicket {
            instrument: click.instrument,
            side: click.side.into(),
            price: click.price,
            quantity: 0.1,
            kind: click.kind,
            strike: click.strike,
        });
    }
}
