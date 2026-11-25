//! The chain grid's paint: the header over the columns, the rows, and one row's cells (the ATM
//! highlight, the in-the-money wash, the working-order and position markers), with the order a
//! bid or ask cell click produces ([`super::OptionChainOrderClick`]).

use egui::{Align2, Color32, Sense, StrokeKind};
use vike_marketdata::OptionKind;
use vike_options::chain::columns;
use vike_ui_theme::color::with_alpha;
use vike_ui_theme::components::ThemeTokens;
use vike_ui_theme::components::state::{ThemeEmptyPane, view};
use vike_ui_theme::maps::{self, ThemeMapRow};
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::options_chain;

use super::layout::OptionChainColumn;
use super::{OptionChainActions, OptionChainGrid, OptionChainInputs, OptionChainOrderClick};

/// Decide the order a chain bid/ask cell click produces. `want_ask` = the ASK cell was clicked
/// (⇒ Buy at ask); otherwise the BID cell (⇒ Sell at bid). Returns `None` — the cell is NOT
/// tradeable — when the quote lacks an `instrument_name` or the relevant price is absent.
pub(super) fn quote_order(
    quote: &vike_marketdata::OptionQuote,
    want_ask: bool,
    kind: OptionKind,
    strike: f64,
) -> Option<OptionChainOrderClick> {
    let instrument = quote.instrument_name.clone()?;
    let price = if want_ask { quote.ask } else { quote.bid }?;
    let side = if want_ask { vike_model::OrderSide::Long } else { vike_model::OrderSide::Short };
    Some(OptionChainOrderClick { instrument, side, price, kind, strike })
}

/// Compact signed-quantity label for a position badge: `+2` / `-1` for whole sizes, two decimals for
/// fractional deribit sizes (e.g. `+0.10`). Sign is always shown.
fn format_position_quantity(q: f64) -> String {
    if (q.fract()).abs() < 1e-9 { format!("{q:+.0}") } else { format!("{q:+.2}") }
}

/// The row of the `callput` map a side of the chain is drawn by.
pub(super) fn leg(kind: OptionKind) -> &'static ThemeMapRow {
    match kind {
        OptionKind::Call => &maps::callput::CALL,
        OptionKind::Put => &maps::callput::PUT,
    }
}

/// The quote a strike row holds for one side of the chain.
fn side_of(
    row: &vike_marketdata::OptionChainStrike,
    kind: OptionKind,
) -> &Option<vike_marketdata::OptionQuote> {
    match kind {
        OptionKind::Call => &row.call,
        OptionKind::Put => &row.put,
    }
}

/// A quote's traded volume — `0.0` when the side holds no quote or the quote carries none.
pub(super) fn volume_of(q: &Option<vike_marketdata::OptionQuote>) -> f64 {
    q.as_ref().and_then(|q| q.volume).unwrap_or(0.0)
}

// value + colour for one cell — value/format from the pure column model; bid/ask NUMBERS use
// the market set's TEXT colour, never its graphic one (Review Focus 5).
pub(super) fn chain_cell(
    t: &ThemeTokens,
    spot: Option<f64>,
    dte: i64,
    c: &OptionChainColumn,
    row: &vike_marketdata::OptionChainStrike,
) -> (String, Color32) {
    match c {
        OptionChainColumn::Strike => (columns::fmt_strike(row.strike), t.theme.text),
        OptionChainColumn::Iv => {
            // centre IV: the call's if a call quote exists, else the put's
            let iv = row
                .call
                .as_ref()
                .and_then(|q| q.iv)
                .or_else(|| row.put.as_ref().and_then(|q| q.iv));
            (
                columns::fmt(iv, "iv"),
                if iv.is_some() { t.theme.text } else { t.theme.text_tertiary },
            )
        }
        OptionChainColumn::Field { field, kind } => {
            let q = side_of(row, *kind).as_ref();
            let raw = columns::cell_value(field, q, spot, dte, 0.0);
            let color = match (*field, raw.is_some()) {
                (_, false) => t.theme.text_tertiary,
                ("bid" | "bidpct", true) => ThemeSidePair::BidAsk.text(true, t),
                ("ask" | "askpct", true) => ThemeSidePair::BidAsk.text(false, t),
                _ => t.theme.text,
            };
            (columns::fmt(raw, field), color)
        }
    }
}

/// Draw phase — the two header rows: the CALLS | Strike | PUTS super-header band and the
/// right-aligned column titles.
pub(super) fn paint_chain_header(ui: &mut egui::Ui, g: &OptionChainGrid<'_>) {
    let OptionChainGrid { t, cols, edges, strike_col, full, header_font, .. } = *g;
    // ---- super-header band: CALLS | Strike | PUTS centred over each group ----
    {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(full, t.metrics.control_height), Sense::hover());
        let p = ui.painter();
        let mid_l = rect.left() + edges[strike_col];
        let mid_r = rect.left() + edges[strike_col + 2];
        let bf = t.font_semibold(ThemeTextRole::Strong);
        p.text(
            egui::pos2((rect.left() + mid_l) / 2.0, rect.center().y),
            Align2::CENTER_CENTER,
            "CALLS",
            bf.clone(),
            maps::callput::CALL.text.resolve(t),
        );
        p.text(
            egui::pos2((mid_l + mid_r) / 2.0, rect.center().y),
            Align2::CENTER_CENTER,
            "Strike",
            bf.clone(),
            t.theme.text_tertiary,
        );
        p.text(
            egui::pos2((mid_r + rect.right()) / 2.0, rect.center().y),
            Align2::CENTER_CENTER,
            "PUTS",
            bf,
            maps::callput::PUT.text.resolve(t),
        );
    }
    // ---- column header row (right-aligned, 8px inset) ----
    {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(full, t.metrics.control_height), Sense::hover());
        let p = ui.painter();
        for (i, c) in cols.iter().enumerate() {
            p.text(
                egui::pos2(rect.left() + edges[i + 1] - space::LG, rect.center().y),
                Align2::RIGHT_CENTER,
                c.0,
                header_font.clone(),
                t.theme.text_secondary,
            );
        }
    }
}

/// Draw phase — the data rows inside the vertical scroll area: the loading / empty tray when there
/// are no strikes, otherwise one [`paint_chain_row`] per strike.
pub(super) fn paint_chain_rows(
    ui: &mut egui::Ui,
    g: &OptionChainGrid<'_>,
    rows: &[vike_marketdata::OptionChainStrike],
    inp: &OptionChainInputs<'_>,
    active: &str,
    actions: &mut OptionChainActions,
) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        // §4.2: loading and empty are different renderings. No chain yet ("Fetching") reads
        // differently from a fetched expiry with no strikes (the empty tray) — the third
        // rendering, Unreachable, needs a fetch-status input this crate does not carry yet (the
        // poll thread's failure lives in vike-desktop); owed to that PR, not guessed here.
        if rows.is_empty() {
            match inp.chains.get(active) {
                None => view(ui, ThemeEmptyPane::Loading("Fetching the chain…")),
                Some(_) => view(ui, ThemeEmptyPane::Empty("No strikes for this expiry")),
            };
        }
        for (ri, o) in rows.iter().enumerate() {
            paint_chain_row(ui, g, inp.working, inp.positions, ri, o, actions);
        }
    });
}

/// Draw phase — ONE strike row: the ITM hatch, the spine and ATM rule, the clickable bid/ask cells,
/// the volume bar and cell text, then the user's own position badge and working-order marker.
fn paint_chain_row(
    ui: &mut egui::Ui,
    g: &OptionChainGrid<'_>,
    working: &std::collections::BTreeMap<String, Vec<String>>,
    positions: &std::collections::BTreeMap<String, vike_model::Position>,
    ri: usize,
    o: &vike_marketdata::OptionChainStrike,
    actions: &mut OptionChainActions,
) {
    let OptionChainGrid {
        t,
        cols,
        edges,
        strike_col,
        full,
        spot,
        dte,
        atm_index,
        max_volume,
        mono,
        ..
    } = *g;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(full, t.metrics.row_height), Sense::hover());
    let p = ui.painter();
    // ITM diagonal-hatch (Qt.BDiagPattern emulation) on in-the-money cells with
    // a quote: calls ITM when strike<spot, puts ITM when strike>spot; skip the
    // centre spine (mirrors OptionsTab._fill_side's `q is not None` gate).
    let hatch = with_alpha(t.theme.border, options_chain::ITM_HATCH_ALPHA);
    for (i, c) in cols.iter().enumerate() {
        let OptionChainColumn::Field { kind, .. } = c.2 else { continue };
        let itm = side_of(o, kind).is_some()
            && spot.is_some_and(|s| match kind {
                OptionKind::Call => o.strike < s,
                OptionKind::Put => o.strike > s,
            });
        if !itm {
            continue;
        }
        let cr = egui::Rect::from_min_max(
            egui::pos2(rect.left() + edges[i], rect.top()),
            egui::pos2(rect.left() + edges[i + 1], rect.bottom()),
        );
        let cp = p.with_clip_rect(cr); // else the diagonals overflow into neighbours
        let mut x = cr.left() - options_chain::HATCH_LEAN;
        while x < cr.right() {
            cp.line_segment(
                [egui::pos2(x, cr.bottom()), egui::pos2(x + options_chain::HATCH_LEAN, cr.top())],
                egui::Stroke::new(stroke::HAIRLINE, hatch),
            );
            x += 6.0;
        }
    }
    let spine = egui::Rect::from_min_max(
        egui::pos2(rect.left() + edges[strike_col], rect.top()),
        egui::pos2(rect.left() + edges[strike_col + 2], rect.bottom()),
    );
    p.rect_filled(spine, 0.0, t.theme.hover);
    if Some(ri) == atm_index {
        p.line_segment(
            [egui::pos2(rect.left(), rect.top()), egui::pos2(rect.right(), rect.top())],
            egui::Stroke::new(stroke::EDGE, t.theme.accent), // the accent as a SHAPE, never a number's colour
        );
    }
    // Clickable bid/ask cells (CALL-bid, CALL-ask, PUT-bid, PUT-ask): a click prefills a
    // CONFIRM ticket (nothing is submitted here). Only cells whose quote has BOTH an
    // instrument_name AND the relevant price are interactive (and get a faint hover fill so
    // they read as clickable). All other cells/columns stay inert. `ui.interact` takes
    // `&self`, so it coexists with the `p = ui.painter()` borrow held for this row.
    for (i, c) in cols.iter().enumerate() {
        let OptionChainColumn::Field { field: field @ ("bid" | "ask"), kind } = c.2 else {
            continue;
        };
        let want_ask = field == "ask";
        let Some(order) =
            side_of(o, kind).as_ref().and_then(|q| quote_order(q, want_ask, kind, o.strike))
        else {
            continue; // not tradeable → no highlight, no interaction
        };
        let cell_rect = egui::Rect::from_min_max(
            egui::pos2(rect.left() + edges[i], rect.top()),
            egui::pos2(rect.left() + edges[i + 1], rect.bottom()),
        );
        let response =
            ui.interact(cell_rect, ui.scope_id().with(("optcell", ri, i)), Sense::click());
        if response.hovered() {
            // faint side-tinted fill: the market's up for a bid (sell), down for an ask (buy)
            let tint = ThemeSidePair::BidAsk.colour(!want_ask, t);
            p.rect_filled(cell_rect, 0.0, with_alpha(tint, options_chain::CELL_HOVER_ALPHA));
        }
        if response.clicked() {
            actions.order = Some(order);
        }
    }
    for (i, c) in cols.iter().enumerate() {
        if let OptionChainColumn::Field { field: "volume", kind } = c.2 {
            let v = volume_of(side_of(o, kind));
            if v > 0.0 {
                let frac = (v / max_volume).min(1.0) as f32;
                let cl = rect.left() + edges[i] + space::SM;
                let cw = (edges[i + 1] - edges[i] - 2.0 * space::SM) * frac;
                let bar = with_alpha(leg(kind).colour.resolve(t), options_chain::VOLUME_BAR_ALPHA);
                p.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(cl, rect.bottom() - space::MD),
                        egui::vec2(cw, options_chain::VOLUME_BAR_HEIGHT),
                    ),
                    0.0,
                    bar,
                );
            }
        }
        let (txt, col) = chain_cell(t, spot, dte, &c.2, o);
        p.text(
            egui::pos2(rect.left() + edges[i + 1] - space::LG, rect.center().y),
            Align2::RIGHT_CENTER,
            txt,
            mono.clone(),
            col,
        );
    }
    // ---- the USER's own working orders + position markers (options-chain-orders) ----
    // For each side whose quote carries an `instrument_name` we hold a book for, paint a
    // compact position badge (the market's up/down) and a clickable working-order marker
    // (`○N`, cancels the first resting order on click). Calls anchor in the free far-LEFT
    // gutter; puts just RIGHT of the centre spine — both land in the empty left area of a
    // NON-bid/ask column, so they never collide with the bid/ask order-click. A side with
    // neither a position nor a working order paints nothing (byte-identical to before).
    let small = t.mono(ThemeTextRole::Caption);
    for kind in [OptionKind::Call, OptionKind::Put] {
        let quote = side_of(o, kind);
        let Some(instrument) = quote.as_ref().and_then(|q| q.instrument_name.as_deref()) else {
            continue;
        };
        let resting = working.get(instrument).map_or(&[][..], Vec::as_slice);
        let position_quantity = positions.get(instrument).map_or(0.0, |p| p.size);
        if position_quantity == 0.0 && resting.is_empty() {
            continue;
        }
        let my = rect.center().y;
        // measure-free width for a monospace pill (avoids a galley borrow)
        let pill_w = |txt: &str| 2.0 * space::SM + txt.chars().count() as f32 * 7.0;
        let mut x = match kind {
            OptionKind::Call => rect.left() + edges[0] + options_chain::BADGE_START_OFFSET,
            OptionKind::Put => {
                rect.left() + edges[strike_col + 2] + options_chain::BADGE_START_OFFSET
            }
        };
        // position badge — filled+outlined tag, e.g. "+2" / "-1"
        if position_quantity != 0.0 {
            let txt = format_position_quantity(position_quantity);
            let long = position_quantity > 0.0;
            let (col, txt_col) =
                (ThemeSidePair::LongShort.colour(long, t), ThemeSidePair::LongShort.text(long, t));
            let w = pill_w(&txt);
            let r = egui::Rect::from_min_size(
                egui::pos2(x, my - options_chain::MARKER_HEIGHT / 2.0),
                egui::vec2(w, options_chain::MARKER_HEIGHT),
            );
            p.rect_filled(
                r,
                options_chain::MARKER_RADIUS,
                with_alpha(col, options_chain::POSITION_FILL_ALPHA),
            );
            p.rect_stroke(
                r,
                options_chain::MARKER_RADIUS,
                egui::Stroke::new(stroke::HAIRLINE, col),
                StrokeKind::Inside,
            );
            p.text(egui::pos2(x + space::SM, my), Align2::LEFT_CENTER, txt, small.clone(), txt_col);
            x += w + options_chain::MARKER_GAP;
        }
        // working-order marker — clickable; a click cancels the FIRST resting order (repeat
        // to peel them off one at a time). Subtle brighten on hover so it reads as clickable.
        if let Some(first) = resting.first() {
            let n = resting.len();
            let txt = format!("○{n}");
            // The calls' colour on a put's marker too: the marker was always the one series blue.
            let col = maps::callput::CALL.colour.resolve(t);
            // Measured width: the mono role's glyph advance is not the 7px/char the old
            // estimate assumed at 11pt — this stays correct at every density/text size.
            let w = p.layout_no_wrap(txt.clone(), small.clone(), col).size().x + 2.0 * space::SM;
            let r = egui::Rect::from_min_size(
                egui::pos2(x, my - options_chain::MARKER_HEIGHT / 2.0),
                egui::vec2(w, options_chain::MARKER_HEIGHT),
            );
            let response =
                ui.interact(r, ui.scope_id().with(("optcancel", ri, kind)), Sense::click());
            let hot = response.hovered();
            p.rect_filled(
                r,
                options_chain::MARKER_RADIUS,
                with_alpha(
                    col,
                    if hot {
                        options_chain::ORDER_FILL_HOT_ALPHA
                    } else {
                        options_chain::ORDER_FILL_ALPHA
                    },
                ),
            );
            p.rect_stroke(
                r,
                options_chain::MARKER_RADIUS,
                egui::Stroke::new(stroke::HAIRLINE, col),
                StrokeKind::Inside,
            );
            p.text(
                egui::pos2(x + space::SM, my),
                Align2::LEFT_CENTER,
                &txt,
                small.clone(),
                if hot { t.theme.text } else { col },
            );
            if response.clicked() {
                actions.cancel = Some(first.clone());
            }
            if hot {
                response.on_hover_text(format!("Cancel working order {first} ({n} resting)"));
            }
        }
    }
}
