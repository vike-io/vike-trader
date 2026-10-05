//! The order ticket's own controls, drawn as the owner's v3 design draws them (the geometry dump of
//! its capture, the same sizes under the design system's tokens): ONE
//! button that is an outline, a CHOSEN outline (the card fill ringed in the accent) or a filled Buy
//! or Sell, and a number field that reads left to right.
//!
//! # Why the ticket has its own button
//!
//! The kit's `ActionButton` has no chosen state, takes its width from its words, and is one height;
//! the design's ticket needs all three at once — three equal-width order types of which one is
//! chosen, a Buy that is taller than any control, a Reduce only that is pressed — and its words
//! mix faces (`Buy 0.010 limit`: Inter, then a mono number, then Inter) and carry an icon (the
//! check of a pressed toggle, the swap arrows of the unit) or a count (`Cancel all (3)`). It paints
//! with the kit's tokens only — the theme's surface, card, border and accent, the market set's up
//! and down, the density's control height, padding and corner radius — so a theme, a density or a
//! text size reaches it like any kit control, and keeps the kit's three promises: a button is
//! never shorter than the control height, a disabled one SAYS WHY ([`Btn::disabled_because`]), and
//! a keyboard-focused one wears the accent ring.
//!
//! The words a screen reader reads are [`Btn::new`]'s `name`, not the pieces drawn: a pressed
//! toggle draws a check icon and the compact TP/SL draws its two distances, and both are still
//! `TP/SL` to the tree.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Color32, CornerRadius, FontFamily, FontId, Galley, Margin, Response, RichText, Sense,
    Stroke, StrokeKind, Ui, Widget, WidgetInfo, WidgetType, vec2,
};
use vike_ui_theme::components::{ON_FILL, Status, Tokens, input};
use vike_ui_theme::fonts;
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::metrics::RADIUS;
use vike_ui_theme::type_scale::TextRole;

/// How far the pointer lifts a FILLED button, as a share of the text colour laid over its fill. The
/// kit's own filled buttons move their fill 12 % toward the text colour on hover
/// (`vike_ui_theme::components::button`'s `Kind::lit`); this is that, drawn as an overlay because a
/// colour derivation outside the kit is a ratchet row.
const LIFT: f32 = 0.12;

/// How a button is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Look {
    /// An outline: the surface fill, the border stroke.
    Plain,
    /// Chosen or pressed: the card fill, outlined in the accent. A chosen order type, the size
    /// that is the field's, Reduce only while on, the one-click padlock while one-click is on.
    Chosen,
    /// Buy: the market set's up, filled.
    Buy,
    /// Sell: the market set's down, filled.
    Sell,
}

/// A run of a button's words: its text, its face, and its colour (`None` is the look's own).
struct Part {
    text: String,
    font: FontId,
    ink: Option<Color32>,
}

/// A ticket button. `ui.add(Btn::text(&t, "Close"))`; a row of them is given each one's width
/// ([`Btn::width`]) by the caller, who measured them ([`Btn::natural_w`]) before it drew any.
#[must_use = "add it with `ui.add(…)`"]
pub(super) struct Btn<'a> {
    parts: Vec<Part>,
    name: String,
    look: Look,
    toggled: Option<bool>,
    off: Option<&'a str>,
    badge: Option<String>,
    snug: bool,
    tall: bool,
    inline: bool,
    width: Option<f32>,
}

impl<'a> Btn<'a> {
    /// A button a screen reader calls `name`, with nothing drawn on it yet.
    pub fn new(name: impl Into<String>) -> Self {
        Btn {
            parts: Vec::new(),
            name: name.into(),
            look: Look::Plain,
            toggled: None,
            off: None,
            badge: None,
            snug: false,
            tall: false,
            inline: false,
            width: None,
        }
    }

    /// A button that reads `words` in the button face (the Body role: the design's 11 px button).
    pub fn text(t: &Tokens, words: &str) -> Self {
        Btn::new(words).words(t, words)
    }

    /// More words, in the button face.
    pub fn words(self, t: &Tokens, words: &str) -> Self {
        self.part(words, t.font(TextRole::Body), None)
    }

    /// More words, in the button face and `ink`.
    pub fn words_in(self, t: &Tokens, words: &str, ink: Color32) -> Self {
        self.part(words, t.font(TextRole::Body), Some(ink))
    }

    /// A number, in the mono face (the quick sizes, the shares, a TP/SL distance).
    pub fn digits(self, t: &Tokens, digits: &str) -> Self {
        self.part(digits, t.mono(TextRole::Body), None)
    }

    /// An icon, as large as the words beside it, in `ink` (`None`: the look's own).
    pub fn icon(self, t: &Tokens, icon: Icon, ink: Option<Color32>) -> Self {
        let font = FontId::new(t.text.px(TextRole::Body), icons::family());
        self.part(icon.rich().text(), font, ink)
    }

    /// A run of text in any face — what Buy and Sell's semibold faces are drawn through.
    pub fn part(mut self, text: &str, font: FontId, ink: Option<Color32>) -> Self {
        self.parts.push(Part { text: text.to_string(), font, ink });
        self
    }

    /// How it is drawn.
    pub fn look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    /// A TOGGLE in state `on`: drawn chosen while on, and the only kind of button a screen reader
    /// announces as pressed or not.
    pub fn toggled(mut self, on: bool) -> Self {
        self.toggled = Some(on);
        self.look = if on { Look::Chosen } else { Look::Plain };
        self
    }

    /// Disable it and say why on hover — the ONLY way to disable a ticket button, as for the kit's.
    pub fn disabled_because(mut self, why: &'a str) -> Self {
        self.off = Some(why);
        self
    }

    /// A count in a pill after the words (`Cancel all (3)`).
    pub fn badge(mut self, count: impl Into<String>) -> Self {
        self.badge = Some(count.into());
        self
    }

    /// Half the cell padding: the design's padding of a button that GROWS to fill its row, where a
    /// button of its own width has the whole of it.
    pub fn snug(mut self) -> Self {
        self.snug = true;
        self
    }

    /// The tall button, [`tall_h`] high: Buy and Sell.
    pub fn tall(mut self) -> Self {
        self.tall = true;
        self
    }

    /// Its runs of text follow one another as one sentence, spaces and all (`Buy 0.010 limit`).
    /// Without it each run is an item of its own, a gap from the next — the design's buttons are
    /// flex rows: a check and its words, the TP and its price, the unit and its arrows.
    pub fn inline(mut self) -> Self {
        self.inline = true;
        self
    }

    /// Exactly this wide, whatever its words need: a button in a row that fills its width.
    pub fn width(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }

    fn pad_x(&self, t: &Tokens) -> f32 {
        if self.snug { t.metrics.pad / 2.0 } else { t.metrics.pad }
    }

    /// The colour of words that name none: the chosen look reads in the full text colour, a filled
    /// one in the on-fill black.
    fn ink(&self, t: &Tokens) -> Color32 {
        match self.look {
            Look::Plain => t.theme.text_ui,
            Look::Chosen => t.theme.text,
            Look::Buy | Look::Sell => ON_FILL,
        }
    }

    fn laid(&self, ctx: &egui::Context, t: &Tokens) -> Laid {
        let ink = self.ink(t);
        let mut job = LayoutJob::default();
        for (i, p) in self.parts.iter().enumerate() {
            let format = TextFormat {
                font_id: p.font.clone(),
                color: p.ink.unwrap_or(ink),
                valign: Align::Center,
                ..Default::default()
            };
            let before = if i > 0 && !self.inline { t.metrics.gap } else { 0.0 };
            job.append(&p.text, before, format);
        }
        let words = ctx.fonts_mut(|f| f.layout_job(job));
        let badge = self.badge.as_ref().map(|count| {
            let g = ctx.fonts_mut(|f| {
                f.layout_no_wrap(count.clone(), t.mono(TextRole::Caption), t.theme.text)
            });
            let side = badge_h(t);
            // The pill is as wide as its count and never narrower than it is tall: a one-digit count
            // is a circle, a three-digit one a pill.
            let w = (g.size().x + t.metrics.pad - 2.0).max(side);
            (g, w)
        });
        let content_w = words.size().x + badge.as_ref().map_or(0.0, |(_, w)| t.metrics.gap + w);
        Laid { words, badge, content_w }
    }

    /// How wide it is drawn with no width given: its words, its count and the cell padding each
    /// side, and the stroke each side (the design's boxes include their border).
    pub fn natural_w(&self, ctx: &egui::Context, t: &Tokens) -> f32 {
        self.laid(ctx, t).content_w + 2.0 * self.pad_x(t) + 2.0
    }
}

/// What a button's words and count came to when laid out.
struct Laid {
    words: Arc<Galley>,
    badge: Option<(Arc<Galley>, f32)>,
    content_w: f32,
}

/// How tall a count's pill is: the density's row height less two points a side's worth of air.
fn badge_h(t: &Tokens) -> f32 {
    t.metrics.row_h - 4.0
}

/// How tall the tall button (Buy, Sell) is: a control and the cell padding — the design's 32 at the
/// Normal density.
pub(super) fn tall_h(t: &Tokens) -> f32 {
    t.metrics.control_h + t.metrics.pad
}

impl Widget for Btn<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = Tokens::of(ui.ctx());
        let off = self.off;
        let resp = ui
            .scope(|ui| {
                if off.is_some() {
                    ui.disable();
                }
                self.paint(ui, &t)
            })
            .inner;
        match off {
            Some(why) => resp.on_disabled_hover_text(why),
            None => resp,
        }
    }
}

impl Btn<'_> {
    fn paint(self, ui: &mut Ui, t: &Tokens) -> Response {
        let enabled = self.off.is_none();
        let laid = self.laid(ui.ctx(), t);
        let h = if self.tall { tall_h(t) } else { t.metrics.control_h };
        let w = self.width.unwrap_or(laid.content_w + 2.0 * self.pad_x(t) + 2.0);
        let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
        if enabled && resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        // Only what is in view is painted: a form that scrolls holds buttons under its clip, and a
        // text painted there is still a text on the frame (it lies on whatever is drawn below).
        if ui.is_rect_visible(rect) {
            self.paint_face(ui, t, rect, &resp, &laid);
        }
        let (name, toggled) = (&self.name, self.toggled);
        resp.widget_info(|| match toggled {
            Some(on) => WidgetInfo::selected(WidgetType::Button, enabled, on, name),
            None => WidgetInfo::labeled(WidgetType::Button, enabled, name),
        });
        resp
    }

    /// The button's fill, edge, words and count in `rect`, and its focus ring.
    fn paint_face(&self, ui: &Ui, t: &Tokens, rect: egui::Rect, resp: &Response, laid: &Laid) {
        let hot = self.off.is_none() && resp.hovered();
        let round = CornerRadius::same(RADIUS);
        let (fill, stroke) = match self.look {
            Look::Plain => (
                if hot { t.theme.hover } else { t.theme.surface },
                Stroke::new(1.0, t.theme.border),
            ),
            Look::Chosen => (t.theme.card, Stroke::new(1.0, t.theme.accent)),
            Look::Buy => (t.market.up, Stroke::NONE),
            Look::Sell => (t.market.down, Stroke::NONE),
        };
        let p = ui.painter();
        p.rect(rect, round, fill, stroke, StrokeKind::Inside);
        if hot && matches!(self.look, Look::Buy | Look::Sell) {
            let mut over = p.clone();
            over.multiply_opacity(LIFT);
            over.rect_filled(rect, round, t.theme.text);
        }
        // The words, then the count, centred together.
        let mut x = rect.center().x - laid.content_w / 2.0;
        p.galley(
            egui::pos2(x, rect.center().y - laid.words.size().y / 2.0),
            laid.words.clone(),
            t.theme.text,
        );
        if let Some((count, pill_w)) = &laid.badge {
            x += laid.words.size().x + t.metrics.gap;
            let side = badge_h(t);
            let pill = egui::Rect::from_min_size(
                egui::pos2(x, rect.center().y - side / 2.0),
                vec2(*pill_w, side),
            );
            p.rect_filled(pill, CornerRadius::same((side / 2.0) as u8), t.theme.border);
            p.galley(pill.center() - count.size() / 2.0, count.clone(), t.theme.text);
        }
        if resp.has_focus() {
            let ring = Stroke::new(1.0, t.theme.accent);
            let round = CornerRadius::same(RADIUS + 2);
            p.rect_stroke(rect.expand(2.0), round, ring, StrokeKind::Inside);
        }
    }
}

/// Inert text in the Semibold face: Buy and Sell's words (the design's 600 weight).
pub(super) fn semibold(t: &Tokens, role: TextRole) -> FontId {
    FontId::new(t.text.px(role), FontFamily::Name(fonts::SEMIBOLD.into()))
}

/// ...and a number in it.
pub(super) fn mono_semibold(t: &Tokens, role: TextRole) -> FontId {
    FontId::new(t.text.px(role), FontFamily::Name(fonts::MONO_SEMIBOLD.into()))
}

/// A number field `width` wide that reads from the left, in the mono face, on the window's ground
/// with the border for its edge — the accent while it has focus, the status red while what it holds
/// is not a number. Its second answer is whether it is not a number: the caller says so in words
/// under the row, where a message beside the field would take the room of the buttons next to it.
///
/// The kit's `input::number` draws the same field right-aligned with its unit inside it; the
/// design's size and price read from the left and carry their unit on the button beside them.
pub(super) fn number_field(
    ui: &mut Ui,
    t: &Tokens,
    value: &mut String,
    hint: &str,
    width: f32,
) -> (Response, bool) {
    let bad = !value.trim().is_empty() && input::parse_number(value).is_none();
    let resp = ui
        .scope(|ui| {
            // The field's id is taken here, exactly as `TextEdit` would take it, so its focus can
            // be read before it is drawn (the kit's `input` does the same).
            let id = ui.next_auto_id();
            ui.skip_ahead_auto_ids(1);
            let focused = ui.memory(|m| m.has_focus(id));
            let edge = match (bad, focused) {
                (true, _) => Status::Error.color(),
                (false, true) => t.theme.accent,
                (false, false) => t.theme.border,
            };
            // The design's field has 6 px of padding inside a 1 px border.
            let across = (t.metrics.pad - 2.0) as i8;
            let font = t.mono(TextRole::Strong);
            // 2 px above and below the text, less where the row and the 1 px edge leave no room for
            // that inside `control_h`: Large's mono Strong row is 20 and a Normal field is 24, and at a
            // fixed 2 px both the price and the size drew 26, so a window opened for the whole form
            // hid TP/SL 3.9 pt below the fold. (The kit's `input` takes the same rule.)
            let row = ui.ctx().fonts_mut(|f| f.row_height(&font));
            let down = (((t.metrics.control_h - row - 2.0) / 2.0).floor() as i8).clamp(0, 2);
            let frame = egui::Frame::new()
                .fill(t.theme.bg)
                .stroke(Stroke::new(1.0, edge))
                .corner_radius(CornerRadius::same(RADIUS))
                .inner_margin(Margin::symmetric(across, down));
            let edit = egui::TextEdit::singleline(value)
                .id(id)
                .frame(frame)
                .font(font)
                .text_color(t.theme.text)
                .hint_text(RichText::new(hint).color(t.theme.text3))
                .desired_width(width)
                .min_size(vec2(width, t.metrics.control_h))
                .vertical_align(Align::Center);
            ui.add(edit)
        })
        .inner;
    (resp, bad)
}
