//! The DOM (depth-of-market) tool-window GLUE — `vike-app`'s wiring AROUND the
//! [`vike_panels::dom`] widget, moved down out of the CI-excluded `main.rs` (tool-view extraction
//! batch 3, audit F1). The widget itself already lived in vike-panels; what moves here is the
//! snapshot→widget adaptation the binary owned: mark resolution, the per-venue order/position
//! projection, the synthetic-book fallback, and the action drain into `tv.dom_actions`.
//!
//! Read-only per-window transport (symbol / venue / book / staleness / live) arrives through
//! [`ToolCtx::book`]; `&mut ToolView` stays the separate param the actions land in.

use super::ToolCtx;
use crate::dom_math::{default_price, signed_position_size, synth_book, tick_for};
use crate::tools;
use vike_panels::dom;

/// The DOM ladder window body (Pro/Elite). Renders the LIVE L2 book for the window's SELECTED venue
/// (Binance/Bybit/OKX managed diff-depth, folded in `data_sink::BookStore`, arriving as
/// [`ToolCtx::book`]); falls back to a synthetic book only until the first real snapshot lands.
/// `stale` (no update in the freshness window) dims the ladder + shows a badge. Working orders +
/// position are shown ONLY on the execution venue (Binance) — the other venues are display-only
/// books for now; each ladder click leaves as a [`dom::DomAction`] the window loop maps to a
/// `vike_exec::Command`.
pub fn dom_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    let snap = ctx.snap;
    let inst = ctx.book.symbol;
    let venue = ctx.book.venue;
    let real = ctx.book.book.filter(|b| b.bid_levels() + b.ask_levels() > 0);
    // last/center: the DISPLAYED book's own mid (venue-correct); fall back to the core mark, then a seed
    let mark = real
        .and_then(|b| b.mid())
        .or_else(|| {
            snap.marks
                .iter()
                .find(|(v, s, _)| v == venue && s == inst)
                .map(|(_, _, p)| *p)
                .filter(|p| *p > 0.0)
        })
        .unwrap_or_else(|| default_price(inst));

    // real book if we have one, else a synthetic stand-in held alive for this frame
    let synth_holder;
    let book_ref: &vike_model::L2Book = match real {
        Some(b) => b,
        None => {
            synth_holder = synth_book(mark, tick_for(mark), tv.dom.tick);
            &synth_holder
        }
    };

    // Orders + position for THIS venue's engine — the cross-venue snapshot carries both per venue:
    // orders span all engines (each stamped with `venue`), positions live in the venue's `VenueBlock`.
    let orders: Vec<dom::DomOrder> = snap
        .orders
        .iter()
        .filter(|o| o.venue == venue && o.symbol == inst)
        .map(|o| dom::DomOrder {
            client_order_id: o.client_order_id.clone(),
            side: o.side,
            price: o.price.or(o.trigger_price).unwrap_or(0.0),
            qty: o.qty,
            is_stop: o.order_type == "stop",
            filled_qty: o.filled_qty,
        })
        .collect();

    let position = snap
        .portfolio
        .venues
        .iter()
        .find(|v| v.venue == venue)
        .and_then(|vb| vb.positions.iter().find(|p| p.symbol == inst))
        .map(|p| {
            let size = signed_position_size(p.size, &p.position_side);
            dom::DomPosition { size, avg_px: p.avg_px, upnl: p.unrealized }
        });

    let inputs = dom::DomInputs {
        book: book_ref,
        last: Some(mark),
        orders: &orders,
        position,
        stale: ctx.book.stale,
        // PAPER unless this venue has a credential-gated live client (lights the DOM's ● LIVE badge)
        paper: !ctx.book.live,
        // the selected venue's DECLARED capabilities (audit br6): the widget greys the drag-to-
        // reprice control when the adapter has no native modify, so an unsupported action is never
        // offered (rather than becoming a post-hoc reject). Unknown venue → conservative UNSUPPORTED.
        caps: vike_model::caps_for(venue),
    };
    let acts = dom::draw(ui, &mut tv.dom, &inputs);
    tv.dom_actions.extend(acts);
}
