//! The segmented control (spec §4.2) — ONE component for the Data Manager's two variants (§4.3): a
//! joined row of options, the selected one a LABEL on the card fill outlined in the accent, the
//! others buttons on the surface fill. Each option carries the hover text that says what it shows.

use egui::{
    Align, CornerRadius, Layout, Response, Role, Sense, Stroke, StrokeKind, Ui, WidgetInfo,
};

use super::ThemeTokens;
use crate::metrics::{RADIUS, stroke};
use crate::type_scale::ThemeTextRole;

/// One option.
#[derive(Clone, Copy, Debug)]
pub struct ThemeSegment<'a, T> {
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
    segments: &[ThemeSegment<'_, T>],
) -> bool {
    let t = ThemeTokens::of(ui.ctx());
    let before = *current;
    // egui's `horizontal_wrapped`, opened `control_height` tall instead of `interact_size.y`: in a row
    // centred on a taller control the shorter opening hung every segment lower than the controls
    // beside it (the chart's `Auto` beside `%`/`Log`/`Lin`).
    let row = if ui.layout().prefer_right_to_left() {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    };
    let size = egui::vec2(ui.available_size_before_wrap().x, t.metrics.control_height);
    ui.allocate_ui_with_layout(size, row.with_main_wrap(true), |ui| {
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

fn segment(ui: &mut Ui, t: &ThemeTokens, label: &str, on: bool) -> Response {
    let ink = if on { t.theme.text } else { t.theme.text_secondary };
    let galley = ui.painter().layout_no_wrap(label.to_string(), t.font(ThemeTextRole::Body), ink);
    let size = egui::vec2(galley.size().x + 2.0 * t.metrics.padding, t.metrics.control_height);
    let (rect, response) =
        ui.allocate_exact_size(size, if on { Sense::hover() } else { Sense::click() });
    let (fill, edge) = if on {
        (t.theme.card, t.theme.accent)
    } else if response.hovered() {
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
    super::focus_ring(ui, t, &response);
    let typ = if on { Role::Label } else { Role::Button };
    response.widget_info(|| WidgetInfo::labeled(typ, true, label));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::ThemeAppearance;
    use crate::components::testing::{harness, named, paint, strokes};
    use crate::painted::context_with;
    use crate::theme::{ThemePalette, ThemePaletteId};
    use egui::accesskit::Role;

    fn spans<'a>() -> [ThemeSegment<'a, u8>; 3] {
        [
            ThemeSegment { value: 0, label: "1D", why: "One day" },
            ThemeSegment { value: 1, label: "1W", why: "One week" },
            ThemeSegment { value: 2, label: "1M", why: "One month" },
        ]
    }

    #[test]
    fn the_selected_segment_is_a_label_outlined_in_each_themes_accent() {
        for id in ThemePaletteId::ALL {
            let context =
                context_with(&ThemeAppearance { theme: id, ..ThemeAppearance::default() });
            let shapes = paint(&context, |ui| {
                let mut current = 1u8;
                segmented(ui, &mut current, &spans());
            });
            assert!(strokes(&shapes).contains(&ThemePalette::of(id).accent), "{id:?}");
        }
        let h = harness(ThemeAppearance::default(), |ui| {
            let mut current = 1u8;
            segmented(ui, &mut current, &spans());
        });
        assert_eq!(named(&h, Role::Label), ["1W"]);
        assert_eq!(named(&h, Role::Button), ["1D", "1M"]);
    }
}
