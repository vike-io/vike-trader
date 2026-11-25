//! The toolbar above the ladder's rows: the Vol toggle at the left beside the ticket, the STALE
//! badge, and the grouping's two buttons and Recenter at the right, each drawn on the design's own
//! grid.

use super::rows::{next_group, previous_group};
use super::{VOLUME_HIDE, VOLUME_SHOW};
use crate::trade_window::TradeWindowState;
use egui::{
    Align, Color32, CornerRadius, FontId, Id, Layout, Painter, Pos2, Rect, Response, RichText,
    Role, Sense, Stroke, StrokeKind, UiBuilder, WidgetInfo, pos2, vec2,
};
use vike_ui_theme::components::venue_chip::chip_pad;
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, chip};
use vike_ui_theme::metrics::{RADIUS, ThemeDensityMetrics, space, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::trade;

// The toolbar's icon button is `trade::ICON_WIDTH` wide, and `trade::ICON_SHRINK` shorter than a control, in
// the design (24 × 20 at the normal density); the grouping's squares are `trade::SQUARE_SHRINK` smaller
// than a control. What a button DRAWS is that size; what it takes a click on, and what the
// accessibility tree reports, is the whole `control_height` (spec §7: every click target keeps the control
// height).

/// The space between the toolbar's controls; the word `Group` keeps `space::XS` more from the
/// button after it, and Recenter `space::SM` more from the one before it.
const TOOL_GAP: f32 = space::XS;

// The grouping's number sits in a box at least `trade::GROUP_NUMBER_WIDTH` wide, so `1` and `10` do not move
// the buttons beside it.

/// A toolbar button's glyph, drawn on the design's own grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TradeWindowLadderToolbarGlyph {
    /// Three bars: the Vol column.
    Bars,
    /// A crosshair: recenter.
    Cross,
    Minus,
    Plus,
}

/// A straight horizontal or vertical stroke with round ends, as the design's `stroke-linecap: round`
/// draws it: a rounded rectangle, because egui's line segments are anti-aliased thinner than their
/// width and their round caps are separate dots.
fn stroke_round(painter: &Painter, from: Pos2, to: Pos2, width: f32, ink: Color32) {
    let body = Rect::from_two_pos(from, to).expand(width / 2.0);
    painter.rect_filled(body, CornerRadius::same(trade::GLYPH_CAP_RADIUS), ink);
}

impl TradeWindowLadderToolbarGlyph {
    /// Whether the button is drawn with a permanent border (the grouping's squares) rather than
    /// bare until it is pressed or pointed at (the icon buttons).
    pub(super) fn framed(self) -> bool {
        matches!(self, TradeWindowLadderToolbarGlyph::Minus | TradeWindowLadderToolbarGlyph::Plus)
    }

    /// Draw the glyph centred on `at`. The icon buttons' are the design's 12 × 12 drawings; the
    /// squares' are its 10 × 10 ones at 8 × 8.
    pub(super) fn paint(self, painter: &Painter, at: Pos2, ink: Color32) {
        let o = at - vec2(6.0, 6.0);
        let p = |x: f32, y: f32| o + vec2(x, y);
        match self {
            TradeWindowLadderToolbarGlyph::Bars => {
                for (x, from, to) in [(3.0, 10.0, 6.0), (6.0, 10.0, 3.0), (9.0, 10.0, 7.0)] {
                    stroke_round(painter, p(x, from), p(x, to), stroke::LINE, ink);
                }
            }
            TradeWindowLadderToolbarGlyph::Cross => {
                painter.circle_stroke(p(6.0, 6.0), 3.5, Stroke::new(trade::CROSS_GLYPH_WIDTH, ink));
                for (a, b) in [
                    ((6.0, 0.8), (6.0, 2.8)),
                    ((6.0, 9.2), (6.0, 11.2)),
                    ((0.8, 6.0), (2.8, 6.0)),
                    ((9.2, 6.0), (11.2, 6.0)),
                ] {
                    stroke_round(painter, p(a.0, a.1), p(b.0, b.1), trade::CROSS_GLYPH_WIDTH, ink);
                }
            }
            TradeWindowLadderToolbarGlyph::Minus | TradeWindowLadderToolbarGlyph::Plus => {
                let q = |x: f32, y: f32| at + vec2(x - 4.0, y - 4.0);
                stroke_round(painter, q(1.6, 4.0), q(6.4, 4.0), trade::PLUS_MINUS_GLYPH_WIDTH, ink);
                if self == TradeWindowLadderToolbarGlyph::Plus {
                    stroke_round(
                        painter,
                        q(4.0, 1.6),
                        q(4.0, 6.4),
                        trade::PLUS_MINUS_GLYPH_WIDTH,
                        ink,
                    );
                }
            }
        }
    }
}

/// One toolbar button: `hit` is what takes the click and what the accessibility tree reports,
/// `face` what is drawn in it. Its `name` is both what a screen reader reads and what the pointer
/// is told on hover, one string. `selected` makes it a TOGGLE, drawn pressed — the card fill
/// outlined in the accent — while on (the design's `aria-pressed`).
///
/// Its id is the caller's `key`, never its place: the toolbar's pieces come and go with the room
/// STALE takes, and a press held across that must still be a press on this button.
fn tool_button(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    key: Id,
    (hit, face): (Rect, Rect),
    name: &str,
    glyph: TradeWindowLadderToolbarGlyph,
    selected: Option<bool>,
) -> Response {
    let response = ui.interact(hit, key, Sense::click());
    response.widget_info(|| match selected {
        Some(on) => WidgetInfo::selected(Role::Button, true, on, name),
        None => WidgetInfo::labeled(Role::Button, true, name),
    });
    let (on, hot) = (selected == Some(true), response.hovered());
    let round = CornerRadius::same(RADIUS);
    let (fill, edge, ink) = if on {
        (t.theme.card, Stroke::new(stroke::HAIRLINE, t.theme.accent), t.theme.text)
    } else if glyph.framed() {
        let fill = if hot { t.theme.hover } else { Color32::TRANSPARENT };
        (fill, Stroke::new(stroke::HAIRLINE, t.theme.border), t.theme.text_secondary)
    } else if hot {
        (t.theme.hover, Stroke::NONE, t.theme.text_ui)
    } else {
        (Color32::TRANSPARENT, Stroke::NONE, t.theme.text_secondary)
    };
    let painter = ui.painter();
    painter.rect(face, round, fill, edge, StrokeKind::Inside);
    glyph.paint(painter, face.center(), ink);
    if response.has_focus() {
        // The keyboard-focus ring every focusable kit widget draws (spec §2).
        let ring = Stroke::new(stroke::HAIRLINE, t.theme.accent);
        let round = CornerRadius::same(RADIUS + 2);
        painter.rect_stroke(face.expand(space::XS), round, ring, StrokeKind::Inside);
    }
    response.on_hover_text(name)
}

/// Where the toolbar's pieces sit, from the band they are in and the widths of their words: the
/// design's layout, the Vol button at the left, the rest set against the right edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TradeWindowLadderToolbarGeometry {
    /// The Vol toggle's click target; `None` under the ticket, where there is no Vol column.
    pub(super) volume: Option<Rect>,
    /// Where STALE may begin: right of the Vol button.
    pub(super) left_end: f32,
    pub(super) less: Rect,
    /// The grouping's number box.
    pub(super) number: Rect,
    pub(super) more: Rect,
    pub(super) recenter: Rect,
    /// The word `Group`, where it fits to the right of everything on the left (`left_end` plus
    /// what STALE takes, `stale_w`); `None` where it does not.
    pub(super) word: Option<Rect>,
}

impl TradeWindowLadderToolbarGeometry {
    pub(super) fn new(
        band: Rect,
        m: &ThemeDensityMetrics,
        beside: bool,
        stale_w: f32,
        number_w: f32,
        word_w: f32,
    ) -> Self {
        let hit = |left: f32, width: f32| {
            Rect::from_min_size(pos2(left, band.min.y), vec2(width, band.height()))
        };
        let square = m.control_height - trade::SQUARE_SHRINK;
        let volume = beside.then(|| hit(band.min.x, trade::ICON_WIDTH));
        let left_end = volume.map_or(band.min.x, |v| v.max.x + TOOL_GAP);
        let mut x = band.max.x;
        let recenter = hit(x - trade::ICON_WIDTH, trade::ICON_WIDTH);
        x -= trade::ICON_WIDTH + space::SM + TOOL_GAP;
        let more = hit(x - square, square);
        x -= square + TOOL_GAP;
        let number = hit(x - number_w, number_w);
        x -= number_w + TOOL_GAP;
        let less = hit(x - square, square);
        x -= square + TOOL_GAP + space::XS;
        let word = hit(x - word_w, word_w);
        let word = (word.min.x >= left_end + stale_w + TOOL_GAP).then_some(word);
        TradeWindowLadderToolbarGeometry { volume, left_end, less, number, more, recenter, word }
    }
}

/// The toolbar above the rows: the Vol toggle at the left (beside the ticket), STALE when the book
/// is, and at the right the price grouping and recenter.
pub(super) fn toolbar(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    rect: Rect,
    stale: bool,
    beside: bool,
) {
    let m = t.metrics;
    let line = Rect::from_min_max(pos2(rect.min.x, rect.max.y - stroke::HAIRLINE), rect.max);
    ui.painter().rect_filled(line, 0.0, t.theme.hover);
    let band = Rect::from_min_max(rect.min, pos2(rect.max.x, line.min.y));
    let painter = ui.painter().clone();
    let width_of = |text: &str, font: FontId| {
        painter.layout_no_wrap(text.to_string(), font, t.theme.text).size().x
    };
    let group = state.group.to_string();
    let number_font = t.mono(ThemeTextRole::Body);
    let number_text_w = width_of(&group, number_font.clone());
    let word_font = t.font(ThemeTextRole::Caption);
    // STALE is as wide as the kit's badge: its words and the padding the badge puts each side.
    let stale_w = if stale {
        width_of("STALE", t.mono(ThemeTextRole::Caption)) + 2.0 * chip_pad(t)
    } else {
        0.0
    };
    // The word `Group` goes first where the row is short of room (a ladder at its narrowest, a
    // loose look, STALE showing): the − and + say what they do on hover, and STALE must never be
    // the one pushed out of the ladder (MEASURED by the fit sweep: into the ticket beside it).
    let g = TradeWindowLadderToolbarGeometry::new(
        band,
        &m,
        beside,
        stale_w,
        number_text_w.max(trade::GROUP_NUMBER_WIDTH),
        width_of("Group", word_font.clone()),
    );
    let key = ui.scope_id().with("trade_toolbar");
    let face = |hit: Rect, w: f32, h: f32| Rect::from_center_size(hit.center(), vec2(w, h));
    let (icon_h, square) =
        (m.control_height - trade::ICON_SHRINK, m.control_height - trade::SQUARE_SHRINK);

    if let Some(hit) = g.volume {
        let name = if state.volume { VOLUME_HIDE } else { VOLUME_SHOW };
        let at = (hit, face(hit, trade::ICON_WIDTH, icon_h));
        if tool_button(
            ui,
            t,
            key.with("volume"),
            at,
            name,
            TradeWindowLadderToolbarGlyph::Bars,
            Some(state.volume),
        )
        .clicked()
        {
            state.volume = !state.volume;
        }
    }
    if stale {
        let mut badge = ui.new_child(
            UiBuilder::new()
                .scope_id(key.with("stale"))
                .max_rect(Rect::from_min_max(pos2(g.left_end, band.min.y), band.max))
                .layout(Layout::left_to_right(Align::Center)),
        );
        chip::badge(&mut badge, "STALE", ThemeStatus::Warning);
    }
    // A label of its own id, never its place, for the same reason as the buttons.
    let label = |ui: &mut egui::Ui, salt: &str, rect: Rect, text: RichText| {
        let mut child = ui.new_child(
            UiBuilder::new()
                .scope_id(key.with(salt))
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        child.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend));
    };
    if let Some(word) = g.word {
        label(
            ui,
            "group_word",
            word,
            RichText::new("Group").font(word_font).color(t.theme.text_tertiary),
        );
    }
    let at = |hit: Rect| (hit, face(hit, square, square));
    if tool_button(
        ui,
        t,
        key.with("group_less"),
        at(g.less),
        GROUP_LESS,
        TradeWindowLadderToolbarGlyph::Minus,
        None,
    )
    .clicked()
    {
        state.group = previous_group(state.group);
        state.group_chosen = true;
    }
    let text = RichText::new(group).font(number_font).color(t.theme.text);
    let number = Rect::from_center_size(g.number.center(), vec2(number_text_w, g.number.height()));
    label(ui, "group_number", number, text);
    if tool_button(
        ui,
        t,
        key.with("group_more"),
        at(g.more),
        GROUP_MORE,
        TradeWindowLadderToolbarGlyph::Plus,
        None,
    )
    .clicked()
    {
        state.group = next_group(state.group);
        state.group_chosen = true;
    }
    // The rows follow the book's mid until scrolled; this un-latches them.
    let at = (g.recenter, face(g.recenter, trade::ICON_WIDTH, icon_h));
    if tool_button(
        ui,
        t,
        key.with("recenter"),
        at,
        RECENTER,
        TradeWindowLadderToolbarGlyph::Cross,
        None,
    )
    .clicked()
    {
        state.center = None;
        state.anchor = None;
    }
}

/// The names of the grouping's two buttons and of Recenter: what a screen reader reads and what
/// the pointer is told.
pub(crate) const GROUP_LESS: &str = "Less price grouping";
pub(crate) const GROUP_MORE: &str = "More price grouping";
pub(crate) const RECENTER: &str = "Recenter on the market";
