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
use vike_ui_theme::components::{Status, Tokens, input};
use vike_ui_theme::fonts;
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::maps::{self, MapRow};
use vike_ui_theme::metrics::{RADIUS, alpha, space, stroke};
use vike_ui_theme::roles::ColourRole;
use vike_ui_theme::type_scale::TextRole;

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

impl Look {
    /// The row of `ui-theme.toml`'s `button` map this look wears — the kit's own table, which the
    /// kit's `ActionButton` reads too: a plain button is the kit's secondary one, Buy and Sell are
    /// the kit's, and the chosen look is the one row the kit has no kind for. Exhaustive: a look
    /// without a row does not compile.
    fn row(self) -> &'static MapRow {
        match self {
            Look::Plain => &maps::button::SECONDARY,
            Look::Chosen => &maps::button::CHOSEN,
            Look::Buy => &maps::button::BUY,
            Look::Sell => &maps::button::SELL,
        }
    }

    /// `(fill, outline)` of the look's button: the row's `fill` and `stroke` (a hairline, or none);
    /// under the pointer (`hot`) the fill its `colour` names, where it names one — the plain button
    /// turns to the theme's hover fill, the chosen look and the filled ones name none.
    fn face(self, hot: bool, t: &Tokens) -> (Color32, Stroke) {
        let row = self.row();
        let fill = if hot && row.colour != ColourRole::None { row.colour } else { row.fill };
        (fill.resolve(t), row.stroke.stroke(stroke::HAIRLINE, t))
    }
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
    pub(crate) fn new(name: impl Into<String>) -> Self {
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
    pub(crate) fn text(t: &Tokens, words: &str) -> Self {
        Btn::new(words).words(t, words)
    }

    /// More words, in the button face.
    pub(crate) fn words(self, t: &Tokens, words: &str) -> Self {
        self.part(words, t.font(TextRole::Body), None)
    }

    /// More words, in the button face and `ink`.
    pub(crate) fn words_in(self, t: &Tokens, words: &str, ink: Color32) -> Self {
        self.part(words, t.font(TextRole::Body), Some(ink))
    }

    /// A number, in the mono face (the quick sizes, the shares, a TP/SL distance).
    pub(crate) fn digits(self, t: &Tokens, digits: &str) -> Self {
        self.part(digits, t.mono(TextRole::Body), None)
    }

    /// An icon, as large as the words beside it, in `ink` (`None`: the look's own).
    pub(crate) fn icon(self, t: &Tokens, icon: Icon, ink: Option<Color32>) -> Self {
        let font = FontId::new(t.text.px(TextRole::Body), icons::family());
        self.part(icon.rich().text(), font, ink)
    }

    /// A run of text in any face — what Buy and Sell's semibold faces are drawn through.
    pub(crate) fn part(mut self, text: &str, font: FontId, ink: Option<Color32>) -> Self {
        self.parts.push(Part { text: text.to_string(), font, ink });
        self
    }

    /// How it is drawn.
    pub(crate) fn look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    /// A TOGGLE in state `on`: drawn chosen while on, and the only kind of button a screen reader
    /// announces as pressed or not.
    pub(crate) fn toggled(mut self, on: bool) -> Self {
        self.toggled = Some(on);
        self.look = if on { Look::Chosen } else { Look::Plain };
        self
    }

    /// Disable it and say why on hover — the ONLY way to disable a ticket button, as for the kit's.
    pub(crate) fn disabled_because(mut self, why: &'a str) -> Self {
        self.off = Some(why);
        self
    }

    /// A count in a pill after the words (`Cancel all (3)`).
    pub(crate) fn badge(mut self, count: impl Into<String>) -> Self {
        self.badge = Some(count.into());
        self
    }

    /// Half the cell padding: the design's padding of a button that GROWS to fill its row, where a
    /// button of its own width has the whole of it.
    pub(crate) fn snug(mut self) -> Self {
        self.snug = true;
        self
    }

    /// The tall button, [`tall_h`] high: Buy and Sell.
    pub(crate) fn tall(mut self) -> Self {
        self.tall = true;
        self
    }

    /// Its runs of text follow one another as one sentence, spaces and all (`Buy 0.010 limit`).
    /// Without it each run is an item of its own, a gap from the next — the design's buttons are
    /// flex rows: a check and its words, the TP and its price, the unit and its arrows.
    pub(crate) fn inline(mut self) -> Self {
        self.inline = true;
        self
    }

    /// Exactly this wide, whatever its words need: a button in a row that fills its width.
    pub(crate) fn width(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }

    fn pad_x(&self, t: &Tokens) -> f32 {
        if self.snug { t.metrics.pad / 2.0 } else { t.metrics.pad }
    }

    /// The colour of words that name none: the look's own label — the full text colour on the
    /// chosen look, the on-fill black on a filled one.
    fn ink(&self, t: &Tokens) -> Color32 {
        self.look.row().text.resolve(t)
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
    pub(crate) fn natural_w(&self, ctx: &egui::Context, t: &Tokens) -> f32 {
        self.laid(ctx, t).content_w + 2.0 * self.pad_x(t) + 2.0 * stroke::HAIRLINE
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
    t.metrics.row_h - 2.0 * space::XS
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
        let w = self.width.unwrap_or(laid.content_w + 2.0 * self.pad_x(t) + 2.0 * stroke::HAIRLINE);
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
        let (fill, stroke) = self.look.face(hot, t);
        let p = ui.painter();
        p.rect(rect, round, fill, stroke, StrokeKind::Inside);
        if hot && matches!(self.look, Look::Buy | Look::Sell) {
            // The pointer lifts a FILLED button by `alpha::LIFT`, as a share of the text colour laid
            // over its fill: what the kit's own filled buttons do on hover
            // (`vike_ui_theme::components::button`'s `Kind::lit`), drawn as an overlay because a
            // colour derivation outside the kit is a ratchet row.
            let mut over = p.clone();
            over.multiply_opacity(alpha::LIFT);
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
            let ring = Stroke::new(stroke::HAIRLINE, t.theme.accent);
            let round = CornerRadius::same(RADIUS + 2);
            p.rect_stroke(rect.expand(space::XS), round, ring, StrokeKind::Inside);
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
            let down = (((t.metrics.control_h - row - 2.0 * stroke::HAIRLINE) / 2.0).floor() as i8)
                .clamp(0, 2);
            let frame = egui::Frame::new()
                .fill(t.theme.bg)
                .stroke(Stroke::new(stroke::HAIRLINE, edge))
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

#[cfg(test)]
mod tests {
    use vike_ui_theme::appearance::Appearance;
    use vike_ui_theme::components::ON_FILL;
    use vike_ui_theme::components::button::Kind;
    use vike_ui_theme::market::MarketId;
    use vike_ui_theme::theme::ThemeId;

    use super::*;

    const LOOKS: [Look; 4] = [Look::Plain, Look::Chosen, Look::Buy, Look::Sell];

    fn tokens(theme: ThemeId, market: MarketId) -> Tokens {
        Tokens::from_appearance(&Appearance { theme, market, ..Appearance::default() })
    }

    /// Each look's fill (at rest and under the pointer), outline and label are what the ticket
    /// painted before they moved to `ui-theme.toml`'s `button` map: plain the surface outlined in the
    /// border with the UI text, turning to the theme's hover fill; chosen the card ringed in the
    /// accent with the full text colour; Buy and Sell the market set's up and down with no outline
    /// and the on-fill black. Changing a row of the table changes the ticket, and this test is what
    /// says so by name.
    #[test]
    fn a_look_wears_the_fill_outline_and_label_the_button_map_gives_it() {
        for id in ThemeId::ALL {
            for m in MarketId::ALL {
                let t = tokens(id, m);
                let ring = |c: Color32| Stroke::new(stroke::HAIRLINE, c);
                for (look, fill, hot_fill, outline, label) in [
                    (
                        Look::Plain,
                        t.theme.surface,
                        t.theme.hover,
                        ring(t.theme.border),
                        t.theme.text_ui,
                    ),
                    (Look::Chosen, t.theme.card, t.theme.card, ring(t.theme.accent), t.theme.text),
                    (Look::Buy, t.market.up, t.market.up, Stroke::NONE, ON_FILL),
                    (Look::Sell, t.market.down, t.market.down, Stroke::NONE, ON_FILL),
                ] {
                    let at = format!("{id:?} {m:?} {look:?}");
                    assert_eq!(look.face(false, &t), (fill, outline), "{at}: at rest");
                    assert_eq!(look.face(true, &t), (hot_fill, outline), "{at}: under the pointer");
                    assert_eq!(Btn::new("x").look(look).ink(&t), label, "{at}: the label");
                }
            }
        }
    }

    /// The ticket reads the kit's table: a plain button is the kit's secondary one, Buy and Sell are
    /// the kit's, the chosen look is the one row of the `button` map the kit has no kind for, no two
    /// looks share a row, and no row of the map is read by neither the kit nor the ticket.
    #[test]
    fn every_look_reads_the_row_of_the_kit_it_is_and_every_row_of_the_button_map_has_a_reader() {
        for (look, kind) in [
            (Look::Plain, Some(Kind::Secondary)),
            (Look::Chosen, None),
            (Look::Buy, Some(Kind::Buy)),
            (Look::Sell, Some(Kind::Sell)),
        ] {
            match kind {
                Some(kind) => assert_eq!(look.row(), kind.row(), "{look:?} is the kit's {kind:?}"),
                None => assert_eq!(look.row().key, "CHOSEN", "{look:?}"),
            }
            assert_eq!(
                LOOKS.iter().filter(|o| o.row() == look.row()).count(),
                1,
                "{look:?}'s row is shared"
            );
        }
        for row in maps::button::ALL {
            let kit = Kind::ALL.iter().any(|k| k.row() == *row);
            let ticket = LOOKS.iter().any(|l| l.row() == *row);
            assert!(kit || ticket, "{} is read by neither the kit nor the ticket", row.key);
        }
    }

    /// Only the plain button turns to another fill under the pointer through its row: the chosen look
    /// does not change, and Buy and Sell are lifted by an overlay in `paint_face`, so their face is
    /// the same either way.
    #[test]
    fn only_the_plain_button_names_another_fill_for_the_pointer() {
        for id in ThemeId::ALL {
            let t = tokens(id, MarketId::default());
            for look in LOOKS {
                let turns = look.face(false, &t) != look.face(true, &t);
                assert_eq!(turns, look == Look::Plain, "{id:?} {look:?}");
            }
        }
    }
}
