//! Text and number inputs (spec §4.1): the control height, the border — the accent as a focus ring
//! while the field has focus; the status red, with its message under the field, while it is in
//! error — an optional unit drawn inside the field, and an optional mask for a secret.

use egui::{CornerRadius, Response, RichText, Stroke, Ui};

use super::{ThemeStatus, ThemeTokens};
use crate::metrics::{RADIUS, space, stroke};
use crate::type_scale::ThemeTextRole;

/// What a field says besides its value.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThemeInputField<'a> {
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
pub fn text(ui: &mut Ui, value: &mut String, field: ThemeInputField<'_>) -> Response {
    edit(ui, value, field, false)
}

/// A number input: monospace and right-aligned. `value` is the caller's text and [`parse_number`]
/// is what it means. Text that does not parse is an error of its own — "Not a number" — unless the
/// caller gave one.
pub fn number(ui: &mut Ui, value: &mut String, field: ThemeInputField<'_>) -> Response {
    let own = (!value.trim().is_empty() && parse_number(value).is_none()).then_some("Not a number");
    edit(ui, value, ThemeInputField { error: field.error.or(own), ..field }, true)
}

/// A finite number, surrounding spaces ignored.
pub fn parse_number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

fn edit(ui: &mut Ui, value: &mut String, field: ThemeInputField<'_>, numeric: bool) -> Response {
    let t = ThemeTokens::of(ui.ctx());
    let response = ui
        .scope(|ui| {
            // The field frames ITSELF, from its focus as egui last saw it. egui would frame a
            // focused `TextEdit` in `selection.stroke` — the colour it also paints SELECTED glyphs
            // in — so a ring borrowed from there would draw a selected number in the accent
            // (spec §2). The field's id is taken here, exactly as `TextEdit` would take it.
            let id = ui.next_auto_id();
            ui.skip_ahead_auto_ids(1);
            let focused = ui.memory(|m| m.has_focus(id));
            let edge = match (field.error, focused) {
                (Some(_), _) => ThemeStatus::Error.color(),
                (None, true) => t.theme.accent,
                (None, false) => t.theme.border,
            };
            let font =
                if numeric { t.mono(ThemeTextRole::Body) } else { t.font(ThemeTextRole::Body) };
            // 2 px above and below the text, less when the row and the 1 px edge leave no room for
            // that inside `control_height`: a Compact field is 20 pt tall and Standard's Body row is 15.
            let row = ui.ctx().fonts_mut(|f| f.row_height(&font));
            let margin_y =
                (((t.metrics.control_height - row - 2.0) / 2.0).floor() as i8).clamp(0, 2);
            let frame = egui::Frame::new()
                .fill(ui.visuals().text_edit_bg_color())
                .stroke(Stroke::new(stroke::HAIRLINE, edge))
                .corner_radius(CornerRadius::same(RADIUS))
                .inner_margin(egui::Margin::symmetric(space::SM as i8, margin_y));
            // `password` is egui's own mask, and it is also what files the field as a
            // `Role::PasswordInput` — the credential editor's masking today.
            let mut edit = egui::TextEdit::singleline(value)
                .id(id)
                .frame(frame)
                .font(font)
                .password(field.secret)
                .hint_text(RichText::new(field.hint).color(t.theme.text_tertiary))
                .min_size(egui::vec2(0.0, t.metrics.control_height))
                .align(egui::Align2([
                    if numeric { egui::Align::RIGHT } else { egui::Align::LEFT },
                    egui::Align::Center,
                ]));
            if let Some(unit) = field.unit {
                edit = edit.suffix(
                    RichText::new(unit)
                        .font(t.font(ThemeTextRole::Caption))
                        .color(t.theme.text_tertiary),
                );
            }
            ui.add(edit)
        })
        .inner;
    if let Some(e) = field.error {
        ui.label(
            RichText::new(e).font(t.font(ThemeTextRole::Caption)).color(ThemeStatus::Error.color()),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::ThemeAppearance;
    use crate::components::testing::{harness, paint, strokes, texts};
    use crate::metrics::ThemeDensity;
    use crate::painted::context_with;
    use crate::theme::{ThemePalette, ThemePaletteId};
    use egui::accesskit::Role;
    use egui_kittest::kittest::NodeT;

    #[test]
    fn a_field_is_the_densitys_control_height() {
        for d in ThemeDensity::ALL {
            let context =
                context_with(&ThemeAppearance { density: d, ..ThemeAppearance::default() });
            let mut h = 0.0;
            let mut s = String::new();
            paint(&context, |ui| h = text(ui, &mut s, ThemeInputField::default()).rect.height());
            assert_eq!(h, d.metrics().control_height, "{d:?}");
        }
    }

    /// A focused field draws the theme's accent as its ring, on every theme.
    #[test]
    fn focus_draws_the_accent_ring() {
        for id in ThemePaletteId::ALL {
            let context =
                context_with(&ThemeAppearance { theme: id, ..ThemeAppearance::default() });
            let mut s = "BTCUSDT".to_string();
            paint(&context, |ui| text(ui, &mut s, ThemeInputField::default()).request_focus());
            let shapes = paint(&context, |ui| {
                text(ui, &mut s, ThemeInputField::default());
            });
            assert!(strokes(&shapes).contains(&ThemePalette::of(id).accent), "{id:?}");
        }
    }

    /// Selecting text keeps the app's selection colours. The focus ring is the accent, but a
    /// selected number is never DRAWN in it (spec §2: the accent is never the colour of a number) —
    /// egui paints selected glyphs in the colour of `selection.stroke`, so the ring may not borrow it.
    #[test]
    fn selected_text_is_never_drawn_in_the_accent() {
        for id in ThemePaletteId::ALL {
            let th = ThemePalette::of(id);
            let context =
                context_with(&ThemeAppearance { theme: id, ..ThemeAppearance::default() });
            let mut s = "0.25".to_string();
            let mut field = None;
            paint(&context, |ui| {
                let r = number(ui, &mut s, ThemeInputField::default());
                r.request_focus();
                field = Some(r.id);
            });
            // A pass that gains the focus, then the selection: all four characters.
            paint(&context, |ui| {
                number(ui, &mut s, ThemeInputField::default());
            });
            let fid = field.expect("the field was drawn");
            let mut state = egui::text_edit::TextEditState::load(&context, fid).unwrap_or_default();
            let all = egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(4),
            );
            state.cursor.set_char_range(Some(all));
            state.store(&context, fid);
            let shapes = paint(&context, |ui| {
                number(ui, &mut s, ThemeInputField::default());
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
        let context = context_with(&ThemeAppearance::default());
        let mut s = "kraken".to_string();
        let shapes = paint(&context, |ui| {
            text(
                ui,
                &mut s,
                ThemeInputField { error: Some("Unknown venue"), ..ThemeInputField::default() },
            );
        });
        assert!(strokes(&shapes).contains(&ThemeStatus::Error.color()));
        assert!(
            texts(&shapes).contains(&("Unknown venue".to_string(), ThemeStatus::Error.color()))
        );
    }

    #[test]
    fn a_number_shows_its_unit_and_says_when_it_is_not_a_number() {
        let context = context_with(&ThemeAppearance::default());
        let (mut good, mut bad) = ("0.25".to_string(), "abc".to_string());
        let shapes = paint(&context, |ui| {
            number(
                ui,
                &mut good,
                ThemeInputField { unit: Some("BTC"), ..ThemeInputField::default() },
            );
            number(ui, &mut bad, ThemeInputField::default());
        });
        let t: Vec<String> = texts(&shapes).into_iter().map(|(s, _)| s).collect();
        assert!(t.contains(&"BTC".to_string()), "{t:?}");
        assert!(t.contains(&"Not a number".to_string()), "{t:?}");
    }

    /// A secret field draws no character of its value — one password bullet per character — and
    /// tells the accessibility tree it is a password, so nothing reads the key aloud.
    #[test]
    fn a_secret_field_masks_its_value() {
        let context = context_with(&ThemeAppearance::default());
        let mut s = "hunter2".to_string();
        // A hint is what names an unlabelled field for assistive tech (egui_kittest 0.37 refuses a
        // nameless input), so the fixture carries one.
        let secret =
            ThemeInputField { secret: true, hint: "API key", ..ThemeInputField::default() };
        let shapes = paint(&context, |ui| {
            text(ui, &mut s, secret);
        });
        let t: Vec<String> = texts(&shapes).into_iter().map(|(s, _)| s).collect();
        assert!(!t.iter().any(|s| s.contains("hunter2")), "the secret was drawn: {t:?}");
        let mask: String =
            std::iter::repeat_n(egui::epaint::text::PASSWORD_REPLACEMENT_CHAR, 7).collect();
        assert!(t.contains(&mask), "{t:?}");
        let h = harness(ThemeAppearance::default(), |ui| {
            text(ui, &mut "hunter2".to_string(), secret);
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
