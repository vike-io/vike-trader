//! The navigation rail (spec §4.2, from the Data Manager's rail): group headings, then rows of an
//! icon, a label and a right-aligned mono count. The selected row is filled and marked by a 2 px
//! accent edge — the selected-row marker — and reported as a LABEL, the others as buttons.

use egui::{Align2, CornerRadius, FontId, Rect, Sense, Stroke, Ui, WidgetInfo, WidgetType};

use super::Tokens;
use crate::icons::{self, Icon};
use crate::metrics::{RADIUS, stroke};
use crate::type_scale::TextRole;

/// One destination.
#[derive(Clone, Copy, Debug)]
pub struct RailItem<'a, T> {
    pub value: T,
    /// Its group's heading, drawn wherever the group changes from one item to the next.
    pub group: &'a str,
    pub icon: Icon,
    pub label: &'a str,
    /// Right-aligned in the caption mono; `None` — not known — draws nothing.
    pub count: Option<u64>,
}

/// The rail, over the available width. Switches `current` on a click and says whether it did.
pub fn nav_rail<T: PartialEq + Copy>(
    ui: &mut Ui,
    current: &mut T,
    items: &[RailItem<'_, T>],
) -> bool {
    let t = Tokens::of(ui.ctx());
    let before = *current;
    let full = ui.available_width();
    let mut group: Option<&str> = None;
    for item in items {
        if group != Some(item.group) {
            let first = group.is_none();
            group = Some(item.group);
            ui.add_space(if first { t.metrics.gap } else { 2.0 * t.metrics.gap });
            let cap = t.font(TextRole::Caption);
            let (r, _) = ui.allocate_exact_size(egui::vec2(full, cap.size + 4.0), Sense::hover());
            let p = ui.painter();
            if !first {
                p.hline(
                    r.x_range(),
                    r.top() - t.metrics.gap,
                    Stroke::new(stroke::HAIRLINE, t.theme.border),
                );
            }
            let at = egui::pos2(r.left() + t.metrics.pad, r.center().y);
            p.text(at, Align2::LEFT_CENTER, item.group, cap, t.theme.text3);
        }
        let on = *current == item.value;
        let sense = if on { Sense::hover() } else { Sense::click() };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(full, t.metrics.control_h), sense);
        let p = ui.painter();
        if on {
            p.rect_filled(rect, CornerRadius::same(RADIUS), t.theme.surface);
            let edge = Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height()));
            p.rect_filled(edge, 0.0, t.theme.accent);
        } else if resp.hovered() {
            p.rect_filled(rect, CornerRadius::same(RADIUS), t.theme.hover);
        }
        let icon_px = t.text.px(TextRole::Title);
        let icon_at = egui::pos2(rect.left() + t.metrics.pad + icon_px / 2.0, rect.center().y);
        let icon_ink = if on { t.theme.accent } else { t.theme.text3 };
        let icon_font = FontId::new(icon_px, icons::family());
        item.icon.paint(p, icon_at, Align2::CENTER_CENTER, icon_font, icon_ink);
        let label_at =
            egui::pos2(rect.left() + t.metrics.pad + icon_px + t.metrics.gap, rect.center().y);
        let label_ink = if on { t.theme.text } else { t.theme.text2 };
        p.text(label_at, Align2::LEFT_CENTER, item.label, t.font(TextRole::Body), label_ink);
        if let Some(n) = item.count {
            let at = egui::pos2(rect.right() - t.metrics.pad, rect.center().y);
            let ink = if on { t.theme.text2 } else { t.theme.text3 };
            p.text(at, Align2::RIGHT_CENTER, n.to_string(), t.mono(TextRole::Caption), ink);
        }
        let spoken = match item.count {
            Some(n) => format!("{} {n}", item.label),
            None => item.label.to_string(),
        };
        super::focus_ring(ui, &t, &resp);
        let typ = if on { WidgetType::Label } else { WidgetType::Button };
        resp.widget_info(|| WidgetInfo::labeled(typ, true, &spoken));
        if resp.clicked() {
            *current = item.value;
        }
    }
    *current != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{ctx_with, fills, harness, named, paint, texts};
    use crate::theme::{Theme, ThemeId};
    use egui::accesskit::Role;
    use egui_kittest::kittest::NodeT;
    use std::cell::Cell;
    use std::rc::Rc;

    fn items<'a>() -> [RailItem<'a, u8>; 4] {
        [
            RailItem {
                value: 0,
                group: "Browse",
                icon: icons::OVERVIEW,
                label: "Overview",
                count: None,
            },
            RailItem {
                value: 1,
                group: "Browse",
                icon: icons::ALL_SERIES,
                label: "All series",
                count: Some(1204),
            },
            RailItem {
                value: 2,
                group: "Live",
                icon: icons::CACHED_FEEDS,
                label: "Cached feeds",
                count: Some(0),
            },
            RailItem {
                value: 3,
                group: "Live",
                icon: icons::ACTIVITY_LOG,
                label: "Activity log",
                count: None,
            },
        ]
    }

    #[test]
    fn the_selected_row_is_a_label_with_each_themes_accent_edge() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let shapes = paint(&ctx, |ui| {
                nav_rail(ui, &mut 1u8, &items());
            });
            assert!(fills(&shapes).contains(&Theme::of(id).accent), "{id:?}");
        }
        let h = harness(Appearance::default(), |ui| {
            nav_rail(ui, &mut 1u8, &items());
        });
        assert_eq!(named(&h, Role::Label), ["All series 1204"]);
        assert_eq!(named(&h, Role::Button), ["Overview", "Cached feeds 0", "Activity log"]);
    }

    #[test]
    fn each_group_gets_exactly_one_heading() {
        let ctx = ctx_with(&Appearance::default());
        let shapes = paint(&ctx, |ui| {
            nav_rail(ui, &mut 0u8, &items());
        });
        let t: Vec<String> = texts(&shapes).into_iter().map(|(s, _)| s).collect();
        assert_eq!(t.iter().filter(|s| *s == "Browse").count(), 1, "{t:?}");
        assert_eq!(t.iter().filter(|s| *s == "Live").count(), 1, "{t:?}");
    }

    #[test]
    fn clicking_a_row_selects_it() {
        let cur = Rc::new(Cell::new(0u8));
        let c = cur.clone();
        let mut h = harness(Appearance::default(), move |ui| {
            let mut v = c.get();
            nav_rail(ui, &mut v, &items());
            c.set(v);
        });
        h.root()
            .children_recursive()
            .find(|n| n.accesskit_node().label().as_deref() == Some("Activity log"))
            .expect("an unselected row is a button")
            .click();
        h.run();
        assert_eq!(cur.get(), 3);
    }
}
