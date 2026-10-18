//! Switch, checkbox and radio button (spec §4.1), painted here at the density's control height. A
//! switch that is on, a checked box and a selected radio are filled with the theme's accent (the PR 6
//! plan's owner decision 6), their mark in the on-fill black.

use std::sync::Arc;

use egui::{
    CornerRadius, Galley, Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType,
};

use super::{ON_FILL, Tokens};
use crate::icons;
use crate::metrics::{RADIUS, stroke};
use crate::type_scale::TextRole;

/// The box, track and dot size for a control height: three fifths of it, in whole pixels.
fn mark_side(control_h: f32) -> f32 {
    (control_h * 0.6).round()
}

/// Allocate a `[mark][gap][label]` row one control height tall; the label laid out.
fn row(ui: &mut Ui, t: &Tokens, mark_w: f32, label: &str) -> (Rect, Response, Arc<Galley>) {
    let galley =
        ui.painter().layout_no_wrap(label.to_string(), t.font(TextRole::Body), t.theme.text);
    let w = mark_w + t.metrics.gap + galley.size().x;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, t.metrics.control_h), Sense::click());
    (rect, resp, galley)
}

/// A switch: a track with a knob, the accent when on. Clicking flips `on`.
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let t = Tokens::of(ui.ctx());
    let side = mark_side(t.metrics.control_h);
    let track = egui::vec2(side * 1.8, side);
    let (rect, mut resp, galley) = row(ui, &t, track.x, label);
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let track_rect =
        Rect::from_min_size(egui::pos2(rect.left(), rect.center().y - side / 2.0), track);
    let (fill, edge, knob) = if *on {
        (t.theme.accent, t.theme.accent, ON_FILL)
    } else {
        (t.theme.surface, t.theme.border, t.theme.text2)
    };
    let p = ui.painter();
    let round = CornerRadius::same((side / 2.0) as u8);
    p.rect(track_rect, round, fill, Stroke::new(stroke::HAIRLINE, edge), StrokeKind::Inside);
    let knob_x = if *on { track_rect.right() - side / 2.0 } else { track_rect.left() + side / 2.0 };
    p.circle_filled(egui::pos2(knob_x, track_rect.center().y), side / 2.0 - 2.0, knob);
    let text_at =
        egui::pos2(track_rect.right() + t.metrics.gap, rect.center().y - galley.size().y / 2.0);
    p.galley(text_at, galley, t.theme.text);
    super::focus_ring(ui, &t, &resp);
    let now = *on;
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, now, label));
    resp
}

/// A checkbox: a rounded box — the accent, with the check icon, when checked.
pub fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str) -> Response {
    let t = Tokens::of(ui.ctx());
    let side = mark_side(t.metrics.control_h);
    let (rect, mut resp, galley) = row(ui, &t, side, label);
    if resp.clicked() {
        *checked = !*checked;
        resp.mark_changed();
    }
    let b = Rect::from_min_size(
        egui::pos2(rect.left(), rect.center().y - side / 2.0),
        egui::vec2(side, side),
    );
    let p = ui.painter();
    if *checked {
        p.rect(b, CornerRadius::same(RADIUS), t.theme.accent, Stroke::NONE, StrokeKind::Inside);
        let mark = egui::FontId::new(side - 2.0, icons::family());
        icons::CHECK.paint(p, b.center(), egui::Align2::CENTER_CENTER, mark, ON_FILL);
    } else {
        let edge = Stroke::new(stroke::HAIRLINE, t.theme.border);
        p.rect(b, CornerRadius::same(RADIUS), t.theme.bg, edge, StrokeKind::Inside);
    }
    let text_at = egui::pos2(b.right() + t.metrics.gap, rect.center().y - galley.size().y / 2.0);
    p.galley(text_at, galley, t.theme.text);
    super::focus_ring(ui, &t, &resp);
    let now = *checked;
    resp.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, true, now, label));
    resp
}

/// A radio button for `value`: selected when `*current == value`; clicking selects it.
pub fn radio<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, value: T, label: &str) -> Response {
    let t = Tokens::of(ui.ctx());
    let side = mark_side(t.metrics.control_h);
    let (rect, mut resp, galley) = row(ui, &t, side, label);
    if resp.clicked() && *current != value {
        *current = value;
        resp.mark_changed();
    }
    let selected = *current == value;
    let c = egui::pos2(rect.left() + side / 2.0, rect.center().y);
    let p = ui.painter();
    if selected {
        p.circle_filled(c, side / 2.0, t.theme.accent);
        p.circle_filled(c, side / 5.0, ON_FILL);
    } else {
        p.circle(c, side / 2.0 - 0.5, t.theme.bg, Stroke::new(stroke::HAIRLINE, t.theme.border));
    }
    let text_at =
        egui::pos2(rect.left() + side + t.metrics.gap, rect.center().y - galley.size().y / 2.0);
    p.galley(text_at, galley, t.theme.text);
    super::focus_ring(ui, &t, &resp);
    resp.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, label));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{circles, ctx_with, fills, harness, paint};
    use crate::metrics::Density;
    use crate::theme::{Theme, ThemeId};
    use egui::accesskit::{Role, Toggled};
    use egui_kittest::kittest::NodeT;
    use std::cell::Cell;
    use std::rc::Rc;

    /// On is the theme's accent (owner decision 6), off is not — on every theme.
    #[test]
    fn on_is_the_accent_and_off_is_not() {
        for id in ThemeId::ALL {
            let accent = Theme::of(id).accent;
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let on = paint(&ctx, |ui| {
                switch(ui, &mut true, "a");
                checkbox(ui, &mut true, "b");
                radio(ui, &mut 1u8, 1, "c");
            });
            assert!(fills(&on).contains(&accent), "{id:?} switch/checkbox");
            assert!(circles(&on).contains(&accent), "{id:?} radio");
            let off = paint(&ctx, |ui| {
                switch(ui, &mut false, "a");
                checkbox(ui, &mut false, "b");
                radio(ui, &mut 0u8, 1, "c");
            });
            assert!(!fills(&off).contains(&accent) && !circles(&off).contains(&accent), "{id:?}");
        }
    }

    #[test]
    fn a_toggle_row_is_the_densitys_control_height() {
        for d in Density::ALL {
            let ctx = ctx_with(&Appearance { density: d, ..Appearance::default() });
            let mut hs = Vec::new();
            paint(&ctx, |ui| {
                hs = vec![
                    switch(ui, &mut true, "a").rect.height(),
                    checkbox(ui, &mut true, "b").rect.height(),
                    radio(ui, &mut 0u8, 0, "c").rect.height(),
                ];
            });
            assert!(hs.iter().all(|h| *h == d.metrics().control_h), "{d:?}: {hs:?}");
        }
    }

    #[test]
    fn clicking_flips_and_the_tree_reports_the_state() {
        let state = Rc::new(Cell::new((false, false, 0u8)));
        let s = state.clone();
        let mut h = harness(Appearance::default(), move |ui| {
            let (mut sw, mut cb, mut r) = s.get();
            switch(ui, &mut sw, "Live orders");
            checkbox(ui, &mut cb, "Show volume");
            radio(ui, &mut r, 0, "UTC");
            radio(ui, &mut r, 1, "Local");
            s.set((sw, cb, r));
        });
        for name in ["Live orders", "Show volume", "Local"] {
            h.root()
                .children_recursive()
                .find(|n| n.accesskit_node().label().as_deref() == Some(name))
                .unwrap_or_else(|| panic!("{name}"))
                .click();
            h.run();
        }
        assert_eq!(state.get(), (true, true, 1));
        let checked = h
            .root()
            .children_recursive()
            .filter(|n| n.accesskit_node().role() == Role::CheckBox)
            .all(|n| n.accesskit_node().toggled() == Some(Toggled::True));
        assert!(checked, "both check-box nodes report checked");
    }
}
