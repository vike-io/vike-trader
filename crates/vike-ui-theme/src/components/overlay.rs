//! Menus, tooltips and toasts (spec §4.1). egui draws a menu's and a tooltip's frame from the
//! installed visuals — the 4 px radius, the border, no shadow (`crate::appearance::visuals`). The
//! kit adds what goes INSIDE (the menu row, the tooltip body) and the toast, which egui has none of.

use egui::{Align2, CornerRadius, Response, RichText, Role, Sense, Stroke, Ui, WidgetInfo};

use super::button::ThemeButtonIcon;
use super::{ThemeStatus, ThemeTokens};
use crate::icons::{self, ThemeIcon};
use crate::metrics::{RADIUS, stroke};
use crate::type_scale::ThemeTextRole;

/// How long a toast stays up.
pub const TOAST_SECS: f64 = 6.0;

/// One menu row: an icon column (empty when `icon` is `None`, so labels line up), the label, and a
/// right-aligned shortcut in the caption mono; the control height, the hover fill across the row.
/// Clicking it closes the menu.
pub fn menu_item(
    ui: &mut Ui,
    icon: Option<ThemeIcon>,
    label: &str,
    shortcut: Option<&str>,
) -> Response {
    let t = ThemeTokens::of(ui.ctx());
    let size = egui::vec2(ui.available_width(), t.metrics.control_height);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(RADIUS), t.theme.hover);
    }
    let icon_px = t.text.px(ThemeTextRole::Body);
    let x = rect.left() + t.metrics.padding;
    if let Some(i) = icon {
        let font = egui::FontId::new(icon_px, icons::family());
        i.paint(
            p,
            egui::pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            font,
            t.theme.text_secondary,
        );
    }
    let label_at = egui::pos2(x + icon_px + t.metrics.gap, rect.center().y);
    p.text(label_at, Align2::LEFT_CENTER, label, t.font(ThemeTextRole::Strong), t.theme.text);
    if let Some(s) = shortcut {
        let at = egui::pos2(rect.right() - t.metrics.padding, rect.center().y);
        p.text(at, Align2::RIGHT_CENTER, s, t.mono(ThemeTextRole::Caption), t.theme.text_tertiary);
    }
    super::focus_ring(ui, &t, &resp);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    if resp.clicked() {
        ui.close();
    }
    resp
}

/// A tooltip's body: the body role, wrapping at about forty characters.
pub fn tooltip_body(ui: &mut Ui, text: &str) {
    let t = ThemeTokens::of(ui.ctx());
    ui.set_max_width(t.text.px(ThemeTextRole::Body) * 24.0);
    ui.label(RichText::new(text).font(t.font(ThemeTextRole::Body)).color(t.theme.text));
}

/// A toast: a card whose LEFT EDGE carries the status — the stripe is coloured, not the text
/// (spec §3.1: a warning is "a dot or a toast's edge stripe").
pub fn toast_body(ui: &mut Ui, status: ThemeStatus, text: &str) -> Response {
    let t = ThemeTokens::of(ui.ctx());
    let stripe = 3.0;
    let margin = egui::Margin {
        left: (t.metrics.padding + stripe) as i8,
        right: t.metrics.padding as i8,
        top: t.metrics.gap as i8,
        bottom: t.metrics.gap as i8,
    };
    let r = egui::Frame::new()
        .fill(t.theme.card)
        .stroke(Stroke::new(stroke::HAIRLINE, t.theme.border))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_max_width(t.text.px(ThemeTextRole::Body) * 30.0);
            ui.label(RichText::new(text).font(t.font(ThemeTextRole::Body)).color(t.theme.text));
        })
        .response;
    let edge = egui::Rect::from_min_size(r.rect.min, egui::vec2(stripe, r.rect.height()));
    let round_left = CornerRadius { nw: RADIUS, sw: RADIUS, ne: 0, se: 0 };
    ui.painter().rect_filled(edge, round_left, status.color());
    r
}

/// The toast stack: [`ThemeToasts::push`] from anywhere, [`ThemeToasts::show`] once a frame from the shell.
/// Bottom-right, newest last; each lasts [`TOAST_SECS`] or until its close icon is clicked.
#[derive(Clone, Debug, Default)]
pub struct ThemeToasts {
    items: Vec<ThemeToast>,
    next: u64,
}

#[derive(Clone, Debug)]
struct ThemeToast {
    id: u64,
    status: ThemeStatus,
    text: String,
    until: f64,
}

fn toasts_id() -> egui::Id {
    egui::Id::unique("vike_ui_theme::components::toasts")
}

impl ThemeToasts {
    /// Queue a toast on `context`.
    pub fn push(context: &egui::Context, status: ThemeStatus, text: impl Into<String>) {
        let until = context.input(|i| i.time) + TOAST_SECS;
        context.data_mut(|d| {
            let s = d.get_temp_mut_or_default::<ThemeToasts>(toasts_id());
            let id = s.next;
            s.next += 1;
            s.items.push(ThemeToast { id, status, text: text.into(), until });
        });
        context.request_repaint();
    }

    /// Draw the live toasts; drop the expired and the dismissed.
    pub fn show(context: &egui::Context) {
        let now = context.input(|i| i.time);
        let mut s: ThemeToasts = context.data(|d| d.get_temp(toasts_id())).unwrap_or_default();
        s.items.retain(|x| x.until > now);
        let mut dismissed = Vec::new();
        if !s.items.is_empty() {
            egui::Area::new(toasts_id())
                .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
                .order(egui::Order::Foreground)
                .show(context, |ui| {
                    for x in &s.items {
                        ui.horizontal(|ui| {
                            toast_body(ui, x.status, &x.text);
                            if ui.add(ThemeButtonIcon::new(icons::CLOSE, "Dismiss")).clicked() {
                                dismissed.push(x.id);
                            }
                        });
                    }
                });
            context.request_repaint_after(std::time::Duration::from_millis(250));
        }
        s.items.retain(|x| !dismissed.contains(&x.id));
        context.data_mut(|d| d.insert_temp(toasts_id(), s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::ThemeAppearance;
    use crate::components::testing::{harness, named, paint, texts};
    use crate::painted::fills;
    use crate::painted::{context_with, raw};
    use egui::accesskit::Role;

    /// The stripe carries the status; the text stays the text colour — for every status.
    #[test]
    fn a_toast_carries_its_status_on_its_edge() {
        let context = context_with(&ThemeAppearance::default());
        let t = ThemeTokens::of(&context);
        for s in ThemeStatus::ALL {
            let shapes = paint(&context, |ui| {
                toast_body(ui, s, "Saved to the store");
            });
            assert!(fills(&shapes).contains(&s.color()), "{s:?}");
            assert!(
                texts(&shapes).contains(&("Saved to the store".to_string(), t.theme.text)),
                "{s:?}"
            );
        }
    }

    /// A pushed toast's words are painted until it expires. "Painted" means its TEXT is among the
    /// shapes: egui lays a new area out invisibly on its first frame (its sizing pass), and an
    /// invisible painter still emits `Shape::Noop`s, so a bare "any shape at all" would pass on that
    /// frame for nothing.
    #[test]
    fn a_toast_is_drawn_until_it_expires() {
        let context = context_with(&ThemeAppearance::default());
        let at = |secs: f64| egui::RawInput { time: Some(secs), ..raw() };
        context
            .run_ui(at(0.0), |ui| ThemeToasts::push(ui.ctx(), ThemeStatus::Ok, "Saved"))
            .drop_without_applying_deltas();
        let drawn = |secs: f64| {
            let mut out = context.run_ui(at(secs), |ui| ThemeToasts::show(ui.ctx()));
            let shapes: Vec<egui::Shape> =
                std::mem::take(&mut out.shapes).into_iter().map(|c| c.shape).collect();
            out.drop_without_applying_deltas();
            texts(&shapes).iter().any(|(s, _)| s == "Saved")
        };
        // The area's sizing pass: laid out, not painted.
        drawn(0.5);
        assert!(drawn(1.0), "a fresh toast is drawn");
        assert!(!drawn(TOAST_SECS + 1.0), "an expired toast is gone");
    }

    #[test]
    fn a_menu_item_is_a_button_named_by_its_label() {
        let h = harness(ThemeAppearance::default(), |ui| {
            menu_item(ui, Some(icons::SAVE), "Save layout", Some("Ctrl+S"));
        });
        assert!(named(&h, Role::Button).contains(&"Save layout".to_string()));
    }

    /// Owner decision 1 of the design-system step-7 plan keeps the whole main menu on the Strong
    /// role (relayed to PR 6b, closed here). `menu_item` is live in the Data Manager's dropdown
    /// rows, so its label must not paint one step smaller than the rest of the menu, at either
    /// text size — the same check `menu_bar.rs`'s `the_whole_main_menu_is_the_strong_role` runs for the
    /// bar itself.
    #[test]
    fn a_menu_items_label_is_the_strong_role() {
        for size in crate::type_scale::ThemeTextSize::ALL {
            let context =
                context_with(&ThemeAppearance { text_size: size, ..ThemeAppearance::default() });
            let shapes = paint(&context, |ui| {
                menu_item(ui, None, "Load layout", None);
            });
            let px = shapes.iter().find_map(|s| match s {
                egui::Shape::Text(t) if t.galley.text() == "Load layout" => {
                    Some(t.galley.job.sections[0].format.font_id.size)
                }
                _ => None,
            });
            assert_eq!(px, Some(size.px(ThemeTextRole::Strong)), "{size:?}");
        }
    }
}
