//! Menus, tooltips and toasts (spec §4.1). egui draws a menu's and a tooltip's frame from the
//! installed visuals — the 4 px radius, the border, no shadow (`crate::appearance::visuals`). The
//! kit adds what goes INSIDE (the menu row, the tooltip body) and the toast, which egui has none of.

use egui::{Align2, CornerRadius, Response, RichText, Sense, Stroke, Ui, WidgetInfo, WidgetType};

use super::button::IconButton;
use super::{Status, Tokens};
use crate::icons::{self, Icon};
use crate::metrics::{RADIUS, stroke};
use crate::type_scale::TextRole;

/// How long a toast stays up.
pub const TOAST_SECS: f64 = 6.0;

/// One menu row: an icon column (empty when `icon` is `None`, so labels line up), the label, and a
/// right-aligned shortcut in the caption mono; the control height, the hover fill across the row.
/// Clicking it closes the menu.
pub fn menu_item(ui: &mut Ui, icon: Option<Icon>, label: &str, shortcut: Option<&str>) -> Response {
    let t = Tokens::of(ui.ctx());
    let size = egui::vec2(ui.available_width(), t.metrics.control_h);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(RADIUS), t.theme.hover);
    }
    let icon_px = t.text.px(TextRole::Body);
    let x = rect.left() + t.metrics.pad;
    if let Some(i) = icon {
        let font = egui::FontId::new(icon_px, icons::family());
        i.paint(p, egui::pos2(x, rect.center().y), Align2::LEFT_CENTER, font, t.theme.text2);
    }
    let label_at = egui::pos2(x + icon_px + t.metrics.gap, rect.center().y);
    p.text(label_at, Align2::LEFT_CENTER, label, t.font(TextRole::Strong), t.theme.text);
    if let Some(s) = shortcut {
        let at = egui::pos2(rect.right() - t.metrics.pad, rect.center().y);
        p.text(at, Align2::RIGHT_CENTER, s, t.mono(TextRole::Caption), t.theme.text3);
    }
    super::focus_ring(ui, &t, &resp);
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    if resp.clicked() {
        ui.close();
    }
    resp
}

/// A tooltip's body: the body role, wrapping at about forty characters.
pub fn tooltip_body(ui: &mut Ui, text: &str) {
    let t = Tokens::of(ui.ctx());
    ui.set_max_width(t.text.px(TextRole::Body) * 24.0);
    ui.label(RichText::new(text).font(t.font(TextRole::Body)).color(t.theme.text));
}

/// Hover text drawn with the kit's tooltip body.
pub fn tooltip(resp: Response, text: &str) -> Response {
    resp.on_hover_ui(|ui| tooltip_body(ui, text))
}

/// A toast: a card whose LEFT EDGE carries the status — the stripe is coloured, not the text
/// (spec §3.1: a warning is "a dot or a toast's edge stripe").
pub fn toast_body(ui: &mut Ui, status: Status, text: &str) -> Response {
    let t = Tokens::of(ui.ctx());
    let stripe = 3.0;
    let margin = egui::Margin {
        left: (t.metrics.pad + stripe) as i8,
        right: t.metrics.pad as i8,
        top: t.metrics.gap as i8,
        bottom: t.metrics.gap as i8,
    };
    let r = egui::Frame::new()
        .fill(t.theme.card)
        .stroke(Stroke::new(stroke::HAIRLINE, t.theme.border))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_max_width(t.text.px(TextRole::Body) * 30.0);
            ui.label(RichText::new(text).font(t.font(TextRole::Body)).color(t.theme.text));
        })
        .response;
    let edge = egui::Rect::from_min_size(r.rect.min, egui::vec2(stripe, r.rect.height()));
    let round_left = CornerRadius { nw: RADIUS, sw: RADIUS, ne: 0, se: 0 };
    ui.painter().rect_filled(edge, round_left, status.color());
    r
}

/// The toast stack: [`Toasts::push`] from anywhere, [`Toasts::show`] once a frame from the shell.
/// Bottom-right, newest last; each lasts [`TOAST_SECS`] or until its close icon is clicked.
#[derive(Clone, Debug, Default)]
pub struct Toasts {
    items: Vec<Toast>,
    next: u64,
}

#[derive(Clone, Debug)]
struct Toast {
    id: u64,
    status: Status,
    text: String,
    until: f64,
}

fn toasts_id() -> egui::Id {
    egui::Id::new("vike_ui_theme::components::toasts")
}

impl Toasts {
    /// Queue a toast on `ctx`.
    pub fn push(ctx: &egui::Context, status: Status, text: impl Into<String>) {
        let until = ctx.input(|i| i.time) + TOAST_SECS;
        ctx.data_mut(|d| {
            let s = d.get_temp_mut_or_default::<Toasts>(toasts_id());
            let id = s.next;
            s.next += 1;
            s.items.push(Toast { id, status, text: text.into(), until });
        });
        ctx.request_repaint();
    }

    /// Draw the live toasts; drop the expired and the dismissed.
    pub fn show(ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let mut s: Toasts = ctx.data(|d| d.get_temp(toasts_id())).unwrap_or_default();
        s.items.retain(|x| x.until > now);
        let mut dismissed = Vec::new();
        if !s.items.is_empty() {
            egui::Area::new(toasts_id())
                .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    for x in &s.items {
                        ui.horizontal(|ui| {
                            toast_body(ui, x.status, &x.text);
                            if ui.add(IconButton::new(icons::CLOSE, "Dismiss")).clicked() {
                                dismissed.push(x.id);
                            }
                        });
                    }
                });
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        s.items.retain(|x| !dismissed.contains(&x.id));
        ctx.data_mut(|d| d.insert_temp(toasts_id(), s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Appearance;
    use crate::components::testing::{ctx_with, fills, harness, named, paint, raw, texts};
    use egui::accesskit::Role;

    /// The stripe carries the status; the text stays the text colour — for every status.
    #[test]
    fn a_toast_carries_its_status_on_its_edge() {
        let ctx = ctx_with(&Appearance::default());
        let t = Tokens::of(&ctx);
        for s in Status::ALL {
            let shapes = paint(&ctx, |ui| {
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
        let ctx = ctx_with(&Appearance::default());
        let at = |secs: f64| egui::RawInput { time: Some(secs), ..raw() };
        ctx.run_ui(at(0.0), |ui| Toasts::push(ui.ctx(), Status::Ok, "Saved"))
            .drop_without_applying_deltas();
        let drawn = |secs: f64| {
            let mut out = ctx.run_ui(at(secs), |ui| Toasts::show(ui.ctx()));
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
        let h = harness(Appearance::default(), |ui| {
            menu_item(ui, Some(icons::SAVE), "Save layout", Some("Ctrl+S"));
        });
        assert!(named(&h, Role::Button).contains(&"Save layout".to_string()));
    }

    /// Owner decision 1 of the design-system step-7 plan keeps the whole main menu on the Strong
    /// role (relayed to PR 6b, closed here). `menu_item` is live in the Data Manager's dropdown
    /// rows, so its label must not paint one step smaller than the rest of the menu, at either
    /// text size — the same check `menu.rs`'s `the_whole_main_menu_is_the_strong_role` runs for the
    /// bar itself.
    #[test]
    fn a_menu_items_label_is_the_strong_role() {
        for size in crate::type_scale::TextSize::ALL {
            let ctx = ctx_with(&Appearance { text_size: size, ..Appearance::default() });
            let shapes = paint(&ctx, |ui| {
                menu_item(ui, None, "Load layout", None);
            });
            let px = shapes.iter().find_map(|s| match s {
                egui::Shape::Text(t) if t.galley.text() == "Load layout" => {
                    Some(t.galley.job.sections[0].format.font_id.size)
                }
                _ => None,
            });
            assert_eq!(px, Some(size.px(TextRole::Strong)), "{size:?}");
        }
    }
}
