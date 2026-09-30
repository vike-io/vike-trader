//! Underline tabs with an optional count (spec §2 "Tabs: underline", §4.1). The selected tab is a
//! LABEL marked by a 2 px accent underline; the others are frameless buttons in the secondary text
//! colour; a 1 px hairline runs under the whole row. An unknown count is not drawn, never as 0.

use egui::{Response, Sense, Shape, Stroke, Ui, WidgetInfo, WidgetType};

use super::Tokens;
use crate::type_scale::TextRole;

/// One tab.
#[derive(Clone, Copy, Debug)]
pub struct Tab<'a, T> {
    pub value: T,
    pub label: &'a str,
    /// Drawn after the label in the caption mono; `None` — not known — draws nothing.
    pub count: Option<u64>,
}

/// A row of underline tabs. Switches `current` when one is clicked and says whether it did.
pub fn underline<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, tabs: &[Tab<'_, T>]) -> bool {
    let t = Tokens::of(ui.ctx());
    let before = *current;
    // The hairline is painted UNDER the tabs, so the selected tab's underline sits on top of it.
    let hairline = ui.painter().add(Shape::Noop);
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for tab in tabs {
            let on = *current == tab.value;
            if cell(ui, &t, tab.label, tab.count, on).clicked() {
                *current = tab.value;
            }
        }
    });
    let r = row.response.rect;
    let rule = Shape::hline(r.x_range(), r.bottom() - 0.5, Stroke::new(1.0, t.theme.border));
    ui.painter().set(hairline, rule);
    *current != before
}

fn cell(ui: &mut Ui, t: &Tokens, label: &str, count: Option<u64>, on: bool) -> Response {
    let words =
        ui.painter().layout_no_wrap(label.to_string(), t.font(TextRole::Strong), t.theme.text);
    let number = count.map(|n| {
        ui.painter().layout_no_wrap(n.to_string(), t.mono(TextRole::Caption), t.theme.text3)
    });
    let words_w = words.size().x;
    let number_w = number.as_ref().map_or(0.0, |g| t.metrics.gap + g.size().x);
    let size = egui::vec2(words_w + number_w + 2.0 * t.metrics.pad, t.metrics.control_h);
    let (rect, resp) =
        ui.allocate_exact_size(size, if on { Sense::hover() } else { Sense::click() });
    let ink = if on || resp.hovered() { t.theme.text } else { t.theme.text2 };
    let p = ui.painter();
    let x = rect.left() + t.metrics.pad;
    let words_y = rect.center().y - words.size().y / 2.0;
    p.galley_with_override_text_color(egui::pos2(x, words_y), words, ink);
    if let Some(g) = number {
        let y = rect.center().y - g.size().y / 2.0;
        p.galley(egui::pos2(x + words_w + t.metrics.gap, y), g, t.theme.text3);
    }
    if on {
        p.hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(2.0, t.theme.accent));
    }
    let spoken = match count {
        Some(n) => format!("{label} {n}"),
        None => label.to_string(),
    };
    super::focus_ring(ui, t, &resp);
    let typ = if on { WidgetType::Label } else { WidgetType::Button };
    resp.widget_info(|| WidgetInfo::labeled(typ, true, &spoken));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{ctx_with, harness, named, paint, strokes, texts};
    use crate::theme::{Theme, ThemeId};
    use egui::accesskit::Role;
    use egui_kittest::kittest::NodeT;
    use std::cell::Cell;
    use std::rc::Rc;

    fn three<'a>() -> [Tab<'a, u8>; 3] {
        [
            Tab { value: 0, label: "Credentials", count: Some(12) },
            Tab { value: 1, label: "Backend", count: None },
            Tab { value: 2, label: "Accounts", count: Some(0) },
        ]
    }

    /// The selected tab is a LABEL and the others are buttons — "which tab am I on" is answered by
    /// role, and the current tab cannot be re-picked (spec §4.2).
    #[test]
    fn the_selected_tab_is_a_label_and_the_others_are_buttons() {
        let h = harness(Appearance::default(), |ui| {
            let mut cur = 0u8;
            underline(ui, &mut cur, &three());
        });
        assert!(named(&h, Role::Label).contains(&"Credentials 12".to_string()));
        let buttons = named(&h, Role::Button);
        assert!(buttons.contains(&"Backend".to_string()), "{buttons:?}");
        assert!(!buttons.iter().any(|b| b.starts_with("Credentials")), "{buttons:?}");
    }

    #[test]
    fn the_selected_tab_is_underlined_in_each_themes_accent() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let shapes = paint(&ctx, |ui| {
                let mut cur = 1u8;
                underline(ui, &mut cur, &three());
            });
            assert!(strokes(&shapes).contains(&Theme::of(id).accent), "{id:?}");
        }
    }

    /// An unknown count draws nothing; a known zero draws "0" (spec §4.2).
    #[test]
    fn an_unknown_count_is_not_drawn_and_a_known_zero_is() {
        let ctx = ctx_with(&Appearance::default());
        let shapes = paint(&ctx, |ui| {
            let mut cur = 0u8;
            underline(ui, &mut cur, &three());
        });
        let t: Vec<String> = texts(&shapes).into_iter().map(|(s, _)| s).collect();
        assert_eq!(t, ["Credentials", "12", "Backend", "Accounts", "0"]);
    }

    #[test]
    fn clicking_a_tab_switches_to_it() {
        let cur = Rc::new(Cell::new(0u8));
        let c = cur.clone();
        let mut h = harness(Appearance::default(), move |ui| {
            let mut v = c.get();
            underline(ui, &mut v, &three());
            c.set(v);
        });
        h.root()
            .children_recursive()
            .find(|n| n.accesskit_node().label().as_deref() == Some("Backend"))
            .expect("the Backend tab is a button")
            .click();
        h.run();
        assert_eq!(cur.get(), 1);
    }
}
