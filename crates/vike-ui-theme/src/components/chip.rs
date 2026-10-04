//! Chips, dots, badges and marks (spec §4.1, §4.2):
//! - the LIVE / DEMO / PAPER mode chips and an account chip;
//! - the status dot with its label;
//! - the count badge and the outlined badge;
//! - the Connections presence mark, with its one-line legend.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Response, RichText, Sense, Stroke, StrokeKind, Ui,
    WidgetInfo, WidgetType,
};

use super::{ON_FILL, Status, Tokens};
use crate::fonts::SEMIBOLD;
use crate::metrics::RADIUS;
use crate::type_scale::TextRole;

/// Where an account trades.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Live,
    Demo,
    Paper,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Live, Mode::Demo, Mode::Paper];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Live => "LIVE",
            Mode::Demo => "DEMO",
            Mode::Paper => "PAPER",
        }
    }

    /// `(fill, stroke, text)`. LIVE — real money — is one of the accent's six shapes (spec §2): an
    /// accent fill, and no other mode is filled in the accent. The owner ruled it so on 2026-10-03
    /// (ruling B), over the danger red the Trade window's build had filled it in. DEMO — a real
    /// venue, play money — is outlined in the warning colour; PAPER, where nothing leaves the box,
    /// in the border grey. LIVE is told from DEMO by SHAPE, filled against outlined, which is all
    /// that does it on Carbon, whose amber accent sits 4° of hue from the warning amber.
    pub fn colours(self, t: &Tokens) -> (Color32, Stroke, Color32) {
        match self {
            Mode::Live => (t.theme.accent, Stroke::NONE, ON_FILL),
            Mode::Demo => {
                let w = Status::Warning.color();
                (Color32::TRANSPARENT, Stroke::new(1.0, w), w)
            }
            Mode::Paper => (Color32::TRANSPARENT, Stroke::new(1.0, t.theme.border), t.theme.text2),
        }
    }

    /// `t`'s background washed toward this mode's FILL: the fill of a region that marks the mode
    /// without taking any of its room. The Trade window washes the confirm prompt of an order held
    /// on a LIVE account in LIVE's own fill — a low tint of the theme's accent — so the chip and
    /// the prompt read one colour and cannot drift apart. Opaque, so what is written over it is
    /// measured exactly: on LIVE's wash the theme's text, the warning amber and LIVE's fill itself
    /// each clear the 4.5:1 text floor, on every theme
    /// (`the_live_wash_keeps_what_is_written_on_it_readable`). A mode that is not filled (DEMO,
    /// PAPER) has nothing to wash toward and gives back the background.
    pub fn wash(self, t: &Tokens) -> Color32 {
        let (fill, ..) = self.colours(t);
        if fill == Color32::TRANSPARENT {
            t.theme.bg
        } else {
            t.theme.bg.lerp_to_gamma(fill, WASH_T)
        }
    }
}

/// How far [`Mode::wash`] moves the background toward the mode's fill: enough to read as a coloured
/// band on every theme's near-black background, and little enough that what is written on it still
/// reads. On LIVE's accent wash the least of the three inks is the accent itself as text on Dusk,
/// 5.58:1; the theme text clears 12.21:1 and the warning amber 7.53:1 on every theme, Carbon's
/// amber wash included (8.25:1).
const WASH_T: f32 = 0.14;

/// A rounded box around `text`: the one shape every chip here is.
fn pill(
    ui: &mut Ui,
    t: &Tokens,
    text: &str,
    font: FontId,
    fill: Color32,
    stroke: Stroke,
    ink: Color32,
) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, ink);
    let pad = egui::vec2((t.metrics.pad * 0.75).round(), 2.0);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + pad * 2.0, Sense::hover());
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(RADIUS), fill, stroke, StrokeKind::Inside);
    p.galley(rect.min + pad, galley, ink);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    resp
}

/// A LIVE / DEMO / PAPER chip, in the caption role at the semibold weight.
pub fn mode(ui: &mut Ui, m: Mode) -> Response {
    let t = Tokens::of(ui.ctx());
    let (fill, stroke, ink) = m.colours(&t);
    let font = FontId::new(t.text.px(TextRole::Caption), FontFamily::Name(SEMIBOLD.into()));
    pill(ui, &t, m.label(), font, fill, stroke, ink)
}

/// An account chip: its mode's own chip, then its name. The SELECTED account is a label outlined in
/// the accent — never a button, so "which account" is answered by role (spec §4.2); every other
/// account is a button outlined in the border grey.
///
/// ⚠ The mode is a CHIP — LIVE filled in the accent, DEMO outlined in the warning colour — never a
/// coloured dot. Carbon's accent sits 4° of hue from the warning amber, and the design accepts that
/// only because the accent is a shape and the warning a dot (spec §3.1): a LIVE dot in the accent
/// beside a DEMO dot in the warning colour would be the same mark on Carbon.
pub fn account(ui: &mut Ui, name: &str, m: Mode, selected: bool) -> Response {
    let t = Tokens::of(ui.ctx());
    let (tag_fill, tag_edge, tag_ink) = m.colours(&t);
    let tag_font = FontId::new(t.text.px(TextRole::Caption), FontFamily::Name(SEMIBOLD.into()));
    let tag = ui.painter().layout_no_wrap(m.label().to_string(), tag_font, tag_ink);
    let words = ui.painter().layout_no_wrap(name.to_string(), t.font(TextRole::Body), t.theme.text);
    let tag_pad = egui::vec2((t.metrics.pad * 0.5).round(), 1.0);
    let tag_size = tag.size() + tag_pad * 2.0;
    let w = 2.0 * t.metrics.pad + tag_size.x + t.metrics.gap + words.size().x;
    let sense = if selected { Sense::hover() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, t.metrics.control_h), sense);
    let edge = if selected { t.theme.accent } else { t.theme.border };
    let fill = if resp.hovered() && !selected { t.theme.hover } else { Color32::TRANSPARENT };
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(RADIUS), fill, Stroke::new(1.0, edge), StrokeKind::Inside);
    let tag_at = egui::pos2(rect.left() + t.metrics.pad, rect.center().y - tag_size.y / 2.0);
    let tag_rect = egui::Rect::from_min_size(tag_at, tag_size);
    p.rect(tag_rect, CornerRadius::same(RADIUS), tag_fill, tag_edge, StrokeKind::Inside);
    p.galley(tag_rect.min + tag_pad, tag, tag_ink);
    let x = tag_rect.right() + t.metrics.gap;
    p.galley(egui::pos2(x, rect.center().y - words.size().y / 2.0), words, t.theme.text);
    super::focus_ring(ui, &t, &resp);
    let typ = if selected { WidgetType::Label } else { WidgetType::Button };
    resp.widget_info(|| WidgetInfo::labeled(typ, true, name));
    resp
}

/// A status dot and its label. The dot is the status colour — fixed in every theme, so
/// "connected" is `Status::Ok`, never the accent (spec §2) — and the label is secondary text.
pub fn status_dot(ui: &mut Ui, status: Status, label: &str) -> Response {
    let t = Tokens::of(ui.ctx());
    let galley =
        ui.painter().layout_no_wrap(label.to_string(), t.font(TextRole::Body), t.theme.text2);
    let d = (t.text.px(TextRole::Caption) * 0.7).round();
    let size = egui::vec2(d + t.metrics.gap + galley.size().x, galley.size().y.max(d));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.circle_filled(egui::pos2(rect.left() + d / 2.0, rect.center().y), d / 2.0, status.color());
    let x = rect.left() + d + t.metrics.gap;
    p.galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, t.theme.text2);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, label));
    resp
}

/// A count badge. `None` — a count that is not known — draws NOTHING and answers `None`: an
/// unknown count is never shown as 0 (spec §4.2).
pub fn count(ui: &mut Ui, n: Option<u64>) -> Option<Response> {
    let n = n?;
    let t = Tokens::of(ui.ctx());
    let text = n.to_string();
    let galley =
        ui.painter().layout_no_wrap(text.clone(), t.mono(TextRole::Caption), t.theme.text2);
    let h = galley.size().y + 2.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(galley.size().x + h, h), Sense::hover());
    let p = ui.painter();
    // Pills and dots stay round (spec §3.4).
    p.rect_filled(rect, CornerRadius::same((h / 2.0) as u8), t.theme.card);
    p.galley(rect.center() - galley.size() / 2.0, galley, t.theme.text2);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &text));
    Some(resp)
}

/// A badge's text colour: the status colour, except `Muted`, whose grey is below the 4.5:1 text
/// floor — its text is the caption grey instead (`every_badge_reads_on_every_theme`).
pub fn badge_ink(t: &Tokens, status: Status) -> Color32 {
    match status {
        Status::Muted => t.theme.text3,
        s => s.color(),
    }
}

/// An outlined badge (spec §4.2, from the Connections redesign): short mono text inside an outline
/// in a status colour — `READ-ONLY` in the warning colour, say.
pub fn badge(ui: &mut Ui, text: &str, status: Status) -> Response {
    let t = Tokens::of(ui.ctx());
    let ink = badge_ink(&t, status);
    let edge = Stroke::new(1.0, status.color());
    pill(ui, &t, text, t.mono(TextRole::Caption), Color32::TRANSPARENT, edge, ink)
}

/// The four presence marks of the Connections credential rail (spec §4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Presence {
    Configured,
    NotSet,
    NoSuchTier,
    Unknown,
}

impl Presence {
    pub const ALL: [Presence; 4] =
        [Presence::Configured, Presence::NotSet, Presence::NoSuchTier, Presence::Unknown];

    /// The mark — text, not an icon: `crates/vike-connections/src/view.rs`'s rail prints it.
    pub fn glyph(self) -> &'static str {
        match self {
            Presence::Configured => "\u{25CF}",
            Presence::NotSet => "\u{25CB}",
            Presence::NoSuchTier => "\u{00B7}",
            Presence::Unknown => "?",
        }
    }

    /// The word the legend gives it.
    pub fn word(self) -> &'static str {
        match self {
            Presence::Configured => "configured",
            Presence::NotSet => "not set",
            Presence::NoSuchTier => "no such tier",
            Presence::Unknown => "not measured",
        }
    }

    pub fn color(self, t: &Tokens) -> Color32 {
        match self {
            Presence::Configured => Status::Ok.color(),
            Presence::NotSet => Status::Muted.color(),
            Presence::NoSuchTier => t.theme.text3,
            Presence::Unknown => Status::Warning.color(),
        }
    }
}

/// One presence mark; hovering it names the key it is about.
pub fn presence(ui: &mut Ui, mark: Presence, key: &str) -> Response {
    let t = Tokens::of(ui.ctx());
    let text = RichText::new(mark.glyph()).font(t.mono(TextRole::Body)).color(mark.color(&t));
    ui.add(egui::Label::new(text).selectable(false)).on_hover_text(key)
}

/// The marks' one-line legend: each mark, then its word.
pub fn presence_legend(ui: &mut Ui) {
    let t = Tokens::of(ui.ctx());
    ui.horizontal(|ui| {
        for m in Presence::ALL {
            ui.label(RichText::new(m.glyph()).font(t.mono(TextRole::Caption)).color(m.color(&t)));
            ui.label(RichText::new(m.word()).font(t.font(TextRole::Caption)).color(t.theme.text3));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::color_math::contrast_ratio;
    use crate::components::testing::{
        circles, ctx_with, fills, harness, named, paint, strokes, texts,
    };
    use crate::theme::{Theme, ThemeId};
    use egui::accesskit::Role;

    /// LIVE is the accent SHAPE (spec §2, and the owner's ruling B of 2026-10-03, which took it
    /// back from the danger red the Trade window's build had filled it in): the installed theme's
    /// accent fill with the on-fill label, and no other mode is filled in the accent. DEMO is
    /// outlined in the warning colour, PAPER in the border grey. The label reads on its fill:
    /// `contrast_ratio`, the 4.5:1 text floor, on every theme.
    ///
    /// LIVE and DEMO also differ by SHAPE on every theme — filled and never outlined, against
    /// outlined and never filled — because on Carbon that is all that tells them apart: its amber
    /// accent sits 4° of hue from the warning amber DEMO is outlined in (spec §3.1).
    #[test]
    fn live_is_the_accent_demo_the_warning_paper_the_border() {
        let danger = Status::Error.color();
        let warning = Status::Warning.color();
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let th = Theme::of(id);
            let live = paint(&ctx, |ui| {
                mode(ui, Mode::Live);
            });
            assert!(fills(&live).contains(&th.accent), "{id:?}: LIVE is the theme's accent fill");
            assert!(!fills(&live).contains(&danger), "{id:?}: LIVE is not the danger red");
            assert!(texts(&live).contains(&("LIVE".to_string(), ON_FILL)), "{id:?}");
            let (fill, _, ink) = Mode::Live.colours(&Tokens::of(&ctx));
            let r = contrast_ratio(ink, fill);
            assert!(r >= 4.5, "{id:?}: the LIVE label on its fill: {r:.2}");
            let demo = paint(&ctx, |ui| {
                mode(ui, Mode::Demo);
            });
            assert!(strokes(&demo).contains(&warning), "{id:?}");
            let paper = paint(&ctx, |ui| {
                mode(ui, Mode::Paper);
            });
            assert!(strokes(&paper).contains(&th.border), "{id:?}");
            for (other, shapes) in [("DEMO", &demo), ("PAPER", &paper)] {
                let f = fills(shapes);
                assert!(!f.contains(&th.accent), "{id:?}: {other} is not filled in the accent");
            }
            // The SHAPE: every rectangle LIVE paints is unoutlined, every one DEMO paints unfilled.
            let clear = |c: &Color32| *c == Color32::TRANSPARENT;
            assert!(strokes(&live).iter().all(clear), "{id:?}: LIVE is outlined: {live:?}");
            assert!(fills(&demo).iter().all(clear), "{id:?}: DEMO is filled: {demo:?}");
        }
    }

    /// LIVE's wash (the Trade window's LIVE confirm prompt) is a band of its own — neither the
    /// background nor LIVE's fill — and everything that prompt writes on it reads: the theme's
    /// text, the warning amber of a Stop that triggers at once, and the word LIVE in LIVE's own
    /// fill (the theme's accent), each at the 4.5:1 text floor, on every theme. Carbon is the case
    /// to watch: its wash is a tint of an amber, and the warning amber is written on it. CONTROL:
    /// an unfilled mode's wash is the background itself.
    #[test]
    fn the_live_wash_keeps_what_is_written_on_it_readable() {
        for id in ThemeId::ALL {
            let t = Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() });
            let (fill, ..) = Mode::Live.colours(&t);
            let wash = Mode::Live.wash(&t);
            assert_eq!(wash.a(), 255, "{id:?}: opaque, so it is measured exactly");
            assert!(wash != t.theme.bg && wash != fill, "{id:?}: a band of its own");
            for (what, ink) in [
                ("the text", t.theme.text),
                ("the warning amber", Status::Warning.color()),
                ("LIVE's fill", fill),
            ] {
                let r = contrast_ratio(ink, wash);
                assert!(r >= 4.5, "{id:?}: {what} on LIVE's wash: {r:.2}");
            }
            for m in [Mode::Demo, Mode::Paper] {
                assert_eq!(m.wash(&t), t.theme.bg, "{id:?}: CONTROL: {m:?} has no fill to wash");
            }
        }
    }

    #[test]
    fn the_selected_account_is_a_label_and_the_others_buttons() {
        let h = harness(Appearance::default(), |ui| {
            account(ui, "main", Mode::Live, true);
            account(ui, "hedge", Mode::Demo, false);
        });
        assert_eq!(named(&h, Role::Label), ["main"]);
        assert_eq!(named(&h, Role::Button), ["hedge"]);
    }

    /// Carbon's accent sits 4° of hue from the warning amber, and the design accepts that only
    /// because the accent is a SHAPE and the warning a dot (spec §3.1). So an account shows its mode
    /// as that mode's own chip — LIVE filled in the accent, DEMO outlined in the warning colour —
    /// never as a coloured dot, and the selected account is outlined in the accent: on every theme.
    #[test]
    fn an_account_shows_its_mode_as_a_chip_not_a_dot_on_every_theme() {
        let warning = Status::Warning.color();
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let th = Theme::of(id);
            let live = paint(&ctx, |ui| {
                account(ui, "main", Mode::Live, false);
            });
            let demo = paint(&ctx, |ui| {
                account(ui, "hedge", Mode::Demo, false);
            });
            assert!(circles(&live).is_empty() && circles(&demo).is_empty(), "{id:?}: a mode dot");
            assert!(fills(&live).contains(&th.accent), "{id:?}: LIVE is the accent fill");
            assert!(texts(&live).contains(&("LIVE".to_string(), ON_FILL)), "{id:?}");
            assert!(strokes(&demo).contains(&warning), "{id:?}: DEMO is outlined in the warning");
            assert!(!fills(&demo).contains(&warning), "{id:?}: DEMO is filled");
            assert!(texts(&demo).contains(&("DEMO".to_string(), warning)), "{id:?}");
            let selected = paint(&ctx, |ui| {
                account(ui, "hedge", Mode::Demo, true);
            });
            assert!(strokes(&selected).contains(&th.accent), "{id:?}: the selection outline");
        }
    }

    /// A status dot is its status colour on every theme — "connected" is never the accent.
    #[test]
    fn a_status_dot_is_its_status_colour_on_every_theme() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            for s in Status::ALL {
                let shapes = paint(&ctx, |ui| {
                    status_dot(ui, s, "Connected");
                });
                assert_eq!(circles(&shapes), [s.color()], "{id:?} {s:?}");
            }
        }
    }

    /// An unknown count draws NOTHING; a known zero draws "0" (spec §4.2).
    #[test]
    fn an_unknown_count_draws_nothing_and_a_known_zero_draws_zero() {
        let ctx = ctx_with(&Appearance::default());
        let mut none = Some(());
        let unknown = paint(&ctx, |ui| none = count(ui, None).map(|_| ()));
        assert!(unknown.is_empty() && none.is_none());
        let zero = paint(&ctx, |ui| {
            count(ui, Some(0));
        });
        assert!(texts(&zero).iter().any(|(s, _)| s == "0"));
    }

    /// A badge's text reads (4.5:1) on the background and on a card, for every status and theme.
    #[test]
    fn every_badge_reads_on_every_theme() {
        for id in ThemeId::ALL {
            let t = Tokens::from_appearance(&Appearance { theme: id, ..Appearance::default() });
            for s in Status::ALL {
                for ground in [t.theme.bg, t.theme.card] {
                    let r = contrast_ratio(badge_ink(&t, s), ground);
                    assert!(r >= 4.5, "{id:?} {s:?}: {r:.2}");
                }
            }
        }
    }

    #[test]
    fn the_legend_names_every_mark() {
        let ctx = ctx_with(&Appearance::default());
        let shapes = paint(&ctx, presence_legend);
        let t: Vec<String> = texts(&shapes).into_iter().map(|(s, _)| s).collect();
        for m in Presence::ALL {
            assert!(
                t.contains(&m.glyph().to_string()) && t.contains(&m.word().to_string()),
                "{m:?}: {t:?}"
            );
        }
    }
}
