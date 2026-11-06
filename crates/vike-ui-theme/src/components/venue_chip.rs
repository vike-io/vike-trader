//! The venue chip and the underlying row — what the symbol picker draws (owner design of
//! 2026-10-04, `docs/superpowers/specs/2026-10-04-symbol-search-design.md`): ONE LINE per
//! underlying (`BTC`, `ETH/BTC`) with a chip for every venue that lists it. The chart window's
//! picker and the Trade window's picker draw the same widget, so they cannot drift apart.
//!
//! The kit knows no catalog: a row takes strings and marks. What a line IS (base, kind, folded
//! quote) is `vike_catalog`'s `Underlying`; this file paints it.
//!
//! # ⚠ The account mode is a DOT here, and that is an owner ruling, not an oversight
//!
//! [`chip::account`](super::chip::account) draws the mode as a pill and says "never a coloured
//! dot", because the mode must not rest on colour alone. The owner looked at
//! the pill and the dot on every theme and chose the dot INSIDE the picker, where a pill per chip
//! made each row "spam". `account` itself is unchanged and keeps its test. The dot's SHAPE carries
//! the mode, so it never rests on colour alone: LIVE is a filled disc, DEMO and PAPER are rings, in
//! the colours of each mode's `[[mode]]` row of `ui-theme.toml` (`crate::roles`), and a chip with no
//! account is DASHED.
//! The mark is small, which is why [`VenueChip::why`] spells the mode in WORDS as the chip's hover text and its
//! accessible name: the mark is never the only carrier.

use egui::{
    CornerRadius, FontFamily, FontId, Pos2, Rect, Response, Role, Sense, Shape, Stroke, StrokeKind,
    Ui, Vec2, WidgetInfo, pos2, vec2,
};
use vike_model::AccountTier;

use super::chip::{self, TIERS_LISTED};
use super::{Tokens, focus_ring};
use crate::fonts::SEMIBOLD;
use crate::metrics::{RADIUS, stroke};
use crate::roles::MarkShape;
use crate::type_scale::TextRole;

/// What a venue chip says about the account an order would go to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Nothing: the chart window's chips, where no order is sent.
    Hidden,
    /// No account on this venue: a dashed outline, view only.
    NoAccount,
    /// The account's tier, as a dot whose shape carries it.
    Tier(AccountTier),
}

/// One venue on a line.
#[derive(Clone, Copy, Debug)]
pub struct VenueChip<'a> {
    /// The venue as the settings database spells it; drawn upper-cased.
    pub label: &'a str,
    pub mark: Mark,
    /// The venue this window is on: outlined in the accent, and a Label rather than a Button (a
    /// selected item is a Label, spec §4.2).
    pub current: bool,
    /// A public venue whose list is not loaded: dashed, and a click asks to load it.
    pub cold: bool,
    /// What the chip does, with the mode in WORDS ("Bybit, DEMO account"): the hover text and the
    /// accessible name.
    pub why: &'a str,
}

/// Which part of a row was clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowPick {
    /// The line itself, not one of its chips.
    Row,
    /// The chip at this index of [`UnderlyingRow::chips`].
    Chip(usize),
}

/// One line of the picker.
#[derive(Clone, Copy, Debug)]
pub struct UnderlyingRow<'a> {
    pub heading: &'a str,
    /// The kind in words (`spot`, `perpetual`).
    pub kind: &'a str,
    pub chips: &'a [VenueChip<'a>],
    /// The keyboard's line: filled with the hover colour.
    pub hot: bool,
    /// The line holding the window's own instrument: a 2 pt accent edge on its left.
    pub current: bool,
}

fn has_dot(c: &VenueChip<'_>) -> bool {
    matches!(c.mark, Mark::Tier(_))
}

fn dashed(c: &VenueChip<'_>) -> bool {
    c.cold || c.mark == Mark::NoAccount
}

/// The mark's diameter: a little over half the caption's height.
fn dot_diameter(t: &Tokens) -> f32 {
    (t.text.px(TextRole::Caption) * 0.6).round().max(5.0)
}

/// The padding a chip leaves each side of its words: three quarters of the density cell padding,
/// to a whole point. A caller that sizes a chip it does not paint (a header width reserve for a
/// badge) asks here rather than repeating the formula, so the reserve follows the chip.
pub fn chip_pad(t: &Tokens) -> f32 {
    (t.metrics.pad * 0.75).round()
}

fn chip_galley(ui: &Ui, t: &Tokens, c: &VenueChip<'_>) -> std::sync::Arc<egui::Galley> {
    let font = FontId::new(t.text.px(TextRole::Caption), FontFamily::Name(SEMIBOLD.into()));
    let ink = if c.current { t.theme.text } else { t.theme.text2 };
    ui.painter().layout_no_wrap(c.label.to_uppercase(), font, ink)
}

fn chip_size(t: &Tokens, c: &VenueChip<'_>, text_w: f32) -> Vec2 {
    let mark_w = if has_dot(c) { dot_diameter(t) + t.metrics.gap } else { 0.0 };
    vec2(2.0 * chip_pad(t) + mark_w + text_w, t.metrics.row_h)
}

/// The mark: the shape and the colour role `tier`'s `[[mode]]` row of `ui-theme.toml` gives it, resolved
/// against `t` — a disc or a ring, so the shape carries the tier and the colour only helps.
fn paint_mark(p: &egui::Painter, t: &Tokens, at: Pos2, tier: AccountTier, d: f32) {
    let r = d / 2.0;
    let row = chip::tier_roles(tier);
    let colour = row.mark_colour.resolve(t);
    match row.mark {
        MarkShape::Filled => p.circle_filled(at, r, colour),
        MarkShape::Ring => p.circle_stroke(at, r - 0.75, Stroke::new(stroke::LINE, colour)),
    };
}

/// Paint one chip into `rect`. `hovered` lifts a clickable chip's fill.
fn paint_chip(
    ui: &Ui,
    t: &Tokens,
    rect: Rect,
    c: &VenueChip<'_>,
    galley: std::sync::Arc<egui::Galley>,
    hovered: bool,
) {
    let fill = if c.current {
        t.theme.card
    } else if hovered {
        t.theme.hover
    } else {
        t.theme.surface
    };
    let edge = if c.current { t.theme.accent } else { t.theme.border };
    let p = ui.painter();
    let round = CornerRadius::same(RADIUS);
    if dashed(c) {
        p.rect_filled(rect, round, fill);
        let r = rect.shrink(0.5);
        let outline =
            [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
        p.extend(Shape::dashed_line(&outline, Stroke::new(stroke::HAIRLINE, edge), 3.0, 2.0));
    } else {
        p.rect(rect, round, fill, Stroke::new(stroke::HAIRLINE, edge), StrokeKind::Inside);
    }
    let mut x = rect.left() + chip_pad(t);
    if let Mark::Tier(tier) = c.mark {
        let d = dot_diameter(t);
        paint_mark(p, t, pos2(x + d / 2.0, rect.center().y), tier, d);
        x += d + t.metrics.gap;
    }
    p.galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, t.theme.text2);
}

/// The chip's interaction and its accessibility: a Label when current, else a Button named by
/// `why`.
fn finish_chip(ui: &Ui, t: &Tokens, resp: Response, c: &VenueChip<'_>) -> Response {
    focus_ring(ui, t, &resp);
    let name = if c.why.is_empty() { c.label } else { c.why };
    let typ = if c.current { Role::Label } else { Role::Button };
    resp.widget_info(|| WidgetInfo::labeled(typ, true, name));
    if c.why.is_empty() { resp } else { resp.on_hover_text(c.why) }
}

/// A venue chip on its own, allocated where the layout is.
pub fn venue_chip(ui: &mut Ui, c: &VenueChip<'_>) -> Response {
    let t = Tokens::of(ui.ctx());
    let galley = chip_galley(ui, &t, c);
    let size = chip_size(&t, c, galley.size().x);
    let sense = if c.current { Sense::hover() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    paint_chip(ui, &t, rect, c, galley, resp.hovered());
    finish_chip(ui, &t, resp, c)
}

/// How the chips flow: `None` when they all fit beside the heading on one line, else the chips of
/// each line (left-aligned under the heading), so a line with every roster venue wraps instead of
/// clipping off the popup's edge.
fn flow(sizes: &[Vec2], gap: f32, width: f32) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 0.0_f32;
    for (i, s) in sizes.iter().enumerate() {
        let need = if lines.last().is_some_and(|l| l.is_empty()) { s.x } else { gap + s.x };
        if used + need > width && !lines.last().is_some_and(|l| l.is_empty()) {
            lines.push(Vec::new());
            used = s.x;
        } else {
            used += need;
        }
        if let Some(last) = lines.last_mut() {
            last.push(i);
        }
    }
    lines
}

/// One line of the picker: the heading, its kind, and a chip per venue, right-aligned on the same
/// line when they fit and flowed underneath when they do not. Answers which part was clicked; a
/// chip click is [`RowPick::Chip`] and never also [`RowPick::Row`].
pub fn underlying_row(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    row: &UnderlyingRow<'_>,
) -> Option<RowPick> {
    let t = Tokens::of(ui.ctx());
    let gap = t.metrics.gap;
    let head =
        ui.painter().layout_no_wrap(row.heading.to_string(), t.mono(TextRole::Title), t.theme.text);
    let kind =
        ui.painter().layout_no_wrap(row.kind.to_string(), t.font(TextRole::Body), t.theme.text2);
    let galleys: Vec<_> = row.chips.iter().map(|c| chip_galley(ui, &t, c)).collect();
    let sizes: Vec<Vec2> =
        row.chips.iter().zip(&galleys).map(|(c, g)| chip_size(&t, c, g.size().x)).collect();

    let avail = ui.available_width();
    let left_w = t.metrics.pad + head.size().x + 2.0 * gap + kind.size().x + gap;
    let chips_w: f32 =
        sizes.iter().map(|s| s.x).sum::<f32>() + gap * sizes.len().saturating_sub(1) as f32;
    let one_line = left_w + chips_w + t.metrics.pad <= avail;
    let lines = if one_line {
        vec![(0..sizes.len()).collect()]
    } else {
        flow(&sizes, gap, avail - 2.0 * t.metrics.pad)
    };
    let band = t.metrics.control_h;
    let h = if one_line { band } else { band + lines.len() as f32 * (t.metrics.row_h + 2.0) + 4.0 };

    // The row's identity is the CALLER's key, not its place in the list: a list that changes under
    // a held press (a venue loads, a line drops out) must not hand the press to another line.
    let (_, rect) = ui.allocate_space(vec2(avail, h));
    let row_resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    let round = CornerRadius::same(RADIUS);
    if row.hot || row_resp.hovered() {
        ui.painter().rect_filled(rect, round, t.theme.hover);
    }
    if row.current {
        let edge = Rect::from_min_max(
            pos2(rect.left(), rect.top() + 3.0),
            pos2(rect.left() + 2.0, rect.bottom() - 3.0),
        );
        ui.painter().rect_filled(edge, CornerRadius::same(1), t.theme.accent);
    }
    let head_y = rect.top() + band / 2.0;
    let x = rect.left() + t.metrics.pad;
    ui.painter().galley(pos2(x, head_y - head.size().y / 2.0), head.clone(), t.theme.text);
    let kind_x = x + head.size().x + 2.0 * gap;
    ui.painter().galley(pos2(kind_x, head_y - kind.size().y / 2.0), kind, t.theme.text2);

    let mut pick = None;
    for (n, line) in lines.iter().enumerate() {
        let y = if one_line {
            rect.top() + (band - t.metrics.row_h) / 2.0
        } else {
            rect.top() + band + n as f32 * (t.metrics.row_h + 2.0)
        };
        let line_w: f32 = line.iter().map(|&i| sizes[i].x).sum::<f32>()
            + gap * line.len().saturating_sub(1) as f32;
        let mut cx = if one_line {
            rect.right() - t.metrics.pad - line_w
        } else {
            rect.left() + t.metrics.pad
        };
        for &i in line {
            let r = Rect::from_min_size(pos2(cx, y), sizes[i]);
            let c = &row.chips[i];
            let sense = if c.current { Sense::hover() } else { Sense::click() };
            let resp = ui.interact(r, row_resp.id.with(("chip", i)), sense);
            paint_chip(ui, &t, r, c, galleys[i].clone(), resp.hovered());
            let resp = finish_chip(ui, &t, resp, c);
            if resp.clicked() {
                pick = Some(RowPick::Chip(i));
            }
            cx += sizes[i].x + gap;
        }
    }
    focus_ring(ui, &t, &row_resp);
    let spoken = format!("{} {}", row.heading, row.kind);
    row_resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &spoken));
    pick.or_else(|| row_resp.clicked().then_some(RowPick::Row))
}

/// The legend the picker prints under its lines: each tier's own mark with its word, and what a
/// dashed chip says. The marks are painted by the same function the chips use, so the legend cannot
/// show a mark the chips do not draw.
pub fn mark_legend(ui: &mut Ui) {
    let t = Tokens::of(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = t.metrics.gap;
        let d = dot_diameter(&t);
        for tier in TIERS_LISTED {
            let (rect, _) = ui.allocate_exact_size(vec2(d, d), Sense::hover());
            paint_mark(ui.painter(), &t, rect.center(), tier, d);
            ui.label(
                egui::RichText::new(chip::tier_label(tier))
                    .font(t.font(TextRole::Caption))
                    .color(t.theme.text3),
            );
        }
        ui.label(
            egui::RichText::new("dashed = no account, view only")
                .font(t.font(TextRole::Caption))
                .color(t.theme.text3),
        );
    });
}

/// A source chip, the picker's venue FILTER: outlined in the accent while ON (a toggle, so it stays
/// clickable and is announced as a checkbox, never as a Label), and DASHED when the venue's list is
/// not loaded (a click asks to load it, and it is a plain button). `why` is the hover text and the
/// accessible name.
pub fn source_chip(ui: &mut Ui, label: &str, on: bool, cold: bool, why: &str) -> Response {
    let t = Tokens::of(ui.ctx());
    let c = VenueChip { label, mark: Mark::Hidden, current: on && !cold, cold, why };
    let galley = chip_galley(ui, &t, &c);
    let size = chip_size(&t, &c, galley.size().x);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    paint_chip(ui, &t, rect, &c, galley, resp.hovered());
    focus_ring(ui, &t, &resp);
    let name = if why.is_empty() { label } else { why };
    if cold {
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, name));
    } else {
        resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, on, name));
    }
    if why.is_empty() { resp } else { resp.on_hover_text(why) }
}

/// One row of the Trade picker's flat list (the owner's v3 design): the symbol in mono, its pair
/// beside it, and the venue at the right — one row per instrument and venue, so a symbol on many
/// venues never overflows a line. The picker fetches no price for a row (ruled 2026-10-04).
#[derive(Clone, Copy, Debug)]
pub struct FlatRow<'a> {
    pub symbol: &'a str,
    /// The pair and kind in words (`BTC/USDT perpetual`); clipped, never wrapped.
    pub pair: &'a str,
    /// The venue as the settings database spells it; drawn upper-cased.
    pub venue: &'a str,
    /// The keyboard's row.
    pub hot: bool,
    /// The window's own instrument: a 2 pt accent edge.
    pub current: bool,
}

/// Draw a flat row; the response is the click on it. Its accessible name is "symbol, pair, venue".
pub fn flat_row(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    row: &FlatRow<'_>,
) -> Response {
    let t = Tokens::of(ui.ctx());
    let (_, rect) = ui.allocate_space(vec2(ui.available_width(), t.metrics.control_h));
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    let p = ui.painter();
    if row.hot || resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(RADIUS), t.theme.hover);
    }
    if row.current {
        let edge = Rect::from_min_max(
            pos2(rect.left(), rect.top() + 3.0),
            pos2(rect.left() + 2.0, rect.bottom() - 3.0),
        );
        p.rect_filled(edge, CornerRadius::same(1), t.theme.accent);
    }
    let pad = t.metrics.pad;
    let y = rect.center().y;
    let venue_font = FontId::new(t.text.px(TextRole::Caption), FontFamily::Name(SEMIBOLD.into()));
    let venue = p.layout_no_wrap(row.venue.to_uppercase(), venue_font, t.theme.text3);
    let venue_x = rect.right() - pad - venue.size().x;
    // The symbol takes up to 40% of the row, the pair the room up to the venue; each is clipped at
    // its column rather than wrapped (a long symbol is `BTC-USDT-SWAP`, a long pair a long name).
    let sym_w = (rect.width() * 0.40).max(80.0);
    let clip = |left: f32, right: f32| {
        p.with_clip_rect(Rect::from_min_max(pos2(left, rect.top()), pos2(right, rect.bottom())))
    };
    let symbol = p.layout_no_wrap(row.symbol.to_string(), t.mono(TextRole::Strong), t.theme.text);
    clip(rect.left() + pad, rect.left() + pad + sym_w - gap(&t)).galley(
        pos2(rect.left() + pad, y - symbol.size().y / 2.0),
        symbol,
        t.theme.text,
    );
    let pair = p.layout_no_wrap(row.pair.to_string(), t.font(TextRole::Body), t.theme.text2);
    let pair_x = rect.left() + pad + sym_w;
    clip(pair_x, venue_x - gap(&t)).galley(
        pos2(pair_x, y - pair.size().y / 2.0),
        pair,
        t.theme.text2,
    );
    p.galley(pos2(venue_x, y - venue.size().y / 2.0), venue, t.theme.text3);
    focus_ring(ui, &t, &resp);
    let name = format!("{}, {}, {}", row.symbol, row.pair, row.venue);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    resp
}

fn gap(t: &Tokens) -> f32 {
    t.metrics.gap
}

#[cfg(test)]
mod tests;
