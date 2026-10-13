//! Trailing interaction controls painted OVER the price plot after it lays out
//! (chart refactor PR-4): the right (y) + bottom (x) drag-zoom gutters and the
//! bottom-center nav / scale overlay rows. Each was inline in `draw()`; the bodies
//! are moved verbatim, reading the frame + follow/settings state and writing their
//! result through `&mut` out-params (`nav_out`/`scale_change`), so the render is
//! byte-identical. See `chart/mod.rs`'s `draw`.

use crate::chart::Nav;
use crate::interact::{AutoYEvent, FollowLive, next_auto_y};
use crate::options::{ChartOptions, SettingsDialog};
use crate::scale::ScaleMode;
use egui::{Align, CursorIcon, Layout, Rect, Sense, UiBuilder, Vec2};
use vike_ui_theme::components::Tokens;
use vike_ui_theme::components::button::{ActionButton, IconButton};
use vike_ui_theme::components::overlay::menu_item;
use vike_ui_theme::components::segmented::{Segment, segmented};
use vike_ui_theme::components::toggle::{checkbox, radio};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::chart;

/// Right gutter (chart-UX bundle T4): y-drag zoom + dbl-click "back to
/// auto" over the price plot's own vertical span. MUST come after `plot.show`
/// above (ordering invariant): `frame` (the plot-area rect) is only known
/// from `resp.transform` once the price plot has actually laid out this
/// frame — native `.allow_axis_zoom_drag`/`.allow_double_click_reset` are
/// disabled on every pane (see the Plot builders above), so this
/// hand-rolled rect is now the ONLY thing that responds to a right-gutter
/// drag/dbl-click.
/// C2b Task 7b: with a secondary price axis active the right gutter is TWO
/// columns wide (primary inner + compare outer); span the y-drag / dbl-click
/// zone across BOTH so a drag anywhere in the price margin still zooms the
/// primary y. INACTIVE ⇒ `chart::Y_AXIS_GUTTER_W`, byte-identical to before.
///
/// TradingView parity: right-clicking the price axis opens a scale context
/// menu (Auto / Regular / Logarithmic / Percent / Indexed to 100 / Invert). It
/// writes into the SAME `scale_change`/`nav_out` out-params `scale_row`'s hover
/// buttons do — the two paths are equivalent; the hover buttons stay.
/// `requested_scale` is the active (as-requested, pre-fallback) mode, used to
/// radio-check the current item; `requested_invert` is the persisted Invert flag
/// (an orthogonal modifier, not a mode), rendered as a checkbox that emits
/// `invert_change`.
pub(crate) fn y_gutter(
    ui: &egui::Ui,
    frame: egui::Rect,
    sec_axis_active: bool,
    follow: &mut FollowLive,
    requested_scale: ScaleMode,
    requested_invert: bool,
    scale_change: &mut Option<ScaleMode>,
    invert_change: &mut Option<bool>,
    nav_out: &mut Option<Nav>,
) {
    let right_gutter_w =
        if sec_axis_active { 2.0 * chart::Y_AXIS_GUTTER_W } else { chart::Y_AXIS_GUTTER_W };
    let y_gutter = Rect::from_min_max(
        egui::pos2(frame.right(), frame.top()),
        egui::pos2(frame.right() + right_gutter_w, frame.bottom()),
    );
    let y_resp = ui.interact(y_gutter, ui.id().with("y_gutter"), Sense::click_and_drag());
    if y_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if y_resp.dragged() {
        follow.auto_y = next_auto_y(follow.auto_y, AutoYEvent::Drag); // manual y-scale: takes over from autofit
        follow.pending_y_drag += y_resp.drag_delta().y;
    }
    if y_resp.double_clicked() {
        follow.auto_y = next_auto_y(follow.auto_y, AutoYEvent::DblClick); // restores autofit (native reset is disabled)
    }
    // TradingView-parity right-click scale menu on the price axis, on the kit's menu rows
    // (GUI design system step 7): `menu_item` for "Auto", `radio` for the four scale modes and
    // `checkbox` for "Invert scale" — the same rows `scale_row`'s hover buttons duplicate.
    y_resp.context_menu(|ui| {
        // "Auto" = Y-only re-fit (same Nav::AutoY the Auto hover-button emits);
        // NOT Nav::Reset, which would also reset the x-range / re-pin follow.
        if menu_item(ui, None, "Auto (fits data to screen)", None).clicked() {
            *nav_out = Some(Nav::AutoY);
            ui.close();
        }
        ui.separator();
        // Radio-checked against the REQUESTED mode (matches scale_row's active
        // highlight, not the possibly-downgraded effective mode).
        let mut mode = requested_scale;
        for (m, label) in [
            (ScaleMode::Linear, "Regular"),
            (ScaleMode::Log, "Logarithmic"),
            (ScaleMode::Percent, "Percent"),
            // "Indexed to 100": the Percent twin — rebase first-visible to 100.
            (ScaleMode::Indexed, "Indexed to 100"),
        ] {
            if radio(ui, &mut mode, m, label).clicked() {
                *scale_change = Some(m);
                ui.close();
            }
        }
        ui.separator();
        // "Invert scale" is an ORTHOGONAL modifier (flips the y-axis vertically),
        // usable on top of ANY mode — a checkbox, not a radio item. A local `bool`
        // fed to `checkbox` toggles and is compared to emit `invert_change`.
        let mut invert = requested_invert;
        if checkbox(ui, &mut invert, "Invert scale").changed() {
            *invert_change = Some(invert);
            ui.close();
        }
    });
}

/// Bottom gutter (chart-UX bundle T4): x-drag zoom + dbl-click reset,
/// belonging to whichever pane ended up bottom-most this frame (tracked via
/// `bottom_frame` above — price when there are no sub-panes, otherwise the
/// last sub-pane, since only IT renders the shared time axis). MUST come
/// after every `plot.show` above (ordering invariant, same reason as the
/// right gutter): `bottom_frame` is only final once the bottom-most pane
/// has actually laid out this frame.
pub(crate) fn x_gutter(
    ui: &egui::Ui,
    bottom_frame: egui::Rect,
    follow: &mut FollowLive,
    nav_out: &mut Option<Nav>,
) {
    let x_gutter = Rect::from_min_max(
        egui::pos2(bottom_frame.left(), bottom_frame.bottom()),
        egui::pos2(bottom_frame.right(), bottom_frame.bottom() + chart::AXIS_LABEL_H),
    );
    let x_resp = ui.interact(x_gutter, ui.id().with("x_gutter"), Sense::click_and_drag());
    if x_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    if x_resp.dragged() {
        follow.pending_x_drag += x_resp.drag_delta().x;
    }
    if x_resp.double_clicked() {
        *nav_out = Some(Nav::Reset); // bottom gutter dbl-click reuses the existing Reset path
    }
}

/// T6: a gear (`icons::SETTINGS`) trailing the nav row opens the "Chart settings" dialog. It
/// is NOT a `Nav` (no bounds effect) — handled via `toggle_settings` below. Every button in the
/// row is an icon alone, so each is named by what it does (the kit's `IconButton::new(icon, tip)`
/// files `tip` as its accessible name; GUI design system step 7): the words are its accessible
/// name and its hover text.
pub(crate) fn nav_row(
    ui: &mut egui::Ui,
    frame: egui::Rect,
    settings: &mut SettingsDialog,
    options: &ChartOptions,
    nav_out: &mut Option<Nav>,
    // Feature #1b (TradingView parity): the price-pane maximize flag
    // (`FollowLive::price_maximized`); the maximize button below toggles it.
    maximized: &mut bool,
) {
    // TradingView parity: the nav overlay appears only while the pointer is over the price
    // frame — the chart stays clean otherwise (TV has no permanent on-chart toolbar). Only the
    // BUTTONS are hover-gated (below); the child ui is created unconditionally so its id is
    // present in BOTH egui layout passes (see the `new_child` block).
    let hovered = ui.ctx().pointer_hover_pos().is_some_and(|p| frame.contains(p));
    let mut toggle_settings = false;
    let labels = [
        (icons::ZOOM_OUT, Nav::ZoomOut, "Zoom out"),
        (icons::ZOOM_IN, Nav::ZoomIn, "Zoom in"),
        (icons::EARLIER, Nav::PanLeft, "Earlier"),
        (icons::LATER, Nav::PanRight, "Later"),
        (icons::RESET_VIEW, Nav::Reset, "Reset view"),
    ];
    // The row's size comes from the density (spec §4): each control is a `control_h` square,
    // `space::XS` apart — the kit's own icon-button metric, not a hand-picked pixel width.
    let t = Tokens::of(ui.ctx());
    let side = t.metrics.control_h;
    let n = labels.len() + 2; // + the gear and maximize/restore
    let row_w = side * n as f32 + space::XS * (n - 1) as f32;
    let row = Rect::from_min_size(
        egui::pos2(frame.center().x - row_w / 2.0, frame.bottom() - chart::NAV_ROW_FROM_BOTTOM),
        Vec2::new(row_w, side),
    );
    // Drawn via `new_child` (NOT `scope_builder`), so the parent layout cursor is NOT advanced
    // by this overlay — the price plot already moved it, and the sub-panes below must land there
    // regardless. A cursor-advancing builder would shift the panes across egui's two layout
    // passes on a hover transition (the "panes jump on hover" bug). The child ui is created
    // UNCONDITIONALLY (stable id in both passes); only the BUTTONS are hover-gated — otherwise
    // the child's id appears in one pass and not the other and egui flashes a red
    // "changed id between passes" box.
    let mut nav_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(row)
            .id_salt("vike_nav_row")
            .layout(Layout::left_to_right(Align::Center)),
    );
    if hovered {
        let ui = &mut nav_ui;
        ui.spacing_mut().item_spacing.x = space::XS;
        for (icon, n, tip) in labels {
            if ui.add(IconButton::new(icon, tip)).clicked() {
                *nav_out = Some(n);
            }
        }
        if ui.add(IconButton::new(icons::SETTINGS, "Chart settings")).clicked() {
            toggle_settings = true;
        }
        // Feature #1b: MAXIMIZE fills the chart with the price pane (hides the sub-panes);
        // when maximized it becomes a RESTORE control. Mirrors TradingView's per-pane
        // maximize/restore affordance.
        let (icon, hint) = if *maximized {
            (icons::RESTORE, "Restore panes")
        } else {
            (icons::MAXIMIZE, "Maximize price")
        };
        if ui.add(IconButton::new(icon, hint)).clicked() {
            *maximized = !*maximized;
        }
    }
    // Toggle the dialog (opens next frame; see the top-of-`draw` dialog block).
    // On OPEN, seed the working copy from the COMMITTED options so an edit
    // session always starts from what's on screen.
    if toggle_settings {
        settings.open = !settings.open;
        if settings.open {
            settings.working = options.clone();
        }
    }
}

/// The scale row's segments: the requested scale is the selected one (a Label, spec §4.2).
/// `Indexed to 100`, reachable from the price-axis menu, selects none of them.
const SCALES: [Segment<'static, ScaleMode>; 3] = [
    Segment { value: ScaleMode::Linear, label: "Lin", why: "Price on a linear scale" },
    Segment { value: ScaleMode::Log, label: "Log", why: "Price on a logarithmic scale" },
    Segment {
        value: ScaleMode::Percent,
        label: "%",
        why: "Change from the first visible bar, in percent",
    },
];

/// Log / % scale toggles + Auto (chart-UX bundle T3; on the kit since GUI design system step 7):
/// one segmented `Lin`/`Log`/`%` control plus a secondary `Auto` button, right-aligned at the
/// frame's bottom-right. The selected segment reports the REQUESTED mode (not the possibly-
/// downgraded effective mode), matching `y_gutter`'s price-axis menu radio-check; no market
/// colour marks the active one any more (decision 4).
pub(crate) fn scale_row(
    ui: &mut egui::Ui,
    frame: egui::Rect,
    requested_scale: ScaleMode,
    scale_change: &mut Option<ScaleMode>,
    nav_out: &mut Option<Nav>,
) {
    // TradingView parity: the Log/%/Auto scale controls appear only on hover over the price
    // frame — TV keeps them off the chart (scale mode lives in the price-axis right-click menu /
    // Alt+L / Alt+P). Only the BUTTONS are hover-gated; the child ui is created unconditionally
    // (stable id in both layout passes — see the `new_child` block).
    let hovered = ui.ctx().pointer_hover_pos().is_some_and(|p| frame.contains(p));
    let t = Tokens::of(ui.ctx());
    let right = frame.right() - space::LG;
    let top = frame.bottom() - chart::SCALE_ROW_FROM_BOTTOM;
    let scale_row = Rect::from_min_max(
        egui::pos2(right - chart::SCALE_ROW_MAX_W, top),
        egui::pos2(right, top + t.metrics.control_h),
    );
    // `new_child` (not `scope_builder`): non-cursor-advancing overlay, so this row never shifts
    // the panes below across egui's layout passes (see `nav_row`). Child created
    // unconditionally (stable id); only the BUTTONS are hover-gated.
    let mut scale_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(scale_row)
            .id_salt("vike_scale_row")
            .layout(Layout::right_to_left(Align::Center)),
    );
    if hovered {
        let ui = &mut scale_ui;
        ui.spacing_mut().item_spacing.x = t.metrics.gap;
        if ui.add(ActionButton::secondary("Auto")).clicked() {
            // Y-only re-fit (TradingView oracle) — do NOT reuse Nav::Reset, which also resets
            // cx0/cx1 and re-pins follow.on.
            *nav_out = Some(Nav::AutoY);
        }
        let mut mode = requested_scale;
        if segmented(ui, &mut mode, &SCALES) {
            *scale_change = Some(mode);
        }
    }
}
