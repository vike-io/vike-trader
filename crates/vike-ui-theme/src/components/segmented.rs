//! The segmented control (spec §4.2) — ONE component for the Data Manager's two variants (§4.3): a
//! joined row of options, the selected one a LABEL on the card fill outlined in the accent, the
//! others buttons on the surface fill. Each option carries the hover text that says what it shows.

use egui::{CornerRadius, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType};

use super::Tokens;
use crate::metrics::{RADIUS, stroke};
use crate::type_scale::TextRole;

/// One option.
#[derive(Clone, Copy, Debug)]
pub struct Segment<'a, T> {
    pub value: T,
    pub label: &'a str,
    /// The hover text: what choosing it shows.
    pub why: &'a str,
}

/// A segmented control. Switches `current` when an option is clicked and says whether it did.
/// It wraps when its row is too narrow (the Data Manager's inspector needs that).
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    current: &mut T,
    segments: &[Segment<'_, T>],
) -> bool {
    let t = Tokens::of(ui.ctx());
    let before = *current;
    ui.horizontal_wrapped(|ui| {
        // 1 px, not 0: each segment strokes its own edges, so butting them flush would draw a
        // double rule on every shared edge.
        ui.spacing_mut().item_spacing.x = 1.0;
        for s in segments {
            let on = *current == s.value;
            if segment(ui, &t, s.label, on).on_hover_text(s.why).clicked() {
                *current = s.value;
            }
        }
    });
    *current != before
}

fn segment(ui: &mut Ui, t: &Tokens, label: &str, on: bool) -> Response {
    let ink = if on { t.theme.text } else { t.theme.text2 };
    let galley = ui.painter().layout_no_wrap(label.to_string(), t.font(TextRole::Body), ink);
    let size = egui::vec2(galley.size().x + 2.0 * t.metrics.pad, t.metrics.control_h);
    let (rect, resp) =
        ui.allocate_exact_size(size, if on { Sense::hover() } else { Sense::click() });
    let (fill, edge) = if on {
        (t.theme.card, t.theme.accent)
    } else if resp.hovered() {
        (t.theme.hover, t.theme.border)
    } else {
        (t.theme.surface, t.theme.border)
    };
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(RADIUS),
        fill,
        Stroke::new(stroke::HAIRLINE, edge),
        StrokeKind::Inside,
    );
    p.galley(rect.center() - galley.size() / 2.0, galley, ink);
    super::focus_ring(ui, t, &resp);
    let typ = if on { WidgetType::Label } else { WidgetType::Button };
    resp.widget_info(|| WidgetInfo::labeled(typ, true, label));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{ctx_with, harness, named, paint, strokes};
    use crate::theme::{Theme, ThemeId};
    use egui::accesskit::Role;

    fn spans<'a>() -> [Segment<'a, u8>; 3] {
        [
            Segment { value: 0, label: "1D", why: "One day" },
            Segment { value: 1, label: "1W", why: "One week" },
            Segment { value: 2, label: "1M", why: "One month" },
        ]
    }

    #[test]
    fn the_selected_segment_is_a_label_outlined_in_each_themes_accent() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let shapes = paint(&ctx, |ui| {
                let mut cur = 1u8;
                segmented(ui, &mut cur, &spans());
            });
            assert!(strokes(&shapes).contains(&Theme::of(id).accent), "{id:?}");
        }
        let h = harness(Appearance::default(), |ui| {
            let mut cur = 1u8;
            segmented(ui, &mut cur, &spans());
        });
        assert_eq!(named(&h, Role::Label), ["1W"]);
        assert_eq!(named(&h, Role::Button), ["1D", "1M"]);
    }
}
