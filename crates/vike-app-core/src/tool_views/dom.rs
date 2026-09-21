//! The DOM (depth-of-market) tool-window GLUE — `vike-app`'s wiring AROUND the
//! [`vike_panels::dom`] widget, moved down out of the CI-excluded `main.rs` (tool-view extraction
//! batch 3, audit F1). The widget itself already lived in vike-panels; what moves here is the
//! snapshot→widget adaptation the binary owned: mark resolution, the per-venue order/position
//! projection, the no-book classification, and the action drain into `tv.dom_actions`.
//!
//! Read-only per-window transport (symbol / venue / book / staleness / live) arrives through
//! [`ToolCtx::book`]; `&mut ToolView` stays the separate param the actions land in.
//!
//! # ⚠ This file used to fabricate the ladder, and the replacement is nothing, not a better fake
//!
//! Until 2026-09-15 the `None` arm of the book lookup called `dom_math::synth_book` and handed the
//! widget 40 invented levels per side, seeded on `tv.dom.tick` — the frame counter
//! `vike_panels::dom::draw` increments at its top, so the whole ladder re-rolled every frame. The
//! mark fell back to `dom_math::default_price`, a table that answers `62_800.0` for `BTCUSDT`. An
//! operator asking "is my DOM getting real data" had a dim overlay and a `● STALE` badge to go on,
//! against a ladder that moved. Both helpers are deleted; the widget now draws nothing
//! ladder-shaped without a book and this file supplies the WORDS instead
//! ([`crate::dom_math::depth_absence`]).
//!
//! # ⚠ Two sockets, and only one of them carries a book
//!
//! The shell's status bar reports the TRADEHUB observe connection. That wire carries no depth:
//! `vike_tradehub_client::wire::WireSnapshot` has no book, depth, quote or tape field,
//! `vike_core::CoreSnapshot` has none either, and the core's `Ingest::Book` arm folds the
//! `Arc<L2Book>` and drops it. Depth reaches this ladder over the DATAHUB market-data link
//! ([`crate::md_session`]) instead. So `tradehub [LIVE] — OBSERVING … (connected)` says nothing
//! about whether this window has data, and the DOM now states its OWN link
//! ([`vike_panels::dom::DomInputs::source`]) rather than leaving a global strip about a different
//! socket to be read as an answer.

use super::ToolCtx;
use crate::dom_math::{
    depth_absence, depth_absence_headline, depth_absence_next_step, signed_position_size,
};
use crate::tools;
use vike_panels::dom;

/// The DOM ladder window body (Pro/Elite). Renders the LIVE L2 book for the window's SELECTED venue
/// (Binance/Bybit/OKX/Aster/Hyperliquid managed diff-depth, folded in `data_sink::BookStore`,
/// arriving as [`ToolCtx::book`]). With no book it renders an explicit empty state naming the cause
/// — never a stand-in ladder. `stale` (no update in the freshness window) dims a REAL ladder and
/// shows a badge. Working orders + position are shown ONLY on the execution venue — the others are
/// display-only books for now; each ladder click leaves as a [`dom::DomAction`] the window loop maps
/// to a `vike_exec::Command`.
pub fn dom_tool_content(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView) {
    let snap = ctx.snap;
    let inst = ctx.book.symbol;
    let venue = ctx.book.venue;
    let real = ctx.book.book.filter(|b| b.bid_levels() + b.ask_levels() > 0);
    // last/center: the DISPLAYED book's own mid (venue-correct); fall back to the core mark.
    //
    // ⚠ There is NO third rung. It used to be `dom_math::default_price(inst)`, and the header
    // printed that table value in the same amber and the same place a venue's real last price uses.
    // `None` now reaches the widget as a dash.
    let mark: Option<f64> = real.and_then(|b| b.mid()).or_else(|| {
        snap.marks
            .iter()
            .find(|(v, s, _)| v == venue && s == inst)
            .map(|(_, _, p)| *p)
            .filter(|p| *p > 0.0)
    });

    // THE DOM's OWN SOURCE, verbatim: this venue's market-data status line, written by the thread
    // that dials, subscribes and reads (`md_session`'s `refresh_statuses`). A venue with no producer
    // is ABSENT from the map — a different statement from a producer that has written nothing, and
    // `depth_absence` keeps the two apart.
    let source: Option<String> =
        ctx.feed_statuses.get(venue).map(|h| h.lock().unwrap_or_else(|e| e.into_inner()).clone());

    // An EMPTY book held alive for this frame. The widget takes the no-book decision from the book
    // it is handed (no levels either side ⇒ empty state), so this carries no price information at
    // all and none can leak onto the screen through it.
    let empty_holder;
    let book_ref: &vike_model::L2Book = match real {
        Some(b) => b,
        None => {
            empty_holder = vike_model::L2Book::new(1.0);
            &empty_holder
        }
    };

    // The WORDS for the empty state — computed whenever there is no book, from the one fact the
    // shell holds. The widget ignores them when a ladder is drawn.
    let absence = real.is_none().then(|| {
        let a = depth_absence(source.as_deref());
        dom::BookAbsence {
            headline: depth_absence_headline(a),
            // The status line VERBATIM. `depth_absence` picks a coarse bucket; these are the exact
            // words, so a bucket that is vague still leaves the operator with the truth.
            cause: source.as_deref().unwrap_or("").trim(),
            next_step: depth_absence_next_step(a),
        }
    });

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
        last: mark,
        orders: &orders,
        position,
        stale: ctx.book.stale,
        // PAPER unless this venue has a credential-gated live client (lights the DOM's ● LIVE badge)
        paper: !ctx.book.live,
        // the selected venue's DECLARED capabilities (audit br6): the widget greys the drag-to-
        // reprice control when the adapter has no native modify, so an unsupported action is never
        // offered (rather than becoming a post-hoc reject). Unknown venue → conservative UNSUPPORTED.
        caps: vike_model::caps_for(venue),
        source: source.as_deref().unwrap_or(""),
        absence,
    };
    let acts = dom::draw(ui, &mut tv.dom, &inputs);
    tv.dom_actions.extend(acts);
}
