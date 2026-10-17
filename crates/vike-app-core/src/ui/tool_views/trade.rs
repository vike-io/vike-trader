//! The Trade window's GLUE (`docs/superpowers/specs/2026-09-30-trade-window-design.md` §4): it builds
//! `vike_panels::trade::TradeInputs` from the backend snapshot, the node's `Directory` reply, the
//! window's own market-data book and the instrument catalog, draws the widget, paints its view
//! controls into the title bar's slot, and leaves its intents in `tv` for the shell to address and
//! dispatch. It replaces the DOM window's glue and the old Trade window's ticket; `account.rs` keeps
//! the rest of that window as the Account window.
//!
//! Everything the widget is handed comes out of ONE pure function, [`window_parts`], so the wiring
//! spec §7 lists (the two-account picker, the published mode chip, the `free_bp` sizes, Ruling R9's
//! hidden markers) is unit-tested without drawing anything.
//!
//! # ⚠ It invents nothing (the DOM glue's rule, kept)
//! No book → an EMPTY book reaches the widget, which draws the absence; no price → `None`, a dash;
//! a node that says nothing about an account's tier → `None` (`MODE ?`), never a guess; a
//! symbol the catalog does not list → a lot of `0.0`, so every size is refused and the status strip
//! says what is missing (I18). A credentialed venue whose catalog is thin (alpaca and the other
//! `Credentialed` rows of `crates/vike-catalog/src/availability.rs`) therefore cannot trade a
//! symbol its catalog does not list; whether the window may take a lot size from anywhere else is
//! the owner's open question, and this file does not answer it by making one up.
//!
//! # ⚠ A stopped core and a halted account trade nothing, and say so
//! The deleted ticket refused every order while `CoreSnapshot::fault` was set. The window keeps that
//! rule and adds the per-account halt (`VenueBlock::trading_state`): on a faulted core every window
//! is untradable with the fault on its strip, never "Waiting for the node." (a core that faults
//! while publishing leaves a snapshot with no blocks, which would otherwise read as one that has not
//! said anything yet); on a HALTED account its window is untradable and says why; a REDUCING one
//! still trades and says it takes only reducing orders. Both rules are `order_dispatch::tradable`'s,
//! so the dispatcher refuses what the window does not offer.
//!
//! A desktop that cannot SEND trades nothing either, and says so before the click (the owner's
//! decision of 10-03, item 11): a read-only desktop (no control channel) and one whose control link
//! has closed are causes like the others ([`ControlLink`], [`refusal`]).
//!
//! The widget is TOLD the cause: [`refusal`] computes it once, the strip shows it, and the same text
//! goes into `Tradable::No::why`, which the ticket's line, the ladder's hint and every disabled
//! button say. The widget never reads the strip to guess it (the strip also carries the window's
//! own last rejection, which is not why the window takes no order).
//!
//! ⚠ What an untradable window offers for orders ALREADY working is not uniform, and this is what
//! the code does. The LADDER offers no cancel: its marker click sits behind the same `Tradable::Yes`
//! as its order clicks, so a click on a marker does nothing. The TICKET's cancel row (Cancel all,
//! Bids, Asks) stays enabled — it waits only for the node to name each order's account (Ruling R9)
//! and for there to be an order — and the dispatcher passes a cancel through on a halted account and
//! a stopped core alike. A stopped core rarely leaves it anything: entering its safe state sweeps
//! its working orders itself, best-effort. A halted account's resting orders stay where they are
//! until the operator cancels them: from that row, or outside the window.
//!
//! # ⚠ An account text that is not a label trades NOTHING
//! A window names its account as text (`ToolView::trade_account`). It is read with
//! `order_dispatch::TradeAddress::parse`, and a text that is not an account label refuses the whole
//! window rather than becoming `None`: `None` is the DEFAULT account, so the old `.ok()` habit would
//! have turned a corrupted label into orders on a different book.
//!
//! # Catalog answers are cached per window (I13)
//! A catalog scan is a linear pass over every instrument with a sort, and the picker asks about
//! every account's venue. [`TradeCatalog`] keeps the window instrument's grid, its spelling on
//! every other venue and the symbol picker's matches, each keyed on what it was computed for and
//! all of them on the catalog's `Arc`: a refreshed catalog drops them all. The account rows
//! themselves are built every frame from those answers and the snapshot.
//!
//! # What the status strip follows, and what it cannot
//! Spec §3.8 wants the strip to follow the window's OWN order by its client order id. The window
//! never learns that id (the dispatcher mints it and returns no per-window record), so the strip
//! follows the newest order on the window's venue, symbol AND account that appeared after the window
//! looked ([`follow_status`]). On one address that is the window's order unless a second window on
//! the same address, or a strategy trading that account and symbol, placed one since.
//!
//! # What "the most recently used window" means here
//! [`ToolCtx::last_trade`] seeds a new window (spec §3.11). This file only reads it; the shell
//! writes it, and it must write it from the window the trader last ACTED in (an intent or a pick),
//! never from every Trade window every frame, or a new window opens on whichever window the loop
//! happened to visit last.

use std::borrow::Cow;

use super::ToolCtx;
use crate::lock;
use crate::orders::order_dispatch::TradeAddress;
use crate::tools;
use vike_exec::{CoreSnapshot, VenueBlock};
use vike_model::TradingState;
use vike_panels::trade::StatusKind;
use vike_ui_theme::components::state::{self, Load};

mod accounts;
mod catalog;
mod reasons;
mod window;

pub use accounts::{TradePick, venue_label};
pub use catalog::TradeCatalog;
pub use reasons::ControlLink;
pub(crate) use reasons::UNADDRESSABLE_WHY;
pub use window::SeenOrder;

use accounts::{NotRunningRow, not_running_rows, seed};
use reasons::{HALTED_WHY, NO_CONTROL_LINK_WHY, NOT_RUNNING_WHY, READ_ONLY_WHY, WAITING_FOR_NODE};
use window::{Refusal, TAPE_SHOWN, WindowSources, follow_status, window_parts};

/// Why no window trades on a core that has stopped (`vike_exec::CoreSnapshot::fault`: a handler
/// panicked and the core is in its safe state), with the core's own words.
fn stopped_why(fault: &str) -> String {
    format!("The server has stopped trading: {fault}")
}

/// The cause a window at `address` takes no order for (`address` is `None` when its account text
/// names no account; `block` is its account's engine, if the snapshot runs one; `link` is whether
/// this desktop can send at all). ERROR causes first — each outranks the window's own last order on
/// the strip — then Info ones, which that line outranks there; within each, the widest scope first:
///
/// * a core that has stopped (the whole server), then a LOST control link (this whole desktop),
///   then an account text that is not a label (this window), then a HALTED engine (this account) —
///   all errors;
/// * then a READ-ONLY desktop (no control channel: a setting of this desktop, not a fault), a node
///   that has said nothing yet, and an account the server does not run — Info lines. Where the node
///   names that account as not running (`said`, decision 0156), the line carries its reason;
/// * last, `symbol` (when the picked symbol is not the one the engine was started on): the account
///   keeps to its one symbol, or its venue's catalog does not list the picked one
///   ([`reasons::one_symbol_why`], [`reasons::not_in_catalog_why`]).
///
/// `None` while the window takes orders, and for the one cause the widget words from the account's
/// list itself: an engine that trades other symbols on an older node that publishes no list (anything
/// but its primary market). Every cause here is one the window refuses
/// (`order_dispatch::tradable` for the snapshot's, [`window_parts`] for the link's and the symbol's),
/// so a stated cause never reaches a window that trades.
fn refusal(
    snap: &CoreSnapshot,
    address: Option<&TradeAddress>,
    block: Option<&VenueBlock>,
    said: Option<&NotRunningRow>,
    link: ControlLink,
    symbol: Option<String>,
) -> Option<Refusal> {
    let (kind, text) = if let Some(fault) = snap.fault.as_deref() {
        (StatusKind::Error, Cow::Owned(stopped_why(fault)))
    } else if link == ControlLink::Lost {
        (StatusKind::Error, Cow::Borrowed(NO_CONTROL_LINK_WHY))
    } else if address.is_none() {
        (StatusKind::Error, Cow::Borrowed(UNADDRESSABLE_WHY))
    } else if block.is_some_and(|b| b.trading_state == TradingState::Halted) {
        (StatusKind::Error, Cow::Borrowed(HALTED_WHY))
    } else if link == ControlLink::ReadOnly {
        (StatusKind::Info, Cow::Borrowed(READ_ONLY_WHY))
    } else if snap.portfolio.venues.is_empty() {
        (StatusKind::Info, Cow::Borrowed(WAITING_FOR_NODE))
    } else if block.is_none() {
        let text = said.map_or(Cow::Borrowed(NOT_RUNNING_WHY), |n| Cow::Owned(n.why.clone()));
        (StatusKind::Info, text)
    } else {
        (StatusKind::Info, Cow::Owned(symbol?))
    };
    Some(Refusal { kind, text })
}

/// The window's view controls (header variant 1, spec §3.2), painted into the title bar through a
/// detached `Ui`, right-aligned in the slot the bar reserved for them. `ui` lends its context, its
/// layer, its id and its style; nothing is allocated in it. Answers the window size a click asked
/// for ([`vike_panels::trade::view_controls`]).
///
/// ⚠ Across, the slot; DOWN, the whole bar, the controls centred in it. The slot is only
/// `title_bar::TAB_H` (24) tall: it is bottom-seated so a Connections chip breaks the hairline,
/// which these controls do not do. The detached `Ui`'s rect is its clip, so in the slot's rect
/// they were cut along their whole bottom edge (the render check of 2026-10-03, when each was a
/// `control_h` square, 28 pt at Comfortable). They are the design's 24 × 20 now at every density,
/// in a 25 pt bar, level with the window's own controls, which are centred in it too.
pub(crate) fn title_bar_view_controls(
    ui: &egui::Ui,
    slot: &crate::ui::workspace::TitleTabSlot,
    state: &mut vike_panels::trade::TradeState,
) -> Option<egui::Vec2> {
    let w = vike_panels::trade::layout::VIEW_CONTROLS_W;
    let r = egui::Rect::from_min_max(
        egui::pos2((slot.tabs.max.x - w).max(slot.tabs.min.x), slot.bar.min.y),
        egui::pos2(slot.tabs.max.x, slot.bar.max.y),
    );
    let mut bar = egui::Ui::new(
        ui.ctx().clone(),
        ui.id().with("trade_view_controls"),
        egui::UiBuilder::new()
            .layer_id(ui.layer_id())
            .max_rect(r)
            .layout(egui::Layout::left_to_right(egui::Align::Center))
            .style(ui.style().clone()),
    );
    vike_panels::trade::view_controls(&mut bar, state)
}

/// The window body. `slot` is the title bar's reserved rect (`WinKind::Trade.carries_title_tabs`),
/// where the view controls go (header variant 1).
pub fn trade_tool_content(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    slot: &crate::ui::workspace::TitleTabSlot,
) {
    if let Some(size) = title_bar_view_controls(ui, slot, &mut tv.trade) {
        tv.trade_resize = Some(size);
    }

    let (venue, symbol) = (ctx.book.venue, ctx.book.symbol);
    if venue.is_empty() || symbol.is_empty() {
        // A window's first frame: open it on the seed, or say what it is waiting for (minor 27) —
        // or, on a core that has stopped, that it has stopped, which no waiting will cure.
        let pick = seed(ctx.snap, ctx.last_trade);
        let stopped = ctx.snap.fault.as_deref().map(stopped_why);
        match (&pick, &stopped) {
            (Some(_), _) => state::view(ui, Load::Loading("Opening the instrument.")),
            (None, Some(why)) => state::view(ui, Load::Unreachable(why)),
            (None, None) => state::view(ui, Load::Loading(WAITING_FOR_NODE)),
        };
        tv.trade_pick = pick;
        return;
    }

    tv.trade_catalog.sync(ctx.symbols, venue, symbol, &tv.trade.query);
    let account = tv.trade_account.clone();
    follow_status(tv, ctx.snap, venue, account.as_deref(), symbol);
    // THE window's own depth link, verbatim: this venue's market-data status line. A venue with no
    // producer is ABSENT from the map, a different statement from one that has written nothing.
    let source: Option<String> = ctx.feed_statuses.get(venue).map(|h| lock(h).clone());
    // The last two minutes of prints, read without taking them: a chart window on the same symbol
    // drains the same key.
    let tape = ctx.book.trades.map_or_else(Vec::new, |s| s.recent(venue, symbol, TAPE_SHOWN));
    // The accounts the node does not run, with their reasons — empty on a node that names none.
    let not_running = not_running_rows(ctx.snap);
    let parts = window_parts(&WindowSources {
        snap: ctx.snap,
        directory: ctx.directory,
        catalog: &tv.trade_catalog,
        venue,
        account: account.as_deref(),
        symbol,
        book: ctx.book.book,
        tape: &tape,
        stale: ctx.book.stale,
        source: source.as_deref(),
        recent: &tv.trade_recent,
        status: tv.trade_status.as_ref(),
        bracket_wire: crate::backend::tradehub_control::bracket_has_wire_form(),
        control_link: ctx.control_link,
        directory_unavailable: ctx.directory_unavailable,
        not_running: &not_running,
    });
    let acts = vike_panels::trade::draw(ui, &mut tv.trade, &parts.inputs());
    tv.trade_actions.extend(acts);
}

#[path = "trade_tests.rs"]
#[cfg(test)]
mod trade_tests;
