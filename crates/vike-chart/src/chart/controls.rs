//! Trailing interaction controls painted OVER the price plot after it lays out
//! (chart refactor PR-4): the right (y) + bottom (x) drag-zoom gutters and the
//! bottom-center nav / scale overlay rows. Each was inline in `draw()`; the bodies
//! are moved verbatim, reading the frame + follow/settings state and writing their
//! result through `&mut` out-params (`nav_out`/`scale_change`), so the render is
//! byte-identical. See `chart/mod.rs`'s `draw`.

use super::consts::{AXIS_LABEL_H, Y_AXIS_GUTTER_W};
use crate::chart::Nav;
use crate::interact::{AutoYEvent, FollowLive, next_auto_y};
use crate::options::{ChartOptions, SettingsDialog};
use crate::scale::ScaleMode;
use egui::{Align, Button, CursorIcon, Layout, Rect, RichText, Sense, UiBuilder, Vec2};

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
/// primary y. INACTIVE ⇒ `Y_AXIS_GUTTER_W`, byte-identical to before.
///
/// TradingView parity: right-clicking the price axis opens a scale context
/// menu (Auto / Regular / Logarithmic / Percent / Indexed to 100 / Invert). It
/// writes into the SAME `scale_change`/`nav_out` out-params `scale_row`'s hover
/// buttons do — the two paths are equivalent; the hover buttons stay.
/// `requested_scale` is the active (as-requested, pre-fallback) mode, used to
/// radio-check the current item; `requested_invert` is the persisted Invert flag
/// (an orthogonal modifier, not a mode), rendered as a checkbox that emits
/// `invert_change`.
#[allow(clippy::too_many_arguments)]
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
    let right_gutter_w = if sec_axis_active { 2.0 * Y_AXIS_GUTTER_W } else { Y_AXIS_GUTTER_W };
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
    // TradingView-parity right-click scale menu on the price axis.
    y_resp.context_menu(|ui| {
        // "Auto" = Y-only re-fit (same Nav::AutoY the Auto hover-button emits);
        // NOT Nav::Reset, which would also reset the x-range / re-pin follow.
        if ui.button("Auto (fits data to screen)").clicked() {
            *nav_out = Some(Nav::AutoY);
            ui.close();
        }
        ui.separator();
        // Radio-checked against the REQUESTED mode (matches scale_row's active
        // highlight, not the possibly-downgraded effective mode).
        if ui.selectable_label(requested_scale == ScaleMode::Linear, "Regular").clicked() {
            *scale_change = Some(ScaleMode::Linear);
            ui.close();
        }
        if ui.selectable_label(requested_scale == ScaleMode::Log, "Logarithmic").clicked() {
            *scale_change = Some(ScaleMode::Log);
            ui.close();
        }
        if ui.selectable_label(requested_scale == ScaleMode::Percent, "Percent").clicked() {
            *scale_change = Some(ScaleMode::Percent);
            ui.close();
        }
        // "Indexed to 100": the Percent twin — rebase first-visible to 100.
        if ui.selectable_label(requested_scale == ScaleMode::Indexed, "Indexed to 100").clicked() {
            *scale_change = Some(ScaleMode::Indexed);
            ui.close();
        }
        ui.separator();
        // "Invert scale" is an ORTHOGONAL modifier (flips the y-axis vertically),
        // usable on top of ANY mode — a checkbox, not a radio item. A local `bool`
        // fed to `ui.checkbox` toggles and is compared to emit `invert_change`.
        let mut invert = requested_invert;
        if ui.checkbox(&mut invert, "Invert scale").changed() {
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
        egui::pos2(bottom_frame.right(), bottom_frame.bottom() + AXIS_LABEL_H),
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

/// T6: a gear (⚙) trailing the nav row opens the "Chart settings" dialog. It
/// is NOT a `Nav` (no bounds effect) — handled via `toggle_settings` below.
pub(crate) fn nav_row(
    ui: &mut egui::Ui,
    frame: egui::Rect,
    settings: &mut SettingsDialog,
    options: &ChartOptions,
    nav_out: &mut Option<Nav>,
    // Feature #1b (TradingView parity): the price-pane maximize flag
    // (`FollowLive::price_maximized`); the ⛶ button below toggles it.
    maximized: &mut bool,
) {
    // TradingView parity: the nav overlay appears only while the pointer is over the price
    // frame — the chart stays clean otherwise (TV has no permanent on-chart toolbar). Only the
    // BUTTONS are hover-gated (below); the child ui is created unconditionally so its id is
    // present in BOTH egui layout passes (see the `new_child` block).
    let hovered = ui.ctx().pointer_hover_pos().is_some_and(|p| frame.contains(p));
    let mut toggle_settings = false;
    let labels = [
        ("−", Nav::ZoomOut),
        ("+", Nav::ZoomIn),
        ("‹", Nav::PanLeft),
        ("›", Nav::PanRight),
        ("⟳", Nav::Reset),
    ];
    let bw = 32.0;
    let row_w = bw * (labels.len() + 2) as f32; // +2 for the gear and ⛶ maximize
    let row = Rect::from_min_size(
        egui::pos2(frame.center().x - row_w / 2.0, frame.bottom() - 44.0),
        Vec2::new(row_w, 28.0),
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
        ui.spacing_mut().item_spacing.x = 2.0;
        for (g, n) in labels {
            if ui.add_sized([bw - 2.0, 26.0], Button::new(RichText::new(g).size(15.0))).clicked() {
                *nav_out = Some(n);
            }
        }
        if ui
            .add_sized([bw - 2.0, 26.0], Button::new(RichText::new("⚙").size(15.0)))
            .on_hover_text("Chart settings")
            .clicked()
        {
            toggle_settings = true;
        }
        // Feature #1b: ⛶ maximizes the price pane (hides the sub-panes); when
        // maximized it becomes a restore control. Mirrors TradingView's
        // per-pane maximize/restore affordance.
        let (glyph, hint) =
            if *maximized { ("❐", "Restore panes") } else { ("⛶", "Maximize price") };
        if ui
            .add_sized([bw - 2.0, 26.0], Button::new(RichText::new(glyph).size(15.0)))
            .on_hover_text(hint)
            .clicked()
        {
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

/// Log / % scale toggles + Auto (chart-UX bundle T3): same overlay row
/// style as Auto, positioned so Auto's own rect is UNCHANGED from before
/// (toggle_w + gap + pct_w + gap == 60.0 == Auto's old left-shift, so its
/// right edge stays at frame.right() - 8.0 and its geometry is pixel-for-
/// pixel identical to pre-T3). Active toggle (matches the REQUESTED mode,
/// not the possibly-downgraded effective mode) renders in the same UP
/// green as Auto; inactive uses the default button text color.
pub(crate) fn scale_row(
    ui: &mut egui::Ui,
    frame: egui::Rect,
    requested_scale: ScaleMode,
    up_col: egui::Color32,
    scale_change: &mut Option<ScaleMode>,
    nav_out: &mut Option<Nav>,
) {
    // TradingView parity: the Log/%/Auto scale controls appear only on hover over the price
    // frame — TV keeps them off the chart (scale mode lives in the price-axis right-click menu /
    // Alt+L / Alt+P). Only the BUTTONS are hover-gated; the child ui is created unconditionally
    // (stable id in both layout passes — see the `new_child` block).
    let hovered = ui.ctx().pointer_hover_pos().is_some_and(|p| frame.contains(p));
    const TOGGLE_W: f32 = 30.0;
    const PCT_W: f32 = 26.0;
    const AUTO_W: f32 = 52.0;
    const GAP: f32 = 2.0;
    let row_w = TOGGLE_W + GAP + PCT_W + GAP + AUTO_W;
    let scale_row = Rect::from_min_size(
        egui::pos2(frame.right() - 8.0 - row_w, frame.bottom() - 34.0),
        Vec2::new(row_w, 22.0),
    );
    // `new_child` (not `scope_builder`): non-cursor-advancing overlay, so this row never shifts
    // the panes below across egui's layout passes (see `nav_row`). Child created
    // unconditionally (stable id); only the BUTTONS are hover-gated.
    let mut scale_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(scale_row)
            .id_salt("vike_scale_row")
            .layout(Layout::left_to_right(Align::Center)),
    );
    if hovered {
        let ui = &mut scale_ui;
        ui.spacing_mut().item_spacing.x = GAP;
        let log_active = requested_scale == ScaleMode::Log;
        let mut log_text = RichText::new("Log").size(12.0);
        if log_active {
            log_text = log_text.color(up_col);
        }
        if ui.add_sized([TOGGLE_W, 22.0], Button::new(log_text)).clicked() {
            *scale_change = Some(if log_active { ScaleMode::Linear } else { ScaleMode::Log });
        }
        let pct_active = requested_scale == ScaleMode::Percent;
        let mut pct_text = RichText::new("%").size(12.0);
        if pct_active {
            pct_text = pct_text.color(up_col);
        }
        if ui.add_sized([PCT_W, 22.0], Button::new(pct_text)).clicked() {
            *scale_change = Some(if pct_active { ScaleMode::Linear } else { ScaleMode::Percent });
        }
        if ui
            .add_sized([AUTO_W, 22.0], Button::new(RichText::new("Auto").size(12.0).color(up_col)))
            .clicked()
        {
            // Final-review fix: Y-ONLY re-fit (TradingView oracle) — do
            // NOT reuse Nav::Reset, which also resets cx0/cx1 and
            // re-pins follow.on.
            *nav_out = Some(Nav::AutoY);
        }
    }
}
