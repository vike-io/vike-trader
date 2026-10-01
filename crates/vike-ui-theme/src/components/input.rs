//! Text and number inputs (spec §4.1): the control height, the border — the accent as a focus ring
//! while the field has focus; the status red, with its message under the field, while it is in
//! error — an optional unit drawn inside the field, and an optional mask for a secret.

use egui::{CornerRadius, Response, RichText, Stroke, Ui};

use super::{Status, Tokens};
use crate::metrics::RADIUS;
use crate::type_scale::TextRole;

/// What a field says besides its value.
#[derive(Clone, Copy, Debug, Default)]
pub struct Field<'a> {
    /// Shown, dimmed, while the field is empty.
    pub hint: &'a str,
    /// Drawn inside the field after the value: `BTC`, `%`, `ms`.
    pub unit: Option<&'a str>,
    /// Draws the field's border in the status red and says this under it.
    pub error: Option<&'a str>,
    /// Masks the value — every character drawn as egui's password bullet — and files the field
    /// as a password input for assistive tech: an API key or a secret, never a value the operator
    /// is there to read.
    pub secret: bool,
}

/// A single-line text input.
pub fn text(ui: &mut Ui, value: &mut String, field: Field<'_>) -> Response {
    edit(ui, value, field, false)
}

/// A number input: monospace and right-aligned. `value` is the caller's text and [`parse_number`]
/// is what it means. Text that does not parse is an error of its own — "Not a number" — unless the
/// caller gave one.
pub fn number(ui: &mut Ui, value: &mut String, field: Field<'_>) -> Response {
    let own = (!value.trim().is_empty() && parse_number(value).is_none()).then_some("Not a number");
    edit(ui, value, Field { error: field.error.or(own), ..field }, true)
}

/// A finite number, surrounding spaces ignored.
pub fn parse_number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn edit(ui: &mut Ui, value: &mut String, field: Field<'_>, numeric: bool) -> Response {
    let t = Tokens::of(ui.ctx());
    let resp = ui
        .scope(|ui| {
            // The field frames ITSELF, from its focus as egui last saw it. egui would frame a
            // focused `TextEdit` in `selection.stroke` — the colour it also paints SELECTED glyphs
            // in — so a ring borrowed from there would draw a selected number in the accent
            // (spec §2). The field's id is taken here, exactly as `TextEdit` would take it.
            let id = ui.next_auto_id();
            ui.skip_ahead_auto_ids(1);
            let focused = ui.memory(|m| m.has_focus(id));
            let edge = match (field.error, focused) {
                (Some(_), _) => Status::Error.color(),
                (None, true) => t.theme.accent,
                (None, false) => t.theme.border,
            };
            let frame = egui::Frame::new()
                .fill(ui.visuals().text_edit_bg_color())
                .stroke(Stroke::new(1.0, edge))
                .corner_radius(CornerRadius::same(RADIUS))
                .inner_margin(egui::Margin::symmetric(4, 2));
            let font = if numeric { t.mono(TextRole::Body) } else { t.font(TextRole::Body) };
            // `password` is egui's own mask, and it is also what files the field as a
            // `Role::PasswordInput` — the credential editor's masking today.
            let mut edit = egui::TextEdit::singleline(value)
                .id(id)
                .frame(frame)
                .font(font)
                .password(field.secret)
                .hint_text(RichText::new(field.hint).color(t.theme.text3))
                .min_size(egui::vec2(0.0, t.metrics.control_h))
                .vertical_align(egui::Align::Center);
            if numeric {
                edit = edit.horizontal_align(egui::Align::RIGHT);
            }
            if let Some(unit) = field.unit {
                edit = edit.suffix(
                    RichText::new(unit).font(t.font(TextRole::Caption)).color(t.theme.text3),
                );
            }
            ui.add(edit)
        })
        .inner;
    if let Some(e) = field.error {
        ui.label(RichText::new(e).font(t.font(TextRole::Caption)).color(Status::Error.color()));
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{ctx_with, harness, paint, strokes, texts};
    use crate::metrics::Density;
    use crate::theme::{Theme, ThemeId};
    use egui::accesskit::Role;
    use egui_kittest::kittest::NodeT;

    #[test]
    fn a_field_is_the_densitys_control_height() {
        for d in Density::ALL {
            let ctx = ctx_with(&Appearance { density: d, ..Appearance::default() });
            let mut h = 0.0;
            let mut s = String::new();
            paint(&ctx, |ui| h = text(ui, &mut s, Field::default()).rect.height());
            assert_eq!(h, d.metrics().control_h, "{d:?}");
        }
    }

    /// A focused field draws the theme's accent as its ring, on every theme.
    #[test]
    fn focus_draws_the_accent_ring() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let mut s = "BTCUSDT".to_string();
            paint(&ctx, |ui| text(ui, &mut s, Field::default()).request_focus());
            let shapes = paint(&ctx, |ui| {
                text(ui, &mut s, Field::default());
            });
            assert!(strokes(&shapes).contains(&Theme::of(id).accent), "{id:?}");
        }
    }

    /// Selecting text keeps the app's selection colours. The focus ring is the accent, but a
    /// selected number is never DRAWN in it (spec §2: the accent is never the colour of a number) —
    /// egui paints selected glyphs in the colour of `selection.stroke`, so the ring may not borrow it.
    #[test]
    fn selected_text_is_never_drawn_in_the_accent() {
        for id in ThemeId::ALL {
            let th = Theme::of(id);
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let mut s = "0.25".to_string();
            let mut field = None;
            paint(&ctx, |ui| {
                let r = number(ui, &mut s, Field::default());
                r.request_focus();
                field = Some(r.id);
            });
            // A pass that gains the focus, then the selection: all four characters.
            paint(&ctx, |ui| {
                number(ui, &mut s, Field::default());
            });
            let fid = field.expect("the field was drawn");
            let mut state = egui::text_edit::TextEditState::load(&ctx, fid).unwrap_or_default();
            let all = egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(4),
            );
            state.cursor.set_char_range(Some(all));
            state.store(&ctx, fid);
            let shapes = paint(&ctx, |ui| {
                number(ui, &mut s, Field::default());
            });
            let painted: Vec<egui::Color32> = shapes
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Text(t) => Some(t),
                    _ => None,
                })
                .flat_map(|t| t.galley.rows.iter())
                .flat_map(|r| r.row.visuals.mesh.vertices.iter().map(|v| v.color))
                .collect();
            assert!(painted.contains(&th.hover), "{id:?}: no selection was painted");
            assert!(!painted.contains(&th.accent), "{id:?}: selected text drawn in the accent");
            assert!(strokes(&shapes).contains(&th.accent), "{id:?}: the focus ring is gone");
        }
    }

    #[test]
    fn an_error_is_the_status_red_with_its_message() {
        let ctx = ctx_with(&Appearance::default());
        let mut s = "kraken".to_string();
        let shapes = paint(&ctx, |ui| {
            text(ui, &mut s, Field { error: Some("Unknown venue"), ..Field::default() });
        });
        assert!(strokes(&shapes).contains(&Status::Error.color()));
        assert!(texts(&shapes).contains(&("Unknown venue".to_string(), Status::Error.color())));
    }

    #[test]
    fn a_number_shows_its_unit_and_says_when_it_is_not_a_number() {
        let ctx = ctx_with(&Appearance::default());
        let (mut good, mut bad) = ("0.25".to_string(), "abc".to_string());
        let shapes = paint(&ctx, |ui| {
            number(ui, &mut good, Field { unit: Some("BTC"), ..Field::default() });
            number(ui, &mut bad, Field::default());
        });
        let t: Vec<String> = texts(&shapes).into_iter().map(|(s, _)| s).collect();
        assert!(t.contains(&"BTC".to_string()), "{t:?}");
        assert!(t.contains(&"Not a number".to_string()), "{t:?}");
    }

    /// A secret field draws no character of its value — one password bullet per character — and
    /// tells the accessibility tree it is a password, so nothing reads the key aloud.
    #[test]
    fn a_secret_field_masks_its_value() {
        let ctx = ctx_with(&Appearance::default());
        let mut s = "hunter2".to_string();
        let shapes = paint(&ctx, |ui| {
            text(ui, &mut s, Field { secret: true, ..Field::default() });
        });
        let t: Vec<String> = texts(&shapes).into_iter().map(|(s, _)| s).collect();
        assert!(!t.iter().any(|s| s.contains("hunter2")), "the secret was drawn: {t:?}");
        let mask: String =
            std::iter::repeat_n(egui::epaint::text::PASSWORD_REPLACEMENT_CHAR, 7).collect();
        assert!(t.contains(&mask), "{t:?}");
        let h = harness(Appearance::default(), |ui| {
            text(ui, &mut "hunter2".to_string(), Field { secret: true, ..Field::default() });
        });
        let roles: Vec<Role> =
            h.root().children_recursive().map(|n| n.accesskit_node().role()).collect();
        assert!(roles.contains(&Role::PasswordInput), "{roles:?}");
    }

    #[test]
    fn parse_number_takes_a_finite_number_and_nothing_else() {
        assert_eq!(parse_number(" 1.5 "), Some(1.5));
        assert_eq!(parse_number("-2"), Some(-2.0));
        assert_eq!(parse_number("abc"), None);
        assert_eq!(parse_number("inf"), None);
        assert_eq!(parse_number(""), None);
    }
}
